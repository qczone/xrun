use crate::{config, error::ErrorCode};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn directory() -> Result<PathBuf> {
    let directory = config::device_dir()?.join("cloudflare");
    fs::create_dir_all(&directory)?;
    config::restrict_dir(&directory)?;
    Ok(directory)
}

pub(super) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 63
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        bail!(
            ErrorCode::InvalidName
                .error("Worker name must contain 1-63 lowercase letters, numbers or hyphens")
        )
    }
    Ok(())
}

pub(super) fn validate_account(account: &str) -> Result<()> {
    if account.len() != 32 || !account.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            ErrorCode::InvalidConfig
                .error("Cloudflare account ID must be 32 hexadecimal characters")
        )
    }
    Ok(())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Deployment {
    pub name: String,
    pub account_id: String,
    pub ownership: String,
    pub url: Option<String>,
    #[serde(default)]
    pub status: DeploymentStatus,
    #[serde(default)]
    pub region: super::Region,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DeploymentStatus {
    #[default]
    Active,
    Removing,
    Removed,
}

impl Deployment {
    pub(super) fn path(directory: &Path, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        Ok(directory.join(format!("deploy-{name}.toml")))
    }

    pub(super) fn load(directory: &Path, name: &str) -> Result<Option<Self>> {
        let path = Self::path(directory, name)?;
        let value: Self = match fs::read_to_string(&path) {
            Ok(source) => toml::from_str(&source).map_err(|_| {
                ErrorCode::InvalidConfig.error("invalid saved Cloudflare deployment record")
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        validate_account(&value.account_id)?;
        if value.name != name
            || value.ownership.len() != 26
            || !value
                .ownership
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'2'..=b'7'))
        {
            bail!(ErrorCode::InvalidConfig.error("invalid Cloudflare deployment record"))
        }
        Ok(Some(value))
    }

    pub(super) fn save(&self, directory: &Path) -> Result<()> {
        config::atomic_private_write(
            &Self::path(directory, &self.name)?,
            toml::to_string(self)?.as_bytes(),
        )
    }

    pub(super) fn validate_ownership(&self, worker: &serde_json::Value) -> Result<()> {
        let tag = format!("xrun-deployment-{}", self.ownership);
        if !worker["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|value| value == &tag))
        {
            bail!(
                ErrorCode::AlreadyExists
                    .error("the remote Worker no longer matches this deployment record")
            )
        }
        Ok(())
    }
}

pub(super) fn lock(directory: &Path, name: &str) -> Result<fs::File> {
    validate_name(name)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!("{name}.lock")))?;
    file.try_lock()
        .map_err(|_| ErrorCode::FileBusy.error("another Cloudflare deployment is running"))?;
    Ok(file)
}
