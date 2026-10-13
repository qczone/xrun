//! A normal anonymous relay handshake; no identity creation or ownership changes.
use super::{links, transport};
use crate::{crypto, error::ErrorCode, net, protocol::RelayMessage};
use anyhow::{Result, bail};
use std::time::Duration;

pub(crate) async fn check(link: &str) -> Result<()> {
    let endpoint = links::endpoint(link)?;
    let mut last = None;
    for address in endpoint.addresses {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let ca = links::discover_relay_ca(&address, &endpoint.pin)
                .await
                .map_err(classify_tls)?;
            let mut ws = net::websocket_at(
                &address,
                "/networks/probe/status",
                crypto::relay_tls_config(&ca)?,
            )
            .await
            .map_err(classify_tls)?;
            if !matches!(
                transport::receive(&mut ws).await?,
                RelayMessage::Challenge { .. }
            ) {
                bail!(ErrorCode::InvalidMessage.error("expected a relay challenge"));
            }
            let RelayMessage::RelayInfo { owner } = transport::receive(&mut ws).await? else {
                bail!(ErrorCode::InvalidRelayMessage.error("relay did not report its ownership"));
            };
            if let Some(owner) = owner {
                owner.validate()?;
            }
            Ok(())
        })
        .await;
        match result {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) if net::explicit(&error) => return Err(error),
            Ok(Err(error)) => last = Some(error),
            Err(_) => {
                last = Some(
                    ErrorCode::ConnectTimeout
                        .error("relay did not respond")
                        .into(),
                )
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        ErrorCode::ConnectFailed
            .error("relay is unavailable")
            .into()
    }))
}

fn classify_tls(error: anyhow::Error) -> anyhow::Error {
    if error.chain().any(|cause| {
        cause.downcast_ref::<rustls::Error>().is_some()
            || cause
                .downcast_ref::<std::io::Error>()
                .and_then(|error| error.get_ref())
                .is_some_and(|source| source.is::<rustls::Error>())
    }) {
        error
            .context(ErrorCode::InvalidCertificate.error("relay TLS certificate validation failed"))
    } else {
        error
    }
}
