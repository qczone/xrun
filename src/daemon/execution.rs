//! Durable acceptance, process execution, cancellation and output capture.
use crate::error::ErrorCode;
use crate::{config, protocol::*, store::JobStore};
use anyhow::{Context, Result, bail};
#[cfg(windows)]
use std::collections::HashSet;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
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
    if jobs >= rt.config()?.max_concurrent_jobs {
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
    let mut job = Job::accepted(
        source,
        &rt.id.device_id,
        &JobContext {
            request_id: request.request_id.clone(),
            db_id: request.db_id.clone(),
        },
        hash,
        JobDetails::Exec(CommandParams {
            program: request.program.clone(),
            args: request.args.clone(),
            cwd: request.cwd.clone(),
            timeout: request.timeout,
            shell: request.shell.clone(),
            input_size: Some(request.input_size),
            input_sha256: Some(request.input_sha256.clone()),
        }),
    );
    job.job_id = id;
    validate_job_size(&job)?;
    rt.store.insert(&job)?;
    let background = rt.clone();
    let saved = job.clone();
    let mut jobs = rt.jobs.lock().unwrap();
    while let Some(result) = jobs.try_join_next() {
        if let Err(error) = result {
            tracing::error!(%error, "task execution panicked");
        }
    }
    jobs.spawn(async move {
        let started = Arc::new(AtomicBool::new(false));
        if let Err(e) = execute(
            background.clone(),
            saved.clone(),
            request,
            input,
            started.clone(),
        )
        .await
        {
            crate::process::force_kill(
                background
                    .running
                    .lock()
                    .unwrap()
                    .remove(&saved.job_id)
                    .unwrap_or(0),
            );
            let recorded = if started.load(Ordering::SeqCst) {
                let mut lost = outcome(JobState::Lost);
                lost.error_code = Some("RESULT_LOST".into());
                lost.error_message = Some(format!(
                    "RESULT_LOST: execution started but its result could not be recorded: {e:#}"
                ));
                lost.output_loss_reason = Some("CAPTURE_ERROR: execution interrupted".into());
                lost.leftover_possible = cfg!(unix);
                background.store.finish(&saved.job_id, lost).await
            } else {
                background
                    .store
                    .mark_failed(&saved.job_id, format!("EXECUTION_ERROR: {e:#}"))
                    .await
            };
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

async fn execute(
    rt: Arc<Runtime>,
    job: Job,
    request: Execution,
    input: Vec<u8>,
    started: Arc<AtomicBool>,
) -> Result<()> {
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
    let runtime = rt.clone();
    let launching = job.job_id.clone();
    let spawned = tokio::task::spawn_blocking(move || {
        let _gate = runtime.gate.lock().unwrap();
        if runtime.canceled.lock().unwrap().contains(&launching)
            || runtime.stopping.load(Ordering::SeqCst)
        {
            runtime
                .store
                .finish_sync(&launching, outcome(JobState::Canceled))?;
            return Ok(None);
        }
        let child = crate::process::spawn(&resolved, &args, &cwd, &env, &launching, cmd_script)?;
        // Even recording the PID can fail after the process has made changes.
        started.store(true, Ordering::SeqCst);
        let process = ProcessIdentity {
            pid: child.pid,
            boot_id: boot_id(),
            start: process_start(child.pid),
        };
        if let Err(error) = runtime.store.record_process(&launching, process) {
            crate::process::force_kill(child.pid);
            return Err(error);
        }
        runtime.running.lock().unwrap().insert(launching, child.pid);
        Ok(Some(child))
    })
    .await??;
    let Some(mut child) = spawned else {
        return Ok(());
    };
    // Remove the PID before the child can be reaped, including error paths.
    let running = RunningJob {
        rt: rt.clone(),
        id: job.job_id.clone(),
    };
    let pid = child.pid;
    rt.store.mark_running(&job.job_id).await?;
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
    let mut readers = OutputReaders { tasks: readers };
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
        let status = tokio::select! {
            status = child.wait() => Some(status),
            _ = tokio::time::sleep(CANCELLATION_SCAN_INTERVAL) => None,
        };
        if let Some(status) = status {
            break status?;
        }
        let canceled =
            rt.canceled.lock().unwrap().contains(&job.job_id) || rt.stopping.load(Ordering::SeqCst);
        let timed_out = request.timeout > 0
            && crate::clock::elapsed_clock_ms()?.saturating_sub(start)
                >= request.timeout.saturating_mul(1000);
        if canceled || timed_out {
            reason = Some(if canceled {
                JobState::Canceled
            } else {
                JobState::TimedOut
            });
            crate::process::terminate(pid);
            break match tokio::time::timeout(PROCESS_TERMINATION_GRACE, child.wait()).await {
                Ok(status) => status?,
                Err(_) => {
                    crate::process::force_kill(pid);
                    child.wait().await?
                }
            };
        }
    };
    crate::process::terminate(pid);
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    for reader in &mut readers.tasks {
        if tokio::time::timeout_at(drain_deadline, &mut *reader)
            .await
            .is_err()
        {
            reader.abort();
            let _ = (&mut *reader).await;
            crate::store::merge_incomplete(
                &mut incomplete.lock().unwrap(),
                Some("DETACHED_OUTPUT".into()),
            );
        }
    }
    if tokio::time::timeout_at(drain_deadline, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
        let _ = writer.await;
    }
    crate::process::force_kill(pid);
    drop(running);
    child.reap().await?;
    drop(script);
    rt.store.flush().await?;
    let exit_code = status.code().map(i64::from);
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal = None;
    let state = reason.unwrap_or(if exit_code == Some(0) && signal.is_none() {
        JobState::Succeeded
    } else {
        JobState::Failed
    });
    let mut completed = outcome(state);
    completed.result = Some(JobResult::Command(CommandResult {
        exit_code,
        signal,
        duration_ms: crate::clock::elapsed_clock_ms()?.saturating_sub(start),
        ..Default::default()
    }));
    completed.output_loss_reason = incomplete.lock().unwrap().clone();
    rt.store.finish(&job.job_id, completed).await?;
    Ok(())
}
struct OutputReaders {
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for OutputReaders {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
const CANCELLATION_SCAN_INTERVAL: Duration = Duration::from_millis(100);
const PROCESS_TERMINATION_GRACE: Duration = Duration::from_secs(5);
const LOG_BATCH_BYTES: usize = 32 * 1024;
const LOG_BATCH_WAIT: Duration = Duration::from_millis(50);
fn outcome(state: JobState) -> crate::store::JobOutcome {
    crate::store::JobOutcome::new(state)
}

async fn drain(
    store: Arc<JobStore>,
    id: String,
    stream: String,
    mut pipe: Box<dyn AsyncRead + Unpin + Send>,
    incomplete: Arc<Mutex<Option<String>>>,
) {
    let mut bytes = vec![0u8; LOG_BATCH_BYTES];
    let mut used = 0;
    let mut deadline = tokio::time::Instant::now() + LOG_BATCH_WAIT;
    loop {
        let result = if used == 0 {
            Some(pipe.read(&mut bytes).await)
        } else {
            tokio::select! {
                result = pipe.read(&mut bytes[used..]) => Some(result),
                _ = tokio::time::sleep_until(deadline) => None,
            }
        };
        let eof = matches!(result, Some(Ok(0)) | Some(Err(_)));
        match result {
            Some(Ok(count)) => {
                if used == 0 {
                    deadline = tokio::time::Instant::now() + LOG_BATCH_WAIT;
                }
                used += count;
            }
            Some(Err(error)) => crate::store::merge_incomplete(
                &mut incomplete.lock().unwrap(),
                Some(format!("CAPTURE_ERROR: {error}")),
            ),
            None => {}
        }
        if used > 0 && (eof || used == LOG_BATCH_BYTES || tokio::time::Instant::now() >= deadline) {
            match store
                .append_async(&id, &stream, bytes[..used].to_vec())
                .await
            {
                Ok(Some(_)) => {}
                Ok(None) => crate::store::merge_incomplete(
                    &mut incomplete.lock().unwrap(),
                    Some("TRUNCATED".into()),
                ),
                Err(error) => crate::store::merge_incomplete(
                    &mut incomplete.lock().unwrap(),
                    Some(format!("CAPTURE_ERROR: {error}")),
                ),
            }
            used = 0;
        }
        if eof {
            return;
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

    #[tokio::test]
    async fn log_batches_flush_at_size_deadline_and_eof() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Arc::new(JobStore::open(&directory.path().join("tasks.db"), true)?);
        store.insert(&Job {
            job_id: "BATCH1".into(),
            request_id: "batch".into(),
            request_hash: "hash".into(),
            source_device_id: "source".into(),
            target_device_id: "target".into(),
            db_id: store.db_id.clone(),
            state: JobState::Running,
            last_log_seq: 0,
            output_loss_reason: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
            leftover_possible: false,
            process: None,
            details: JobDetails::Exec(CommandParams {
                program: "test".into(),
                args: vec![],
                cwd: "/".into(),
                timeout: 0,
                shell: None,
                input_size: None,
                input_sha256: None,
            }),
            result: None,
            output_complete: Some(true),
            error_code: None,
            error_message: None,
            log_bytes: 0,
            attachments: vec![],
            started_at_ms: None,
            finished_at_ms: if (JobState::Running).terminal() {
                Some(now_ms())
            } else {
                None
            },
        })?;
        let (mut output, input) = tokio::io::duplex(2 * LOG_BATCH_BYTES);
        let incomplete = Arc::new(Mutex::new(None));
        let mut changes = store.subscribe();
        let capture = tokio::spawn(drain(
            store.clone(),
            "BATCH1".into(),
            "stdout".into(),
            Box::new(input),
            incomplete.clone(),
        ));
        output.write_all(&vec![b'x'; LOG_BATCH_BYTES]).await?;
        tokio::time::timeout(Duration::from_secs(1), changes.changed()).await??;
        assert_eq!(store.get_async("BATCH1").await?.unwrap().last_log_seq, 1);
        changes.borrow_and_update();
        output.write_all(b"timer").await?;
        tokio::time::timeout(LOG_BATCH_WAIT + Duration::from_secs(1), changes.changed()).await??;
        assert_eq!(store.get_async("BATCH1").await?.unwrap().last_log_seq, 2);
        output.write_all(b"eof").await?;
        drop(output);
        capture.await?;
        store.flush().await?;
        assert_eq!(store.get_async("BATCH1").await?.unwrap().last_log_seq, 3);
        assert!(incomplete.lock().unwrap().is_none());
        let events = store.logs("BATCH1", 0)?;
        use base64::Engine;
        let bytes: Vec<_> = events
            .iter()
            .flat_map(|event| {
                base64::engine::general_purpose::STANDARD
                    .decode(&event.data_base64)
                    .unwrap()
            })
            .collect();
        assert_eq!(bytes.len(), LOG_BATCH_BYTES + 8);
        assert!(bytes.ends_with(b"timereof"));
        Ok(())
    }

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
                    store: Arc::new(JobStore::open(&dir.join("tasks.db"), true)?),
                    running: Mutex::new(Default::default()),
                    jobs: Mutex::new(tokio::task::JoinSet::new()),
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
                        JobState::Accepted
                    );
                    if stopping {
                        rt.stopping.store(true, Ordering::SeqCst);
                        assert!(crate::error::is(
                            &rt.check_session(&rt.id.device_id, 0).unwrap_err(),
                            ErrorCode::DaemonStopping
                        ));
                        let rejected =
                            submit(rt.clone(), &rt.id.device_id, request, vec![]).unwrap_err();
                        assert!(crate::error::is(&rejected, ErrorCode::DaemonStopping));
                    } else {
                        rt.canceled.lock().unwrap().insert(accepted.job_id.clone());
                    }
                    let job = tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            let job = rt.store.get(&accepted.job_id)?.unwrap();
                            // The commit becomes visible before execute returns
                            // and the background task removes its cancel marker.
                            if job.state.terminal()
                                && !rt.canceled.lock().unwrap().contains(&job.job_id)
                            {
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
