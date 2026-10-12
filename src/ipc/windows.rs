//! Verify the connected pipe before transmitting even the local authentication token.
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE},
    Security::{EqualSid, GetTokenInformation, IsValidSid, TOKEN_QUERY, TOKEN_USER, TokenUser},
    System::{
        Pipes::GetNamedPipeServerProcessId,
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

struct User(Vec<usize>);
impl User {
    fn read(process: HANDLE) -> io::Result<Self> {
        let mut token = std::ptr::null_mut();
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // Keep every real process/token handle owned, including all failure paths.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut bytes = 0;
        let rc = unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut bytes,
            )
        };
        if rc != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
            || bytes < std::mem::size_of::<TOKEN_USER>() as u32
            || bytes > 64 * 1024
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // TOKEN_USER contains pointers, so an aligned allocation is required.
        let mut user = Self(vec![
            0;
            (bytes as usize).div_ceil(std::mem::size_of::<usize>())
        ]);
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                user.0.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe { IsValidSid(user.sid()) } == 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(user)
    }
    fn sid(&self) -> *mut std::ffi::c_void {
        // The OS wrote this pointer into the allocation and it remains alive.
        unsafe { (*(self.0.as_ptr().cast::<TOKEN_USER>())).User.Sid }
    }
}
fn require_same_user(server: &User, client: &User) -> io::Result<()> {
    if unsafe { EqualSid(server.sid(), client.sid()) } == 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
pub(super) fn verify_server(pipe: &impl AsRawHandle) -> io::Result<()> {
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 || pid == 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let server = User::read(process.as_raw_handle())?;
    let client = User::read(unsafe { GetCurrentProcess() })?;
    // Session IDs deliberately need not match: scheduled per-user services can
    // run in another logon session while retaining the correct user SID.
    require_same_user(&server, &client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn owned_pipe_is_verified_before_any_payload_and_query_failure_denies()
    -> anyhow::Result<()> {
        let address = format!(r"\\.\pipe\xrun-{}", uuid::Uuid::new_v4());
        let server = super::super::pipe(&address, true)?;
        let client = tokio::net::windows::named_pipe::ClientOptions::new().open(&address)?;
        server.connect().await?;
        verify_server(&client)?;
        assert!(User::read(std::ptr::null_mut()).is_err());
        Ok(())
    }

    #[test]
    fn a_different_sid_is_rejected() -> anyhow::Result<()> {
        use windows_sys::Win32::{
            Foundation::LocalFree, Security::Authorization::ConvertStringSidToSidW,
        };
        let current = User::read(unsafe { GetCurrentProcess() })?;
        let mut different = User::read(unsafe { GetCurrentProcess() })?;
        let text: Vec<u16> = "S-1-1-0".encode_utf16().chain(Some(0)).collect();
        let mut sid = std::ptr::null_mut();
        assert_ne!(
            unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) },
            0
        );
        unsafe {
            (*(different.0.as_mut_ptr().cast::<TOKEN_USER>())).User.Sid = sid;
        }
        let result = require_same_user(&different, &current);
        unsafe {
            LocalFree(sid);
        }
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        Ok(())
    }
}
