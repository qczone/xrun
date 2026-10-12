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
    let id = rt.authorization()?.identity.clone();
    let (outer, flow) = network::attach(&id, address, generation, sid).await?;
    serve_socket(rt, outer, flow).await
}
pub(super) async fn serve_socket(rt: Arc<Runtime>, outer: Ws, flow_control: bool) -> Result<()> {
    super::access::refresh(rt.clone(), crate::config::device_dir()?, false).await?;
    let id = rt.authorization()?.identity.clone();
    let (mut ws, certificate) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        secure::server_with_flow(outer, &id, flow_control),
    )
    .await??;
    if let Some(certificate) = certificate {
        let result = async {
            let (_, source, protocol, purpose) = tokio::time::timeout(
                CONNECT_TIMEOUT,
                secure::exchange_server(&mut ws, &rt.members, &rt.network_id, &certificate),
            )
            .await??;
            if matches!(purpose, secure::Purpose::Channel) {
                rt.allow(&source)?;
                net::send(&mut ws,&Data::ChannelReady{device_id:id.device_id.clone(),protocol}).await?;
                return channel_gateway(rt.clone(),&mut ws, id.clone(),source,protocol,certificate.clone()).await;
            }
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
            let (_, cert) = x509_parser::parse_x509_certificate(&certificate).map_err(|_| {
                anyhow::anyhow!(ErrorCode::InvalidCertificate.error("malformed member certificate"))
            })?;
            let expiry = cert.validity().not_after.timestamp()
                .min(crate::crypto::certificate_expiry(&id.cert_pem)?)
                .min(crate::crypto::certificate_expiry(&id.ca_pem)?);
            let lease = AccessLease { source: &source, generation, identity: &id, expiry,
                authority_generation: rt.members.load(&rt.network_id)?.roster.authority_generation };
            send_ready(&rt, &mut ws, protocol).await?;
            let mut stop = rt.stop.subscribe();
            loop {
                let req = tokio::select! {
                    req = tokio::time::timeout(REQUEST_IDLE_TIMEOUT, net::receive::<Data>(&mut ws)) => req??,
                    _ = stop.changed() => bail!(ErrorCode::DaemonStopping.error("daemon shutting down")),
                    result = access_ended(&rt, &lease) => return result,
                };
                super::access::refresh(rt.clone(), crate::config::device_dir()?, false).await?;
                rt.check_session(&source, generation)?;
                lease.check(&rt)?;
                if let Data::SessionProbe { roster_version } = req {
                    // Current peer membership, authority generation, freshness
                    // and grants were checked above. Other members changing
                    // does not invalidate this already authenticated pair.
                    let current = rt.members.require_fresh(&rt.network_id, now_ms())?;
                    net::send(&mut ws, &Data::Membership {
                        roster: (roster_version != current.roster.roster.version).then_some(current.roster),
                        lease: current.lease,
                    }).await?;
                    send_ready(&rt, &mut ws, protocol).await?;
                    continue;
                }
                let Data::Request { request } = req else {
                    bail!(ErrorCode::InvalidMessage.error("expected operation"));
                };
                ProtocolRange::require(protocol)?;
                let changes_remote_state = matches!(
                    &request,
                    Request::Exec { .. } | Request::Push { .. } | Request::Kill { .. }
                );
                tokio::select! {
                    result = serve(rt.clone(), &source, generation, &mut ws, request) => result?,
                    _ = stop.changed() => {
                        if changes_remote_state {
                            bail!(ErrorCode::Unconfirmed.error(
                                "daemon stopped while processing the operation; its outcome requires confirmation"
                            ));
                        }
                        bail!(ErrorCode::DaemonStopping.error("daemon shutting down"));
                    },
                    result = access_ended(&rt, &lease) => {
                        // Dropping a handler cannot cancel a database worker or
                        // undo an operation that already crossed its commit point.
                        // Only the handler's own pre-operation checks prove refusal.
                        if changes_remote_state {
                            return result.map_err(|error| anyhow::anyhow!(
                                ErrorCode::Unconfirmed.error(format!(
                                    "operation interrupted; its outcome requires confirmation: {error:#}"
                                ))
                            ));
                        }
                        return result;
                    },
                }
                net::send(&mut ws, &Data::Complete).await?;
            }
        }
        .await;
        close_result(&rt, ws, result).await
    } else {
        // Pairing is the only anonymous inner-TLS operation and is processed
        // by the local manager, never by the relay or an ordinary member.
        let result = network::serve_pair(&id, &mut ws).await;
        close_result(&rt, ws, result).await
    }
}
async fn close_result(rt: &Runtime, mut ws: Ws, result: Result<()>) -> Result<()> {
    if let Err(error) = result {
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, net::send(&mut ws, &Data::error(&error))).await;
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
struct AccessLease<'a> {
    source: &'a str,
    generation: u64,
    identity: &'a Identity,
    expiry: i64,
    authority_generation: u64,
}
impl AccessLease<'_> {
    fn check(&self, rt: &Runtime) -> Result<()> {
        rt.check_session(self.source, self.generation)?;
        if rt.members.load(&rt.network_id)?.roster.authority_generation != self.authority_generation
        {
            bail!(
                ErrorCode::MembershipChanged
                    .error("management generation changed; renew the connection")
            );
        }
        if self.expiry <= now_ms() / 1000
            || rt.authorization()?.identity.cert_pem != self.identity.cert_pem
        {
            bail!(ErrorCode::SessionExpired.error("renew the authenticated connection"));
        }
        Ok(())
    }
}
async fn access_ended(rt: &Runtime, lease: &AccessLease<'_>) -> Result<()> {
    let mut changes = rt.access.subscribe();
    let mut membership = rt.members.subscribe();
    loop {
        lease.check(rt)?;
        let freshness = rt.members.require_fresh(&rt.network_id, now_ms())?;
        let end = (lease.expiry * 1000).min(freshness.lease.lease.expires_at_ms);
        let remaining = Duration::from_millis((end - now_ms()).max(0) as u64);
        tokio::select! {
            result = changes.changed() => result?,
            result = membership.changed() => result?,
            _ = tokio::time::sleep(remaining) => {},
        }
    }
}
async fn send_ready(rt: &Runtime, ws: &mut Ws, selected_protocol: u32) -> Result<()> {
    net::send(
        ws,
        &Data::Ready {
            version: VERSION.into(),
            protocol: ProtocolRange::CURRENT,
            selected_protocol,
            device_id: rt.id.device_id.clone(),
            db_id: rt.store.db_id.clone(),
            default_cwd: rt.cwd()?.to_string_lossy().into(),
        },
    )
    .await
}

