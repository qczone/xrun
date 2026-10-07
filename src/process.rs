use anyhow::Result;
use std::{collections::BTreeMap, path::Path};
use tokio::io::{AsyncRead, AsyncWrite};

pub(crate) type Input = Box<dyn AsyncWrite + Unpin + Send>;
pub(crate) type Output = Box<dyn AsyncRead + Unpin + Send>;

// Strip implicit build-session controls on every task launch, regardless of how
// the daemon was started. Explicit config/request overrides are applied afterward.
const INHERITED_BUILD_ENV: &[&str] = &[
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET",
    "RUSTUP_TOOLCHAIN",
    "RUST_RECURSION_COUNT",
    "RUSTC",
    "RUSTDOC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
];

pub struct ManagedChild {
    pub pid: u32,
    pub stdin: Option<Input>,
    pub stdout: Option<Output>,
    pub stderr: Option<Output>,
    #[cfg(unix)]
    child: tokio::process::Child,
    #[cfg(unix)]
    reaped: bool,
    #[cfg(windows)]
    child: windows::NativeChild,
}

impl ManagedChild {
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            #[cfg(target_os = "linux")]
            let pidfd = {
                use std::os::fd::FromRawFd;
                let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, self.pid, 0) };
                if raw >= 0 {
                    let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw as i32) };
                    tokio::io::unix::AsyncFd::new(owned).ok()
                } else {
                    None
                }
            };
            loop {
                {
                    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                    let result = unsafe {
                        libc::waitid(
                            libc::P_PID,
                            self.pid as libc::id_t,
                            &mut info,
                            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                        )
                    };
                    if result == -1 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        return Err(error.into());
                    }
                    if unsafe { info.si_pid() } != 0 {
                        let status = unsafe { info.si_status() };
                        let raw = match info.si_code {
                            libc::CLD_EXITED => status << 8,
                            libc::CLD_DUMPED => status | 0x80,
                            _ => status,
                        };
                        return Ok(std::process::ExitStatus::from_raw(raw));
                    }
                }
                #[cfg(target_os = "linux")]
                if let Some(fd) = &pidfd {
                    let _ready = fd.readable().await?;
                    continue;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
        #[cfg(windows)]
        {
            self.child.wait().await
        }
    }
    // Keep the leader waitable until all group signals have been sent. Its PID
    // cannot be reused while it remains a zombie.
    pub async fn reap(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            self.reaped = true;
            self.child.wait().await?;
        }
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for ManagedChild {
    fn drop(&mut self) {
        if !self.reaped {
            force_kill(self.pid);
        }
    }
}

#[cfg(unix)]
pub fn spawn(
    path: &Path,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    _job_id: &str,
    _cmd_script: bool,
) -> Result<ManagedChild> {
    use std::os::unix::process::CommandExt;
    let mut cmd = tokio::process::Command::new(path);
    for name in INHERITED_BUILD_ENV {
        cmd.env_remove(name);
    }
    cmd.args(args)
        .current_dir(cwd)
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    cmd.as_std_mut().process_group(0);
    let mut child = cmd.spawn()?;
    Ok(ManagedChild {
        pid: child
            .id()
            .ok_or_else(|| anyhow::anyhow!("child PID missing"))?,
        stdin: Some(Box::new(child.stdin.take().unwrap())),
        stdout: Some(Box::new(child.stdout.take().unwrap())),
        stderr: Some(Box::new(child.stderr.take().unwrap())),
        child,
        reaped: false,
    })
}

#[cfg(windows)]
pub fn spawn(
    path: &Path,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    job_id: &str,
    cmd_script: bool,
) -> Result<ManagedChild> {
    let native = windows::spawn(path, args, cwd, env, job_id, cmd_script)?;
    Ok(ManagedChild {
        pid: native.pid,
        stdin: Some(Box::new(native.stdin)),
        stdout: Some(Box::new(native.stdout)),
        stderr: Some(Box::new(native.stderr)),
        child: native.child,
    })
}

#[cfg(unix)]
pub fn terminate(pid: u32) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
}
#[cfg(unix)]
pub fn force_kill(pid: u32) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(windows)]
pub fn terminate(pid: u32) {
    windows::terminate(pid);
}
#[cfg(windows)]
pub fn force_kill(pid: u32) {
    windows::terminate(pid);
}

