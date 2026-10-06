//! Stable machine-readable errors. Display text is for people, never dispatch.
use std::fmt;

macro_rules! codes {
    ($($variant:ident => $wire:literal,)*) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum ErrorCode {
            $($variant,)*
            /// Preserve newer peers' codes without assigning local semantics.
            Unknown(String),
        }
        impl ErrorCode {
            pub fn from_wire(code: String) -> Self {
                match code.as_str() {
                    $($wire => Self::$variant,)*
                    _ => Self::Unknown(code),
                }
            }
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)*
                    Self::Unknown(code) => code,
                }
            }
        }
    };
}
codes! {
    AccessPaused => "ACCESS_PAUSED",
    AlreadyExists => "ALREADY_EXISTS",
    CaptureError => "CAPTURE_ERROR",
    CaExpired => "CA_EXPIRED",
    CertificateExpired => "CERTIFICATE_EXPIRED",
    ChecksumMismatch => "CHECKSUM_MISMATCH",
    ConnectionClosed => "CONNECTION_CLOSED",
    ConnectionLimit => "CONNECTION_LIMIT",
    ConnectFailed => "CONNECT_FAILED",
    ConnectTimeout => "CONNECT_TIMEOUT",
    ControlTimeout => "CONTROL_TIMEOUT",
    DaemonChanged => "DAEMON_CHANGED",
    DaemonNotInitialized => "DAEMON_NOT_INITIALIZED",
    DaemonRunning => "DAEMON_RUNNING",
    DaemonStopping => "DAEMON_STOPPING",
    DaemonStopTimeout => "DAEMON_STOP_TIMEOUT",
    DaemonUpgradeRequired => "DAEMON_UPGRADE_REQUIRED",
    DbCorrupt => "DB_CORRUPT",
    DbMissing => "DB_MISSING",
    DbReset => "DB_RESET",
    DeviceBusy => "DEVICE_BUSY",
    DeviceMismatch => "DEVICE_MISMATCH",
    DeviceOffline => "DEVICE_OFFLINE",
    DeviceRevoked => "DEVICE_REVOKED",
    ExecutionError => "EXECUTION_ERROR",
    FileBusy => "FILE_BUSY",
    FileNotFound => "FILE_NOT_FOUND",
    FileTooLarge => "FILE_TOO_LARGE",
    ForwardConnectFailed => "FORWARD_CONNECT_FAILED",
    ForwardListenFailed => "FORWARD_LISTEN_FAILED",
    ForwardTimeout => "FORWARD_TIMEOUT",
    HttpError => "HTTP_ERROR",
    IdentityChanged => "IDENTITY_CHANGED",
    IdentityMismatch => "IDENTITY_MISMATCH",
    InputTooLarge => "INPUT_TOO_LARGE",
    InteractiveRequired => "INTERACTIVE_REQUIRED",
    InvalidAck => "INVALID_ACK",
    InvalidAddress => "INVALID_ADDRESS",
    InvalidBody => "INVALID_BODY",
    InvalidCertificate => "INVALID_CERTIFICATE",
    InvalidCommand => "INVALID_COMMAND",
    InvalidConfig => "INVALID_CONFIG",
    InvalidCsr => "INVALID_CSR",
    InvalidCursor => "INVALID_CURSOR",
    InvalidCwd => "INVALID_CWD",
    InvalidDeviceId => "INVALID_DEVICE_ID",
    InvalidExpect => "INVALID_EXPECT",
    InvalidFilter => "INVALID_FILTER",
    InvalidJobRef => "INVALID_JOB_REF",
    InvalidKey => "INVALID_KEY",
    InvalidLink => "INVALID_LINK",
    InvalidMessage => "INVALID_MESSAGE",
    InvalidName => "INVALID_NAME",
    InvalidPath => "INVALID_PATH",
    InvalidPort => "INVALID_PORT",
    InvalidReceipt => "INVALID_RECEIPT",
    InvalidRelay => "INVALID_RELAY",
    InvalidRelayMessage => "INVALID_RELAY_MESSAGE",
    InvalidRequest => "INVALID_REQUEST",
    InvalidRoster => "INVALID_ROSTER",
    InvalidScript => "INVALID_SCRIPT",
    InvalidScriptArgument => "INVALID_SCRIPT_ARGUMENT",
    InvalidService => "INVALID_SERVICE",
    InvalidSession => "INVALID_SESSION",
    InvalidShell => "INVALID_SHELL",
    InvalidSignature => "INVALID_SIGNATURE",
    InvalidToken => "INVALID_TOKEN",
    InvitationLimit => "INVITATION_LIMIT",
    IsDirectory => "IS_DIRECTORY",
    JobNotFound => "JOB_NOT_FOUND",
    ManagerBusy => "MANAGER_BUSY",
    ManagerOffline => "MANAGER_OFFLINE",
    ManagerProtected => "MANAGER_PROTECTED",
    ManagerStateExists => "MANAGER_STATE_EXISTS",
    ManagerStateMismatch => "MANAGER_STATE_MISMATCH",
    ManagerStateMissing => "MANAGER_STATE_MISSING",
    MembershipChanged => "MEMBERSHIP_CHANGED",
    MemberLimit => "MEMBER_LIMIT",
    MemberStateMissing => "MEMBER_STATE_MISSING",
    MessageTooLarge => "MESSAGE_TOO_LARGE",
    MigrationRequired => "MIGRATION_REQUIRED",
    NameInUse => "NAME_IN_USE",
    NameTaken => "NAME_TAKEN",
    NetworkMismatch => "NETWORK_MISMATCH",
    NotManager => "NOT_MANAGER",
    NoAddress => "NO_ADDRESS",
    NoDisplay => "NO_DISPLAY",
    PairingTimeout => "PAIRING_TIMEOUT",
    ParentNotFound => "PARENT_NOT_FOUND",
    PermissionDenied => "PERMISSION_DENIED",
    ProgramNotFound => "PROGRAM_NOT_FOUND",
    RelayChanged => "RELAY_CHANGED",
    RenewTimeout => "RENEW_TIMEOUT",
    RequestConflict => "REQUEST_CONFLICT",
    ResultLost => "RESULT_LOST",
    RosterConflict => "ROSTER_CONFLICT",
    RosterRollback => "ROSTER_ROLLBACK",
    ScreenshotFailed => "SCREENSHOT_FAILED",
    ScreenshotUnavailable => "SCREENSHOT_UNAVAILABLE",
    ScreenLocked => "SCREEN_LOCKED",
    ServerRunning => "SERVER_RUNNING",
    ServiceFailed => "SERVICE_FAILED",
    ServiceNotInstalled => "SERVICE_NOT_INSTALLED",
    ServiceUnavailable => "SERVICE_UNAVAILABLE",
    SessionClosed => "SESSION_CLOSED",
    SessionExpired => "SESSION_EXPIRED",
    SessionLimit => "SESSION_LIMIT",
    SessionRejected => "SESSION_REJECTED",
    SessionUnavailable => "SESSION_UNAVAILABLE",
    ShellRequired => "SHELL_REQUIRED",
    ShellUnsupported => "SHELL_UNSUPPORTED",
    SignatureError => "SIGNATURE_ERROR",
    SourceNotAllowed => "SOURCE_NOT_ALLOWED",
    Stale => "STALE",
    StorageError => "STORAGE_ERROR",
    StreamTimeout => "STREAM_TIMEOUT",
    TlsIdentityIncomplete => "TLS_IDENTITY_INCOMPLETE",
    Unauthenticated => "UNAUTHENTICATED",
    Unconfirmed => "UNCONFIRMED",
    UnknownDevice => "UNKNOWN_DEVICE",
    UnsupportedPlatform => "UNSUPPORTED_PLATFORM",
    VersionMismatch => "VERSION_MISMATCH",
    WaitTimeout => "WAIT_TIMEOUT",
}

