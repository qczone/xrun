use crate::protocol::Registration;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub port: u16,
    pub addresses: Vec<String>,
    pub manual: bool,
    pub no_detect: bool,
    pub data_dir: PathBuf,
}
impl ServerConfig {
    pub fn load() -> Result<Self> {
        read(&device_dir()?.join("config.toml"))
    }
    pub fn save(&self) -> Result<()> {
        write(&device_dir()?.join("config.toml"), self)
    }
    pub fn urls(&self) -> Vec<String> {
        self.addresses
            .iter()
            .map(|a| format!("https://{a}"))
            .collect()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub device_id: String,
    pub name: String,
    pub addresses: Vec<String>,
    pub ca_pem: String,
    pub cert_pem: String,
    pub key_pem: String,
    pub registration: Registration,
    #[serde(default)]
    pub network: Option<NetworkIdentity>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkIdentity {
    pub network_id: String,
    pub manager_id: String,
}
impl Identity {
    pub fn load() -> Result<Self> {
        read(&device_dir()?.join("identity.toml"))
    }
    pub fn save(&self) -> Result<()> {
        write(&device_dir()?.join("identity.toml"), self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PendingIdentity {
    pub key_pem: String,
    pub ca_pem: String,
    pub pin: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default)]
    pub deny_from: Vec<String>,
    #[serde(default)]
    pub allow_all: bool,
    #[serde(default)]
    pub remote_access_paused: bool,
    // A pause invalidates existing sessions even if access is resumed before
    // the daemon next polls the file.
    #[serde(default)]
    pub pause_generation: u64,
    #[serde(default)]
    pub default_cwd: Option<PathBuf>,
    #[serde(default = "concurrency")]
    pub max_concurrent_jobs: usize,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}
fn concurrency() -> usize {
    4
}
impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            allow_from: vec![],
            deny_from: vec![],
            allow_all: false,
            remote_access_paused: false,
            pause_generation: 0,
            default_cwd: None,
            max_concurrent_jobs: 4,
            env: BTreeMap::new(),
        }
    }
}
impl DaemonConfig {
    pub fn check_access(&self, source: &str) -> Result<()> {
        if self.remote_access_paused {
            bail!("ACCESS_PAUSED: remote access is paused on this device")
        }
        if self.deny_from.iter().any(|id| id == source)
            || !(self.allow_all || self.allow_from.iter().any(|id| id == source))
        {
            bail!("SOURCE_NOT_ALLOWED: source device is not allowed")
        }
        Ok(())
    }
    fn set_permission(&mut self, device_id: &str, allow: bool) {
        self.allow_from.retain(|id| id != device_id);
        self.deny_from.retain(|id| id != device_id);
        if allow {
            self.allow_from.push(device_id.to_string());
        } else {
            self.deny_from.push(device_id.to_string());
        }
    }
    fn set_paused(&mut self, paused: bool) -> Result<()> {
        if paused && !self.remote_access_paused {
            self.pause_generation = self
                .pause_generation
                .checked_add(1)
                .context("INVALID_CONFIG: pause generation exhausted")?;
        }
        self.remote_access_paused = paused;
        Ok(())
    }
    pub fn load() -> Result<Self> {
        let path = device_dir()?.join("daemon.toml");
        let value: Self = if path.exists() {
            read(&path)?
        } else {
            Self::default()
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<()> {
        if !(1..=64).contains(&self.max_concurrent_jobs) {
            bail!("INVALID_CONFIG: max_concurrent_jobs must be 1..64")
        }
        if self.default_cwd.as_ref().is_some_and(|p| !p.is_absolute()) {
            bail!("INVALID_CONFIG: default_cwd must be absolute")
        }
        if self
            .env
            .iter()
            .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
        {
            bail!("INVALID_CONFIG: invalid environment")
        }
        #[cfg(windows)]
        {
            let mut keys = std::collections::HashSet::new();
            if self
                .env
                .keys()
                .any(|k| !keys.insert(k.to_ascii_lowercase()))
            {
                bail!("INVALID_CONFIG: duplicate environment key")
            }
        }
        Ok(())
    }
    pub fn save(&self) -> Result<()> {
        self.validate()?;
        write(&device_dir()?.join("daemon.toml"), self)
    }
}
pub fn home_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    let value = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let value = std::env::var_os("HOME");
    value
        .map(PathBuf::from)
        .context("HOME/USERPROFILE is unset")
}
pub fn device_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".xrun"))
}
pub fn instance_running(path: &Path) -> Result<bool> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}
pub fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    toml::from_str(
        &std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )
    .context("parse config")
}
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if path.parent() == Some(device_dir()?.as_path()) {
        std::fs::create_dir_all(path.parent().unwrap())?;
        restrict_dir(path.parent().unwrap())?;
    }
    atomic_private_write(path, toml::to_string(value)?.as_bytes())
}

pub fn update_permission(device_id: &str, allow: bool) -> Result<()> {
    update_daemon_config(&device_dir()?, |cfg| {
        cfg.set_permission(device_id, allow);
        Ok(())
    })
}

pub fn update_all_permissions(allow: bool) -> Result<()> {
    Identity::load()?;
    update_daemon_config(&device_dir()?, |cfg| {
        cfg.allow_all = allow;
        Ok(())
    })
}

