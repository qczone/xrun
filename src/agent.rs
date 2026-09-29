use crate::{
    config::{AgentConfig, Identity, device_dir},
    crypto,
    protocol::*,
    store::Store,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{
        Message, client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig,
    },
};

struct Runtime {
    identity: Mutex<Identity>,
    config: AgentConfig,
    store: Arc<Store>,
    store_id: String,
    boot_id: String,
    outbound: Mutex<Option<mpsc::Sender<AgentMessage>>>,
    running: Mutex<HashMap<String, u32>>,
    control: Mutex<()>,
    canceled: Mutex<HashSet<String>>,
    _instance_lock: std::fs::File,
}

impl Runtime {
    async fn emit(&self, m: AgentMessage) {
        let tx = self.outbound.lock().unwrap().clone();
        if let Some(tx) = tx {
            let _ = tx.send(m).await;
        }
    }
    async fn save_state(&self, j: &Job) {
        if self.store.save_job(j).is_ok() {
            self.emit(AgentMessage::State { job: j.clone() }).await;
        }
    }
}

fn boot_id() -> String {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_else(|_| "unknown".into())
            .trim()
            .to_owned()
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        let name = CString::new("kern.boottime").unwrap();
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut size = std::mem::size_of::<libc::timeval>();
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                &mut tv as *mut _ as *mut _,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            format!("{}:{}", tv.tv_sec, tv.tv_usec)
        } else {
            "unknown".into()
        }
    }
    #[cfg(windows)]
    {
        "windows-job-object".into()
    }
}

pub fn init() -> Result<()> {
    let _ = Identity::load()?;
    let dir = device_dir()?;
    std::fs::create_dir_all(&dir)?;
    crate::config::restrict_dir(&dir)?;
    crate::config::atomic_private_write(&dir.join("agent.initialized"), b"1")?;
    Store::init_agent(&dir.join("agent.sqlite"))
}