#[cfg(windows)]
mod windows {
    use crate::error::ErrorCode;
    use anyhow::{Result, bail};
    use std::{
        collections::{BTreeMap, HashMap},
        ffi::OsStr,
        mem::size_of,
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        path::Path,
        ptr::{null, null_mut},
        sync::{Arc, Mutex, OnceLock},
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
        },
        Security::SECURITY_ATTRIBUTES,
        System::{
            JobObjects::{
                CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Pipes::CreatePipe,
            Threading::{
                CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
                EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
                InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
                STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    };

    static JOBS: OnceLock<Mutex<HashMap<u32, isize>>> = OnceLock::new();
    fn jobs() -> &'static Mutex<HashMap<u32, isize>> {
        JOBS.get_or_init(|| Mutex::new(HashMap::new()))
    }
    pub(super) fn terminate(pid: u32) {
        if let Some(h) = jobs().lock().unwrap().get(&pid).copied() {
            unsafe {
                TerminateJobObject(h as HANDLE, 1);
            }
        }
    }

    struct Handle(HANDLE);
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}
    impl Handle {
        fn new(h: HANDLE) -> Result<Self> {
            if h.is_null() {
                Err(std::io::Error::last_os_error().into())
            } else {
                Ok(Self(h))
            }
        }
        fn into_file(self) -> std::fs::File {
            let h = self.0;
            std::mem::forget(self);
            unsafe { std::fs::File::from_raw_handle(h) }
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    pub(super) struct NativeChild {
        pub(super) pid: u32,
        _process: Arc<Handle>,
        job: Handle,
        waiter: tokio::task::JoinHandle<Result<u32>>,
        exit: Option<u32>,
    }
    impl Drop for NativeChild {
        fn drop(&mut self) {
            jobs().lock().unwrap().remove(&self.pid);
        }
    }
    impl NativeChild {
        pub(super) async fn wait(&mut self) -> Result<std::process::ExitStatus> {
            use std::os::windows::process::ExitStatusExt;
            let code = if let Some(code) = self.exit {
                code
            } else {
                let code = (&mut self.waiter).await??;
                self.exit = Some(code);
                code
            };
            Ok(std::process::ExitStatus::from_raw(code))
        }
    }

    pub(super) struct Spawned {
        pub(super) pid: u32,
        pub(super) stdin: tokio::fs::File,
        pub(super) stdout: tokio::fs::File,
        pub(super) stderr: tokio::fs::File,
        pub(super) child: NativeChild,
    }
    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn quote_arg(s: &str) -> String {
        if !s.is_empty() && !s.contains([' ', '\t', '\n', '"']) {
            return s.into();
        }
        let mut out = String::from("\"");
        let mut slashes = 0;
        for c in s.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(slashes * 2 + 1));
                    out.push('"');
                    slashes = 0;
                }
                _ => {
                    out.push_str(&"\\".repeat(slashes));
                    slashes = 0;
                    out.push(c);
                }
            }
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        out
    }

    fn environment(overrides: &BTreeMap<String, String>) -> Result<Vec<u16>> {
        // Windows may expose hidden per-drive cwd entries such as `=C:`.
        // `cwd` is explicit for this process, so do not copy those entries
        // into the ordinary key/value environment block.
        let mut values: Vec<(String, String)> = std::env::vars()
            .filter(|(name, _)| {
                !name.starts_with('=')
                    && !super::INHERITED_BUILD_ENV
                        .iter()
                        .any(|blocked| name.eq_ignore_ascii_case(blocked))
            })
            .collect();
        for (k, v) in overrides {
            if let Some(existing) = values
                .iter_mut()
                .find(|(name, _)| name.eq_ignore_ascii_case(k))
            {
                existing.1 = v.clone();
            } else {
                values.push((k.clone(), v.clone()));
            }
        }
        values.sort_by_key(|(k, _)| k.to_ascii_lowercase());
        let mut block = Vec::new();
        for (k, v) in values {
            if k.contains(['\0', '=']) || v.contains('\0') {
                bail!("invalid Windows environment");
            }
            block.extend(format!("{k}={v}").encode_utf16());
            block.push(0);
        }
        block.push(0);
        Ok(block)
    }

    unsafe fn pipe() -> Result<(Handle, Handle)> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1,
        };
        let (mut read, mut write) = (null_mut(), null_mut());
        if unsafe { CreatePipe(&mut read, &mut write, &sa, 0) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok((Handle::new(read)?, Handle::new(write)?))
    }

    pub(super) fn spawn(
        path: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        _job_id: &str,
        cmd_script: bool,
    ) -> Result<Spawned> {
        let app = wide(path.as_os_str());
        let cwd_w = wide(cwd.as_os_str());
        let mut cmdline = quote_arg(&path.to_string_lossy());
        let mut env = env.clone();
        if cmd_script {
            // cmd has different quoting rules from the CRT. Expand each value
            // once inside quotes, with delayed expansion disabled, so paths
            // containing &, ^, %, ! and spaces remain literal.
            cmdline.push_str(" /D /S /V:OFF /C \"");
            for (i, arg) in args.iter().enumerate() {
                if arg.contains(['\"', '\r', '\n']) {
                    bail!(
                        ErrorCode::InvalidScriptArgument
                            .error("cmd arguments cannot contain quotes or newlines")
                    );
                }
                let name = format!("XRUN_CMD_ARG_{i}");
                env.retain(|key, _| !key.eq_ignore_ascii_case(&name));
                env.insert(name.clone(), arg.clone());
                if i != 0 {
                    cmdline.push(' ');
                }
                if arg.is_empty() {
                    // Empty environment values are undefined to cmd, so an
                    // expansion could become the literal variable reference.
                    cmdline.push_str("\"\"");
                } else {
                    cmdline.push_str(&format!("\"%{name}%\""));
                }
            }
            cmdline.push('\"');
        } else {
            for arg in args {
                cmdline.push(' ');
                cmdline.push_str(&quote_arg(arg));
            }
        }
        let mut command: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
        if command.len() > 32767 {
            bail!("Windows command line exceeds 32767 UTF-16 units");
        }
        let environment = environment(&env)?;
        let (stdin_read, stdin_write) = unsafe { pipe()? };
        let (stdout_read, stdout_write) = unsafe { pipe()? };
        let (stderr_read, stderr_write) = unsafe { pipe()? };
        for handle in [&stdin_write, &stdout_read, &stderr_read] {
            if unsafe { SetHandleInformation(handle.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        let job = Handle::new(unsafe { CreateJobObjectW(null(), null()) })?;
        let mut limit = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limit.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limit as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut size = 0usize;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size);
        }
        let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
        let list = storage.as_mut_ptr() as *mut _;
        if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let jobs_list = [job.0];
        let handles = [stdin_read.0, stdout_write.0, stderr_write.0];
        let setup = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                jobs_list.as_ptr() as *const _,
                size_of::<HANDLE>(),
                null_mut(),
                null(),
            ) != 0
                && UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr() as *const _,
                    size_of::<[HANDLE; 3]>(),
                    null_mut(),
                    null(),
                ) != 0
        };
        if !setup {
            unsafe {
                DeleteProcThreadAttributeList(list);
            }
            return Err(std::io::Error::last_os_error().into());
        }
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdin_read.0;
        startup.StartupInfo.hStdOutput = stdout_write.0;
        startup.StartupInfo.hStdError = stderr_write.0;
        startup.lpAttributeList = list;
        let mut info = PROCESS_INFORMATION::default();
        let ok = unsafe {
            CreateProcessW(
                app.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr() as *const _,
                cwd_w.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            )
        };
        unsafe {
            DeleteProcThreadAttributeList(list);
        }
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let process = Arc::new(Handle::new(info.hProcess)?);
        let _thread = Handle::new(info.hThread)?;
        let waiter_handle = process.clone();
        let waiter = tokio::task::spawn_blocking(move || -> Result<u32> {
            if unsafe { WaitForSingleObject(waiter_handle.0, INFINITE) } != WAIT_OBJECT_0 {
                bail!(
                    "WaitForSingleObject failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            let mut code = 0u32;
            if unsafe { GetExitCodeProcess(waiter_handle.0, &mut code) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(code)
        });
        let child = NativeChild {
            pid: info.dwProcessId,
            _process: process,
            job,
            waiter,
            exit: None,
        };
        jobs()
            .lock()
            .unwrap()
            .insert(info.dwProcessId, child.job.0 as isize);
        Ok(Spawned {
            pid: info.dwProcessId,
            stdin: tokio::fs::File::from_std(stdin_write.into_file()),
            stdout: tokio::fs::File::from_std(stdout_read.into_file()),
            stderr: tokio::fs::File::from_std(stderr_read.into_file()),
            child,
        })
    }
}
