use anyhow::Result;
use std::sync::Arc;
use xrun::testing::{config::ServerConfig, crypto, membership::*, protocol::sha256};

fn manager(path: &std::path::Path) -> Result<Manager> {
    let relay = crypto::load_or_create_server(&ServerConfig {
        port: 9528,
        addresses: vec!["127.0.0.1:9528".into()],
        manual: true,
        no_detect: true,
        data_dir: path.join("relay"),
    })?;
    Ok(Manager::create(
        &path.join("manager"),
        "manager1",
        vec!["https://127.0.0.1:9528".into()],
        relay.ca_pem,
    )?
    .0)
}

#[test]
fn invitations_are_atomic_single_use_and_receipts_bind_both_devices() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let manager = manager(dir.path())?;
    let token = manager.invite(true)?;
    let (_, csr) = crypto::new_device_request()?;
    assert!(manager.pair(&token, "manager1", &csr).is_err());
    assert!(manager.pair(&token, "member1", b"bad CSR").is_err());
    let joined = manager.pair(&token, "member1", &csr)?;
    assert_eq!(joined.roster.roster.version, 2);
    joined
        .receipt
        .verify(&joined.roster, &joined.member.device_id)?;
    assert!(joined.receipt.receipt.allow);
    assert!(
        joined
            .receipt
            .verify(&joined.roster, &joined.roster.roster.manager_id)
            .is_err()
    );
    let mut forged = joined.receipt.clone();
    forged.receipt.allow = false;
    assert!(
        forged
            .verify(&joined.roster, &joined.member.device_id)
            .is_err()
    );
    let (_, other) = crypto::new_device_request()?;
    assert!(manager.pair(&token, "member2", &other).is_err());
    let retry = manager.pair("", "ignored-name", &csr)?;
    assert_eq!(retry.member, joined.member);
    assert_eq!(retry.roster.roster.version, 2);
    let der = crypto::cert_der(&joined.cert_pem)?;
    assert_eq!(
        joined.roster.peer(&der, Some(&joined.member.device_id))?,
        &joined.member
    );
    assert!(
        joined
            .roster
            .peer(&der, Some(&joined.roster.roster.manager_id))
            .is_err()
    );
    // The authority never stores the bearer token itself.
    let db = rusqlite::Connection::open(dir.path().join("manager/manager.db"))?;
    let stored: i64 = db.query_row(
        "SELECT COUNT(*) FROM invitations WHERE hash=?1",
        [sha256(token.as_bytes())],
        |r| r.get(0),
    )?;
    assert_eq!(stored, 0);
    Ok(())
}

#[test]
fn cached_versions_survive_restart_and_reject_forgery_rollback_and_reactivation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let manager = manager(dir.path())?;
    let initial = manager.roster()?;
    let network = &initial.roster.network_id;
    let path = dir.path().join("member/roster.db");
    let cache = RosterCache::open(&path)?;
    cache.observe(network, &initial)?;
    let (_, csr) = crypto::new_device_request()?;
    let joined = manager.pair(&manager.invite(false)?, "member1", &csr)?;
    cache.observe(network, &joined.roster)?;
    drop(cache);
    let cache = RosterCache::open(&path)?;
    assert!(
        cache
            .observe(network, &initial)
            .unwrap_err()
            .to_string()
            .contains("ROSTER_ROLLBACK")
    );
    let mut altered = joined.roster.clone();
    altered.roster.members[0].name = "forged".into();
    assert!(cache.observe(network, &altered).is_err());
    let key = std::fs::read_to_string(dir.path().join("manager/root.key"))?;
    altered.signature = sign(
        &key,
        "roster",
        &(&altered.roster, sha256(&crypto::cert_der(&altered.ca_pem)?)),
    )?;
    assert!(
        cache
            .observe(network, &altered)
            .unwrap_err()
            .to_string()
            .contains("ROSTER_CONFLICT")
    );
    let revoked = manager.revoke("member1")?;
    cache.observe(network, &revoked)?;
    assert!(
        revoked
            .peer(&crypto::cert_der(&joined.cert_pem)?, None)
            .unwrap_err()
            .to_string()
            .contains("DEVICE_REVOKED")
    );
    let unused = manager.invite(false)?;
    assert!(
        manager
            .pair(&unused, "member1", &csr)
            .unwrap_err()
            .to_string()
            .contains("DEVICE_REVOKED")
    );
    let mut reactivated = revoked.clone();
    reactivated.roster.version += 1;
    reactivated.roster.members[1].revoked = false;
    reactivated.signature = sign(
        &key,
        "roster",
        &(
            &reactivated.roster,
            sha256(&crypto::cert_der(&reactivated.ca_pem)?),
        ),
    )?;
    assert!(cache.observe(network, &reactivated).is_err());
    // A fresh identity can reuse a revoked member's name; its old key never can.
    let (_, csr) = crypto::new_device_request()?;
    let replacement = manager.pair(&unused, "member1", &csr)?;
    cache.observe(network, &replacement.roster)?;
    assert_ne!(replacement.member.device_id, joined.member.device_id);
    assert_eq!(cache.load(network)?.roster.version, 4);
    Ok(())
}

#[test]
fn concurrent_manager_processes_commit_distinct_monotonic_versions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let manager = Arc::new(manager(dir.path())?);
    let second = Arc::new(Manager::open(&dir.path().join("manager"))?);
    let mut workers = vec![];
    for n in 0..12 {
        let instance = if n % 2 == 0 {
            manager.clone()
        } else {
            second.clone()
        };
        let token = manager.invite(false)?;
        workers.push(std::thread::spawn(move || -> Result<u64> {
            let (_, csr) = crypto::new_device_request()?;
            Ok(instance
                .pair(&token, &format!("member{n}"), &csr)?
                .roster
                .roster
                .version)
        }));
    }
    let mut versions = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Result<Vec<_>>>()?;
    versions.sort_unstable();
    assert_eq!(versions, (2..=13).collect::<Vec<_>>());
    assert_eq!(
        Manager::open(&dir.path().join("manager"))?
            .roster()?
            .roster
            .members
            .len(),
        13
    );
    Ok(())
}

#[test]
fn authority_loss_is_not_silently_recovered() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let manager = manager(dir.path())?;
    drop(manager);
    std::fs::remove_file(dir.path().join("manager/manager.db"))?;
    assert!(Manager::open(&dir.path().join("manager")).is_err());
    assert!(
        Manager::create(
            &dir.path().join("manager"),
            "manager1",
            vec!["https://127.0.0.1:9528".into()],
            String::new()
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn roster_signature_binds_root_certificate_as_well_as_its_public_key() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let manager = manager(dir.path())?;
    let mut record = manager.roster()?;
    let original_pin = crypto::ca_spki_pin(&record.ca_pem)?;
    let key = rcgen::KeyPair::from_pem(&std::fs::read_to_string(
        dir.path().join("manager/root.key"),
    )?)?;
    let mut params = rcgen::CertificateParams::default();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "substituted root certificate");
    record.ca_pem = params.self_signed(&key)?.pem();
    assert_eq!(crypto::ca_spki_pin(&record.ca_pem)?, original_pin);
    assert!(
        record
            .verify(&record.roster.network_id)
            .unwrap_err()
            .to_string()
            .contains("INVALID_SIGNATURE")
    );
    Ok(())
}
