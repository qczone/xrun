//! Explicit, source-scoped file recovery. Local private snapshots never replay command jobs.
use crate::{
    config::{self, Identity},
    error::ErrorCode,
    net,
    protocol::*,
    session::Session,
    transfer::{
        self,
        checkpoint::{Checkpoint, RETENTION_MS},
    },
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Destination and replacement policy for one upload.
pub struct UploadOptions {
    /// Target path.
    pub path: String,
    /// Absolute target working directory.
    pub cwd: Option<String>,
    /// Create parent directories.
    pub mkdir: bool,
    /// Refuse any existing destination, including identical content.
    pub no_overwrite: bool,
    /// Current destination digest required before replacement.
    pub expect: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    network: String,
    source: String,
    target: String,
    ca_pin: String,
    request: Request,
    local: Option<PathBuf>,
    offset: u64,
    size: u64,
    sha256: String,
    created_at_ms: i64,
    expires_at_ms: i64,
}
/// Confirmed transfer result, including the exact recovered intent.
#[derive(Serialize)]
pub struct FileTransfer {
    /// Download destination, absent for uploads and stdout.
    pub path: Option<PathBuf>,
    /// Resolved target path.
    pub remote_path: String,
    /// Exact plaintext size.
    pub size: u64,
    /// Exact plaintext digest.
    pub sha256: String,
    /// All payload bytes already existed at the receiver.
    pub skipped: bool,
    /// Verified prefix reused by this call.
    pub resumed_from: u64,
    /// Original target job snapshot.
    pub job: Job,
}
/// A local unfinished transfer available to explicitly resume.
#[derive(Serialize)]
pub struct TransferReceipt {
    /// Original source request ID.
    pub request_id: String,
    /// Target identity.
    pub target_device_id: String,
    /// Upload or download.
    pub kind: JobKind,
    /// Target path label.
    pub label: String,
    /// Checkpoint deadline.
    pub expires_at_ms: i64,
}
fn dir() -> Result<PathBuf> {
    Ok(config::device_dir()?.join("client-transfers"))
}
fn target(id: &Identity, value: &str) -> Result<String> {
    super::remote::resolve(id, value)
}
fn saved(
    id: &Identity,
    target: &str,
    request: Request,
    local: Option<PathBuf>,
    size: u64,
    sha256: String,
) -> Result<Saved> {
    Ok(Saved {
        network: id
            .network
            .as_ref()
            .context("joined network")?
            .network_id
            .clone(),
        source: id.device_id.clone(),
        target: target.into(),
        ca_pin: crate::crypto::ca_spki_pin(&id.ca_pem)?,
        request,
        local,
        offset: 0,
        size,
        sha256,
        created_at_ms: now_ms(),
        expires_at_ms: now_ms() + RETENTION_MS,
    })
}
fn check(id: &Identity, target: &str, s: &Saved) -> Result<()> {
    if s.network != id.network.as_ref().context("joined network")?.network_id
        || s.source != id.device_id
        || s.target != target
        || s.ca_pin != crate::crypto::ca_spki_pin(&id.ca_pem)?
        || now_ms() >= s.expires_at_ms
    {
        bail!(
            ErrorCode::RequestConflict
                .error("saved transfer belongs to another identity, network or expired intent")
        );
    }
    if !matches!(s.request, Request::Push { .. } | Request::Pull { .. }) {
        bail!(ErrorCode::InvalidRequest.error("not a saved file intent"));
    }
    Ok(())
}
/// Read only this device's saved file intents for a verified target.
pub fn recent(value: &str) -> Result<Vec<TransferReceipt>> {
    let id = Identity::load()?;
    let target = target(&id, value)?;
    let base = dir()?;
    transfer::checkpoint::expire(&base, |_| Ok(()))?;
    let entries = match std::fs::read_dir(&base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let mut receipts = vec![];
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let bytes = std::fs::read(entry.path())?;
        if bytes.len() > 64 * 1024 {
            continue;
        }
        let s: Saved = serde_json::from_slice(&bytes)?;
        if check(&id, &target, &s).is_err() {
            continue;
        }
        let context = s.request.job_context().context("saved file binding")?;
        let details = s.request.job_details().unwrap();
        receipts.push(TransferReceipt {
            request_id: context.request_id,
            target_device_id: s.target,
            kind: details.kind(),
            label: details.label(),
            expires_at_ms: s.expires_at_ms,
        });
    }
    receipts.sort_by_key(|a| std::cmp::Reverse(a.expires_at_ms));
    receipts.truncate(32);
    Ok(receipts)
}
/// Upload an immutable copy once. On interruption the error carries a recovery ID.
pub async fn push(value: &str, local: &Path, options: UploadOptions) -> Result<FileTransfer> {
    let id = super::remote::identity().await?;
    push_with_identity(
        &id,
        &target(&id, value)?,
        local.to_string_lossy().as_ref(),
        options,
    )
    .await
}
pub(crate) async fn push_with_identity(
    id: &Identity,
    target: &str,
    local: &str,
    options: UploadOptions,
) -> Result<FileTransfer> {
    transfer::checkpoint::expire(&dir()?, |_| Ok(()))?;
    let snapshot = if local == "-" {
        transfer::snapshot_input(std::io::stdin().lock())?
    } else {
        let local = PathBuf::from(local);
        tokio::task::spawn_blocking(move || {
            let (file, _, _) = transfer::prepare_upload(&local)?;
            transfer::snapshot_input(file)
        })
        .await??
    };
    let (snapshot, size, hash) = snapshot;
    snapshot.as_file().sync_all()?;
    let session = Session::open(id, target).await?;
    let context = JobContext::new(&session.db_id);
    let mut cp = Checkpoint::open(&dir()?, &context.request_id)?;
    cp.adopt(snapshot)?;
    cp.save(saved(
        id,
        target,
        Request::Push {
            context,
            path: options.path,
            cwd: options.cwd,
            size,
            sha256: hash.clone(),
            mkdir: options.mkdir,
            no_overwrite: options.no_overwrite,
            expect: options.expect,
        },
        None,
        size,
        hash,
    )?)?;
    perform(id, target, session, cp, false).await
}
/// Download once to a chosen path; matching contents skip the network payload.
pub async fn pull(
    value: &str,
    remote: &str,
    local: Option<&Path>,
    cwd: Option<String>,
) -> Result<FileTransfer> {
    let id = super::remote::identity().await?;
    pull_with_identity(&id, &target(&id, value)?, remote, local, cwd).await
}
pub(crate) async fn pull_with_identity(
    id: &Identity,
    target: &str,
    remote: &str,
    local: Option<&Path>,
    cwd: Option<String>,
) -> Result<FileTransfer> {
    transfer::checkpoint::expire(&dir()?, |_| Ok(()))?;
    let session = Session::open(id, target).await?;
    let context = JobContext::new(&session.db_id);
    let mut cp = Checkpoint::open(&dir()?, &context.request_id)?;
    cp.new_blob()?.sync_all()?;
    let local = local
        .map(|path| {
            if path == Path::new("-") {
                Ok(path.to_owned())
            } else {
                Ok::<_, anyhow::Error>(std::env::current_dir()?.join(path))
            }
        })
        .transpose()?;
    cp.save(saved(
        id,
        target,
        Request::Pull {
            context,
            path: remote.into(),
            cwd,
        },
        local,
        0,
        sha256(&[]),
    )?)?;
    perform(id, target, session, cp, false).await
}
/// Resume only the saved file intent. This never reruns commands or creates a second job.
pub async fn resume(value: &str, request_id: &str) -> Result<FileTransfer> {
    let id = super::remote::identity().await?;
    resume_with_identity(&id, &target(&id, value)?, request_id).await
}
pub(crate) async fn resume_with_identity(
    id: &Identity,
    target: &str,
    request_id: &str,
) -> Result<FileTransfer> {
    let cp: Checkpoint<Saved> = Checkpoint::open(&dir()?, request_id)?;
    let s = cp
        .state
        .as_ref()
        .context(ErrorCode::InvalidRequest.error("no local transfer checkpoint"))?;
    check(id, target, s)?;
    let session = Session::open(id, target).await?;
    perform(id, target, session, cp, true).await
}
async fn perform(
    id: &Identity,
    target: &str,
    mut session: Session,
    mut cp: Checkpoint<Saved>,
    resume: bool,
) -> Result<FileTransfer> {
    let mut s = cp.state.clone().context("saved transfer")?;
    check(id, target, &s)?;
    let context = s.request.job_context().unwrap();
    if session.db_id != context.db_id {
        bail!(ErrorCode::DbReset.error("original transfer database no longer exists"));
    }
    let operation = async {
        let request = if resume {
            session
                .send_request(Request::Jobs {
                    id: None,
                    running: false,
                    request_id: Some(context.request_id.clone()),
                    limit: 1,
                    offset: 0,
                })
                .await?;
            let Data::Jobs { jobs, .. } = super::remote::response(&mut session).await? else {
                bail!(ErrorCode::InvalidMessage.error("expected original file job"));
            };
            let job =
                jobs.first()
                    .context(ErrorCode::Unconfirmed.error(
                        "original file intent was not confirmed; no resubmission performed",
                    ))?;
            if job.request_hash != s.request.job_hash().unwrap()
                || job.source_device_id != id.device_id
                || job.target_device_id != target
            {
                bail!(
                    ErrorCode::RequestConflict.error("original transfer differs from saved intent")
                );
            }
            // End this query before starting a separate recovery operation.
            session.finish().await;
            session = Session::open(id, target).await?;
            Request::ResumeTransfer {
                id: job.job_id.clone(),
                context: context.clone(),
            }
        } else {
            s.request.clone()
        };
        session.send_request(request).await?;
        let job = session.accept_transfer(resume).await?;
        if job.db_id != context.db_id
            || job.request_id != context.request_id
            || job.request_hash != s.request.job_hash().unwrap()
            || job.source_device_id != id.device_id
            || job.target_device_id != target
        {
            bail!(ErrorCode::InvalidMessage.error("file admission differs from saved intent"));
        }
        let upload = matches!(s.request, Request::Push { .. });
        let remote_path;
        if !upload {
            let Data::File {
                path, size, sha256, ..
            } = super::remote::response(&mut session).await?
            else {
                bail!(ErrorCode::InvalidMessage.error("expected immutable file header"));
            };
            remote_path = path;
            if resume && (size != s.size || sha256 != s.sha256) {
                bail!(ErrorCode::RequestConflict.error("resumed download snapshot differs"));
            }
            s.size = size;
            s.sha256 = sha256;
            if !resume
                && let Some(path) = s
                    .local
                    .as_ref()
                    .filter(|path| path.as_path() != Path::new("-"))
                && path.exists()
            {
                let (file, size, hash) = transfer::prepare_upload(path)?;
                if size == s.size && hash == s.sha256 {
                    let mut out = std::fs::File::options().write(true).open(&cp.blob)?;
                    std::io::copy(&mut { file }, &mut out)?;
                    out.sync_all()?;
                    s.offset = s.size;
                }
            }
            cp.save(s.clone())?;
            let file = std::fs::File::open(&cp.blob)?;
            net::send(
                &mut session.ws,
                &Data::TransferWant {
                    offset: s.offset,
                    sha256: net::prefix_hash(&file, s.offset)?,
                },
            )
            .await?;
        } else {
            remote_path = String::new();
        }
        let Data::TransferOffer {
            size,
            sha256,
            offset,
            skipped,
        } = super::remote::response(&mut session).await?
        else {
            bail!(ErrorCode::InvalidMessage.error("expected verified transfer cursor"));
        };
        if size != s.size
            || sha256 != s.sha256
            || offset > size
            || (skipped && offset != size)
            || (!upload && offset != s.offset)
        {
            bail!(
                ErrorCode::InvalidMessage.error("transfer cursor differs from immutable content")
            );
        }
        let resumed_from = offset;
        if !skipped {
            let file = std::fs::File::options()
                .read(true)
                .write(true)
                .open(&cp.blob)?;
            if upload {
                net::send_remaining(&mut session.ws, &file, size, offset, |offset| {
                    s.offset = offset;
                    cp.save(s.clone())
                })
                .await?;
            } else {
                net::receive_remaining(&mut session.ws, &file, size, &sha256, offset, |offset| {
                    s.offset = offset;
                    cp.save(s.clone())
                })
                .await?;
            }
        }
        let remote_path = if upload {
            let Data::File {
                path,
                size: confirmed,
                sha256: confirmed_hash,
                ..
            } = super::remote::response(&mut session).await?
            else {
                bail!(ErrorCode::InvalidMessage.error("expected completed upload"));
            };
            if confirmed != size || confirmed_hash != sha256 {
                bail!(ErrorCode::InvalidMessage.error("upload result differs"));
            }
            path
        } else {
            remote_path
        };
        session.finish().await;
        let path = if !upload {
            let mut file = std::fs::File::open(&cp.blob)?;
            if net::prefix_hash(&file, size)? != sha256 {
                bail!(
                    ErrorCode::ChecksumMismatch.error("local download differs before publication")
                );
            }
            std::io::Seek::rewind(&mut file)?;
            if s.local.as_deref() == Some(Path::new("-")) {
                std::io::copy(&mut file, &mut std::io::stdout().lock())?;
                None
            } else {
                Some(if let Some(path) = &s.local {
                    transfer::save_local_reader(path, &mut file)?
                } else {
                    let mut temp = tempfile::Builder::new().prefix("xrun-pull-").tempfile()?;
                    std::io::copy(&mut file, temp.as_file_mut())?;
                    temp.as_file().sync_all()?;
                    temp.keep()?.1
                })
            }
        } else {
            None
        };
        cp.clear()?;
        Ok(FileTransfer {
            path,
            remote_path,
            size,
            sha256,
            skipped,
            resumed_from,
            job,
        })
    }
    .await;
    operation.map_err(|error: anyhow::Error| {
        if crate::error::code(&error).is_some_and(|code| code.rejects_submission()) {
            return error;
        }
        ErrorCode::Unconfirmed
            .error(format!(
                "{error:#}; resume this file intent with xrun {target} resume {}",
                context.request_id
            ))
            .into()
    })
}

/// Explicitly abandon a saved file intent, canceling its source-owned remote job when known.
pub async fn cancel(value: &str, request_id: &str) -> Result<()> {
    let id = super::remote::identity().await?;
    let target = target(&id, value)?;
    let mut cp: Checkpoint<Saved> = Checkpoint::open(&dir()?, request_id)?;
    let s = cp
        .state
        .as_ref()
        .context(ErrorCode::InvalidRequest.error("no saved transfer"))?;
    check(&id, &target, s)?;
    let context = s.request.job_context().unwrap();
    let mut session = Session::open(&id, &target).await?;
    if session.db_id != context.db_id {
        bail!(ErrorCode::DbReset.error("original file database no longer exists"));
    }
    session
        .send_request(Request::Jobs {
            id: None,
            running: false,
            request_id: Some(context.request_id.clone()),
            limit: 1,
            offset: 0,
        })
        .await?;
    let Data::Jobs { jobs, .. } = super::remote::response(&mut session).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected original file job"));
    };
    session.finish().await;
    if let Some(job) = jobs.first() {
        if job.request_hash != s.request.job_hash().unwrap() {
            bail!(ErrorCode::RequestConflict.error("original file job differs"));
        }
        if !job.state.terminal() {
            super::remote::cancel(&target, &context.db_id, &job.job_id).await?;
        }
    }
    cp.clear()
}
