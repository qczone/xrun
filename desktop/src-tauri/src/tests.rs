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
