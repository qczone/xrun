//! Loopback listener and forwarding connection lifecycle.
use crate::error::ErrorCode;
use crate::{config::Identity, net, protocol::*};
use anyhow::{Context, Result, bail};
use std::time::Duration;

use super::support::*;

pub(super) async fn run(id: Identity, target: &str, ports: (u16, u16), json: bool) -> Result<i32> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, ports.0))
        .await
        .context(ErrorCode::ForwardListenFailed.error("cannot bind local loopback port"))?;
    let address = listener.local_addr()?;
    print(
        json,
        &serde_json::json!({"device_id":target,"local_address":address.to_string(),"remote_port":ports.1}),
        || println!("{address} -> {target}:{} (Ctrl-C to stop)", ports.1),
    );
    let mut connections = tokio::task::JoinSet::new();
    let target = target.to_owned();
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (tcp, _) = result?;
                if connections.len() >= 32 {
                    diagnostic(json, &anyhow::anyhow!(ErrorCode::DeviceBusy.error("too many local forwarded connections")));
                    continue;
                }
                let id = id.clone();
                let target = target.clone();
                connections.spawn(async move {
                    let mut session = session(&id, &target).await?;
                    net::send(&mut session.ws, &Data::Request { request: Request::Forward { port: ports.1 } }).await?;
                    match tokio::time::timeout(Duration::from_secs(10), response(&mut session.ws)).await
                        .context(ErrorCode::ForwardTimeout.error("target did not acknowledge the connection"))?? {
                        Data::ForwardReady { port } if port == ports.1 => crate::forwarding::bridge(&mut session.ws, tcp).await,
                        _ => bail!(ErrorCode::InvalidMessage.error("expected forwarding acknowledgement")),
                    }
                });
            },
            result = connections.join_next(), if !connections.is_empty() => {
                match result {
                    Some(Ok(Ok(()))) | None => {},
                    Some(Ok(Err(error))) => diagnostic(json, &error),
                    Some(Err(error)) => diagnostic(json, &anyhow::anyhow!(error)),
                }
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = termination() => break,
        }
    }
    connections.shutdown().await;
    Ok(0)
}
