//! Sequence-based output consumption and incomplete-output reporting.
use crate::error::{CodedError, ErrorCode};
use crate::{config::Identity, net::Ws, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::io::Write;

use super::support::*;

pub(super) fn output(events: &[LogEvent]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    for e in events {
        let bytes = STANDARD.decode(&e.data_base64)?;
        if e.stream == "stderr" {
            err.write_all(&bytes)?;
            err.flush()?
        } else {
            out.write_all(&bytes)?;
            out.flush()?
        }
    }
    Ok(())
}
pub(super) struct LogCursor {
    pub(super) after: u64,
    pub(super) db_id: Option<String>,
    pub(super) received: bool,
}
pub(super) fn log_error(job: &Job) -> Option<String> {
    log_failure(job).map(|error| error.to_string())
}
fn log_failure(job: &Job) -> Option<CodedError> {
    job.output_loss_reason.as_ref().map(|reason| {
        let code = match reason.as_str() {
            "TRUNCATED" => ErrorCode::LogTruncated,
            "LOG_EXPIRED" => ErrorCode::LogUnavailable,
            _ => ErrorCode::LogIncomplete,
        };
        code.error(reason.clone())
    })
}
pub(super) fn log_code(job: &Job, json: bool) -> i32 {
    if let Some(error) = log_failure(job) {
        diagnostic(json, &anyhow::anyhow!(error));
        1
    } else {
        0
    }
}
pub(super) async fn stream_logs(
    id: &Identity,
    target: &str,
    job: &str,
    follow: bool,
    json: bool,
    cursor: &mut LogCursor,
) -> Result<Job> {
    let mut s = session(id, target).await?;
    if cursor.db_id.as_ref().is_some_and(|db| *db != s.db_id) {
        bail!(ErrorCode::DbReset.error("original task database no longer exists"))
    }
    cursor.db_id = Some(s.db_id.clone());
    s.send_request(Request::Logs {
        id: job.into(),
        after: cursor.after,
        follow,
        tail: None,
    })
    .await?;
    let job = receive_logs(&mut s.ws, json, cursor).await?;
    s.finish().await;
    Ok(job)
}
pub(super) async fn receive_logs(ws: &mut Ws, json: bool, cursor: &mut LogCursor) -> Result<Job> {
    let mut final_job = None;
    loop {
        match response(ws).await? {
            Data::Logs { events, job } => {
                cursor.received = true;
                if json {
                    if !events.is_empty() {
                        println!("{}", serde_json::to_string(&events)?);
                    }
                } else {
                    output(&events)?
                };
                if let Some(e) = events.last() {
                    cursor.after = e.seq
                }
                final_job = Some(job)
            }
            Data::End => {
                return final_job
                    .context(ErrorCode::InvalidMessage.error("logs ended without job state"));
            }
            _ => bail!(ErrorCode::InvalidMessage.error("expected logs")),
        }
    }
}
pub(super) async fn collect_logs(
    id: &Identity,
    target: &str,
    job: &str,
    after: u64,
    tail: usize,
) -> Result<(Vec<LogEvent>, Job)> {
    let mut s = session(id, target).await?;
    s.send_request(Request::Logs {
        id: job.into(),
        after,
        follow: false,
        tail: Some(tail),
    })
    .await?;
    let mut events = vec![];
    let mut state = None;
    loop {
        match response(&mut s.ws).await? {
            Data::Logs { events: chunk, job } => {
                events.extend(chunk);
                state = Some(job);
            }
            Data::End => {
                s.finish().await;
                return Ok((
                    events,
                    state
                        .context(ErrorCode::InvalidMessage.error("logs ended without job state"))?,
                ));
            }
            _ => bail!(ErrorCode::InvalidMessage.error("expected logs")),
        }
    }
}
