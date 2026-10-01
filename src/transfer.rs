use crate::{config::sync_parent, protocol::MAX_FILE};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::Mutex as AsyncMutex;

pub fn remote_path(value: &str, cwd: &Path) -> Result<PathBuf> {
    if value.is_empty() || value.contains('\0') {
        bail!("INVALID_PATH: empty path or NUL")
    };
    let p = Path::new(value);
    Ok(if p.is_absolute() {
        p.into()
    } else {
        cwd.join(p)
    })
}
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}
fn open_file(path: &Path) -> Result<std::fs::File> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("read file metadata: {}", path.display()))?;
    if metadata.is_dir() {
        bail!("IS_DIRECTORY: {}", path.display())
    }
    if !metadata.is_file() {
        bail!("INVALID_PATH: only regular files are supported")
    }
    if metadata.len() > MAX_FILE {
        bail!("FILE_TOO_LARGE: maximum {MAX_FILE} bytes")
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("INVALID_PATH: only regular files are supported")
    }
    Ok(file)
}
pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    let file = open_file(path)?;
    let mut bytes = vec![];
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        bail!("FILE_TOO_LARGE: maximum {MAX_FILE} bytes")
    }
    Ok(bytes)
}
fn copy_hashed(file: std::fs::File, output: &mut impl std::io::Write) -> Result<(u64, String)> {
    let mut input = file.take(MAX_FILE + 1);
    let mut chunk = [0u8; 64 * 1024];
    let mut size = 0;
    let mut digest = Sha256::new();
    loop {
        let n = input.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_FILE {
            bail!("FILE_TOO_LARGE: maximum {MAX_FILE} bytes")
        }
        output.write_all(&chunk[..n])?;
        digest.update(&chunk[..n]);
    }
    Ok((size, hex::encode(digest.finalize())))
}
pub async fn snapshot(path: PathBuf) -> Result<(tempfile::NamedTempFile, u64, String)> {
    tokio::task::spawn_blocking(move || {
        let mut temp = tempfile::Builder::new()
            .prefix("xrun-download-")
            .tempfile()?;
        let (size, hash) = copy_hashed(open_file(&path)?, temp.as_file_mut())?;
        temp.as_file_mut().seek(SeekFrom::Start(0))?;
        Ok((temp, size, hash))
    })
    .await?
}
fn destination(path: &Path, mkdir: bool) -> Result<PathBuf> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        let target = std::fs::read_link(path)?;
        let target = if target.is_absolute() {
            target
        } else {
            path.parent().context("missing parent")?.join(target)
        };
        return destination_inner(&target, mkdir, 1);
    }
    destination_inner(path, mkdir, 0)
}
fn destination_inner(path: &Path, mkdir: bool, depth: usize) -> Result<PathBuf> {
    if depth > 40 {
        bail!("INVALID_PATH: symlink loop")
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        let target = std::fs::read_link(path)?;
        let target = if target.is_absolute() {
            target
        } else {
            path.parent().context("missing parent")?.join(target)
        };
        return destination_inner(&target, mkdir, depth + 1);
    }
    if let Ok(metadata) = std::fs::metadata(path) {
        if metadata.is_dir() {
            bail!("IS_DIRECTORY: {}", path.display())
        }
        if !metadata.is_file() {
            bail!("INVALID_PATH: only regular files are supported")
        }
    }
    let parent = path.parent().context("INVALID_PATH: no parent directory")?;
    if mkdir {
        std::fs::create_dir_all(parent)?
    }
    let parent = std::fs::canonicalize(parent)
        .context("PARENT_NOT_FOUND: destination directory must exist")?;
    Ok(parent.join(path.file_name().context("INVALID_PATH: no filename")?))
}
static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>> = OnceLock::new();
pub async fn push(
    path: PathBuf,
    mut contents: tempfile::NamedTempFile,
    mkdir: bool,
    no_overwrite: bool,
    expect: Option<String>,
) -> Result<PathBuf> {
    if contents.as_file().metadata()?.len() > MAX_FILE {
        bail!("FILE_TOO_LARGE: maximum {MAX_FILE} bytes")
    }
    if expect.as_ref().is_some_and(|s| !valid_hash(s)) {
        bail!("INVALID_EXPECT: expected a full SHA-256")
    }
    if no_overwrite && expect.is_some() {
        bail!("INVALID_REQUEST: expect conflicts with no-overwrite")
    }
    let path = destination(&path, mkdir)?;
    let lock = {
        let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap();
        locks.retain(|_, v| v.strong_count() > 0);
        match locks.get(&path).and_then(Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = Arc::new(AsyncMutex::new(()));
                locks.insert(path.clone(), Arc::downgrade(&lock));
                lock
            }
        }
    };
    let _guard = lock.lock().await;
    let work = path.clone();
    tokio::task::spawn_blocking(move || {
        contents.as_file_mut().seek(SeekFrom::Start(0))?;
        save(
            &work,
            contents.as_file_mut(),
            no_overwrite,
            expect.as_deref(),
        )
    })
    .await??;
    Ok(path)
}
fn check_expect(path: &Path, expect: Option<&str>) -> Result<()> {
    if let Some(hash) = expect {
        match open_file(path).and_then(|file| copy_hashed(file, &mut std::io::sink())) {
            Ok((_, actual)) if actual.eq_ignore_ascii_case(hash) => {}
            _ => bail!("STALE: destination no longer matches expected SHA-256"),
        }
    }
    Ok(())
}
fn save(
    path: &Path,
    contents: &mut impl Read,
    no_overwrite: bool,
    expect: Option<&str>,
) -> Result<()> {
    check_expect(path, expect)?;
    let metadata = std::fs::metadata(path).ok();
    if let Some(m) = &metadata {
        if m.is_dir() {
            bail!("IS_DIRECTORY: {}", path.display())
        }
        if !m.is_file() {
            bail!("INVALID_PATH: only regular files are supported")
        }
    }
    if no_overwrite && metadata.is_some() {
        bail!("ALREADY_EXISTS: {}", path.display())
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix(".xrun-transfer-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut temp = builder.tempfile_in(path.parent().context("missing parent")?)?;
    let defaults = temp.as_file().metadata()?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    if metadata.is_some() {
        crate::config::private_acl(temp.path(), false)?;
    }
    std::io::copy(contents, temp.as_file_mut())?;
    temp.as_file()
        .set_permissions(metadata.map(|m| m.permissions()).unwrap_or(defaults))?;
    temp.as_file().sync_all()?;
    check_expect(path, expect)?;
    if no_overwrite {
        temp.persist_noclobber(path)
            .map_err(|e| anyhow::anyhow!("ALREADY_EXISTS: {}", e.error))?;
    } else {
        replace(temp, path)?;
    }
    sync_parent(path).context("UNCONFIRMED: destination replaced but directory sync failed")?;
    Ok(())
}
#[cfg(not(windows))]
fn replace(temp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
#[cfg(windows)]
fn replace(temp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    // ReplaceFileW opens its replacement without sharing, so release our
    // write handle while retaining cleanup ownership of its path.
    let temp = temp.into_temp_path();
    if !path.exists() {
        temp.persist(path).map_err(|e| e.error)?;
        return Ok(());
    }
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let old: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let new: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe {
        ReplaceFileW(
            old.as_ptr(),
            new.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
pub fn save_local(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    // Resolve the chosen parent, never the final path component. Publication
    // replaces that directory entry rather than writing through a link.
    let name = path.file_name().context("INVALID_PATH: missing filename")?;
    let path = path
        .parent()
        .context("INVALID_PATH: missing parent")?
        .canonicalize()?
        .join(name);
    let metadata = local_metadata(&path)?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    std::io::Write::write_all(&mut temp, bytes)?;
    if let Some(metadata) = metadata {
        temp.as_file().set_permissions(metadata.permissions())?;
    }
    temp.as_file().sync_all()?;
    local_metadata(&path)?;
    // MoveFileExW on Windows, rename on Unix; do not use ReplaceFileW here.
    temp.into_temp_path().persist(&path).map_err(|e| e.error)?;
    sync_parent(&path).context("UNCONFIRMED: destination replaced but directory sync failed")?;
    Ok(path)
}
fn local_metadata(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                bail!("INVALID_PATH: local destination must not be a symbolic link")
            }
            if metadata.is_dir() {
                bail!("IS_DIRECTORY: local destination is a directory")
            }
            if !metadata.is_file() {
                bail!("INVALID_PATH: local destination must be a regular file")
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
