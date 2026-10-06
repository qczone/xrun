//! Boot and process identity used for conservative crash recovery.

pub(super) fn boot_id() -> String {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_default()
            .trim()
            .into()
    }
    #[cfg(target_os = "macos")]
    {
        let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&tv);
        let rc = unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&mut tv as *mut libc::timeval).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            format!("{}:{}", tv.tv_sec, tv.tv_usec)
        } else {
            String::new()
        }
    }
    #[cfg(windows)]
    {
        "windows-job-object".into()
    }
}
pub(super) fn process_start(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        s.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)
            .map(str::to_string)
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info);
        let n = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size as i32,
            )
        };
        if n == size as i32 {
            Some(format!(
                "{}:{}",
                info.pbi_start_tvsec, info.pbi_start_tvusec
            ))
        } else {
            None
        }
    }
    #[cfg(windows)]
    {
        let _ = pid;
        None
    }
}