pub async fn run() -> Result<()> {
    let lock_path = device_dir()?.join("agent.lock");
    let instance_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    instance_lock
        .try_lock()
        .context("another Agent instance is already running")?;
    let identity = Identity::load()?;
    let config = AgentConfig::load()?;
    let dir = device_dir()?;
    if !dir.join("agent.sqlite").exists() && !dir.join("agent.initialized").exists() {
        init()?;
    }
    let store = Arc::new(Store::open(&device_dir()?.join("agent.sqlite"), true)?);
    store.prune_old_logs()?;
    let store_id = store.store_id()?;
    let runtime = Arc::new(Runtime {
        identity: Mutex::new(identity),
        config,
        store,
        store_id,
        boot_id: boot_id(),
        outbound: Mutex::new(None),
        running: Mutex::new(HashMap::new()),
        control: Mutex::new(()),
        canceled: Mutex::new(HashSet::new()),
        _instance_lock: instance_lock,
    });
    // Interrupted jobs are never automatically restarted. Without proof that their whole process
    // group has gone, their state remains unknown and occupies capacity.
    for mut j in runtime.store.all_jobs()? {
        if !j.state.terminal() {
            j.last_confirmed_state = j.state.clone();
            j.state = if let Some((pid, boot)) = runtime.store.process(&j.job_id)? {
                if boot != runtime.boot_id || process_gone(pid) {
                    JobState::Lost
                } else {
                    JobState::Unknown
                }
            } else {
                #[cfg(windows)]
                {
                    JobState::Lost
                }
                #[cfg(unix)]
                {
                    JobState::Unknown
                }
            };
            if j.state == JobState::Lost {
                j.error = Some(ErrorData {
                    code: "RESULT_LOST".into(),
                    message: "process no longer exists; execution result was lost".into(),
                });
            }
            j.updated_at_ms = now_ms();
            runtime.store.save_job(&j)?;
        }
    }
    let audit = runtime.clone();
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            timer.tick().await;
            if let Err(e) = refresh_unknown(&audit).await {
                tracing::error!(error=%e,"unknown job reconciliation failed");
            }
        }
    });
    let mut delay = 1u64;
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        runtime.store.prune_old_logs()?;
        let outcome = tokio::select! {
            outcome=connect(runtime.clone())=>outcome,
            _=&mut shutdown=>{shutdown_jobs(&runtime).await?;return Ok(());}
        };
        match outcome {
            Ok(()) => delay = 1,
            Err(e) => {
                tracing::warn!(error=%e,"agent disconnected");
            }
        }
        tokio::select! {
            _=tokio::time::sleep(std::time::Duration::from_secs(delay))=>{},
            _=&mut shutdown=>{shutdown_jobs(&runtime).await?;return Ok(());}
        }
        delay = (delay * 2).min(30);
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {_ = tokio::signal::ctrl_c()=>{},_ = term.recv()=>{}}
    }
    #[cfg(windows)]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn shutdown_jobs(rt: &Runtime) -> Result<()> {
    {
        let _control = rt.control.lock().unwrap();
        for mut j in rt
            .store
            .all_jobs()?
            .into_iter()
            .filter(|j| !j.state.terminal())
        {
            if j.state == JobState::Unknown {
                continue;
            }
            if let Some(pid) = rt.running.lock().unwrap().get(&j.job_id).copied() {
                rt.canceled.lock().unwrap().insert(j.job_id);
                kill_tree(pid);
            } else {
                j.state = JobState::Canceled;
                j.last_confirmed_state = JobState::Canceled;
                j.output_complete = true;
                j.updated_at_ms = now_ms();
                rt.store.save_job(&j)?;
            }
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if rt
            .store
            .all_jobs()?
            .into_iter()
            .all(|j| j.state.terminal() || j.state == JobState::Unknown)
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    for pid in rt.running.lock().unwrap().values().copied() {
        force_kill_tree(pid);
    }
    Ok(())
}

async fn refresh_unknown(rt: &Runtime) -> Result<()> {
    for mut j in rt
        .store
        .all_jobs()?
        .into_iter()
        .filter(|j| j.state == JobState::Unknown)
    {
        let Some((pid, boot)) = rt.store.process(&j.job_id)? else {
            continue;
        };
        if boot != rt.boot_id || process_gone(pid) {
            j.state = JobState::Lost;
            j.error = Some(ErrorData {
                code: "RESULT_LOST".into(),
                message: "process no longer exists; execution result was lost".into(),
            });
            j.updated_at_ms = now_ms();
            rt.save_state(&j).await;
        }
    }
    Ok(())
}

async fn connect(runtime: Arc<Runtime>) -> Result<()> {
    let mut identity = runtime.identity.lock().unwrap().clone();
    if crypto::certificate_expiring(&identity.cert_pem, 30)? {
        match crypto::renew_identity(&mut identity).await {
            Ok(()) => *runtime.identity.lock().unwrap() = identity.clone(),
            Err(e) if !crypto::certificate_expiring(&identity.cert_pem, 0)? => {
                tracing::warn!(error=%e,"certificate renewal deferred")
            }
            Err(e) => return Err(e),
        }
    }
    let url = identity.server_url.replace("https://", "wss://") + "/agent";
    let mut req = url.into_client_request()?;
    req.headers_mut()
        .insert("x-xrun-version", HeaderValue::from_static(VERSION));
    let tls = crypto::client_tls_config(&identity)?;
    let ws_config = WebSocketConfig::default().max_message_size(Some(MAX_EXEC_BODY));
    let (socket, _) =
        connect_async_tls_with_config(req, Some(ws_config), false, Some(Connector::Rustls(tls)))
            .await?;
    let (mut write, mut read) = socket.split();
    let cwd = runtime
        .config
        .default_cwd
        .clone()
        .unwrap_or(crate::config::home_dir()?);
    let hello = AgentMessage::Hello {
        agent_version: VERSION.into(),
        store_id: runtime.store_id.clone(),
        boot_id: runtime.boot_id.clone(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        hostname: std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_default(),
        execution_user: std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default(),
        home_dir: crate::config::home_dir()?.display().to_string(),
        default_cwd: cwd.display().to_string(),
        path: runtime
            .config
            .env
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default(),
        allow_from: runtime.config.allow_from.clone(),
    };
    write
        .send(Message::Text(serde_json::to_string(&hello)?.into()))
        .await?;
    let ack = read.next().await.context("no hello ack")??;
    let ServerMessage::HelloAck { session_id } =
        serde_json::from_str::<ServerMessage>(ack.to_text()?)?
    else {
        bail!("expected hello ack");
    };
    let (tx, mut rx) = mpsc::channel::<AgentMessage>(128);
    *runtime.outbound.lock().unwrap() = Some(tx);
    let refresh = tokio::time::sleep(std::time::Duration::from_secs(24 * 3600));
    tokio::pin!(refresh);
    loop {
        tokio::select! {
            Some(m)=rx.recv()=>{write.send(Message::Text(serde_json::to_string(&m)?.into())).await?;},
            Some(msg)=read.next()=>{
                let msg=msg?;
                if let Message::Text(text)=msg {
                    if text.len()>MAX_EXEC_BODY{bail!("Agent message exceeds 2 MiB");}
                    let command:ServerMessage=serde_json::from_str(&text)?;
                    if matches!(command,ServerMessage::Ping){write.send(Message::Text(serde_json::to_string(&AgentMessage::Pong)?.into())).await?;continue;}
                    handle(runtime.clone(),&session_id,command).await?;
                }else if matches!(msg,Message::Close(_)){break;}
            },
            _=&mut refresh=>break,
            else=>break
        }
    }
    *runtime.outbound.lock().unwrap() = None;
    Ok(())
}

async fn handle(rt: Arc<Runtime>, session: &str, msg: ServerMessage) -> Result<()> {
    match msg {
        ServerMessage::Exec {
            job_id,
            source_device_id,
            store_id,
            session_id,
            request_hash,
            request,
        } => {
            if session_id != session || store_id != rt.store_id {
                return Ok(());
            }
            let device_id = rt.identity.lock().unwrap().device_id.clone();
            if request.target_device_id != device_id {
                return Ok(());
            }
            let now = now_ms();
            let mut j = Job {
                job_id: job_id.clone(),
                request_id: request.request_id.clone(),
                source_device_id: source_device_id.clone(),
                target_device_id: device_id,
                request_hash: request_hash.clone(),
                program: request.program.clone(),
                args: request.args.clone(),
                cwd: request.cwd.clone(),
                state: JobState::Starting,
                last_confirmed_state: JobState::Starting,
                origin: "xrun".into(),
                exit_code: None,
                signal: None,
                duration_ms: None,
                last_seq: 0,
                output_complete: false,
                error: None,
                created_at_ms: now,
                updated_at_ms: now,
                dispatch_started: true,
                target_store_id: rt.store_id.clone(),
            };
            if let Some(old) = rt.store.job(&job_id)? {
                if old
                    .error
                    .as_ref()
                    .is_some_and(|e| e.code == "NOT_DISPATCHED")
                {
                    rt.emit(AgentMessage::State { job: old }).await;
                    return Ok(());
                }
                if old.request_hash != request_hash {
                    bail!("agent job ID conflict");
                }
                rt.emit(AgentMessage::State { job: old }).await;
                return Ok(());
            }
            if !rt.config.allow_from.contains(&source_device_id) {
                fail(&mut j, "SOURCE_NOT_ALLOWED", "source not allowed");
                rt.store.insert_job(&j)?;
                rt.emit(AgentMessage::State { job: j }).await;
                return Ok(());
            }
            let occupied = rt
                .store
                .all_jobs()?
                .into_iter()
                .filter(|j| !j.state.terminal())
                .count();
            if occupied >= rt.config.max_concurrent_jobs {
                fail(&mut j, "DEVICE_BUSY", "agent concurrency limit reached");
                rt.store.insert_job(&j)?;
                rt.emit(AgentMessage::State { job: j }).await;
                return Ok(());
            }
            let input = match STANDARD.decode(request.stdin_base64.as_deref().unwrap_or("")) {
                Ok(v) if v.len() <= MAX_STDIN => v,
                _ => {
                    fail(&mut j, "STDIN_TOO_LARGE", "invalid stdin");
                    rt.store.insert_job(&j)?;
                    rt.emit(AgentMessage::State { job: j }).await;
                    return Ok(());
                }
            };
            rt.store.save_process(&job_id, 0, &rt.boot_id)?;
            rt.store.insert_job(&j)?;
            rt.emit(AgentMessage::State { job: j.clone() }).await;
            tokio::spawn(async move {
                if let Err(e) = execute(rt.clone(), j.clone(), *request, input).await
                    && let Ok(Some(mut current)) = rt.store.job(&job_id)
                    && !current.state.terminal()
                {
                    fail(&mut current, "SPAWN_FAILED", e);
                    rt.save_state(&current).await;
                }
            });
        }
        ServerMessage::Cancel {
            job_id,
            source_device_id,
        } => {
            let control = rt.control.lock().unwrap();
            let Some(mut j) = rt.store.job(&job_id)? else {
                return Ok(());
            };
            if !rt.config.allow_from.contains(&source_device_id) {
                return Ok(());
            }
            if j.state.terminal() {
                drop(control);
                rt.emit(AgentMessage::State { job: j }).await;
                return Ok(());
            }
            if let Some(pid) = rt.running.lock().unwrap().get(&job_id).copied() {
                rt.canceled.lock().unwrap().insert(job_id);
                kill_tree(pid);
                drop(control);
            } else {
                j.state = JobState::Canceled;
                j.last_confirmed_state = JobState::Canceled;
                j.output_complete = true;
                j.updated_at_ms = now_ms();
                rt.store.save_job(&j)?;
                drop(control);
                rt.emit(AgentMessage::State { job: j }).await;
            }
        }
        ServerMessage::ReconcileJob { job_id } => {
            let j = if let Some(j) = rt.store.job(&job_id)? {
                j
            } else {
                let now = now_ms();
                let j = Job {
                    job_id: job_id.clone(),
                    request_id: job_id.clone(),
                    source_device_id: "tombstone".into(),
                    target_device_id: rt.identity.lock().unwrap().device_id.clone(),
                    request_hash: String::new(),
                    program: String::new(),
                    args: vec![],
                    cwd: None,
                    state: JobState::Failed,
                    last_confirmed_state: JobState::Failed,
                    origin: "xrun".into(),
                    exit_code: None,
                    signal: None,
                    duration_ms: None,
                    last_seq: 0,
                    output_complete: true,
                    error: Some(ErrorData {
                        code: "NOT_DISPATCHED".into(),
                        message: "Agent has no record of this job".into(),
                    }),
                    created_at_ms: now,
                    updated_at_ms: now,
                    dispatch_started: false,
                    target_store_id: rt.store_id.clone(),
                };
                rt.store.insert_job(&j)?;
                j
            };
            rt.emit(AgentMessage::ReconcileResult { job: j }).await;
        }
        ServerMessage::ReadLogs {
            correlation_id,
            job_id,
            after,
        } => {
            let events = rt.store.logs(&job_id, after)?;
            let done = events.len() < 16;
            rt.emit(AgentMessage::Logs {
                correlation_id,
                events,
                done,
            })
            .await;
        }
        ServerMessage::Ping => rt.emit(AgentMessage::Pong).await,
        _ => {}
    }
    Ok(())
}

fn fail(j: &mut Job, code: &str, message: impl ToString) {
    j.state = JobState::Failed;
    j.last_confirmed_state = JobState::Failed;
    j.error = Some(ErrorData {
        code: code.into(),
        message: message.to_string(),
    });
    j.output_complete = true;
    j.updated_at_ms = now_ms();
}

fn resolve_program(program: &str, cwd: &Path, env: &BTreeMap<String, String>) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        return resolve_windows_program(program, cwd, env);
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
                return Ok(std::fs::canonicalize(path)?);
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
                return Ok(std::fs::canonicalize(candidate)?);
            }
        }
    }
    bail!("PROGRAM_NOT_FOUND: {program}")
}

