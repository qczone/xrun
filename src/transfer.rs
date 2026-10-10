use crate::config::sync_parent;
use crate::error::{ErrorCode, file_io};
#[cfg(any(target_os = "macos", windows, test))]
use crate::protocol::MAX_SCREENSHOT;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex as AsyncMutex;

pub(crate) fn remote_path(value: &str, cwd: &Path) -> Result<PathBuf> {
    if value.is_empty() || value.contains('\0') {
        bail!(ErrorCode::InvalidPath.error("empty path or NUL"))
    };
    let p = Path::new(value);
    Ok(if p.is_absolute() {
        p.into()
    } else {
        cwd.join(p)
    })
}
pub(crate) fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}
fn open_file(path: &Path) -> Result<std::fs::File> {
    let metadata = std::fs::metadata(path)
        .map_err(file_io)
        .with_context(|| format!("read file metadata: {}", path.display()))?;
    if metadata.is_dir() {
        bail!(ErrorCode::IsDirectory.error(format!("{}", path.display())))
    }
    if !metadata.is_file() {
        bail!(ErrorCode::InvalidPath.error("only regular files are supported"))
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(file_io)?;
    if !file.metadata()?.is_file() {
        bail!(ErrorCode::InvalidPath.error("only regular files are supported"))
    }
    Ok(file)
}
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn read_screenshot(path: &Path) -> Result<Vec<u8>> {
    let file = open_file(path)?;
    if file.metadata()?.len() > MAX_SCREENSHOT {
        bail!(ErrorCode::FileTooLarge.error("screenshot exceeds 64 MiB"));
    }
    let mut bytes = vec![];
    file.take(MAX_SCREENSHOT + 1)
        .read_to_end(&mut bytes)
        .map_err(file_io)?;
    if bytes.len() as u64 > MAX_SCREENSHOT {
        bail!(ErrorCode::FileTooLarge.error("screenshot exceeds 64 MiB"))
    }
    Ok(bytes)
}
fn copy_hashed(
    mut input: impl Read,
    output: &mut impl std::io::Write,
    canceled: Option<&AtomicBool>,
) -> Result<(u64, String)> {
    let mut chunk = [0u8; 64 * 1024];
    let mut size = 0;
    let mut digest = Sha256::new();
    loop {
        if let Some(canceled) = canceled {
            check_canceled(canceled)?;
        }
        let n = input.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        output.write_all(&chunk[..n])?;
        digest.update(&chunk[..n]);
    }
    Ok((size, hex::encode(digest.finalize())))
}
/// Canceling the async waiter must not release admission while disk IO is running.
pub(crate) async fn snapshot(
    path: PathBuf,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<(
    tempfile::NamedTempFile,
    u64,
    String,
    tokio::sync::OwnedSemaphorePermit,
)> {
    let cancel = CancelOnDrop::default();
    let canceled = cancel.0.clone();
    tokio::task::spawn_blocking(move || {
        let permit = permit;
        check_canceled(&canceled)?;
        let mut temp = tempfile::Builder::new()
            .prefix("xrun-download-")
            .tempfile()?;
        let (size, hash) = copy_hashed(open_file(&path)?, temp.as_file_mut(), Some(&canceled))?;
        temp.as_file_mut().seek(SeekFrom::Start(0))?;
        Ok((temp, size, hash, permit))
    })
    .await?
}
fn destination(path: &Path, mkdir: bool) -> Result<PathBuf> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        let target = std::fs::read_link(path).map_err(file_io)?;
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
        bail!(ErrorCode::InvalidPath.error("symlink loop"))
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        let target = std::fs::read_link(path).map_err(file_io)?;
        let target = if target.is_absolute() {
            target
        } else {
            path.parent().context("missing parent")?.join(target)
        };
        return destination_inner(&target, mkdir, depth + 1);
    }
    if let Ok(metadata) = std::fs::metadata(path) {
        if metadata.is_dir() {
            bail!(ErrorCode::IsDirectory.error(format!("{}", path.display())))
        }
        if !metadata.is_file() {
            bail!(ErrorCode::InvalidPath.error("only regular files are supported"))
        }
    }
    let parent = path
        .parent()
        .context(ErrorCode::InvalidPath.error("no parent directory"))?;
    if mkdir {
        std::fs::create_dir_all(parent).map_err(file_io)?
    }
    let parent = std::fs::canonicalize(parent)
        .context(ErrorCode::ParentNotFound.error("destination directory must exist"))?;
    Ok(parent.join(
        path.file_name()
            .context(ErrorCode::InvalidPath.error("no filename"))?,
    ))
}
#[derive(Default)]
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
fn check_canceled(canceled: &AtomicBool) -> Result<()> {
    if canceled.load(Ordering::SeqCst) {
        bail!(ErrorCode::ConnectionClosed.error("file operation canceled before publication"));
    }
    Ok(())
}

