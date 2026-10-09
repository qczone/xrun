mod common;
use anyhow::{Context, Result, bail};
use common::*;
use std::time::Duration;
use xrun::testing::{net, protocol::*, store::JobStore};

async fn open(lab: &Lab) -> Result<net::Ws> {
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
async fn admission(ws: &mut net::Ws, request: Request) -> Result<(Job, bool)> {
    net::send(ws, &Data::Request { request }).await?;
    let Data::Accepted { job, fresh } = net::receive(ws).await? else {
        bail!("job admission expected")
    };
    Ok((job, fresh))
}
fn store(lab: &Lab) -> Result<JobStore> {
    JobStore::open(&lab.target.join(".xrun/daemon.db"), false)
}
async fn finished(store: &JobStore, id: &str) -> Result<Job> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = store.get(id)?.context("accepted job missing")?;
            if job.state.terminal() {
                return Ok(job);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfers_deduplicate_by_type_and_request_without_replaying_files() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(25), async {
        let lab = Lab::new().await?;
        let store = store(&lab)?;
        let path = lab.target.join("uploaded.txt");
        let bytes = b"retained upload";
        let context = JobContext::new(&store.db_id);
        let request = Request::Push {context:context.clone(), path:path.to_string_lossy().into(), cwd:None,
            size:bytes.len() as u64, sha256:sha256(bytes), mkdir:false, no_overwrite:false, expect:None};
        let mut ws = open(&lab).await?;
        let (accepted, fresh) = admission(&mut ws, request.clone()).await?;
        assert!(fresh);
        assert_eq!(accepted.kind(), JobKind::Push);
        assert_eq!(accepted.state, JobState::Accepted);
        assert!(accepted.result.is_none());
        assert_eq!(store.active_count()?, 0, "file jobs must not consume command admission");
        net::send_bytes(&mut ws, bytes).await?;
        assert!(matches!(net::receive::<Data>(&mut ws).await?, Data::File {..}));
        assert!(matches!(net::receive::<Data>(&mut ws).await?, Data::Complete));
        let completed = finished(&store, &accepted.job_id).await?;
        assert_eq!(completed.state, JobState::Succeeded);
        assert_eq!(completed.attachments.len(), 1);
        let JobResult::File(result) = completed.result.as_ref().unwrap() else { panic!("file result") };
        assert_eq!(result.sha256, sha256(bytes));
        std::fs::write(&path, b"subsequent local edit")?;
        let (repeated, fresh) = admission(&mut ws, request.clone()).await?;
        assert!(!fresh);
        assert_eq!(repeated.job_id, accepted.job_id);
        assert_eq!(repeated.created_at_ms, accepted.created_at_ms);
        assert_eq!(repeated.state, JobState::Succeeded);
        assert!(matches!(net::receive::<Data>(&mut ws).await?, Data::Complete));
        assert_eq!(std::fs::read(&path)?, b"subsequent local edit");
        assert_eq!(std::fs::read(lab.target.join(".xrun/attachments").join(format!("{}.blob", completed.attachments[0].metadata.id)))?, bytes);
        let changed_type = Request::Pull {context:context.clone(), path:path.to_string_lossy().into(), cwd:None};
        net::send(&mut ws, &Data::Request {request:changed_type}).await?;
        assert!(matches!(net::receive::<Data>(&mut ws).await?, Data::Error {code,..} if code=="REQUEST_CONFLICT"));
        drop(ws);
        let mut reset_request = request;
        let Request::Push {context,..} = &mut reset_request else { unreachable!() };
        context.request_id = "never-admitted".into();
        context.db_id = "previous-database".into();
        let mut ws = open(&lab).await?;
        net::send(&mut ws, &Data::Request {request:reset_request}).await?;
        assert!(matches!(net::receive::<Data>(&mut ws).await?, Data::Error {code,..} if code=="DB_RESET"));
        assert!(store.by_request(&lab.source_identity.device_id, "never-admitted")?.is_none());
        let wait = json(cli(&lab.source, &["target1","wait",&accepted.job_id,"--json"]).await);
        assert!(wait["logs_error"].is_null());
        assert_eq!(wait["job"]["kind"], "push");
        let local = lab.source.join("second.txt");
        std::fs::write(&local, b"second upload")?;
        let second = json(cli(&lab.source, &["target1","push",local.to_str().unwrap(),"second.txt","--json"]).await);
        let recent = json(cli(&lab.source, &["recent","--json"]).await);
        let submission = recent.as_array().unwrap().iter().find(|record|record["request_id"] == second["request_id"]).context("CLI upload submission missing")?;
        assert_eq!(submission["kind"], "push");
        assert_eq!(submission["status"], "accepted");
        assert!(submission["job_id"].is_string());
        Ok::<_,anyhow::Error>(())
    }).await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_ends_incoming_upload_and_forward_jobs_without_hanging() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(25), async {
        let lab = Lab::new().await?;
        let store = store(&lab)?;
        let destination = lab.target.join("canceled-upload.txt");
        let mut upload = open(&lab).await?;
        let (job, _) = admission(
            &mut upload,
            Request::Push {
                context: JobContext::new(&store.db_id),
                path: destination.to_string_lossy().into(),
                cwd: None,
                size: 1,
                sha256: sha256(b"x"),
                mkdir: false,
                no_overwrite: false,
                expect: None,
            },
        )
        .await?;
        let canceled = json(cli(&lab.source, &["target1", "kill", &job.job_id, "--json"]).await);
        assert_eq!(canceled["state"], "canceled");
        assert_eq!(
            finished(&store, &job.job_id).await?.state,
            JobState::Canceled
        );
        assert!(!destination.exists());
        drop(upload);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let mut forward = open(&lab).await?;
        let (job, _) = admission(
            &mut forward,
            Request::Forward {
                context: JobContext::new(&store.db_id),
                port: listener.local_addr()?.port(),
            },
        )
        .await?;
        assert!(matches!(
            net::receive::<Data>(&mut forward).await?,
            Data::ForwardReady { .. }
        ));
        let (mut backend, _) = listener.accept().await?;
        let canceled = json(cli(&lab.source, &["target1", "kill", &job.job_id, "--json"]).await);
        assert_eq!(canceled["state"], "canceled");
        let mut byte = [0];
        use tokio::io::AsyncReadExt;
        assert_eq!(backend.read(&mut byte).await?, 0);
        assert_eq!(
            finished(&store, &job.job_id).await?.state,
            JobState::Canceled
        );
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_cancellation_persists_process_result_before_exit_ack_and_never_restarts()
-> Result<()> {
    tokio::time::timeout(Duration::from_secs(25), async {
        let lab = Lab::new().await?;
        let store = store(&lab)?;
        let source = lab.root.path().join("job-child.rs");
        std::fs::write(
            &source,
            "fn main() { std::thread::sleep(std::time::Duration::from_secs(60)); }",
        )?;
        let executable = lab.root.path().join(if cfg!(windows) {
            "job-child.exe"
        } else {
            "job-child"
        });
        let compiled = tokio::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .await?;
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let request = Request::StreamExec {
            context: JobContext::new(&store.db_id),
            execution: StreamExecution {
                program: executable.to_string_lossy().into(),
                args: vec![],
                cwd: lab.target.to_string_lossy().into(),
                env: Default::default(),
                timeout: 30,
            },
        };
        let mut stream = open(&lab).await?;
        let (job, _) = admission(&mut stream, request.clone()).await?;
        assert!(matches!(
            net::receive::<Data>(&mut stream).await?,
            Data::StreamReady
        ));
        net::send(&mut stream, &Data::End).await?;
        assert_eq!(store.active_count()?, 1);
        let running = store.get(&job.job_id)?.unwrap();
        assert_eq!(running.state, JobState::Running);
        assert!(running.process.is_some());
        let mut repeated = open(&lab).await?;
        let (same, fresh) = admission(&mut repeated, request).await?;
        assert!(!fresh);
        assert_eq!(same.job_id, job.job_id);
        assert_eq!(store.active_count()?, 1);
        drop(repeated);
        let killed = json(cli(&lab.source, &["target1", "kill", &job.job_id, "--json"]).await);
        assert_eq!(killed["state"], "canceled");
        let terminal = finished(&store, &job.job_id).await?;
        assert_eq!(terminal.state, JobState::Canceled);
        assert!(terminal.command_result().is_some());
        assert_eq!(terminal.output_complete, None);
        assert_eq!(store.active_count()?, 0);
        let Data::StreamExit { result } = net::receive(&mut stream).await? else {
            bail!("stream exit expected")
        };
        assert!(result.canceled);
        net::send(&mut stream, &Data::StreamExitAck).await?;
        assert!(matches!(
            net::receive::<Data>(&mut stream).await?,
            Data::Complete
        ));
        let logs = cli(&lab.source, &["target1", "logs", &job.job_id, "--json"]).await;
        assert!(!logs.status.success());
        assert!(String::from_utf8_lossy(&logs.stderr).contains("LOG_UNAVAILABLE"));
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