pub fn pause_remote_access(paused: bool) -> Result<()> {
    Identity::load()?;
    update_daemon_config(&device_dir()?, |cfg| cfg.set_paused(paused))
}

pub fn update_execution(
    default_cwd: Option<PathBuf>,
    max_concurrent_jobs: usize,
    path: Option<String>,
) -> Result<()> {
    if let Some(cwd) = &default_cwd
        && !cwd.is_dir()
    {
        bail!("INVALID_CWD: working directory must exist")
    }
    update_daemon_config(&device_dir()?, |cfg| {
        cfg.default_cwd = default_cwd;
        cfg.max_concurrent_jobs = max_concurrent_jobs;
        cfg.env.remove("PATH");
        #[cfg(windows)]
        cfg.env.retain(|key, _| !key.eq_ignore_ascii_case("PATH"));
        if let Some(path) = path {
            cfg.env.insert("PATH".into(), path);
        }
        Ok(())
    })
}

pub(crate) fn update_daemon_config(
    dir: &Path,
    update: impl FnOnce(&mut DaemonConfig) -> Result<()>,
) -> Result<()> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("permissions.lock"))?;
    lock.lock()?;
    let path = dir.join("daemon.toml");
    let mut cfg = if path.exists() {
        read::<DaemonConfig>(&path)?
    } else {
        DaemonConfig::default()
    };
    cfg.validate()?;
    update(&mut cfg)?;
    cfg.validate()?;
    write(&path, &cfg)
}
pub fn sync_parent(_path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(_path.parent().context("missing parent")?)?.sync_all()?;
    Ok(())
}
pub fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    std::io::Write::write_all(&mut file, bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    sync_parent(path)?;
    #[cfg(windows)]
    private_acl(path, false)?;
    Ok(())
}

pub fn restrict_dir(_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    private_acl(_path, true)?;
    Ok(())
}

#[cfg(windows)]
pub(crate) fn private_acl(path: &Path, directory: bool) -> Result<()> {
    use std::{
        os::windows::ffi::OsStrExt,
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            ACL,
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                SE_FILE_OBJECT, SetNamedSecurityInfoW,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
    };
    let sddl = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    };
    let sddl_w: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_w.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut present = 0;
    let mut acl: *mut ACL = null_mut();
    let mut defaulted = 0;
    let got_acl =
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) };
    let path_w: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let status = if got_acl != 0 && present != 0 {
        unsafe {
            SetNamedSecurityInfoW(
                path_w.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null(),
            )
        }
    } else {
        1
    };
    unsafe {
        LocalFree(descriptor);
    }
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_are_explicit_and_pause_survives_reload() -> Result<()> {
        let mut cfg: DaemonConfig = toml::from_str("allow_from = ['known']")?;
        assert!(cfg.check_access("known").is_ok());
        assert!(cfg.check_access("future").is_err());
        cfg.allow_all = true;
        assert!(cfg.check_access("future").is_ok());
        cfg.set_permission("future", false);
        assert!(cfg.check_access("future").is_err());
        cfg.allow_all = false;
        assert!(cfg.check_access("known").is_ok());
        cfg.set_permission("future", true);
        assert!(cfg.check_access("future").is_ok());
        cfg.set_paused(true)?;
        let generation = cfg.pause_generation;
        let reloaded: DaemonConfig = toml::from_str(&toml::to_string(&cfg)?)?;
        assert!(reloaded.remote_access_paused);
        assert!(
            reloaded
                .check_access("known")
                .unwrap_err()
                .to_string()
                .starts_with("ACCESS_PAUSED")
        );
        cfg.set_paused(true)?;
        assert_eq!(cfg.pause_generation, generation);
        cfg.set_paused(false)?;
        assert!(cfg.check_access("known").is_ok());
        cfg.set_paused(true)?;
        assert!(cfg.pause_generation > generation);
        Ok(())
    }

    #[test]
    fn settings_and_permissions_do_not_overwrite_each_other() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let cfg = DaemonConfig {
            allow_from: vec!["source-a".into()],
            env: BTreeMap::from([("OTHER_SETTING".into(), "preserve-me".into())]),
            ..DaemonConfig::default()
        };
        write(&dir.path().join("daemon.toml"), &cfg)?;
        std::thread::scope(|scope| -> Result<()> {
            let permission = scope.spawn(|| {
                update_daemon_config(dir.path(), |cfg| {
                    cfg.allow_from.push("source-b".into());
                    Ok(())
                })
            });
            let settings = scope.spawn(|| {
                update_daemon_config(dir.path(), |cfg| {
                    cfg.max_concurrent_jobs = 8;
                    Ok(())
                })
            });
            permission.join().unwrap()?;
            settings.join().unwrap()?;
            Ok(())
        })?;
        let cfg: DaemonConfig = read(&dir.path().join("daemon.toml"))?;
        assert_eq!(cfg.allow_from, ["source-a", "source-b"]);
        assert_eq!(cfg.max_concurrent_jobs, 8);
        assert_eq!(cfg.env["OTHER_SETTING"], "preserve-me");
        let failed = update_daemon_config(dir.path(), |cfg| {
            cfg.max_concurrent_jobs = 0;
            Ok(())
        });
        assert!(failed.is_err());
        assert_eq!(
            read::<DaemonConfig>(&dir.path().join("daemon.toml"))?.max_concurrent_jobs,
            8
        );
        Ok(())
    }
}
