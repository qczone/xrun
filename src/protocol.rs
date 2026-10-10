//! Negotiated wire messages and shared limits.
#![deny(missing_docs)]
mod job;
mod relay;
mod traffic;
mod version;
pub use job::*;
pub use relay::{ChallengeBinding, Proof, RelayMessage, valid_relay_route};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use traffic::*;
pub use version::{PROTOCOL, ProtocolRange, SIGNATURE_FORMAT};
/// Package release for display, diagnostics and local CLI/daemon IPC.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Maximum serialized JSON frame size in bytes.
pub const MAX_MESSAGE: usize = 1024 * 1024;
/// Maximum buffered execution stdin size in bytes.
pub const MAX_INPUT: usize = 1024 * 1024;
/// Maximum buffered screenshot payload size in bytes.
pub const MAX_SCREENSHOT: u64 = 64 * 1024 * 1024;
/// Maximum ciphertext frame payload in bytes, shared with Cloudflare.
pub const FILE_CHUNK: usize = 64 * 1024;
/// Maximum unacknowledged ciphertext per direction on relays without drain().
pub const RELAY_WINDOW: usize = 64 * FILE_CHUNK;
/// Maximum persisted output chunk in bytes.
pub const LOG_CHUNK: usize = 32 * 1024;
pub(crate) const MAX_NETWORK_MEMBERS: usize = 256;
pub(crate) const MAX_RELAY_ADDRESSES: usize = 8;
/// Relay heartbeats keep control bindings alive between requests.
pub(crate) const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);
pub(crate) const HEARTBEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
pub(crate) const RELAY_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
pub(crate) const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub(crate) const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub(crate) const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const REQUEST_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Member identity plus optional peer metadata; online reports a relay control binding.
pub struct Device {
    /// Immutable device ID.
    pub device_id: String,
    /// Human-readable device name.
    pub name: String,
    /// Whether the relay reports a live control binding; this does not imply authorization.
    pub online: bool,
    /// Whether the device is the network membership authority.
    pub admin: bool,
    /// Whether signed membership permanently rejects this key.
    pub revoked: bool,
    /// Peer-reported operating system, if available.
    pub os: Option<String>,
    /// Peer-reported CPU architecture, if available.
    pub arch: Option<String>,
    #[serde(rename = "daemon_version")]
    /// Peer package release for display and diagnostics.
    pub version: Option<String>,
    /// Peer-reported hostname, if available.
    pub hostname: Option<String>,
    /// User account under which remote processes execute, if reported.
    pub execution_user: Option<String>,
    /// Default working directory announced by the endpoint.
    pub default_cwd: Option<String>,
    /// Last peer observation in Unix milliseconds, if known.
    pub last_seen: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Inviter and signed mutual-access grant recorded during registration.
pub struct Registration {
    /// Immutable manager ID that invited this member, if any.
    pub inviter_id: Option<String>,
    /// Signed grant of mutual access with the inviter.
    pub allow_inviter: bool,
}
#[derive(Debug, Serialize, Deserialize)]
/// Anonymous pairing request, restricted to the network manager.
pub struct PairRequest {
    /// Peer package release for diagnostics.
    pub version: String,
    /// Implemented protocol range, negotiated before consuming an invitation.
    pub protocol: ProtocolRange,
    /// Single-use bearer invitation secret.
    pub token: String,
    /// Human-readable device name.
    pub name: String,
    /// Base64 DER certificate signing request for the device key.
    pub csr_base64: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Persisted execution intent; request identity enables deduplication without replay.
pub struct Execution {
    /// Caller-chosen idempotency key, scoped to source and target identity.
    pub request_id: String,
    /// Database generation that prevents replay after storage reset.
    pub db_id: String,
    /// Executable name or path.
    pub program: String,
    /// Arguments passed as separate values without implicit shell parsing.
    pub args: Vec<String>,
    /// Absolute working directory, or an empty value requesting the endpoint default.
    pub cwd: String,
    /// Explicit environment overrides applied after inherited build controls are filtered.
    pub env: BTreeMap<String, String>,
    /// Execution deadline in seconds; zero means no requested deadline.
    pub timeout: u64,
    /// Explicit script interpreter, when executing a script.
    pub shell: Option<String>,
    /// Declared buffered stdin size in bytes.
    pub input_size: u64,
    /// Lowercase SHA-256 digest of the buffered stdin bytes.
    pub input_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Streaming execution intent whose lifetime is tied to the connection.
pub struct StreamExecution {
    /// Executable name or path.
    pub program: String,
    /// Arguments passed as separate values without implicit shell parsing.
    pub args: Vec<String>,
    /// Absolute working directory, or an empty value requesting the endpoint default.
    pub cwd: String,
    /// Explicit environment overrides applied after inherited build controls are filtered.
    pub env: BTreeMap<String, String>,
    /// Execution deadline in seconds; zero means no requested deadline.
    pub timeout: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Final streaming process outcome, sent after output and before the exit acknowledgment.
pub struct StreamResult {
    /// Process exit code when available; separate from protocol/CLI failure status.
    pub exit_code: Option<i64>,
    /// Terminating Unix signal, if applicable.
    pub signal: Option<i32>,
    /// Whether the requested execution deadline was exceeded.
    pub timed_out: bool,
    /// Whether the process was stopped by an explicit job cancellation.
    pub canceled: bool,
    /// Elapsed process time in milliseconds.
    pub duration_ms: u64,
}
impl Execution {
    /// Compute the deduplication digest of immutable execution fields; no IO or mutation.
    pub fn hash(&self) -> String {
        let mut value = serde_json::to_value(self).expect("serializable execution");
        value.as_object_mut().unwrap().remove("request_id");
        value.as_object_mut().unwrap().remove("db_id");
        value["kind"] = serde_json::json!("exec");
        sha256(&serde_json::to_vec(&value).unwrap())
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Persistent job lifecycle; terminal states never transition back to active.
pub enum JobState {
    /// Accepted and awaiting execution.
    Accepted,
    /// Operation is running.
    Running,
    /// Operation completed successfully.
    Succeeded,
    /// Operation failed, including a nonzero process exit.
    Failed,
    /// Cancellation or daemon shutdown ended the task.
    Canceled,
    /// Requested task deadline ended execution.
    TimedOut,
    /// Execution may have started but its final outcome is unavailable; the intent is not replayed.
    Lost,
}
impl JobState {
    /// Whether lifecycle updates must no longer return this task to active state.
    pub fn terminal(&self) -> bool {
        !matches!(self, Self::Accepted | Self::Running)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Process proof used to avoid killing a reused PID during recovery.
pub struct ProcessIdentity {
    /// Operating system process ID.
    pub pid: u32,
    /// Host boot identifier used in process identity validation.
    pub boot_id: String,
    /// OS process-start marker used to detect PID reuse, if available.
    pub start: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// One accepted business job with typed parameters, outcome and attachments.
pub struct Job {
    /// Target-local job identifier within db_id.
    pub job_id: String,
    /// Caller-chosen idempotency key, scoped to source and target identity.
    pub request_id: String,
    /// Hash of operation type and immutable fields, excluding request_id and db_id.
    pub request_hash: String,
    /// Immutable identity that submitted the task or file operation.
    pub source_device_id: String,
    /// Immutable execution target identity.
    pub target_device_id: String,
    /// Database generation that prevents replay after storage reset.
    pub db_id: String,
    /// Operation type and its immutable, displayable parameters.
    #[serde(flatten)]
    pub details: JobDetails,
    /// Current persistent lifecycle state.
    pub state: JobState,
    /// Type-specific operation outcome.
    pub result: Option<JobResult>,
    /// Highest output sequence assigned; does not fall when logs are pruned.
    pub last_log_seq: u64,
    /// Currently retained output bytes.
    pub log_bytes: u64,
    /// Whether all expected output remains available.
    pub output_complete: Option<bool>,
    /// Cumulative strongest loss reason, such as LOG_EXPIRED, TRUNCATED or CAPTURE_ERROR.
    pub output_loss_reason: Option<String>,
    /// Machine-readable operation failure code, when recorded.
    pub error_code: Option<String>,
    /// Bounded diagnostic accompanying error_code.
    pub error_message: Option<String>,
    /// Creation timestamp in Unix milliseconds.
    pub created_at_ms: i64,
    /// Time the operation actually started.
    pub started_at_ms: Option<i64>,
    /// Time the operation entered a terminal state.
    pub finished_at_ms: Option<i64>,
    /// Last lifecycle/output update in Unix milliseconds.
    pub updated_at_ms: i64,
    /// Whether recovery could not prove that all previous processes were terminated.
    pub leftover_possible: bool,
    /// Persisted process proof used during crash recovery, if launched.
    pub process: Option<ProcessIdentity>,
    /// Retained file and screenshot snapshots.
    pub attachments: Vec<crate::attachments::Attachment>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// A sequenced binary output event from a task.
pub struct LogEvent {
    /// Monotonically increasing sequence within one task.
    pub seq: u64,
    /// Output stream name, stdout or stderr.
    pub stream: String,
    /// Base64 bytes; consumers must preserve binary data and split UTF-8 sequences.
    pub data_base64: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
/// Authenticated operation within an end-to-end peer session.
pub enum Request {
    /// Accept or deduplicate a persisted execution intent.
    Exec {
        /// Execution intent to validate and run.
        execution: Execution,
        #[serde(default)]
        /// Whether to stream future changes after the initial result.
        follow: bool,
    },
    /// Query task status, scoped to the authenticated source.
    Jobs {
        /// Target-local job identifier.
        id: Option<String>,
        /// Restrict the task query to active states.
        running: bool,
        /// Caller-chosen idempotency key, scoped to source and target identity.
        request_id: Option<String>,
        /// Maximum number of results requested.
        limit: usize,
        /// Result offset within the source-scoped task query.
        offset: usize,
    },
    /// Read sequenced output, optionally following updates.
    Logs {
        /// Target-local job identifier.
        id: String,
        /// Only return output with sequence greater than this value.
        after: u64,
        /// Whether to stream future changes after the initial result.
        follow: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Ask the endpoint to return only the final lines of the snapshot.
        /// Older endpoints may ignore this optimization; callers still trim locally.
        tail: Option<usize>,
    },
    /// Wait for a task to become terminal.
    Wait {
        /// Target-local job identifier.
        id: String,
    },
    /// Request cancellation of an accepted task.
    Kill {
        /// Target-local job identifier.
        id: String,
    },
    /// Receive a file with optional compare-and-replace semantics.
    Push {
        /// Identity of this upload attempt.
        context: JobContext,
        /// Operation path, resolved relative to cwd when permitted.
        path: String,
        /// Absolute working directory; `None` requests the endpoint default.
        cwd: Option<String>,
        /// Declared file payload size in bytes.
        size: u64,
        /// Lowercase SHA-256 hex digest of the file payload.
        sha256: String,
        /// Create missing parent directories for a push.
        mkdir: bool,
        /// Refuse to replace an existing destination.
        no_overwrite: bool,
        /// Expected current destination SHA-256 for a conditional replacement.
        expect: Option<String>,
    },
    /// Read a remote file.
    Pull {
        /// Identity of this download attempt.
        context: JobContext,
        /// Operation path, resolved relative to cwd when permitted.
        path: String,
        /// Absolute working directory, or an empty value requesting the endpoint default.
        cwd: Option<String>,
    },
    /// Capture a supported unlocked desktop.
    Screenshot {
        /// Identity of this capture attempt.
        context: JobContext,
    },
    /// Open a TCP connection to an endpoint-local port.
    Forward {
        /// Identity of this forwarding connection.
        context: JobContext,
        /// Endpoint-local TCP port.
        port: u16,
    },
    /// Run a streaming process bound to this session.
    StreamExec {
        /// Identity of this streaming command.
        context: JobContext,
        /// Execution intent to validate and run.
        execution: StreamExecution,
    },
}
impl Request {
    /// Oldest protocol that implements this operation and its semantic options.
    /// New operations and behavior-changing fields must add explicit version gates here.
    pub fn minimum_protocol(&self) -> u32 {
        match self {
            Self::Exec { .. }
            | Self::Jobs { .. }
            | Self::Logs { .. }
            | Self::Kill { .. }
            | Self::Wait { .. }
            | Self::Push { .. }
            | Self::Pull { .. }
            | Self::Screenshot { .. }
            | Self::Forward { .. }
            | Self::StreamExec { .. } => 1,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
/// Endpoint request/response messages; Complete bounds one cached-session operation.
pub enum Data {
    /// Checks a cached session before sending an operation. A roster change
    /// requires a new authenticated exchange.
    SessionProbe {
        /// Accepted signed roster version.
        roster_version: u64,
    },
    /// All response frames for this request have been sent.
    Complete,
    /// Authenticated endpoint identity, storage generation and execution default.
    Ready {
        /// Peer package release for diagnostics.
        version: String,
        /// Implemented protocol range.
        protocol: ProtocolRange,
        /// Highest common version selected during authenticated roster exchange.
        selected_protocol: u32,
        /// Immutable device ID.
        device_id: String,
        /// Database generation that prevents replay after storage reset.
        db_id: String,
        /// Default working directory announced by the endpoint.
        default_cwd: String,
    },
    /// Authenticated operation within an end-to-end peer session.
    Request {
        /// Authenticated operation request.
        request: Request,
    },
    /// One accepted business job with typed parameters, outcome and attachments.
    Job {
        /// Task snapshot associated with the response.
        job: Job,
    },
    /// Admission result for a connection-bound operation, before any payload.
    Accepted {
        /// Original or freshly accepted job.
        job: Job,
        /// False for a repeated request; its data stream is never replayed.
        fresh: bool,
    },
    /// Query task status, scoped to the authenticated source.
    Jobs {
        /// Source-scoped task snapshots.
        jobs: Vec<Job>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Continue the requested page here when its byte budget was reached.
        next_offset: Option<usize>,
    },
    /// Read sequenced output, optionally following updates.
    Logs {
        /// Sequenced output events, possibly followed by a newer task snapshot.
        events: Vec<LogEvent>,
        /// Task snapshot associated with the response.
        job: Job,
    },
    /// File or screenshot metadata preceding binary payload.
    File {
        /// Operation path, resolved relative to cwd when permitted.
        path: String,
        /// Declared file payload size in bytes.
        size: u64,
        /// Lowercase SHA-256 hex digest of the following binary payload.
        sha256: String,
        /// Screenshot width in pixels, when the file is a capture.
        width: Option<u32>,
        /// Screenshot height in pixels, when the file is a capture.
        height: Option<u32>,
        /// Capture timestamp, when applicable.
        captured_at: Option<String>,
    },
    /// End of the current binary/log stream.
    End,
    /// Endpoint has connected to the requested local TCP port.
    ForwardReady {
        /// Endpoint-local TCP port.
        port: u16,
    },
    /// The endpoint consumed the forwarded input EOF.
    ForwardEofAck,
    /// The endpoint is ready for streaming stdin and output.
    StreamReady,
    /// Final streaming process result after output has ended.
    StreamExit {
        /// Final streaming process outcome.
        result: StreamResult,
    },
    /// Caller has received the final process result.
    StreamExitAck,
    /// Machine code plus human-readable diagnostic; dispatch only on code.
    Error {
        /// Stable machine-readable error code.
        code: String,
        /// Human-readable detail; changes do not define machine semantics.
        message: String,
    },
}
impl Data {
    /// Convert an internal error to its wire response.
    pub fn error(error: &anyhow::Error) -> Self {
        let (code, message) = crate::error::wire(error);
        Self::Error { code, message }
    }
}

pub(crate) fn validate_job_size(job: &Job) -> anyhow::Result<()> {
    // Leave room for process identity, final state and one encoded log chunk.
    if serde_json::to_vec(&Data::Job { job: job.clone() })?.len() > MAX_MESSAGE - 64 * 1024 {
        anyhow::bail!(
            crate::error::ErrorCode::InvalidCommand.error("task metadata exceeds response budget")
        );
    }
    Ok(())
}
/// Compute lowercase SHA-256 hex for arbitrary bytes.
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
/// Return current Unix time in milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
/// Command names that cannot be used as device selectors.
pub const RESERVED: &[&str] = &[
    "up",
    "join",
    "invite",
    "allow-from",
    "deny-from",
    "revoke",
    "status",
    "traffic",
    "recent",
    "down",
    "server",
    "relay",
    "daemon",
    "doc",
    "start",
    "info",
    "jobs",
    "wait",
    "logs",
    "kill",
    "push",
    "pull",
    "screenshot",
    "forward",
    "help",
];
/// Validate the lowercase, 1–32 character, nonreserved device name contract.
pub fn valid_name(name: &str) -> bool {
    !RESERVED.contains(&name)
        && (1..=32).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    #[test]
    fn ordinary_messages_ignore_safe_metadata_but_keep_required_fields() {
        let request: Request = serde_json::from_value(serde_json::json!({
            "op":"forward", "port":1234, "context":{"request_id":"request","db_id":"db"}, "diagnostic":"optional metadata",
        }))
        .unwrap();
        assert!(matches!(request, Request::Forward { port: 1234, .. }));
        assert_eq!(request.minimum_protocol(), 1);
        assert!(serde_json::from_value::<Request>(serde_json::json!({"op":"forward"})).is_err());
        let ready: Data = serde_json::from_value(serde_json::json!({
            "type":"ready", "version":"future-release", "protocol":{"min":1,"max":2},
            "selected_protocol":2, "device_id":"device", "db_id":"db", "default_cwd":"/tmp",
            "diagnostic":"optional metadata",
        }))
        .unwrap();
        let Data::Ready {
            protocol,
            selected_protocol,
            ..
        } = ready
        else {
            panic!("ready")
        };
        ProtocolRange::CURRENT
            .confirm(protocol, selected_protocol)
            .unwrap();
    }
    #[test]
    fn relay_limits_match_the_cross_implementation_contract() {
        let contract: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/relay-limits.json")).unwrap();
        assert_eq!(
            FILE_CHUNK,
            contract["frameBytes"].as_u64().unwrap() as usize
        );
        assert_eq!(
            RELAY_WINDOW,
            contract["windowBytes"].as_u64().unwrap() as usize
        );
    }
}
