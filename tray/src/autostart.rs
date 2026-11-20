//! Windows logon auto-start through the per-user Run key (Issue #67, Slice 2).
//!
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, one value `MemocampusTray` holding the
//! quoted path of this executable. No administrator rights; setting it twice is the same value (no
//! duplicate is possible); turning it off deletes that one value. Nothing else in the key is read
//! for any purpose but a status, and nothing else is ever changed.
//!
//! Windows' own startup management (Settings > Apps > Startup, Task Manager) records the user's
//! on/off choice for a Run entry in `...\Explorer\StartupApproved\Run`. That key is only **read**
//! here, to show the truth: when the user has turned the entry off there, a setting of "on" is shown
//! as "disabled in Windows" and is not overridden. The tray never writes or deletes there.
//!
//! What is verified and what is not: on the machine this was developed on, every entry in that key
//! is 12 bytes with first byte `02` (enabled) or `00` (not interpreted). A "disabled" value was not
//! available there; that it is `03` is the commonly documented behaviour and is assumed, only ever
//! for display. Only a `REG_BINARY` value of exactly 12 bytes with a known first byte and zero
//! reserved bytes is read as enabled or disabled; anything else is reported as `Unknown`.

use std::io;
use std::path::Path;

pub const VALUE_NAME: &str = "MemocampusTray";
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// What Windows' startup management says about the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// No record, or a record that says enabled.
    Enabled,
    /// The user turned it off in Windows. The tray respects that.
    DisabledByWindows,
    /// A record in a shape this build does not know. Not rewritten, not guessed.
    Unknown,
}

/// The record Windows keeps for an entry: `REG_BINARY`, exactly 12 bytes, a first byte that says
/// on/off, three zero bytes, then a time stamp. Only the forms known to be that are read as
/// enabled or disabled: `02` and `06` enabled, `03` disabled. A different length, a different type
/// (the caller passes only `REG_BINARY` data), a non-zero reserved byte or any other first byte is
/// `Unknown`: not guessed, not rewritten. (Seen on the development machine: `02`, and `00` which
/// is not interpreted.)
pub fn approval_from_bytes(bytes: Option<&[u8]>) -> Approval {
    match bytes {
        None => Approval::Enabled,
        Some(bytes) if bytes.len() == 12 && bytes[1..4] == [0, 0, 0] => match bytes[0] {
            0x02 | 0x06 => Approval::Enabled,
            0x03 => Approval::DisabledByWindows,
            _ => Approval::Unknown,
        },
        Some(_) => Approval::Unknown,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Autostart {
    /// There is no `MemocampusTray` value.
    NotRegistered,
    /// Registered for this executable.
    Registered(Approval),
    /// Registered, but for another path: the executable was moved or this is another copy. The
    /// tray says so and changes it only when the user asks.
    PathMismatch(Approval),
    /// The key could not be read.
    Unreadable,
}

/// The Run value for `exe`: its path in quotes (a path with spaces or non-ASCII letters stays one
/// token), and no arguments.
pub fn run_value(exe: &Path) -> Result<String, io::Error> {
    let text = exe
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not Unicode"))?;
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    if text.contains('"') || text.contains('\0') || !exe.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path cannot be registered",
        ));
    }
    Ok(format!("\"{text}\""))
}

fn normalized(text: &str) -> String {
    text.trim()
        .trim_matches('"')
        .replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .to_lowercase()
}

/// Whether a stored Run value names `exe`, however the case or slashes are spelled.
pub fn names_path(value: &str, exe: &Path) -> bool {
    normalized(value) == normalized(&exe.to_string_lossy())
}

/// The registry, as far as this module needs it. `WinRegistry` is the real one; tests also use an
/// isolated key and a fake.
pub trait RunRegistry {
    fn run_value(&self) -> io::Result<Option<String>>;
    fn set_run_value(&self, value: &str) -> io::Result<()>;
    fn delete_run_value(&self) -> io::Result<()>;
    fn approved_bytes(&self) -> io::Result<Option<Vec<u8>>>;
}

pub fn status(registry: &dyn RunRegistry, exe: &Path) -> Autostart {
    let Ok(value) = registry.run_value() else {
        return Autostart::Unreadable;
    };
    let Some(value) = value else {
        return Autostart::NotRegistered;
    };
    let approval = match registry.approved_bytes() {
        Ok(bytes) => approval_from_bytes(bytes.as_deref()),
        Err(_) => Approval::Unknown,
    };
    if names_path(&value, exe) {
        Autostart::Registered(approval)
    } else {
        Autostart::PathMismatch(approval)
    }
}