async fn execute(rt: Arc<Runtime>, mut j: Job, request: ExecRequest, input: Vec<u8>) -> Result<()> {
    let cwd = PathBuf::from(
        request
            .cwd
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                rt.config
                    .default_cwd
                    .clone()
                    .unwrap_or(crate::config::home_dir().unwrap_or_default())
                    .display()
                    .to_string()
            }),
    );
    if !cwd.is_absolute() || !cwd.is_dir() {
        fail(
            &mut j,
            "INVALID_CWD",
            format!("invalid cwd: {}", cwd.display()),
        );
        rt.save_state(&j).await;
        return Ok(());
    }
    j.cwd = Some(cwd.display().to_string());
    let mut env = rt.config.env.clone();
    for (k, v) in &request.env {
        #[cfg(windows)]
        {
            env.retain(|existing, _| !existing.eq_ignore_ascii_case(k));
        }
        env.insert(k.clone(), v.clone());
    }
    let path = match resolve_program(&request.program, &cwd, &env) {
        Ok(v) => v,
        Err(e) => {
            let code = if e.to_string().starts_with("SHELL_REQUIRED") {
                "SHELL_REQUIRED"
            } else {
                "PROGRAM_NOT_FOUND"
            };
            fail(&mut j, code, e);
            rt.save_state(&j).await;
            return Ok(());
        }
    };
    let started = {
        let _control = rt.control.lock().unwrap();
        if rt
            .store
            .job(&j.job_id)?
            .is_some_and(|existing| existing.state == JobState::Canceled)
        {
            return Ok(());
        }
        let result = crate::process::spawn(&path, &request.args, &cwd, &env, &j.job_id);
        if let Ok(child) = &result {
            if let Err(e) = rt.store.save_process(&j.job_id, child.pid, &rt.boot_id) {
                crate::process::force_kill(child.pid);
                return Err(e);
            }
            rt.running
                .lock()
                .unwrap()
                .insert(j.job_id.clone(), child.pid);
        }
        result
    };
    let mut child = match started {
        Ok(v) => v,
        Err(e) => {
            fail(&mut j, "SPAWN_FAILED", e);
            rt.save_state(&j).await;
            return Ok(());
        }
    };
    let pid = child.pid;
    j.state = JobState::Running;
    j.last_confirmed_state = JobState::Running;
    j.origin = "process".into();
    j.updated_at_ms = now_ms();
    rt.save_state(&j).await;
    let start = crate::clock::elapsed_clock_ms()?;
    let seq = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let order = Arc::new(Mutex::new(()));
    let out = tokio::spawn(drain(
        rt.clone(),
        j.job_id.clone(),
        "stdout",
        child.stdout.take().unwrap(),
        seq.clone(),
        bytes.clone(),
        order.clone(),
    ));
    let err = tokio::spawn(drain(
        rt.clone(),
        j.job_id.clone(),
        "stderr",
        child.stderr.take().unwrap(),
        seq.clone(),
        bytes.clone(),
        order,
    ));
    let stdin = child.stdin.take().unwrap();
    let mut writer = tokio::spawn(async move {
        let mut stdin = stdin;
        let result = stdin.write_all(&input).await;
        drop(stdin);
        result
    });
    let mut timed_out = false;
    let mut was_canceled = false;
    let mut write_result = None;
    let mut stdin_error = false;
    let status = loop {
        tokio::select! {
            status=child.wait()=>break status?,
            result=&mut writer, if write_result.is_none()=>{
                let result=result?;
                stdin_error=result.as_ref().is_err_and(|e| e.kind()!=std::io::ErrorKind::BrokenPipe);
                write_result=Some(result);
                if stdin_error {break terminate_and_wait(&mut child,pid).await?;}
            },
            _=tokio::time::sleep(std::time::Duration::from_secs(1))=>{
                was_canceled=rt.canceled.lock().unwrap().contains(&j.job_id);
                timed_out=request.timeout_seconds!=0 && crate::clock::elapsed_clock_ms()?.saturating_sub(start)>=request.timeout_seconds*1000;
                if was_canceled||timed_out{break terminate_and_wait(&mut child,pid).await?;}
            }
        }
    };
    cleanup_group(pid).await;
    rt.running.lock().unwrap().remove(&j.job_id);
    let write_result = match write_result {
        Some(result) => result,
        None => writer.await?,
    };
    stdin_error |= write_result
        .as_ref()
        .is_err_and(|e| e.kind() != std::io::ErrorKind::BrokenPipe);
    out.await??;
    err.await??;
    j.last_seq = seq.load(Ordering::SeqCst);
    j.duration_ms = Some(crate::clock::elapsed_clock_ms()?.saturating_sub(start));
    j.output_complete = bytes.load(Ordering::SeqCst) <= 64 * 1024 * 1024;
    was_canceled |= rt.canceled.lock().unwrap().remove(&j.job_id);
    if was_canceled {
        j.state = JobState::Canceled;
    } else if timed_out {
        j.state = JobState::TimedOut;
        j.error = Some(ErrorData {
            code: "TIMED_OUT".into(),
            message: "execution timed out".into(),
        });
    } else if stdin_error {
        j.state = JobState::Failed;
        j.error = Some(ErrorData {
            code: "STDIN_IO_ERROR".into(),
            message: "failed to write stdin".into(),
        });
    } else {
        j.state = JobState::Exited;
        j.exit_code = status.code().map(i64::from);
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            j.signal = status.signal();
        }
    }
    j.last_confirmed_state = j.state.clone();
    j.updated_at_ms = now_ms();
    rt.save_state(&j).await;
    Ok(())
}

