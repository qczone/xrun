//! Actual desktop client operations against isolated local relay and device profiles.
mod common;
use anyhow::{Context, Result};
use common::*;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use xrun::{
    client::remote,
    testing::config::{self, DaemonConfig},
};

#[tokio::test]
async fn desktop_operation_fixture() -> Result<()> {
    let Ok(source) = std::env::var("XRUN_DESKTOP_OPERATION") else {
        return Ok(());
    };
    let input: Value = serde_json::from_str(&source)?;
    let target = input["target"].as_str().context("target")?;
    let result: Result<Value> = match input["operation"].as_str().context("operation")? {
        "submit" => remote::submit(target, serde_json::from_value(input["command"].clone())?)
            .await
            .and_then(|job| serde_json::to_value(job).map_err(Into::into)),
        "query" => remote::query(target, input["request_id"].as_str().context("request ID")?)
            .await
            .and_then(|job| serde_json::to_value(job).map_err(Into::into)),
        "recent" => {
            remote::recent(target).and_then(|rows| serde_json::to_value(rows).map_err(Into::into))
        }
        "output" => remote::output(
            target,
            input["db_id"].as_str().context("database")?,
            input["job_id"].as_str().context("job")?,
            input["after"].as_u64().unwrap_or(0),
        )
        .await
        .and_then(|output| serde_json::to_value(output).map_err(Into::into)),
        "cancel" => remote::cancel(
            target,
            input["db_id"].as_str().context("database")?,
            input["job_id"].as_str().context("job")?,
        )
        .await
        .and_then(|job| serde_json::to_value(job).map_err(Into::into)),
        other => anyhow::bail!("unknown fixture operation {other}"),
    };
    let output = match result {
        Ok(value) => json!({"value":value}),
        Err(error) => json!({"error":xrun::protocol::Data::error(&error)}),
    };
    println!("XRUN_DESKTOP_RESULT {output}");
    Ok(())
}

async fn operation(home: &Path, input: Value) -> Result<Value> {
    let output = tokio::time::timeout(
        Duration::from_secs(40),
        tokio::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "desktop_operation_fixture", "--nocapture"])
            .env("XRUN_DESKTOP_OPERATION", input.to_string())
            .env("HOME", home)
            .env("USERPROFILE", home)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    let output = ok(output);
    let data = output
        .lines()
        .find_map(|line| line.strip_prefix("XRUN_DESKTOP_RESULT "))
        .context("desktop fixture result")?;
    Ok(serde_json::from_str(data)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_grants_only_the_verified_manager_and_respects_declined_consent() -> Result<()> {
    let root = tempfile::tempdir()?;
    let manager_home = root.path().join("manager");
    let (mut relay, link) = TestRelay::new(&root.path().join("relay")).await?;
    let created = json(
        cli(
            &manager_home,
            &[
                "connect",
                &link,
                "--name",
                "mac-pro",
                "--no-daemon",
                "--json",
            ],
        )
        .await,
    );
    let mut manager = logged(&manager_home, &["daemon"], "manager")?.spawn()?;
    online(&manager_home, "mac-pro").await?;
    for (name, grant, requested) in [("member1", true, false), ("member2", false, true)] {
        let home = root.path().join(name);
        let invite = json(
            cli(
                &manager_home,
                if requested {
                    &["invite", "--allow", "--json"]
                } else {
                    &["invite", "--json"]
                },
            )
            .await,
        );
        let mut args = vec![
            "connect",
            invite["link"].as_str().context("invitation")?,
            "--name",
            name,
            "--no-daemon",
            "--json",
        ];
        if grant {
            args.push("--allow-manager");
        }
        let member = json(cli(&home, &args).await);
        let policy: DaemonConfig = config::read(&home.join(".xrun/daemon.toml"))?;
        let manager_id = created["device_id"].as_str().context("manager ID")?;
        assert!(!policy.allow_all);
        assert_eq!(policy.allow_from.contains(&manager_id.into()), grant);
        assert_eq!(policy.deny_from.contains(&manager_id.into()), !grant);
        let manager_policy: DaemonConfig = config::read(&manager_home.join(".xrun/daemon.toml"))?;
        let member_id = member["device_id"].as_str().context("member ID")?;
        assert!(!manager_policy.allow_from.contains(&member_id.into()));
        let mut daemon = logged(&home, &["daemon"], name)?.spawn()?;
        online(&manager_home, name).await?;
        // A manual all-members policy and stale individual grants must not
        // expose the manager's root key through incoming business operations.
        let mut stale = manager_policy;
        stale.allow_all = true;
        stale.allow_from.push(member_id.into());
        config::write(&manager_home.join(".xrun/daemon.toml"), &stale)?;
        let denied = cli(&home, &["mac-pro", "--", "unused"]).await;
        assert_eq!(denied.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&denied.stderr).contains("MANAGER_PROTECTED"));
        let denied = cli(&manager_home, &["allow-from", "--all"]).await;
        assert_eq!(denied.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&denied.stderr).contains("MANAGER_PROTECTED"));
        let result = cli(
            &manager_home,
            &[
                name,
                "--json",
                "--",
                if cfg!(windows) {
                    "whoami.exe"
                } else {
                    "whoami"
                },
            ],
        )
        .await;
        if grant {
            ok(result);
        } else {
            assert_eq!(result.status.code(), Some(125));
            assert_eq!(
                serde_json::from_slice::<Value>(&result.stderr)?["code"],
                "SOURCE_NOT_ALLOWED"
            );
        }
        stop_daemon(&home, &mut daemon).await?;
    }
    let stranger = root.path().join("stranger");
    let refused = cli(
        &stranger,
        &[
            "connect",
            &link,
            "--name",
            "stranger",
            "--no-daemon",
            "--json",
        ],
    )
    .await;
    assert_eq!(refused.status.code(), Some(125));
    assert_eq!(
        serde_json::from_slice::<Value>(&refused.stderr)?["code"],
        "RELAY_CLAIMED"
    );
    assert!(!stranger.join(".xrun/identity.toml").exists());
    stop_daemon(&manager_home, &mut manager).await?;
    relay.stop().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn desktop_commands_survive_reopening_without_reexecution_and_reject_database_reset()
