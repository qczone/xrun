//! Local daemon state and generation-bound graceful shutdown through private IPC.
use crate::config;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub generation: String,
    pub connected: bool,
}
pub struct Control {
    dir: PathBuf,
    value: Mutex<State>,
    stop: tokio::sync::watch::Sender<bool>,
}
impl Control {
    pub fn new(dir: &Path) -> Result<Self> {
        let value = State {
            generation: uuid::Uuid::new_v4().to_string(),
            connected: false,
        };
        config::atomic_private_write(
            &dir.join("daemon-runtime.json"),
            &serde_json::to_vec(&value)?,
        )?;
        Ok(Self {
            dir: dir.into(),
            value: Mutex::new(value),
            stop: tokio::sync::watch::channel(false).0,
        })
    }
    pub fn connected(&self, connected: bool) -> Result<()> {
        let mut value = self.value.lock().unwrap();
        value.connected = connected;
        config::atomic_private_write(
            &self.dir.join("daemon-runtime.json"),
            &serde_json::to_vec(&*value)?,
        )
    }
    pub(crate) fn check_generation(&self, generation: &str) -> Result<()> {
        if self.value.lock().unwrap().generation != generation {
            bail!("DAEMON_CHANGED: stop request belongs to an earlier daemon");
        }
        Ok(())
    }
    pub(crate) fn request_stop(&self, generation: &str) -> Result<()> {
        self.check_generation(generation)?;
        self.stop.send_replace(true);
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        let mut stop = self.stop.subscribe();
        stop.wait_for(|value| *value).await?;
        Ok(())
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        if state(&self.dir)
            .ok()
            .flatten()
            .is_some_and(|s| s.generation == self.value.lock().unwrap().generation)
        {
            let _ = std::fs::remove_file(self.dir.join("daemon-runtime.json"));
            let _ = std::fs::remove_file(self.dir.join("daemon-stop"));
        }
    }
}
pub fn state(dir: &Path) -> Result<Option<State>> {
    match std::fs::read(dir.join("daemon-runtime.json")) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub async fn request_shutdown(dir: &Path) -> Result<()> {
    let Some(state) = state(dir)? else {
        bail!(
            "DAEMON_UPGRADE_REQUIRED: restart the daemon with the current xrun before stopping it from the app"
        )
    };
    if dir.join("daemon-ipc.json").exists() {
        crate::ipc::stop(dir, &state.generation).await
    } else {
        // Also allows the app to stop an installed daemon from before IPC.
        config::atomic_private_write(&dir.join("daemon-stop"), state.generation.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[tokio::test]
    async fn stale_stop_does_not_stop_a_new_generation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let old = Control::new(dir.path())?;
        let old_generation = state(dir.path())?.unwrap().generation;
        let current = Control::new(dir.path())?;
        drop(old);
        assert!(current.request_stop(&old_generation).is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(150), current.shutdown())
                .await
                .is_err()
        );
        assert!(state(dir.path())?.is_some());
        current.request_stop(&state(dir.path())?.unwrap().generation)?;
        tokio::time::timeout(Duration::from_secs(1), current.shutdown()).await??;
        drop(current);
        assert!(state(dir.path())?.is_none());
        Ok(())
    }
}
