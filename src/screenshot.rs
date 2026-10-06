use crate::error::ErrorCode;
use crate::protocol::MAX_FILE;
use anyhow::{Context, Result, bail};
pub struct Capture {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub at: String,
}
pub async fn capture() -> Result<Capture> {
    let bytes = platform().await?;
    if bytes.len() as u64 > MAX_FILE {
        bail!(ErrorCode::FileTooLarge.error("screenshot exceeds 64 MiB"))
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
    let reader = decoder
        .read_info()
        .context(ErrorCode::ScreenshotFailed.error("invalid PNG"))?;
    let width = reader.info().width;
    let height = reader.info().height;
    Ok(Capture {
        bytes,
        width,
        height,
        at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?,
    })
}
#[cfg(target_os = "macos")]
async fn platform() -> Result<Vec<u8>> {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        bail!(
            ErrorCode::PermissionDenied
                .error("grant Screen Recording permission to the daemon executable")
        )
    }
    // screencapture rejects hidden output names, even while returning exit 0.
    let temp = tempfile::Builder::new()
        .prefix("xrun-capture-")
        .suffix(".png")
        .tempfile()?;
    let status = tokio::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-m"])
        .arg(temp.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    if !status.success() {
        bail!(ErrorCode::NoDisplay.error("screenshot requires a logged-in graphical session"))
    }
    crate::transfer::read_file(temp.path())
        .context(ErrorCode::ScreenshotFailed.error("screencapture did not create a readable PNG"))
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
async fn platform() -> Result<Vec<u8>> {
    let temp = windows_capture_path()?;
    let script = r#"$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class DesktopCheck { [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access); [DllImport("user32.dll")] public static extern bool CloseDesktop(IntPtr handle); }'
$d=[DesktopCheck]::OpenInputDesktop(0,$false,1); if($d -eq [IntPtr]::Zero){exit 77}; [void][DesktopCheck]::CloseDesktop($d)
$r=[Windows.Forms.Screen]::PrimaryScreen.Bounds; $b=New-Object Drawing.Bitmap $r.Width,$r.Height; $g=[Drawing.Graphics]::FromImage($b); try{$g.CopyFromScreen($r.Location,[Drawing.Point]::Empty,$r.Size);$b.Save($env:XRUN_CAPTURE_PATH,[Drawing.Imaging.ImageFormat]::Png)}finally{$g.Dispose();$b.Dispose()}"#;
    let status = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("XRUN_CAPTURE_PATH", &temp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    if status.code() == Some(77) {
        bail!(ErrorCode::ScreenLocked.error("interactive desktop is inaccessible"))
    }
    if !status.success() {
        bail!(ErrorCode::ScreenshotFailed.error("PowerShell could not capture or save the display"))
    }
    crate::transfer::read_file(&temp)
        .context(ErrorCode::ScreenshotFailed.error("cannot read captured PNG"))
}

#[cfg(all(test, windows))]
mod tests {
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
        let bytes = crate::transfer::read_file(&temp).unwrap();
        let reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        assert_eq!((reader.info().width, reader.info().height), (1, 1));
        let path = temp.to_path_buf();
        drop(temp);
        assert!(!path.exists(), "capture file was not cleaned up");
    }
}
#[cfg(target_os = "linux")]
async fn platform() -> Result<Vec<u8>> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s == "wayland")
    {
        bail!(ErrorCode::ScreenshotUnavailable.error("this release supports X11 only"))
    }
    if std::env::var_os("DISPLAY").is_none() {
        bail!(ErrorCode::NoDisplay.error("DISPLAY is unset"))
    }
    tokio::task::spawn_blocking(x11_capture).await?
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
