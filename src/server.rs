use crate::{config::ServerConfig, crypto, protocol::*, store::Store};
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc},
};
use tower::ServiceExt;

type ApiResult<T> = std::result::Result<Json<T>, (StatusCode, Json<ApiError>)>;
type AgentConnection = (String, mpsc::Sender<ServerMessage>);
type PendingLog = (String, mpsc::Sender<(Vec<LogEvent>, bool)>);

#[derive(Clone)]
struct Auth(Option<String>);

#[derive(Clone)]
struct App {
    db: Arc<Store>,
    keys: Arc<crypto::ServerKeys>,
    config: ServerConfig,
    agents: Arc<Mutex<HashMap<String, AgentConnection>>>,
    pending_logs: Arc<Mutex<HashMap<String, PendingLog>>>,
    events: broadcast::Sender<String>,
}

fn err(code: &str, message: impl ToString, status: StatusCode) -> (StatusCode, Json<ApiError>) {
    (
        status,
        Json(ApiError {
            error: ErrorData {
                code: code.into(),
                message: message.to_string(),
            },
        }),
    )
}

fn internal(e: impl ToString) -> (StatusCode, Json<ApiError>) {
    err("STORAGE_ERROR", e, StatusCode::INTERNAL_SERVER_ERROR)
}

fn authorized(app: &App, auth: &Auth) -> std::result::Result<Device, (StatusCode, Json<ApiError>)> {
    let fp = auth.0.as_ref().ok_or_else(|| {
        err(
            "UNAUTHENTICATED",
            "client certificate required",
            StatusCode::UNAUTHORIZED,
        )
    })?;
    let (d, revoked) = app
        .db
        .device_by_cert(fp)
        .map_err(internal)?
        .ok_or_else(|| {
            err(
                "UNAUTHENTICATED",
                "unknown certificate",
                StatusCode::UNAUTHORIZED,
            )
        })?;
    if revoked {
        return Err(err(
            "DEVICE_REVOKED",
            "device revoked",
            StatusCode::FORBIDDEN,
        ));
    }
    Ok(d)
}

fn can_access(
    app: &App,
    auth: &Auth,
    target: &str,
) -> std::result::Result<(Device, Device), (StatusCode, Json<ApiError>)> {
    let source = authorized(app, auth)?;
    let (dest, revoked) = app
        .db
        .device(target)
        .map_err(internal)?
        .ok_or_else(|| err("DEVICE_OFFLINE", "target not found", StatusCode::NOT_FOUND))?;
    if revoked || !dest.allow_from.contains(&source.device_id) {
        return Err(err(
            "SOURCE_NOT_ALLOWED",
            "target does not allow this source",
            StatusCode::FORBIDDEN,
        ));
    }
    Ok((source, dest))
}

fn check_version(h: &HeaderMap) -> std::result::Result<(), (StatusCode, Json<ApiError>)> {
    if h.get("x-xrun-version").and_then(|v| v.to_str().ok()) != Some(VERSION) {
        return Err(err(
            "VERSION_MISMATCH",
            format!("server version is {VERSION}"),
            StatusCode::BAD_REQUEST,
        ));
    }
    Ok(())
}

fn request_hash(r: &ExecRequest, target_id: &str, stdin: &[u8]) -> String {
    let value = serde_json::json!({"target":target_id,"program":r.program,"args":r.args,"cwd":r.cwd,"env":r.env,"timeout":r.timeout_seconds,"stdin_len":stdin.len(),"stdin_hash":hex::encode(Sha256::digest(stdin))});
    hex::encode(Sha256::digest(serde_json::to_vec(&value).unwrap()))
}

fn validate_name(name: &str) -> bool {
    let reserved = [
        "server", "agent", "join", "pair", "renew", "revoke", "ls", "info", "exec", "jobs", "job",
        "logs", "kill", "help", "version", "config", "login", "logout", "doctor", "update", "push",
        "pull",
    ];
    (1..=32).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !reserved.contains(&name)
}

