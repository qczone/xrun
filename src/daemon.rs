use crate::{
    config::{self, DaemonConfig, Identity},
    net::{self, Ws},
    protocol::*,
    store::TaskStore,
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::{Semaphore, watch},
};
use tokio_tungstenite::tungstenite::Message;

pub fn init() -> Result<()> {
    let id = Identity::load()?;
    let dir = config::device_dir()?;
    std::fs::create_dir_all(&dir)?;
    config::restrict_dir(&dir)?;
    let marker = dir.join("daemon.initialized");
    if !marker.exists() {
        TaskStore::open(&dir.join("daemon.db"), true)?;
        config::atomic_private_write(&marker, b"2\n")?;
    } else {
        TaskStore::open(&dir.join("daemon.db"), false)?;
    }
    if !dir.join("daemon.toml").exists() {
        let mut cfg = DaemonConfig::default();
        if id.registration.allow_inviter
            && let Some(inviter) = id.registration.inviter_id
        {
            cfg.allow_from.push(inviter)
        }
        cfg.save()?;
    }
    Ok(())
}
pub fn instance_lock() -> Result<std::fs::File> {
    let dir = config::device_dir()?;
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("daemon.lock"))?;
    file.try_lock()
        .context("DAEMON_RUNNING: stop daemon before this operation")?;
    Ok(file)
}
pub fn reset() -> Result<()> {
    let _lock = instance_lock()?;
    let dir = config::device_dir()?;
    for name in ["daemon.db", "daemon.db-wal", "daemon.db-shm"] {
        let path = dir.join(name);
        if path.exists() {
            std::fs::remove_file(path)?
        }
    }
    TaskStore::open(&dir.join("daemon.db"), true)?;
    config::atomic_private_write(&dir.join("daemon.initialized"), b"2\n")?;
    Ok(())
}
struct Runtime {
    control: Arc<crate::control::Control>,
    id: Identity,
    store: Arc<TaskStore>,
    running: Mutex<HashMap<String, u32>>,
    canceled: Mutex<HashSet<String>>,
    gate: Mutex<()>,
    files: Arc<Semaphore>,
    forwards: Arc<Semaphore>,
    streams: AtomicUsize,
    stopping: AtomicBool,
    fatal: Mutex<Option<String>>,
    stop: watch::Sender<bool>,
}
struct RunningJob {
    rt: Arc<Runtime>,
    id: String,
}
impl Drop for RunningJob {
    fn drop(&mut self) {
        self.rt.running.lock().unwrap().remove(&self.id);
    }
}
struct RunningStream {
    rt: Arc<Runtime>,
    id: String,
}
impl Drop for RunningStream {
    fn drop(&mut self) {
        self.rt.running.lock().unwrap().remove(&self.id);
        self.rt.streams.fetch_sub(1, Ordering::SeqCst);
    }
}
struct FileAudit {
    store: Arc<TaskStore>,
    value: serde_json::Value,
    completed: bool,
    stream_counts: Option<Arc<crate::streaming::Counts>>,
}
impl Drop for FileAudit {
    fn drop(&mut self) {
        self.value["ended_at_ms"] = serde_json::json!(now_ms());
        if let Some(counts) = &self.stream_counts {
            self.value["bytes"] = counts.snapshot();
        }
        self.value["result"] = serde_json::json!(if self.completed {
            "ok"
        } else {
            "failed_or_disconnected"
        });
        if let Err(error) = self.store.audit(self.value.clone()) {
            tracing::error!(%error,"operation audit could not be saved");
        }
    }
}
impl Runtime {
    fn config(&self) -> Result<DaemonConfig> {
        DaemonConfig::load()
    }
    fn allow(&self, source: &str) -> Result<()> {
        self.config()?.check_access(source)
    }
    fn check_session(&self, source: &str, generation: u64) -> Result<()> {
        let cfg = self.config()?;
        cfg.check_access(source)?;
        if cfg.pause_generation != generation {
            bail!("SESSION_CLOSED: remote access was paused; open a new session")
        }
        Ok(())
    }
    fn cwd(&self) -> Result<PathBuf> {
        Ok(self.config()?.default_cwd.unwrap_or(config::home_dir()?))
    }
    fn owned_job(&self, source: &str, id: &str) -> Result<Job> {
        let job = self
            .store
            .get(id)?
            .context("JOB_NOT_FOUND: unknown or expired job")?;
        if job.source_device_id != source {
            bail!("SOURCE_NOT_ALLOWED: job belongs to another source")
        }
        Ok(job)
    }
}
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install TERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
pub async fn run() -> Result<()> {
    let _lock = instance_lock()?;
    let dir = config::device_dir()?;
    let control = Arc::new(crate::control::Control::new(&dir)?);
    let mut id = Identity::load()?;
    tokio::select! { _=net::renew_identity(&mut id)=>{}, result=control.shutdown()=>{result?;return Ok(())} }
    if !dir.join("daemon.initialized").exists() {
        bail!("DAEMON_NOT_INITIALIZED: run daemon install")
    }
    let store = Arc::new(TaskStore::open(&dir.join("daemon.db"), false)?);
    store.prune()?;
    for mut job in store.all()? {
        if !job.state.terminal() {
            job.leftover_possible = true;
            if let Some(p) = &job.process {
                if !p.boot_id.is_empty()
                    && p.boot_id == boot_id()
                    && p.start.is_some()
                    && p.start == process_start(p.pid)
                {
                    crate::process::force_kill(p.pid);
                    job.leftover_possible = false;
                } else if p.boot_id != boot_id() {
                    job.leftover_possible = false;
                }
            }
            #[cfg(windows)]
            {
                job.leftover_possible = false;
            }
            job.state = JobState::Lost;
            job.error = Some("RESULT_LOST: daemon stopped before recording result".into());
            job.updated_at_ms = now_ms();
            store.save(&job)?;
        }
    }
    let (stop, _) = watch::channel(false);
    let rt = Arc::new(Runtime {
        control: control.clone(),
        id,
        store,
        running: Mutex::new(HashMap::new()),
        canceled: Mutex::new(HashSet::new()),
        gate: Mutex::new(()),
        files: Arc::new(Semaphore::new(8)),
        forwards: Arc::new(Semaphore::new(32)),
        streams: AtomicUsize::new(0),
        stopping: AtomicBool::new(false),
        fatal: Mutex::new(None),
        stop,
    });
    let connector = control_reconnect(rt.clone());
    tokio::pin!(connector);
    let mut stop = rt.stop.subscribe();
    let outcome = tokio::select! {
        result=&mut connector=>result,
        _=shutdown_signal()=>Ok(()),
        result=control.shutdown()=>result,
        _=stop.changed()=>Err(anyhow::anyhow!("STORAGE_ERROR: {}", rt.fatal.lock().unwrap().as_deref().unwrap_or("daemon stopped"))),
    };
    rt.stopping.store(true, Ordering::SeqCst);
    let _ = control.connected(false);
    let _ = rt.stop.send(true);
    if let Ok(jobs) = rt.store.all() {
        for job in jobs {
            if !job.state.terminal() {
                rt.canceled.lock().unwrap().insert(job.job_id.clone());
            }
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while !rt.running.lock().unwrap().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for pid in rt.running.lock().unwrap().values() {
        crate::process::force_kill(*pid)
    }
    outcome
}
async fn control_reconnect(rt: Arc<Runtime>) -> Result<()> {
    let mut delay = 1u64;
    loop {
        let started = tokio::time::Instant::now();
        let result = control_once(rt.clone()).await;
        rt.control.connected(false)?;
        if started.elapsed() > Duration::from_secs(45) {
            delay = 1;
        }
        if let Err(e) = result {
            tracing::warn!(error=%e,"daemon disconnected");
            if e.to_string().starts_with("DEVICE_REVOKED")
                || e.to_string().starts_with("VERSION_MISMATCH")
                || e.to_string().starts_with("IDENTITY_CHANGED")
            {
                return Err(e);
            }
        }
        let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) * delay * 1000 / 1024;
        tokio::time::sleep(Duration::from_millis(delay * 1000 - jitter)).await;
        delay = (delay * 2).min(30);
    }
}
async fn control_once(rt: Arc<Runtime>) -> Result<()> {
    let mut current = Identity::load()?;
    if current.device_id != rt.id.device_id {
        bail!("IDENTITY_CHANGED: restart daemon after replacing its identity");
    }
    net::renew_identity(&mut current).await?;
    let (mut ws, address) = net::websocket(&current, "/daemon").await?;
    net::send(
        &mut ws,
        &Control::Hello {
            version: VERSION.into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            hostname: std::env::var("HOSTNAME")
                .or_else(|_| std::env::var("COMPUTERNAME"))
                .ok(),
            execution_user: std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .ok(),
            default_cwd: Some(rt.cwd()?.to_string_lossy().into()),
        },
    )
    .await?;
    match net::receive::<Control>(&mut ws).await? {
        Control::HelloAck => {}
        _ => bail!("INVALID_MESSAGE: expected hello acknowledgement"),
    }
    rt.control.connected(true)?;
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    let mut last = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _=ping.tick()=>{if last.elapsed()>Duration::from_secs(45){bail!("CONTROL_TIMEOUT: relay stopped responding")}ws.send(Message::Ping(vec![].into())).await?;rt.store.prune()?;},
            message=ws.next()=>{match message.context("CONNECTION_CLOSED: control disconnected")?? {
                Message::Pong(_)=>last=tokio::time::Instant::now(),Message::Ping(b)=>{last=tokio::time::Instant::now();ws.send(Message::Pong(b)).await?},
                Message::Text(text)=>{last=tokio::time::Instant::now();match serde_json::from_str::<Control>(&text)?{
                    Control::SessionRequest{session_id,source_device_id}=>{
                        if let Err(e)=rt.allow(&source_device_id){net::send(&mut ws,&Control::SessionReject{session_id,code:e.to_string()}).await?;continue}
                        let rt=rt.clone();let address=address.clone();tokio::spawn(async move{let result=data_session(rt,&address,&session_id,&source_device_id).await;if let Err(e)=result{tracing::debug!(error=%e,"data session ended")}});
                    },
                    Control::Grant{device_id}=>{config::update_permission(&device_id,true)?;net::send(&mut ws,&Control::GrantAck{device_id}).await?},
                    _=>bail!("INVALID_MESSAGE: unexpected control message"),
                }},_=>bail!("CONNECTION_CLOSED: control disconnected")
            }}
        }
    }
}
async fn data_session(rt: Arc<Runtime>, address: &str, sid: &str, source: &str) -> Result<()> {
    let mut ws = tokio::time::timeout(
        Duration::from_secs(10),
        net::websocket_at(
            address,
            &format!("/daemon/sessions/{sid}"),
            crate::crypto::client_tls_config(&Identity::load()?)?,
        ),
    )
    .await??;
    let policy = rt.config()?;
    policy.check_access(source)?;
    let generation = policy.pause_generation;
    net::send(
        &mut ws,
        &Data::Ready {
            version: VERSION.into(),
            device_id: rt.id.device_id.clone(),
            db_id: rt.store.db_id.clone(),
            default_cwd: rt.cwd()?.to_string_lossy().into(),
        },
    )
    .await?;
    let mut stop = rt.stop.subscribe();
    let denied = async {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if rt.check_session(source, generation).is_err() {
                break;
            }
        }
    };
    let result = tokio::select! {
        r=serve(rt.clone(),source,generation,&mut ws)=>r,
        _=stop.changed()=>Err(anyhow::anyhow!("DAEMON_STOPPING: daemon shutting down")),
        // Close without claiming an operation was rejected: it may already have committed.
        _=denied=>Ok(()),
    };
    if let Err(e) = result {
        let _ = tokio::time::timeout(Duration::from_secs(1), net::send(&mut ws, &Data::error(&e)))
            .await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
    Ok(())
}
async fn serve(rt: Arc<Runtime>, source: &str, generation: u64, ws: &mut Ws) -> Result<()> {
    let req = tokio::time::timeout(Duration::from_secs(300), net::receive::<Data>(ws))
        .await?
        .context("INVALID_REQUEST: malformed request")?;
    rt.check_session(source, generation)?;
    let Data::Request { request } = req else {
        bail!("INVALID_MESSAGE: expected operation")
    };
    let file_op = match &request {
        Request::Push { path, .. } => Some(("push", Some(path.clone()))),
        Request::Pull { path, .. } => Some(("pull", Some(path.clone()))),
        Request::Screenshot => Some(("screenshot", None)),
        Request::Forward { port } => Some(("forward", Some(format!("localhost:{port}")))),
        Request::StreamExec { execution } => Some(("stream_exec", Some(execution.program.clone()))),
        _ => None,
    };
    let mut audit = file_op.map(|(op, path)| FileAudit {
        store: rt.store.clone(),
        value: serde_json::json!({"source_device_id":source,"op":op,"path":path,"size":null,"started_at_ms":now_ms()}),
        completed: false,
        stream_counts: None,
    });
    match request {
        Request::StreamExec { execution } => {
            let counts = Arc::new(crate::streaming::Counts::default());
            if let Some(a) = &mut audit {
                a.stream_counts = Some(counts.clone());
            }
            let cfg = rt.config()?;
            validate_stream(&execution)?;
            let cwd = PathBuf::from(&execution.cwd);
            let mut env = cfg.env;
            for (k, v) in &execution.env {
                #[cfg(windows)]
                env.retain(|key, _| !key.eq_ignore_ascii_case(k));
                env.insert(k.clone(), v.clone());
            }
            let program = resolve_program(&execution.program, &cwd, &env)?;
            let id = format!("stream_{}", uuid::Uuid::new_v4());
            let child = {
                let _gate = rt.gate.lock().unwrap();
                rt.check_session(source, generation)?;
                if rt.stopping.load(Ordering::SeqCst) {
                    bail!("DAEMON_STOPPING: daemon shutting down")
                }
                check_capacity(&rt)?;
                let child =
                    crate::process::spawn(&program, &execution.args, &cwd, &env, &id, false)?;
                rt.running.lock().unwrap().insert(id.clone(), child.pid);
                rt.streams.fetch_add(1, Ordering::SeqCst);
                child
            };
            let running = RunningStream { rt: rt.clone(), id };
            if let Some(a) = &mut audit {
                a.value["args"] = serde_json::json!(execution.args);
                a.value["cwd"] = serde_json::json!(execution.cwd);
                a.value["started_at_ms"] = serde_json::json!(now_ms());
            }
            let outcome =
                crate::streaming::serve(ws, child, execution.timeout, counts, move || {
                    drop(running)
                })
                .await?;
            if let Some(a) = &mut audit {
                a.completed = true;
                a.value["outcome"] = serde_json::to_value(outcome)?;
            }
        }
        Request::Forward { port } => {
            let _permit = rt
                .forwards
                .clone()
                .try_acquire_owned()
                .context("DEVICE_BUSY: too many forwarded connections")?;
            let tcp = crate::forwarding::connect_loopback(port).await?;
            rt.check_session(source, generation)?;
            net::send(ws, &Data::ForwardReady { port }).await?;
            crate::forwarding::bridge(ws, tcp).await?;
            if let Some(a) = &mut audit {
                a.completed = true;
            }
        }
        Request::Exec { execution } => {
            let input = net::receive_bytes(
                ws,
                execution.input_size,
                &execution.input_sha256,
                MAX_INPUT as u64,
            )
            .await?;
            rt.check_session(source, generation)?;
            let job = submit(rt, source, execution, input)?;
            net::send(ws, &Data::Job { job }).await?
        }
        Request::Jobs {
            id,
            running,
            request_id,
            limit,
            offset,
        } => {
            if let Some(id) = id {
                let job = rt.owned_job(source, &id)?;
                net::send(ws, &Data::Job { job }).await?
            } else {
                let jobs = rt
                    .store
                    .all()?
                    .into_iter()
                    .filter(|j| {
                        j.source_device_id == source
                            && (!running || !j.state.terminal())
                            && request_id.as_ref().is_none_or(|r| j.request_id == *r)
                    })
                    .skip(offset)
                    .take(limit.min(1000))
                    .collect();
                net::send(ws, &Data::Jobs { jobs }).await?
            }
        }
        Request::Kill { id } => {
            {
                let _gate = rt.gate.lock().unwrap();
                let job = rt.owned_job(source, &id)?;
                if !job.state.terminal() {
                    rt.canceled.lock().unwrap().insert(id.clone());
                }
            }
            follow(&rt, source, ws, &id, 0, false, true).await?
        }
        Request::Wait { id } => follow(&rt, source, ws, &id, 0, false, true).await?,
        Request::Logs {
            id,
            after,
            follow: following,
        } => follow(&rt, source, ws, &id, after, true, following).await?,
        Request::Push {
            path,
            cwd,
            size,
            sha256,
            mkdir,
            no_overwrite,
            expect,
        } => {
            let _permit = rt
                .files
                .clone()
                .try_acquire_owned()
                .context("DEVICE_BUSY: too many file operations")?;
            if no_overwrite && expect.is_some() {
                bail!("INVALID_REQUEST: expect conflicts with no-overwrite")
            }
            let contents = net::receive_file(ws, size, &sha256).await?;
            rt.check_session(source, generation)?;
            if let Some(a) = &mut audit {
                a.value["size"] = serde_json::json!(size);
            }
            let path = crate::transfer::remote_path(&path, &operation_cwd(&rt, cwd)?)?;
            let path = crate::transfer::push(path, contents, mkdir, no_overwrite, expect).await?;
            if let Some(a) = &mut audit {
                a.completed = true;
                a.value["path"] = serde_json::json!(path);
            }
            net::send(
                ws,
                &Data::File {
                    path: path.to_string_lossy().into(),
                    size,
                    sha256,
                    width: None,
                    height: None,
                    captured_at: None,
                },
            )
            .await?;
        }
        Request::Pull { path, cwd } => {
            let _permit = rt
                .files
                .clone()
                .try_acquire_owned()
                .context("DEVICE_BUSY: too many file operations")?;
            let path = crate::transfer::remote_path(&path, &operation_cwd(&rt, cwd)?)?;
            let (contents, size, hash) = crate::transfer::snapshot(path.clone()).await?;
            if let Some(a) = &mut audit {
                a.value["size"] = serde_json::json!(size);
                a.value["path"] = serde_json::json!(path);
            }
            net::send(
                ws,
                &Data::File {
                    path: path.to_string_lossy().into(),
                    size,
                    sha256: hash,
                    width: None,
                    height: None,
                    captured_at: None,
                },
            )
            .await?;
            net::send_file(ws, contents.as_file()).await?;
            if let Some(a) = &mut audit {
                a.completed = true;
            }
        }
        Request::Screenshot => {
            let _permit = rt
                .files
                .clone()
                .try_acquire_owned()
                .context("DEVICE_BUSY: too many file operations")?;
            let capture = crate::screenshot::capture().await?;
            if let Some(a) = &mut audit {
                a.value["size"] = serde_json::json!(capture.bytes.len());
                a.value["captured_at"] = serde_json::json!(capture.at);
            }
            net::send(
                ws,
                &Data::File {
                    path: String::new(),
                    size: capture.bytes.len() as u64,
                    sha256: sha256(&capture.bytes),
                    width: Some(capture.width),
                    height: Some(capture.height),
                    captured_at: Some(capture.at),
                },
            )
            .await?;
            net::send_bytes(ws, &capture.bytes).await?;
            if let Some(a) = &mut audit {
                a.completed = true;
            }
        }
    }
    Ok(())
}
async fn follow(
    rt: &Runtime,
    source: &str,
    ws: &mut Ws,
    id: &str,
    mut after: u64,
    logs: bool,
    following: bool,
) -> Result<()> {
    let mut ping = tokio::time::Instant::now();
    let mut previous = None;
    let snapshot = if following {
        None
    } else {
        Some(rt.owned_job(source, id)?.last_seq)
    };
    loop {
        rt.allow(source)?;
        let job = rt.owned_job(source, id)?;
        let mut events = if logs {
            rt.store.logs(id, after)?
        } else {
            vec![]
        };
        if let Some(last) = snapshot {
            events.retain(|e| e.seq <= last);
        }
        let count = events.len();
        if let Some(e) = events.last() {
            after = e.seq;
        }
        let state = (
            job.state.clone(),
            job.output_complete,
            job.incomplete_reason.clone(),
        );
        if logs && (count > 0 || previous.as_ref() != Some(&state)) {
            net::send(
                ws,
                &Data::Logs {
                    events,
                    job: job.clone(),
                },
            )
            .await?;
        } else if job.state.terminal() && !logs {
            net::send(ws, &Data::Job { job: job.clone() }).await?;
        }
        previous = Some(state);
        if (!following && count < 16) || (job.state.terminal() && (!logs || count < 16)) {
            if logs {
                net::send(ws, &Data::End).await?
            }
            return Ok(());
        }
        if ping.elapsed() > Duration::from_secs(15) {
            ws.send(Message::Ping(vec![].into())).await?;
            ping = tokio::time::Instant::now();
        }
        if count == 16 {
            continue;
        }
        tokio::select! {_=tokio::time::sleep(Duration::from_millis(100))=>{},m=ws.next()=>match m {Some(Ok(Message::Ping(b)))=>ws.send(Message::Pong(b)).await?,Some(Ok(Message::Pong(_)))=>{},_=>bail!("CONNECTION_CLOSED: subscriber disconnected")}}
    }
}
fn short_id() -> String {
    const ABC: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).unwrap();
    bytes
        .iter()
        .map(|b| ABC[(b & 31) as usize] as char)
        .collect()
}
fn validate_stream(request: &StreamExecution) -> Result<()> {
    if !Path::new(&request.cwd).is_absolute() || request.cwd.contains('\0') {
        bail!("INVALID_REQUEST: cwd must be absolute")
    }
    if request.program.is_empty()
        || request.program.contains('\0')
        || request.args.iter().any(|a| a.contains('\0'))
    {
        bail!("INVALID_REQUEST: invalid command")
    }
    if request
        .env
        .iter()
        .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
    {
        bail!("INVALID_REQUEST: invalid environment")
    }
    #[cfg(windows)]
    {
        let mut keys = HashSet::new();
        if request
            .env
            .keys()
            .any(|k| !keys.insert(k.to_ascii_lowercase()))
        {
            bail!("INVALID_REQUEST: duplicate Windows environment key")
        }
    }
    Ok(())
}
fn check_capacity(rt: &Runtime) -> Result<()> {
    let jobs = rt
        .store
        .all()?
        .iter()
        .filter(|j| !j.state.terminal())
        .count();
    if jobs + rt.streams.load(Ordering::SeqCst) >= rt.config()?.max_concurrent_jobs {
        bail!("DEVICE_BUSY: job capacity reached")
    }
    Ok(())
}
fn submit(rt: Arc<Runtime>, source: &str, request: Execution, input: Vec<u8>) -> Result<Job> {
    let _gate = rt.gate.lock().unwrap();
    rt.allow(source)?;
    if rt.stopping.load(Ordering::SeqCst) {
        bail!("DAEMON_STOPPING: daemon shutting down")
    }
    if request.db_id != rt.store.db_id {
        bail!("DB_RESET: original database no longer exists")
    }
    if request.request_id.is_empty() || request.request_id.len() > 128 {
        bail!("INVALID_REQUEST: invalid request-id")
    }
    if !Path::new(&request.cwd).is_absolute() || request.cwd.contains('\0') {
        bail!("INVALID_REQUEST: cwd must be absolute")
    }
    if request
        .env
        .iter()
        .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
    {
        bail!("INVALID_REQUEST: invalid environment")
    }
    if request.program.contains('\0') || request.args.iter().any(|a| a.contains('\0')) {
        bail!("INVALID_REQUEST: NUL in command")
    }
    #[cfg(windows)]
    {
        let mut names = HashSet::new();
        if request
            .env
            .keys()
            .any(|name| !names.insert(name.to_ascii_lowercase()))
        {
            bail!("INVALID_REQUEST: duplicate Windows environment key");
        }
    }
    if let Some(shell) = &request.shell {
        if !["sh", "bash", "zsh", "powershell", "pwsh", "cmd"].contains(&shell.as_str()) {
            bail!("SHELL_UNSUPPORTED: {shell}")
        }
        std::str::from_utf8(&input).context("INVALID_SCRIPT: script must be UTF-8")?;
        if shell == "cmd" && !input.is_ascii() {
            bail!("INVALID_SCRIPT: cmd requires ASCII")
        }
        if shell == "cmd"
            && request
                .args
                .iter()
                .any(|arg| arg.contains(['\"', '\r', '\n']))
        {
            bail!("INVALID_SCRIPT_ARGUMENT: cmd arguments cannot contain quotes or newlines")
        }
    }
    let hash = request.hash();
    if let Some(job) = rt.store.by_request(source, &request.request_id)? {
        if job.request_hash != hash {
            bail!("REQUEST_CONFLICT: request-id reused with different execution parameters")
        }
        return Ok(job);
    }
    check_capacity(&rt)?;
    let mut id = short_id();
    while rt.store.get(&id)?.is_some() {
        id = short_id()
    }
    let job = Job {
        job_id: id,
        request_id: request.request_id.clone(),
        request_hash: hash,
        source_device_id: source.into(),
        target_device_id: rt.id.device_id.clone(),
        db_id: rt.store.db_id.clone(),
        program: request.program.clone(),
        args: request.args.clone(),
        cwd: request.cwd.clone(),
        state: JobState::Starting,
        exit_code: None,
        signal: None,
        duration_ms: None,
        last_seq: 0,
        output_complete: true,
        incomplete_reason: None,
        error: None,
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
        leftover_possible: false,
        process: None,
    };
    rt.store.insert(&job)?;
    let background = rt.clone();
    let saved = job.clone();
    tokio::spawn(async move {
        if let Err(e) = execute(background.clone(), saved.clone(), request, input).await {
            crate::process::force_kill(
                background
                    .running
                    .lock()
                    .unwrap()
                    .remove(&saved.job_id)
                    .unwrap_or(0),
            );
            let recorded = (|| -> Result<()> {
                let mut job = background
                    .store
                    .get(&saved.job_id)?
                    .context("missing accepted task")?;
                job.state = JobState::Failed;
                job.error = Some(format!("EXECUTION_ERROR: {e:#}"));
                job.updated_at_ms = now_ms();
                background.store.save(&job)
            })();
            if let Err(error) = recorded {
                tracing::error!(%error, "task result could not be saved; stopping daemon");
                *background.fatal.lock().unwrap() = Some(error.to_string());
                background.stopping.store(true, Ordering::SeqCst);
                let _ = background.stop.send(true);
            }
        }
    });
    Ok(job)
}

