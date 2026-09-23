use std::fs;
use std::path::Path;

#[cfg(target_os = "windows")]
use std::ffi::c_void;

use anyhow::{Context, Result, bail};

pub(crate) fn set_user_only_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!("failed to set user-only permissions on {}", path.display())
        })?;
    }
    #[cfg(target_os = "windows")]
    set_windows_user_only_acl(path, true)?;
    Ok(())
}

pub(crate) fn set_user_only_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
            format!("failed to set user-only permissions on {}", path.display())
        })?;
    }
    #[cfg(target_os = "windows")]
    set_windows_user_only_acl(path, false)?;
    Ok(())
}

pub(crate) fn verify_user_only_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect private Local file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "private Local state must be a regular file: {}",
            path.display()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "private Local file permissions are too broad ({mode:o}); expected user-only access"
            );
        }
    }
    #[cfg(target_os = "windows")]
    verify_windows_user_only_acl(path)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_windows_user_only_acl(path: &Path, directory: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    with_current_user_sid(|sid| {
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: if directory {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                NO_INHERITANCE
            },
            Trustee: windows_sys::Win32::Security::Authorization::TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: sid.cast(),
                ..Default::default()
            },
        };

        let mut acl = std::ptr::null_mut();
        let result = unsafe { SetEntriesInAclW(1, &entry, std::ptr::null(), &mut acl) };
        if result != ERROR_SUCCESS {
            return Err(windows_error(result)).with_context(|| {
                format!(
                    "failed to build user-only Windows ACL for {}",
                    path.display()
                )
            });
        }
        let _acl = LocalAllocation(acl.cast());
        let mut wide_path = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let result = unsafe {
            SetNamedSecurityInfoW(
                wide_path.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(windows_error(result)).with_context(|| {
                format!(
                    "failed to restrict Windows permissions on {}",
                    path.display()
                )
            });
        }
        Ok(())
    })
}

#[cfg(target_os = "windows")]
fn verify_windows_user_only_acl(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{
        GRANT_ACCESS, GetExplicitEntriesFromAclW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
        TRUSTEE_IS_SID,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, EqualSid, GetSecurityDescriptorControl, SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    with_current_user_sid(|sid| {
        let wide_path = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut dacl = std::ptr::null_mut();
        let mut security_descriptor = std::ptr::null_mut();
        let result = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut security_descriptor,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(windows_error(result)).with_context(|| {
                format!(
                    "failed to inspect Windows permissions on {}",
                    path.display()
                )
            });
        }
        let _security_descriptor = LocalAllocation(security_descriptor.cast());

        let mut control = 0u16;
        let mut revision = 0u32;
        if unsafe { GetSecurityDescriptorControl(security_descriptor, &mut control, &mut revision) }
            == 0
            || control & SE_DACL_PROTECTED == 0
        {
            bail!(
                "private Local file has broader Windows ACLs than expected: {}",
                path.display()
            );
        }

        let mut count = 0u32;
        let mut entries = std::ptr::null_mut();
        let result = unsafe { GetExplicitEntriesFromAclW(dacl, &mut count, &mut entries) };
        if result != ERROR_SUCCESS {
            return Err(windows_error(result)).with_context(|| {
                format!(
                    "failed to enumerate Windows permissions on {}",
                    path.display()
                )
            });
        }
        let _entries = LocalAllocation(entries.cast());
        if count != 1 || entries.is_null() {
            bail!(
                "private Local file has broader Windows ACLs than expected: {}",
                path.display()
            );
        }
        let entry = unsafe { &*entries };
        let trustee_sid: windows_sys::Win32::Security::PSID = entry.Trustee.ptstrName.cast();
        if entry.grfAccessMode != GRANT_ACCESS
            || entry.Trustee.TrusteeForm != TRUSTEE_IS_SID
            || entry.grfAccessPermissions & FILE_ALL_ACCESS != FILE_ALL_ACCESS
            || trustee_sid.is_null()
            || unsafe { EqualSid(trustee_sid, sid) } == 0
        {
            bail!(
                "private Local file has broader Windows ACLs than expected: {}",
                path.display()
            );
        }
        Ok(())
    })
}

#[cfg(target_os = "windows")]
fn with_current_user_sid<T>(
    f: impl FnOnce(windows_sys::Win32::Security::PSID) -> Result<T>,
) -> Result<T> {
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to open current Windows process token");
    }
    let _token = Handle(token);

    let mut bytes = 0u32;
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut bytes);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || bytes == 0 {
        return Err(error).context("failed to size current Windows user token information");
    }
    let word = std::mem::size_of::<usize>();
    let words = (bytes as usize).div_ceil(word);
    let mut buffer = vec![0usize; words];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("failed to read current Windows user token information");
    }
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    if token_user.User.Sid.is_null() {
        bail!("current Windows process token did not contain a user SID");
    }
    f(token_user.User.Sid)
}

#[cfg(target_os = "windows")]
fn windows_error(code: u32) -> std::io::Error {
    std::io::Error::from_raw_os_error(code as i32)
}

#[cfg(target_os = "windows")]
struct LocalAllocation(*mut c_void);

#[cfg(target_os = "windows")]
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(self.0);
            }
        }
    }
}

#[cfg(target_os = "windows")]
struct Handle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(target_os = "windows")]
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        set_user_only_directory, set_user_only_file, verify_user_only_file,
        verify_windows_user_only_acl,
    };

    #[test]
    fn windows_private_file_acl_is_user_only_and_verifiable() {
        let temp = tempdir().unwrap();
        let private_dir = temp.path().join("private");
        fs::create_dir(&private_dir).unwrap();
        set_user_only_directory(&private_dir).unwrap();

        let private_file = private_dir.join("secret.txt");
        fs::write(&private_file, b"secret").unwrap();
        set_user_only_file(&private_file).unwrap();
        verify_user_only_file(&private_file).unwrap();
    }

    #[test]
    fn windows_recreated_private_file_is_resecured() {
        let temp = tempdir().unwrap();
        let private_file = temp.path().join("secret.txt");
        fs::write(&private_file, b"first").unwrap();
        set_user_only_file(&private_file).unwrap();
        verify_user_only_file(&private_file).unwrap();

        fs::remove_file(&private_file).unwrap();
        fs::write(&private_file, b"replacement").unwrap();
        set_user_only_file(&private_file).unwrap();
        verify_user_only_file(&private_file).unwrap();
    }

    #[test]
    fn windows_recreated_private_directory_is_resecured() {
        let temp = tempdir().unwrap();
        let private_dir = temp.path().join("private");
        fs::create_dir(&private_dir).unwrap();
        set_user_only_directory(&private_dir).unwrap();
        verify_windows_user_only_acl(&private_dir).unwrap();

        fs::remove_dir_all(&private_dir).unwrap();
        fs::create_dir(&private_dir).unwrap();
        set_user_only_directory(&private_dir).unwrap();
        verify_windows_user_only_acl(&private_dir).unwrap();
    }
}
