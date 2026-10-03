use crate::{
    config::ServerConfig,
    crypto,
    protocol::*,
    store::{Invitation, ServerStore},
};
use anyhow::{Context, Result, bail};
use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::HashMap,
    net::IpAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Auth(Option<(String, String)>);
struct Api(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for Api {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}
impl IntoResponse for Api {
    fn into_response(self) -> Response {
        let data = Data::error(&self.0);
        let status = if matches!(&data, Data::Error { code, .. } if code == "RATE_LIMITED") {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::BAD_REQUEST
        };
        (status, Json(data)).into_response()
    }
}
type ApiResult<T> = std::result::Result<T, Api>;

const MAX_CONNECTIONS: usize = 512;
const MAX_ANONYMOUS_PER_IP: usize = 16;
#[derive(Default)]
struct ConnectionLimits {
    total: usize,
    anonymous: HashMap<IpAddr, usize>,
}
// The permit belongs to the socket, including after a WebSocket upgrade.
struct LimitedTcp {
    inner: tokio::net::TcpStream,
    limits: Arc<std::sync::Mutex<ConnectionLimits>>,
    ip: IpAddr,
    anonymous: bool,
}
impl LimitedTcp {
    fn new(
        inner: tokio::net::TcpStream,
        ip: IpAddr,
        limits: Arc<std::sync::Mutex<ConnectionLimits>>,
    ) -> Option<Self> {
        {
            let mut counts = limits.lock().unwrap();
            if counts.total >= MAX_CONNECTIONS
                || counts.anonymous.get(&ip).copied().unwrap_or(0) >= MAX_ANONYMOUS_PER_IP
            {
                return None;
            }
            counts.total += 1;
            *counts.anonymous.entry(ip).or_default() += 1;
        }
        Some(Self {
            inner,
            limits,
            ip,
            anonymous: true,
        })
    }
    fn authenticated(&mut self) {
        if self.anonymous {
            self.limits.lock().unwrap().release_anonymous(self.ip);
            self.anonymous = false;
        }
    }
}
impl ConnectionLimits {
    fn release_anonymous(&mut self, ip: IpAddr) {
        let count = self.anonymous.get_mut(&ip).unwrap();
        *count -= 1;
        if *count == 0 {
            self.anonymous.remove(&ip);
        }
    }
}
impl Drop for LimitedTcp {
    fn drop(&mut self) {
        let mut counts = self.limits.lock().unwrap();
        counts.total -= 1;
        if self.anonymous {
            counts.release_anonymous(self.ip);
        }
    }
}
impl AsyncRead for LimitedTcp {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for LimitedTcp {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
struct ControlConnection {
    generation: String,
    tx: mpsc::Sender<Control>,
    cancel: watch::Sender<bool>,
}
struct Session {
    source: String,
    target: String,
    generation: String,
    target_tx: Option<oneshot::Sender<std::result::Result<WebSocket, Data>>>,
    cancel: watch::Sender<bool>,
}
#[derive(Default)]
struct Connections {
    controls: HashMap<String, ControlConnection>,
    sessions: HashMap<String, Session>,
}
struct App {
    store: ServerStore,
    keys: crypto::ServerKeys,
    connections: Mutex<Connections>,
    config: ServerConfig,
    pair_limits: Mutex<HashMap<std::net::IpAddr, (Instant, u32)>>,
}
fn version(headers: &HeaderMap) -> Result<()> {
    if headers.get("x-xrun-version").and_then(|v| v.to_str().ok()) != Some(VERSION) {
        bail!("VERSION_MISMATCH: all components must run {VERSION}")
    }
    Ok(())
}
fn authorized(app: &App, auth: &Auth) -> Result<crate::store::Registered> {
    let (id, key) = auth
        .0
        .as_ref()
        .context("UNAUTHENTICATED: device certificate required")?;
    let registered = app
        .store
        .get(id)?
        .context("UNKNOWN_DEVICE: unregistered certificate")?;
    if registered.key_fp != *key {
        bail!("UNAUTHENTICATED: certificate key does not match device")
    }
    if registered.device.revoked {
        bail!("DEVICE_REVOKED: identity has been revoked")
    }
    Ok(registered)
}
async fn pair(
    State(app): State<Arc<App>>,
    Extension(peer): Extension<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PairRequest>,
) -> ApiResult<Json<PairResponse>> {
    version(&headers)?;
    {
        let mut limits = app.pair_limits.lock().await;
        limits.retain(|_, (started, _)| started.elapsed() < Duration::from_secs(60));
        if limits.len() >= 4096 && !limits.contains_key(&peer.ip()) {
            return Err(Api(anyhow::anyhow!(
                "RATE_LIMITED: pairing rate limit reached"
            )));
        }
        let (_, attempts) = limits.entry(peer.ip()).or_insert((Instant::now(), 0));
        if *attempts >= 60 {
            return Err(Api(anyhow::anyhow!(
                "RATE_LIMITED: at most 60 pairing attempts per minute per IP"
            )));
        }
        *attempts += 1;
    }
    use base64::{Engine, engine::general_purpose::STANDARD};
    let csr = STANDARD.decode(req.csr_base64)?;
    let key = crypto::csr_key(&csr)?;
    let (registered, fresh) = app.store.register(&req.token, &req.name, &key)?;
    let cert = crypto::issue_device_certificate(
        &app.keys.ca_pem,
        &app.keys.ca_key_pem,
        &csr,
        &registered.device.device_id,
    )?;
    if fresh {
        app.store.audit("device_registered",serde_json::json!({"device_id":registered.device.device_id,"inviter_id":registered.registration.inviter_id}))?;
        if registered.registration.allow_inviter
            && let Some(inviter) = &registered.registration.inviter_id
        {
            let connections = app.connections.lock().await;
            if let Some(control) = connections.controls.get(inviter) {
                let _ = control.tx.try_send(Control::Grant {
                    device_id: registered.device.device_id.clone(),
                });
            }
        }
    }
    Ok(Json(PairResponse {
        device_id: registered.device.device_id,
        name: registered.device.name,
        cert_pem: cert,
        registration: registered.registration,
    }))
}
async fn invite(
    State(app): State<Arc<App>>,
    Extension(auth): Extension<Auth>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    version(&headers)?;
    let source = authorized(&app, &auth)?;
    let token = app.store.invite(&Invitation {
        inviter_id: Some(source.device.device_id),
        allow: req["allow"].as_bool().unwrap_or(false),
        admin: false,
    })?;
    let addresses = app.config.addresses.join(",");
    let link = format!(
        "xrun://{addresses}/{}#{token}",
        crypto::ca_spki_pin(&app.keys.ca_pem)?
    );
    Ok(Json(
        serde_json::json!({"link":link,"expires_in":600,"allow":req["allow"].as_bool().unwrap_or(false)}),
    ))
}
async fn devices(
    State(app): State<Arc<App>>,
    Extension(auth): Extension<Auth>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<Device>>> {
    version(&headers)?;
    authorized(&app, &auth)?;
    let connections = app.connections.lock().await;
    let mut devices = app.store.list()?;
    for d in &mut devices {
        d.online = !d.revoked && connections.controls.contains_key(&d.device_id)
    }
    Ok(Json(devices))
}
async fn device(
    State(app): State<Arc<App>>,
    Extension(auth): Extension<Auth>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Device>> {
    version(&headers)?;
    authorized(&app, &auth)?;
    let mut device = app
        .store
        .get(&id)?
        .context("UNKNOWN_DEVICE: device not registered")?
        .device;
    if device.revoked {
        return Err(Api(anyhow::anyhow!(
            "DEVICE_REVOKED: target has been revoked"
        )));
    }
    device.online = app
        .connections
        .lock()
        .await
        .controls
        .contains_key(&device.device_id);
    Ok(Json(device))
}
async fn revoke(
    State(app): State<Arc<App>>,
    Extension(auth): Extension<Auth>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    version(&headers)?;
    let source = authorized(&app, &auth)?;
    if !source.device.admin {
        return Err(Api(anyhow::anyhow!(
            "NOT_ADMIN: administrator identity required"
        )));
    }
    let id = req["device"]
        .as_str()
        .context("INVALID_REQUEST: missing device")?;
    let target = app.store.revoke(id)?;
    let mut connections = app.connections.lock().await;
    if let Some(control) = connections.controls.remove(&target.device.device_id) {
        let _ = control.cancel.send(true);
    }
    for s in connections.sessions.values() {
        if s.source == target.device.device_id || s.target == target.device.device_id {
            let _ = s.cancel.send(true);
        }
    }
    app.store.audit(
        "device_revoked",
        serde_json::json!({"source":source.device.device_id,"target":target.device.device_id}),
    )?;
    Ok(Json(
        serde_json::json!({"device_id":target.device.device_id,"revoked":true}),
    ))
}
async fn daemon(
    State(app): State<Arc<App>>,
    Extension(auth): Extension<Auth>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    let source = authorized(&app, &auth)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| control_loop(app, source.device.device_id, socket)))
}
async fn control_loop(app: Arc<App>, id: String, mut ws: WebSocket) {
    let hello = tokio::time::timeout(Duration::from_secs(10), ws.next()).await;
    let (os, arch, hostname, user, cwd) = match hello {
        Ok(Some(Ok(Message::Text(s)))) => match serde_json::from_str::<Control>(&s) {
            Ok(Control::Hello {
                version,
                os,
                arch,
                hostname,
                execution_user,
                default_cwd,
            }) if version == VERSION => (os, arch, hostname, execution_user, default_cwd),
            _ => return,
        },
        _ => return,
    };
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let (tx, mut rx) = mpsc::channel(16);
    let (cancel, mut closed) = watch::channel(false);
    {
        let mut c = app.connections.lock().await;
        if app
            .store
            .get(&id)
            .ok()
            .flatten()
            .is_none_or(|r| r.device.revoked)
        {
            return;
        }
        if let Some(old) = c.controls.insert(
            id.clone(),
            ControlConnection {
                generation: generation.clone(),
                tx,
                cancel,
            },
        ) {
            let _ = old.cancel.send(true);
        }
        for s in c.sessions.values() {
            if s.target == id {
                let _ = s.cancel.send(true);
            }
        }
    }
    let _ = app.store.metadata(&id, os, arch, hostname, user, cwd);
    let acknowledged = ws
        .send(Message::Text(
            serde_json::to_string(&Control::HelloAck).unwrap().into(),
        ))
        .await
        .is_ok();
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    let mut last = Instant::now();
    if acknowledged {
        loop {
            tokio::select! {
                _=closed.changed()=>break,
                _=tick.tick()=>{if last.elapsed()>Duration::from_secs(45) || app.store.get(&id).ok().flatten().is_none_or(|r|r.device.revoked){break}
                    if ws.send(Message::Ping(vec![].into())).await.is_err(){break}},
                message=rx.recv()=>match message{Some(m)=>if ws.send(Message::Text(serde_json::to_string(&m).unwrap().into())).await.is_err(){break},None=>break},
                message=ws.next()=>{match message {
                    Some(Ok(Message::Pong(_)))=>{last=Instant::now();let _=app.store.seen(&id);},
                    Some(Ok(Message::Ping(b)))=>{last=Instant::now();if ws.send(Message::Pong(b)).await.is_err(){break}},
                    Some(Ok(Message::Text(text)))=>{last=Instant::now();match serde_json::from_str::<Control>(&text){
                        Ok(Control::SessionReject{session_id,code})=>{let mut c=app.connections.lock().await;if let Some(s)=c.sessions.get_mut(&session_id)&& s.target==id&&s.generation==generation&& let Some(sender)=s.target_tx.take(){let _=sender.send(Err(Data::error(&anyhow::anyhow!(code))));}},
                        Ok(Control::GrantAck{device_id})=>{let _=app.store.audit("grant_ack",serde_json::json!({"target":id,"source":device_id}));},_=>break
                    }},_=>break
                }}
            }
        }
    }
    let mut c = app.connections.lock().await;
    if c.controls
        .get(&id)
        .is_some_and(|v| v.generation == generation)
    {
        c.controls.remove(&id);
        for s in c.sessions.values() {
            if s.target == id {
                let _ = s.cancel.send(true);
            }
        }
    }
    drop(c);
    let _ = tokio::time::timeout(Duration::from_secs(1), ws.close()).await;
}
async fn source_session(
    State(app): State<Arc<App>>,
    Extension(auth): Extension<Auth>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    let source = authorized(&app, &auth)?.device.device_id;
    let target = app
        .store
        .get(&id)?
        .context("UNKNOWN_DEVICE: device not registered")?
        .device;
    if target.revoked {
        return Err(Api(anyhow::anyhow!(
            "DEVICE_REVOKED: target identity revoked"
        )));
    }
    let target = target.device_id;
    let peers = (source.clone(), target.clone());
    let sid = uuid::Uuid::new_v4().simple().to_string();
    let (tx, rx) = oneshot::channel();
    let (cancel, closed) = watch::channel(false);
    {
        let mut c = app.connections.lock().await;
        authorized(&app, &auth)?;
        if app.store.get(&target)?.is_none_or(|r| r.device.revoked) {
            return Err(Api(anyhow::anyhow!(
                "DEVICE_REVOKED: target identity revoked"
            )));
        }
        if c.sessions.values().filter(|s| s.source == source).count() >= 16
            || c.sessions.values().filter(|s| s.target == target).count() >= 32
        {
            return Err(Api(anyhow::anyhow!(
                "SESSION_LIMIT: too many data sessions"
            )));
        }
        let control = c
            .controls
            .get(&target)
            .context("DEVICE_OFFLINE: target daemon offline")?;
        let generation = control.generation.clone();
        control
            .tx
            .try_send(Control::SessionRequest {
                session_id: sid.clone(),
                source_device_id: source.clone(),
            })
            .context("DEVICE_BUSY: control connection busy")?;
        c.sessions.insert(
            sid.clone(),
            Session {
                source,
                target,
                generation,
                target_tx: Some(tx),
                cancel,
            },
        );
    }
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| relay(app, sid, socket, rx, closed, peers)))
}
async fn target_session(
    State(app): State<Arc<App>>,
    Extension(auth): Extension<Auth>,
    headers: HeaderMap,
    Path(sid): Path<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    let target = authorized(&app, &auth)?.device.device_id;
    let sender = {
        let mut c = app.connections.lock().await;
        let generation = c
            .controls
            .get(&target)
            .context("INVALID_SESSION: missing control connection")?
            .generation
            .clone();
        let s = c
            .sessions
            .get_mut(&sid)
            .context("INVALID_SESSION: expired or unknown session")?;
        if s.target != target || s.generation != generation || *s.cancel.borrow() {
            return Err(Api(anyhow::anyhow!(
                "INVALID_SESSION: session binding mismatch"
            )));
        }
        s.target_tx
            .take()
            .context("INVALID_SESSION: session already claimed")?
    };
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| async move {
            let _ = sender.send(Ok(socket));
        }))
}
async fn relay(
    app: Arc<App>,
    sid: String,
    mut source: WebSocket,
    rx: oneshot::Receiver<std::result::Result<WebSocket, Data>>,
    mut closed: watch::Receiver<bool>,
    peers: (String, String),
) {
    let started = Instant::now();
    let mut source_bytes = 0u64;
    let mut target_bytes = 0u64;
    let target = tokio::select! {
        value=tokio::time::timeout(Duration::from_secs(10),rx)=>value.ok().and_then(|r|r.ok()),
        _=closed.changed()=>None,
        _=source.next()=>None,
    };
    let established = matches!(&target, Some(Ok(_)));
    if let Some(Ok(mut target)) = target {
        let activity = AtomicU64::new(0);
        let sent = AtomicU64::new(0);
        let received = AtomicU64::new(0);
        {
            let (source_tx, source_rx) = (&mut source).split();
            let (target_tx, target_rx) = (&mut target).split();
            let idle = async {
                loop {
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    if started
                        .elapsed()
                        .as_secs()
                        .saturating_sub(activity.load(Ordering::Relaxed))
                        >= 300
                    {
                        break;
                    }
                }
            };
            // A blocked writer in one direction must not stop reads in the
            // opposite direction (full-duplex uploads and responses).
            tokio::select! {
                _=relay_direction(source_rx,target_tx,&started,&activity,&sent)=>{},
                _=relay_direction(target_rx,source_tx,&started,&activity,&received)=>{},
                _=closed.changed()=>{},
                _=idle=>{},
            }
        }
        source_bytes = sent.load(Ordering::Relaxed);
        target_bytes = received.load(Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_secs(1), target.close()).await;
    } else {
        let error = target.and_then(|t| t.err()).unwrap_or(Data::Error {
            code: "SESSION_UNAVAILABLE".into(),
            message: "target did not establish an authorized data session".into(),
        });
        let _ = source
            .send(Message::Text(serde_json::to_string(&error).unwrap().into()))
            .await;
    }
    app.connections.lock().await.sessions.remove(&sid);
    if let Err(error) = app.store.audit("session_closed", serde_json::json!({"session_id":sid,"source_device_id":peers.0,"target_device_id":peers.1,"duration_ms":started.elapsed().as_millis() as u64,"source_bytes":source_bytes,"target_bytes":target_bytes,"established":established})) {
        tracing::error!(%error,"session audit could not be saved");
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), source.close()).await;
}
async fn relay_direction<R, W>(
    mut reader: R,
    mut writer: W,
    started: &Instant,
    activity: &AtomicU64,
    bytes: &AtomicU64,
) -> Result<()>
where
    R: futures_util::Stream<Item = std::result::Result<Message, axum::Error>> + Unpin,
    W: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    while let Some(message) = reader.next().await {
        let message = message?;
        activity.store(started.elapsed().as_secs(), Ordering::Relaxed);
        bytes.fetch_add(message_len(&message), Ordering::Relaxed);
        let close = matches!(message, Message::Close(_));
        tokio::time::timeout(Duration::from_secs(300), writer.send(message)).await??;
        if close {
            break;
        }
    }
    Ok(())
}

