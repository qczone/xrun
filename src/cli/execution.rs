//! Reliable submission, result recovery and interactive execution.
use crate::error::ErrorCode;
use crate::{
    config::Identity,
    crypto, net,
    protocol::*,
    store::{Submission, SubmissionStore},
};
use anyhow::{Context, Result, bail};
use futures_util::SinkExt;
use std::{collections::BTreeMap, time::Duration};

use super::args::Execute;
use super::logs::{LogCursor, receive_logs, stream_logs};
use super::support::*;

enum SubmissionOutcome {
    Response(Result<Box<Job>>),
    Interrupted,
    Terminated,
}
fn unconfirmed_submission(request: &str, recovery_hint: bool) -> anyhow::Error {
    let hint = if recovery_hint {
        "; use recent or jobs --request-id"
    } else {
        ""
    };
    anyhow::anyhow!(
        ErrorCode::Unconfirmed.error(format!("request {request} may have executed{hint}"))
    )
}
fn detached_job(job: &Job, json: bool) -> i32 {
    diagnostic(
        json,
        &anyhow::anyhow!(
            ErrorCode::Unconfirmed.error(format!("{} continues remotely", job_ref(job)))
        ),
    );
    75
}
pub(super) async fn run(
    id: &Identity,
    selected: (&str, &str),
    e: Execute,
    background: bool,
    json: bool,
    store: &SubmissionStore,
    prior: Option<Submission>,
) -> Result<i32> {
    let (target, target_name) = selected;
    let input = if e.stdin || e.script.is_some() {
        read_input(MAX_INPUT as u64)?
    } else {
        vec![]
    };
    if e.script
        .as_ref()
        .is_some_and(|s| !["sh", "bash", "zsh", "powershell", "pwsh", "cmd"].contains(&s.as_str()))
    {
        diagnostic(
            json,
            &anyhow::anyhow!(
                ErrorCode::InvalidShell.error("select sh, bash, zsh, powershell, pwsh or cmd")
            ),
        );
        return Ok(2);
    }
    let mut s = session(id, target).await?;
    let cwd = e.cwd.unwrap_or(s.cwd.clone());
    // Remote Windows paths must be validated by the target, not the source OS.
    let program = if e.script.is_some() {
        String::new()
    } else {
        e.command[0].clone()
    };
    let args = if e.script.is_some() {
        e.command
    } else {
        e.command[1..].to_vec()
    };
    let execution = Execution {
        request_id: e
            .request_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        db_id: prior
            .as_ref()
            .map(|p| p.db_id.clone())
            .unwrap_or(s.db_id.clone()),
        program,
        args,
        cwd,
        env: e.env.into_iter().collect::<BTreeMap<_, _>>(),
        timeout: e.timeout.unwrap_or(if background { 0 } else { 1800 }),
        shell: e.script,
        input_size: input.len() as u64,
        input_sha256: sha256(&input),
    };
    let hash = execution.hash();
    if let Some(prior) = &prior {
        if prior.request_hash != hash {
            diagnostic(
                json,
                &anyhow::anyhow!(
                    ErrorCode::RequestConflict.error("original execution parameters must match")
                ),
            );
            return Ok(2);
        }
        if prior.db_id != s.db_id {
            diagnostic(
                json,
                &anyhow::anyhow!(
                    ErrorCode::DbReset.error("original task database no longer exists")
                ),
            );
            return Ok(125);
        }
    }
    let mut submission = prior.unwrap_or(Submission {
        request_id: execution.request_id.clone(),
        source_device_id: id.device_id.clone(),
        target_device_id: target.into(),
        target_name: target_name.into(),
        ca_pin: crypto::ca_spki_pin(&id.ca_pem)?,
        db_id: execution.db_id.clone(),
        request_hash: hash,
        program: execution.program.clone(),
        created_at_ms: now_ms(),
        job_id: None,
        status: "not_accepted".into(),
    });
    let request = Request::Exec {
        execution: execution.clone(),
        follow: !background,
    };
    let prepared = match s.prepare_request(request) {
        Ok(message) => message,
        Err(error) if crate::error::is(&error, ErrorCode::MessageTooLarge) => {
            diagnostic(
                json,
                &anyhow::anyhow!(ErrorCode::InvalidCommand.error("execution header exceeds 1 MiB")),
            );
            return Ok(2);
        }
        Err(error) => {
            diagnostic(json, &error);
            return Ok(125);
        }
    };
    submission.status = "unconfirmed".into();
    store.save(&submission)?;
    let sent = std::sync::atomic::AtomicBool::new(false);
    let submitted = async {
        sent.store(true, std::sync::atomic::Ordering::SeqCst);
        // A failed write may still have sent bytes. Only local preparation
        // above can establish that the request was never attempted.
        s.ws.send(prepared).await?;
        net::send_bytes(&mut s.ws, &input).await?;
        match response(&mut s.ws).await? {
            Data::Job { job } => Ok(job),
            _ => bail!(ErrorCode::InvalidMessage.error("expected job acknowledgement")),
        }
    };
    let outcome = tokio::select! {
        result = submitted => SubmissionOutcome::Response(result.map(Box::new)),
        _ = tokio::signal::ctrl_c() => SubmissionOutcome::Interrupted,
        _ = termination() => SubmissionOutcome::Terminated,
    };
    let (job, acknowledged) = match outcome {
        SubmissionOutcome::Response(Ok(job)) => (*job, true),
        SubmissionOutcome::Response(Err(error)) => {
            if definitive(&error) {
                submission.status = "not_accepted".into();
                store.save(&submission)?;
                diagnostic(json, &error);
                return Ok(125);
            }
            match recover(id, target, &execution.request_id, Some(&execution.db_id)).await {
                Ok(job) => (job, false),
                Err(error) => {
                    if crate::error::is(&error, ErrorCode::DbReset) {
                        diagnostic(json, &error);
                        return Ok(125);
                    }
                    diagnostic(json, &unconfirmed_submission(&execution.request_id, true));
                    return Ok(75);
                }
            }
        }
        SubmissionOutcome::Interrupted | SubmissionOutcome::Terminated => {
            if !sent.load(std::sync::atomic::Ordering::SeqCst) {
                submission.status = "not_accepted".into();
                store.save(&submission)?;
                return Ok(if matches!(outcome, SubmissionOutcome::Interrupted) {
                    130
                } else {
                    125
                });
            }
            if matches!(outcome, SubmissionOutcome::Interrupted) {
                return Box::pin(cancel_unknown(
                    id,
                    target,
                    &execution.request_id,
                    &execution.db_id,
                    json,
                ))
                .await;
            }
            diagnostic(json, &unconfirmed_submission(&execution.request_id, false));
            return Ok(75);
        }
    };
    submission.job_id = Some(job_ref(&job));
    submission.status = "confirmed".into();
    if let Err(error) = store.save(&submission) {
        // The request ID was persisted before sending. A failed local update
        // cannot invalidate the target's acknowledgement or hide its task ID.
        diagnostic(
            json,
            &anyhow::anyhow!(ErrorCode::StorageError.error(format!(
                "task {} was accepted; local confirmation could not be saved: {error:#}",
                job_ref(&job)
            ))),
        );
    }
    if background {
        if acknowledged {
            s.finish().await;
        }
        print(json, &job, || println!("{target_name}/{}", job.job_id));
        return Ok(0);
    }
    let mut cursor = LogCursor {
        after: 0,
        db_id: Some(job.db_id.clone()),
        received: false,
    };
    let mut initial = acknowledged.then_some(s);
    let mut deadline = None;
    loop {
        let result = {
            let logs = async {
                if let Some(mut session) = initial.take() {
                    let job = receive_logs(&mut session.ws, false, &mut cursor).await?;
                    session.finish().await;
                    Ok(job)
                } else {
                    stream_logs(id, target, &job.job_id, true, false, &mut cursor).await
                }
            };
            tokio::pin!(logs);
            tokio::select! {
                r=&mut logs=>r,
                _=tokio::signal::ctrl_c()=>{return Box::pin(cancel_known(id,target,&job.job_id,json)).await},
                _=termination()=>return Ok(detached_job(&job, json))
            }
        };
        match result {
            Ok(result) => {
                if let Some(error) = result.error.as_ref().or(result.incomplete_reason.as_ref()) {
                    eprintln!("[xrun] {error}")
                }
                return Ok(job_code(&result));
            }
            Err(error) => {
                if crate::error::is(&error, ErrorCode::DbReset) {
                    diagnostic(json, &error);
                    return Ok(125);
                }
                if cursor.received {
                    deadline = None;
                    cursor.received = false;
                }
                let end =
                    *deadline.get_or_insert(tokio::time::Instant::now() + Duration::from_secs(30));
                if tokio::time::Instant::now() >= end {
                    diagnostic(
                        json,
                        &anyhow::anyhow!(
                            ErrorCode::Unconfirmed.error(format!(
                                "result unavailable for {}: {error}",
                                job_ref(&job)
                            ))
                        ),
                    );
                    return Ok(75);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}
async fn recover(
    id: &Identity,
    target: &str,
    request_id: &str,
    expected_db: Option<&str>,
) -> Result<Job> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match session(id, target).await {
                Ok(mut s) => {
                    if expected_db.is_some_and(|db| db != s.db_id) {
                        bail!(ErrorCode::DbReset.error("original task database no longer exists"))
                    }
                    s.send_request(Request::Jobs {
                        id: None,
                        running: false,
                        request_id: Some(request_id.into()),
                        limit: 1,
                        offset: 0,
                    })
                    .await?;
                    if let Data::Jobs { jobs, .. } = response(&mut s.ws).await? {
                        s.finish().await;
                        if let Some(job) = jobs.into_iter().next() {
                            return Ok(job);
                        }
                    }
                }
                Err(e) if net::explicit(&e) && !crate::error::is(&e, ErrorCode::DeviceOffline) => {
                    return Err(e);
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .context(ErrorCode::Unconfirmed.error("request recovery timed out"))?
}
async fn cancel_unknown(
    id: &Identity,
    target: &str,
    request_id: &str,
    db_id: &str,
    json: bool,
) -> Result<i32> {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let job = recover(id, target, request_id, Some(db_id)).await?;
        cancel_known(id, target, &job.job_id, json).await
    })
    .await;
    match result {
        Ok(Ok(code)) => Ok(code),
        _ => {
            diagnostic(
                json,
                &anyhow::anyhow!(
                    ErrorCode::Unconfirmed
                        .error(format!("cancellation unconfirmed for request {request_id}"))
                ),
            );
            Ok(75)
        }
    }
}
async fn cancel_known(id: &Identity, target: &str, job: &str, json: bool) -> Result<i32> {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        request(id, target, Request::Kill { id: job.into() }).await?;
        wait(id, target, job).await
    })
    .await;
    match result {
        Ok(Ok(job)) => {
            if json {
                show_job(true, &job)
            }
            Ok(if job.state == JobState::Canceled {
                130
            } else {
                job_code(&job)
            })
        }
        _ => {
            diagnostic(
                json,
                &anyhow::anyhow!(
                    ErrorCode::Unconfirmed
                        .error(format!("cancellation unconfirmed for {target}/{job}"))
                ),
            );
            Ok(75)
        }
    }
}

pub(super) async fn stream(id: &Identity, target: &str, e: Execute) -> Result<i32> {
    let mut s = session(id, target).await?;
    let execution = StreamExecution {
        program: e
            .command
            .first()
            .context(ErrorCode::InvalidRequest.error("program required"))?
            .clone(),
        args: e.command.into_iter().skip(1).collect(),
        cwd: e.cwd.unwrap_or_else(|| s.cwd.clone()),
        env: e.env.into_iter().collect(),
        timeout: e.timeout.unwrap_or(1800),
    };
    s.send_request(Request::StreamExec { execution }).await?;
    if !matches!(response(&mut s.ws).await?, Data::StreamReady) {
        bail!(ErrorCode::InvalidMessage.error("expected stream acknowledgement"))
    }
    let result = tokio::select! {
        result = crate::streaming::client(&mut s.ws) => result?,
        _ = tokio::signal::ctrl_c() => return Ok(130),
        _ = termination() => return Ok(125),
    };
    Ok(if result.timed_out {
        124
    } else if let Some(signal) = result.signal {
        128 + signal
    } else {
        match result.exit_code {
            Some(code) if (0..=255).contains(&code) => code as i32,
            Some(code) => {
                eprintln!("[xrun] remote exit code {code}");
                1
            }
            None => 125,
        }
    })
}
