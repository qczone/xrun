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
