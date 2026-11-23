//! Native queries for transport_auth. None reads or writes application bytes.
use super::auth::{self, Query};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::{io, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, LocalFree},
    Security::{
        Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT},
        EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType},
    System::{
        Pipes::{GetNamedPipeInfo, PIPE_SERVER_END},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

fn failed(api: &str) -> io::Error {
    io::Error::other(format!("{api}: {}", io::Error::last_os_error()))
}

/// Owns the aligned TOKEN_USER buffer, including its SID; no pointer outlives this allocation.
pub(super) struct User(Vec<usize>);
impl User {
    pub(super) fn current() -> io::Result<Self> {
        // SAFETY: GetCurrentProcess returns a valid pseudo handle, not an owned handle to close.
        Self::of_process(unsafe { GetCurrentProcess() })
    }
    fn of_process(process: HANDLE) -> io::Result<Self> {
        // SAFETY: process is a live handle held by the caller; out pointers and lengths are valid.
        // The owned token is closed on every return, after copying TokenUser into our own buffer.
        unsafe {
            let mut token = ptr::null_mut();
            if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
                return Err(failed("OpenProcessToken"));
            }
            let token = OwnedHandle::from_raw_handle(token);
            let mut needed = 0;
            let result = GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut needed,
            );
            let error = io::Error::last_os_error();
            if result != 0 || error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
                return Err(io::Error::other(format!(
                    "GetTokenInformation size query: {error}"
                )));
            }
            if needed < std::mem::size_of::<TOKEN_USER>() as u32 {
                return Err(io::Error::other(
                    "GetTokenInformation returned a short TokenUser size",
                ));
            }
            let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
            if GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            ) == 0
            {
                return Err(failed("GetTokenInformation(TokenUser)"));
            }
            Ok(Self(buffer))
        }
    }
    pub(super) fn sid(&self) -> PSID {
        // SAFETY: this is the aligned successful TokenUser result; Windows puts its SID in the same buffer.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

/// GetSecurityInfo allocates this descriptor; its owner SID points into the same allocation.
struct Owner {
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    sid: PSID,
}
impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: GetSecurityInfo returned this allocation; the owner SID is never used after drop.
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

struct NativeQuery(HANDLE);
impl Query for NativeQuery {
    type Owner = Owner;
    type User = User;
    fn file_is_pipe(&self) -> io::Result<bool> {
        // SAFETY: the caller retains the opened stream for this entire verification.
        Ok(unsafe { GetFileType(self.0) } == FILE_TYPE_PIPE)
    }
    fn pipe_is_client_end(&self) -> io::Result<bool> {
        let mut flags = 0;
        // SAFETY: the retained pipe handle and flags out pointer are valid; unused outputs may be null.
        if unsafe {
            GetNamedPipeInfo(
                self.0,
                &mut flags,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(failed("GetNamedPipeInfo"));
        }
        Ok(flags & PIPE_SERVER_END == 0)
    }
    fn pipe_owner(&self) -> io::Result<Owner> {
        let mut sid = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        // SAFETY: the caller retains the connected handle. GetSecurityInfo returns an allocated
        // descriptor containing the SID; unused output pointers may be null. It returns an error
        // code directly, not through GetLastError. READ_CONTROL comes from the read/write open.
        let status = unsafe {
            GetSecurityInfo(
                self.0,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut sid,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "GetSecurityInfo(owner): {}",
                io::Error::from_raw_os_error(status as i32)
            )));
        }
        let owner = Owner { descriptor, sid };
        if owner.descriptor.is_null() || owner.sid.is_null() {
            return Err(io::Error::other("GetSecurityInfo returned no pipe owner"));
        }
        Ok(owner)
    }
    fn current_user(&self) -> io::Result<User> {
        User::current()
    }
    fn owner_is_user(&self, owner: &Owner, current: &User) -> io::Result<bool> {
        // SAFETY: the descriptor and TokenUser buffer own both valid SIDs throughout EqualSid.
        Ok(unsafe { EqualSid(owner.sid, current.sid()) } != 0)
    }
}

pub(super) fn authenticate<S: AsRawHandle>(stream: S, endpoint: &Path) -> io::Result<S> {
    let query = NativeQuery(stream.as_raw_handle());
    auth::checked(stream, endpoint, &query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_auth_refuses_regular_file_without_altering_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-pipe");
        std::fs::write(&path, b"original protected bytes").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let error = authenticate(file, &path).unwrap_err();
        assert!(auth::is_refusal(&error));
        assert!(error.to_string().contains("not-a-pipe"));
        assert_eq!(std::fs::read(path).unwrap(), b"original protected bytes");
    }

    #[tokio::test]
    async fn server_end_is_not_an_authenticated_client_connection() {
        let endpoint: std::path::PathBuf = format!(
            r"\\.\pipe\vqro-auth-test-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap()
        )
        .into();
        let server = super::super::windows::instance(&endpoint, true).unwrap();
        let error = authenticate(server, &endpoint).unwrap_err();
        assert!(auth::is_refusal(&error));
        assert!(error.to_string().contains("client end"));
    }

    #[test]
    fn native_token_users_compare_equal() {
        let first = User::current().unwrap();
        let second = User::current().unwrap();
        // SAFETY: both owned TokenUser buffers are live and contain valid SIDs.
        assert_ne!(unsafe { EqualSid(first.sid(), second.sid()) }, 0);
    }
}
