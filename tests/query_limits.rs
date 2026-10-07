mod common;
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use std::time::Duration;
use xrun::testing::{net, protocol::*, store::TaskStore};

fn job(lab: &Lab, store: &TaskStore, id: &str) -> Job {
    Job {
        job_id: id.into(),
        request_id: format!("query-{id}"),
        request_hash: "hash".into(),
        source_device_id: lab.source_identity.device_id.clone(),
        target_device_id: lab.target_identity.device_id.clone(),
        db_id: store.db_id.clone(),
        program: "fixture".into(),
        args: vec![],
        cwd: lab.target.to_string_lossy().into(),
        state: JobState::Running,
        exit_code: None,
        signal: None,
        duration_ms: None,
        last_seq: 0,
        output_complete: true,
        incomplete_reason: None,
        error: None,
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
        leftover_possible: false,
        process: None,
    }
}

async fn tail(lab: &Lab, after: u64, lines: usize) -> Result<(Vec<LogEvent>, usize)> {
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
    net::send(
        &mut ws,
        &Data::Request {
            request: Request::Logs {
                id: "TA1101".into(),
                after,
                follow: false,
                tail: Some(lines),
            },
        },
    )
    .await?;
    let mut events = vec![];
    let mut bytes = 0;
    loop {
        let message = net::receive::<Data>(&mut ws).await?;
        bytes += serde_json::to_vec(&message)?.len();
        match message {
            Data::Logs { events: chunk, .. } => events.extend(chunk),
            Data::End => break,
            other => panic!("unexpected tail response: {other:?}"),
        }
    }
    Ok((events, bytes))
}

fn output(events: &[LogEvent]) -> Vec<u8> {
    events
        .iter()
        .flat_map(|e| STANDARD.decode(&e.data_base64).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_task_pages_continue_without_exceeding_message_limit() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut lab = Lab::new().await?;
        let store = TaskStore::open(&lab.target.join(".xrun/daemon.db"), false)?;
        for index in 0..50 {
            let mut job = job(&lab, &store, &format!("Q{index:05}"));
            job.args = vec!["x".repeat(24 * 1024)];
            job.state = JobState::Exited;
            job.exit_code = Some(0);
            store.insert(&job)?;
        }
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
        net::send(
            &mut ws,
            &Data::Request {
                request: Request::Jobs {
                    id: None,
                    running: false,
                    request_id: None,
                    limit: 50,
                    offset: 0,
                },
            },
        )
        .await?;
        let response = net::receive::<Data>(&mut ws).await?;
        assert!(serde_json::to_vec(&response)?.len() <= MAX_MESSAGE);
        let Data::Jobs { jobs, next_offset } = response else {
            panic!("expected jobs")
        };
        assert!(!jobs.is_empty() && jobs.len() < 50);
        assert_eq!(next_offset, Some(jobs.len()));
        drop(ws);
        let page = json(cli(&lab.source, &["target1", "jobs", "--json"]).await);
        let page = page.as_array().unwrap();
        assert_eq!(page.len(), 50);
        assert_eq!(page[0]["job_id"], "Q00049");
        assert_eq!(page[49]["job_id"], "Q00000");
        let page = json(
            cli(
                &lab.source,
                &[
                    "target1", "jobs", "--limit", "45", "--offset", "5", "--json",
                ],
            )
            .await,
        );
        assert_eq!(page.as_array().unwrap().len(), 45);
        assert_eq!(page[0]["job_id"], "Q00044");
        assert_eq!(page[44]["job_id"], "Q00000");
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_tail_reads_only_the_suffix_and_preserves_binary_event_order() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut lab = Lab::new().await?;
        let store = TaskStore::open(&lab.target.join(".xrun/daemon.db"), false)?;
        store.insert(&job(&lab, &store, "TA1101"))?;
        for _ in 0..64 {
            store.append("TA1101", "stdout", &b"old line\n".repeat(4096))?;
        }
        let boundary = store.get("TA1101")?.unwrap().last_seq;
        store.append("TA1101", "stdout", b"first\nsecond \xe4")?;
        store.append("TA1101", "stderr", b"\xb8\xad\nlast")?;
        store.append("TA1101", "stdout", b" line\n")?;
        let mut completed = store.get("TA1101")?.unwrap();
        completed.state = JobState::Exited;
        completed.exit_code = Some(0);
        xrun::testing::replace_task_fixture(&store, &completed)?;
        let (events, bytes) = tail(&lab, 0, 2).await?;
        assert_eq!(output(&events), "second 中\nlast line\n".as_bytes());
        assert!(bytes < 8192, "tail transferred {bytes} bytes");
        assert_eq!(
            events.iter().map(|e| e.stream.as_str()).collect::<Vec<_>>(),
            ["stdout", "stderr", "stdout"]
        );
        assert_eq!(
            output(&tail(&lab, boundary + 1, 10).await?.0),
            b"\xb8\xad\nlast line\n"
        );
        assert!(tail(&lab, u64::MAX, 2).await?.0.is_empty());
        assert!(tail(&lab, 0, 0).await?.0.is_empty());
        let logs = cli(&lab.source, &["target1", "logs", "TA1101", "--tail", "2"]).await;
        // stdout and stderr are intentionally separate; raw protocol above verifies their ordering.
        assert!(
            logs.status.success(),
            "{}",
            String::from_utf8_lossy(&logs.stderr)
        );
        assert_eq!(logs.stdout, b"second \xe4 line\n");
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execution_that_cannot_fit_its_acknowledgment_is_rejected_before_acceptance() -> Result<()>
{
    let mut lab = Lab::new().await?;
    let store = TaskStore::open(&lab.target.join(".xrun/daemon.db"), false)?;
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
    let request = Execution {
        request_id: "too-large-to-ack".into(),
        db_id: store.db_id.clone(),
        program: "unused".into(),
        args: vec!["x".repeat(MAX_MESSAGE - 60 * 1024)],
        cwd: lab.target.to_string_lossy().into(),
        env: Default::default(),
        timeout: 1,
        shell: None,
        input_size: 0,
        input_sha256: sha256(&[]),
    };
    net::send(
        &mut ws,
        &Data::Request {
            request: Request::Exec {
                execution: request,
                follow: false,
            },
        },
    )
    .await?;
    net::send_bytes(&mut ws, &[]).await?;
    assert!(
        matches!(net::receive::<Data>(&mut ws).await?, Data::Error { code, .. } if code == "INVALID_COMMAND")
    );
    assert!(
        store
            .by_request(&lab.source_identity.device_id, "too-large-to-ack")?
            .is_none()
    );
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    Ok(())
}
