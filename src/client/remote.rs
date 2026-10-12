//! Source-scoped remote commands for desktop adapters, with durable submission IDs.
mod capture;
mod command;
use crate::{
    config::{self, Identity},
    error::ErrorCode,
    net,
    protocol::*,
    session::Session,
    store::SubmissionStore,
};
use anyhow::{Context, Result, bail};
pub use capture::{ScreenshotDownload, screenshot};
pub use command::{CommandRequest, submit};
use serde::Serialize;
use std::time::Duration;

/// A local receipt used to recover an accepted or unconfirmed desktop submission.
#[derive(Serialize)]
pub struct Receipt {
    /// Durable caller-generated deduplication key.
    pub request_id: String,
    /// Target-local database generation.
    pub db_id: String,
    /// Acknowledged job ID, when available.
    pub job_id: Option<String>,
    /// Local admission state; this is not the remote execution result.
    pub status: String,
    /// Submission timestamp.
    pub created_at_ms: i64,
}

/// Read this identity's recent command receipts for a verified target, without network IO.
pub fn recent(target: &str) -> Result<Vec<Receipt>> {
    let id = Identity::load()?;
    let target = resolve(&id, target)?;
    let path = config::device_dir()?.join("submissions.sqlite");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let pin = crate::crypto::ca_spki_pin(&id.ca_pem)?;
    Ok(SubmissionStore::open(&path)?
        .recent()?
        .into_iter()
        .filter(|s| {
            s.target_device_id == target
                && s.source_device_id == id.device_id
                && s.ca_pin == pin
                && s.kind == JobKind::Exec
        })
        .take(20)
        .map(|s| Receipt {
            request_id: s.request_id,
            db_id: s.db_id,
            job_id: s.job_id,
            status: s.status,
            created_at_ms: s.created_at_ms,
        })
        .collect())
}

pub(super) fn resolve(id: &Identity, value: &str) -> Result<String> {
    let roster = crate::network::current(id)?;
    let target = roster.member(value)?;
    if target.revoked {
        bail!(ErrorCode::DeviceRevoked.error("target has been revoked"));
    }
    if target.device_id == id.device_id {
        bail!(ErrorCode::InvalidRequest.error("select another device for a remote command"));
    }
    Ok(target.device_id.clone())
}

pub(super) async fn identity() -> Result<Identity> {
    let mut id = Identity::load()?;
    net::renew_identity(&mut id).await?;
    Ok(id)
}
pub(super) async fn response(session: &mut Session) -> Result<Data> {
    match net::receive(&mut session.ws).await? {
        Data::Error { code, message } => bail!(crate::error::CodedError::from_wire(code, message)),
        value => Ok(value),
    }
}
pub(super) async fn open(id: &Identity, target: &str, db_id: &str) -> Result<Session> {
    let session = Session::open(id, target).await?;
    if session.db_id != db_id {
        bail!(ErrorCode::DbReset.error("original task database no longer exists"));
    }
    Ok(session)
}

pub(super) async fn find(
    id: &Identity,
    target: &str,
    db_id: &str,
    request_id: &str,
) -> Result<Job> {
    let mut session = open(id, target, db_id).await?;
    session
        .send_request(Request::Jobs {
            id: None,
            running: false,
            request_id: Some(request_id.into()),
            limit: 1,
            offset: 0,
        })
        .await?;
    let Data::Jobs { jobs, .. } = response(&mut session).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected a source-scoped task query"));
    };
    session.finish().await;
    let job = jobs
        .into_iter()
        .next()
        .context(ErrorCode::Unconfirmed.error(format!(
            "request {request_id} has no confirmed result; it will not be submitted again"
        )))?;
    if job.db_id != db_id
        || job.source_device_id != id.device_id
        || job.target_device_id != target
        || job.request_id != request_id
        || job.kind() != JobKind::Exec
    {
        bail!(ErrorCode::InvalidMessage.error("task query returned a different submission"));
    }
    Ok(job)
}

/// Recover a saved submission by its request ID. This never re-executes a command.
pub async fn query(target: &str, request_id: &str) -> Result<Job> {
    let id = identity().await?;
    let target = resolve(&id, target)?;
    let store = SubmissionStore::open(&config::device_dir()?.join("submissions.sqlite"))?;
    let mut receipt = store
        .get(request_id)?
        .context(ErrorCode::JobNotFound.error("no local submission receipt"))?;
    command::check_binding(&id, &target, &receipt)?;
    let job = tokio::time::timeout(
        Duration::from_secs(30),
        find(&id, &target, &receipt.db_id, request_id),
    )
    .await
    .context(ErrorCode::ConnectTimeout.error("task query timed out"))??;
    if job.request_hash != receipt.request_hash {
        bail!(ErrorCode::InvalidMessage.error("task query returned a different command"));
    }
    receipt.job_id = Some(job.job_id.clone());
    receipt.status = "confirmed".into();
    if let Err(error) = store.save(&receipt) {
        tracing::warn!("task receipt update failed: {error}");
    }
    Ok(job)
}

/// Bounded output page with the latest task snapshot.
#[derive(Serialize)]
pub struct Output {
    /// Latest remote snapshot.
    pub job: Job,
    /// Ordered stdout/stderr fragments, encoded as base64.
    pub events: Vec<LogEvent>,
    /// More stored output remains after this page.
    pub has_more: bool,
}

/// Read a task's output after a sequence number. Target authorization and source
/// ownership are checked for every read; reset database generations are rejected.
pub async fn output(target: &str, db_id: &str, job_id: &str, after: u64) -> Result<Output> {
    let id = identity().await?;
    let target = resolve(&id, target)?;
    let mut session = open(&id, &target, db_id).await?;
    session
        .send_request(Request::Logs {
            id: job_id.into(),
            after,
            follow: false,
            tail: None,
        })
        .await?;
    let mut events = Vec::new();
    let mut latest = None;
    let mut bytes = 0usize;
    let mut cursor = after;
    loop {
        match response(&mut session).await? {
            Data::Logs { events: chunk, job } => {
                for event in chunk {
                    if event.seq <= cursor {
                        bail!(ErrorCode::InvalidMessage.error("unordered task output"));
                    }
                    cursor = event.seq;
                    bytes += event.data_base64.len();
                    events.push(event);
                }
                if bytes >= 1024 * 1024 {
                    // Drop the unfinished session; the next page resumes from the
                    // last consumed sequence rather than buffering unbounded output.
                    return Ok(Output {
                        has_more: cursor < job.last_log_seq,
                        job,
                        events,
                    });
                }
                latest = Some(job);
            }
            Data::End => {
                session.finish().await;
                return Ok(Output {
                    job: latest.context("logs ended without a task snapshot")?,
                    events,
                    has_more: false,
                });
            }
            _ => bail!(ErrorCode::InvalidMessage.error("expected task output")),
        }
    }
}

/// Request cancellation and return its acknowledged snapshot. Losing the response
/// yields UNCONFIRMED, so callers can query instead of assuming it was cancelled.
pub async fn cancel(target: &str, db_id: &str, job_id: &str) -> Result<Job> {
    let id = identity().await?;
    let target = resolve(&id, target)?;
    let mut session = open(&id, &target, db_id).await?;
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        session
            .send_request(Request::Kill { id: job_id.into() })
            .await?;
        response(&mut session).await
    })
    .await
    .context(ErrorCode::Unconfirmed.error("cancellation response timed out; query the task"))??;
    let Data::Job { job } = result else {
        bail!(ErrorCode::InvalidMessage.error("expected cancellation snapshot"));
    };
    session.finish().await;
    Ok(job)
}
