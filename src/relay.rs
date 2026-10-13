//! Rust relay assembly, shared socket validation and private deployment state.
mod admission;
mod control;
mod pairing;
mod pairing_limits;
mod sessions;
mod standby;
mod traffic;
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
// Authenticated members have independent network/device quotas; anonymous peers use IP.
const MAX_SOURCE_SESSIONS: usize = 16;
const CACHE_STATE_MESSAGE_BYTES: usize = 128;
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
fn protocol(headers: &HeaderMap) -> Result<u32> {
    let value = headers
        .get("x-xrun-protocol")
        .and_then(|value| value.to_str().ok())
        .context(ErrorCode::VersionMismatch.error("missing protocol range"))?;
    ProtocolRange::CURRENT.negotiate(ProtocolRange::from_header(value)?)
}
fn negotiated(mut response: Response, selected: u32) -> Response {
    response.headers_mut().insert(
        "x-xrun-protocol",
        axum::http::HeaderValue::from_str(&selected.to_string()).expect("integer header"),
    );
    response
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    #[test]
    fn admission_depends_on_protocol_and_not_release_identity() {
        let mut headers = HeaderMap::new();
        headers.insert("x-xrun-version", "future-release".parse().unwrap());
        headers.insert("x-xrun-protocol", "1-1".parse().unwrap());
        assert_eq!(protocol(&headers).unwrap(), PROTOCOL);
        for unsupported in ["2-2", "3-3", "4-4", "5-5", "2-5"] {
            headers.insert("x-xrun-protocol", unsupported.parse().unwrap());
            assert!(crate::error::is(
                &protocol(&headers).unwrap_err(),
                ErrorCode::VersionMismatch
            ));
        }
        let future = PROTOCOL + 1;
        headers.insert(
            "x-xrun-protocol",
            format!("{future}-{future}").parse().unwrap(),
        );
        assert!(crate::error::is(
            &protocol(&headers).unwrap_err(),
            ErrorCode::VersionMismatch
        ));
        headers.remove("x-xrun-protocol");
        assert!(crate::error::is(
            &protocol(&headers).unwrap_err(),
            ErrorCode::VersionMismatch
        ));
    }
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
    authority_generation: u64,
    protocol: u32,
    generation: String,
    manager: bool,
    tx: mpsc::Sender<RelayMessage>,
    cancel: watch::Sender<bool>,
}
struct Session {
    authority_generation: u64,
    network: String,
    source: std::net::IpAddr,
    device: Option<String>,
    cached_at: Option<Instant>,
    anonymous: bool,
    target: String,
    generation: String,
    claim: Option<oneshot::Sender<std::result::Result<WebSocket, Data>>>,
    cancel: watch::Sender<bool>,
}
/// Challenges the client and returns the proven device and whether it holds
/// the network root key, or None for a pairing client without a certificate.
async fn authenticate(
    app: &App,
    ws: &mut WebSocket,
    network: &str,
    path: &str,
    permit: &TransportPermit,
    public_pairing: bool,
) -> Result<Option<(String, bool)>> {
    let nonce = crypto::random_token();
    send(
        ws,
        &RelayMessage::Challenge {
            nonce: nonce.clone(),
            authority: if public_pairing {
                None
            } else {
                app.admission.authority()
            },
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
    if app.admission.authenticate(&proof, manager)?
        && let Some(state) = &proof.authority
    {
        publish_connections(app, state).await;
    }
    permit.authenticated();
    Ok(Some((proof.device_id, manager)))
}
async fn publish_connections(app: &App, state: &crate::membership::AuthorityState) {
    let connections = app.connections.lock().await;
    let roster = &state.roster;
    for ((network, device), control) in &connections.controls {
        if network != &roster.roster.network_id {
            continue;
        }
        let revoked = roster.member(device).map_or(true, |member| member.revoked);
        let _ = control.tx.try_send(RelayMessage::Authority {
            state: state.clone(),
        });
        if revoked
            || control.authority_generation != roster.roster.authority_generation
            || control.manager != (device == &roster.roster.manager_id)
        {
            let _ = control.cancel.send(true);
        }
    }
    for session in connections.sessions.values() {
        if session.network == roster.roster.network_id
            && (session.authority_generation != roster.roster.authority_generation
                || roster
                    .member(&session.target)
                    .map_or(true, |member| member.revoked)
                || session.device.as_ref().is_some_and(|device| {
                    roster.member(device).map_or(true, |member| member.revoked)
                }))
        {
            let _ = session.cancel.send(true);
        }
    }
}
async fn member(
    app: &App,
    ws: &mut WebSocket,
    network: &str,
    path: &str,
    permit: &TransportPermit,
) -> Result<(String, bool)> {
    authenticate(app, ws, network, path, permit, false)
        .await?
        .context(ErrorCode::Unauthenticated.error("member proof required"))
}
#[derive(Default)]
struct Connections {
    controls: HashMap<(String, String), ControlConnection>,
    sessions: HashMap<String, Session>,
    standby: HashMap<(String, String), standby::Standby>,
    pairings: HashMap<String, pairing::Room>,
}
struct App {
    connections: Mutex<Connections>,
    traffic: Arc<traffic::Traffic>,
    pairing_limits: pairing_limits::PairingLimits,
    admission: admission::Admission,
}

async fn finish(ws: &mut WebSocket, result: Result<()>) {
    if let Err(error) = result {
        let _ = send(ws, &RelayMessage::error(&error)).await;
    }
    // Sending Close only flushes our frame. Keep reading until the peer closes
    // too so dropping the TLS socket cannot reset an unread error on Windows.
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
        ws.close().await?;
        while let Some(message) = ws.next().await {
            if matches!(message?, Message::Close(_)) {
                ws.flush().await?;
                break;
            }
        }
        Ok::<_, axum::Error>(())
    })
    .await;
}
pub async fn run(config: ServerConfig) -> Result<()> {
    let _lock = server::instance_lock(&config.data_dir)?;
    let keys = crypto::load_or_create_server(&config)?;
    let app = Arc::new(App {
        pairing_limits: pairing_limits::PairingLimits::open(
            &config.data_dir.join("pairing.sqlite"),
        )?,
        admission: admission::Admission::open(&config.data_dir)?,
        connections: Mutex::new(Connections::default()),
        traffic: Arc::new(traffic::Traffic::open(
            &config.data_dir.join("traffic.sqlite"),
        )),
    });
    let router = Router::new()
        .route("/networks/{network}/claim", get(admission::claim_route))
        .route("/networks/{network}/status", get(status_route))
        .route("/networks/{network}/traffic", get(traffic::traffic_route))
        .route("/networks/{network}/control", get(control_route))
        .route("/networks/{network}/connect/{target}", get(source_route))
        .route(
            "/networks/{network}/standby/{target}/{generation}",
            get(standby::route),
        )
        .route(
            "/networks/{network}/attach/{target}/{generation}/{sid}",
            get(attach_route),
        )
        .with_state(app.clone());
    server::serve_http(
        config.port,
        tokio_rustls::TlsAcceptor::from(keys.tls_config),
        Router::new()
            .merge(router)
            .route("/.well-known/xrun/pair/{locator}", get(pairing::route))
            .with_state(app),
    )
    .await
}
pub fn addresses(config: &ServerConfig) -> Result<Vec<String>> {
    Ok(config.urls())
}
pub fn deployment_link(config: &ServerConfig) -> Result<String> {
    let keys = crypto::load_or_create_server(config)?;
    Ok(format!(
        "xrun-relay://{}/{}",
        config.addresses.join(","),
        crypto::ca_spki_pin(&keys.ca_pem)?
    ))
}
