use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct SecurityDescriptor(*mut c_void);

impl SecurityDescriptor {
    fn for_current_user(directory: bool) -> io::Result<Self> {
        let sid = current_user_sid()?;
        let inheritance = if directory { "OICI" } else { "" };
        let sddl: Vec<u16> = format!("D:P(A;{inheritance};FA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = null_mut();
        // P protects this DACL from broader permissions inherited from the parent.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // The conversion API allocates descriptors with LocalAlloc.
        unsafe { LocalFree(self.0) };
    }
}

fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // A successful OpenProcessToken transfers one owned handle to us.
    let _token_owner = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut length = 0;
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    // TOKEN_USER contains pointers; allocate pointer-aligned storage for the API.
    let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid_string = null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_string) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe {
        let mut count = 0;
        while *sid_string.add(count) != 0 {
            count += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_string, count));
        LocalFree(sid_string.cast());
        sid
    };
    Ok(sid)
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut path: Vec<u16> = path.as_os_str().encode_wide().collect();
    if path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    path.push(0);
    Ok(path)
}

fn open_file(path: &Path, disposition: u32) -> io::Result<File> {
    let path = wide_path(path)?;
    let descriptor = SecurityDescriptor::for_current_user(false)?;
    let attributes = descriptor.attributes();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // File owns and closes the newly created, non-inheritable handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub fn create_new(path: &Path) -> io::Result<File> {
    open_file(path, CREATE_NEW)
}

pub fn open_lock(path: &Path) -> io::Result<File> {
    open_file(path, OPEN_ALWAYS)
}

pub fn env_names_equal(a: &str, b: &str) -> io::Result<bool> {
    let a: Vec<u16> = a.encode_utf16().chain(Some(0)).collect();
    let b: Vec<u16> = b.encode_utf16().chain(Some(0)).collect();
    // Match std::process::Command's Windows environment comparison, including Unicode.
    let result = unsafe { CompareStringOrdinal(a.as_ptr(), -1, b.as_ptr(), -1, 1) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result == CSTR_EQUAL)
    }
}

pub fn create_dir_all(path: &Path) -> io::Result<()> {
    match path.metadata() {
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    let wide = wide_path(path)?;
    let descriptor = SecurityDescriptor::for_current_user(true)?;
    let attributes = descriptor.attributes();
    if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorControl,
        SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    struct TestDir(std::path::PathBuf);

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn assert_current_user_only(path: &Path) {
        let wide = wide_path(path).unwrap();
        let mut acl = null_mut();
        let mut descriptor = null_mut();
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut acl,
                null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, 0, "cannot inspect created object's DACL");
        let descriptor = SecurityDescriptor(descriptor);
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) },
            0
        );
        assert_ne!(
            control & SE_DACL_PROTECTED,
            0,
            "must block inherited broader grants"
        );
        assert!(!acl.is_null(), "null DACL allows everyone");
        assert_eq!(
            unsafe { (*acl).AceCount },
            1,
            "only one principal may be granted access"
        );
        let mut ace = null_mut();
        assert_ne!(unsafe { GetAce(acl, 0, &mut ace) }, 0);
        let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        assert_eq!(ace.Header.AceType, 0, "expected an access-allowed ACE");
        assert_eq!(ace.Mask, FILE_ALL_ACCESS);
        let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
        let mut sid_string = null_mut();
        assert_ne!(unsafe { ConvertSidToStringSidW(sid, &mut sid_string) }, 0);
        let actual = unsafe {
            let mut length = 0;
            while *sid_string.add(length) != 0 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(sid_string, length));
            LocalFree(sid_string.cast());
            text
        };
        assert_eq!(actual, current_user_sid().unwrap());
    }

    #[test]
    fn new_directory_key_and_lock_have_protected_current_user_dacl() {
        let test_dir = TestDir(std::env::temp_dir().join(format!(
            "molso-dacl-test-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        )));
        create_dir_all(&test_dir.0).unwrap();
        let key = test_dir.0.join("key");
        drop(create_new(&key).unwrap());
        let lock = test_dir.0.join("vault.lock");
        drop(open_lock(&lock).unwrap());
        for path in [&test_dir.0, &key, &lock] {
            assert_current_user_only(path);
        }
    }
}
