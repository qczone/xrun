use super::*;
use crate::store::Submission;
use futures_util::SinkExt;
use serde::Deserialize;

/// An explicit script submission. Keep request_id unchanged when recovering an
/// uncertain result; repeated submissions query the original job without replay.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    /// Caller-generated stable submission ID.
    pub request_id: String,
    /// UTF-8 script passed to the selected interpreter without local shell parsing.
    pub script: String,
    /// Explicit remote interpreter: sh, bash, zsh, powershell, pwsh or cmd.
    pub shell: String,
    /// Absolute remote working directory; None uses the endpoint's current default.
    pub cwd: Option<String>,
    /// Execution deadline in seconds.
    pub timeout: u64,
}

pub(super) fn check_binding(id: &Identity, target: &str, receipt: &Submission) -> Result<()> {
    if receipt.source_device_id != id.device_id
        || receipt.ca_pin != crate::crypto::ca_spki_pin(&id.ca_pem)?
    {
        bail!(ErrorCode::IdentityMismatch.error("submission belongs to another identity"));
    }
    if receipt.target_device_id != target || receipt.kind != JobKind::Exec {
        bail!(ErrorCode::DeviceMismatch.error("submission belongs to another target or operation"));
    }
    Ok(())
}

/// Submit a durable command once. Any later use of its request ID only looks up
/// the original result, including after App restart or an acknowledgement timeout.
pub async fn submit(target: &str, command: CommandRequest) -> Result<Job> {
    if command.request_id.is_empty()
        || command.request_id.len() > 128
        || command.script.trim().is_empty()
        || command.script.len() > MAX_INPUT
        || !["sh", "bash", "zsh", "powershell", "pwsh", "cmd"].contains(&command.shell.as_str())
        || command.timeout == 0
        || command.timeout > 86_400
    {
        bail!(
            ErrorCode::InvalidRequest.error("invalid command, interpreter, deadline or request ID")
        );
    }
    let id = identity().await?;
    let target = resolve(&id, target)?;
    let store = SubmissionStore::open(&config::device_dir()?.join("submissions.sqlite"))?;
    if let Some(prior) = store.get(&command.request_id)? {
        check_binding(&id, &target, &prior)?;
        return find(&id, &target, &prior.db_id, &command.request_id).await;
    }
    let mut session = Session::open(&id, &target).await?;
    let input = command.script.into_bytes();
    let execution = Execution {
        request_id: command.request_id,
        db_id: session.db_id.clone(),
        program: String::new(),
        args: Vec::new(),
        cwd: command.cwd.unwrap_or(session.cwd.clone()),
        env: Default::default(),
        timeout: command.timeout,
        shell: Some(command.shell),
        input_size: input.len() as u64,
        input_sha256: sha256(&input),
    };
    let message = session.prepare_request(Request::Exec {
        execution: execution.clone(),
        follow: false,
    })?;
    let roster = crate::network::current(&id)?;
    let mut receipt = Submission {
        request_id: execution.request_id.clone(),
        source_device_id: id.device_id.clone(),
        target_device_id: target.clone(),
        target_name: roster.member(&target)?.name.clone(),
        ca_pin: crate::crypto::ca_spki_pin(&id.ca_pem)?,
        db_id: session.db_id.clone(),
        request_hash: execution.hash(),
        kind: JobKind::Exec,
        label: format!("{} script", execution.shell.as_deref().unwrap_or("sh")),
        created_at_ms: now_ms(),
        job_id: None,
        status: "unconfirmed".into(),
    };
    // This receipt commits before any bytes that could execute a command.
    store.save(&receipt)?;
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        session.ws.send(message).await?;
        net::send_bytes(&mut session.ws, &input).await?;
        response(&mut session).await
    })
    .await;
    let job = match result {
        Ok(Ok(Data::Job { job })) => job,
        Ok(Err(error))
            if crate::error::code(&error).is_some_and(|code| code.rejects_submission()) =>
        {
            receipt.status = "not_accepted".into();
            store.save(&receipt)?;
            return Err(error);
        }
        _ => bail!(ErrorCode::Unconfirmed.error(format!(
            "request {} may have executed; query its result instead of submitting again",
            receipt.request_id
        ))),
    };
    if job.request_id != receipt.request_id
        || job.db_id != receipt.db_id
        || job.source_device_id != id.device_id
        || job.target_device_id != target
        || job.request_hash != receipt.request_hash
        || job.kind() != JobKind::Exec
    {
        bail!(
            ErrorCode::Unconfirmed
                .error("task acknowledgement does not match the submission; query its result")
        );
    }
    receipt.status = "confirmed".into();
    receipt.job_id = Some(job.job_id.clone());
    if let Err(error) = store.save(&receipt) {
        tracing::warn!(
            "task {} was accepted; its local receipt could not be updated: {error}",
            job.job_id
        );
    }
    session.finish().await;
    Ok(job)
}
