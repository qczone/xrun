mod common;
use anyhow::Result;
use common::*;
use std::time::Duration;
use xrun::protocol::{Registration, RelayMessage};
use xrun::testing::{
    config::{self, Identity, NetworkIdentity},
    crypto,
    membership::{Manager, RosterCache},
    net,
};

#[tokio::test]
async fn maintenance_fixture() -> Result<()> {
    if std::env::var_os("XRUN_MAINTENANCE_FIXTURE").is_some() {
        xrun::testing::daemon::maintain_membership().await?;
    }
    Ok(())
}

async fn maintain(home: &std::path::Path) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "maintenance_fixture", "--nocapture"])
            .env("XRUN_MAINTENANCE_FIXTURE", "1")
            .env("HOME", home)
            .env("USERPROFILE", home)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ok(output);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_renewal_recovers_when_the_manager_returns_without_restarting_the_daemon()
-> Result<()> {
    use rcgen::{
        CertificateParams, CertificateSigningRequestParams, DnType, ExtendedKeyUsagePurpose,
        Issuer, KeyPair, KeyUsagePurpose,
    };
    let mut lab = Lab::new().await?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    let running_pid = lab.daemon.id();
    let manager_dir = lab.source.join(".xrun/manager");
    let key = KeyPair::from_pem(&std::fs::read_to_string(manager_dir.join("root.key"))?)?;
    let root = std::fs::read_to_string(manager_dir.join("root.pem"))?;
    let issuer = Issuer::from_ca_cert_pem(&root, &key)?;
    let csr = crypto::renew_device_request(&lab.target_identity.key_pem)?;
    let mut request = CertificateSigningRequestParams::from_der(&csr.as_slice().into())?;
    let mut params = CertificateParams::new(vec![xrun::testing::membership::device_name(
        &lab.target_identity.device_id,
    )?])?;
    params
        .distinguished_name
        .push(DnType::CommonName, &lab.target_identity.device_id);
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(2);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    request.params = params;
    let certificate = request.signed_by(&issuer)?.pem();
    lab.target_identity.cert_pem = certificate.clone();
    let identity_path = lab.target.join(".xrun/identity.toml");
    config::write(&identity_path, &lab.target_identity)?;
    maintain(&lab.target).await?;
    let unchanged: Identity = config::read(&identity_path)?;
    assert_eq!(unchanged.cert_pem, certificate);
    assert!(crypto::certificate_expiring(&unchanged.cert_pem, 30)?);
    assert!(
        xrun::testing::control::state(&lab.target.join(".xrun"))?
            .unwrap()
            .connected
    );

    lab.source_daemon = logged(&lab.source, &["daemon"], "manager-returned")?.spawn()?;
    online(&lab.source, "source1").await?;
    maintain(&lab.target).await?;
    let renewed: Identity = config::read(&identity_path)?;
    assert_ne!(renewed.cert_pem, certificate);
    assert!(!crypto::certificate_expiring(&renewed.cert_pem, 30)?);
    assert_eq!(renewed.device_id, lab.target_identity.device_id);
    assert_eq!(renewed.key_pem, lab.target_identity.key_pem);
    assert_eq!(lab.daemon.id(), running_pid);
    assert!(lab.daemon.try_wait()?.is_none());
    ok(cli(
        &lab.source,
        &["target1", "--", &binary().to_string_lossy(), "--version"],
    )
    .await);
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_response_does_not_wait_for_unrelated_slow_peers() -> Result<()> {
    let mut lab = Lab::new().await?;
    let invitation = json(cli(&lab.source, &["invite", "--json"]).await);
    let manager = Manager::open(&lab.source.join(".xrun/manager"))?;
    let mut peers = Vec::new();
    // Two concurrent 5-second state requests across thirteen live but silent
    // peers exceed join's own 30-second deadline in the previous implementation.
    for index in 0..13 {
        let (key, csr) = crypto::new_device_request()?;
        let pair = manager.pair(&manager.invite(false)?, &format!("slow{index}"), &csr)?;
        peers.push(Identity {
            device_id: pair.member.device_id,
            name: pair.member.name,
            addresses: pair.roster.roster.relay_addresses.clone(),
            ca_pem: pair.roster.ca_pem.clone(),
            cert_pem: pair.cert_pem,
            key_pem: key,
            registration: Registration {
                inviter_id: None,
                allow_inviter: false,
            },
            network: Some(NetworkIdentity {
                network_id: pair.roster.roster.network_id.clone(),
                manager_id: pair.roster.roster.manager_id.clone(),
            }),
        });
    }
    let roster = manager.roster()?;
    RosterCache::open(&lab.source.join(".xrun/roster.db"))?
        .observe(&roster.roster.network_id, &roster)?;
    let mut controls = tokio::task::JoinSet::new();
    for id in peers {
        let path = format!("/networks/{}/control", roster.roster.network_id);
        let mut ws = relay_socket(Some(&id), &roster, &path).await?;
        assert!(matches!(
            net::receive::<RelayMessage>(&mut ws).await?,
            RelayMessage::HelloAck { .. }
        ));
        controls.spawn(async move {
            // Remain relay-online and answer heartbeat Pings, but never attach
            // Incoming sessions. No timer or fake network error drives the test.
            while net::receive::<RelayMessage>(&mut ws).await.is_ok() {}
        });
    }
    let joining = lab.root.path().join("joining");
    std::fs::create_dir_all(&joining)?;
    ok(cli(
        &joining,
        &[
            "join",
            invitation["link"].as_str().unwrap(),
            "--name",
            "joining1",
            "--no-daemon",
        ],
    )
    .await);
    let joined: Identity = config::read(&joining.join(".xrun/identity.toml"))?;
    assert!(
        manager
            .roster()?
            .roster
            .members
            .iter()
            .any(|member| member.device_id == joined.device_id && member.name == "joining1")
    );
    controls.shutdown().await;
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    Ok(())
}