/// Registers `exe`, and reads it back: the result is the status afterwards, not an assumption.
pub fn enable(registry: &dyn RunRegistry, exe: &Path) -> io::Result<Autostart> {
    let value = run_value(exe)?;
    registry.set_run_value(&value)?;
    let after = status(registry, exe);
    match after {
        Autostart::Registered(_) => Ok(after),
        _ => Err(io::Error::other("the Run value did not read back")),
    }
}

/// Removes the tray's value (nothing else), and reads back that it is gone.
pub fn disable(registry: &dyn RunRegistry, exe: &Path) -> io::Result<Autostart> {
    registry.delete_run_value()?;
    match status(registry, exe) {
        Autostart::NotRegistered => Ok(Autostart::NotRegistered),
        _ => Err(io::Error::other("the Run value is still there")),
    }
}

#[cfg(windows)]
pub use win::WinRegistry;

#[cfg(windows)]
mod win {
    use super::{RunRegistry, APPROVED_KEY, RUN_KEY, VALUE_NAME};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_KEY_DELETED, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW,
        RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        KEY_SET_VALUE, REG_BINARY, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain([0])
            .collect()
    }

    /// The current user's Run key, or, for tests, a private copy of the same two keys under
    /// `HKCU\Software\MemocampusTrayTest\<id>` that `remove_isolated` deletes again.
    #[derive(Debug, Clone)]
    pub struct WinRegistry {
        run_key: String,
        approved_key: String,
        isolated_root: Option<String>,
        /// Whether this handle may write the Run value. Only the product handle and an isolated
        /// one may; the check is in the write path itself, not at its callers (S2-01).
        writable: bool,
    }

    const TEST_ROOT: &str = r"Software\MemocampusTrayTest";

    impl WinRegistry {
        /// The real Run key of the current user, **read-only**: for showing the status and for
        /// tests that check the real key did not change. Every write through it fails.
        pub fn user() -> Self {
            Self {
                run_key: RUN_KEY.into(),
                approved_key: APPROVED_KEY.into(),
                isolated_root: None,
                writable: false,
            }
        }

        /// The real Run key of the current user, writable. Only a product start (`RunMode::Product`)
        /// is given this.
        pub fn product() -> Self {
            Self {
                writable: true,
                ..Self::user()
            }
        }

        /// The registry a run of this mode gets.
        pub fn for_mode(mode: &crate::mode::RunMode) -> io::Result<Self> {
            match mode {
                crate::mode::RunMode::Product => Ok(Self::product()),
                crate::mode::RunMode::Isolated(id) => Self::isolated(id),
                crate::mode::RunMode::ReadOnly => Ok(Self::user()),
            }
        }

        pub fn is_writable(&self) -> bool {
            self.writable
        }

        /// A private pair of keys: nothing under `Software\Microsoft` is touched. `id` is letters,
        /// digits and `-` only.
        pub fn isolated(id: &str) -> io::Result<Self> {
            let valid = !id.is_empty()
                && id.len() <= 64
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
            if !valid {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "bad isolated id",
                ));
            }
            let root = format!(r"{TEST_ROOT}\{id}");
            Ok(Self {
                run_key: format!(r"{root}\Run"),
                approved_key: format!(r"{root}\StartupApproved\Run"),
                isolated_root: Some(root),
                writable: true,
            })
        }

        pub fn is_isolated(&self) -> bool {
            self.isolated_root.is_some()
        }

        /// Deletes the private keys. Refuses on the real registry.
        pub fn remove_isolated(&self) -> io::Result<()> {
            let Some(root) = &self.isolated_root else {
                return Err(io::Error::other("not an isolated registry"));
            };
            debug_assert!(root.starts_with(TEST_ROOT));
            let name = wide(root);
            // SAFETY: a NUL-terminated name under HKCU that lives for the call.
            let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, name.as_ptr()) };
            if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
                // The shared parent goes too, but only if nothing is left in it (a plain delete of
                // a key fails while it has subkeys, e.g. another test's).
                let parent = wide(TEST_ROOT);
                // SAFETY: as above.
                unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, parent.as_ptr()) };
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(status as i32))
            }
        }

        /// Writes a raw value into the Run key under another name: for tests that need "another
        /// application's entry". Isolated registries only.
        pub fn put_other(&self, name: &str, data: &str) -> io::Result<()> {
            assert!(self.is_isolated(), "only in an isolated registry");
            self.set(&self.run_key, name, data)
        }

        pub fn other(&self, name: &str) -> io::Result<Option<String>> {
            self.get_string(&self.run_key, name)
        }

        /// Writes the StartupApproved record: for tests that stand in for the user's choice in
        /// Windows. Isolated registries only.
        pub fn put_approved(&self, bytes: &[u8]) -> io::Result<()> {
            assert!(self.is_isolated(), "only in an isolated registry");
            self.require_writable()?;
            let key = self.create(&self.approved_key)?;
            let name = wide(VALUE_NAME);
            // SAFETY: an open key, a NUL-terminated name and a live buffer of the length passed.
            let status = unsafe {
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    REG_BINARY,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                )
            };
            check(status)
        }

        /// Writes a `REG_SZ` record where Windows keeps a binary one: for tests of an unexpected
        /// type. Isolated registries only.
        pub fn put_approved_text(&self, text: &str) -> io::Result<()> {
            assert!(self.is_isolated(), "only in an isolated registry");
            self.set(&self.approved_key, VALUE_NAME, text)
        }

        fn require_writable(&self) -> io::Result<()> {
            if self.writable {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "this run may not write the Run key",
                ))
            }
        }

        fn create(&self, path: &str) -> io::Result<Key> {
            let name = wide(path);
            // Another isolated registry may be removing the shared `MemocampusTrayTest` parent at
            // this moment: a key that is "marked for deletion" (1018) is gone a moment later, and
            // creating it again then works. Bounded; only isolated registries have a shared parent.
            let attempts = if self.isolated_root.is_some() { 50 } else { 1 };
            let mut status = ERROR_SUCCESS;
            let mut key: HKEY = std::ptr::null_mut();
            for attempt in 0..attempts {
                key = std::ptr::null_mut();
                // SAFETY: a NUL-terminated path, live out-pointers; `Key` closes the handle.
                status = unsafe {
                    RegCreateKeyExW(
                        HKEY_CURRENT_USER,
                        name.as_ptr(),
                        0,
                        std::ptr::null(),
                        REG_OPTION_NON_VOLATILE,
                        KEY_SET_VALUE | KEY_READ,
                        std::ptr::null(),
                        &mut key,
                        std::ptr::null_mut(),
                    )
                };
                if status != ERROR_KEY_DELETED || attempt + 1 == attempts {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            check(status)?;
            Ok(Key(key))
        }

        fn open(&self, path: &str, access: u32) -> io::Result<Option<Key>> {
            let name = wide(path);
            let mut key: HKEY = std::ptr::null_mut();
            // SAFETY: a NUL-terminated path and a live out-pointer.
            let status =
                unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, access, &mut key) };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(status)?;
            Ok(Some(Key(key)))
        }

        fn set(&self, path: &str, name: &str, data: &str) -> io::Result<()> {
            self.require_writable()?;
            let key = self.create(path)?;
            let (name, data) = (wide(name), wide(data));
            // SAFETY: an open key; NUL-terminated name and data; the byte length counts the NUL.
            let status = unsafe {
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr().cast(),
                    (data.len() * 2) as u32,
                )
            };
            check(status)
        }

        fn query(&self, path: &str, name: &str) -> io::Result<Option<(u32, Vec<u8>)>> {
            let Some(key) = self.open(path, KEY_READ)? else {
                return Ok(None);
            };
            let name = wide(name);
            let mut size = 0u32;
            let mut kind = 0u32;
            // SAFETY: the documented size query: no buffer, live out-pointers.
            let status = unsafe {
                RegQueryValueExW(
                    key.0,
                    name.as_ptr(),
                    std::ptr::null(),
                    &mut kind,
                    std::ptr::null_mut(),
                    &mut size,
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(status)?;
            let mut buffer = vec![0u8; size as usize];
            // SAFETY: `buffer` has `size` bytes, as the query said.
            let status = unsafe {
                RegQueryValueExW(
                    key.0,
                    name.as_ptr(),
                    std::ptr::null(),
                    &mut kind,
                    buffer.as_mut_ptr(),
                    &mut size,
                )
            };
            check(status)?;
            buffer.truncate(size as usize);
            Ok(Some((kind, buffer)))
        }

        fn get_string(&self, path: &str, name: &str) -> io::Result<Option<String>> {
            match self.query(path, name)? {
                None => Ok(None),
                Some((kind, bytes)) if kind == REG_SZ => {
                    let units: Vec<u16> = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| u16::from_le_bytes(*pair))
                        .collect();
                    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                    Ok(Some(String::from_utf16_lossy(&units[..end])))
                }
                // A value of another type is not ours to interpret.
                Some(_) => Err(io::Error::other("the Run value is not a string")),
            }
        }
    }

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: an open key closed once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn check(status: u32) -> io::Result<()> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    impl RunRegistry for WinRegistry {
        fn run_value(&self) -> io::Result<Option<String>> {
            self.get_string(&self.run_key, VALUE_NAME)
        }

        fn set_run_value(&self, value: &str) -> io::Result<()> {
            self.require_writable()?;
            self.set(&self.run_key, VALUE_NAME, value)
        }

        fn delete_run_value(&self) -> io::Result<()> {
            self.require_writable()?;
            let Some(key) = self.open(&self.run_key, KEY_SET_VALUE)? else {
                return Ok(());
            };
            let name = wide(VALUE_NAME);
            // SAFETY: an open key and a NUL-terminated name: this one value only.
            let status = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            check(status)
        }

        fn approved_bytes(&self) -> io::Result<Option<Vec<u8>>> {
            match self.query(&self.approved_key, VALUE_NAME)? {
                None => Ok(None),
                Some((kind, bytes)) if kind == REG_BINARY => Ok(Some(bytes)),
                // Another type is not a record this build knows: an error, so the status is Unknown.
                Some(_) => Err(io::Error::other("the StartupApproved value is not binary")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[derive(Default)]
    struct Fake {
        value: RefCell<Option<String>>,
        approved: RefCell<Option<Vec<u8>>>,
        fail_reads: bool,
        fail_writes: bool,
        /// Writes are accepted and silently lost.
        lose_writes: bool,
    }

    impl RunRegistry for Fake {
        fn run_value(&self) -> io::Result<Option<String>> {
            if self.fail_reads {
                return Err(io::Error::other("read"));
            }
            Ok(self.value.borrow().clone())
        }
        fn set_run_value(&self, value: &str) -> io::Result<()> {
            if self.fail_writes {
                return Err(io::Error::other("write"));
            }
            if !self.lose_writes {
                *self.value.borrow_mut() = Some(value.to_owned());
            }
            Ok(())
        }
        fn delete_run_value(&self) -> io::Result<()> {
            if self.fail_writes {
                return Err(io::Error::other("delete"));
            }
            if !self.lose_writes {
                *self.value.borrow_mut() = None;
            }
            Ok(())
        }
        fn approved_bytes(&self) -> io::Result<Option<Vec<u8>>> {
            Ok(self.approved.borrow().clone())
        }
    }

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Users\ユーザー\My Apps\Memocampus\memocampus-tray.exe")
    }

    #[test]
    fn the_value_is_the_quoted_absolute_path_without_arguments() {
        assert_eq!(
            run_value(&exe()).unwrap(),
            "\"C:\\Users\\ユーザー\\My Apps\\Memocampus\\memocampus-tray.exe\""
        );
        // The extended-length prefix is not written into the value.
        assert_eq!(
            run_value(Path::new(r"\\?\C:\a b\t.exe")).unwrap(),
            "\"C:\\a b\\t.exe\""
        );
        for bad in ["relative\\t.exe", "C:\\a\"b\\t.exe"] {
            assert!(run_value(Path::new(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_stored_value_names_a_path_however_it_is_spelled() {
        let value = run_value(&exe()).unwrap();
        assert!(names_path(&value, &exe()));
        assert!(names_path(
            r#""c:/users/ユーザー/my apps/memocampus/MEMOCAMPUS-TRAY.EXE""#,
            &exe()
        ));
        assert!(!names_path(
            "\"C:\\Users\\ユーザー\\My Apps\\Other\\memocampus-tray.exe\"",
            &exe()
        ));
        // A value with arguments is not "this path".
        assert!(!names_path(&format!("{value} --x"), &exe()));
    }

    #[test]
    fn the_windows_choice_is_read_only_from_a_known_record_and_never_guessed() {
        assert_eq!(approval_from_bytes(None), Approval::Enabled);
        let record = |first: u8| {
            let mut bytes = vec![first, 0, 0, 0];
            bytes.extend([0u8; 8]);
            bytes
        };
        for enabled in [0x02, 0x06] {
            assert_eq!(
                approval_from_bytes(Some(&record(enabled))),
                Approval::Enabled
            );
        }
        assert_eq!(
            approval_from_bytes(Some(&record(0x03))),
            Approval::DisabledByWindows
        );
        // Not known to mean anything: 00 (seen, not understood), 01, 04, 05, 07 and the rest.
        for other in [0x00, 0x01, 0x04, 0x05, 0x07, 0x0a, 0x82, 0xff] {
            assert_eq!(
                approval_from_bytes(Some(&record(other))),
                Approval::Unknown,
                "{other:#x}"
            );
        }
        // A different length, whatever the first byte says.
        for length in [0usize, 1, 2, 4, 8, 11, 13, 16] {
            let mut bytes = record(0x03);
            bytes.resize(length, 0);
            assert_eq!(
                approval_from_bytes(Some(&bytes)),
                Approval::Unknown,
                "{length}"
            );
            let mut bytes = record(0x02);
            bytes.resize(length, 0);
            assert_eq!(
                approval_from_bytes(Some(&bytes)),
                Approval::Unknown,
                "{length}"
            );
        }
        // A reserved byte that is not zero: a newer layout, not read.
        for at in 1..4 {
            let mut bytes = record(0x03);
            bytes[at] = 1;
            assert_eq!(approval_from_bytes(Some(&bytes)), Approval::Unknown);
        }
        // The time stamp may be anything.
        let mut stamped = record(0x03);
        stamped[4..].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            approval_from_bytes(Some(&stamped)),
            Approval::DisabledByWindows
        );
    }

    #[test]
    fn enabling_registers_once_and_reads_back_and_disabling_removes_only_the_value() {
        let registry = Fake::default();
        assert_eq!(status(&registry, &exe()), Autostart::NotRegistered);
        assert_eq!(
            enable(&registry, &exe()).unwrap(),
            Autostart::Registered(Approval::Enabled)
        );
        // Again: the same single value, no duplicate.
        let first = registry.value.borrow().clone();
        assert_eq!(
            enable(&registry, &exe()).unwrap(),
            Autostart::Registered(Approval::Enabled)
        );
        assert_eq!(*registry.value.borrow(), first);
        assert_eq!(
            disable(&registry, &exe()).unwrap(),
            Autostart::NotRegistered
        );
        // Off when it is already off is not an error.
        assert_eq!(
            disable(&registry, &exe()).unwrap(),
            Autostart::NotRegistered
        );
    }

    #[test]
    fn a_failed_or_lost_write_is_an_error_and_never_reported_as_on() {
        let failing = Fake {
            fail_writes: true,
            ..Fake::default()
        };
        assert!(enable(&failing, &exe()).is_err());
        assert!(disable(&failing, &exe()).is_err());
        let lossy = Fake {
            lose_writes: true,
            ..Fake::default()
        };
        assert!(
            enable(&lossy, &exe()).is_err(),
            "the read-back did not show it"
        );
        let stuck = Fake {
            lose_writes: true,
            ..Fake::default()
        };
        *stuck.value.borrow_mut() = Some(run_value(&exe()).unwrap());
        assert!(disable(&stuck, &exe()).is_err(), "still there");
        let unreadable = Fake {
            fail_reads: true,
            ..Fake::default()
        };
        assert_eq!(status(&unreadable, &exe()), Autostart::Unreadable);
    }

    #[test]
    fn a_moved_executable_is_a_mismatch_and_is_not_repaired_by_status() {
        let registry = Fake::default();
        *registry.value.borrow_mut() = Some("\"D:\\old\\memocampus-tray.exe\"".into());
        assert_eq!(
            status(&registry, &exe()),
            Autostart::PathMismatch(Approval::Enabled)
        );
        assert_eq!(
            registry.value.borrow().as_deref(),
            Some("\"D:\\old\\memocampus-tray.exe\""),
            "looking does not change it"
        );
    }

    #[test]
    fn a_choice_made_in_windows_is_shown_and_is_not_undone_by_enabling() {
        let registry = Fake::default();
        *registry.approved.borrow_mut() = Some(vec![3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let after = enable(&registry, &exe()).unwrap();
        assert_eq!(after, Autostart::Registered(Approval::DisabledByWindows));
        assert_eq!(
            registry.approved.borrow().as_deref(),
            Some(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..]),
            "the record belongs to Windows and is untouched"
        );
    }
}
