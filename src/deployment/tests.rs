use super::*;
use crate::error::ErrorCode;

#[tokio::test]
async fn saved_deployment_survives_failure_and_reports_current_readiness_and_cleanup() -> Result<()>
{
    let reporter = DeploymentReporter::new("owned-test".into(), "cloudflare", "deploy", |_| {});
    reporter
        .scope(async {
            assert!(saved_result().is_none());
            report(
                DeploymentStage::Deployed,
                ProgressStatus::Completed,
                ProgressDetails {
                    deployment: Some(crate::cloudflare::DeployedRelay {
                        provider: "cloudflare",
                        name: "owned-test".into(),
                        account_id: "a".repeat(32),
                        link: "https://owned-test.example.workers.dev".into(),
                        region: Default::default(),
                    }),
                    ..Default::default()
                },
            );
            let (_, ready, cleanup) = saved_result().unwrap();
            assert!(!ready);
            assert_eq!(cleanup, "not_required");
            step(DeploymentStage::RelayCheck, async { Ok(()) }).await?;
            step(DeploymentStage::AuthorizationCleanup, async { Ok(()) }).await?;
            let (_, ready, cleanup) = saved_result().unwrap();
            assert!(ready);
            assert_eq!(cleanup, "complete");
            let pending = step::<()>(DeploymentStage::RelayCheck, std::future::pending());
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), pending)
                    .await
                    .is_err()
            );
            let failed: Result<()> = step(DeploymentStage::AuthorizationCleanup, async {
                Err(ErrorCode::OAuthRevocationFailed
                    .error("owned revocation fixture failed")
                    .into())
            })
            .await;
            assert!(failed.is_err());
            let (relay, ready, cleanup) = saved_result().unwrap();
            assert_eq!(relay.name, "owned-test");
            assert!(!ready);
            assert_eq!(cleanup, "unconfirmed");
            Ok::<_, anyhow::Error>(())
        })
        .await
}

#[tokio::test]
async fn progress_preserves_failures_and_cancellation_and_never_logs_handoff_secrets() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let log = temp.path().join("operations.log");
    let events = Arc::new(Mutex::new(Vec::new()));
    let received = events.clone();
    let reporter =
        DeploymentReporter::new("owned-test".into(), "cloudflare", "deploy", move |event| {
            received.lock().unwrap().push(event);
        })
        .with_log(log.clone());
    reporter
        .scope(async {
            report(
                DeploymentStage::Authorization,
                ProgressStatus::Waiting,
                ProgressDetails {
                    authorization_url: Some(
                        "https://dash.cloudflare.com/oauth2/auth?state=owned-test-secret".into(),
                    ),
                    ..Default::default()
                },
            );
            let failure: Result<()> = step(DeploymentStage::WorkerUpload, async {
                Err(ErrorCode::HttpError
                    .error("owned-test-private-diagnostic")
                    .into())
            })
            .await;
            assert!(error::is(&failure.unwrap_err(), ErrorCode::HttpError));
            let pending = step::<()>(DeploymentStage::RelayCheck, std::future::pending());
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), pending)
                    .await
                    .is_err()
            );
            Ok::<_, anyhow::Error>(())
        })
        .await?;
    let events = events.lock().unwrap();
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    assert!(
        events
            .iter()
            .any(|event| event.status == ProgressStatus::Failed)
    );
    assert_eq!(events.last().unwrap().status, ProgressStatus::Cancelled);
    let logged = std::fs::read_to_string(log)?;
    assert!(!logged.contains("owned-test-secret"));
    assert!(!logged.contains("owned-test-private-diagnostic"));
    for line in logged.lines() {
        let _: serde_json::Value = serde_json::from_str(line)?;
    }
    Ok(())
}
