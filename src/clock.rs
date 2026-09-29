use anyhow::Result;

pub fn elapsed_clock_ms() -> Result<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000)
    }
    #[cfg(target_os = "macos")]
    {
        #[repr(C)]
        struct Timebase {
            numer: u32,
            denom: u32,
        }
        unsafe extern "C" {
            fn mach_continuous_time() -> u64;
            fn mach_timebase_info(info: *mut Timebase) -> i32;
        }
        let mut info = Timebase { numer: 0, denom: 0 };
        if unsafe { mach_timebase_info(&mut info) } != 0 || info.denom == 0 {
            anyhow::bail!("mach_timebase_info failed");
        }
        let ticks = unsafe { mach_continuous_time() } as u128;
        Ok((ticks * info.numer as u128 / info.denom as u128 / 1_000_000) as u64)
    }
    #[cfg(windows)]
    {
        Ok(unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        anyhow::bail!("unsupported platform")
    }
}
