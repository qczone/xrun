//! Daemon resources, startup, recovery and shutdown.
mod access;
mod control;
mod execution;
mod files;
mod jobs;
mod process_identity;
mod program;
mod requests;
mod session;
mod streams;

use crate::error::ErrorCode;
use crate::{
    config::{self, DaemonConfig, Identity},
    membership::RosterCache,
    net::{self},
    network,
    protocol::*,
    store::JobStore,
};
use anyhow::{Context, Result, bail};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, watch};

pub(crate) use self::control::maintain_membership;
use self::control::{control_reconnect, membership_sync};
use self::process_identity::{boot_id, process_start};

const FILE_OPERATION_LIMIT: usize = 8;

pub(crate) fn init() -> Result<()> {
    let id = Identity::load()?;
    let dir = config::device_dir()?;
    std::fs::create_dir_all(&dir)?;
    config::restrict_dir(&dir)?;
    let marker = dir.join("daemon.initialized");
    if !marker.exists() {
        JobStore::open(&dir.join("daemon.db"), true)?;
        config::atomic_private_write(&marker, b"2\n")?;
    } else {
        JobStore::open(&dir.join("daemon.db"), false)?;
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
        .context(ErrorCode::DaemonRunning.error("stop daemon before this operation"))?;
    Ok(file)
}
pub(crate) fn reset() -> Result<()> {
    let _lock = instance_lock()?;
    let dir = config::device_dir()?;
    for name in ["daemon.db", "daemon.db-wal", "daemon.db-shm"] {
        let path = dir.join(name);
        if path.exists() {
            std::fs::remove_file(path)?
        }
    }
    JobStore::open(&dir.join("daemon.db"), true)?;
    config::atomic_private_write(&dir.join("daemon.initialized"), b"2\n")?;
    Ok(())
}
struct Runtime {
    control: Arc<crate::control::Control>,
    id: Identity,
    members: RosterCache,
    access: watch::Sender<access::Snapshot>,
    access_scan: Mutex<()>,
    network_id: String,
    store: Arc<JobStore>,
    running: Mutex<HashMap<String, u32>>,
    jobs: Mutex<tokio::task::JoinSet<()>>,
    canceled: Mutex<HashSet<String>>,
    gate: Mutex<()>,
    sessions: Arc<Semaphore>,
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
        self.rt.canceled.lock().unwrap().remove(&self.id);
    }
}
struct OperationJob {
    store: Arc<JobStore>,
    job_id: String,
    finished: bool,
    attachment_error: Option<String>,
    leftover_possible: bool,
}
impl OperationJob {
    async fn retain_file(&mut self, path: &std::path::Path, name: String) {
        let cache = self.store.attachments();
        let path = path.to_owned();
        let result = tokio::task::spawn_blocking(move || cache.save_file(&path, &name))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|result| result);
        self.retained(result).await;
    }
    async fn retain_bytes(&mut self, bytes: Vec<u8>, name: String) {
        let cache = self.store.attachments();
        let result =
            tokio::task::spawn_blocking(move || cache.save(std::io::Cursor::new(bytes), &name))
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
        self.retained(result).await;
    }
    async fn retained(&mut self, result: Result<crate::attachments::AttachmentMetadata>) {
        let result = match result {
            Ok(metadata) => self.store.attach(&self.job_id, metadata).await,
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            tracing::warn!(%error,"job attachment could not be retained");
            let mut diagnostic = format!("{error:#}");
            crate::store::jobs::bound_diagnostic(&mut diagnostic);
            self.attachment_error = Some(diagnostic);
        }
    }
    fn finish(&mut self, state: JobState, mut result: JobResult) -> Result<()> {
        match &mut result {
            JobResult::File(value) => value.attachment_error = self.attachment_error.take(),
            JobResult::Screenshot(value) => value.attachment_error = self.attachment_error.take(),
            _ => {}
        }
        self.store.finish_sync(
            &self.job_id,
            crate::store::JobOutcome {
                result: Some(result),
                ..crate::store::JobOutcome::new(state)
            },
        )?;
        self.finished = true;
        Ok(())
    }
    fn cancel(&mut self) -> Result<()> {
        self.store.finish_sync(
            &self.job_id,
            crate::store::JobOutcome {
                error_code: Some("JOB_CANCELED".into()),
                error_message: Some("operation canceled by its source device".into()),
                ..crate::store::JobOutcome::new(JobState::Canceled)
            },
        )?;
        self.finished = true;
        Ok(())
    }
    fn fail(&mut self, error: &anyhow::Error) -> Result<()> {
        let mut outcome = crate::store::JobOutcome::failed(error);
        if crate::error::code(error).is_none_or(|code| {
            matches!(
                code,
                ErrorCode::Unconfirmed | ErrorCode::StorageError | ErrorCode::ConnectionClosed
            )
        }) {
            outcome.state = JobState::Lost;
            outcome.leftover_possible = self.leftover_possible;
        }
        self.store.finish_sync(&self.job_id, outcome)?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for OperationJob {
    fn drop(&mut self) {
        if !self.finished {
            let outcome = crate::store::JobOutcome {
                error_code: Some("RESULT_LOST".into()),
                error_message: Some("operation interrupted before its result was recorded".into()),
                leftover_possible: self.leftover_possible,
                ..crate::store::JobOutcome::new(JobState::Lost)
            };
            if let Err(error) = self.store.finish_detached(self.job_id.clone(), outcome) {
                tracing::error!(%error,"interrupted job could not be queued");
            }
        }
    }
}
impl Runtime {
    fn storage_failure(&self) -> anyhow::Error {
        anyhow::anyhow!(
            ErrorCode::StorageError.error(
                self.fatal
                    .lock()
                    .unwrap()
                    .as_deref()
                    .unwrap_or("daemon stopped")
                    .to_string()
            )
        )
    }
    fn config(&self) -> Result<DaemonConfig> {
        Ok(self.authorization()?.config.clone())
    }
    fn membership(&self, source: &str) -> Result<()> {
        let access = self.authorization()?;
        let roster = &access.roster;
        if roster.member(&self.id.device_id)?.revoked || roster.member(source)?.revoked {
            bail!(ErrorCode::DeviceRevoked.error("session member has been revoked"))
        }
        Ok(())
    }
    fn allow(&self, source: &str) -> Result<()> {
        self.membership(source)?;
        self.config()?.check_access(source)
    }
    fn check_session(&self, source: &str, generation: u64) -> Result<()> {
        if self.stopping.load(Ordering::SeqCst) {
            bail!(ErrorCode::DaemonStopping.error("daemon shutting down"));
        }
        self.membership(source)?;
        let cfg = self.config()?;
        cfg.check_access(source)?;
        if cfg.pause_generation != generation {
            bail!(ErrorCode::SessionClosed.error("remote access was paused; open a new session"))
        }
        Ok(())
    }
    fn cwd(&self) -> Result<PathBuf> {
        Ok(self.config()?.default_cwd.unwrap_or(config::home_dir()?))
    }
    async fn owned_job(&self, source: &str, id: &str) -> Result<Job> {
        let job = self
            .store
            .get_async(id)
            .await?
            .context(ErrorCode::JobNotFound.error("unknown or expired job"))?;
        if job.source_device_id != source {
            bail!(ErrorCode::SourceNotAllowed.error("job belongs to another source"))
        }
        Ok(job)
    }
}
pub(crate) async fn shutdown_signal() {
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
    let local_listener = crate::ipc::Listener::bind(&dir)?;
    let local = crate::pool::run(local_listener, control.clone());
    tokio::pin!(local);
    let mut id = Identity::load()?;
    tokio::select! { result=&mut local=>{return result}, _=net::renew_identity(&mut id)=>{}, result=control.shutdown()=>{result?;return Ok(())} }
    if !dir.join("daemon.initialized").exists() {
        bail!(ErrorCode::DaemonNotInitialized.error("run daemon install"))
    }
    let network_id = network::authority(&id)?.network_id.clone();
    network::current(&id)?;
    let members = network::cache()?;
    let store = Arc::new(JobStore::open(&dir.join("daemon.db"), false)?);
    store.prune()?;
    for mut job in store.unfinished()? {
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
        job.error_code = Some("RESULT_LOST".into());
        job.error_message = Some("RESULT_LOST: daemon stopped before recording result".into());
        job.updated_at_ms = now_ms();
        store.finish_sync(
            &job.job_id,
            crate::store::JobOutcome {
                error_code: Some("RESULT_LOST".into()),
                error_message: job.error_message,
                output_loss_reason: job.output_complete.map(|_| "DETACHED_OUTPUT".into()),
                leftover_possible: job.leftover_possible,
                ..crate::store::JobOutcome::new(job.state)
            },
        )?;
    }
    store.attachments().prune_orphans()?;
    let (stop, _) = watch::channel(false);
    let initial_access = Arc::new(access::Authorization {
        config: DaemonConfig::load()?,
        roster: members.load(&network_id)?,
        identity: id.clone(),
        files: None,
    });
    let (access, _) = watch::channel(Ok(initial_access));
    let rt = Arc::new(Runtime {
        control: control.clone(),
        id,
        members,
        access,
        access_scan: Mutex::new(()),
        network_id,
        store,
        running: Mutex::new(HashMap::new()),
        jobs: Mutex::new(tokio::task::JoinSet::new()),
        canceled: Mutex::new(HashSet::new()),
        gate: Mutex::new(()),
        sessions: Arc::new(Semaphore::new(32)),
        files: Arc::new(Semaphore::new(FILE_OPERATION_LIMIT)),
        forwards: Arc::new(Semaphore::new(32)),
        streams: AtomicUsize::new(0),
        stopping: AtomicBool::new(false),
        fatal: Mutex::new(None),
        stop,
    });
    let connector = control_reconnect(rt.clone());
    tokio::pin!(connector);
    let sync = membership_sync();
    tokio::pin!(sync);
    let mut stop = rt.stop.subscribe();
    let access_monitor = access::monitor(rt.clone(), dir);
    tokio::pin!(access_monitor);
    let outcome = tokio::select! {
        result=&mut local=>result,
        result=&mut connector=>result,
        result=&mut sync=>result,
        result=&mut access_monitor=>result,
        _=shutdown_signal()=>Ok(()),
        result=control.shutdown()=>result,
        _=stop.changed()=>Err(rt.storage_failure()),
    };
    rt.stopping.store(true, Ordering::SeqCst);
    rt.files.close();
    let _ = control.connected(false);
    let _ = rt.stop.send(true);
    let cleanup = tokio::time::timeout(Duration::from_secs(8), finish_tasks(rt.clone())).await;
    if !matches!(cleanup, Ok(Ok(()))) {
        for pid in rt.running.lock().unwrap().values() {
            crate::process::force_kill(*pid)
        }
    }
    cleanup.context(ErrorCode::DaemonStopTimeout.error("task cleanup did not finish"))??;
    if rt.fatal.lock().unwrap().is_some() {
        return Err(rt.storage_failure());
    }
    outcome
}

async fn finish_tasks(rt: Arc<Runtime>) -> Result<()> {
    // A submission already holding the gate may still be saving acceptance.
    // Collect its task only after it has registered the complete execution future.
    let runtime = rt.clone();
    let mut jobs = tokio::task::spawn_blocking(move || {
        let _gate = runtime.gate.lock().unwrap();
        std::mem::take(&mut *runtime.jobs.lock().unwrap())
    })
    .await?;
    if let Ok(ids) = rt.store.unfinished_ids().await {
        rt.canceled.lock().unwrap().extend(ids);
    }
    while let Some(result) = jobs.join_next().await {
        result?;
    }
    // Connection-bound execution has separate ownership, but must stop too.
    while !rt.running.lock().unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Canceled file requests retain their permits until their worker/capture
    // process has stopped and the operation's job result has been saved.
    while rt.files.available_permits() != FILE_OPERATION_LIMIT {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    rt.store.flush().await?;
    if !rt.store.unfinished_ids().await?.is_empty() {
        bail!(ErrorCode::StorageError.error("accepted tasks have no persisted final result"));
    }
    Ok(())
}
