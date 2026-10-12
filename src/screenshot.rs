use crate::error::ErrorCode;
use crate::protocol::{MAX_SCREENSHOT, ScreenshotFormat, ScreenshotOptions};
use anyhow::{Context, Result, bail};
use tokio::sync::OwnedSemaphorePermit;
pub struct Capture {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub at: String,
    pub format: ScreenshotFormat,
}
pub async fn capture() -> Result<Capture> {
    decode(platform(None).await?.0)
}
pub(crate) async fn capture_with_permit(
    permit: OwnedSemaphorePermit,
    options: ScreenshotOptions,
) -> Result<(Capture, OwnedSemaphorePermit)> {
    options.validate()?;
    let (bytes, permit) = platform(Some(permit)).await?;
    tokio::task::spawn_blocking(move || {
        let permit = permit.expect("capture retains admission");
        Ok((encode(decode(bytes)?, &options)?, permit))
    })
    .await?
}

// Keep the output path and admission alive until the external writer has exited.
// Dropping a Tokio Child alone does not kill it; deleting its PNG first lets it
// recreate an orphan file after a canceled request has already released admission.
#[cfg(any(target_os = "macos", windows))]
struct CaptureProcess {
    child: Option<tokio::process::Child>,
    path: Option<tempfile::TempPath>,
    permit: Option<OwnedSemaphorePermit>,
}
#[cfg(any(target_os = "macos", windows))]
impl CaptureProcess {
    async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        let status = self.child.as_mut().unwrap().wait().await?;
        self.child.take();
        Ok(status)
    }
}
#[cfg(any(target_os = "macos", windows))]
impl Drop for CaptureProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let path = self.path.take();
            let permit = self.permit.take();
            let _ = child.start_kill();
            // The runtime still drives canceled session cleanup. kill_on_drop is
            // also set on the child for shutdown before this cleanup can be polled.
            tokio::spawn(async move {
                let _ = child.wait().await;
                drop(path);
                drop(permit);
            });
        }
    }
}
fn decode(bytes: Vec<u8>) -> Result<Capture> {
    if bytes.len() as u64 > MAX_SCREENSHOT {
        bail!(ErrorCode::FileTooLarge.error("screenshot exceeds 64 MiB"))
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
    let reader = decoder
        .read_info()
        .context(ErrorCode::ScreenshotFailed.error("invalid PNG"))?;
    let width = reader.info().width;
    let height = reader.info().height;
    dimensions(width, height)?;
    Ok(Capture {
        bytes,
        width,
        height,
        format: ScreenshotFormat::Png,
        at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?,
    })
}
const MAX_PIXELS: u64 = 32_000_000;
fn dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        bail!(
            ErrorCode::ScreenshotFailed
                .error("screenshot dimensions exceed the 32 megapixel resource budget")
        );
    }
    Ok(())
}
fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_PIXELS * 4);
    limits
}
fn encode(capture: Capture, options: &ScreenshotOptions) -> Result<Capture> {
    options.validate()?;
    dimensions(capture.width, capture.height)?;
    let width = options
        .max_width
        .map_or(capture.width, |width| width.min(capture.width));
    if width == capture.width && options.format == ScreenshotFormat::Png {
        return Ok(capture);
    }
    let mut reader = image::ImageReader::with_format(
        std::io::Cursor::new(&capture.bytes),
        image::ImageFormat::Png,
    );
    reader.limits(limits());
    let mut pixels = reader
        .decode()
        .context(ErrorCode::ScreenshotFailed.error("cannot decode captured PNG"))?;
    if width < capture.width {
        let height =
            (u64::from(capture.height) * u64::from(width) / u64::from(capture.width)).max(1) as u32;
        pixels = pixels.resize_exact(width, height, image::imageops::FilterType::Triangle);
    }
    let (width, height) = (pixels.width(), pixels.height());
    let quality = options.quality.unwrap_or(80);
    let bytes = match options.format {
        ScreenshotFormat::Png => {
            let mut out = std::io::Cursor::new(Vec::new());
            pixels.write_to(&mut out, image::ImageFormat::Png)?;
            out.into_inner()
        }
        ScreenshotFormat::Jpeg => {
            let pixels = pixels.into_rgb8();
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode(
                pixels.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgb8,
            )?;
            out
        }
        ScreenshotFormat::Webp => {
            let pixels = pixels.into_rgba8();
            webp::Encoder::from_rgba(pixels.as_raw(), width, height)
                .encode_simple(false, f32::from(quality))
                .map_err(|error| {
                    ErrorCode::ScreenshotFailed.error(format!("WebP encoding failed: {error:?}"))
                })?
                .to_vec()
        }
    };
    if bytes.len() as u64 > MAX_SCREENSHOT {
        bail!(ErrorCode::FileTooLarge.error("encoded screenshot exceeds 64 MiB"));
    }
    Ok(Capture {
        bytes,
        width,
        height,
        at: capture.at,
        format: options.format,
    })
}
pub(crate) fn validate_received(
    bytes: &[u8],
    format: ScreenshotFormat,
    width: u32,
    height: u32,
) -> Result<()> {
    dimensions(width, height)?;
    let actual = image::guess_format(bytes)
        .context(ErrorCode::InvalidMessage.error("invalid screenshot encoding"))?;
    let expected = match format {
        ScreenshotFormat::Png => image::ImageFormat::Png,
        ScreenshotFormat::Jpeg => image::ImageFormat::Jpeg,
        ScreenshotFormat::Webp => image::ImageFormat::WebP,
    };
    if actual != expected {
        bail!(ErrorCode::InvalidMessage.error("screenshot format differs from its metadata"));
    }
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), actual);
    reader.limits(limits());
    if reader.into_dimensions()? != (width, height) {
        bail!(ErrorCode::InvalidMessage.error("screenshot dimensions differ from its metadata"));
    }
    Ok(())
}
#[cfg(test)]
mod encoding_tests {
    use super::*;
    fn fixture() -> Result<Vec<u8>> {
        let pixels = image::RgbImage::from_fn(640, 360, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 251) as u8, ((x * y) % 251) as u8])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(pixels).write_to(&mut out, image::ImageFormat::Png)?;
        Ok(out.into_inner())
    }
    #[test]
    fn original_png_is_preserved_and_all_formats_resize_with_actual_metadata() -> Result<()> {
        let original = fixture()?;
        let preserved = encode(decode(original.clone())?, &ScreenshotOptions::default())?;
        assert_eq!(preserved.bytes, original);
        for format in [
            ScreenshotFormat::Png,
            ScreenshotFormat::Jpeg,
            ScreenshotFormat::Webp,
        ] {
            let output = encode(
                decode(original.clone())?,
                &ScreenshotOptions {
                    max_width: Some(320),
                    format,
                    quality: (format != ScreenshotFormat::Png).then_some(70),
                },
            )?;
            assert_eq!((output.width, output.height), (320, 180));
            assert_eq!(output.format, format);
            validate_received(&output.bytes, format, 320, 180)?;
            assert!(output.bytes.len() < original.len());
            assert!(validate_received(&output.bytes, format, 640, 360).is_err());
        }
        let output = encode(
            decode(original)?,
            &ScreenshotOptions {
                max_width: Some(1280),
                format: ScreenshotFormat::Jpeg,
                quality: Some(90),
            },
        )?;
        assert_eq!((output.width, output.height), (640, 360));
        Ok(())
    }
    #[test]
    fn dimensions_quality_and_encoding_are_checked_before_allocation_or_publication() -> Result<()>
    {
        assert!(dimensions(u32::MAX, u32::MAX).is_err());
        assert!(dimensions(0, 360).is_err());
        for options in [
            ScreenshotOptions {
                max_width: Some(0),
                ..Default::default()
            },
            ScreenshotOptions {
                format: ScreenshotFormat::Jpeg,
                quality: Some(0),
                max_width: None,
            },
            ScreenshotOptions {
                quality: Some(80),
                ..Default::default()
            },
        ] {
            assert!(options.validate().is_err());
        }
        assert!(validate_received(&fixture()?, ScreenshotFormat::Webp, 640, 360).is_err());
        Ok(())
    }
}

