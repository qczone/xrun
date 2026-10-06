//! Serializable command failures; the UI branches on codes, never on prose.
use serde::Serialize;
use std::fmt;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CommandError {
    pub code: String,
    pub message: String,
}
impl CommandError {
    pub(crate) fn from_error(error: impl Into<anyhow::Error>) -> Self {
        let (code, message) = xrun::error::wire(&error.into());
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
    xrun::error::CodedError::from_wire(code, message).into()
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
    }
}