async fn pair(
    State(app): State<App>,
    headers: HeaderMap,
    Json(req): Json<PairRequest>,
) -> ApiResult<PairResponse> {
    check_version(&headers)?;
    let csr = STANDARD
        .decode(&req.csr_base64)
        .map_err(|e| err("INVALID_REQUEST", e, StatusCode::BAD_REQUEST))?;
    let _ = rcgen::CertificateSigningRequestParams::from_der(&csr.clone().into())
        .map_err(|e| err("INVALID_REQUEST", e, StatusCode::BAD_REQUEST))?;
    use x509_parser::prelude::FromDer;
    let (_, parsed) = x509_parser::certification_request::X509CertificationRequest::from_der(&csr)
        .map_err(|e| err("INVALID_REQUEST", e, StatusCode::BAD_REQUEST))?;
    let key_fp = hex::encode(Sha256::digest(
        parsed.certification_request_info.subject_pki.raw,
    ));
    let token_hash = hex::encode(Sha256::digest(req.token.as_bytes()));
    let renew_id = app.db.token_renew_id(&token_hash).map_err(internal)?;
    let is_renew = renew_id.is_some();
    let (device_id, name) = if let Some(id) = renew_id {
        let (d, revoked) = app.db.device(&id).map_err(internal)?.ok_or_else(|| {
            err(
                "UNAUTHENTICATED",
                "renewal target missing",
                StatusCode::FORBIDDEN,
            )
        })?;
        if revoked {
            return Err(err(
                "DEVICE_REVOKED",
                "device revoked",
                StatusCode::FORBIDDEN,
            ));
        }
        (d.device_id, d.name)
    } else {
        let name = req
            .name
            .ok_or_else(|| err("INVALID_REQUEST", "name required", StatusCode::BAD_REQUEST))?;
        if !validate_name(&name) {
            return Err(err(
                "INVALID_REQUEST",
                "invalid or reserved device name",
                StatusCode::BAD_REQUEST,
            ));
        }
        (format!("dev_{}", uuid::Uuid::new_v4()), name)
    };
    let cert =
        crypto::issue_device_certificate(&app.keys.ca_pem, &app.keys.ca_key_pem, &csr, &device_id)
            .map_err(internal)?;
    let cert_fp = crypto::certificate_fingerprint(&cert).map_err(internal)?;
    let d = Device {
        device_id: device_id.clone(),
        name: name.clone(),
        os: String::new(),
        arch: String::new(),
        hostname: String::new(),
        agent_version: String::new(),
        execution_user: String::new(),
        home_dir: String::new(),
        default_cwd: String::new(),
        path: String::new(),
        online: false,
        last_seen_ms: None,
        allow_from: vec![],
        store_id: String::new(),
        boot_id: String::new(),
    };
    let response = PairResponse {
        device_id,
        name,
        cert_pem: cert,
        ca_pem: app.keys.ca_pem.clone(),
        server_url: app.config.public_url.clone(),
    };
    let response_json = serde_json::to_string(&response).map_err(internal)?;
    let old = app
        .db
        .consume_token(&token_hash, &response_json, &d, &cert_fp, &key_fp)
        .map_err(|e| err("INVALID_REQUEST", e, StatusCode::BAD_REQUEST))?;
    app.db
        .audit(
            "pair",
            serde_json::json!({"device_id":d.device_id,"name":d.name,"renew":is_renew}),
        )
        .map_err(internal)?;
    Ok(Json(if let Some(old) = old {
        serde_json::from_str(&old).map_err(internal)?
    } else {
        response
    }))
}

async fn renew(
    State(app): State<App>,
    auth: axum::Extension<Auth>,
    headers: HeaderMap,
    Json(req): Json<RenewRequest>,
) -> ApiResult<RenewResponse> {
    check_version(&headers)?;
    let device = authorized(&app, &auth.0)?;
    let csr = STANDARD
        .decode(req.csr_base64)
        .map_err(|e| err("INVALID_REQUEST", e, StatusCode::BAD_REQUEST))?;
    let _ = rcgen::CertificateSigningRequestParams::from_der(&csr.clone().into())
        .map_err(|e| err("INVALID_REQUEST", e, StatusCode::BAD_REQUEST))?;
    use x509_parser::prelude::FromDer;
    let (_, parsed) = x509_parser::certification_request::X509CertificationRequest::from_der(&csr)
        .map_err(|e| err("INVALID_REQUEST", e, StatusCode::BAD_REQUEST))?;
    let key_fp = hex::encode(Sha256::digest(
        parsed.certification_request_info.subject_pki.raw,
    ));
    let cert = crypto::issue_device_certificate(
        &app.keys.ca_pem,
        &app.keys.ca_key_pem,
        &csr,
        &device.device_id,
    )
    .map_err(internal)?;
    let cert_fp = crypto::certificate_fingerprint(&cert).map_err(internal)?;
    app.db
        .renew_cert(&device.device_id, &key_fp, &cert_fp)
        .map_err(|e| err("UNAUTHENTICATED", e, StatusCode::FORBIDDEN))?;
    app.db
        .audit("renew", serde_json::json!({"device_id":device.device_id}))
        .map_err(internal)?;
    Ok(Json(RenewResponse { cert_pem: cert }))
}

