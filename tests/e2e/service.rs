//! Foreground Linux relay service startup and shutdown contract.
use super::*;

pub(crate) async fn foreground_shutdown() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path();
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let cfg = ServerConfig {
        port,
        addresses: vec![format!("127.0.0.1:{port}")],
        manual: true,
        no_detect: true,
        data_dir: home.join(".xrun/server"),
    };
    std::fs::create_dir_all(home.join(".xrun"))?;
    xrun::testing::config::write(&home.join(".xrun/config.toml"), &cfg)?;
    let link = xrun::testing::relay::deployment_link(&cfg)?;
    let mut child = common::logged(home, &["relay", "run"], "relay")?.spawn()?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            assert!(child.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    ok(cli(home, &["up", "--relay", &link, "--no-daemon", "--json"]).await);
    let id: xrun::testing::config::Identity =
        xrun::testing::config::read(&home.join(".xrun/identity.toml"))?;
    // Re-running up retains the manager identity and signing authority.
    ok(cli(home, &["up", "--relay", &link, "--no-daemon", "--json"]).await);
    let same: xrun::testing::config::Identity =
        xrun::testing::config::read(&home.join(".xrun/identity.toml"))?;
    assert_eq!(id.device_id, same.device_id);
    assert_eq!(id.ca_pem, same.ca_pem);
    unsafe {
        libc::kill(child.id().unwrap() as i32, libc::SIGTERM);
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await??
            .success()
    );
    // Missing manager identity must not silently recreate an authority.
    std::fs::remove_file(home.join(".xrun/identity.toml"))?;
    let rejected = cli(home, &["up", "--relay", &link, "--no-daemon"]).await;
    assert_eq!(rejected.status.code(), Some(125));
    Ok(())
}
