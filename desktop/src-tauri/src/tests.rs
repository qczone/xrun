use super::*;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};
use xrun::{
    config::{self, DaemonConfig, Identity, NetworkIdentity, ServerConfig},
    membership::{Manager as NetworkManager, RosterCache},
};

#[cfg(target_os = "macos")]
#[allow(dead_code)] // Called by the native_ui target, which runs on the process main thread.
pub(crate) mod native_ui;

#[cfg(target_os = "macos")]
#[test]
fn start_initializes_and_stops_a_development_helper_without_service_registration() -> Result<()> {
    isolated(
        "start_initializes_and_stops_a_development_helper_without_service_registration",
        || {
            let root = config::home_dir()?;
            seed(&relay_config(&root)?)?;
            let source = root.join("helper.rs");
            std::fs::write(&source, include_str!("../tests/fixtures/dev-daemon.rs"))?;
            let helper = std::env::current_exe()?.with_file_name("xrun");
            let build = Command::new("rustc")
                .arg(source)
                .arg("-o")
                .arg(&helper)
                .env("XRUN_FIXTURE_VERSION", xrun::protocol::VERSION)
                .output()?;
            anyhow::ensure!(
                build.status.success(),
                "{}",
                String::from_utf8_lossy(&build.stderr)
            );
            let (_app, window) = build_app();
            assert_eq!(invoke(&window, "start", json!({})), Ok(Value::Null));
            let dir = config::device_dir()?;
            assert!(config::instance_running(&dir.join("daemon.lock"))?);
            assert_eq!(xrun::control::state(&dir)?.unwrap().generation, "fixture");
            assert!(
                !root
                    .join("Library/LaunchAgents/com.xrun.daemon.plist")
                    .exists()
            );
            assert_eq!(invoke(&window, "stop", json!({})), Ok(Value::Null));
            assert!(!config::instance_running(&dir.join("daemon.lock"))?);
            assert!(Identity::load()?.network.is_some());
            Ok(())
        },
    )
}

// Each case gets its own process, home and executable directory. No global
// environment mutation or installed helper/service can leak into these tests.
fn isolated(name: &str, test: impl FnOnce() -> Result<()>) -> Result<()> {
    if std::env::var("XRUN_NATIVE_TEST").as_deref() == Ok(name) {
        return test();
    }
    let temp = tempfile::tempdir()?;
    let exe = temp.path().join(if cfg!(windows) {
        "native-test.exe"
    } else {
        "native-test"
    });
    std::fs::copy(std::env::current_exe()?, &exe)?;
    let home = temp.path().join("home with spaces");
    std::fs::create_dir(&home)?;
    let mut child = Command::new(exe)
        .args(["--exact", &format!("tests::{name}"), "--nocapture"])
        .env("XRUN_NATIVE_TEST", name)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(90);
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            child.kill()?;
            let output = child.wait_with_output()?;
            anyhow::bail!(
                "native command test timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn build_app() -> (tauri::App<MockRuntime>, tauri::WebviewWindow<MockRuntime>) {
    let app = commands(mock_builder())
        .build(mock_context(noop_assets()))
        .unwrap();
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    (app, window)
}

fn invoke(
    window: &tauri::WebviewWindow<MockRuntime>,
    command: &str,
    body: Value,
) -> Result<Value, Value> {
    tauri::test::get_ipc_response(
        window,
        tauri::webview::InvokeRequest {
            cmd: command.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: if cfg!(windows) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
            body: tauri::ipc::InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.into(),
        },
    )
    .map(|body| body.deserialize().unwrap())
}

fn seed(cfg: &ServerConfig) -> Result<Identity> {
    let dir = config::device_dir()?;
    let ca = std::fs::read_to_string(cfg.data_dir.join("ca.pem"))?;
    let (manager, member, key_pem, cert_pem) = NetworkManager::create(
        &dir.join("manager"),
        "manager1",
        xrun::relay::addresses(cfg)?,
        ca,
    )?;
    let roster = manager.roster()?;
    let id = Identity {
        device_id: member.device_id,
        name: member.name,
        addresses: roster.roster.relay_addresses.clone(),
        ca_pem: roster.ca_pem.clone(),
        cert_pem,
        key_pem,
        registration: xrun::protocol::Registration {
            inviter_id: None,
            allow_inviter: false,
        },
        network: Some(NetworkIdentity {
            network_id: roster.roster.network_id.clone(),
            manager_id: roster.roster.manager_id.clone(),
        }),
    };
    RosterCache::open(&dir.join("roster.db"))?.observe(&roster.roster.network_id, &roster)?;
    id.save()?;
    xrun::daemon::init()?;
    Ok(id)
}

fn relay_config(path: &Path) -> Result<ServerConfig> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let cfg = ServerConfig {
        port,
        addresses: vec![format!("127.0.0.1:{port}")],
        manual: true,
        no_detect: true,
        data_dir: path.join("relay"),
    };
    xrun::relay::deployment_link(&cfg)?;
    Ok(cfg)
}