async fn devices(
    State(app): State<App>,
    auth: axum::Extension<Auth>,
    headers: HeaderMap,
) -> ApiResult<Vec<Device>> {
    check_version(&headers)?;
    authorized(&app, &auth.0)?;
    Ok(Json(app.db.list_devices().map_err(internal)?))
}
async fn device(
    State(app): State<App>,
    auth: axum::Extension<Auth>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Device> {
    check_version(&headers)?;
    authorized(&app, &auth.0)?;
    let (d, _) = app
        .db
        .device(&id)
        .map_err(internal)?
        .ok_or_else(|| err("DEVICE_OFFLINE", "device not found", StatusCode::NOT_FOUND))?;
    Ok(Json(d))
}

async fn submit(
    State(app): State<App>,
    auth: axum::Extension<Auth>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Job> {
    check_version(&headers)?;
    if body.len() > MAX_EXEC_BODY {
        return Err(err(
            "INVALID_REQUEST",
            "request too large",
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    let source = authorized(&app, &auth.0)?;
    let mut r: ExecRequest = serde_json::from_slice(&body)
        .map_err(|e| err("INVALID_REQUEST", e, StatusCode::BAD_REQUEST))?;
    let mut metadata = r.clone();
    metadata.stdin_base64 = None;
    if serde_json::to_vec(&metadata).map_err(internal)?.len() > 64 * 1024 {
        return Err(err(
            "INVALID_REQUEST",
            "execution metadata exceeds 64 KiB",
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    let stdin = STANDARD
        .decode(r.stdin_base64.as_deref().unwrap_or(""))
        .map_err(|e| err("INVALID_REQUEST", e, StatusCode::BAD_REQUEST))?;
    if stdin.len() > MAX_STDIN {
        return Err(err(
            "STDIN_TOO_LARGE",
            "stdin exceeds 1 MiB",
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    if r.program.is_empty()
        || r.program.contains('\0')
        || r.args.iter().any(|v| v.contains('\0'))
        || r.env
            .iter()
            .any(|(k, v)| k.is_empty() || k.contains(['\0', '=']) || v.contains('\0'))
        || r.timeout_seconds > 86400 * 30
    {
        return Err(err(
            "INVALID_REQUEST",
            "invalid command or environment",
            StatusCode::BAD_REQUEST,
        ));
    }
    if let Some(old) = app
        .db
        .job_by_request(&source.device_id, &r.request_id)
        .map_err(internal)?
    {
        can_access(&app, &auth.0, &old.target_device_id)?;
        let hash = request_hash(&r, &old.target_device_id, &stdin);
        if old.request_hash == hash {
            return Ok(Json(old));
        }
        return Err(err(
            "REQUEST_CONFLICT",
            "request ID used with different parameters",
            StatusCode::CONFLICT,
        ));
    }
    let (target, revoked) = app
        .db
        .device(&r.target_device_id)
        .map_err(internal)?
        .ok_or_else(|| err("DEVICE_OFFLINE", "target not found", StatusCode::NOT_FOUND))?;
    if revoked || !target.allow_from.contains(&source.device_id) {
        return Err(err(
            "SOURCE_NOT_ALLOWED",
            "target does not allow source",
            StatusCode::FORBIDDEN,
        ));
    }
    if target.os == "windows" {
        let mut names = std::collections::HashSet::new();
        if r.env
            .keys()
            .any(|name| !names.insert(name.to_ascii_lowercase()))
        {
            return Err(err(
                "INVALID_REQUEST",
                "duplicate Windows environment key",
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    let hash = request_hash(&r, &target.device_id, &stdin);
    r.target_device_id = target.device_id.clone();
    let (session, tx) = app
        .agents
        .lock()
        .unwrap()
        .get(&target.device_id)
        .cloned()
        .ok_or_else(|| {
            err(
                "DEVICE_OFFLINE",
                "target Agent offline",
                StatusCode::SERVICE_UNAVAILABLE,
            )
        })?;
    if target.store_id.is_empty() {
        return Err(err(
            "DEVICE_OFFLINE",
            "Agent not initialized",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }
    let now = now_ms();
    let mut j = Job {
        job_id: format!("job_{}", uuid::Uuid::new_v4()),
        request_id: r.request_id.clone(),
        source_device_id: source.device_id.clone(),
        target_device_id: target.device_id.clone(),
        request_hash: hash.clone(),
        program: r.program.clone(),
        args: r.args.clone(),
        cwd: r.cwd.clone(),
        state: JobState::Accepted,
        last_confirmed_state: JobState::Accepted,
        origin: "xrun".into(),
        exit_code: None,
        signal: None,
        duration_ms: None,
        last_seq: 0,
        output_complete: false,
        error: None,
        created_at_ms: now,
        updated_at_ms: now,
        dispatch_started: false,
        target_store_id: target.store_id.clone(),
    };
    // Persist the dispatch intent before putting a command on the Agent channel.
    // A crash between these steps must reconcile, never claim a safe local cancel.
    j.dispatch_started = true;
    if let Err(e) = app.db.insert_job(&j) {
        if let Some(old) = app
            .db
            .job_by_request(&source.device_id, &j.request_id)
            .map_err(internal)?
        {
            if old.request_hash == hash {
                return Ok(Json(old));
            }
            return Err(err(
                "REQUEST_CONFLICT",
                "request ID used with different parameters",
                StatusCode::CONFLICT,
            ));
        }
        return Err(internal(e));
    }
    app.db.audit("submit",serde_json::json!({"source_device_id":j.source_device_id,"target_device_id":j.target_device_id,"request_id":j.request_id,"job_id":j.job_id,"program":j.program,"args":j.args,"cwd":j.cwd})).map_err(internal)?;
    let msg = ServerMessage::Exec {
        job_id: j.job_id.clone(),
        source_device_id: source.device_id,
        store_id: target.store_id,
        session_id: session,
        request_hash: hash,
        request: Box::new(r),
    };
    let _ = tx.send(msg).await;
    Ok(Json(j))
}

#[derive(Deserialize)]
struct JobQuery {
    device: Option<String>,
    request_id: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}
async fn jobs(
    State(app): State<App>,
    auth: axum::Extension<Auth>,
    headers: HeaderMap,
    Query(q): Query<JobQuery>,
) -> ApiResult<Vec<Job>> {
    check_version(&headers)?;
    let source = authorized(&app, &auth.0)?;
    let limit = q.limit.unwrap_or(50);
    if limit == 0 || limit > 100 {
        return Err(err(
            "INVALID_REQUEST",
            "limit must be 1..100",
            StatusCode::BAD_REQUEST,
        ));
    }
    let device_id = if let Some(name) = q.device.as_deref() {
        Some(
            app.db
                .device(name)
                .map_err(internal)?
                .ok_or_else(|| err("DEVICE_OFFLINE", "device not found", StatusCode::NOT_FOUND))?
                .0
                .device_id,
        )
    } else {
        None
    };
    let items = app
        .db
        .jobs(
            &source.device_id,
            device_id.as_deref(),
            q.request_id.as_deref(),
            limit,
            q.offset.unwrap_or(0),
        )
        .map_err(internal)?
        .into_iter()
        .filter(|j| can_access(&app, &auth.0, &j.target_device_id).is_ok())
        .collect();
    Ok(Json(items))
}
async fn job(
    State(app): State<App>,
    auth: axum::Extension<Auth>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Job> {
    check_version(&headers)?;
    let j = app
        .db
        .job(&id)
        .map_err(internal)?
        .ok_or_else(|| err("JOB_NOT_FOUND", "job not found", StatusCode::NOT_FOUND))?;
    can_access(&app, &auth.0, &j.target_device_id)?;
    Ok(Json(j))
}
async fn cancel(
    State(app): State<App>,
    auth: axum::Extension<Auth>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Job> {
    check_version(&headers)?;
    let mut j = app
        .db
        .job(&id)
        .map_err(internal)?
        .ok_or_else(|| err("JOB_NOT_FOUND", "job not found", StatusCode::NOT_FOUND))?;
    let (source, _) = can_access(&app, &auth.0, &j.target_device_id)?;
    if j.state.terminal() {
        return Ok(Json(j));
    }
    if !j.dispatch_started {
        j.state = JobState::Canceled;
        j.last_confirmed_state = JobState::Canceled;
        j.output_complete = true;
        app.db.save_job(&j).map_err(internal)?;
        return Ok(Json(j));
    }
    let tx = app
        .agents
        .lock()
        .unwrap()
        .get(&j.target_device_id)
        .map(|(_, tx)| tx.clone())
        .ok_or_else(|| {
            err(
                "DEVICE_OFFLINE",
                "target offline; retry cancel after reconnect",
                StatusCode::SERVICE_UNAVAILABLE,
            )
        })?;
    tx.send(ServerMessage::Cancel {
        job_id: id,
        source_device_id: source.device_id.clone(),
    })
    .await
    .map_err(|_| {
        err(
            "DEVICE_OFFLINE",
            "target disconnected",
            StatusCode::SERVICE_UNAVAILABLE,
        )
    })?;
    let _=app.db.audit("cancel",serde_json::json!({"source_device_id":source.device_id,"target_device_id":j.target_device_id,"job_id":j.job_id}));
    Ok(Json(j))
}

#[derive(Deserialize)]
struct LogsQuery {
    after: Option<u64>,
    follow: Option<bool>,
}
async fn logs(
    State(app): State<App>,
    auth: axum::Extension<Auth>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<LogsQuery>,
    ws: WebSocketUpgrade,
) -> std::result::Result<Response, (StatusCode, Json<ApiError>)> {
    check_version(&headers)?;
    let j = app
        .db
        .job(&id)
        .map_err(internal)?
        .ok_or_else(|| err("JOB_NOT_FOUND", "job not found", StatusCode::NOT_FOUND))?;
    can_access(&app, &auth.0, &j.target_device_id)?;
    Ok(ws
        .max_message_size(1024 * 1024)
        .on_upgrade(move |socket| {
            log_socket(
                app,
                auth.0,
                id,
                q.after.unwrap_or(0),
                q.follow.unwrap_or(false),
                socket,
            )
        })
        .into_response())
}
async fn log_socket(
    app: App,
    auth: Auth,
    id: String,
    after: u64,
    follow: bool,
    mut socket: WebSocket,
) {
    let mut rx = app.events.subscribe();
    let mut seq = after;
    let target = match app.db.job(&id) {
        Ok(Some(j)) if can_access(&app, &auth, &j.target_device_id).is_ok() => j.target_device_id,
        _ => return,
    };
    let agent_tx = app
        .agents
        .lock()
        .unwrap()
        .get(&target)
        .map(|(_, tx)| tx.clone());
    let source = if agent_tx.is_some() {
        "agent"
    } else {
        "server_cache"
    };
    let (first_seq, last_seq) = app.db.log_range(&id).unwrap_or((None, None));
    if socket.send(Message::Text(serde_json::json!({"type":"log_source","source":source,"first_seq":first_seq,"last_seq":last_seq}).to_string().into())).await.is_err(){return;}
    if let Some(agent_tx) = agent_tx.as_ref() {
        match replay_agent(&app, &auth, &target, &id, seq, agent_tx, &mut socket).await {
            Ok(next) => seq = next,
            Err(_) => return,
        }
    } else {
        loop {
            let events = match app.db.logs(&id, seq) {
                Ok(v) => v,
                Err(_) => return,
            };
            let n = events.len();
            for e in events {
                seq = e.seq;
                if socket
                    .send(Message::Text(
                        serde_json::json!({"type":"output","event":e})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            if n < 16 {
                break;
            }
        }
    }
    if !follow {
        let _ = socket
            .send(Message::Text(
                serde_json::json!({"type":"snapshot_end"})
                    .to_string()
                    .into(),
            ))
            .await;
        return;
    }
    loop {
        if let Ok(Some(j)) = app.db.job(&id) {
            if can_access(&app, &auth, &j.target_device_id).is_err() {
                return;
            }
            if j.state.terminal() {
                if seq < j.last_seq
                    && let Some(agent_tx) = agent_tx.as_ref()
                {
                    let _ =
                        replay_agent(&app, &auth, &target, &id, seq, agent_tx, &mut socket).await;
                }
                let _ = socket
                    .send(Message::Text(
                        serde_json::json!({"type":"result","job":j})
                            .to_string()
                            .into(),
                    ))
                    .await;
                return;
            }
        }
        match tokio::time::timeout(std::time::Duration::from_secs(15), rx.recv()).await {
            Ok(Ok(changed)) if changed == id => {
                if let Ok(events) = app.db.logs(&id, seq) {
                    for e in events {
                        if e.seq != seq + 1 {
                            break;
                        }
                        seq = e.seq;
                        if socket
                            .send(Message::Text(
                                serde_json::json!({"type":"output","event":e})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            _ => {
                if socket.send(Message::Ping(vec![].into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn replay_agent(
    app: &App,
    auth: &Auth,
    target: &str,
    id: &str,
    after: u64,
    agent_tx: &mpsc::Sender<ServerMessage>,
    socket: &mut WebSocket,
) -> Result<u64> {
    let correlation_id = uuid::Uuid::new_v4().to_string();
    let (tx, mut pages) = mpsc::channel(16);
    app.pending_logs
        .lock()
        .unwrap()
        .insert(correlation_id.clone(), (id.to_owned(), tx));
    let result = async {
        agent_tx
            .send(ServerMessage::ReadLogs {
                correlation_id: correlation_id.clone(),
                job_id: id.to_owned(),
                after,
            })
            .await?;
        let mut seq = after;
        loop {
            let (events, done) =
                tokio::time::timeout(std::time::Duration::from_secs(10), pages.recv())
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Agent log stream closed"))?;
            if can_access(app, auth, target).is_err() {
                anyhow::bail!("source authorization changed");
            }
            let before = seq;
            for e in events {
                if e.seq <= seq {
                    continue;
                }
                seq = e.seq;
                socket
                    .send(Message::Text(
                        serde_json::json!({"type":"output","event":e})
                            .to_string()
                            .into(),
                    ))
                    .await?;
            }
            if done {
                break;
            }
            if seq == before {
                anyhow::bail!("Agent log page did not advance");
            }
            agent_tx
                .send(ServerMessage::ReadLogs {
                    correlation_id: correlation_id.clone(),
                    job_id: id.to_owned(),
                    after: seq,
                })
                .await?;
        }
        Ok(seq)
    }
    .await;
    app.pending_logs.lock().unwrap().remove(&correlation_id);
    result
}

async fn agent_ws(
    State(app): State<App>,
    auth: axum::Extension<Auth>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> std::result::Result<Response, (StatusCode, Json<ApiError>)> {
    check_version(&headers)?;
    let d = authorized(&app, &auth.0)?;
    Ok(ws
        .max_message_size(1024 * 1024)
        .on_upgrade(move |socket| agent_socket(app, d, socket))
        .into_response())
}
async fn agent_socket(app: App, mut d: Device, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let Some(Ok(Message::Text(raw))) = stream.next().await else {
        return;
    };
    let Ok(AgentMessage::Hello {
        agent_version,
        store_id,
        boot_id,
        os,
        arch,
        hostname,
        execution_user,
        home_dir,
        default_cwd,
        path,
        allow_from,
    }) = serde_json::from_str::<AgentMessage>(&raw)
    else {
        return;
    };
    if agent_version != VERSION {
        return;
    }
    d.agent_version = agent_version;
    d.store_id = store_id;
    d.boot_id = boot_id;
    d.os = os;
    d.arch = arch;
    d.hostname = hostname;
    d.execution_user = execution_user;
    d.home_dir = home_dir;
    d.default_cwd = default_cwd;
    d.path = path;
    d.allow_from = allow_from;
    d.online = true;
    d.last_seen_ms = Some(now_ms());
    if app.db.update_device(&d).is_err() {
        return;
    }
    let session = format!("session_{}", uuid::Uuid::new_v4());
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(64);
    app.agents
        .lock()
        .unwrap()
        .insert(d.device_id.clone(), (session.clone(), tx));
    if sink
        .send(Message::Text(
            serde_json::to_string(&ServerMessage::HelloAck {
                session_id: session.clone(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    for j in app
        .db
        .all_jobs()
        .unwrap_or_default()
        .into_iter()
        .filter(|j| {
            j.target_device_id == d.device_id
                && j.target_store_id == d.store_id
                && !j.state.terminal()
        })
    {
        let _ = sink
            .send(Message::Text(
                serde_json::to_string(&ServerMessage::ReconcileJob { job_id: j.job_id })
                    .unwrap()
                    .into(),
            ))
            .await;
    }
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
    let mut last_message = std::time::Instant::now();
    let mut last_record = std::time::Instant::now();
    loop {
        tokio::select! {
            Some(msg)=rx.recv()=>{
                if app.agents.lock().unwrap().get(&d.device_id).is_none_or(|(current,_)| current != &session){break;}
                if sink.send(Message::Text(serde_json::to_string(&msg).unwrap().into())).await.is_err(){break;}
            },
            Some(incoming)=stream.next()=>{
                match incoming {
                    Ok(Message::Text(s))=>{
                        if s.len()>1024*1024{break;}
                        if app.agents.lock().unwrap().get(&d.device_id).is_none_or(|(current,_)| current != &session){break;}
                        last_message=std::time::Instant::now();
                        if last_record.elapsed()>std::time::Duration::from_secs(15){d.last_seen_ms=Some(now_ms());let _=app.db.update_device(&d);last_record=std::time::Instant::now();}
                        if let Ok(m)=serde_json::from_str::<AgentMessage>(&s){handle_agent_message(&app,&d.device_id,&session,m);}
                    },
                    Ok(Message::Close(_))|Err(_)=>break,
                    _=>{}
                }
            },
            _=heartbeat.tick()=>{
                if app.agents.lock().unwrap().get(&d.device_id).is_none_or(|(s,_)| s != &session){break;}
                if last_message.elapsed()>std::time::Duration::from_secs(45){break;}
                if sink.send(Message::Text(serde_json::to_string(&ServerMessage::Ping).unwrap().into())).await.is_err(){break;}
            },
            else=>break
        }
    }
    let still_current = app
        .agents
        .lock()
        .unwrap()
        .get(&d.device_id)
        .is_some_and(|(s, _)| s == &session);
    if still_current {
        app.agents.lock().unwrap().remove(&d.device_id);
        d.online = false;
        d.last_seen_ms = Some(now_ms());
        let _ = app.db.update_device(&d);
        for mut j in app
            .db
            .all_jobs()
            .unwrap_or_default()
            .into_iter()
            .filter(|j| j.target_device_id == d.device_id && !j.state.terminal())
        {
            j.last_confirmed_state = j.state.clone();
            j.state = JobState::Unknown;
            j.updated_at_ms = now_ms();
            let _ = app.db.save_job(&j);
            let _ = app.events.send(j.job_id);
        }
    }
}

fn handle_agent_message(app: &App, target: &str, session: &str, msg: AgentMessage) {
    if app
        .agents
        .lock()
        .unwrap()
        .get(target)
        .is_none_or(|(s, _)| s != session)
    {
        return;
    }
    match msg {
        AgentMessage::State { job } | AgentMessage::ReconcileResult { job } => {
            if let Ok(Some(old)) = app.db.job(&job.job_id) {
                if old.target_device_id != target
                    || old.state.terminal()
                    || old.target_store_id != job.target_store_id
                {
                    return;
                }
                let mut updated = old;
                updated.state = job.state;
                updated.last_confirmed_state = job.last_confirmed_state;
                updated.origin = job.origin;
                updated.cwd = job.cwd.or(updated.cwd);
                updated.exit_code = job.exit_code;
                updated.signal = job.signal;
                updated.duration_ms = job.duration_ms;
                updated.last_seq = job.last_seq;
                updated.output_complete = job.output_complete;
                updated.error = job.error;
                updated.updated_at_ms = now_ms();
                if app.db.save_job(&updated).is_ok() {
                    if updated.state.terminal() {
                        let _=app.db.audit("result",serde_json::json!({"job_id":updated.job_id,"state":updated.state,"exit_code":updated.exit_code,"duration_ms":updated.duration_ms,"error":updated.error}));
                    }
                    let _ = app.events.send(updated.job_id);
                }
            }
        }
        AgentMessage::Output { event } => {
            if let Ok(Some(j)) = app.db.job(&event.job_id)
                && j.target_device_id == target
                && app.db.append_log(&event).is_ok()
            {
                let _ = app.events.send(event.job_id);
            }
        }
        AgentMessage::Logs {
            correlation_id,
            events,
            done,
        } => {
            if let Some((expected, tx)) = app.pending_logs.lock().unwrap().get(&correlation_id)
                && events.iter().all(|e| &e.job_id == expected)
            {
                let _ = tx.try_send((events, done));
            }
        }
        _ => {}
    }
}

#[cfg(unix)]
async fn admin(app: App) -> Result<()> {
    let path = app.config.data_dir.join("admin.sock");
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    loop {
        let (mut socket, _) = listener.accept().await?;
        let app = app.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = vec![0; 4096];
            let response = match socket.read(&mut buf).await {
                Ok(n) => {
                    let result = (|| -> Result<String> {
                        let value: serde_json::Value = serde_json::from_slice(&buf[..n])?;
                        match value["command"].as_str().context("missing command")? {
                            "pair" => {
                                let renew_id = if let Some(id) = value["renew"].as_str() {
                                    let (d, revoked) =
                                        app.db.device(id)?.context("device not found")?;
                                    if revoked {
                                        anyhow::bail!("device revoked");
                                    }
                                    Some(d.device_id)
                                } else {
                                    None
                                };
                                let mut token = [0u8; 32];
                                getrandom::fill(&mut token)?;
                                let plain = hex::encode(token);
                                let hash = hex::encode(Sha256::digest(plain.as_bytes()));
                                app.db
                                    .token(&hash, now_ms() + 600_000, renew_id.as_deref())?;
                                let ca = crypto::ca_link_value(&app.keys.ca_pem)?;
                                Ok(format!(
                                    "{}/pair#token={plain}&ca={}&cert={ca}",
                                    app.config.public_url, app.keys.ca_pin
                                ))
                            }
                            "revoke" => {
                                let id = value["device"].as_str().context("missing device")?;
                                let (mut d, _) = app.db.device(id)?.context("device not found")?;
                                d.online = false;
                                app.db.update_device(&d)?;
                                app.db.revoke(&d.device_id)?;
                                app.db.audit(
                                    "revoke",
                                    serde_json::json!({"device_id":d.device_id,"name":d.name}),
                                )?;
                                app.agents.lock().unwrap().remove(&d.device_id);
                                Ok(format!("revoked {}", d.device_id))
                            }
                            _ => anyhow::bail!("unknown admin command"),
                        }
                    })();
                    serde_json::json!({"ok":result.is_ok(),"value":result.unwrap_or_else(|e|e.to_string())}).to_string()
                }
                Err(e) => serde_json::json!({"ok":false,"value":e.to_string()}).to_string(),
            };
            let _ = socket.write_all(response.as_bytes()).await;
        });
    }
}

pub async fn run(config: ServerConfig) -> Result<()> {
    std::fs::create_dir_all(&config.data_dir)?;
    let server_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config.data_dir.join("server.lock"))?;
    server_lock
        .try_lock()
        .context("another Server instance is already running")?;
    let keys = Arc::new(crypto::load_or_create_server(&config)?);
    let tls_current = Arc::new(tokio::sync::RwLock::new(keys.tls_config.clone()));
    let renewal_tls = tls_current.clone();
    let renewal_config = config.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30 * 24 * 3600)).await;
            match crypto::load_or_create_server(&renewal_config) {
                Ok(keys) => *renewal_tls.write().await = keys.tls_config,
                Err(e) => tracing::error!(error=%e,"server TLS certificate renewal failed"),
            }
        }
    });
    let db = Arc::new(Store::open(&config.data_dir.join("server.sqlite"), false)?);
    db.prune_old_logs()?;
    let maintenance_db = db.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
            if let Err(e) = maintenance_db.prune_old_logs() {
                tracing::error!(error=%e,"log retention cleanup failed");
            }
        }
    });
    for mut d in db.list_devices()? {
        d.online = false;
        db.update_device(&d)?;
    }
    for mut j in db.all_jobs()?.into_iter().filter(|j| !j.state.terminal()) {
        j.last_confirmed_state = j.state.clone();
        j.state = JobState::Unknown;
        db.save_job(&j)?;
    }
    let (events, _) = broadcast::channel(2048);
    let app = App {
        db,
        keys,
        config: config.clone(),
        agents: Arc::new(Mutex::new(HashMap::new())),
        pending_logs: Arc::new(Mutex::new(HashMap::new())),
        events,
    };
    #[cfg(unix)]
    {
        tokio::spawn(admin(app.clone()));
    }
    let router = Router::new()
        .route("/pair", post(pair))
        .route("/devices/self/renew", post(renew))
        .route("/devices", get(devices))
        .route("/devices/{id}", get(device))
        .route("/jobs", post(submit).get(jobs))
        .route("/jobs/{id}", get(job))
        .route("/jobs/{id}/cancel", post(cancel))
        .route("/jobs/{id}/logs", get(logs))
        .route("/agent", get(agent_ws))
        .with_state(app.clone());
    let listener = TcpListener::bind(&config.listen).await?;
    tracing::info!(listen=%config.listen,"xrun server listening");
    loop {
        let (tcp, _) = listener.accept().await?;
        let acceptor = tokio_rustls::TlsAcceptor::from(tls_current.read().await.clone());
        let router = router.clone();
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(tcp).await else {
                return;
            };
            let fp = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|c| c.first())
                .map(|c| hex::encode(Sha256::digest(c.as_ref())));
            let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let router = router.clone();
                let fp = fp.clone();
                async move {
                    let mut req = req.map(Body::new);
                    req.extensions_mut().insert(Auth(fp));
                    let resp = router.oneshot(req).await.unwrap();
                    Ok::<_, std::convert::Infallible>(resp)
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(tls), service)
                .with_upgrades()
                .await;
        });
    }
}