fn message_len(message: &Message) -> u64 {
    match message {
        Message::Text(text) => text.len() as u64,
        Message::Binary(bytes) => bytes.len() as u64,
        _ => 0,
    }
}

pub fn instance_lock(dir: &std::path::Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(dir)?;
    crate::config::restrict_dir(dir)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("server.lock"))?;
    lock.try_lock()
        .context("SERVER_RUNNING: server already running")?;
    Ok(lock)
}
pub async fn run(config: ServerConfig) -> Result<()> {
    let dir = &config.data_dir;
    let _lock = instance_lock(dir)?;
    let keys = crypto::load_or_create_server(&config)?;
    let acceptor = tokio_rustls::TlsAcceptor::from(keys.tls_config.clone());
    let app = Arc::new(App {
        store: ServerStore::open(&dir.join("server.db"))?,
        keys,
        connections: Mutex::new(Connections::default()),
        config: config.clone(),
        pair_limits: Mutex::new(HashMap::new()),
    });
    let router = Router::new()
        .route("/pair", post(pair))
        .route("/invites", post(invite))
        .route("/devices", get(devices))
        .route("/devices/{id}", get(device))
        .route("/admin/revoke", post(revoke))
        .route("/daemon", get(daemon))
        .route("/devices/{id}/session", get(source_session))
        .route("/daemon/sessions/{sid}", get(target_session))
        .layer(DefaultBodyLimit::max(MAX_MESSAGE))
        .with_state(app.clone());
    let listener =
        tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, config.port)).await?;
    tracing::info!(port = config.port, "xrun relay listening");
    let limits = Arc::new(std::sync::Mutex::new(ConnectionLimits::default()));
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                tracing::warn!(%error, "accept failed; retrying");
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
        };
        let Some(tcp) = LimitedTcp::new(tcp, peer.ip(), limits.clone()) else {
            continue;
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let mut tls =
                match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                    Ok(Ok(tls)) => tls,
                    Ok(Err(error)) => {
                        tracing::warn!(%peer,%error,"TLS authentication failed");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!(%peer,"TLS handshake timed out");
                        return;
                    }
                };
            let auth = Auth(
                tls.get_ref()
                    .1
                    .peer_certificates()
                    .and_then(|c| c.first())
                    .and_then(|c| crypto::peer_identity(c).ok()),
            );
            if authorized(&app, &auth).is_ok() {
                tls.get_mut().0.authenticated();
            }
            let service =
                hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let auth = auth.clone();
                    async move {
                        let mut req = req.map(Body::new);
                        req.extensions_mut().insert(auth);
                        req.extensions_mut().insert(peer);
                        let mut response = router.oneshot(req).await?;
                        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
                            response.headers_mut().insert(
                                axum::http::header::CONNECTION,
                                axum::http::HeaderValue::from_static("close"),
                            );
                        }
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });
            // Bound slow request bodies as well as idle HTTP connections.
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(Duration::from_secs(10));
            let _ = tokio::time::timeout(
                Duration::from_secs(30),
                builder
                    .serve_connection(hyper_util::rt::TokioIo::new(tls), service)
                    .with_upgrades(),
            )
            .await;
        });
    }
}
