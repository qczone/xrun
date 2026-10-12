//! Explicit remembered login uses the OS store. There is no file or mock fallback.
use super::Credentials;
use crate::{error::ErrorCode, protocol::sha256};
use anyhow::{Result, bail};
use keyring::v1::{Entry, Error};
use std::path::Path;

pub(super) trait Vault {
    fn load(&self) -> Result<Option<Credentials>>;
    fn save(&self, credentials: &Credentials) -> Result<()>;
    fn delete(&self) -> Result<()>;
}
pub(super) fn decode(source: &str) -> Result<Credentials> {
    serde_json::from_str(source).map_err(|_| {
        ErrorCode::InvalidConfig
            .error("invalid Cloudflare login in system credential store; authorize again")
            .into()
    })
}
fn unavailable() -> anyhow::Error {
    ErrorCode::CredentialStoreUnavailable
        .error("system credential store unavailable; use temporary authorization instead")
        .into()
}
pub(super) struct SystemVault(Entry);
impl SystemVault {
    pub(super) fn open(directory: &Path, client: &str) -> Result<Self> {
        let key = sha256(format!("{}\0{client}", directory.canonicalize()?.display()).as_bytes());
        Ok(Self(
            Entry::new("xrun.cloudflare", &key).map_err(|_| unavailable())?,
        ))
    }
}
impl Vault for SystemVault {
    fn load(&self) -> Result<Option<Credentials>> {
        match self.0.get_password() {
            Ok(value) => Ok(Some(decode(&value)?)),
            Err(Error::NoEntry) => Ok(None),
            Err(_) => Err(unavailable()),
        }
    }
    fn save(&self, credentials: &Credentials) -> Result<()> {
        if credentials.refresh_token.is_none() {
            bail!(ErrorCode::InvalidToken.error("remembered login requires a refresh token"));
        }
        self.0
            .set_password(&serde_json::to_string(credentials)?)
            .map_err(|_| unavailable())
    }
    fn delete(&self) -> Result<()> {
        match self.0.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(_) => Err(unavailable()),
        }
    }
}