#[cfg(target_os = "macos")]
async fn platform(
    permit: Option<OwnedSemaphorePermit>,
) -> Result<(Vec<u8>, Option<OwnedSemaphorePermit>)> {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    mac_capture(
        unsafe { CGPreflightScreenCaptureAccess() },
        std::path::Path::new("/usr/sbin/screencapture"),
        permit,
    )
    .await
}
#[cfg(target_os = "macos")]
async fn mac_capture(
    allowed: bool,
    program: &std::path::Path,
    permit: Option<OwnedSemaphorePermit>,
) -> Result<(Vec<u8>, Option<OwnedSemaphorePermit>)> {
    if !allowed {
        bail!(
            ErrorCode::PermissionDenied
                .error("grant Screen Recording permission to the daemon executable")
        )
    }
    // screencapture rejects hidden output names, even while returning exit 0.
    let temp = tempfile::Builder::new()
        .prefix("xrun-capture-")
        .suffix(".png")
        .tempfile()?
        .into_temp_path();
    let child = tokio::process::Command::new(program)
        .args(["-x", "-m"])
        .arg(&temp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut capture = CaptureProcess {
        child: Some(child),
        path: Some(temp),
        permit,
    };
    let status = capture.wait().await?;
    if !status.success() {
        bail!(ErrorCode::NoDisplay.error("screenshot requires a logged-in graphical session"))
    }
    let bytes = crate::transfer::read_screenshot(capture.path.as_ref().unwrap()).context(
        ErrorCode::ScreenshotFailed.error("screencapture did not create a readable PNG"),
    )?;
    Ok((bytes, capture.permit.take()))
}
#[cfg(windows)]
fn windows_capture_path() -> Result<tempfile::TempPath> {
    // GDI+ cannot save over a file held open for writing. TempPath closes the
    // handle while retaining ownership so the PNG is still removed on drop.
    Ok(tempfile::Builder::new()
        .suffix(".png")
        .tempfile()?
        .into_temp_path())
}
#[cfg(windows)]
async fn platform(
    permit: Option<OwnedSemaphorePermit>,
) -> Result<(Vec<u8>, Option<OwnedSemaphorePermit>)> {
    let script = r#"$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class DesktopCheck {
    [DllImport("user32.dll", SetLastError=true)]
    public static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
    [DllImport("user32.dll")]
    public static extern bool CloseDesktop(IntPtr handle);
}
'@
$desktop = [DesktopCheck]::OpenInputDesktop(0, $false, 1)
if ($desktop -eq [IntPtr]::Zero) { exit 77 }
[void][DesktopCheck]::CloseDesktop($desktop)
$bounds = [Windows.Forms.Screen]::PrimaryScreen.Bounds
if ([long]$bounds.Width * $bounds.Height -gt 32000000) { exit 78 }
$bitmap = New-Object Drawing.Bitmap $bounds.Width, $bounds.Height
$graphics = [Drawing.Graphics]::FromImage($bitmap)
try {
    $graphics.CopyFromScreen($bounds.Location, [Drawing.Point]::Empty, $bounds.Size)
    $bitmap.Save($env:XRUN_CAPTURE_PATH, [Drawing.Imaging.ImageFormat]::Png)
} finally {
    $graphics.Dispose()
    $bitmap.Dispose()
}"#;
    windows_capture(script, permit).await
}
#[cfg(windows)]
async fn windows_capture(
    script: &str,
    permit: Option<OwnedSemaphorePermit>,
) -> Result<(Vec<u8>, Option<OwnedSemaphorePermit>)> {
    let temp = windows_capture_path()?;
    let child = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("XRUN_CAPTURE_PATH", &temp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut capture = CaptureProcess {
        child: Some(child),
        path: Some(temp),
        permit,
    };
    let status = capture.wait().await?;
    if status.code() == Some(77) {
        bail!(ErrorCode::ScreenLocked.error("interactive desktop is inaccessible"))
    }
    if status.code() == Some(78) {
        bail!(
            ErrorCode::ScreenshotFailed.error("display exceeds the 32 megapixel resource budget")
        );
    }
    if !status.success() {
        bail!(ErrorCode::ScreenshotFailed.error("PowerShell could not capture or save the display"))
    }
    let bytes = crate::transfer::read_screenshot(capture.path.as_ref().unwrap())
        .context(ErrorCode::ScreenshotFailed.error("cannot read captured PNG"))?;
    Ok((bytes, capture.permit.take()))
}

#[cfg(target_os = "linux")]
async fn platform(
    permit: Option<OwnedSemaphorePermit>,
) -> Result<(Vec<u8>, Option<OwnedSemaphorePermit>)> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s == "wayland")
    {
        bail!(ErrorCode::ScreenshotUnavailable.error("this release supports X11 only"))
    }
    if std::env::var_os("DISPLAY").is_none() {
        bail!(ErrorCode::NoDisplay.error("DISPLAY is unset"))
    }
    tokio::task::spawn_blocking(move || Ok((x11_capture()?, permit))).await?
}
#[cfg(target_os = "linux")]
fn x11_capture() -> Result<Vec<u8>> {
    use x11_dl::{xlib, xrandr};
    let x =
        xlib::Xlib::open().context(ErrorCode::ScreenshotUnavailable.error("libX11 is required"))?;
    unsafe {
        let display = (x.XOpenDisplay)(std::ptr::null());
        if display.is_null() {
            bail!(ErrorCode::NoDisplay.error("cannot open X11 display"))
        }
        struct DisplayGuard<'a>(&'a xlib::Xlib, *mut xlib::Display);
        impl Drop for DisplayGuard<'_> {
            fn drop(&mut self) {
                unsafe {
                    (self.0.XCloseDisplay)(self.1);
                }
            }
        }
        let _display = DisplayGuard(&x, display);
        let screen = (x.XDefaultScreen)(display);
        let root = (x.XRootWindow)(display, screen);
        let (mut left, mut top) = (0, 0);
        let (mut width, mut height) = (
            (x.XDisplayWidth)(display, screen),
            (x.XDisplayHeight)(display, screen),
        );
        if let Ok(randr) = xrandr::Xrandr::open() {
            let mut count = 0;
            let monitors = (randr.XRRGetMonitors)(display, root, 1, &mut count);
            if !monitors.is_null() && count > 0 {
                let list = std::slice::from_raw_parts(monitors, count as usize);
                let monitor = list.iter().find(|m| m.primary != 0).unwrap_or(&list[0]);
                left = monitor.x;
                top = monitor.y;
                width = monitor.width;
                height = monitor.height;
                (randr.XRRFreeMonitors)(monitors);
            }
        }
        if width <= 0 || height <= 0 || (width as u64) * (height as u64) * 3 > 256 * 1024 * 1024 {
            bail!(ErrorCode::ScreenshotFailed.error("unsupported display size"))
        }
        let image = (x.XGetImage)(
            display,
            root,
            left,
            top,
            width as u32,
            height as u32,
            !0,
            xlib::ZPixmap,
        );
        if image.is_null() {
            bail!(ErrorCode::ScreenshotFailed.error("XGetImage returned no pixels"))
        }
        struct ImageGuard<'a>(&'a xlib::Xlib, *mut xlib::XImage);
        impl Drop for ImageGuard<'_> {
            fn drop(&mut self) {
                unsafe {
                    (self.0.XDestroyImage)(self.1);
                }
            }
        }
        let _image = ImageGuard(&x, image);
        let masks = [(*image).red_mask, (*image).green_mask, (*image).blue_mask];
        if masks.contains(&0) {
            bail!(
                ErrorCode::ScreenshotUnavailable
                    .error("indexed-color X11 displays are unsupported")
            )
        }
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
        for row in 0..height {
            for col in 0..width {
                let pixel = (x.XGetPixel)(image, col, row);
                for mask in masks {
                    let shift = mask.trailing_zeros();
                    let value = (pixel & mask) >> shift;
                    pixels.push((value * 255 / (mask >> shift)) as u8);
                }
            }
        }
        let mut bytes = vec![];
        {
            let mut encoder = png::Encoder::new(&mut bytes, width as u32, height as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&pixels)?;
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn error<T>(result: Result<T>, expected: ErrorCode) {
        let error = match result {
            Ok(_) => panic!("expected {expected:?}"),
            Err(error) => error,
        };
        assert!(crate::error::is(&error, expected), "{error:#}");
    }

    #[test]
    fn png_metadata_and_invalid_or_oversized_output() -> Result<()> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()?
                .write_image_data(&[255, 0, 0, 0, 255, 0])?;
        }
        let capture = decode(bytes.clone())?;
        assert_eq!((capture.width, capture.height), (2, 1));
        assert_eq!(capture.bytes, bytes);
        assert!(capture.at.ends_with('Z'));
        for bytes in [vec![], b"not a PNG".to_vec()] {
            error(decode(bytes), ErrorCode::ScreenshotFailed);
        }
        error(
            decode(vec![0; MAX_SCREENSHOT as usize + 1]),
            ErrorCode::FileTooLarge,
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn canceling_capture_reaps_writer_before_releasing_admission_and_removing_png()
    -> Result<()> {
        use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
        let dir = tempfile::tempdir()?;
        let program = dir.path().join("capture");
        let started = dir.path().join("started");
        let gate = dir.path().join("gate");
        let marker = dir.path().join("marker");
        std::fs::write(
            &program,
            format!(
                concat!(
                    "#!/bin/sh\nprintf '%s\\n%s' \"$$\" \"$3\" > '{}'\n",
                    "while test ! -e '{}'; do /bin/sleep 0.02; done\n",
                    "printf orphan > \"$3\"\nprintf survived > '{}'\n",
                ),
                started.display(),
                gate.display(),
                marker.display(),
            ),
        )?;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = admission.clone().acquire_owned().await?;
        let task = tokio::spawn(async move { mac_capture(true, &program, Some(permit)).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let started = std::fs::read_to_string(started)?;
        let (pid, path) = started.split_once('\n').unwrap();
        let pid: i32 = pid.parse()?;
        let path = std::path::PathBuf::from(path);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let _permit = tokio::time::timeout(Duration::from_secs(5), admission.acquire()).await??;
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "capture writer survived cancellation"
        );
        assert!(!path.exists(), "capture output was not cleaned up");
        std::fs::write(gate, b"continue")?;
        assert!(!marker.exists());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn mac_permission_and_capture_failures_have_distinct_codes() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        // Permission denial must return before attempting to execute anything.
        error(
            mac_capture(false, std::path::Path::new("/does-not-exist"), None).await,
            ErrorCode::PermissionDenied,
        );
        let dir = tempfile::tempdir()?;
        let program = dir.path().join("capture with spaces");
        for (script, expected) in [
            ("#!/bin/sh\nexit 1\n", ErrorCode::NoDisplay),
            ("#!/bin/sh\nexit 0\n", ErrorCode::ScreenshotFailed),
            (
                "#!/bin/sh\nprintf invalid > \"$3\"\n",
                ErrorCode::ScreenshotFailed,
            ),
        ] {
            std::fs::write(&program, script)?;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
            error(
                mac_capture(true, &program, None)
                    .await
                    .and_then(|(bytes, _)| decode(bytes)),
                expected,
            );
        }
        Ok(())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_locked_desktop_and_failed_capture_have_distinct_codes() {
        error(
            windows_capture("exit 77", None).await,
            ErrorCode::ScreenLocked,
        );
        error(
            windows_capture("exit 1", None).await,
            ErrorCode::ScreenshotFailed,
        );
        error(
            windows_capture("exit 0", None)
                .await
                .and_then(|(bytes, _)| decode(bytes)),
            ErrorCode::ScreenshotFailed,
        );
    }

    #[cfg(windows)]
    #[test]
    fn gdi_can_save_to_capture_path() {
        let temp = super::windows_capture_path().unwrap();
        // A bitmap in memory exercises the same GDI+ save operation without
        // requiring an interactive desktop on the Windows CI runner.
        let script = r#"$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Drawing
$b=New-Object Drawing.Bitmap 1,1; try{$b.Save($env:XRUN_CAPTURE_PATH,[Drawing.Imaging.ImageFormat]::Png)}finally{$b.Dispose()}"#;
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("XRUN_CAPTURE_PATH", &temp)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "GDI+ save failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let bytes = crate::transfer::read_screenshot(&temp).unwrap();
        let reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        assert_eq!((reader.info().width, reader.info().height), (1, 1));
        let path = temp.to_path_buf();
        drop(temp);
        assert!(!path.exists(), "capture file was not cleaned up");
    }
}
