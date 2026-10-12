//! Listen only on the explicitly selected assigned private adapter; pause closes the port.
use super::Runtime;
use crate::{
    lan::{self, Candidate},
    net,
    protocol::*,
};
use anyhow::Result;
use std::{net::SocketAddrV4, sync::Arc};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::protocol::{Role, WebSocketConfig},
};
pub(super) async fn run(rt: Arc<Runtime>) -> Result<()> {
    let mut changes = rt.access.subscribe();
    let mut stop = rt.stop.subscribe();
    let mut listener = None::<tokio::net::TcpListener>;
    let mut selected = None;
    let mut tasks = tokio::task::JoinSet::new();
    let mut interfaces = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        let config = match rt.config() {
            Ok(config) => config,
            Err(error) => {
                // Invalid access snapshots deny remote operations, but the local
                // reload/stop IPC must stay alive so a corrected file can recover.
                tracing::debug!(%error, "direct listener disabled until authorization recovers");
                listener = None;
                selected = None;
                lan::advertise(None);
                tasks.abort_all();
                tokio::select! {
                    result = changes.changed() => { if result.is_err() { break; } },
                    _ = stop.changed() => break,
                }
                continue;
            }
        };
        let interface = if config.remote_access_paused {
            None
        } else {
            lan::selected().unwrap_or(None)
        };
        let wanted = interface.as_ref().map(|interface| interface.address);
        if wanted != selected {
            listener = None;
            lan::advertise(None);
            tasks.abort_all();
            if let Some(address) = wanted
                && let Some(interface) = interface
            {
                let socket = match tokio::net::TcpListener::bind(SocketAddrV4::new(address, 0))
                    .await
                {
                    Ok(socket) => socket,
                    Err(error) => {
                        tracing::warn!(%error,"private interface unavailable; relay remains usable");
                        selected = None;
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        continue;
                    }
                };
                let std::net::SocketAddr::V4(address) = socket.local_addr()? else {
                    unreachable!()
                };
                lan::advertise(Some(Candidate {
                    address,
                    netmask: interface.netmask,
                }));
                listener = Some(socket);
            }
            selected = wanted;
        }
        let accepts = async {
            match &listener {
                Some(listener) => listener.accept().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            accepted=accepts=>{
                let(tcp,_)=accepted?;tcp.set_nodelay(true)?;
                let Ok(permit)=rt.sessions.clone().try_acquire_owned() else{continue;};
                let rt=rt.clone();tasks.spawn(async move{
                    let _permit=permit;
                    let config=WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
                    let ws=WebSocketStream::from_raw_socket(net::SocketIo::new(Box::new(tcp)),Role::Server,Some(config)).await;
                    if let Err(error)=super::session::serve_direct(rt,ws).await{tracing::debug!(%error,"direct channel ended");}
                });
            },
            _=changes.changed()=>{},
            _=interfaces.tick(),if config.lan_address.is_some()=>{},
            _=stop.changed()=>break,
            _=tasks.join_next(),if !tasks.is_empty()=>{},
        }
    }
    lan::advertise(None);
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}
