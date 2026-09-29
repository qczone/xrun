use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: String,
    pub public_url: String,
    pub data_dir: PathBuf,
}

impl ServerConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let value: Self = toml::from_str(
            &std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
        )?;
        let url = url::Url::parse(&value.public_url)?;
        if url.scheme() != "https" || url.host_str().is_none() || url.port().is_none() {
            bail!("public_url must be an https URL with an explicit port");
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub device_id: String,
    pub name: String,
    pub server_url: String,
    pub ca_pem: String,
    pub cert_pem: String,
    pub key_pem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default)]
    pub default_cwd: Option<PathBuf>,
    #[serde(default = "default_concurrency")]
    pub max_concurrent_jobs: usize,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn default_concurrency() -> usize {
    4
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            allow_from: Vec::new(),
            default_cwd: None,
            max_concurrent_jobs: 4,
            env: BTreeMap::new(),
        }
    }
}

pub fn home_dir() -> Result<PathBuf> {
    #[cfg(unix)]
    let value = std::env::var_os("HOME");
    #[cfg(windows)]
    let value = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    value
        .map(PathBuf::from)
        .context("HOME/USERPROFILE is unset")
}

pub fn device_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".xrun"))
}

impl Identity {
    pub fn load() -> Result<Self> {
        let path = device_dir()?.join("identity.toml");
        toml::from_str(
            &std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .context("parse identity")
    }

    pub fn save(&self) -> Result<()> {
        let dir = device_dir()?;
        std::fs::create_dir_all(&dir)?;
        restrict_dir(&dir)?;
        let path = dir.join("identity.toml");
        atomic_private_write(&path, toml::to_string(self)?.as_bytes())
    }
}

impl AgentConfig {
    pub fn load() -> Result<Self> {
        let path = device_dir()?.join("agent.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let value: Self =
            toml::from_str(&std::fs::read_to_string(&path)?).context("parse agent.toml")?;
        if !(1..=64).contains(&value.max_concurrent_jobs) {
            bail!("max_concurrent_jobs must be 1..64");
        }
        if value
            .default_cwd
            .as_ref()
            .is_some_and(|cwd| !cwd.is_absolute())
        {
            bail!("default_cwd must be absolute");
        }
        if value
            .env
            .iter()
            .any(|(k, v)| k.is_empty() || k.contains(['\0', '=']) || v.contains('\0'))
        {
            bail!("invalid Agent environment override");
        }
        #[cfg(windows)]
        {
            let mut names = std::collections::HashSet::new();
            if value
                .env
                .keys()
                .any(|name| !names.insert(name.to_ascii_lowercase()))
            {
                bail!("duplicate Windows Agent environment key");
            }
        }
        Ok(value)
    }
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
fn private_acl(path: &Path, directory: bool) -> Result<()> {
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
