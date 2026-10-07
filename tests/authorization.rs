mod common;

use anyhow::{Context, Result, bail};
use common::*;
use std::{io::Write, time::Duration};
use xrun::{
    error::{self, ErrorCode},
    testing::{config, control, net, protocol::Data},
};

async fn established(lab: &Lab) -> Result<net::Ws> {
    let mut ws = peer_session(
        &lab.source,
        &lab.source_identity,
        &lab.target_identity.device_id,
    )
    .await?;
    assert!(matches!(
        net::receive::<Data>(&mut ws).await?,
        Data::Ready { .. }
    ));
    Ok(ws)
}

async fn invalidated(ws: &mut net::Ws, expected: &str) -> Result<()> {
    let message = tokio::time::timeout(Duration::from_secs(3), net::receive::<Data>(ws))
        .await?
        .with_context(|| format!("receive {expected} invalidation"))?;
    match message {
        Data::Error { code, .. } => assert_eq!(code, expected),
        other => bail!("expected authorization failure, received {other:?}"),
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explicit_reload_reads_equal_metadata_edits_and_preserves_failure_codes() -> Result<()> {
    let lab = Lab::new().await?;
    let directory = lab.target.join(".xrun");
    let path = directory.join("daemon.toml");
    let original = std::fs::read(&path)?;

    let mut ws = established(&lab).await?;
    control::refresh_access(&directory).await?;
    let before = std::fs::metadata(&path)?;
    let edited = String::from_utf8(original.clone())?.replace(
        &lab.source_identity.device_id,
        &lab.target_identity.device_id,
    );
    assert_eq!(edited.len(), original.len());
    assert_ne!(edited.as_bytes(), original.as_slice());
    let mut file = std::fs::OpenOptions::new().write(true).open(&path)?;
    file.write_all(edited.as_bytes())?;
    file.sync_all()?;
    file.set_times(std::fs::FileTimes::new().set_modified(before.modified()?))?;
    drop(file);
    assert_eq!(std::fs::metadata(&path)?.modified()?, before.modified()?);
    control::refresh_access(&directory).await?;
    invalidated(&mut ws, "SOURCE_NOT_ALLOWED").await?;

    config::atomic_private_write(&path, &original)?;
    control::refresh_access(&directory).await?;
    let mut ws = established(&lab).await?;
    let mut invalid: config::DaemonConfig = config::read(&path)?;
    invalid.max_concurrent_jobs = 0;
    config::write(&path, &invalid)?;
    let failure = control::refresh_access(&directory).await.unwrap_err();
    assert_eq!(error::code(&failure), Some(ErrorCode::InvalidConfig));
    invalidated(&mut ws, "INVALID_CONFIG").await?;

    config::atomic_private_write(&path, &original)?;
    control::refresh_access(&directory).await?;
    let mut ws = established(&lab).await?;
    let identity_path = directory.join("identity.toml");
    let original_identity = std::fs::read(&identity_path)?;
    let mut identity: config::Identity = config::read(&identity_path)?;
    identity.device_id = lab.source_identity.device_id.clone();
    config::write(&identity_path, &identity)?;
    let failure = control::refresh_access(&directory).await.unwrap_err();
    assert_eq!(error::code(&failure), Some(ErrorCode::IdentityChanged));
    invalidated(&mut ws, "IDENTITY_CHANGED").await?;
    config::atomic_private_write(&identity_path, &original_identity)?;
    control::refresh_access(&directory)
        .await
        .context("restore isolated identity")?;
    Ok(())
}