impl ErrorCode {
    pub fn error(self, message: impl Into<String>) -> CodedError {
        CodedError {
            code: self,
            message: message.into(),
        }
    }

    pub fn is_explicit(&self) -> bool {
        matches!(
            self,
            Self::DeviceRevoked
                | Self::VersionMismatch
                | Self::SourceNotAllowed
                | Self::AccessPaused
                | Self::DeviceOffline
                | Self::SessionLimit
                | Self::NotManager
                | Self::MigrationRequired
                | Self::IdentityMismatch
                | Self::UnknownDevice
                | Self::InvalidToken
                | Self::InvalidName
                | Self::NameTaken
                | Self::MemberLimit
        )
    }

    pub fn is_network(&self) -> bool {
        matches!(
            self,
            Self::ConnectFailed
                | Self::ConnectTimeout
                | Self::ConnectionClosed
                | Self::ConnectionLimit
                | Self::SessionUnavailable
                | Self::SessionRejected
                | Self::HttpError
        )
    }

    /// A reply that proves the operation was rejected before acceptance.
    /// Transport failures and unknown peer codes must go through recovery.
    pub fn rejects_submission(&self) -> bool {
        matches!(
            self,
            Self::DeviceBusy
                | Self::DbReset
                | Self::RequestConflict
                | Self::SourceNotAllowed
                | Self::JobNotFound
                | Self::Stale
                | Self::AlreadyExists
                | Self::FileTooLarge
                | Self::FileNotFound
                | Self::FileBusy
                | Self::InvalidPath
                | Self::InvalidRequest
                | Self::InvalidCwd
                | Self::InvalidScript
                | Self::InvalidScriptArgument
                | Self::InvalidBody
                | Self::ChecksumMismatch
                | Self::ShellUnsupported
                | Self::IsDirectory
                | Self::ParentNotFound
                | Self::PermissionDenied
        )
    }
}

