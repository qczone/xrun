mod common;
use anyhow::Result;
use common::*;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn traffic_cli_reports_network_or_own_device_without_charging_queries() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut lab = Lab::new().await?;
        let report = json(cli(&lab.source, &["traffic", "--period", "all", "--json"]).await);
        assert!(report["device_id"].is_null());
        assert_eq!(report["complete"], true);
        assert!(report["totals"]["egress_bytes"].as_u64().unwrap() > 0);
        assert_eq!(
            report["network_id"],
            lab.source_identity.network.as_ref().unwrap().network_id
        );
        let own = json(cli(&lab.target, &["traffic", "--period", "all", "--json"]).await);
        assert_eq!(own["device_id"], lab.target_identity.device_id);
        assert_eq!(own["devices"].as_array().unwrap().len(), 1);
        assert_eq!(
            own["devices"][0]["device_id"],
            lab.target_identity.device_id
        );
        assert!(own["totals"]["egress_bytes"].as_u64().unwrap() > 0);
        assert!(
            ok(cli(&lab.target, &["traffic", "--period", "today"]).await).contains("received:")
        );
        let repeated = json(cli(&lab.source, &["traffic", "--period", "all", "--json"]).await);
        assert_eq!(repeated["totals"], report["totals"]);
        assert_eq!(
            cli(&lab.source, &["traffic", "--limit", "0"])
                .await
                .status
                .code(),
            Some(2)
        );
        assert_eq!(
            cli(&lab.source, &["traffic", "--period", "unknown"])
                .await
                .status
                .code(),
            Some(2)
        );
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