async fn channel_gateway(
    rt: Arc<Runtime>,
    ws: &mut Ws,
    id: Identity,
    source: String,
    protocol: u32,
    certificate: Vec<u8>,
) -> Result<()> {
    let (dummy, peer) = tokio::io::duplex(1);
    drop(peer);
    let replacement = tokio_tungstenite::WebSocketStream::from_raw_socket(
        net::SocketIo::new(Box::new(dummy)),
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    let ws = std::mem::replace(ws, replacement);
    let (channel, mut incoming) = crate::multiplex::start(ws, yamux::Mode::Server);
    let (_, cert) = x509_parser::parse_x509_certificate(&certificate)
        .map_err(|_| ErrorCode::InvalidCertificate.error("malformed member certificate"))?;
    let expiry = cert
        .validity()
        .not_after
        .timestamp()
        .min(crate::crypto::certificate_expiry(&id.cert_pem)?)
        .min(crate::crypto::certificate_expiry(&id.ca_pem)?);
    let generation = rt.config()?.pause_generation;
    let authority_generation = rt.members.load(&rt.network_id)?.roster.authority_generation;
    let lease = AccessLease {
        source: &source,
        generation,
        identity: &id,
        expiry,
        authority_generation,
    };
    let mut stop = rt.stop.subscribe();
    let mut operations = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
                            stream=incoming.recv()=>match stream{
                Some(ws)=>{
                                let rt=rt.clone();
                                let id=id.clone();
                                let source=source.clone();
        operations.spawn(async move{channel_operation(rt,ws,PairContext{id,source,protocol,generation,expiry,authority_generation}).await});

                                },
                                None=>break,
                            },
                            _=stop.changed()=>break,
                            _=access_ended(&rt,&lease)=>break,
                Some(result)=operations.join_next(),if !operations.is_empty()=>{
                            if let Err(error)=result{tracing::warn!(%error,"operation stream panicked");
                            }},
                        }
    }
    channel.close();
    operations.abort_all();
    while operations.join_next().await.is_some() {}
    Ok(())
}
#[derive(Clone)]
struct PairContext {
    id: Identity,
    source: String,
    protocol: u32,
    generation: u64,
    expiry: i64,
    authority_generation: u64,
}
async fn channel_operation(rt: Arc<Runtime>, mut ws: Ws, pair: PairContext) -> Result<()> {
    let PairContext {
        id,
        source,
        protocol,
        generation,
        expiry,
        authority_generation,
    } = pair;
    let lease = AccessLease {
        source: &source,
        generation,
        identity: &id,
        expiry,
        authority_generation,
    };
    let result=async{
let Data::SessionProbe {roster_version}=tokio::time::timeout(CONNECT_TIMEOUT,net::receive(&mut ws)).await??
        else {bail!(ErrorCode::InvalidMessage.error("operation stream requires a membership probe"));
        };

        super::access::refresh(rt.clone(),crate::config::device_dir()?,false).await?;
        lease.check(&rt)?;
        let current=rt.members.require_fresh(&rt.network_id,now_ms())?;
net::send(&mut ws,&Data::Membership{roster:(roster_version!=current.roster.roster.version).then_some(current.roster),lease:current.lease}).await?;

        send_ready(&rt,&mut ws,protocol).await?;
let Data::Request {request}=tokio::time::timeout(REQUEST_IDLE_TIMEOUT,net::receive(&mut ws)).await??
        else {bail!(ErrorCode::InvalidMessage.error("one operation per stream"));
        };

        tokio::select!{result=serve(rt.clone(),&source,generation,&mut ws,request)=>result?,result=access_ended(&rt,&lease)=>return result}
        net::send(&mut ws,&Data::Complete).await
    }.await;
    close_result(&rt, ws, result).await
}

pub(super) async fn standby_loop(
    rt: Arc<Runtime>,
    address: String,
    generation: String,
) -> Result<()> {
    let mut stop = rt.stop.subscribe();
    loop {
        let id = rt.authorization()?.identity.clone();
        let prepared = network::standby(&id, &address, &generation);
        let mut ws = tokio::select! {ws=prepared=>match ws{Ok(ws)=>ws,Err(error)=>{
        tracing::debug!(%error,"standby unavailable");
        tokio::time::sleep(Duration::from_secs(2)).await;
        continue;
        }},_=stop.changed()=>return Ok(())};

        let claimed = tokio::select! {reply=network::receive(&mut ws)=>reply ,
        _=stop.changed()=>return Ok(()) ,
        _=tokio::time::sleep(Duration::from_secs(290))=>continue};

        if let Ok(RelayMessage::Connected { flow_control }) = claimed {
            let permit = rt
                .sessions
                .clone()
                .try_acquire_owned()
                .map_err(|_| ErrorCode::DeviceBusy.error("encrypted session limit reached"))?;
            let runtime = rt.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) = serve_socket(runtime, ws, flow_control).await {
                    tracing::debug!(%error,"preconnected data session ended");
                }
            });
        }
    }
}
