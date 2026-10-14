//! Windows service entry without a console window. The scheduled task and the App run
//! this GUI-subsystem program as `xrun.exe`; it starts the bundled console CLI
//! `cli\xrun.exe` hidden, with the same arguments and standard handles, and returns
//! its exit status. The core is therefore shipped once, as the terminal CLI.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    std::process::exit(match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xrun: could not start the bundled CLI: {error}");
            125
        }
    })
}

#[cfg(windows)]
fn run() -> std::io::Result<i32> {
    use std::os::windows::process::CommandExt;
    let cli = std::env::current_exe()?
        .with_file_name("cli")
        .join("xrun.exe");
    // Ending this process (Task Scheduler, uninstall, a test's kill) must end the CLI.
    end_children_with_this_process();
    let status = std::process::Command::new(cli)
        .args(std::env::args_os().skip(1))
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .status()?;
    Ok(status.code().unwrap_or(1))
}

/// Join a job whose members are terminated when its last handle closes; this process
/// holds the only handle until it exits. A failure loses only that cleanup guarantee.
#[cfg(windows)]
fn end_children_with_this_process() {
    use std::ptr::null;
    use windows_sys::Win32::System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Threading::GetCurrentProcess,
    };
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return;
    }
    let mut limit = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limit.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limit as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if configured != 0 {
        unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) };
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("xrun-launcher only starts the Windows App's bundled CLI");
    std::process::exit(2);
}