static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>> = OnceLock::new();
type PushCallback = Box<dyn FnOnce(&Path, bool, Option<&anyhow::Error>) -> Result<()> + Send>;
struct PushCompletion {
    path: PathBuf,
    callback: Option<PushCallback>,
}
impl PushCompletion {
    fn complete(&mut self, published: bool, error: Option<&anyhow::Error>) -> Result<()> {
        self.callback.take().expect("push completion runs once")(&self.path, published, error)
    }
}
impl Drop for PushCompletion {
    fn drop(&mut self) {
        if let Some(callback) = self.callback.take() {
            let error = ErrorCode::ConnectionClosed
                .error("push interrupted before completion")
                .into();
            if let Err(error) = callback(&self.path, false, Some(&error)) {
                tracing::error!(%error, "interrupted push result could not be recorded");
            }
        }
    }
}
pub(crate) async fn push(
    path: PathBuf,
    mut contents: tempfile::NamedTempFile,
    mkdir: bool,
    no_overwrite: bool,
    expect: Option<String>,
    finished: impl FnOnce(&Path, bool, Option<&anyhow::Error>) -> Result<()> + Send + 'static,
) -> Result<PathBuf> {
    let mut completion = PushCompletion {
        path: path.clone(),
        callback: Some(Box::new(finished)),
    };
    let prepared = (|| {
        if expect.as_ref().is_some_and(|s| !valid_hash(s)) {
            bail!(ErrorCode::InvalidExpect.error("expected a full SHA-256"));
        }
        if no_overwrite && expect.is_some() {
            bail!(ErrorCode::InvalidRequest.error("expect conflicts with no-overwrite"));
        }
        destination(&path, mkdir)
    })();
    let path = match prepared {
        Ok(path) => path,
        Err(error) => {
            completion.complete(false, Some(&error))?;
            return Err(error);
        }
    };
    completion.path = path.clone();
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
    let guard = lock.lock_owned().await;
    let cancel = CancelOnDrop::default();
    let canceled = cancel.0.clone();
    let work = path.clone();
    tokio::task::spawn_blocking(move || {
        // The worker owns admission (inside finished), path serialization and job completion
        // through actual completion, even when its async waiter has been dropped.
        let _guard = guard;
        let mut published = false;
        let result = (|| {
            check_canceled(&canceled)?;
            contents.as_file_mut().seek(SeekFrom::Start(0))?;
            save(
                &work,
                contents.as_file_mut(),
                no_overwrite,
                expect.as_deref(),
                &canceled,
                &mut published,
            )
        })();
        let completion_result = completion.complete(published, result.as_ref().err());
        result?;
        completion_result
    })
    .await??;
    Ok(path)
}
fn check_expect(path: &Path, expect: Option<&str>) -> Result<()> {
    if let Some(hash) = expect {
        match open_file(path).and_then(|file| copy_hashed(file, &mut std::io::sink(), None)) {
            Ok((_, actual)) if actual.eq_ignore_ascii_case(hash) => {}
            _ => bail!(ErrorCode::Stale.error("destination no longer matches expected SHA-256")),
        }
    }
    Ok(())
}
fn save(
    path: &Path,
    contents: &mut impl Read,
    no_overwrite: bool,
    expect: Option<&str>,
    canceled: &AtomicBool,
    published: &mut bool,
) -> Result<()> {
    check_canceled(canceled)?;
    check_expect(path, expect)?;
    let metadata = std::fs::metadata(path).ok();
    if let Some(m) = &metadata {
        if m.is_dir() {
            bail!(ErrorCode::IsDirectory.error(format!("{}", path.display())))
        }
        if !m.is_file() {
            bail!(ErrorCode::InvalidPath.error("only regular files are supported"))
        }
    }
    if no_overwrite && metadata.is_some() {
        bail!(ErrorCode::AlreadyExists.error(format!("{}", path.display())))
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
    let mut chunk = [0u8; 64 * 1024];
    loop {
        check_canceled(canceled)?;
        let count = contents.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        std::io::Write::write_all(temp.as_file_mut(), &chunk[..count])?;
    }
    temp.as_file()
        .set_permissions(metadata.map(|m| m.permissions()).unwrap_or(defaults))?;
    temp.as_file().sync_all()?;
    check_expect(path, expect)?;
    // Cancellation before this point preserves the old destination. Once the
    // atomic replacement starts, callers must treat an interrupted reply as unknown.
    check_canceled(canceled)?;
    if no_overwrite {
        temp.persist_noclobber(path)
            .map_err(|e| anyhow::anyhow!(ErrorCode::AlreadyExists.error(format!("{}", e.error))))?;
    } else {
        replace(temp, path)?;
    }
    *published = true;
    sync_parent(path)
        .context(ErrorCode::Unconfirmed.error("destination replaced but directory sync failed"))?;
    Ok(())
}
#[cfg(not(windows))]
fn replace(temp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    temp.persist(path).map_err(|e| file_io(e.error))?;
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
        return Err(file_io(std::io::Error::last_os_error()).into());
    }
    Ok(())
}
pub fn prepare_upload(path: &Path) -> Result<(std::fs::File, u64, String)> {
    let mut file = open_file(path)?;
    let (size, hash) = copy_hashed(file.try_clone()?, &mut std::io::sink(), None)?;
    file.seek(SeekFrom::Start(0))?;
    Ok((file, size, hash))
}
pub(crate) fn snapshot_input(input: impl Read) -> Result<(tempfile::NamedTempFile, u64, String)> {
    let mut temp = tempfile::Builder::new().prefix("xrun-input-").tempfile()?;
    let (size, hash) = copy_hashed(input, temp.as_file_mut(), None)?;
    temp.as_file_mut().seek(SeekFrom::Start(0))?;
    Ok((temp, size, hash))
}
pub fn save_local(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    save_local_reader(path, &mut std::io::Cursor::new(bytes))
}
pub(crate) fn save_local_reader(path: &Path, input: &mut impl Read) -> Result<PathBuf> {
    // Resolve the chosen parent, never the final path component. Publication
    // replaces that directory entry rather than writing through a link.
    let name = path
        .file_name()
        .context(ErrorCode::InvalidPath.error("missing filename"))?;
    let path = path
        .parent()
        .context(ErrorCode::InvalidPath.error("missing parent"))?
        .canonicalize()
        .map_err(file_io)?
        .join(name);
    let metadata = local_metadata(&path)?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    std::io::copy(input, &mut temp)?;
    if let Some(metadata) = metadata {
        temp.as_file().set_permissions(metadata.permissions())?;
    }
    temp.as_file().sync_all()?;
    local_metadata(&path)?;
    // MoveFileExW on Windows, rename on Unix; do not use ReplaceFileW here.
    temp.into_temp_path()
        .persist(&path)
        .map_err(|e| file_io(e.error))?;
    sync_parent(&path)
        .context(ErrorCode::Unconfirmed.error("destination replaced but directory sync failed"))?;
    Ok(path)
}
fn local_metadata(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                bail!(ErrorCode::InvalidPath.error("local destination must not be a symbolic link"))
            }
            if metadata.is_dir() {
                bail!(ErrorCode::IsDirectory.error("local destination is a directory"))
            }
            if !metadata.is_file() {
                bail!(ErrorCode::InvalidPath.error("local destination must be a regular file"))
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(file_io(error).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_capture_files_are_still_rejected_before_buffering() -> Result<()> {
        let file = tempfile::NamedTempFile::new()?;
        file.as_file().set_len(MAX_SCREENSHOT + 1)?;
        assert!(crate::error::is(
            &read_screenshot(file.path()).unwrap_err(),
            ErrorCode::FileTooLarge
        ));
        Ok(())
    }

    #[test]
    fn canceled_queued_push_keeps_admission_and_path_lock_until_worker_finishes() -> Result<()> {
        use std::io::Write;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()?;
        runtime.block_on(async {
            let dir = tempfile::tempdir()?;
            let path = dir.path().canonicalize()?.join("destination");
            std::fs::write(&path, b"original")?;
            let mut input = tempfile::NamedTempFile::new()?;
            input.write_all(b"replacement")?;
            let admission = Arc::new(tokio::sync::Semaphore::new(1));
            let permit = admission.clone().acquire_owned().await?;
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                release_rx.recv().unwrap();
            });
            started_rx.await?;
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let target = path.clone();
            let task = tokio::spawn(async move {
                push(target, input, false, false, None, move |_, published, _| {
                    let _permit = permit;
                    let _ = done_tx.send(published);
                    Ok(())
                })
                .await
            });
            let lock = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let lock = LOCKS
                        .get_or_init(Default::default)
                        .lock()
                        .unwrap()
                        .get(&path)
                        .and_then(Weak::upgrade);
                    if let Some(lock) = lock {
                        break lock;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await?;
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            let retained_admission = admission.available_permits() == 0;
            let retained_lock = lock.try_lock().is_err();
            // Always unblock the worker before assertions so a regression cannot
            // leave Runtime::drop waiting for this fixture forever.
            release_tx.send(())?;
            blocker.await?;
            assert!(!tokio::time::timeout(std::time::Duration::from_secs(5), done_rx).await??);
            assert!(
                retained_admission,
                "canceled waiter released running disk IO admission"
            );
            assert!(retained_lock, "canceled waiter released path serialization");
            let _permit = admission.acquire().await?;
            // The callback releases admission before the worker drops its path guard.
            // Wait for path serialization independently rather than assuming atomic release.
            let _guard = tokio::time::timeout(std::time::Duration::from_secs(5), lock.lock())
                .await
                .context("push worker did not release its path lock")?;
            assert_eq!(std::fs::read(&path)?, b"original");
            Ok(())
        })
    }

    #[test]
    fn cancellation_while_copying_never_publishes_partial_destination() -> Result<()> {
        struct CancelDuringRead<'a> {
            input: std::io::Cursor<&'static [u8]>,
            canceled: &'a AtomicBool,
            at_eof: bool,
        }
        impl Read for CancelDuringRead<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let count = self.input.read(bytes)?;
                if !self.at_eof || count == 0 {
                    self.canceled.store(true, Ordering::SeqCst);
                }
                Ok(count)
            }
        }
        for at_eof in [false, true] {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("destination");
            std::fs::write(&path, b"original")?;
            let canceled = AtomicBool::new(false);
            let mut input = CancelDuringRead {
                input: std::io::Cursor::new(b"replacement"),
                canceled: &canceled,
                at_eof,
            };
            let mut published = false;
            assert!(save(&path, &mut input, false, None, &canceled, &mut published).is_err());
            assert!(!published);
            assert_eq!(std::fs::read(path)?, b"original");
            assert_eq!(
                std::fs::read_dir(dir.path())?.count(),
                1,
                "temporary upload survived cancellation"
            );
        }
        Ok(())
    }

    #[test]
    fn user_paths_have_explicit_file_error_codes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        for error in [
            read_screenshot(&dir.path().join("absent")).unwrap_err(),
            prepare_upload(&dir.path().join("absent")).unwrap_err(),
        ] {
            assert!(crate::error::is(&error, ErrorCode::FileNotFound));
            assert_eq!(crate::error::wire(&error).0, "FILE_NOT_FOUND");
        }
        assert!(crate::error::is(
            &read_screenshot(dir.path()).unwrap_err(),
            ErrorCode::IsDirectory
        ));
        Ok(())
    }
}
