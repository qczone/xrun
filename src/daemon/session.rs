//! Encrypted session lifetime, roster exchange and authorization changes.
use crate::error::ErrorCode;
use crate::{
    config::Identity,
    membership::ReceiptAck,
    net::{self, Ws},
    network,
    protocol::*,
    secure,
};
use anyhow::{Result, bail};
use std::{sync::Arc, time::Duration};

use super::Runtime;
use super::requests::serve;

pub(super) async fn data_session(
    rt: Arc<Runtime>,
    address: &str,
    generation: &str,
    sid: &str,
) -> Result<()> {
    let id = Identity::load()?;
    let (mut ws, certificate) = tokio::time::timeout(Duration::from_secs(10), async {
        let (outer, flow_control) = network::attach(&id, address, generation, sid).await?;
        secure::server_with_flow(outer, &id, flow_control).await
    })
    .await??;
    let result = if let Some(certificate) = certificate {
        async {
            let (_, source) = tokio::time::timeout(
                Duration::from_secs(10),
                secure::exchange_server(&mut ws, &rt.members, &rt.network_id, &certificate),
            )
            .await??;
            let purpose: secure::Purpose =
                tokio::time::timeout(Duration::from_secs(10), net::receive(&mut ws)).await??;
            if matches!(purpose, secure::Purpose::State) {
                let state = network::PeerState {
                    device: network::local_device(&id, rt.cwd()?.to_string_lossy().into())?,
                    ack: ReceiptAck::create(&id, &network::current(&id)?)?,
                };
                net::send(&mut ws, &state).await?;
                return Ok(());
            }
            rt.allow(&source)?;
            let generation = rt.config()?.pause_generation;
            let (_, cert) = x509_parser::parse_x509_certificate(&certificate).map_err(|_| anyhow::anyhow!(ErrorCode::InvalidCertificate.error("malformed member certificate")))?;
            let expiry = cert.validity().not_after.timestamp()
                .min(crate::crypto::certificate_expiry(&id.cert_pem)?)
                .min(crate::crypto::certificate_expiry(&id.ca_pem)?);
            send_ready(&rt, &mut ws).await?;
            let mut stop = rt.stop.subscribe();
            loop {
                let req = tokio::select! {
                    req = tokio::time::timeout(Duration::from_secs(60), net::receive::<Data>(&mut ws)) => req??,
                    _ = stop.changed() => bail!(ErrorCode::DaemonStopping.error("daemon shutting down")),
                };
                rt.check_session(&source, generation)?;
                if expiry <= now_ms() / 1000 || Identity::load()?.cert_pem != id.cert_pem {
                    bail!(ErrorCode::SessionExpired.error("renew the authenticated connection"));
                }
                if let Data::SessionProbe { roster_version } = req {
                    if rt.members.load(&rt.network_id)?.roster.version != roster_version {
                        bail!(ErrorCode::MembershipChanged.error("renew the authenticated connection"));
                    }
                    send_ready(&rt, &mut ws).await?;
                    continue;
                }
                let Data::Request { request } = req else { bail!(ErrorCode::InvalidMessage.error("expected operation")); };
                let denied = async {
                    loop {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        if rt.check_session(&source, generation).is_err() || expiry <= now_ms() / 1000 { break; }
                    }
                };
                tokio::select! {
                    result = serve(rt.clone(), &source, generation, &mut ws, request) => result?,
                    _ = stop.changed() => bail!(ErrorCode::DaemonStopping.error("daemon shutting down")),
                    _ = denied => bail!(ErrorCode::SessionClosed.error("access changed")),
                }
                net::send(&mut ws, &Data::Complete).await?;
            }
        }
        .await
    } else {
        // Pairing is the only anonymous inner-TLS operation and is processed
        // by the local manager, never by the relay or an ordinary member.
        network::serve_pair(&id, &mut ws).await
    };
    if let Err(error) = result {
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            net::send(&mut ws, &Data::error(&error)),
        )
        .await;
    }
    let mut stop = rt.stop.subscribe();
    if !*stop.borrow() {
        tokio::select! {
            _ = net::close(&mut ws) => {},
            _ = stop.changed() => {},
        }
    }
    Ok(())
}
async fn send_ready(rt: &Runtime, ws: &mut Ws) -> Result<()> {
    net::send(
        ws,
        &Data::Ready {
            version: VERSION.into(),
            device_id: rt.id.device_id.clone(),
            db_id: rt.store.db_id.clone(),
            default_cwd: rt.cwd()?.to_string_lossy().into(),
        },
    )
    .await
}