-> Result<()> {
    let mut lab = Lab::new().await?;
    let directory = lab.root.path().join("command workspace");
    std::fs::create_dir(&directory)?;
    let target = lab.target_identity.device_id.clone();
    let request_id = uuid::Uuid::new_v4().to_string();
    let script = if cfg!(windows) {
        "@echo off\r\necho once>> invocations.txt\r\necho hello-desktop\r\n"
    } else {
        "printf 'once\n' >> invocations.txt\nprintf 'hello-desktop\n'\n"
    };
    let command = json!({"request_id":request_id,"script":script,"shell":if cfg!(windows){"cmd"}else{"sh"},
        "cwd":directory,"timeout":30});
    let first = operation(
        &lab.source,
        json!({"operation":"submit","target":target,"command":command}),
    )
    .await?;
    let job = &first["value"];
    assert!(first["error"].is_null(), "{first}");
    let job_id = job["job_id"].as_str().context("job ID")?;
    let db_id = job["db_id"].as_str().context("database ID")?;
    let completed = json(cli(&lab.source, &["target1", "wait", job_id, "--json"]).await);
    assert_eq!(completed["job"]["state"], "succeeded");
    let mut different = command.clone();
    different["script"] = json!("echo unintended-replay");
    let retry = operation(
        &lab.source,
        json!({"operation":"submit","target":target,"command":different}),
    )
    .await?;
    assert_eq!(retry["value"]["job_id"], job_id);
    assert_eq!(
        std::fs::read_to_string(directory.join("invocations.txt"))?
            .lines()
            .count(),
        1
    );
    // Simulate losing only the local acknowledgement. A fresh process can query
    // the persisted request identity without reconstructing or repeating its script.
    let db = rusqlite::Connection::open(lab.source.join(".xrun/submissions.sqlite"))?;
    let mut receipt: Value = serde_json::from_str(&db.query_row(
        "SELECT data FROM submissions WHERE id=?1",
        [&request_id],
        |row| row.get::<_, String>(0),
    )?)?;
    receipt["status"] = json!("unconfirmed");
    receipt["job_id"] = Value::Null;
    db.execute(
        "UPDATE submissions SET data=?1 WHERE id=?2",
        [receipt.to_string(), request_id.clone()],
    )?;
    drop(db);
    let recovered = operation(
        &lab.source,
        json!({"operation":"query","target":target,"request_id":request_id}),
    )
    .await?;
    assert_eq!(recovered["value"]["job_id"], job_id);
    let output = operation(
        &lab.source,
        json!({"operation":"output","target":target,"db_id":db_id,"job_id":job_id}),
    )
    .await?;
    assert_eq!(output["value"]["job"]["state"], "succeeded");
    use base64::{Engine, engine::general_purpose::STANDARD};
    let text = output["value"]["events"]
        .as_array()
        .context("output events")?
        .iter()
        .flat_map(|event| {
            STANDARD
                .decode(event["data_base64"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(String::from_utf8(text)?.contains("hello-desktop"));
    let recent = operation(&lab.source, json!({"operation":"recent","target":target})).await?;
    assert_eq!(recent["value"][0]["request_id"], request_id);
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    ok(cli(&lab.target, &["daemon", "reset"]).await);
    lab.daemon = logged(&lab.target, &["daemon"], "reset-target")?.spawn()?;
    online(&lab.source, "target1").await?;
    let stale = operation(
        &lab.source,
        json!({"operation":"submit","target":target,"command":command}),
    )
    .await?;
    assert_eq!(stale["error"]["code"], "DB_RESET");
    assert_eq!(
        std::fs::read_to_string(directory.join("invocations.txt"))?
            .lines()
            .count(),
        1
    );
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    lab.relay.stop().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn desktop_cancel_acknowledges_the_remote_task() -> Result<()> {
    let mut lab = Lab::new().await?;
    let target = &lab.target_identity.device_id;
    let command = json!({"request_id":uuid::Uuid::new_v4().to_string(),
        "script":if cfg!(windows){"Start-Sleep -Seconds 60"}else{"sleep 60"},
        "shell":if cfg!(windows){"powershell"}else{"sh"},"cwd":Value::Null,"timeout":90});
    let submitted = operation(
        &lab.source,
        json!({"operation":"submit","target":target,"command":command}),
    )
    .await?;
    let job = &submitted["value"];
    assert!(submitted["error"].is_null(), "{submitted}");
    let cancelled = operation(
        &lab.source,
        json!({"operation":"cancel","target":target,
        "db_id":job["db_id"],"job_id":job["job_id"]}),
    )
    .await?;
    assert_eq!(cancelled["value"]["state"], "canceled", "{cancelled}");
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    lab.relay.stop().await?;
    Ok(())
}
