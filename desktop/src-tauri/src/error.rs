//! Serializable command failures; the UI branches on codes, never on prose.
use serde::{Deserialize, Serialize};
use std::fmt;

/// A coded failure, also carried inside error chains. Its Display matches the
/// helper's `CODE: message`, so wrapped diagnostics read the same as before.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CommandError {
    pub code: String,
    pub message: String,
}
impl CommandError {
    /// Project an error as the helper does: the first code in the chain, otherwise
    /// STORAGE_ERROR for IO and EXECUTION_ERROR; the message omits a leading code.
    pub(crate) fn from_error(error: impl Into<anyhow::Error>) -> Self {
        let error = error.into();
        let code = error
            .downcast_ref::<Self>()
            .or_else(|| error.chain().find_map(|e| e.downcast_ref::<Self>()))
            .map(|e| e.code.clone())
            .unwrap_or_else(|| {
                if error.chain().any(|e| e.is::<std::io::Error>()) {
                    "STORAGE_ERROR".into()
                } else {
                    "EXECUTION_ERROR".into()
                }
            });
        let message = format!("{error:#}");
        let message = message
            .strip_prefix(&format!("{code}:"))
            .map(str::trim_start)
            .unwrap_or(&message)
            .to_string();
        Self { code, message }
    }
}
impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for CommandError {}

pub(crate) fn failure(code: &str, message: impl Into<String>) -> anyhow::Error {
    CommandError {
        code: code.into(),
        message: message.into(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_errors_keep_codes_when_messages_change() {
        let error =
            failure("SERVICE_START_FAILED", "renamed explanation").context("starting service");
        let serialized = serde_json::to_value(CommandError::from_error(error)).unwrap();
        assert_eq!(serialized["code"], "SERVICE_START_FAILED");
        assert!(
            serialized["message"]
                .as_str()
                .unwrap()
                .contains("renamed explanation")
        );
        // Uncoded failures are classified like the helper's.
        let io = anyhow::Error::from(std::io::Error::other("disk")).context("saving");
        assert_eq!(CommandError::from_error(io).code, "STORAGE_ERROR");
        let other = CommandError::from_error(anyhow::anyhow!("unexpected"));
        assert_eq!(
            (other.code.as_str(), other.message.as_str()),
            ("EXECUTION_ERROR", "unexpected")
        );
        let coded = CommandError::from_error(failure("HELPER_NOT_FOUND", "reinstall"));
        assert_eq!(coded.message, "reinstall");
    }
}
