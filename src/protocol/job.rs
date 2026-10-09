//! Shared job identities, typed parameters and outcomes.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Request identity, retained when querying an uncertain operation.
pub struct JobContext {
    /// Caller-generated identity scoped to the authenticated source.
    pub request_id: String,
    /// Target database generation observed before submission.
    pub db_id: String,
}
impl JobContext {
    /// Create a new operation, rather than replaying a previous request.
    pub fn new(db_id: &str) -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            db_id: db_id.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Business operation recorded in the common lifecycle.
pub enum JobKind {
    /// Connection-independent command.
    Exec,
    /// Command bound to its input/output connection.
    StreamExec,
    /// File received by this device.
    Push,
    /// File sent by this device.
    Pull,
    /// Desktop capture sent by this device.
    Screenshot,
    /// Connection to a device-local TCP port.
    Forward,
}
impl JobKind {
    /// Stable database and wire representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::StreamExec => "stream_exec",
            Self::Push => "push",
            Self::Pull => "pull",
            Self::Screenshot => "screenshot",
            Self::Forward => "forward",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Displayable command parameters, without environment values or input bodies.
pub struct CommandParams {
    /// Program supplied by the caller.
    pub program: String,
    /// Explicit arguments, without implicit shell parsing.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: String,
    /// Requested deadline in seconds.
    pub timeout: u64,
    /// Explicit script interpreter.
    pub shell: Option<String>,
    /// Buffered input length, absent for live streams.
    pub input_size: Option<u64>,
    /// Buffered input digest, without persisting the input itself.
    pub input_sha256: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Immutable upload intent.
pub struct PushParams {
    /// Caller-supplied destination.
    pub path: String,
    /// Requested working directory.
    pub cwd: Option<String>,
    /// Declared payload length.
    pub size: u64,
    /// Declared payload SHA-256.
    pub sha256: String,
    /// Create missing parent directories.
    pub mkdir: bool,
    /// Refuse replacement.
    pub no_overwrite: bool,
    /// Conditional replacement digest.
    pub expect: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Immutable download intent.
pub struct PullParams {
    /// Caller-supplied source path.
    pub path: String,
    /// Requested working directory.
    pub cwd: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Current desktop capture parameters.
pub struct ScreenshotParams {}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Immutable loopback forwarding intent.
pub struct ForwardParams {
    /// Target-local TCP port.
    pub port: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "params", rename_all = "snake_case")]
/// Operation discriminator and its type-checked parameters.
pub enum JobDetails {
    /// Reliable command parameters.
    Exec(CommandParams),
    /// Streaming command parameters.
    StreamExec(CommandParams),
    /// Upload parameters.
    Push(PushParams),
    /// Download parameters.
    Pull(PullParams),
    /// Desktop capture parameters.
    Screenshot(ScreenshotParams),
    /// Loopback forwarding parameters.
    Forward(ForwardParams),
}
impl JobDetails {
    /// Operation discriminator, stored once in the database.
    pub fn kind(&self) -> JobKind {
        match self {
            Self::Exec(_) => JobKind::Exec,
            Self::StreamExec(_) => JobKind::StreamExec,
            Self::Push(_) => JobKind::Push,
            Self::Pull(_) => JobKind::Pull,
            Self::Screenshot(_) => JobKind::Screenshot,
            Self::Forward(_) => JobKind::Forward,
        }
    }
    /// Parameters for either command execution mode.
    pub fn command(&self) -> Option<&CommandParams> {
        match self {
            Self::Exec(value) | Self::StreamExec(value) => Some(value),
            _ => None,
        }
    }
    /// Compact local/CLI description of the operation.
    pub fn label(&self) -> String {
        match self {
            Self::Exec(p) | Self::StreamExec(p) => p.program.clone(),
            Self::Push(p) => p.path.clone(),
            Self::Pull(p) => p.path.clone(),
            Self::Screenshot(_) => "screenshot".into(),
            Self::Forward(p) => format!("localhost:{}", p.port),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Process outcome and optional live-stream byte counts.
pub struct CommandResult {
    /// Original process exit code.
    pub exit_code: Option<i64>,
    /// Original terminating signal.
    pub signal: Option<i32>,
    /// Process elapsed time.
    pub duration_ms: u64,
    /// Live stream input bytes, when recorded.
    pub input_bytes: Option<u64>,
    /// Live stdout bytes, when recorded.
    pub stdout_bytes: Option<u64>,
    /// Live stderr bytes, when recorded.
    pub stderr_bytes: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Outcome of a verified immutable file transfer.
pub struct FileResult {
    /// Resolved target-local path.
    pub path: String,
    /// Transferred byte count.
    pub size: u64,
    /// Transferred content digest.
    pub sha256: String,
    /// Optional copy-retention diagnostic; independent of transfer success.
    pub attachment_error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Outcome of the exact desktop capture transmitted to the caller.
pub struct ScreenshotResult {
    /// Capture timestamp in RFC 3339 format.
    pub captured_at: String,
    /// Capture width.
    pub width: u32,
    /// Capture height.
    pub height: u32,
    /// PNG byte count.
    pub size: u64,
    /// PNG content digest.
    pub sha256: String,
    /// Optional copy-retention diagnostic.
    pub attachment_error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Summary of one loopback connection, never its payload.
pub struct ForwardResult {
    /// Target-local port.
    pub port: u16,
    /// Connection elapsed time.
    pub duration_ms: u64,
    /// Bytes delivered to the target socket.
    pub input_bytes: u64,
    /// Bytes delivered to the calling device.
    pub output_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
/// Type-checked outcome; validated against the job's discriminator.
pub enum JobResult {
    /// Either command mode.
    Command(CommandResult),
    /// Either file direction.
    File(FileResult),
    /// Desktop capture.
    Screenshot(ScreenshotResult),
    /// Loopback connection.
    Forward(ForwardResult),
}
impl JobResult {
    /// Validate the result before committing a final state.
    pub fn matches(&self, kind: JobKind) -> bool {
        matches!(
            (self, kind),
            (Self::Command(_), JobKind::Exec | JobKind::StreamExec)
                | (Self::File(_), JobKind::Push | JobKind::Pull)
                | (Self::Screenshot(_), JobKind::Screenshot)
                | (Self::Forward(_), JobKind::Forward)
        )
    }
}
impl Job {
    /// Construct a fresh accepted intent, with no side effects yet performed.
    pub fn accepted(
        source: &str,
        target: &str,
        context: &JobContext,
        hash: String,
        details: JobDetails,
    ) -> Self {
        let now = now_ms();
        let output_complete = (details.kind() == JobKind::Exec).then_some(true);
        Self {
            job_id: new_job_id(),
            request_id: context.request_id.clone(),
            request_hash: hash,
            source_device_id: source.into(),
            target_device_id: target.into(),
            db_id: context.db_id.clone(),
            details,
            state: JobState::Accepted,
            result: None,
            last_log_seq: 0,
            log_bytes: 0,
            output_complete,
            output_loss_reason: None,
            error_code: None,
            error_message: None,
            created_at_ms: now,
            started_at_ms: None,
            finished_at_ms: None,
            updated_at_ms: now,
            leftover_possible: false,
            process: None,
            attachments: vec![],
        }
    }
    /// Operation discriminator.
    pub fn kind(&self) -> JobKind {
        self.details.kind()
    }
    /// Process result, if the operation is a command and its result is known.
    pub fn command_result(&self) -> Option<&CommandResult> {
        match &self.result {
            Some(JobResult::Command(result)) => Some(result),
            _ => None,
        }
    }
    /// Original process exit code.
    pub fn exit_code(&self) -> Option<i64> {
        self.command_result().and_then(|result| result.exit_code)
    }
    /// Original terminating signal.
    pub fn signal(&self) -> Option<i32> {
        self.command_result().and_then(|result| result.signal)
    }
    /// Elapsed command or connection time.
    pub fn duration_ms(&self) -> Option<u64> {
        match &self.result {
            Some(JobResult::Command(r)) => Some(r.duration_ms),
            Some(JobResult::Forward(r)) => Some(r.duration_ms),
            _ => None,
        }
    }
}
impl Request {
    /// Identity for a business operation; queries and cancellation do not create jobs.
    pub fn job_context(&self) -> Option<JobContext> {
        match self {
            Self::Exec { execution, .. } => Some(JobContext {
                request_id: execution.request_id.clone(),
                db_id: execution.db_id.clone(),
            }),
            Self::Push { context, .. }
            | Self::Pull { context, .. }
            | Self::Screenshot { context }
            | Self::Forward { context, .. }
            | Self::StreamExec { context, .. } => Some(context.clone()),
            _ => None,
        }
    }
    /// Displayable parameters, with input bodies and environment values omitted.
    pub fn job_details(&self) -> Option<JobDetails> {
        match self {
            Self::Exec { execution: e, .. } => Some(JobDetails::Exec(CommandParams {
                program: e.program.clone(),
                args: e.args.clone(),
                cwd: e.cwd.clone(),
                timeout: e.timeout,
                shell: e.shell.clone(),
                input_size: Some(e.input_size),
                input_sha256: Some(e.input_sha256.clone()),
            })),
            Self::StreamExec { execution: e, .. } => Some(JobDetails::StreamExec(CommandParams {
                program: e.program.clone(),
                args: e.args.clone(),
                cwd: e.cwd.clone(),
                timeout: e.timeout,
                shell: None,
                input_size: None,
                input_sha256: None,
            })),
            Self::Push {
                path,
                cwd,
                size,
                sha256,
                mkdir,
                no_overwrite,
                expect,
                ..
            } => Some(JobDetails::Push(PushParams {
                path: path.clone(),
                cwd: cwd.clone(),
                size: *size,
                sha256: sha256.clone(),
                mkdir: *mkdir,
                no_overwrite: *no_overwrite,
                expect: expect.clone(),
            })),
            Self::Pull { path, cwd, .. } => Some(JobDetails::Pull(PullParams {
                path: path.clone(),
                cwd: cwd.clone(),
            })),
            Self::Screenshot { .. } => Some(JobDetails::Screenshot(ScreenshotParams {})),
            Self::Forward { port, .. } => Some(JobDetails::Forward(ForwardParams { port: *port })),
            _ => None,
        }
    }
    /// Digest of operation kind and immutable wire intent, excluding request identity.
    pub fn job_hash(&self) -> Option<String> {
        if let Self::Exec { execution, .. } = self {
            return Some(execution.hash());
        }
        self.job_context()?;
        let mut value = serde_json::to_value(self).expect("serializable request");
        value.as_object_mut().unwrap().remove("context");
        Some(sha256(
            &serde_json::to_vec(&value).expect("serializable intent"),
        ))
    }
}

pub(crate) fn new_job_id() -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("random job identity");
    bytes
        .iter()
        .map(|byte| ALPHABET[(byte & 31) as usize] as char)
        .collect()
}
