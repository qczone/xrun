//! Execution assertions in the shared end-to-end lifecycle.
use super::*;
pub(super) async fn check(suite: &Suite) -> Result<String> {
    let Suite {
        source,
        target,
        runner,
        admin,
        ..
    } = suite;
    let runner = runner.to_string_lossy();
    assert_eq!(
        ok(cli(
            source,
            &[
                "runner1",
                "--",
                &runner,
                "echo",
                "hello world",
                "",
                "中文",
                "a\\\"b"
            ]
        )
        .await),
        "<hello world>\n<>\n<中文>\n<a\\\"b>\n"
    );
    let bytes = b"\0\xffhello\r\n";
    assert_eq!(
        input(
            source,
            &["runner1", "--stdin", "--", &runner, "input"],
            bytes
        )
        .await
        .stdout,
        bytes
    );
    let exit = cli(source, &["runner1", "--", &runner, "exit"]).await;
    assert_eq!(exit.status.code(), Some(7));
    assert_eq!(exit.stderr, b"error bytes\n");
    let timed = cli(
        source,
        &["runner1", "--timeout", "1", "--", &runner, "sleep"],
    )
    .await;
    assert_eq!(timed.status.code(), Some(124));
    let first = json(
        cli(
            source,
            &[
                "runner1",
                "start",
                "--request-id",
                "dedupe-1",
                "--env",
                "XRUN_TEST_SECRET=private-value",
                "--json",
                "--",
                &runner,
                "env",
            ],
        )
        .await,
    );
    let second = json(
        cli(
            source,
            &[
                "runner1",
                "start",
                "--request-id",
                "dedupe-1",
                "--env",
                "XRUN_TEST_SECRET=private-value",
                "--json",
                "--",
                &runner,
                "env",
            ],
        )
        .await,
    );
    assert_eq!(first["job_id"], second["job_id"]);
    assert_eq!(
        cli(
            source,
            &[
                "admin",
                "start",
                "--request-id",
                "dedupe-1",
                "--",
                &runner,
                "echo"
            ]
        )
        .await
        .status
        .code(),
        Some(2)
    );
    let conflict = cli(
        source,
        &[
            "runner1",
            "start",
            "--request-id",
            "dedupe-1",
            "--json",
            "--",
            &runner,
            "echo",
        ],
    )
    .await;
    assert_eq!(conflict.status.code(), Some(2));
    let id = first["job_id"].as_str().unwrap();
    let waited = json(cli(source, &["runner1", "wait", id, "--json"]).await);
    assert_eq!(waited["job"]["state"], "succeeded");
    // Expiring logs must retain the task result and original request deduplication.
    let tasks = xrun::testing::store::JobStore::open(&target.join(".xrun/daemon.db"), false)?;
    let mut expired = tasks.get(id)?.unwrap();
    expired.finished_at_ms = Some(now_ms() - 8 * 86_400_000);
    xrun::testing::replace_job_fixture(&tasks, &expired)?;
    tasks.prune()?;
    let unavailable = cli(source, &["runner1", "logs", id]).await;
    assert_eq!(unavailable.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unavailable.stderr).contains("LOG_UNAVAILABLE"));
    // JSON diagnostics retain their documented code as well as exit status.
    let expired_logs = tasks.get(id)?.unwrap();
    for (reason, code) in [
        ("LOG_EXPIRED", "LOG_UNAVAILABLE"),
        ("TRUNCATED", "LOG_TRUNCATED"),
        ("CAPTURE_ERROR: disk unavailable", "LOG_INCOMPLETE"),
    ] {
        let mut partial = expired_logs.clone();
        partial.output_loss_reason = Some(reason.into());
        xrun::testing::replace_job_fixture(&tasks, &partial)?;
        let result = cli(source, &["runner1", "logs", id, "--json"]).await;
        assert_eq!(result.status.code(), Some(1));
        let diagnostic: Value = serde_json::from_slice(&result.stderr)?;
        assert_eq!(diagnostic["code"], code);
        assert_eq!(diagnostic["message"], reason);
    }
    xrun::testing::replace_job_fixture(&tasks, &expired_logs)?;
    let waited = json(cli(source, &["runner1", "wait", id, "--json"]).await);
    assert_eq!(waited["job"]["state"], "succeeded");
    assert!(
        waited["logs_error"]
            .as_str()
            .unwrap()
            .contains("LOG_UNAVAILABLE")
    );
    assert_eq!(tasks.by_request(admin, "dedupe-1")?.unwrap().job_id, id);
    drop(tasks);
    let recent = json(cli(source, &["recent", "--json"]).await);
    assert!(
        recent
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["request_id"] == "dedupe-1" && s["status"] == "confirmed")
    );
    #[cfg(unix)]
    for (signal, request, code, canceled) in [
        (libc::SIGTERM, "term-test", 75, false),
        (libc::SIGINT, "interrupt-test", 130, true),
    ] {
        let child = command(
            source,
            &["runner1", "--request-id", request, "--", &runner, "sleep"],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
        let mut accepted = None;
        for _ in 0..100 {
            let jobs = json(
                cli(
                    source,
                    &["runner1", "jobs", "--request-id", request, "--json"],
                )
                .await,
            );
            if let Some(job) = jobs
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["state"] == "running")
            {
                accepted = Some(job["job_id"].as_str().unwrap().to_string());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let accepted = accepted.context("signal test task never started")?;
        assert_eq!(unsafe { libc::kill(child.id().unwrap() as i32, signal) }, 0);
        let out = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output()).await??;
        assert_eq!(
            out.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let state = json(cli(source, &["runner1", "jobs", &accepted, "--json"]).await);
        assert_eq!(
            state["state"],
            if canceled { "canceled" } else { "running" }
        );
        if !canceled {
            ok(cli(source, &["runner1", "kill", &accepted]).await);
        }
    }
    let detached = cli(source, &["runner1", "--", &runner, "detached"]).await;
    assert_eq!(detached.status.code(), Some(0));
    assert_eq!(detached.stdout, b"parent done");
    #[cfg(unix)]
    assert_eq!(
        ok(input(
            source,
            &["runner1", "--script", "sh"],
            b"printf 'script ok'"
        )
        .await),
        "script ok"
    );
    #[cfg(windows)]
    for script in [
        b"Write-Output 'script ok'\n".as_slice(),
        b"\xef\xbb\xbfWrite-Output 'script ok'\n".as_slice(),
    ] {
        assert_eq!(
            ok(input(source, &["runner1", "--script", "powershell"], script).await),
            "script ok\r\n"
        );
    }
    #[cfg(windows)]
    {
        // cmd paths and arguments must survive its own parser, not CRT quoting.
        let argument = "space & caret^ percent%XRUN_PATH_TEST% bang!";
        assert_eq!(
            ok(input(
                source,
                &["runner1", "--script", "cmd", "--", argument],
                b"@echo off\necho \"%~1\"\n"
            )
            .await),
            format!("\"{argument}\"\r\n")
        );
        let invalid = input(
            source,
            &["runner1", "--script", "cmd", "--", "a\"b"],
            b"echo unused\n",
        )
        .await;
        assert_eq!(invalid.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("INVALID_SCRIPT_ARGUMENT"));
        assert_eq!(
            ok(input(source,
                &["runner1", "--script", "cmd", "--", "", "marker", "", ""],
                b"@echo off\necho first=[%1]\necho second=[%~2]\necho third=[%3]\necho fourth=[%4]\necho fifth=[%5]\n",
            ).await),
            "first=[\"\"]\r\nsecond=[marker]\r\nthird=[\"\"]\r\nfourth=[\"\"]\r\nfifth=[]\r\n"
        );
    }
    // Capacity rejection does not create a job or poison the request ID.
    let mut jobs = vec![];
    for _ in 0..4 {
        jobs.push(
            ok(cli(source, &["runner1", "start", "--", &runner, "sleep"]).await)
                .trim()
                .to_string(),
        );
    }
    let busy = cli(
        source,
        &[
            "runner1",
            "start",
            "--request-id",
            "busy-retry",
            "--",
            &runner,
            "echo",
            "retry",
        ],
    )
    .await;
    assert_eq!(busy.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&busy.stderr).contains("DEVICE_BUSY"));
    assert_eq!(
        json(
            cli(
                source,
                &["runner1", "jobs", "--request-id", "busy-retry", "--json"]
            )
            .await
        ),
        serde_json::json!([])
    );
    for job in jobs {
        ok(cli(source, &["runner1", "kill", &job]).await);
    }
    let retry = cli(
        source,
        &[
            "runner1",
            "start",
            "--request-id",
            "busy-retry",
            "--",
            &runner,
            "echo",
            "retry",
        ],
    )
    .await;
    ok(retry);

    Ok(id.to_string())
}