#[test]
fn settings_persist_through_native_ipc_and_rejections_leave_config_intact() -> Result<()> {
    isolated(
        "settings_persist_through_native_ipc_and_rejections_leave_config_intact",
        || {
            let root = config::home_dir()?;
            seed(&relay_config(&root)?)?;
            let mut cfg = DaemonConfig::load()?;
            cfg.env.insert("KEEP_ME".into(), "unchanged".into());
            cfg.allow_from.push(format!("dev_{}", "1".repeat(32)));
            cfg.save()?;
            let (app, window) = build_app();
            let execution =
                json!({"default_cwd":root,"max_concurrent_jobs":7,"path":"custom PATH"});
            assert_eq!(
                invoke(&window, "save_settings", json!({"execution":execution})),
                Ok(Value::Null)
            );
            assert_eq!(
                invoke(&window, "settings", json!({})).unwrap()["execution"],
                execution
            );
            let saved = DaemonConfig::load()?;
            assert_eq!(saved.env["KEEP_ME"], "unchanged");
            assert_eq!(saved.allow_from, cfg.allow_from);
            let bytes = std::fs::read(config::device_dir()?.join("daemon.toml"))?;
            for invalid in [
                json!({"default_cwd":root.join("missing"),"max_concurrent_jobs":7,"path":null}),
                json!({"default_cwd":root,"max_concurrent_jobs":0,"path":null}),
                json!({"default_cwd":root,"max_concurrent_jobs":7,"path":"bad\u{0000}path"}),
                json!({"default_cwd":root,"max_concurrent_jobs":7,"path":null,"allow_all":true}),
            ] {
                assert!(invoke(&window, "save_settings", json!({"execution":invalid})).is_err());
                assert_eq!(
                    std::fs::read(config::device_dir()?.join("daemon.toml"))?,
                    bytes
                );
            }
            assert!(app.state::<Desktop>().error.lock().unwrap().is_some());
            let defaults = json!({"default_cwd":null,"max_concurrent_jobs":4,"path":null});
            assert_eq!(
                invoke(&window, "save_settings", json!({"execution":defaults})),
                Ok(Value::Null)
            );
            assert!(app.state::<Desktop>().error.lock().unwrap().is_none());
            assert!(!DaemonConfig::load()?.env.contains_key("PATH"));
            drop(window);
            drop(app);
            let (_reopened, window) = build_app();
            assert_eq!(
                invoke(&window, "settings", json!({})).unwrap()["execution"],
                defaults
            );
            Ok(())
        },
    )
}

#[test]
fn permissions_and_pause_persist_through_native_ipc() -> Result<()> {
    isolated("permissions_and_pause_persist_through_native_ipc", || {
        seed(&relay_config(&config::home_dir()?)?)?;
        let (_app, window) = build_app();
        let peer = format!("dev_{}", "a".repeat(32));
        assert_eq!(
            invoke(&window, "permission", json!({"device":peer,"allow":true})),
            Ok(Value::Null)
        );
        assert_eq!(DaemonConfig::load()?.allow_from, [peer.as_str()]);
        assert!(DaemonConfig::load()?.check_access(&peer).is_ok());
        assert_eq!(
            invoke(&window, "permission", json!({"device":peer,"allow":false})),
            Ok(Value::Null)
        );
        assert_eq!(
            invoke(&window, "all_permissions", json!({"allow":true})),
            Ok(Value::Null)
        );
        let cfg = DaemonConfig::load()?;
        assert!(cfg.allow_all && cfg.allow_from.is_empty());
        assert_eq!(cfg.deny_from, [peer.as_str()]);
        assert!(cfg.check_access(&peer).is_err());
        assert!(cfg.check_access("another-member").is_ok());
        for paused in [true, false] {
            assert_eq!(
                invoke(&window, "pause_access", json!({"paused":paused})),
                Ok(Value::Null)
            );
            let cfg = DaemonConfig::load()?;
            assert_eq!(cfg.remote_access_paused, paused);
            assert_eq!(cfg.check_access("another-member").is_err(), paused);
            assert!(cfg.allow_all);
            assert_eq!(cfg.deny_from, [peer.as_str()]);
            let status = invoke(&window, "status", json!({})).unwrap();
            assert_eq!(status["local"]["remote_access_paused"], paused);
            assert_eq!(status["deny_from"], json!([peer]));
            assert_eq!(status["network"]["is_manager"], true);
        }
        let bytes = std::fs::read(config::device_dir()?.join("daemon.toml"))?;
        assert!(
            invoke(
                &window,
                "permission",
                json!({"device":"dev_invalid","allow":true})
            )
            .unwrap_err()
            .as_str()
            .unwrap()
            .starts_with("INVALID_DEVICE_ID:")
        );
        assert_eq!(
            std::fs::read(config::device_dir()?.join("daemon.toml"))?,
            bytes
        );
        Ok(())
    })
}

