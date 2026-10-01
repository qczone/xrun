//! Local daemon state and a generation-bound graceful stop request. Both files
//! live in the private device directory; no network listener is needed.
use crate::config;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
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
    pub async fn shutdown(&self) -> Result<()> {
        let generation = self.value.lock().unwrap().generation.clone();
        loop {
            match std::fs::read(self.dir.join("daemon-stop")) {
                Ok(bytes) if bytes == generation.as_bytes() => return Ok(()),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
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
pub fn request_shutdown(dir: &Path) -> Result<()> {
    let Some(state) = state(dir)? else {
        bail!(
            "DAEMON_UPGRADE_REQUIRED: restart the daemon with the current xrun before stopping it from the app"
        )
    };
    config::atomic_private_write(&dir.join("daemon-stop"), state.generation.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stale_stop_does_not_stop_a_new_generation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let old = Control::new(dir.path())?;
        request_shutdown(dir.path())?;
        let current = Control::new(dir.path())?;
        drop(old);
        assert!(
            tokio::time::timeout(Duration::from_millis(150), current.shutdown())
                .await
                .is_err()
        );
        assert!(state(dir.path())?.is_some());
        request_shutdown(dir.path())?;
        tokio::time::timeout(Duration::from_secs(1), current.shutdown()).await??;
        drop(current);
        assert!(state(dir.path())?.is_none());
        Ok(())
    }
}
