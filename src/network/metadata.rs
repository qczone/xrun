//! Last observed endpoint metadata, separate from signed membership and access policy.
use crate::{config, database, protocol::Device};
use anyhow::Result;
use rusqlite::{Connection, params};
use std::{collections::HashMap, path::Path};

fn open(path: &Path) -> Result<Connection> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing cache directory"))?;
    std::fs::create_dir_all(parent)?;
    config::restrict_dir(parent)?;
    let mut db = Connection::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    database::configure(&db)?;
    database::initialize(
        &mut db,
        "device metadata",
        1,
        "CREATE TABLE devices(network TEXT NOT NULL,device TEXT NOT NULL,data TEXT NOT NULL,
         PRIMARY KEY(network,device));",
    )?;
    Ok(db)
}

fn read(path: &Path, network: &str) -> Result<HashMap<String, Device>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let db = open(path)?;
    let mut query = db.prepare("SELECT device,data FROM devices WHERE network=?1")?;
    let rows = query.query_map([network], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (device, data) = row?;
        Ok((device, serde_json::from_str(&data)?))
    })
    .collect()
}

fn write(path: &Path, network: &str, device: &Device) -> Result<()> {
    open(path)?.execute(
        "INSERT INTO devices VALUES(?1,?2,?3)
         ON CONFLICT(network,device) DO UPDATE SET data=excluded.data",
        params![network, device.device_id, serde_json::to_string(device)?],
    )?;
    Ok(())
}

pub(super) fn load(network: &str) -> HashMap<String, Device> {
    let result = config::device_dir().and_then(|dir| read(&dir.join("device-info.db"), network));
    result.unwrap_or_else(|error| {
        tracing::warn!(%error, "device metadata cache unavailable");
        HashMap::new()
    })
}

pub(super) fn observe(network: &str, device: &Device) {
    let result =
        config::device_dir().and_then(|dir| write(&dir.join("device-info.db"), network, device));
    if let Err(error) = result {
        tracing::warn!(%error, "device metadata was not cached");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_metadata_survives_reopening_and_is_scoped_to_its_network() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("device-info.db");
        let mut device = Device {
            device_id: "9db79d9bfad74bfe9c2fc01bc6fc79e1".into(),
            name: "pc1".into(),
            online: true,
            admin: false,
            revoked: false,
            os: Some("windows".into()),
            arch: Some("aarch64".into()),
            version: Some("0.1.0-rc.6".into()),
            hostname: None,
            execution_user: None,
            default_cwd: None,
            last_seen: Some(1000),
            connection: None,
        };
        assert!(read(&path, "network1")?.is_empty());
        write(&path, "network1", &device)?;
        assert_eq!(read(&path, "network1")?[&device.device_id].os, device.os);
        assert!(read(&path, "network2")?.is_empty());
        device.version = Some("0.1.0".into());
        device.last_seen = Some(2000);
        write(&path, "network1", &device)?;
        let cached = read(&path, "network1")?;
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[&device.device_id].version, device.version);
        assert_eq!(cached[&device.device_id].last_seen, device.last_seen);
        Ok(())
    }
}
