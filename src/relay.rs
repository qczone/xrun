use crate::{
    config::ServerConfig,
    crypto,
    membership::{self, SignedRoster},
    protocol::*,
    server::{self, TransportPermit},
};
use anyhow::{Context, Result, bail};
use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use rusqlite::{Connection, OptionalExtension, params};
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
    pub signature: String,
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
        id: &crate::config::Identity,
        network: &str,
        path: &str,
        nonce: &str,
    ) -> Result<Self> {
        Ok(Self {
            device_id: id.device_id.clone(),
            cert_pem: id.cert_pem.clone(),
            signature: membership::sign(
                &id.key_pem,
                "relay-proof",
                &ChallengeBinding {
                    network,
                    device: &id.device_id,
                    path,
                    nonce,
                },
            )?,
        })
    }
    pub fn verify(&self, roster: &SignedRoster, path: &str, nonce: &str) -> Result<()> {
        crypto::verify_member_certificate(&self.cert_pem, &roster.ca_pem, &self.device_id)?;
        let der = crypto::cert_der(&self.cert_pem)?;
        roster.peer(&der, Some(&self.device_id))?;
        if crypto::certificate_expiring(&self.cert_pem, 0)? {
            bail!("CERTIFICATE_EXPIRED: renew the member certificate")
        }
        membership::verify(
            &self.cert_pem,
            "relay-proof",
            &ChallengeBinding {
                network: &roster.roster.network_id,
                device: &self.device_id,
                path,
                nonce,
            },
            &self.signature,
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptAck {
    pub network: String,
    pub device_id: String,
    pub version: u64,
    pub hash: String,
    pub cert_pem: String,
    pub signature: String,
}
impl ReceiptAck {
    fn binding(&self) -> serde_json::Value {
        serde_json::json!({"network":self.network,"device_id":self.device_id,"version":self.version,"hash":self.hash})
    }
    pub fn create(id: &crate::config::Identity, roster: &SignedRoster) -> Result<Self> {
        let mut ack = Self {
            network: roster.roster.network_id.clone(),
            device_id: id.device_id.clone(),
            version: roster.roster.version,
            hash: roster.hash()?,
            cert_pem: id.cert_pem.clone(),
            signature: String::new(),
        };
        ack.signature = membership::sign(&id.key_pem, "roster-ack", &ack.binding())?;
        Ok(ack)
    }
    pub fn verify(&self, roster: &SignedRoster) -> Result<()> {
        if self.network != roster.roster.network_id
            || self.version != roster.roster.version
            || self.hash != roster.hash()?
        {
            bail!("INVALID_ACK: acknowledgement is for another roster")
        }
        let der = crypto::cert_der(&self.cert_pem)?;
        crypto::verify_member_certificate(&self.cert_pem, &roster.ca_pem, &self.device_id)?;
        roster.peer(&der, Some(&self.device_id))?;
        membership::verify(
            &self.cert_pem,
            "roster-ack",
            &self.binding(),
            &self.signature,
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelayMessage {
    Challenge {
        nonce: String,
    },
    Authenticate {
        proof: Proof,
    },
    Accepted {
        roster: SignedRoster,
    },
    Hello {
        version: String,
        os: String,
        arch: String,
        hostname: Option<String>,
        execution_user: Option<String>,
        default_cwd: Option<String>,
    },
    HelloAck,
    Incoming {
        session_id: String,
        source_hint: Option<String>,
    },
    Reject {
        session_id: String,
        error: Box<Data>,
    },
    RosterUpdate {
        roster: SignedRoster,
    },
    RosterAck {
        ack: ReceiptAck,
    },
    Connected,
    Status {
        devices: Vec<Device>,
        acks: Vec<ReceiptAck>,
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
async fn send(ws: &mut WebSocket, m: &RelayMessage) -> Result<()> {
    let text = serde_json::to_string(m)?;
    if text.len() > MAX_MESSAGE {
        bail!("MESSAGE_TOO_LARGE: relay metadata limit exceeded")
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
            _ => bail!("INVALID_MESSAGE: expected relay metadata"),
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
        let data = Data::error(&self.0);
        let status = if matches!(&data,Data::Error{code,..} if code=="RATE_LIMITED") {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::BAD_REQUEST
        };
        (status, Json(data)).into_response()
    }
}
type ApiResult<T> = std::result::Result<T, Api>;

struct Cache(std::sync::Mutex<Connection>);
impl Cache {
    fn open(path: &std::path::Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS networks(id TEXT PRIMARY KEY,data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS enrollment(hash TEXT PRIMARY KEY,expires INTEGER NOT NULL)",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self(std::sync::Mutex::new(db)))
    }
    fn get(&self, network: &str) -> Result<SignedRoster> {
        let db = self.0.lock().unwrap();
        let text: String = db
            .query_row("SELECT data FROM networks WHERE id=?1", [network], |r| {
                r.get(0)
            })
            .optional()?
            .context("UNKNOWN_NETWORK: network roster is not cached")?;
        Ok(serde_json::from_str(&text)?)
    }
    fn invite(&self) -> Result<String> {
        let token = crypto::random_token();
        let db = self.0.lock().unwrap();
        db.execute("DELETE FROM enrollment WHERE expires<=?1", [now_ms()])?;
        db.execute(
            "INSERT INTO enrollment VALUES(?1,?2)",
            params![sha256(token.as_bytes()), now_ms() + 600_000],
        )?;
        Ok(token)
    }
    fn publish(&self, network: &str, next: &SignedRoster, token: Option<&str>) -> Result<()> {
        next.verify(network)?;
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        let old: Option<String> = tx
            .query_row("SELECT data FROM networks WHERE id=?1", [network], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(old) = old {
            serde_json::from_str::<SignedRoster>(&old)?.check_successor(next)?;
        } else {
            let hash = sha256(token.unwrap_or_default().as_bytes());
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM enrollment WHERE hash=?1 AND expires>?2)",
                params![hash, now_ms()],
                |r| r.get(0),
            )?;
            if !valid {
                bail!("RELAY_ENROLLMENT_REQUIRED: use a fresh link from xrun relay invite")
            }
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM networks", [], |r| r.get(0))?;
            if count >= 128 {
                bail!("NETWORK_LIMIT: relay network limit reached")
            }
            tx.execute("DELETE FROM enrollment WHERE hash=?1", [hash])?;
        }
        tx.execute(
            "INSERT INTO networks VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![network, serde_json::to_string(next)?],
        )?;
        tx.commit()?;
        Ok(())
    }
}
struct ControlConnection {
    generation: String,
    tx: mpsc::Sender<RelayMessage>,
    cancel: watch::Sender<bool>,
    metadata: Device,
}
struct Session {
    network: String,
    source: Option<String>,
    target: String,
    generation: String,
    claim: Option<oneshot::Sender<std::result::Result<WebSocket, Data>>>,
    cancel: watch::Sender<bool>,
}
#[derive(Default)]
struct Connections {
    controls: HashMap<(String, String), ControlConnection>,
    sessions: HashMap<String, Session>,
    acks: HashMap<(String, String), ReceiptAck>,
}
struct App {
    cache: Cache,
    connections: Mutex<Connections>,
    publish_rates: Mutex<HashMap<std::net::IpAddr, (Instant, u32)>>,
}

async fn roster(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<SignedRoster>> {
    version(&headers)?;
    Ok(Json(app.cache.get(&network)?))
}
async fn publish(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    Extension(peer): Extension<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(next): Json<SignedRoster>,
) -> ApiResult<Json<serde_json::Value>> {
    version(&headers)?;
    {
        let mut rates = app.publish_rates.lock().await;
        rates.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(60));
        if rates.len() >= 4096 && !rates.contains_key(&peer.ip()) {
            bail_api("RATE_LIMITED: relay metadata rate table is full")?;
        }
        let (_, count) = rates.entry(peer.ip()).or_insert((Instant::now(), 0));
        if *count >= 60 {
            bail_api("RATE_LIMITED: too many roster publications")?;
        }
        *count += 1;
    }
    app.cache.publish(
        &network,
        &next,
        headers
            .get("x-xrun-enrollment")
            .and_then(|v| v.to_str().ok()),
    )?;
    let c = app.connections.lock().await;
    for ((net, id), control) in &c.controls {
        if net == &network {
            if next.member(id).is_ok_and(|m| m.revoked) {
                let _ = control.cancel.send(true);
            } else {
                let _ = control.tx.try_send(RelayMessage::RosterUpdate {
                    roster: next.clone(),
                });
            }
        }
    }
    for s in c.sessions.values() {
        if s.network == network
            && (next.member(&s.target).is_ok_and(|m| m.revoked)
                || s.source
                    .as_ref()
                    .is_some_and(|id| next.member(id).is_ok_and(|m| m.revoked)))
        {
            let _ = s.cancel.send(true);
        }
    }
    Ok(Json(serde_json::json!({"version":next.roster.version})))
}
fn bail_api(message: &str) -> ApiResult<()> {
    Err(Api(anyhow::anyhow!("{message}")))
}
async fn authenticate(
    app: &App,
    ws: &mut WebSocket,
    network: &str,
    path: &str,
    permit: &TransportPermit,
) -> Result<String> {
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
    let roster = app.cache.get(network)?;
    proof.verify(&roster, path, &nonce)?;
    permit.authenticated();
    send(ws, &RelayMessage::Accepted { roster }).await?;
    Ok(proof.device_id)
}

async fn status_route(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    app.cache.get(&network)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = async {
                authenticate(
                    &app,
                    &mut ws,
                    &network,
                    &format!("/networks/{network}/status"),
                    &permit,
                )
                .await?;
                let status = {
                    let c = app.connections.lock().await;
                    RelayMessage::Status {
                        devices: c
                            .controls
                            .iter()
                            .filter(|((n, _), _)| n == &network)
                            .map(|(_, v)| v.metadata.clone())
                            .collect(),
                        acks: c
                            .acks
                            .iter()
                            .filter(|((n, _), _)| n == &network)
                            .map(|(_, v)| v.clone())
                            .collect(),
                    }
                };
                send(&mut ws, &status).await
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
    app.cache.get(&network)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = control(&app, &network, &permit, &mut ws).await;
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
    permit: &TransportPermit,
    ws: &mut WebSocket,
) -> Result<()> {
    let id = authenticate(
        app,
        ws,
        network,
        &format!("/networks/{network}/control"),
        permit,
    )
    .await?;
    let RelayMessage::Hello {
        version,
        os,
        arch,
        hostname,
        execution_user,
        default_cwd,
    } = tokio::time::timeout(Duration::from_secs(5), receive(ws)).await??
    else {
        bail!("INVALID_MESSAGE: expected device information")
    };
    if version != VERSION {
        bail!("VERSION_MISMATCH: device release differs")
    }
    let roster = app.cache.get(network)?;
    let member = roster.member(&id)?;
    if member.revoked {
        bail!("DEVICE_REVOKED: member has been revoked")
    }
    let metadata = Device {
        device_id: id.clone(),
        name: member.name.clone(),
        online: true,
        admin: id == roster.roster.manager_id,
        revoked: false,
        os: Some(os),
        arch: Some(arch),
        version: Some(version),
        hostname,
        execution_user,
        default_cwd,
        last_seen: Some(now_ms()),
    };
    let key = (network.to_owned(), id.clone());
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let (tx, mut rx) = mpsc::channel(16);
    let (cancel, mut closed) = watch::channel(false);
    {
        let mut c = app.connections.lock().await;
        if let Some(old) = c.controls.insert(
            key.clone(),
            ControlConnection {
                generation: generation.clone(),
                tx,
                cancel,
                metadata,
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
    let result=async {
        send(ws,&RelayMessage::HelloAck).await?;
        let mut tick=tokio::time::interval(Duration::from_secs(15)); let mut last=Instant::now();
        loop {
            tokio::select! {
                _=closed.changed()=>return Ok(()),
                _=tick.tick()=>{
                    if last.elapsed()>Duration::from_secs(45) {bail!("CONTROL_TIMEOUT: member stopped responding")}
                    tokio::time::timeout(Duration::from_secs(10),ws.send(Message::Ping(vec![].into()))).await??;
                },
                message=rx.recv()=>send(ws,&message.context("CONNECTION_CLOSED: control sender stopped")?).await?,
                message=ws.next()=>match message.context("CONNECTION_CLOSED: control socket closed")?? {
                    Message::Ping(bytes)=>{last=Instant::now();ws.send(Message::Pong(bytes)).await?;},
                    Message::Pong(_)=>last=Instant::now(),
                    Message::Text(text)=>{
                        last=Instant::now();
                        match serde_json::from_str::<RelayMessage>(&text)? {
                            RelayMessage::Reject {session_id,error}=>{
                                let mut c=app.connections.lock().await;
                                if let Some(s)=c.sessions.get_mut(&session_id) && s.network==network && s.target==id && s.generation==generation && let Some(sender)=s.claim.take() {let _=sender.send(Err(*error));}
                            },
                            RelayMessage::RosterAck {ack}=>{
                                if ack.device_id!=id {bail!("INVALID_ACK: acknowledgement belongs to another member")}
                                ack.verify(&app.cache.get(network)?)?;
                                app.connections.lock().await.acks.insert(key.clone(),ack);
                            },
                            _=>bail!("INVALID_MESSAGE: unexpected control metadata"),
                        }
                    },
                    _=>bail!("CONNECTION_CLOSED: control socket closed"),
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
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    let roster = app.cache.get(&network)?;
    if roster.member(&target)?.revoked {
        bail_api("DEVICE_REVOKED: target has been revoked")?;
    }
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = source(&app, &network, &target, false, &permit, &mut ws).await;
            finish(&mut ws, result).await;
        }))
}
async fn pairing_route(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    let manager = app.cache.get(&network)?.roster.manager_id;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = source(&app, &network, &manager, true, &permit, &mut ws).await;
            finish(&mut ws, result).await;
        }))
}
async fn source(
    app: &App,
    network: &str,
    target: &str,
    pairing: bool,
    permit: &TransportPermit,
    ws: &mut WebSocket,
) -> Result<()> {
    let source = if pairing {
        send(
            ws,
            &RelayMessage::Accepted {
                roster: app.cache.get(network)?,
            },
        )
        .await?;
        None
    } else {
        Some(
            authenticate(
                app,
                ws,
                network,
                &format!("/networks/{network}/connect/{target}"),
                permit,
            )
            .await?,
        )
    };
    let sid = uuid::Uuid::new_v4().simple().to_string();
    let (tx, rx) = oneshot::channel();
    let (cancel, mut closed) = watch::channel(false);
    {
        let mut c = app.connections.lock().await;
        let roster = app.cache.get(network)?;
        if roster.member(target)?.revoked
            || source
                .as_ref()
                .is_some_and(|id| roster.member(id).is_ok_and(|m| m.revoked))
        {
            bail!("DEVICE_REVOKED: session member has been revoked")
        }
        if c.sessions
            .values()
            .filter(|s| s.network == network && s.target == target)
            .count()
            >= 32
            || source.as_ref().is_some_and(|id| {
                c.sessions
                    .values()
                    .filter(|s| s.network == network && s.source.as_ref() == Some(id))
                    .count()
                    >= 16
            })
            || (pairing
                && c.sessions
                    .values()
                    .filter(|s| s.network == network && s.source.is_none())
                    .count()
                    >= 4)
        {
            bail!("SESSION_LIMIT: too many concurrent relay sessions")
        }
        let control = c
            .controls
            .get(&(network.into(), target.into()))
            .context("DEVICE_OFFLINE: target is offline")?;
        let generation = control.generation.clone();
        control
            .tx
            .try_send(RelayMessage::Incoming {
                session_id: sid.clone(),
                source_hint: source.clone(),
            })
            .context("DEVICE_BUSY: control queue is full")?;
        c.sessions.insert(
            sid.clone(),
            Session {
                network: network.into(),
                source,
                target: target.into(),
                generation,
                claim: Some(tx),
                cancel,
            },
        );
    }
    let result=async {
        let mut target=tokio::select! {
            value=tokio::time::timeout(Duration::from_secs(10),rx)=>match value?? {Ok(ws)=>ws,Err(Data::Error {code,message})=>bail!("{code}: {message}"),_=>bail!("INVALID_MESSAGE: invalid session rejection")},
            _=closed.changed()=>bail!("CONNECTION_CLOSED: session was cancelled"),
            _=ws.next()=>bail!("CONNECTION_CLOSED: source disconnected before session establishment"),
        };
        send(ws,&RelayMessage::Connected).await?;
        let result=bridge(ws,&mut target,&mut closed).await;
        let _=tokio::time::timeout(Duration::from_secs(1),target.close()).await;
        result
    }.await;
    app.connections.lock().await.sessions.remove(&sid);
    result
}
async fn attach_route(
    State(app): State<Arc<App>>,
    Path((network, sid)): Path<(String, String)>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    app.cache.get(&network)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = async {
                let id = authenticate(
                    &app,
                    &mut ws,
                    &network,
                    &format!("/networks/{network}/attach/{sid}"),
                    &permit,
                )
                .await?;
                let sender = {
                    let mut c = app.connections.lock().await;
                    let generation = c
                        .controls
                        .get(&(network.clone(), id.clone()))
                        .context("INVALID_SESSION: target control is missing")?
                        .generation
                        .clone();
                    let s = c
                        .sessions
                        .get_mut(&sid)
                        .context("INVALID_SESSION: session is missing or expired")?;
                    if s.network != network
                        || s.target != id
                        || s.generation != generation
                        || *s.cancel.borrow()
                    {
                        bail!("INVALID_SESSION: session binding mismatch")
                    }
                    s.claim
                        .take()
                        .context("INVALID_SESSION: session was already claimed")?
                };
                send(&mut ws, &RelayMessage::Connected).await?;
                sender
                    .send(Ok(ws))
                    .map_err(|_| anyhow::anyhow!("CONNECTION_CLOSED: source disappeared"))?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                tracing::debug!(%error,"target attachment failed");
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
    let app = Arc::new(App {
        cache: Cache::open(&config.data_dir.join("relay.db"))?,
        connections: Mutex::new(Connections::default()),
        publish_rates: Mutex::new(HashMap::new()),
    });
    let router = Router::new()
        .route("/networks/{network}/roster", get(roster).post(publish))
        .route("/networks/{network}/status", get(status_route))
        .route("/networks/{network}/control", get(control_route))
        .route("/networks/{network}/connect/{target}", get(source_route))
        .route("/networks/{network}/pairing", get(pairing_route))
        .route("/networks/{network}/attach/{sid}", get(attach_route))
        .layer(DefaultBodyLimit::max(MAX_MESSAGE))
        .with_state(app);
    server::serve_http(
        config.port,
        tokio_rustls::TlsAcceptor::from(keys.tls_config),
        router,
    )
    .await
}
pub fn enrollment(config: &ServerConfig) -> Result<String> {
    let keys = crypto::load_or_create_server(config)?;
    let token = Cache::open(&config.data_dir.join("relay.db"))?.invite()?;
    Ok(format!(
        "xrun-relay://{}/{}#{token}",
        config.addresses.join(","),
        crypto::ca_spki_pin(&keys.ca_pem)?
    ))
}
