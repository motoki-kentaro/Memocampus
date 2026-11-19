//! One tray per application directory (Issue #67).
//!
//! A named mutex `Local\Memocampus.Tray.<hash of the application directory>`. `Local\` is the
//! Windows session namespace: two sessions of one user (fast user switching, a remote session) each
//! get a tray, and the second one's server then meets the first one's port and reports
//! `port_in_use` instead of taking it. Different application directories (the isolated ones the
//! tests use) never collide. The mutex is only an existence marker: it is never waited on or
//! owned for locking, so nothing can block behind it, and it disappears with its last handle,
//! including when the tray is killed.

use std::path::Path;

/// FNV-1a, 64 bit: stable across Rust versions (`DefaultHasher` is not), and this is a name, not a
/// secret.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The directory as Windows compares paths: case-folded, `/` as `\`, no trailing separator.
fn normalized(app_dir: &Path) -> String {
    let text = app_dir.to_string_lossy().replace('/', "\\").to_lowercase();
    text.trim_end_matches('\\').to_owned()
}

pub fn mutex_name(app_dir: &Path) -> String {
    format!(
        "Local\\Memocampus.Tray.{:016x}",
        fnv1a(normalized(app_dir).as_bytes())
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceError {
    /// A tray for this application directory is already running in this session.
    AlreadyRunning,
    Failed,
}

#[cfg(windows)]
pub struct SingleInstance {
    _marker: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
pub fn acquire(app_dir: &Path) -> Result<SingleInstance, InstanceError> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = std::ffi::OsStr::new(&mutex_name(app_dir))
        .encode_wide()
        .chain([0])
        .collect();
    // SAFETY: a NUL-terminated name that lives for the call; default security, not initially owned.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(InstanceError::Failed);
    }
    // SAFETY: `GetLastError` straight after `CreateMutexW`; the handle is ours either way.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // SAFETY: a handle just returned by the OS and owned by nobody else.
    let marker = unsafe { OwnedHandle::from_raw_handle(handle) };
    if existed {
        // Closing our handle to the existing mutex does not affect the first instance's.
        return Err(InstanceError::AlreadyRunning);
    }
    Ok(SingleInstance { _marker: marker })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_name_is_in_the_session_namespace_and_fixed_for_a_directory() {
        let name = mutex_name(Path::new("C:\\Users\\a\\AppData\\Local\\Memocampus"));
        assert!(name.starts_with("Local\\Memocampus.Tray."));
        assert_eq!(name.len(), "Local\\Memocampus.Tray.".len() + 16);
        assert_eq!(
            name,
            mutex_name(Path::new("C:\\Users\\a\\AppData\\Local\\Memocampus"))
        );
    }

    #[test]
    fn spelling_of_one_directory_does_not_change_the_name() {
        let base = mutex_name(Path::new("C:\\Users\\a\\App\\Memocampus"));
        for same in [
            "c:\\users\\A\\app\\memocampus",
            "C:/Users/a/App/Memocampus",
            "C:\\Users\\a\\App\\Memocampus\\",
        ] {
            assert_eq!(mutex_name(&PathBuf::from(same)), base, "{same}");
        }
    }

    #[test]
    fn different_directories_get_different_names() {
        let a = mutex_name(Path::new("C:\\tmp\\one"));
        let b = mutex_name(Path::new("C:\\tmp\\two"));
        assert_ne!(a, b);
    }

    #[cfg(windows)]
    #[test]
    fn a_second_instance_of_the_same_directory_is_refused_and_others_are_not() {
        let unique = format!("C:\\memocampus-test-{}-{}", std::process::id(), line!());
        let first = acquire(Path::new(&unique)).expect("the first instance starts");
        assert_eq!(
            acquire(Path::new(&unique)).err(),
            Some(InstanceError::AlreadyRunning)
        );
        // The refused attempt did not release the first one's claim.
        assert_eq!(
            acquire(Path::new(&unique)).err(),
            Some(InstanceError::AlreadyRunning)
        );
        let other = acquire(Path::new(&format!("{unique}-other"))).expect("another directory");
        drop(other);
        drop(first);
        // Once the first is gone the directory can be claimed again.
        acquire(Path::new(&unique)).expect("free again");
    }
}
