use super::DeploymentProgress;
use std::{fs::OpenOptions, io::Write, path::Path};

pub(super) fn append(path: &Path, event: &DeploymentProgress) {
    let result = || -> anyhow::Result<()> {
        let directory = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("deployment log directory"))?;
        std::fs::create_dir_all(directory)?;
        crate::config::restrict_dir(directory)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(path)?;
        file.try_lock()?;
        if file.metadata()?.len() > 1024 * 1024 {
            file.set_len(0)?;
        }
        let error_code = event.details.error.as_ref().and_then(|error| match error {
            crate::protocol::Data::Error { code, .. } => Some(code.as_str()),
            _ => None,
        });
        let line = serde_json::json!({
            "event": event.event, "operation_id": event.operation_id, "provider": event.provider,
            "operation": event.operation, "sequence": event.sequence, "timestamp_ms": event.timestamp_ms, "stage": event.stage,
            "status": event.status, "elapsed_ms": event.elapsed_ms, "stage_elapsed_ms": event.stage_elapsed_ms,
            "error_code": error_code,
        });
        writeln!(file, "{line}")?;
        Ok(())
    };
    if let Err(error) = result() {
        tracing::debug!(%error, "deployment timing log unavailable");
    }
}