#[derive(Debug)]
pub struct CodedError {
    pub code: ErrorCode,
    pub message: String,
}
impl CodedError {
    pub fn from_wire(code: impl Into<String>, message: impl Into<String>) -> Self {
        ErrorCode::from_wire(code.into()).error(message)
    }
}
impl fmt::Display for CodedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}
impl std::error::Error for CodedError {}

/// Anyhow keeps typed identity through both string and typed context layers.
pub fn code(error: &anyhow::Error) -> Option<ErrorCode> {
    error
        .downcast_ref::<CodedError>()
        .or_else(|| error.chain().find_map(|e| e.downcast_ref::<CodedError>()))
        .map(|e| e.code.clone())
}
pub fn is(error: &anyhow::Error, expected: ErrorCode) -> bool {
    code(error).as_ref() == Some(&expected)
}

pub fn wire(error: &anyhow::Error) -> (String, String) {
    let code = code(error).unwrap_or_else(|| {
        match error
            .chain()
            .find_map(|e| e.downcast_ref::<std::io::Error>())
            .map(std::io::Error::kind)
        {
            Some(std::io::ErrorKind::NotFound) => ErrorCode::FileNotFound,
            Some(std::io::ErrorKind::PermissionDenied) => ErrorCode::PermissionDenied,
            Some(std::io::ErrorKind::WouldBlock) => ErrorCode::FileBusy,
            Some(std::io::ErrorKind::InvalidInput) => ErrorCode::InvalidPath,
            Some(_) => ErrorCode::StorageError,
            None => ErrorCode::ExecutionError,
        }
    });
    let message = format!("{error:#}");
    let message = message
        .strip_prefix(&format!("{}:", code.as_str()))
        .map(str::trim_start)
        .unwrap_or(&message)
        .to_string();
    (code.as_str().to_string(), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn context_and_display_changes_preserve_machine_identity() {
        let error: anyhow::Error = ErrorCode::InvalidRequest
            .error("renamed validation message")
            .into();
        let error = error
            .context("processing request")
            .context("peer operation failed");
        assert!(is(&error, ErrorCode::InvalidRequest));
        assert_eq!(wire(&error).0, "INVALID_REQUEST");
        assert!(wire(&error).1.contains("peer operation failed"));
        assert!(code(&error).unwrap().rejects_submission());

        let contextual = Err::<(), _>(std::io::Error::other("inner detail"))
            .context(ErrorCode::DeviceBusy.error("capacity reached"))
            .unwrap_err()
            .context("serving peer");
        assert!(is(&contextual, ErrorCode::DeviceBusy));
    }

    #[test]
    fn wire_codes_round_trip_and_unknown_codes_have_no_inferred_semantics() {
        let error: anyhow::Error =
            CodedError::from_wire("INVALID_REQUEST_FUTURE", "future rejection").into();
        let error = error.context("peer");
        assert_eq!(wire(&error).0, "INVALID_REQUEST_FUTURE");
        let code = code(&error).unwrap();
        assert!(!code.rejects_submission());
        assert!(!code.is_explicit());
        assert_eq!(ErrorCode::DbReset.as_str(), "DB_RESET");
        assert_eq!(ErrorCode::from_wire("DB_RESET".into()), ErrorCode::DbReset);
    }

    #[test]
    fn text_cannot_impersonate_a_code_and_io_context_retains_its_kind() {
        let error = anyhow::anyhow!("DB_RESET: misleading untyped display text");
        assert_eq!(code(&error), None);
        assert_eq!(wire(&error).0, "EXECUTION_ERROR");
        let error = Err::<(), _>(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing file",
        ))
        .context("loading input")
        .unwrap_err();
        assert_eq!(wire(&error).0, "FILE_NOT_FOUND");
    }
}