struct Relay(tauri::async_runtime::JoinHandle<Result<()>>);
impl Drop for Relay {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn start_relay(cfg: &ServerConfig) -> Result<Relay> {
    let task = tauri::async_runtime::spawn(xrun::relay::run(cfg.clone()));
    let relay = Relay(task);
    tauri::async_runtime::block_on(async {
        let ca = std::fs::read_to_string(cfg.data_dir.join("ca.pem"))?;
        let client = xrun::crypto::http_client(&ca, None)?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client
                    .get(format!("https://127.0.0.1:{}/", cfg.port))
                    .timeout(Duration::from_millis(200))
                    .send()
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(relay)
}

fn retry_existing_daemon(window: &tauri::WebviewWindow<MockRuntime>) -> Result<()> {
    let identity = std::fs::read(config::device_dir()?.join("identity.toml"))?;
    let source = config::home_dir()?.join("helper.rs");
    std::fs::write(
        &source,
        format!(
            "fn main() {{ println!(\"xrun {}\"); }}",
            xrun::protocol::VERSION
        ),
    )?;
    let helper =
        std::env::current_exe()?.with_file_name(if cfg!(windows) { "xrun.exe" } else { "xrun" });
    let compiled = Command::new("rustc")
        .arg(source)
        .arg("-o")
        .arg(helper)
        .output()?;
    anyhow::ensure!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    // Model a daemon that the user started externally after registration failed.
    // The real Start handler must reuse it without re-registering the identity.
    let _running = xrun::daemon::instance_lock()?;
    assert_eq!(invoke(window, "start", json!({})), Ok(Value::Null));
    assert_eq!(
        std::fs::read(config::device_dir()?.join("identity.toml"))?,
        identity
    );
    Ok(())
}

#[test]
fn creation_failure_keeps_identity_and_retry_only_starts_the_service() -> Result<()> {
    isolated(
        "creation_failure_keeps_identity_and_retry_only_starts_the_service",
        || {
            let cfg = relay_config(&config::home_dir()?)?;
            let _relay = start_relay(&cfg)?;
            let (app, window) = build_app();
            let error = invoke(
                &window,
                "create_network",
                json!({"link":"invalid","name":"local1"}),
            )
            .unwrap_err();
            assert!(!error.as_str().unwrap().starts_with("SERVICE_START_FAILED:"));
            assert!(Identity::load().is_err());
            let link = xrun::relay::deployment_link(&cfg)?;
            let error = invoke(
                &window,
                "create_network",
                json!({"link":format!("  {link}  "),"name":" local1 "}),
            )
            .unwrap_err();
            assert!(
                error
                    .as_str()
                    .unwrap()
                    .starts_with("SERVICE_START_FAILED: HELPER_NOT_FOUND:")
            );
            assert_eq!(Identity::load()?.name, "local1");
            assert!(config::device_dir()?.join("daemon.initialized").exists());
            assert!(app.state::<Desktop>().error.lock().unwrap().is_some());
            let status = invoke(&window, "status", json!({})).unwrap();
            assert_eq!(status["local"]["joined"], true);
            assert!(
                status["error"]
                    .as_str()
                    .unwrap()
                    .starts_with("SERVICE_START_FAILED:")
            );
            retry_existing_daemon(&window)?;
            assert!(app.state::<Desktop>().error.lock().unwrap().is_none());
            assert!(invoke(&window, "status", json!({})).unwrap()["error"].is_null());
            Ok(())
        },
    )
}

#[test]
#[ignore = "subprocess fixture for the native join test"]
fn pairing_manager_fixture() -> Result<()> {
    let cfg: ServerConfig = config::read(Path::new(&std::env::var("XRUN_NATIVE_MANAGER_CONFIG")?))?;
    let id = seed(&cfg)?;
    tauri::async_runtime::block_on(async {
        let invitation = xrun::network::invite(&id, false).await?;
        config::atomic_private_write(
            &config::home_dir()?.join("invitation.json"),
            &serde_json::to_vec(&invitation)?,
        )?;
        xrun::daemon::run().await
    })
}

#[test]
fn join_failure_keeps_membership_and_retry_does_not_reuse_the_invitation() -> Result<()> {
    isolated(
        "join_failure_keeps_membership_and_retry_does_not_reuse_the_invitation",
        || {
            let root = config::home_dir()?;
            let cfg = relay_config(&root)?;
            let _relay = start_relay(&cfg)?;
            let manager_home = root.join("manager-home");
            std::fs::create_dir(&manager_home)?;
            let cfg_path = root.join("relay.toml");
            config::write(&cfg_path, &cfg)?;
            let child = Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "tests::pairing_manager_fixture",
                    "--ignored",
                    "--nocapture",
                ])
                .env("HOME", &manager_home)
                .env("USERPROFILE", &manager_home)
                .env("XRUN_NATIVE_MANAGER_CONFIG", cfg_path)
                .spawn()?;
            struct Cleanup(std::process::Child);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            let mut manager = Cleanup(child);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !xrun::control::state(&manager_home.join(".xrun"))?.is_some_and(|s| s.connected) {
                anyhow::ensure!(manager.0.try_wait()?.is_none(), "pairing manager exited");
                anyhow::ensure!(Instant::now() < deadline, "pairing manager did not connect");
                std::thread::sleep(Duration::from_millis(20));
            }
            let invitation: Value =
                serde_json::from_slice(&std::fs::read(manager_home.join("invitation.json"))?)?;
            let (_app, window) = build_app();
            let error = invoke(
                &window,
                "join",
                json!({"link":invitation["link"],"name":"member1"}),
            )
            .unwrap_err();
            assert!(
                error
                    .as_str()
                    .context("join error")?
                    .starts_with("SERVICE_START_FAILED: HELPER_NOT_FOUND:")
            );
            let id = Identity::load()?;
            assert_eq!(id.name, "member1");
            assert_ne!(id.device_id, id.network.unwrap().manager_id);
            assert!(!config::device_dir()?.join("pending.toml").exists());
            // The invitation is single-use; retrying service startup requires no link.
            retry_existing_daemon(&window)?;
            Ok(())
        },
    )
}

#[test]
fn history_ipc_preserves_cursors_binary_logs_and_database_errors() -> Result<()> {
    isolated(
        "history_ipc_preserves_cursors_binary_logs_and_database_errors",
        || {
            use xrun::{protocol::*, store::TaskStore};
            let (_app, window) = build_app();
            let path = config::device_dir()?.join("daemon.db");
            let empty = invoke(&window, "task_history", json!({"filter":"all"})).unwrap();
            assert_eq!(empty["jobs"], json!([]));
            assert!(empty["db_id"].is_null());
            assert_eq!(
                invoke(&window, "file_history", json!({})).unwrap()["entries"],
                json!([])
            );
            assert!(!path.exists(), "opening history created a task database");
            let tasks = TaskStore::open(&path, true)?;
            for n in 1..=55 {
                tasks.insert(&Job {
                    job_id: format!("{n:06}"),
                    request_id: format!("request-{n}"),
                    request_hash: "hash".into(),
                    source_device_id: "source".into(),
                    target_device_id: "target".into(),
                    db_id: tasks.db_id.clone(),
                    program: "program".into(),
                    args: vec!["argument".into()],
                    cwd: config::home_dir()?.to_string_lossy().into(),
                    state: if n == 55 {
                        JobState::Failed
                    } else {
                        JobState::Running
                    },
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
                })?;
                tasks.audit(json!({"source_device_id":"source","op":"pull","path":format!("file-{n}"),"size":2,"result":"ok"}))?;
            }
            let page = invoke(&window, "task_history", json!({"filter":"all"})).unwrap();
            assert_eq!(page["db_id"], tasks.db_id);
            assert_eq!(page["jobs"].as_array().unwrap().len(), 50);
            let older = invoke(
                &window,
                "task_history",
                json!({"filter":"all","before":page["next_cursor"]}),
            )
            .unwrap();
            assert_eq!(older["jobs"].as_array().unwrap().len(), 5);
            assert_eq!(older["jobs"][0]["job_id"], "000005");
            assert!(older["next_cursor"].is_null());
            let failed = invoke(&window, "task_history", json!({"filter":"failed"})).unwrap();
            assert_eq!(failed["jobs"].as_array().unwrap().len(), 1);
            assert_eq!(failed["jobs"][0]["job_id"], "000055");
            assert!(
                invoke(&window, "task_history", json!({"filter":"invalid"}))
                    .unwrap_err()
                    .as_str()
                    .unwrap()
                    .starts_with("INVALID_FILTER:")
            );
            for _ in 0..40 {
                tasks.append("000001", "stdout", b"output\n")?;
            }
            tasks.append("000001", "stderr", &[0xff, 0])?;
            let output = invoke(
                &window,
                "task_output",
                json!({"dbId":tasks.db_id,"job":"000001"}),
            )
            .unwrap();
            assert_eq!(output["events"].as_array().unwrap().len(), 32);
            assert_eq!(output["events"][0]["seq"], 10);
            let next = invoke(
                &window,
                "task_output",
                json!({"dbId":tasks.db_id,"job":"000001","after":40}),
            )
            .unwrap();
            assert_eq!(
                next["events"],
                json!([{"seq":41,"stream":"stderr","data_base64":"/wA="}])
            );
            assert_eq!(next["has_more"], false);
            for (db, job, after, code) in [
                ("previous-database", "000001", 0, "DB_RESET:"),
                (tasks.db_id.as_str(), "missing", 0, "JOB_NOT_FOUND:"),
                (tasks.db_id.as_str(), "000001", u64::MAX, "INVALID_CURSOR:"),
            ] {
                let error = invoke(
                    &window,
                    "task_output",
                    json!({"dbId":db,"job":job,"after":after}),
                )
                .unwrap_err();
                assert!(error.as_str().unwrap().starts_with(code), "{error}");
            }
            let files = invoke(&window, "file_history", json!({})).unwrap();
            assert_eq!(files["entries"].as_array().unwrap().len(), 50);
            let older = invoke(
                &window,
                "file_history",
                json!({"before":files["next_cursor"]}),
            )
            .unwrap();
            assert_eq!(older["entries"].as_array().unwrap().len(), 5);
            assert_eq!(older["entries"][0]["path"], "file-5");
            assert!(older["next_cursor"].is_null());
            Ok(())
        },
    )
}

#[test]
fn native_membership_commands_preserve_invitation_policy_and_revoke_by_name() -> Result<()> {
    isolated(
        "native_membership_commands_preserve_invitation_policy_and_revoke_by_name",
        || {
            let root = config::home_dir()?;
            let cfg = relay_config(&root)?;
            let id = seed(&cfg)?;
            let _relay = start_relay(&cfg)?;
            let (_app, window) = build_app();
            let manager = xrun::network::manager(&id)?;
            let mut members = vec![];
            for (name, allow) in [("reader", false), ("operator", true)] {
                let invite = invoke(&window, "invite", json!({"allow":allow})).unwrap();
                assert_eq!(invite["allow"], allow);
                assert_eq!(invite["expires_in"], 600);
                let token = invite["link"].as_str().unwrap().rsplit_once('#').unwrap().1;
                let (_, csr) = xrun::crypto::new_device_request()?;
                let joined = manager.pair(token, name, &csr)?;
                assert_eq!(joined.receipt.receipt.allow, allow);
                xrun::network::observe(&id, &joined.roster)?;
                members.push(joined.member.device_id);
            }
            let devices = invoke(&window, "devices", json!({})).unwrap();
            assert!(devices["server_error"].is_null(), "{devices}");
            assert_eq!(devices["devices"].as_array().unwrap().len(), 3);
            let error = invoke(&window, "revoke", json!({"device":"manager1"})).unwrap_err();
            assert!(
                error.as_str().unwrap().starts_with("MANAGER_PROTECTED:"),
                "{error}"
            );
            let result = invoke(&window, "revoke", json!({"device":"reader"})).unwrap();
            assert_eq!(result["device_id"], members[0]);
            assert_eq!(result["revoked"], true);
            assert_eq!(result["undelivered"], json!([members[1]]));
            let roster = manager.roster()?;
            assert!(roster.member(&members[0])?.revoked);
            assert!(!roster.member(&members[1])?.revoked);
            assert!(invoke(&window, "status", json!({})).unwrap()["error"].is_null());
            Ok(())
        },
    )
}

#[test]
fn native_stop_finishes_the_daemon_without_erasing_membership() -> Result<()> {
    isolated(
        "native_stop_finishes_the_daemon_without_erasing_membership",
        || {
            let cfg = relay_config(&config::home_dir()?)?;
            seed(&cfg)?;
            let _relay = start_relay(&cfg)?;
            let (_app, window) = build_app();
            let dir = config::device_dir()?;
            let identity = std::fs::read(dir.join("identity.toml"))?;
            let task = tauri::async_runtime::spawn(xrun::daemon::run());
            let deadline = Instant::now() + Duration::from_secs(10);
            while !xrun::control::state(&dir)?.is_some_and(|s| s.connected) {
                anyhow::ensure!(Instant::now() < deadline, "daemon did not connect");
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(invoke(&window, "stop", json!({})), Ok(Value::Null));
            tauri::async_runtime::block_on(async {
                tokio::time::timeout(Duration::from_secs(5), task).await??
            })?;
            assert!(!config::instance_running(&dir.join("daemon.lock"))?);
            assert_eq!(invoke(&window, "stop", json!({})), Ok(Value::Null));
            assert_eq!(std::fs::read(dir.join("identity.toml"))?, identity);
            Ok(())
        },
    )
}

#[test]
fn native_status_reports_invalid_links_and_recovers_membership_read_errors() -> Result<()> {
    isolated(
        "native_status_reports_invalid_links_and_recovers_membership_read_errors",
        || {
            seed(&relay_config(&config::home_dir()?)?)?;
            let (app, window) = build_app();
            assert_eq!(invoke(&window, "hide_icon", json!({})), Ok(Value::Null));
            let error = invoke(
                &window,
                "copy_invitation",
                json!({"link":"https://not-an-invitation"}),
            )
            .unwrap_err();
            assert!(error.as_str().unwrap().starts_with("INVALID_LINK:"));
            assert!(app.state::<Desktop>().error.lock().unwrap().is_some());
            assert_eq!(
                invoke(&window, "pause_access", json!({"paused":false})),
                Ok(Value::Null)
            );
            let cache = config::device_dir()?.join("roster.db");
            let backup = cache.with_extension("backup");
            std::fs::rename(&cache, &backup)?;
            let broken = invoke(&window, "status", json!({})).unwrap();
            assert!(broken["network"].is_null());
            assert!(
                broken["error"]
                    .as_str()
                    .unwrap()
                    .starts_with("MEMBER_STATE_MISSING:")
            );
            std::fs::rename(backup, cache)?;
            let recovered = invoke(&window, "status", json!({})).unwrap();
            assert!(recovered["error"].is_null());
            assert_eq!(recovered["network"]["is_manager"], true);
            #[cfg(target_os = "macos")]
            {
                // An unbundled dev process must not register a real login item.
                assert!(
                    invoke(&window, "autostart", json!({"enabled":true}))
                        .unwrap_err()
                        .as_str()
                        .unwrap()
                        .starts_with("APP_BUNDLE_REQUIRED:")
                );
                assert_eq!(
                    invoke(&window, "remove_service", json!({})),
                    Ok(Value::Null)
                );
                assert!(Identity::load().is_ok());
            }
            Ok(())
        },
    )
}

#[test]
fn tray_text_prioritizes_setup_approval_stop_and_pause_over_connectivity() -> Result<()> {
    isolated(
        "tray_text_prioritizes_setup_approval_stop_and_pause_over_connectivity",
        || {
            let (app, _window) = build_app();
            for (joined, approval, running, paused, connected, expected) in [
                (false, true, true, true, Some(true), "xrun · 尚未加入"),
                (true, true, false, false, Some(true), "xrun · 需要系统授权"),
                (true, false, false, true, Some(true), "xrun · 服务已停止"),
                (true, false, true, true, Some(true), "xrun · 远程访问已暂停"),
                (true, false, true, false, Some(true), "xrun · 已连接"),
                (true, false, true, false, None, "xrun · 旧版服务运行中"),
                (true, false, true, false, Some(false), "xrun · 连接中…"),
            ] {
                let mut status = local_status(app.handle())?;
                status.local.joined = joined;
                status.service.approval_required = approval;
                status.local.daemon_running = running;
                status.local.remote_access_paused = paused;
                status.local.daemon_connected = connected;
                assert_eq!(tray_text(Ok(status)), expected);
            }
            assert_eq!(
                tray_text(Err(anyhow::anyhow!("unreadable config"))),
                "xrun · 状态读取失败"
            );
            Ok(())
        },
    )
}
