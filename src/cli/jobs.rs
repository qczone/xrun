//! Job listing, waiting, logs and cancellation commands.
use crate::error::ErrorCode;
use crate::{config::Identity, net, protocol::*};
use anyhow::{Result, bail};
use std::time::Duration;

use super::args::Remote;
use super::logs::*;
use super::support::*;

pub(super) async fn run(
    id: &Identity,
    target: &str,
    target_name: &str,
    command: Remote,
    json: bool,
) -> Result<i32> {
    match command {
        Remote::Jobs {
            id: job,
            running,
            request_id,
            limit,
            offset,
        } => {
            let job = match job {
                Some(value) => match parse_job(&value, target, target_name) {
                    Ok(id) => Some(id),
                    Err(e) => {
                        diagnostic(json, &e);
                        return Ok(2);
                    }
                },
                None => None,
            };
            let value = request(
                id,
                target,
                Request::Jobs {
                    id: job,
                    running,
                    request_id,
                    limit,
                    offset,
                },
            )
            .await?;
            match value {
                Data::Job { job } => show_job(json, &job),
                Data::Jobs { jobs } => print(json, &jobs, || {
                    for j in &jobs {
                        show_job(false, j)
                    }
                }),
                _ => bail!(ErrorCode::InvalidMessage.error("expected jobs")),
            };
            Ok(0)
        }
        Remote::Wait {
            id: job,
            timeout,
            tail,
        } => {
            let job = match parse_job(&job, target, target_name) {
                Ok(j) => j,
                Err(e) => {
                    diagnostic(json, &e);
                    return Ok(2);
                }
            };
            let result = if timeout == 0 {
                wait(id, target, &job).await
            } else {
                match tokio::time::timeout(Duration::from_secs(timeout), wait(id, target, &job))
                    .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        diagnostic(
                            json,
                            &anyhow::anyhow!(
                                ErrorCode::WaitTimeout.error("task continues running")
                            ),
                        );
                        return Ok(75);
                    }
                }
            };
            match result {
                Ok(job) => {
                    let (logs, logs_error) = if tail == 0 {
                        (vec![], None)
                    } else {
                        match collect_logs(id, target, &job.job_id, 0).await {
                            Ok((logs, state)) => (logs, log_error(&state)),
                            Err(e) => (vec![], Some(e.to_string())),
                        }
                    };
                    let logs = tail_events(logs, tail);
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({"job":job,"logs":logs,"logs_error":logs_error})
                        );
                    } else {
                        output(&logs)?;
                        if let Some(e) = &logs_error {
                            eprintln!("[xrun] {e}")
                        }
                        if let Some(e) = job.error.as_ref().or(job.incomplete_reason.as_ref()) {
                            eprintln!("[xrun] {e}")
                        }
                    }
                    Ok(job_code(&job))
                }
                Err(e) => {
                    diagnostic(json, &e);
                    Ok(
                        if net::explicit(&e) && !crate::error::is(&e, ErrorCode::DeviceOffline) {
                            125
                        } else {
                            75
                        },
                    )
                }
            }
        }
        Remote::Logs {
            id: job,
            follow,
            after,
            tail,
        } => {
            let job = match parse_job(&job, target, target_name) {
                Ok(j) => j,
                Err(e) => {
                    diagnostic(json, &e);
                    return Ok(2);
                }
            };
            if let Some(tail) = tail {
                let (all, state) = collect_logs(id, target, &job, after).await?;
                let latest = all.last().map(|e| e.seq).unwrap_or(after);
                let logs = tail_events(all, tail);
                if json {
                    println!("{}", serde_json::to_string(&logs)?)
                } else {
                    output(&logs)?
                }
                if !follow {
                    return Ok(log_code(&state, json));
                }
                let mut cursor = LogCursor {
                    after: latest,
                    db_id: Some(state.db_id),
                    received: false,
                };
                let state = stream_logs(id, target, &job, true, json, &mut cursor).await?;
                return Ok(log_code(&state, json));
            }
            let mut cursor = LogCursor {
                after,
                db_id: None,
                received: false,
            };
            let state = stream_logs(id, target, &job, follow, json, &mut cursor).await?;
            Ok(log_code(&state, json))
        }
        Remote::Kill { id: job } => {
            let job = match parse_job(&job, target, target_name) {
                Ok(j) => j,
                Err(e) => {
                    diagnostic(json, &e);
                    return Ok(2);
                }
            };
            let mut s = session(id, target).await?;
            let operation = async {
                s.send_request(Request::Kill { id: job }).await?;
                response(&mut s.ws).await
            };
            let result = match tokio::time::timeout(Duration::from_secs(10), operation).await {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    ErrorCode::Unconfirmed.error("cancellation response timed out")
                )),
            };
            match result {
                Ok(Data::Job { job }) => {
                    s.finish().await;
                    show_job(json, &job);
                    Ok(0)
                }
                Ok(_) => bail!(ErrorCode::InvalidMessage.error("expected job")),
                Err(e) => {
                    diagnostic(json, &e);
                    Ok(if definitive(&e) { 125 } else { 75 })
                }
            }
        }
        _ => bail!(ErrorCode::InvalidRequest.error("command dispatched to the wrong handler")),
    }
}
