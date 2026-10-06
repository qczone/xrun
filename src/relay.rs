//! Rust relay assembly, shared socket validation and private deployment state.
mod control;
mod sessions;
use crate::error::ErrorCode;
use crate::{
    config::ServerConfig,
    crypto,
    protocol::*,
    server::{self, TransportPermit},
};
use anyhow::{Context, Result, bail};
use axum::{
    Extension, Json, Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use control::{control_route, status_route};
use futures_util::{SinkExt, StreamExt};
use sessions::{attach_route, source_route};
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::{Mutex, mpsc, oneshot, watch};

const MAX_CONTROL_CONNECTIONS: usize = 4096;
const MAX_NETWORK_CONTROLS: usize = 256;
const MAX_RELAY_SESSIONS: usize = 4096;
const MAX_TARGET_SESSIONS: usize = 32;
const MAX_SOURCE_SESSIONS: usize = 16;
const MAX_ANONYMOUS_TARGET_SESSIONS: usize = 4;

async fn send(ws: &mut WebSocket, message: &RelayMessage) -> Result<()> {
    let text = serde_json::to_string(message)?;
    if text.len() > MAX_MESSAGE {
        bail!(ErrorCode::MessageTooLarge.error("relay message limit exceeded"))
    }
    tokio::time::timeout(CONNECT_TIMEOUT, ws.send(Message::Text(text.into()))).await??;
    Ok(())
}
async fn receive(ws: &mut WebSocket) -> Result<RelayMessage> {
    loop {
        match ws
            .next()
            .await
            .context(ErrorCode::ConnectionClosed.error("relay socket closed"))??
        {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await?,
            Message::Pong(_) => {}
            _ => bail!(ErrorCode::InvalidMessage.error("expected relay message")),
        }
    }
}
fn version(headers: &HeaderMap) -> Result<()> {
    if headers.get("x-xrun-version").and_then(|v| v.to_str().ok()) != Some(VERSION) {
        bail!(ErrorCode::VersionMismatch.error(format!("all components must run {VERSION}")))
    }
    Ok(())
}
struct Api(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for Api {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}
impl IntoResponse for Api {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(Data::error(&self.0))).into_response()
    }
}
type ApiResult<T> = std::result::Result<T, Api>;

struct ControlConnection {
    generation: String,
    manager: bool,
    tx: mpsc::Sender<RelayMessage>,
    cancel: watch::Sender<bool>,
}
struct Session {
    network: String,
    source: std::net::IpAddr,
    anonymous: bool,
    target: String,
    generation: String,
    claim: Option<oneshot::Sender<std::result::Result<WebSocket, Data>>>,
    cancel: watch::Sender<bool>,
}
/// Challenges the client and returns the proven device and whether it holds
/// the network root key, or None for a pairing client without a certificate.
async fn authenticate(
    ws: &mut WebSocket,
    network: &str,
    path: &str,
    permit: &TransportPermit,
) -> Result<Option<(String, bool)>> {
    let nonce = crypto::random_token();
    send(
        ws,
        &RelayMessage::Challenge {
            nonce: nonce.clone(),
        },
    )
    .await?;
    let RelayMessage::Authenticate { proof } =
        tokio::time::timeout(AUTH_TIMEOUT, receive(ws)).await??
    else {
        bail!(ErrorCode::Unauthenticated.error("expected a member proof"))
    };
    let Some(proof) = proof else {
        return Ok(None);
    };
    let manager = proof.verify(network, path, &nonce)?;
    permit.authenticated();
    Ok(Some((proof.device_id, manager)))
}
async fn member(
    ws: &mut WebSocket,
    network: &str,
    path: &str,
    permit: &TransportPermit,
) -> Result<(String, bool)> {
    authenticate(ws, network, path, permit)
        .await?
        .context(ErrorCode::Unauthenticated.error("member proof required"))
}
#[derive(Default)]
struct Connections {
    controls: HashMap<(String, String), ControlConnection>,
    sessions: HashMap<String, Session>,
}
struct App {
    connections: Mutex<Connections>,
}

async fn finish(ws: &mut WebSocket, result: Result<()>) {
    if let Err(error) = result {
        let _ = send(ws, &RelayMessage::error(&error)).await;
    }
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, ws.close()).await;
}
pub async fn run(config: ServerConfig) -> Result<()> {
    let _lock = server::instance_lock(&config.data_dir)?;
    let keys = crypto::load_or_create_server(&config)?;
    let path = route(&config)?;
    let app = Arc::new(App {
        connections: Mutex::new(Connections::default()),
    });
    let router = Router::new()
        .route("/networks/{network}/status", get(status_route))
        .route("/networks/{network}/control", get(control_route))
        .route("/networks/{network}/connect/{target}", get(source_route))
        .route(
            "/networks/{network}/attach/{target}/{generation}/{sid}",
            get(attach_route),
        )
        .with_state(app);
    server::serve_http(
        config.port,
        tokio_rustls::TlsAcceptor::from(keys.tls_config),
        Router::new().nest(&format!("/{path}"), router),
    )
    .await
}
fn route(config: &ServerConfig) -> Result<String> {
    std::fs::create_dir_all(&config.data_dir)?;
    crate::config::restrict_dir(&config.data_dir)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.data_dir.join("route.lock"))?;
    lock.lock()?;
    let path = config.data_dir.join("route");
    match std::fs::read_to_string(&path) {
        Ok(value) if valid_relay_route(&value) => Ok(value),
        Ok(_) => bail!(ErrorCode::InvalidRelay.error("invalid local relay route")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let value = crypto::random_token();
            crate::config::atomic_private_write(&path, value.as_bytes())?;
            Ok(value)
        }
        Err(error) => Err(error.into()),
    }
}
pub fn addresses(config: &ServerConfig) -> Result<Vec<String>> {
    let route = route(config)?;
    Ok(config
        .urls()
        .into_iter()
        .map(|a| format!("{a}/{route}"))
        .collect())
}
pub fn deployment_link(config: &ServerConfig) -> Result<String> {
    let keys = crypto::load_or_create_server(config)?;
    let route = route(config)?;
    Ok(format!(
        "xrun-relay://{}/{}#{route}",
        config.addresses.join(","),
        crypto::ca_spki_pin(&keys.ca_pem)?
    ))
}
