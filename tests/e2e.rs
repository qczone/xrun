mod common;
#[path = "e2e/mod.rs"]
mod scenario;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execution_transfer_and_identity() -> anyhow::Result<()> {
    scenario::execution_transfer_and_identity().await
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn linux_relay_service_configuration_and_foreground_shutdown() -> anyhow::Result<()> {
    scenario::foreground_shutdown().await
}