async fn drain<R: AsyncRead + Unpin>(
    rt: Arc<Runtime>,
    job_id: String,
    stream: &str,
    mut reader: R,
    seq: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    order: Arc<Mutex<()>>,
) -> Result<()> {
    let mut buf = [0u8; MAX_OUTPUT_CHUNK];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let total = bytes.fetch_add(n as u64, Ordering::SeqCst) + n as u64;
        if total > 64 * 1024 * 1024 {
            continue;
        }
        let _order = order.lock().unwrap();
        let event = LogEvent {
            job_id: job_id.clone(),
            seq: seq.fetch_add(1, Ordering::SeqCst) + 1,
            stream: stream.into(),
            data_base64: STANDARD.encode(&buf[..n]),
        };
        if !rt.store.append_log(&event)? {
            bytes.store(64 * 1024 * 1024 + 1, Ordering::SeqCst);
            continue;
        }
        if let Some(tx) = rt.outbound.lock().unwrap().as_ref() {
            let _ = tx.try_send(AgentMessage::Output { event });
        }
    }
    Ok(())
}

fn kill_tree(pid: u32) {
    crate::process::terminate(pid);
}

async fn terminate_and_wait(
    child: &mut crate::process::ManagedChild,
    pid: u32,
) -> Result<std::process::ExitStatus> {
    kill_tree(pid);
    match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
        Ok(status) => Ok(status?),
        Err(_) => {
            force_kill_tree(pid);
            Ok(child.wait().await?)
        }
    }
}

async fn cleanup_group(pid: u32) {
    kill_tree(pid);
    for _ in 0..50 {
        if group_gone(pid) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    force_kill_tree(pid);
}

fn force_kill_tree(pid: u32) {
    crate::process::force_kill(pid);
}

fn group_gone(pid: u32) -> bool {
    crate::process::gone(pid)
}

#[cfg(unix)]
fn process_gone(pid: u32) -> bool {
    pid != 0 && group_gone(pid)
}
#[cfg(windows)]
fn process_gone(pid: u32) -> bool {
    group_gone(pid)
}
