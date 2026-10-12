//! Remote captures are downloaded and verified before a desktop adapter publishes the local path.
use super::*;
use crate::protocol::{ScreenshotFormat, ScreenshotOptions};
use std::path::Path;

/// Exact downloaded image metadata and its verified originating job.
#[derive(Serialize)]
pub struct ScreenshotDownload {
    /// Published local image path.
    pub path: std::path::PathBuf,
    /// Source-scoped target job.
    pub job: Job,
    /// Actual encoded format.
    pub format: ScreenshotFormat,
    /// Actual MIME.
    pub mime: String,
    /// Actual width.
    pub width: u32,
    /// Actual height.
    pub height: u32,
    /// Exact payload bytes.
    pub size: u64,
    /// SHA-256 of encoded bytes.
    pub sha256: String,
    /// Target capture time in RFC 3339 format.
    pub captured_at: String,
    /// Bounded preview data URL; omitted for images above 8 MiB.
    pub preview: Option<String>,
}
/// Capture once with explicit options and save only after metadata and checksum verification.
pub async fn screenshot(
    target: &str,
    path: &Path,
    options: ScreenshotOptions,
) -> Result<ScreenshotDownload> {
    options.validate()?;
    let expected_format = options.format;
    let max_width = options.max_width;
    let id = identity().await?;
    let target = resolve(&id, target)?;
    let mut session = Session::open(&id, &target).await?;
    session
        .send_request(Request::Screenshot {
            context: JobContext::new(&session.db_id),
            options,
        })
        .await?;
    let job = session.accept_operation().await?;
    let Data::File {
        size,
        sha256,
        format: Some(format),
        mime: Some(mime),
        width: Some(width),
        height: Some(height),
        captured_at: Some(captured_at),
        ..
    } = response(&mut session).await?
    else {
        bail!(ErrorCode::InvalidMessage.error("expected complete screenshot metadata"));
    };
    if mime != format.mime() {
        bail!(ErrorCode::InvalidMessage.error("screenshot MIME differs from its format"));
    }
    if format != expected_format || max_width.is_some_and(|limit| width > limit) {
        bail!(
            ErrorCode::InvalidMessage
                .error("screenshot output differs from the requested format or width")
        );
    }
    let bytes = net::receive_bytes(&mut session.ws, size, &sha256, MAX_SCREENSHOT).await?;
    session.finish().await;
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        crate::screenshot::validate_received(&bytes, format, width, height)?;
        let preview = if bytes.len() <= 8 * 1024 * 1024 {
            use base64::{Engine, engine::general_purpose::STANDARD};
            Some(format!("data:{mime};base64,{}", STANDARD.encode(&bytes)))
        } else {
            None
        };
        crate::transfer::save_local(&path, &bytes)?;
        Ok(ScreenshotDownload {
            path,
            job,
            format,
            mime,
            width,
            height,
            size,
            sha256,
            captured_at,
            preview,
        })
    })
    .await?
}
