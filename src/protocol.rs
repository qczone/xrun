use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_MESSAGE: usize = 1024 * 1024;
pub const MAX_INPUT: usize = 1024 * 1024;
pub const MAX_FILE: u64 = 64 * 1024 * 1024;
pub const FILE_CHUNK: usize = 64 * 1024;
/// Maximum unacknowledged ciphertext per direction on relays without drain().
pub const RELAY_WINDOW: usize = 64 * FILE_CHUNK;
pub const LOG_CHUNK: usize = 32 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub device_id: String,
    pub name: String,
    pub online: bool,
    pub admin: bool,
    pub revoked: bool,
    pub os: Option<String>,
    pub arch: Option<String>,
    #[serde(rename = "daemon_version")]
    pub version: Option<String>,
    pub hostname: Option<String>,
    pub execution_user: Option<String>,
    pub default_cwd: Option<String>,
    pub last_seen: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    pub inviter_id: Option<String>,
    pub allow_inviter: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRequest {
    pub version: String,
    pub token: String,
    pub name: String,
    pub csr_base64: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub request_id: String,
    pub db_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub timeout: u64,
    pub shell: Option<String>,
    pub input_size: u64,
    pub input_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamExecution {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub timeout: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamResult {
    pub exit_code: Option<i64>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
}
impl Execution {
    pub fn hash(&self) -> String {
        let mut value = serde_json::to_value(self).expect("serializable execution");
        value.as_object_mut().unwrap().remove("request_id");
        value.as_object_mut().unwrap().remove("db_id");
        sha256(&serde_json::to_vec(&value).unwrap())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Starting,
    Running,
    Exited,
    Failed,
    Canceled,
    TimedOut,
    Lost,
}
impl JobState {
    pub fn terminal(&self) -> bool {
        !matches!(self, Self::Starting | Self::Running)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub boot_id: String,
    pub start: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: String,
    pub request_id: String,
    pub request_hash: String,
    pub source_device_id: String,
    pub target_device_id: String,
    pub db_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub state: JobState,
    pub exit_code: Option<i64>,
    pub signal: Option<i32>,
    pub duration_ms: Option<u64>,
    pub last_seq: u64,
    pub output_complete: bool,
    pub incomplete_reason: Option<String>,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub leftover_possible: bool,
    pub process: Option<ProcessIdentity>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub seq: u64,
    pub stream: String,
    pub data_base64: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Exec {
        execution: Execution,
        #[serde(default)]
        follow: bool,
    },
    Jobs {
        id: Option<String>,
        running: bool,
        request_id: Option<String>,
        limit: usize,
        offset: usize,
    },
    Logs {
        id: String,
        after: u64,
        follow: bool,
    },
    Wait {
        id: String,
    },
    Kill {
        id: String,
    },
    Push {
        path: String,
        cwd: Option<String>,
        size: u64,
        sha256: String,
        mkdir: bool,
        no_overwrite: bool,
        expect: Option<String>,
    },
    Pull {
        path: String,
        cwd: Option<String>,
    },
    Screenshot,
    Forward {
        port: u16,
    },
    StreamExec {
        execution: StreamExecution,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Data {
    /// Checks a cached session before sending an operation. A roster change
    /// requires a new authenticated exchange.
    SessionProbe {
        roster_version: u64,
    },
    /// All response frames for this request have been sent.
    Complete,
    Ready {
        version: String,
        device_id: String,
        db_id: String,
        default_cwd: String,
    },
    Request {
        request: Request,
    },
    Job {
        job: Job,
    },
    Jobs {
        jobs: Vec<Job>,
    },
    Logs {
        events: Vec<LogEvent>,
        job: Job,
    },
    File {
        path: String,
        size: u64,
        sha256: String,
        width: Option<u32>,
        height: Option<u32>,
        captured_at: Option<String>,
    },
    End,
    ForwardReady {
        port: u16,
    },
    ForwardEofAck,
    StreamReady,
    StreamExit {
        result: StreamResult,
    },
    StreamExitAck,
    Error {
        code: String,
        message: String,
    },
}
impl Data {
    pub fn error(error: &anyhow::Error) -> Self {
        let message = format!("{error:#}");
        let candidate = message
            .split([':', ' '])
            .next()
            .unwrap_or("EXECUTION_ERROR")
            .to_string();
        let code = if candidate
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c == b'_')
        {
            candidate
        } else if let Some(error) = error
            .chain()
            .find_map(|e| e.downcast_ref::<std::io::Error>())
        {
            match error.kind() {
                std::io::ErrorKind::NotFound => "FILE_NOT_FOUND",
                std::io::ErrorKind::PermissionDenied => "PERMISSION_DENIED",
                std::io::ErrorKind::WouldBlock => "FILE_BUSY",
                std::io::ErrorKind::InvalidInput => "INVALID_PATH",
                _ => "STORAGE_ERROR",
            }
            .to_string()
        } else {
            "EXECUTION_ERROR".to_string()
        };
        let message = message
            .strip_prefix(&format!("{code}:"))
            .map(str::trim_start)
            .unwrap_or(&message)
            .to_string();
        Self::Error { code, message }
    }
}
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
pub const RESERVED: &[&str] = &[
    "up",
    "join",
    "invite",
    "allow-from",
    "deny-from",
    "revoke",
    "status",
    "recent",
    "down",
    "server",
    "relay",
    "daemon",
    "guide",
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
pub fn valid_name(name: &str) -> bool {
    !RESERVED.contains(&name)
        && (1..=32).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
