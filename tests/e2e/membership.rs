//! Membership assertions in the shared end-to-end lifecycle.
use super::*;
pub(super) async fn check(suite: &mut Suite, id: &str) -> Result<()> {
    let Suite {
        source,
        target,
        invite,
        server,
        target_daemon,
        ..
    } = suite;
    // Revoked identities retain distinct errors and cannot renew using their key.
    let not_admin = cli(target, &["revoke", "admin"]).await;
    assert_eq!(not_admin.status.code(), Some(125));
    let denied_invite = cli(target, &["invite", "--json"]).await;
    assert_eq!(denied_invite.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&denied_invite.stderr).contains("NOT_MANAGER"));
    ok(cli(source, &["revoke", "runner1"]).await);
    let revoked = cli(source, &["runner1", "jobs", id]).await;
    assert_eq!(revoked.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&revoked.stderr).contains("DEVICE_REVOKED"));
    let rejected = cli(target, &["join", invite, "--no-daemon"]).await;
    assert_eq!(rejected.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("DEVICE_REVOKED"));
    target_daemon.0.start_kill()?;
    server.abort();
    let _ = server.await;
    let unreachable = cli(source, &["status", "--json"]).await;
    assert_eq!(unreachable.status.code(), Some(125));
    let status: Value = serde_json::from_slice(&unreachable.stdout)?;
    assert_eq!(status["local"]["joined"], true);
    assert!(status["devices"].is_null());
    assert!(!status["server_error"].is_null());
    Ok(())
}