async fn execute(rt: Arc<Runtime>, mut job: Job, request: Execution, input: Vec<u8>) -> Result<()> {
    let cfg = rt.config()?;
    let cwd = PathBuf::from(&request.cwd);
    let mut env = cfg.env;
    for (k, v) in request.env {
        #[cfg(windows)]
        env.retain(|key, _| !key.eq_ignore_ascii_case(&k));
        env.insert(k, v);
    }
    let mut program = request.program;
    let mut args = request.args;
    let cmd_script = request.shell.as_deref() == Some("cmd");
    let mut script = None;
    let mut stdin = input;
    if let Some(shell) = request.shell {
        let dir = config::device_dir()?.join("scripts");
        std::fs::create_dir_all(&dir)?;
        config::restrict_dir(&dir)?;
        let suffix = match shell.as_str() {
            "cmd" => ".cmd",
            "powershell" | "pwsh" => ".ps1",
            _ => ".sh",
        };
        let mut file = tempfile::Builder::new().suffix(suffix).tempfile_in(dir)?;
        let bytes = if shell == "powershell" && !stdin.starts_with(b"\xef\xbb\xbf") {
            [b"\xef\xbb\xbf".as_slice(), stdin.as_slice()].concat()
        } else if shell == "cmd" {
            String::from_utf8(stdin)?
                .replace("\r\n", "\n")
                .replace('\n', "\r\n")
                .into_bytes()
        } else {
            stdin
        };
        std::io::Write::write_all(&mut file, &bytes)?;
        file.as_file().sync_all()?;
        let path = file.path().to_string_lossy().to_string();
        let mut prefix = match shell.as_str() {
            "powershell" | "pwsh" => vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-File".into(),
                path,
            ],
            _ => vec![path],
        };
        prefix.append(&mut args);
        args = prefix;
        program = shell;
        stdin = vec![];
        // Close the write handle before the shell opens the script. TempPath
        // retains cleanup ownership without Windows file-sharing conflicts.
        script = Some(file.into_temp_path());
    }
    let resolved = resolve_program(&program, &cwd, &env)?;
    let mut child = {
        let _gate = rt.gate.lock().unwrap();
        if rt.canceled.lock().unwrap().contains(&job.job_id) || rt.stopping.load(Ordering::SeqCst) {
            job.state = JobState::Canceled;
            job.updated_at_ms = now_ms();
            rt.store.save(&job)?;
            return Ok(());
        }
        let child = crate::process::spawn(&resolved, &args, &cwd, &env, &job.job_id, cmd_script)?;
        job.process = Some(ProcessIdentity {
            pid: child.pid,
            boot_id: boot_id(),
            start: process_start(child.pid),
        });
        if let Err(e) = rt.store.save(&job) {
            crate::process::force_kill(child.pid);
            return Err(e);
        }
        rt.running
            .lock()
            .unwrap()
            .insert(job.job_id.clone(), child.pid);
        child
    };
    // Remove the PID before the child can be reaped, including error paths.
    let running = RunningJob {
        rt: rt.clone(),
        id: job.job_id.clone(),
    };
    let pid = child.pid;
    job.state = JobState::Running;
    job.updated_at_ms = now_ms();
    rt.store.save(&job)?;
    let incomplete = Arc::new(Mutex::new(None::<String>));
    let mut readers = vec![];
    for (stream, pipe) in [
        ("stdout", child.stdout.take()),
        ("stderr", child.stderr.take()),
    ] {
        if let Some(pipe) = pipe {
            readers.push(tokio::spawn(drain(
                rt.store.clone(),
                job.job_id.clone(),
                stream.into(),
                pipe,
                incomplete.clone(),
            )));
        }
    }
    let child_stdin = child.stdin.take();
    let mut writer = tokio::spawn(async move {
        if let Some(mut pipe) = child_stdin {
            let result = pipe.write_all(&stdin).await;
            if let Err(e) = result
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(e);
            }
            let _ = pipe.shutdown().await;
        }
        Ok::<_, std::io::Error>(())
    });
    // Keep the child outside the stdin task; only its pipe is moved.
    let start = crate::clock::elapsed_clock_ms()?;
    let mut reason = None;
    let status = loop {
        tokio::select! {
            status=child.wait()=>break status?,
            _=tokio::time::sleep(Duration::from_millis(100))=>{
                let canceled=rt.canceled.lock().unwrap().contains(&job.job_id)||rt.stopping.load(Ordering::SeqCst);
                let timed=request.timeout>0&&crate::clock::elapsed_clock_ms()?.saturating_sub(start)>=request.timeout.saturating_mul(1000);
                if canceled||timed{reason=Some(if canceled{JobState::Canceled}else{JobState::TimedOut});crate::process::terminate(pid);
                    let status=match tokio::time::timeout(Duration::from_secs(5),child.wait()).await{Ok(s)=>s?,Err(_)=>{crate::process::force_kill(pid);child.wait().await?}};break status;
                }
            }
        }
    };
    crate::process::terminate(pid);
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    for reader in &mut readers {
        if tokio::time::timeout_at(drain_deadline, &mut *reader)
            .await
            .is_err()
        {
            reader.abort();
            *incomplete.lock().unwrap() = Some("DETACHED_OUTPUT".into());
        }
    }
    if tokio::time::timeout_at(drain_deadline, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
    crate::process::force_kill(pid);
    drop(running);
    child.reap().await?;
    drop(script);
    rt.canceled.lock().unwrap().remove(&job.job_id);
    job.last_seq = rt.store.get(&job.job_id)?.context("job missing")?.last_seq;
    job.state = reason.unwrap_or(JobState::Exited);
    job.exit_code = status.code().map(i64::from);
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        job.signal = status.signal();
    }
    job.duration_ms = Some(crate::clock::elapsed_clock_ms()?.saturating_sub(start));
    job.incomplete_reason = incomplete.lock().unwrap().clone();
    job.output_complete = job.incomplete_reason.is_none();
    job.updated_at_ms = now_ms();
    rt.store.save(&job)?;
    Ok(())
}
async fn drain(
    store: Arc<TaskStore>,
    id: String,
    stream: String,
    mut pipe: Box<dyn AsyncRead + Unpin + Send>,
    incomplete: Arc<Mutex<Option<String>>>,
) {
    let mut bytes = vec![0u8; LOG_CHUNK];
    loop {
        match pipe.read(&mut bytes).await {
            Ok(0) => return,
            Ok(n) => match store.append(&id, &stream, &bytes[..n]) {
                Ok(Some(_)) => {}
                Ok(None) => *incomplete.lock().unwrap() = Some("TRUNCATED".into()),
                Err(e) => {
                    *incomplete.lock().unwrap() = Some(format!("CAPTURE_ERROR: {e}"));
                }
            },
            Err(e) => {
                *incomplete.lock().unwrap() = Some(format!("CAPTURE_ERROR: {e}"));
                return;
            }
        }
    }
}
fn boot_id() -> String {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_default()
            .trim()
            .into()
    }
    #[cfg(target_os = "macos")]
    {
        let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&tv);
        let rc = unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&mut tv as *mut libc::timeval).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            format!("{}:{}", tv.tv_sec, tv.tv_usec)
        } else {
            String::new()
        }
    }
    #[cfg(windows)]
    {
        "windows-job-object".into()
    }
}
fn process_start(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        s.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)
            .map(str::to_string)
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info);
        let n = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size as i32,
            )
        };
        if n == size as i32 {
            Some(format!(
                "{}:{}",
                info.pbi_start_tvsec, info.pbi_start_tvusec
            ))
        } else {
            None
        }
    }
    #[cfg(windows)]
    {
        let _ = pid;
        None
    }
}
fn resolve_program(program: &str, cwd: &Path, env: &BTreeMap<String, String>) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        resolve_windows_program(program, cwd, env)
    }
    #[cfg(unix)]
    {
        let p = Path::new(program);
        let mut candidates = vec![];
        if p.is_absolute() {
            candidates.push(p.to_path_buf());
        } else if program.contains(std::path::MAIN_SEPARATOR) {
            candidates.push(cwd.join(p));
        } else {
            let path = env
                .get("PATH")
                .cloned()
                .or_else(|| std::env::var("PATH").ok())
                .unwrap_or_default();
            for dir in std::env::split_paths(&path) {
                if !dir.as_os_str().is_empty() {
                    candidates.push(if dir.is_absolute() {
                        dir.join(p)
                    } else {
                        cwd.join(dir).join(p)
                    });
                }
            }
        }
        for path in candidates {
            if path.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if std::fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
                // Keep the invoked name/path: rustup and other command shims
                // dispatch on argv[0]. Let the OS follow executable symlinks.
                return Ok(path);
            }
        }
        bail!("PROGRAM_NOT_FOUND: {program}")
    }
}

