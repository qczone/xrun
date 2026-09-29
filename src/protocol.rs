use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_STDIN: usize = 1024 * 1024;
pub const MAX_EXEC_BODY: usize = 2 * 1024 * 1024;
pub const MAX_OUTPUT_CHUNK: usize = 32 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecRequest {
    pub request_id: String,
    pub target_device_id: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub stdin_base64: Option<String>,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Accepted,
    Starting,
    Running,
    Exited,
    Failed,
    Canceled,
    TimedOut,
    Lost,
    Unknown,
}

impl JobState {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Exited | Self::Failed | Self::Canceled | Self::TimedOut | Self::Lost
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorData {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: String,
    pub request_id: String,
    pub source_device_id: String,
    pub target_device_id: String,
    pub request_hash: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub state: JobState,
    pub last_confirmed_state: JobState,
    pub origin: String,
    pub exit_code: Option<i64>,
    pub signal: Option<i32>,
    pub duration_ms: Option<u64>,
    pub last_seq: u64,
    pub output_complete: bool,
    pub error: Option<ErrorData>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub dispatch_started: bool,
    pub target_store_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub device_id: String,
    pub name: String,
    pub os: String,
    pub arch: String,
    pub hostname: String,
    pub agent_version: String,
    pub execution_user: String,
    pub home_dir: String,
    pub default_cwd: String,
    pub path: String,
    pub online: bool,
    pub last_seen_ms: Option<u64>,
    pub allow_from: Vec<String>,
    pub store_id: String,
    pub boot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub job_id: String,
    pub seq: u64,
    pub stream: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    Hello {
        agent_version: String,
        store_id: String,
        boot_id: String,
        os: String,
        arch: String,
        hostname: String,
        execution_user: String,
        home_dir: String,
        default_cwd: String,
        path: String,
        allow_from: Vec<String>,
    },
    State {
        job: Job,
    },
    Output {
        event: LogEvent,
    },
    ReconcileResult {
        job: Job,
    },
    Logs {
        correlation_id: String,
        events: Vec<LogEvent>,
        done: bool,
    },
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    HelloAck {
        session_id: String,
    },
    Exec {
        job_id: String,
        source_device_id: String,
        store_id: String,
        session_id: String,
        request_hash: String,
        request: Box<ExecRequest>,
    },
    Cancel {
        job_id: String,
        source_device_id: String,
    },
    ReconcileJob {
        job_id: String,
    },
    ReadLogs {
        correlation_id: String,
        job_id: String,
        after: u64,
    },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRequest {
    pub token: String,
    pub name: Option<String>,
    pub csr_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairResponse {
    pub device_id: String,
    pub name: String,
    pub cert_pem: String,
    pub ca_pem: String,
    pub server_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewRequest {
    pub csr_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewResponse {
    pub cert_pem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: ErrorData,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
