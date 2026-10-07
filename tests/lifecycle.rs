mod common;
use anyhow::{Context, Result};
use common::*;
use std::{path::Path, time::Duration};
use xrun::testing::{
    config::{self, Identity},
    crypto,
    membership::{RosterCache, device_name},
    protocol::VERSION,
};

fn identity(home: &Path) -> Result<Identity> {
    config::read(&home.join(".xrun/identity.toml"))
}
fn shorten_certificate(home: &Path, manager: &Path, days: i64) -> Result<String> {
    use rcgen::{
        CertificateParams, CertificateSigningRequestParams, DnType, ExtendedKeyUsagePurpose,
        Issuer, KeyPair, KeyUsagePurpose,
    };
    let mut id = identity(home)?;
    let dir = manager.join(".xrun/manager");
    let ca = std::fs::read_to_string(dir.join("root.pem"))?;
    let key = KeyPair::from_pem(&std::fs::read_to_string(dir.join("root.key"))?)?;
    let issuer = Issuer::from_ca_cert_pem(&ca, &key)?;
    let mut request = CertificateSigningRequestParams::from_der(
        &crypto::renew_device_request(&id.key_pem)?.into(),
    )?;
    let mut params = CertificateParams::new(vec![device_name(&id.device_id)?])?;
    params
        .distinguished_name
        .push(DnType::CommonName, &id.device_id);
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(2);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(days);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    request.params = params;
    id.cert_pem = request.signed_by(&issuer)?.pem();
    config::write(&home.join(".xrun/identity.toml"), &id)?;
    Ok(id.cert_pem)
}
fn roster(home: &Path) -> Result<xrun::testing::membership::SignedRoster> {
    let id = identity(home)?;
    RosterCache::open(&home.join(".xrun/roster.db"))?
        .load(&id.network.context("network")?.network_id)
}
async fn version(source: &Path, target: &str) {
    let program = binary().to_string_lossy().into_owned();
    assert_eq!(
        ok(cli(source, &[target, "--", &program, "--version"]).await).trim(),
        format!("xrun {VERSION}")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn certificates_renew_preserve_identity_and_require_manager_after_expiry() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(50), async {
        let mut lab = Lab::new().await?;
        let initial = lab.target_identity.clone();
        let near = shorten_certificate(&lab.target, &lab.source, 20)?;
        assert!(crypto::certificate_expiring(&near, 30)?);
        ok(cli(&lab.target, &["status", "--json"]).await);
        let renewed = identity(&lab.target)?;
        assert_ne!(renewed.cert_pem, near);
        assert!(!crypto::certificate_expiring(&renewed.cert_pem, 30)?);
        assert_eq!(renewed.device_id, initial.device_id);
        assert_eq!(renewed.key_pem, initial.key_pem);
        assert_eq!(roster(&lab.target)?.roster.version, 2);

        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        let near = shorten_certificate(&lab.target, &lab.source, 20)?;
        // Still-valid certificates continue working while renewal is unavailable.
        ok(cli(&lab.target, &["status", "--json"]).await);
        assert_eq!(identity(&lab.target)?.cert_pem, near);
        version(&lab.source, "target1").await;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let expired = shorten_certificate(&lab.target, &lab.source, -1)?;
        let failed = cli(&lab.target, &["status", "--json"]).await;
        assert_eq!(failed.status.code(), Some(125));
        let status: serde_json::Value = serde_json::from_slice(&failed.stdout)?;
        assert_eq!(status["server_error"]["code"], "CERTIFICATE_EXPIRED");
        assert_eq!(identity(&lab.target)?.cert_pem, expired);

        lab.source_daemon = logged(&lab.source, &["daemon"], "manager")?.spawn()?;
        online(&lab.source, "source1").await?;
        ok(cli(&lab.target, &["status", "--json"]).await);
        let renewed = identity(&lab.target)?;
        assert!(!crypto::certificate_expiring(&renewed.cert_pem, 30)?);
        assert_eq!(renewed.device_id, initial.device_id);
        assert_eq!(renewed.key_pem, initial.key_pem);
        // The manager can renew its own leaf locally without a relay.
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        lab.relay.stop().await?;
        let near = shorten_certificate(&lab.source, &lab.source, 20)?;
        assert_eq!(
            cli(&lab.source, &["status", "--json"]).await.status.code(),
            Some(125)
        );
        let manager = identity(&lab.source)?;
        assert_ne!(manager.cert_pem, near);
        assert!(!crypto::certificate_expiring(&manager.cert_pem, 30)?);
        assert_eq!(manager.key_pem, lab.source_identity.key_pem);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_restart_preserves_membership_and_reliable_job_results() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut lab = Lab::new().await?;
        let source = lab.root.path().join("finish.rs");
        let program = lab.root.path().join(if cfg!(windows) {
            "finish.exe"
        } else {
            "finish"
        });
        std::fs::write(
            &source,
            r#"
fn main() {
    let args: Vec<String> = std::env::args().collect();
    std::fs::write(&args[1], b"ready").unwrap();
    while !std::path::Path::new(&args[2]).exists() {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    println!("finished while relay was down");
}
"#,
        )?;
        let output = tokio::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .output()
            .await?;
        anyhow::ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ready = lab.root.path().join("ready.signal");
        let finish = lab.root.path().join("finish.signal");
        let job = json(
            cli(
                &lab.source,
                &[
                    "target1",
                    "start",
                    "--json",
                    "--",
                    &program.to_string_lossy(),
                    &ready.to_string_lossy(),
                    &finish.to_string_lossy(),
                ],
            )
            .await,
        );
        let id = job["job_id"].as_str().context("job ID")?;
        let before = roster(&lab.target)?;
        tokio::time::timeout(Duration::from_secs(10), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await?;
        lab.relay.stop().await?;
        std::fs::write(finish, b"finish")?;
        // Observe completion in target-local storage with the relay still down.
        let storage =
            xrun::testing::store::TaskStore::open(&lab.target.join(".xrun/daemon.db"), false)?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = storage.get(id)?.context("accepted task missing")?;
                if value.state.terminal() {
                    assert_eq!(value.state, xrun::protocol::JobState::Exited);
                    assert_eq!(value.exit_code, Some(0));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        lab.relay.start().await?;
        online(&lab.source, "target1").await?;
        let result = json(cli(&lab.source, &["target1", "wait", id, "--json"]).await);
        assert_eq!(result["job"]["state"], "exited");
        assert_eq!(result["job"]["exit_code"], 0);
        assert!(
            ok(cli(&lab.source, &["target1", "logs", id]).await)
                .contains("finished while relay was down")
        );
        assert_eq!(roster(&lab.target)?.hash()?, before.hash()?);
        assert_eq!(
            identity(&lab.target)?.device_id,
            lab.target_identity.device_id
        );
        version(&lab.source, "target1").await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_relay_change_survives_device_restart_without_new_identity() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut lab = Lab::new().await?;
        let (_new_relay, link) = TestRelay::new(&lab.root.path().join("replacement-relay")).await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        ok(cli(
            &lab.source,
            &["up", "--relay", &link, "--no-daemon", "--json"],
        )
        .await);
        online(&lab.source, "target1").await?;
        let current = roster(&lab.target)?;
        assert_eq!(current.roster.version, 3);
        assert_ne!(
            current.roster.relay_addresses,
            xrun::testing::relay::addresses(&lab.relay.config)?
        );
        assert_eq!(
            identity(&lab.source)?.device_id,
            lab.source_identity.device_id
        );
        assert_eq!(identity(&lab.source)?.key_pem, lab.source_identity.key_pem);
        assert_eq!(identity(&lab.target)?.key_pem, lab.target_identity.key_pem);
        lab.relay.stop().await?;
        version(&lab.source, "target1").await;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        lab.daemon = logged(&lab.target, &["daemon"], "target")?.spawn()?;
        online(&lab.source, "target1").await?;
        version(&lab.source, "target1").await;
        assert_eq!(roster(&lab.target)?.hash()?, current.hash()?);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_device_receives_revocation_on_reconnect_and_reports_pending_delivery() -> Result<()>
{
    tokio::time::timeout(Duration::from_secs(40), async {
        let mut lab = Lab::new().await?;
        let other = lab.root.path().join("other");
        std::fs::create_dir_all(&other)?;
        let invite = json(cli(&lab.source, &["invite", "--json"]).await);
        ok(cli(
            &other,
            &[
                "join",
                invite["link"].as_str().unwrap(),
                "--name",
                "other1",
                "--no-daemon",
            ],
        )
        .await);
        let other_id = identity(&other)?.device_id;
        // Pairing returns before the manager finishes distributing the new roster.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if roster(&lab.target)?.member(&other_id).is_ok() {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .context("target did not receive the new member before granting access")??;
        ok(cli(&lab.target, &["allow-from", "other1"]).await);
        let old = roster(&lab.target)?;
        assert!(!old.member(&other_id)?.revoked);
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let revoked = json(cli(&lab.source, &["revoke", "other1", "--json"]).await);
        assert_eq!(
            revoked["undelivered"],
            serde_json::json!([lab.target_identity.device_id])
        );
        assert_eq!(roster(&lab.target)?.hash()?, old.hash()?);
        lab.daemon = logged(&lab.target, &["daemon"], "target")?.spawn()?;
        online(&lab.source, "target1").await?;
        let current = roster(&lab.target)?;
        assert!(current.member(&other_id)?.revoked);
        assert_eq!(
            current.roster.version,
            revoked["roster_version"].as_u64().unwrap()
        );
        let denied = cli(
            &other,
            &["target1", "--", &binary().to_string_lossy(), "--version"],
        )
        .await;
        assert_eq!(denied.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&denied.stderr).contains("DEVICE_REVOKED"));
        let policy: xrun::testing::config::DaemonConfig =
            config::read(&lab.target.join(".xrun/daemon.toml"))?;
        assert!(policy.allow_from.contains(&other_id));
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peers_deliver_revocation_after_relay_restart_with_manager_offline() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut lab = Lab::new().await?;
        let keeper = lab.root.path().join("keeper");
        let removed = lab.root.path().join("removed");
        for (home, name) in [(&keeper, "keeper1"), (&removed, "removed1")] {
            std::fs::create_dir_all(home)?;
            let invite = json(cli(&lab.source, &["invite", "--json"]).await);
            ok(cli(
                home,
                &[
                    "join",
                    invite["link"].as_str().unwrap(),
                    "--name",
                    name,
                    "--no-daemon",
                ],
            )
            .await);
        }
        let mut keeper_daemon = logged(&keeper, &["daemon"], "keeper")?.spawn()?;
        let mut removed_daemon = logged(&removed, &["daemon"], "removed")?.spawn()?;
        online(&lab.source, "keeper1").await?;
        online(&lab.source, "removed1").await?;
        ok(cli(&lab.target, &["allow-from", "removed1"]).await);
        version(&removed, "target1").await;
        let removed_id = identity(&removed)?.device_id;
        let before = roster(&lab.target)?;
        assert!(!before.member(&removed_id)?.revoked);
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let revoked = json(cli(&lab.source, &["revoke", "removed1", "--json"]).await);
        assert_eq!(
            revoked["undelivered"],
            serde_json::json!([lab.target_identity.device_id])
        );
        assert!(roster(&keeper)?.member(&removed_id)?.revoked);
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        lab.relay.stop().await?;
        assert!(!lab.relay.config.data_dir.join("relay.db").exists());
        lab.relay.start().await?;
        online(&keeper, "keeper1").await?;
        // No execution grant exists between keeper and target. State exchange
        // still synchronizes the manager-signed revocation before business use.
        lab.daemon = logged(&lab.target, &["daemon"], "target")?.spawn()?;
        online(&keeper, "target1").await?;
        assert!(roster(&lab.target)?.member(&removed_id)?.revoked);
        let denied = cli(
            &removed,
            &["target1", "--", &binary().to_string_lossy(), "--version"],
        )
        .await;
        assert_eq!(denied.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&denied.stderr).contains("DEVICE_REVOKED"));
        stop_daemon(&keeper, &mut keeper_daemon).await?;
        stop_daemon(&removed, &mut removed_daemon).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