#[cfg(windows)]
fn resolve_windows_program(
    program: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    let p = Path::new(program);
    let path_value = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let pathext = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATHEXT"))
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATHEXT").ok())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    let extensions: Vec<String> = pathext
        .split(';')
        .filter(|x| !x.is_empty())
        .map(|x| x.to_string())
        .collect();
    let bases: Vec<PathBuf> = if p.is_absolute() {
        vec![p.to_path_buf()]
    } else if program.contains(['\\', '/']) {
        vec![cwd.join(p)]
    } else {
        std::env::split_paths(&path_value)
            .filter(|d| !d.as_os_str().is_empty())
            .map(|d| {
                if d.is_absolute() {
                    d.join(p)
                } else {
                    cwd.join(d).join(p)
                }
            })
            .collect()
    };
    for base in bases {
        let candidates = if p.extension().is_some() {
            vec![base]
        } else {
            extensions
                .iter()
                .map(|ext| base.with_extension(ext.trim_start_matches('.')))
                .collect()
        };
        for candidate in candidates {
            if candidate.is_file() {
                let ext = candidate
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default();
                if ext.eq_ignore_ascii_case("bat") || ext.eq_ignore_ascii_case("cmd") {
                    bail!("SHELL_REQUIRED: {}", candidate.display());
                }
                // Preserve ordinary absolute paths for child applications such as
                // Windows PowerShell; canonicalize adds an incompatible \\?\ prefix.
                return Ok(std::path::absolute(candidate)?);
            }
        }
    }
    bail!("PROGRAM_NOT_FOUND: {program}")
}

fn operation_cwd(rt: &Runtime, cwd: Option<String>) -> Result<PathBuf> {
    let path = cwd.map(PathBuf::from).unwrap_or(rt.cwd()?);
    if !path.is_absolute() {
        bail!("INVALID_CWD: -C must be absolute")
    }
    Ok(path)
}
