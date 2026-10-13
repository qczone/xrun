use crate::error::ErrorCode;
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
        read(&network_dir()?.join("identity.toml"))
    }
    pub fn save(&self) -> Result<()> {
        write(&network_dir()?.join("identity.toml"), self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct PendingIdentity {
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
    #[serde(default = "attachment_retention")]
    pub attachment_retention_days: u16,
    #[serde(default = "membership_lease_minutes")]
    pub membership_lease_minutes: u16,
    #[serde(default)]
    pub traffic_compression: crate::protocol::Compression,
    #[serde(default)]
    pub lan_address: Option<std::net::Ipv4Addr>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}
fn concurrency() -> usize {
    4
}
fn attachment_retention() -> u16 {
    30
}
fn membership_lease_minutes() -> u16 {
    15
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
            attachment_retention_days: attachment_retention(),
            membership_lease_minutes: membership_lease_minutes(),
            traffic_compression: Default::default(),
            lan_address: None,
            env: BTreeMap::new(),
        }
    }
}
impl DaemonConfig {
    pub fn check_access(&self, source: &str) -> Result<()> {
        if self.remote_access_paused {
            bail!(ErrorCode::AccessPaused.error("remote access is paused on this device"))
        }
        if self.deny_from.iter().any(|id| id == source)
            || !(self.allow_all || self.allow_from.iter().any(|id| id == source))
        {
            bail!(ErrorCode::SourceNotAllowed.error("source device is not allowed"))
        }
        Ok(())
    }
    pub fn check_manager_access(&self, source: &str) -> Result<()> {
        self.check_access(source)?;
        if !self.allow_from.iter().any(|id| id == source) {
            bail!(
                ErrorCode::SourceNotAllowed.error(
                    "manager devices require an individual allow-from grant for this source"
                )
            );
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
                .context(ErrorCode::InvalidConfig.error("pause generation exhausted"))?;
        }
        self.remote_access_paused = paused;
        Ok(())
    }
    pub fn load() -> Result<Self> {
        Self::load_at(&device_dir()?)
    }
    pub(crate) fn load_at(dir: &Path) -> Result<Self> {
        let path = network_dir_at(dir)?.join("daemon.toml");
        let value: Self = if path.exists() {
            read(&path)?
        } else {
            Self::default()
        };
        value.validate()?;
        Ok(value)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=1440).contains(&self.membership_lease_minutes) {
            bail!(ErrorCode::InvalidConfig.error("membership_lease_minutes must be 1..1440"));
        }
        if self.attachment_retention_days > 3650 {
            bail!(ErrorCode::InvalidConfig.error(
                "attachment_retention_days must be 0..3650 (0 keeps attachments indefinitely)"
            ))
        }
        if !(1..=64).contains(&self.max_concurrent_jobs) {
            bail!(ErrorCode::InvalidConfig.error("max_concurrent_jobs must be 1..64"))
        }
        if self.default_cwd.as_ref().is_some_and(|p| !p.is_absolute()) {
            bail!(ErrorCode::InvalidConfig.error("default_cwd must be absolute"))
        }
        if self
            .env
            .iter()
            .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
        {
            bail!(ErrorCode::InvalidConfig.error("invalid environment"))
        }
        #[cfg(windows)]
        {
            let mut keys = std::collections::HashSet::new();
            if self
                .env
                .keys()
                .any(|k| !keys.insert(k.to_ascii_lowercase()))
            {
                bail!(ErrorCode::InvalidConfig.error("duplicate environment key"))
            }
        }
        Ok(())
    }
    pub fn save(&self) -> Result<()> {
        self.validate()?;
        write(&network_dir()?.join("daemon.toml"), self)
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
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkActivation {
    pub generation: String,
}
pub(crate) fn network_dir() -> Result<PathBuf> {
    network_dir_at(&device_dir()?)
}
pub(crate) fn network_dir_at(dir: &Path) -> Result<PathBuf> {
    let pointer = dir.join("network-state.json");
    if !pointer.exists() {
        return Ok(dir.to_owned());
    }
    let value: NetworkActivation = serde_json::from_slice(&std::fs::read(pointer)?)
        .map_err(|_| ErrorCode::InvalidConfig.error("invalid network activation record"))?;
    if value.generation.len() != 32
        || !value
            .generation
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        bail!(ErrorCode::InvalidConfig.error("invalid network activation generation"));
    }
    let active = dir.join("network-versions").join(value.generation);
    if !active.is_dir() || active.is_symlink() {
        bail!(ErrorCode::ManagerStateMissing.error("active network state is missing"));
    }
    Ok(active)
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

pub(crate) fn update_all_permissions(allow: bool) -> Result<()> {
    Identity::load()?;
    update_daemon_config(&device_dir()?, |cfg| {
        cfg.allow_all = allow;
        Ok(())
    })
}

pub(crate) fn pause_remote_access(paused: bool) -> Result<()> {
    Identity::load()?;
    update_daemon_config(&device_dir()?, |cfg| cfg.set_paused(paused))
}

pub(crate) fn update_execution(
    default_cwd: Option<PathBuf>,
    max_concurrent_jobs: usize,
    path: Option<String>,
) -> Result<()> {
    if let Some(cwd) = &default_cwd
        && !cwd.is_dir()
    {
        bail!(ErrorCode::InvalidCwd.error("working directory must exist"))
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
    let path = network_dir_at(dir)?.join("daemon.toml");
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
pub(crate) fn sync_parent(_path: &Path) -> Result<()> {
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
    #[cfg(windows)]
    private_acl(file.path(), false).context("restrict temporary private file")?;
    std::io::Write::write_all(&mut file, bytes)?;
    file.as_file().sync_all()?;
    // Close our write handle and secure the temporary file before publishing.
    // No later ACL update should reopen a path that another writer can replace.
    persist_private(file.into_temp_path(), path).context("publish private file")?;
    sync_parent(path)?;
    Ok(())
}

#[cfg(not(windows))]
fn persist_private(file: tempfile::TempPath, path: &Path) -> Result<()> {
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(windows)]
fn persist_private(mut file: tempfile::TempPath, path: &Path) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match file.persist(path) {
            Ok(()) => return Ok(()),
            Err(error)
                if matches!(error.error.raw_os_error(), Some(5 | 32 | 33))
                    && std::time::Instant::now() < deadline =>
            {
                // Readers and virus scanners can briefly deny rename/delete.
                // Retry the same atomic replacement without touching old bytes.
                file = error.path;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error.error.into()),
        }
    }
}

pub(crate) fn restrict_dir(_path: &Path) -> Result<()> {
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

    #[cfg(windows)]
    #[test]
    fn private_write_waits_for_a_transient_windows_reader() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.json");
        atomic_private_write(&path, b"old")?;
        let reader = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)?;
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(reader);
        });
        atomic_private_write(&path, b"new")?;
        release.join().unwrap();
        assert_eq!(std::fs::read(&path)?, b"new");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn private_write_preserves_old_bytes_when_windows_sharing_remains_locked() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.json");
        atomic_private_write(&path, b"old")?;
        let _reader = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)?;
        let error = atomic_private_write(&path, b"new").unwrap_err();
        assert!(matches!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(5 | 32 | 33)
        ));
        assert_eq!(std::fs::read(&path)?, b"old");
        assert_eq!(std::fs::read_dir(dir.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn permissions_are_explicit_and_pause_survives_reload() -> Result<()> {
        let mut cfg: DaemonConfig = toml::from_str("allow_from = ['known']")?;
        assert!(cfg.check_access("known").is_ok());
        assert!(cfg.check_access("future").is_err());
        cfg.allow_all = true;
        assert!(cfg.check_access("future").is_ok());
        assert!(cfg.check_manager_access("future").is_err());
        assert!(cfg.check_manager_access("known").is_ok());
        cfg.set_permission("future", false);
        assert!(cfg.check_access("future").is_err());
        cfg.allow_all = false;
        assert!(cfg.check_access("known").is_ok());
        cfg.set_permission("future", true);
        assert!(cfg.check_access("future").is_ok());
        assert!(cfg.check_manager_access("future").is_ok());
        cfg.set_paused(true)?;
        let generation = cfg.pause_generation;
        let reloaded: DaemonConfig = toml::from_str(&toml::to_string(&cfg)?)?;
        assert!(reloaded.remote_access_paused);
        assert!(crate::error::is(
            &reloaded.check_access("known").unwrap_err(),
            ErrorCode::AccessPaused
        ));
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
