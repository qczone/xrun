//! Explicit screenshot intent; output metadata describes the encoded bytes actually sent.
use crate::error::ErrorCode;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
/// The requested encoded screenshot format.
pub enum ScreenshotFormat {
    #[default]
    /// Lossless PNG; the default preserves original capture bytes.
    Png,
    /// JPEG with optional lossy quality.
    Jpeg,
    /// WebP with optional lossy quality.
    Webp,
}
impl ScreenshotFormat {
    /// MIME type of this encoded format.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }
    /// Suggested filename suffix.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Immutable requested dimensions and encoding policy.
pub struct ScreenshotOptions {
    /// Maximum width; none keeps the original size. Never upscales.
    pub max_width: Option<u32>,
    /// Encoded output format.
    pub format: ScreenshotFormat,
    /// JPEG/WebP quality from 1 to 100; none uses 80. PNG requires none.
    pub quality: Option<u8>,
}
impl ScreenshotOptions {
    /// Validate before accepting a job or allocating decoded image buffers.
    pub fn validate(&self) -> Result<()> {
        if self
            .max_width
            .is_some_and(|width| !(1..=16384).contains(&width))
            || self
                .quality
                .is_some_and(|quality| !(1..=100).contains(&quality))
            || (self.format == ScreenshotFormat::Png && self.quality.is_some())
        {
            bail!(
                ErrorCode::InvalidRequest
                    .error("screenshot width must be 1..16384; JPEG/WebP quality must be 1..100")
            );
        }
        Ok(())
    }
}
