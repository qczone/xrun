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
            let mut query = Request::Jobs {
                id: job,
                running,
                request_id,
                limit: limit.min(1000),
                offset,
            };
            let mut all = vec![];
            loop {
                match request(id, target, query.clone()).await? {
                    Data::Job { job } => {
                        show_job(json, &job);
                        break;
                    }
                    Data::Jobs { jobs, next_offset } => {
                        let Request::Jobs { limit, offset, .. } = &mut query else {
                            unreachable!()
                        };
                        if jobs.len() > *limit {
                            bail!(
                                ErrorCode::InvalidMessage
                                    .error("task page exceeds requested limit")
                            );
                        }
                        if let Some(next) = next_offset {
                            if jobs.is_empty() || offset.checked_add(jobs.len()) != Some(next) {
                                bail!(
                                    ErrorCode::InvalidMessage
                                        .error("invalid task page continuation")
                                );
                            }
                            *offset = next;
                        }
                        *limit -= jobs.len();
                        all.extend(jobs);
                        if next_offset.is_none() || *limit == 0 {
                            print(json, &all, || {
                                for job in &all {
                                    show_job(false, job)
                                }
                            });
                            break;
                        }
                    }
                    _ => bail!(ErrorCode::InvalidMessage.error("expected jobs")),
                }
            }
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
            let deadline =
                (timeout != 0).then(|| tokio::time::Instant::now() + Duration::from_secs(timeout));
            let result = if let Some(deadline) = deadline {
                match tokio::time::timeout_at(deadline, wait(id, target, &job)).await {
                    Ok(r) => r,
                    Err(_) => {
                        diagnostic(
                            json,
                            &anyhow::anyhow!(ErrorCode::WaitTimeout.error(
                                "waiting for task timed out; query jobs/logs for its result"
                            )),
                        );
                        return Ok(75);
                    }
                }
            } else {
                wait(id, target, &job).await
            };
            match result {
                Ok(job) => {
                    let (logs, logs_error) = if tail == 0 || job.kind() != JobKind::Exec {
                        (vec![], None)
                    } else {
                        let read = collect_logs(id, target, &job.job_id, 0, tail);
                        let result = if let Some(deadline) = deadline {
                            match tokio::time::timeout_at(deadline, read).await {
                                Ok(result) => result,
                                Err(_) => Err(anyhow::anyhow!(
                                    ErrorCode::WaitTimeout
                                        .error("task finished; reading its output timed out")
                                )),
                            }
                        } else {
                            read.await
                        };
                        match result {
                            Ok((logs, state)) => (tail_events(logs, tail), log_error(&state)),
                            Err(e) => (vec![], Some(e.to_string())),
                        }
                    };
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
                        if let Some(e) = job
                            .error_message
                            .as_ref()
                            .or(job.output_loss_reason.as_ref())
                        {
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
                let (all, state, latest) = if tail == 0 {
                    // A task snapshot gives an exact starting cursor without
                    // downloading output.
                    let value = request(
                        id,
                        target,
                        Request::Jobs {
                            id: Some(job.clone()),
                            running: false,
                            request_id: None,
                            limit: 1,
                            offset: 0,
                        },
                    )
                    .await?;
                    let Data::Job { job: state } = value else {
                        bail!(ErrorCode::InvalidMessage.error("expected job snapshot"));
                    };
                    let latest = state.last_log_seq.max(after);
                    (vec![], state, latest)
                } else {
                    let (all, state) = collect_logs(id, target, &job, after, tail).await?;
                    // Only advance past output actually returned.
                    let latest = all.last().map(|event| event.seq).unwrap_or(after);
                    (all, state, latest)
                };
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
