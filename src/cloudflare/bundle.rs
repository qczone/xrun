use crate::{
    error::ErrorCode,
    protocol::{ProtocolRange, VERSION, sha256},
};
use anyhow::{Context, Result, bail};
use serde::Deserialize;

include!(concat!(env!("OUT_DIR"), "/cloudflare_bundle.rs"));

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub schema: u32,
    pub version: String,
    pub sha256: String,
    pub compatibility_date: String,
    pub protocol: ProtocolRange,
}

pub(super) struct Bundle {
    pub manifest: Manifest,
    pub source: String,
}

impl Bundle {
    pub(super) fn embedded() -> Result<Self> {
        let (manifest, source) = EMBEDDED.context(ErrorCode::InvalidConfig.error(
            "this development build has no Cloudflare relay; use the packaged xrun binary",
        ))?;
        Self::parse(manifest, source)
    }

    pub(super) fn parse(manifest: &str, source: &str) -> Result<Self> {
        let manifest: Manifest = serde_json::from_str(manifest)?;
        if manifest.schema != 1
            || manifest.version != VERSION
            || manifest.protocol != ProtocolRange::CURRENT
        {
            bail!(
                ErrorCode::VersionMismatch
                    .error("bundled Cloudflare relay does not match this xrun")
            )
        }
        if manifest.sha256 != sha256(source.as_bytes()) {
            bail!(ErrorCode::ChecksumMismatch.error("bundled Cloudflare relay checksum differs"))
        }
        Ok(Self {
            manifest,
            source: source.to_owned(),
        })
    }
}
