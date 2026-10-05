use crate::{
    config::{Identity, ServerConfig},
    crypto,
    membership::{self, Manager},
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
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc, oneshot, watch};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    pub device_id: String,
    pub cert_pem: String,
    pub root_pem: String,
    pub signature: String,
    /// Only the manager's control connection carries this root-key signature,
    /// so pairing without a member certificate reaches no other device.
    pub manager_signature: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct ChallengeBinding<'a> {
    pub network: &'a str,
    pub device: &'a str,
    pub path: &'a str,
    pub nonce: &'a str,
}
impl Proof {
    pub fn create(
        id: &Identity,
        network: &str,
        path: &str,
        nonce: &str,
        manager: Option<&Manager>,
    ) -> Result<Self> {
        let binding = ChallengeBinding {
            network,
            device: &id.device_id,
            path,
            nonce,
        };
        Ok(Self {
            device_id: id.device_id.clone(),
            cert_pem: id.cert_pem.clone(),
            root_pem: id.ca_pem.clone(),
            signature: membership::sign(&id.key_pem, "relay-proof", &binding)?,
            manager_signature: manager.map(|m| m.sign_relay(&binding)).transpose()?,
        })
    }
    /// The network ID is the root key fingerprint, so the relay verifies
    /// members without storing a roster. Revocation remains an endpoint check;
    /// a revoked device can only appear as itself until its certificate expires.
    /// Returns whether the proof also holds the network root key.
    pub fn verify(&self, network: &str, path: &str, nonce: &str) -> Result<bool> {
        if network != format!("net_{}", crypto::ca_spki_pin(&self.root_pem)?) {
            bail!("UNAUTHENTICATED: proof belongs to another network")
        }
        crypto::verify_member_certificate(&self.cert_pem, &self.root_pem, &self.device_id)
            .map_err(|e| anyhow::anyhow!("UNAUTHENTICATED: invalid member certificate: {e}"))?;
        let binding = ChallengeBinding {
            network,
            device: &self.device_id,
            path,
            nonce,
        };
        membership::verify(&self.cert_pem, "relay-proof", &binding, &self.signature)
            .map_err(|_| anyhow::anyhow!("UNAUTHENTICATED: invalid member proof"))?;
        let Some(signature) = &self.manager_signature else {
            return Ok(false);
        };
        membership::verify(&self.root_pem, "relay-manager", &binding, signature)
            .map_err(|_| anyhow::anyhow!("UNAUTHENTICATED: invalid manager proof"))?;
        Ok(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelayMessage {
    Challenge {
        nonce: String,
    },
    /// None only for pairing, which the relay routes to the manager alone.
    Authenticate {
        proof: Option<Proof>,
    },
    HelloAck {
        generation: String,
    },
    Incoming {
        session_id: String,
    },
    Reject {
        session_id: String,
        error: Box<Data>,
    },
    Connected {
        #[serde(default)]
        flow_control: bool,
    },
    Status {
        devices: Vec<String>,
    },
    Error {
        code: String,
        message: String,
    },
}
impl RelayMessage {
    fn error(error: &anyhow::Error) -> Self {
        let Data::Error { code, message } = Data::error(error) else {
            unreachable!()
        };
        Self::Error { code, message }
    }
}
async fn send(ws: &mut WebSocket, message: &RelayMessage) -> Result<()> {
    let text = serde_json::to_string(message)?;
    if text.len() > MAX_MESSAGE {
        bail!("MESSAGE_TOO_LARGE: relay message limit exceeded")
    }
    tokio::time::timeout(Duration::from_secs(10), ws.send(Message::Text(text.into()))).await??;
    Ok(())
}
async fn receive(ws: &mut WebSocket) -> Result<RelayMessage> {
    loop {
        match ws
            .next()
            .await
            .context("CONNECTION_CLOSED: relay socket closed")??
        {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await?,
            Message::Pong(_) => {}
            _ => bail!("INVALID_MESSAGE: expected relay message"),
        }
    }
}
fn version(headers: &HeaderMap) -> Result<()> {
    if headers.get("x-xrun-version").and_then(|v| v.to_str().ok()) != Some(VERSION) {
        bail!("VERSION_MISMATCH: all components must run {VERSION}")
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
        tokio::time::timeout(Duration::from_secs(5), receive(ws)).await??
    else {
        bail!("UNAUTHENTICATED: expected a member proof")
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
        .context("UNAUTHENTICATED: member proof required")
}
#[derive(Default)]
struct Connections {
    controls: HashMap<(String, String), ControlConnection>,
    sessions: HashMap<String, Session>,
}
struct App {
    connections: Mutex<Connections>,
}

async fn status_route(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = async {
                let path = format!("/networks/{network}/status");
                member(&mut ws, &network, &path, &permit).await?;
                let devices = app
                    .connections
                    .lock()
                    .await
                    .controls
                    .keys()
                    .filter(|(n, _)| n == &network)
                    .map(|(_, id)| id.clone())
                    .collect();
                send(&mut ws, &RelayMessage::Status { devices }).await
            }
            .await;
            finish(&mut ws, result).await;
        }))
}
async fn control_route(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = control(&app, &network, &mut ws, &permit).await;
            finish(&mut ws, result).await;
        }))
}
async fn finish(ws: &mut WebSocket, result: Result<()>) {
    if let Err(error) = result {
        let _ = send(ws, &RelayMessage::error(&error)).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), ws.close()).await;
}
async fn control(
    app: &App,
    network: &str,
    ws: &mut WebSocket,
    permit: &TransportPermit,
) -> Result<()> {
    // Only the holder of a device key can take over that device's routing slot.
    // Authorization still happens inside the end-to-end tunnel.
    let path = format!("/networks/{network}/control");
    let (id, manager) = member(ws, network, &path, permit).await?;
    let key = (network.to_owned(), id.clone());
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let (tx, mut rx) = mpsc::channel(16);
    let (cancel, mut closed) = watch::channel(false);
    {
        let mut c = app.connections.lock().await;
        if !c.controls.contains_key(&key)
            && (c.controls.len() >= 4096
                || c.controls.keys().filter(|(n, _)| n == network).count() >= 256)
        {
            bail!("CONNECTION_LIMIT: too many control connections")
        }
        if let Some(old) = c.controls.insert(
            key.clone(),
            ControlConnection {
                generation: generation.clone(),
                manager,
                tx,
                cancel,
            },
        ) {
            let _ = old.cancel.send(true);
        }
        for s in c.sessions.values() {
            if s.network == network && s.target == id {
                let _ = s.cancel.send(true);
            }
        }
    }
    let result = async {
        send(ws, &RelayMessage::HelloAck { generation: generation.clone() }).await?;
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        let mut last = Instant::now();
        loop {
            tokio::select! {
                _ = closed.changed() => return Ok(()),
                _ = tick.tick() => {
                    if last.elapsed() > Duration::from_secs(45) { bail!("CONTROL_TIMEOUT: endpoint stopped responding") }
                    tokio::time::timeout(Duration::from_secs(10), ws.send(Message::Ping(vec![].into()))).await??;
                },
                message = rx.recv() => send(ws, &message.context("CONNECTION_CLOSED: control sender stopped")?).await?,
                message = ws.next() => match message.context("CONNECTION_CLOSED: control socket closed")?? {
                    Message::Ping(bytes) => { last = Instant::now(); ws.send(Message::Pong(bytes)).await?; },
                    Message::Pong(_) => last = Instant::now(),
                    Message::Text(text) => {
                        last = Instant::now();
                        let RelayMessage::Reject { session_id, error } = serde_json::from_str(&text)? else { bail!("INVALID_MESSAGE: unexpected control message") };
                        let mut c = app.connections.lock().await;
                        if let Some(s) = c.sessions.get_mut(&session_id) && s.network == network && s.target == id && s.generation == generation && let Some(sender) = s.claim.take() { let _ = sender.send(Err(*error)); }
                    },
                    _ => bail!("CONNECTION_CLOSED: control socket closed"),
                }
            }
        }
    }.await;
    let mut c = app.connections.lock().await;
    if c.controls
        .get(&key)
        .is_some_and(|v| v.generation == generation)
    {
        c.controls.remove(&key);
        for s in c.sessions.values() {
            if s.network == network && s.target == id {
                let _ = s.cancel.send(true);
            }
        }
    }
    result
}
async fn source_route(
    State(app): State<Arc<App>>,
    Path((network, target)): Path<(String, String)>,
    Extension(permit): Extension<TransportPermit>,
    Extension(peer): Extension<std::net::SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = source(&app, &network, &target, peer.ip(), &mut ws, &permit).await;
            finish(&mut ws, result).await;
        }))
}
async fn source(
    app: &App,
    network: &str,
    target: &str,
    source: std::net::IpAddr,
    ws: &mut WebSocket,
    permit: &TransportPermit,
) -> Result<()> {
    let path = format!("/networks/{network}/connect/{target}");
    let anonymous = authenticate(ws, network, &path, permit).await?.is_none();
    let sid = uuid::Uuid::new_v4().simple().to_string();
    let (tx, rx) = oneshot::channel();
    let (cancel, mut closed) = watch::channel(false);
    {
        let mut c = app.connections.lock().await;
        let to_target = |s: &&Session| s.network == network && s.target == target;
        if c.sessions.len() >= 4096
            || c.sessions.values().filter(to_target).count() >= 32
            || c.sessions.values().filter(|s| s.source == source).count() >= 16
            || (anonymous
                && c.sessions
                    .values()
                    .filter(to_target)
                    .filter(|s| s.anonymous)
                    .count()
                    >= 4)
        {
            bail!("SESSION_LIMIT: too many concurrent relay sessions")
        }
        let control = c
            .controls
            .get(&(network.into(), target.into()))
            .context("DEVICE_OFFLINE: target is offline")?;
        // Joining and renewal clients have no usable member certificate; they
        // may only reach the device that proved it holds the network root key.
        if anonymous && !control.manager {
            bail!("UNAUTHENTICATED: member proof required")
        }
        let generation = control.generation.clone();
        control
            .tx
            .try_send(RelayMessage::Incoming {
                session_id: sid.clone(),
            })
            .context("DEVICE_BUSY: control queue is full")?;
        c.sessions.insert(
            sid.clone(),
            Session {
                network: network.into(),
                source,
                anonymous,
                target: target.into(),
                generation,
                claim: Some(tx),
                cancel,
            },
        );
    }
    let result = async {
        let mut target = tokio::select! {
            value = tokio::time::timeout(Duration::from_secs(10), rx) => match value?? { Ok(ws) => ws, Err(Data::Error { code, message }) => bail!("{code}: {message}"), _ => bail!("INVALID_MESSAGE: invalid session rejection") },
            _ = closed.changed() => bail!("CONNECTION_CLOSED: session was cancelled"),
            _ = ws.next() => bail!("CONNECTION_CLOSED: source disconnected before session establishment"),
        };
        send(ws, &RelayMessage::Connected { flow_control: false }).await?;
        let result = bridge(ws, &mut target, &mut closed).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), target.close()).await;
        result
    }.await;
    app.connections.lock().await.sessions.remove(&sid);
    result
}
async fn attach_route(
    State(app): State<Arc<App>>,
    Path((network, target, generation, sid)): Path<(String, String, String, String)>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = async {
                // The generation and session ID reach only the authenticated
                // control connection, so a matching attach needs no new proof.
                let sender = {
                    let mut c = app.connections.lock().await;
                    let control = c
                        .controls
                        .get(&(network.clone(), target.clone()))
                        .context("INVALID_SESSION: target control is missing")?;
                    if control.generation != generation {
                        bail!("INVALID_SESSION: control binding mismatch")
                    }
                    let s = c
                        .sessions
                        .get_mut(&sid)
                        .context("INVALID_SESSION: session is missing or expired")?;
                    if s.network != network
                        || s.target != target
                        || s.generation != generation
                        || *s.cancel.borrow()
                    {
                        bail!("INVALID_SESSION: session binding mismatch")
                    }
                    s.claim
                        .take()
                        .context("INVALID_SESSION: session was already claimed")?
                };
                permit.authenticated();
                send(
                    &mut ws,
                    &RelayMessage::Connected {
                        flow_control: false,
                    },
                )
                .await?;
                sender
                    .send(Ok(ws))
                    .map_err(|_| anyhow::anyhow!("CONNECTION_CLOSED: source disappeared"))?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                tracing::debug!(%error, "target attachment failed");
            }
        }))
}
async fn bridge(
    source: &mut WebSocket,
    target: &mut WebSocket,
    closed: &mut watch::Receiver<bool>,
) -> Result<()> {
    let (source_tx, source_rx) = source.split();
    let (target_tx, target_rx) = target.split();
    tokio::select! {
        result=direction(source_rx,target_tx)=>result,
        result=direction(target_rx,source_tx)=>result,
        _=closed.changed()=>Ok(()),
    }
}
async fn direction<R, W>(mut reader: R, mut writer: W) -> Result<()>
where
    R: futures_util::Stream<Item = std::result::Result<Message, axum::Error>> + Unpin,
    W: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    loop {
        let message = tokio::time::timeout(Duration::from_secs(300), reader.next())
            .await?
            .context("CONNECTION_CLOSED: ciphertext stream ended")??;
        match &message {
            Message::Binary(bytes) if bytes.len() <= FILE_CHUNK => {}
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => return Ok(()),
            _ => bail!("INVALID_MESSAGE: relay only accepts encrypted binary frames"),
        }
        tokio::time::timeout(Duration::from_secs(300), writer.send(message)).await??;
    }
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
pub fn valid_route(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || matches!(c, b'2'..=b'7'))
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
        Ok(value) if valid_route(&value) => Ok(value),
        Ok(_) => bail!("INVALID_RELAY: invalid local relay route"),
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
