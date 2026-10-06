//! Durable acceptance, process execution, cancellation and output capture.
use crate::error::ErrorCode;
use crate::{config, protocol::*, store::TaskStore};
use anyhow::{Context, Result, bail};
#[cfg(windows)]
use std::collections::HashSet;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::process_identity::{boot_id, process_start};
use super::program::resolve_program;
use super::{RunningJob, Runtime};

fn short_id() -> String {
    const ABC: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).unwrap();
    bytes
        .iter()
        .map(|b| ABC[(b & 31) as usize] as char)
        .collect()
}
pub(super) fn check_capacity(rt: &Runtime) -> Result<()> {
    let jobs = rt.store.active_count()?;
    if jobs + rt.streams.load(Ordering::SeqCst) >= rt.config()?.max_concurrent_jobs {
        bail!(ErrorCode::DeviceBusy.error("job capacity reached"))
    }
    Ok(())
}
pub(super) fn submit(
    rt: Arc<Runtime>,
    source: &str,
    request: Execution,
    input: Vec<u8>,
) -> Result<Job> {
    let _gate = rt.gate.lock().unwrap();
    rt.allow(source)?;
    if rt.stopping.load(Ordering::SeqCst) {
        bail!(ErrorCode::DaemonStopping.error("daemon shutting down"))
    }
    if request.db_id != rt.store.db_id {
        bail!(ErrorCode::DbReset.error("original database no longer exists"))
    }
    if request.request_id.is_empty() || request.request_id.len() > 128 {
        bail!(ErrorCode::InvalidRequest.error("invalid request-id"))
    }
    if !Path::new(&request.cwd).is_absolute() || request.cwd.contains('\0') {
        bail!(ErrorCode::InvalidRequest.error("cwd must be absolute"))
    }
    if request
        .env
        .iter()
        .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
    {
        bail!(ErrorCode::InvalidRequest.error("invalid environment"))
    }
    if request.program.contains('\0') || request.args.iter().any(|a| a.contains('\0')) {
        bail!(ErrorCode::InvalidRequest.error("NUL in command"))
    }
    #[cfg(windows)]
    {
        let mut names = HashSet::new();
        if request
            .env
            .keys()
            .any(|name| !names.insert(name.to_ascii_lowercase()))
        {
            bail!(ErrorCode::InvalidRequest.error("duplicate Windows environment key"));
        }
    }
    if let Some(shell) = &request.shell {
        if !["sh", "bash", "zsh", "powershell", "pwsh", "cmd"].contains(&shell.as_str()) {
            bail!(ErrorCode::ShellUnsupported.error(shell.to_string()))
        }
        std::str::from_utf8(&input)
            .context(ErrorCode::InvalidScript.error("script must be UTF-8"))?;
        if shell == "cmd" && !input.is_ascii() {
            bail!(ErrorCode::InvalidScript.error("cmd requires ASCII"))
        }
        if shell == "cmd"
            && request
                .args
                .iter()
                .any(|arg| arg.contains(['\"', '\r', '\n']))
        {
            bail!(
                ErrorCode::InvalidScriptArgument
                    .error("cmd arguments cannot contain quotes or newlines")
            )
        }
    }
    let hash = request.hash();
    if let Some(job) = rt.store.by_request(source, &request.request_id)? {
        if job.request_hash != hash {
            bail!(
                ErrorCode::RequestConflict
                    .error("request-id reused with different execution parameters")
            )
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
        background.canceled.lock().unwrap().remove(&saved.job_id);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{DaemonConfig, Identity, NetworkIdentity},
        membership::{Manager, RosterCache},
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use tokio::sync::{Semaphore, watch};

    #[test]
    fn cancellation_and_shutdown_before_first_poll_never_spawn_a_process() -> Result<()> {
        // Configuration uses HOME, so isolate the entire case in a child process.
        // Do not mutate the test runner's environment or the user's actual config.
        if std::env::var_os("XRUN_START_CANCEL_CHILD").is_none() {
            let home = tempfile::tempdir()?;
            let mut child = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "daemon::execution::tests::cancellation_and_shutdown_before_first_poll_never_spawn_a_process",
                "--nocapture",
            ])
            .env("XRUN_START_CANCEL_CHILD", "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while child.try_wait()?.is_none() {
                if std::time::Instant::now() > deadline {
                    child.kill()?;
                    child.wait()?;
                    bail!("cancel-before-spawn test timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output()?;
            anyhow::ensure!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let dir = config::device_dir()?;
                let (manager, member, key_pem, cert_pem) = Manager::create(
                    &dir.join("manager"),
                    "cancel-test",
                    vec!["https://127.0.0.1:1".into()],
                    String::new(),
                )?;
                let roster = manager.roster()?;
                let members = RosterCache::open(&dir.join("roster.db"))?;
                members.observe(&roster.roster.network_id, &roster)?;
                let id = Identity {
                    device_id: member.device_id,
                    name: member.name,
                    addresses: roster.roster.relay_addresses.clone(),
                    ca_pem: roster.ca_pem,
                    cert_pem,
                    key_pem,
                    registration: Registration {
                        inviter_id: None,
                        allow_inviter: false,
                    },
                    network: Some(NetworkIdentity {
                        network_id: roster.roster.network_id.clone(),
                        manager_id: roster.roster.manager_id,
                    }),
                };
                DaemonConfig {
                    allow_from: vec![id.device_id.clone()],
                    ..Default::default()
                }
                .save()?;
                let rt = Arc::new(Runtime {
                    control: Arc::new(crate::control::Control::new(&dir)?),
                    access: watch::channel(Ok(Arc::new(super::super::access::Authorization {
                        files: None,
                        config: DaemonConfig::load()?,
                        identity: id.clone(),
                        roster: members.load(&roster.roster.network_id)?,
                    })))
                    .0,
                    access_scan: Mutex::new(()),
                    id,
                    members,
                    network_id: roster.roster.network_id,
                    store: Arc::new(TaskStore::open(&dir.join("tasks.db"), true)?),
                    running: Mutex::new(Default::default()),
                    canceled: Mutex::new(Default::default()),
                    gate: Mutex::new(()),
                    sessions: Arc::new(Semaphore::new(32)),
                    files: Arc::new(Semaphore::new(8)),
                    forwards: Arc::new(Semaphore::new(32)),
                    streams: AtomicUsize::new(0),
                    stopping: AtomicBool::new(false),
                    fatal: Mutex::new(None),
                    stop: watch::channel(false).0,
                });
                for stopping in [false, true] {
                    let request = Execution {
                        request_id: format!("before-poll-{stopping}"),
                        db_id: rt.store.db_id.clone(),
                        program: std::env::current_exe()?.to_string_lossy().into(),
                        args: vec!["--list".into()],
                        cwd: config::home_dir()?.to_string_lossy().into(),
                        env: Default::default(),
                        timeout: 5,
                        shell: None,
                        input_size: 0,
                        input_sha256: sha256(&[]),
                    };
                    // submit queues execute on this single-threaded runtime. Neither
                    // execute nor process::spawn can run before the first await below.
                    let accepted = submit(rt.clone(), &rt.id.device_id, request.clone(), vec![])?;
                    assert_eq!(
                        rt.store.get(&accepted.job_id)?.unwrap().state,
                        JobState::Starting
                    );
                    if stopping {
                        rt.stopping.store(true, Ordering::SeqCst);
                        let rejected =
                            submit(rt.clone(), &rt.id.device_id, request, vec![]).unwrap_err();
                        assert!(crate::error::is(&rejected, ErrorCode::DaemonStopping));
                    } else {
                        rt.canceled.lock().unwrap().insert(accepted.job_id.clone());
                    }
                    let job = tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            let job = rt.store.get(&accepted.job_id)?.unwrap();
                            if job.state.terminal() {
                                return Ok::<_, anyhow::Error>(job);
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await??;
                    assert_eq!(job.state, JobState::Canceled);
                    assert!(job.process.is_none());
                    assert!(rt.running.lock().unwrap().is_empty());
                    assert!(!rt.canceled.lock().unwrap().contains(&job.job_id));
                    assert_eq!(rt.store.active_count()?, 0);
                }
                Ok(())
            })
    }
}
