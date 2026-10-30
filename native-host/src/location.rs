//! Classifies where an active vault would live.
//!
//! Only conditions Windows can report reliably are used. `NoIssueDetected` means "none of the
//! known risks were found", not "safe": unregistered sync tools cannot be detected.

use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    NoIssueDetected,
    NeedsApproval,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Assessment {
    pub verdict: Verdict,
    pub reasons: Vec<&'static str>,
}

// Win32 GetDriveTypeW results.
pub const DRIVE_UNKNOWN: u32 = 0;
pub const DRIVE_NO_ROOT_DIR: u32 = 1;
pub const DRIVE_REMOVABLE: u32 = 2;
pub const DRIVE_FIXED: u32 = 3;
pub const DRIVE_REMOTE: u32 = 4;
pub const DRIVE_CDROM: u32 = 5;
pub const DRIVE_RAMDISK: u32 = 6;

/// Observations about a location, gathered by `probe` (or supplied by tests).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    pub network_path: bool,
    pub drive_type: Option<u32>,
    pub filesystem: Option<String>,
    pub volume_label: Option<String>,
    pub in_sync_root: bool,
    pub cloud_file_attributes: bool,
    pub in_executable_dir: bool,
}

pub fn assess(facts: &Facts) -> Assessment {
    let mut rejected = Vec::new();
    let mut warnings = Vec::new();
    if facts.network_path {
        rejected.push("network_path");
    }
    if facts.in_executable_dir {
        rejected.push("executable_directory");
    }
    match facts.drive_type {
        Some(DRIVE_FIXED) => {}
        Some(DRIVE_REMOVABLE) => warnings.push("removable_drive"),
        Some(DRIVE_REMOTE) => rejected.push("network_drive"),
        Some(DRIVE_CDROM) => rejected.push("optical_drive"),
        Some(DRIVE_RAMDISK) => rejected.push("ram_disk"),
        Some(DRIVE_UNKNOWN | DRIVE_NO_ROOT_DIR) | Some(_) | None => rejected.push("unknown_drive"),
    }
    match facts
        .filesystem
        .as_deref()
        .map(str::to_ascii_uppercase)
        .as_deref()
    {
        Some("NTFS" | "REFS") => {}
        Some(_) => warnings.push("non_local_filesystem"),
        None => warnings.push("volume_info_unavailable"),
    }
    if facts
        .volume_label
        .as_deref()
        .is_some_and(|label| label.to_ascii_lowercase().contains("google drive"))
    {
        warnings.push("google_drive");
    }
    if facts.in_sync_root {
        warnings.push("sync_folder");
    }
    if facts.cloud_file_attributes {
        warnings.push("cloud_files");
    }
    let verdict = if !rejected.is_empty() {
        Verdict::Rejected
    } else if !warnings.is_empty() {
        Verdict::NeedsApproval
    } else {
        Verdict::NoIssueDetected
    };
    rejected.extend(warnings);
    Assessment {
        verdict,
        reasons: rejected,
    }
}

/// `\\server\share`, `//server/share`, `\\?\UNC\...`; checked before any filesystem call so an
/// unreachable server cannot stall the host.
pub fn is_unc(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with(r"\\?\unc\") || lower.starts_with(r"\\.\unc\") {
        return true;
    }
    let mut chars = raw.chars();
    let sep = |c: Option<char>| matches!(c, Some('\\' | '/'));
    sep(chars.next()) && sep(chars.next()) && !matches!(chars.next(), Some('?' | '.'))
}

/// Strips the `\\?\` verbatim prefix that `canonicalize` adds, keeping UNC form recognisable.
pub fn simplify(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Comparison key: verbatim prefix removed, lower case, backslashes, no trailing separator.
fn path_key(path: &Path) -> String {
    simplify(path)
        .to_string_lossy()
        .to_lowercase()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_owned()
}

/// Case-insensitive, component-wise "is `path` inside (or equal to) `root`".
pub fn is_within(path: &Path, root: &Path) -> bool {
    let (path, root) = (path_key(path), path_key(root));
    !root.is_empty() && (path == root || path.starts_with(&format!("{root}\\")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hive {
    LocalMachine,
    CurrentUser,
}

/// Read-only registry access, injected so tests never read or modify the real registry.
pub trait Registry {
    fn subkeys(&self, hive: Hive, path: &str) -> Vec<String>;
    fn string_values(&self, hive: Hive, path: &str) -> Vec<String>;
}

pub const SYNC_ROOT_MANAGER: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager";

/// Sync roots registered with the Windows Cloud Files API (OneDrive, Dropbox, Google Drive
/// mirror, iCloud, Box, ...) machine-wide (HKLM) and per user (HKCU), plus `extra` roots, with
/// duplicates removed. Each provider key lists its roots as values of `<provider>\UserSyncRoots`.
pub fn known_sync_roots(registry: &dyn Registry, extra: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut roots = extra;
    for hive in [Hive::LocalMachine, Hive::CurrentUser] {
        for provider in registry.subkeys(hive, SYNC_ROOT_MANAGER) {
            let key = format!(r"{SYNC_ROOT_MANAGER}\{provider}\UserSyncRoots");
            roots.extend(
                registry
                    .string_values(hive, &key)
                    .into_iter()
                    .map(PathBuf::from),
            );
        }
    }
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .map(|root| {
            std::fs::canonicalize(&root)
                .map(|path| simplify(&path))
                .unwrap_or(root)
        })
        .filter(|root| seen.insert(path_key(root)))
        .collect()
}

/// Gathers facts for a vault file at `target` (which may not exist yet; its parent must).
pub fn probe(target: &Path, exe_dir: Option<&Path>) -> Assessment {
    if is_unc(&target.to_string_lossy()) {
        return probe_with(target, exe_dir, &[]);
    }
    probe_with(target, exe_dir, &platform::sync_roots())
}

/// `probe` with the sync roots supplied by the caller.
pub fn probe_with(target: &Path, exe_dir: Option<&Path>, sync_roots: &[PathBuf]) -> Assessment {
    if is_unc(&target.to_string_lossy()) {
        return assess(&Facts {
            network_path: true,
            drive_type: Some(DRIVE_REMOTE),
            ..Facts::default()
        });
    }
    let Some(parent) = target.parent() else {
        return assess(&Facts::default());
    };
    let Ok(canonical) = std::fs::canonicalize(parent) else {
        return assess(&Facts::default());
    };
    let directory = simplify(&canonical);
    let mut facts = Facts {
        // Junctions, symlinks and SUBST drives resolve to their real location here.
        network_path: is_unc(&directory.to_string_lossy()),
        in_executable_dir: exe_dir
            .and_then(|dir| std::fs::canonicalize(dir).ok())
            .is_some_and(|dir| is_within(&directory, &dir)),
        in_sync_root: sync_roots.iter().any(|root| is_within(&directory, root)),
        ..Facts::default()
    };
    platform::fill(&directory, target, &mut facts);
    assess(&facts)
}

#[cfg(windows)]
mod platform {
    use super::{Facts, Hive, Registry};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW,
    };
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ,
    };

    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    const PINNED: u32 = 0x0008_0000;
    const UNPINNED: u32 = 0x0010_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    const CLOUD_ATTRIBUTES: u32 = RECALL_ON_OPEN | PINNED | UNPINNED | RECALL_ON_DATA_ACCESS;

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    fn from_wide(buffer: &[u16]) -> String {
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    pub fn fill(directory: &Path, target: &Path, facts: &mut Facts) {
        let mut root = vec![0u16; 1024];
        let path = wide(directory.as_os_str());
        if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) } != 0 {
            facts.drive_type = Some(unsafe { GetDriveTypeW(root.as_ptr()) });
            let (mut label, mut filesystem) = (vec![0u16; 261], vec![0u16; 261]);
            let ok = unsafe {
                GetVolumeInformationW(
                    root.as_ptr(),
                    label.as_mut_ptr(),
                    label.len() as u32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    filesystem.as_mut_ptr(),
                    filesystem.len() as u32,
                )
            };
            if ok != 0 {
                facts.volume_label = Some(from_wide(&label));
                facts.filesystem = Some(from_wide(&filesystem));
            }
        }
        facts.cloud_file_attributes = std::iter::once(target)
            .chain(directory.ancestors())
            .filter_map(|path| std::fs::metadata(path).ok())
            .any(|metadata| metadata.file_attributes() & CLOUD_ATTRIBUTES != 0);
    }

    /// OneDrive environment variables plus every registered sync root in HKLM and HKCU.
    pub fn sync_roots() -> Vec<PathBuf> {
        let onedrive = ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"]
            .iter()
            .filter_map(std::env::var_os)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect();
        super::known_sync_roots(&WindowsRegistry, onedrive)
    }

    struct WindowsRegistry;

    impl WindowsRegistry {
        fn with_key<T: Default>(hive: Hive, path: &str, read: impl FnOnce(HKEY) -> T) -> T {
            let root = match hive {
                Hive::LocalMachine => HKEY_LOCAL_MACHINE,
                Hive::CurrentUser => HKEY_CURRENT_USER,
            };
            let Some(key) = open_key(root, &wide(std::ffi::OsStr::new(path))) else {
                return T::default();
            };
            let value = read(key);
            unsafe { RegCloseKey(key) };
            value
        }
    }

    impl Registry for WindowsRegistry {
        fn subkeys(&self, hive: Hive, path: &str) -> Vec<String> {
            Self::with_key(hive, path, |key| {
                let mut names = Vec::new();
                for index in 0.. {
                    let mut name = vec![0u16; 512];
                    let mut length = name.len() as u32;
                    let status = unsafe {
                        RegEnumKeyExW(
                            key,
                            index,
                            name.as_mut_ptr(),
                            &mut length,
                            std::ptr::null(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    };
                    if status != 0 {
                        break;
                    }
                    names.push(from_wide(&name));
                }
                names
            })
        }

        fn string_values(&self, hive: Hive, path: &str) -> Vec<String> {
            Self::with_key(hive, path, string_values)
        }
    }

    fn open_key(parent: HKEY, sub: &[u16]) -> Option<HKEY> {
        let mut key: HKEY = std::ptr::null_mut();
        (unsafe { RegOpenKeyExW(parent, sub.as_ptr(), 0, KEY_READ, &mut key) } == 0).then_some(key)
    }

    fn string_values(key: HKEY) -> Vec<String> {
        let mut values = Vec::new();
        for index in 0.. {
            let mut name = vec![0u16; 512];
            let mut name_length = name.len() as u32;
            let mut data = vec![0u8; 4096];
            let mut data_length = data.len() as u32;
            let mut kind = 0u32;
            let status = unsafe {
                RegEnumValueW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut name_length,
                    std::ptr::null(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut data_length,
                )
            };
            if status != 0 {
                break;
            }
            if kind == REG_SZ || kind == REG_EXPAND_SZ {
                let units: Vec<u16> = data[..data_length as usize]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair))
                    .collect();
                let value = from_wide(&units);
                if !value.is_empty() {
                    values.push(value);
                }
            }
        }
        values
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Facts;
    use std::path::{Path, PathBuf};

    pub fn fill(_directory: &Path, _target: &Path, _facts: &mut Facts) {}

    pub fn sync_roots() -> Vec<PathBuf> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory registry: (hive, key path) -> subkey names / string values.
    #[derive(Default)]
    struct FakeRegistry {
        subkeys: HashMap<(Hive, String), Vec<String>>,
        values: HashMap<(Hive, String), Vec<String>>,
    }

    impl FakeRegistry {
        fn register(mut self, hive: Hive, provider: &str, root: &Path) -> Self {
            self.subkeys
                .entry((hive, SYNC_ROOT_MANAGER.to_owned()))
                .or_default()
                .push(provider.to_owned());
            self.values
                .entry((
                    hive,
                    format!(r"{SYNC_ROOT_MANAGER}\{provider}\UserSyncRoots"),
                ))
                .or_default()
                .push(root.to_string_lossy().into_owned());
            self
        }
    }

    impl Registry for FakeRegistry {
        fn subkeys(&self, hive: Hive, path: &str) -> Vec<String> {
            self.subkeys
                .get(&(hive, path.to_owned()))
                .cloned()
                .unwrap_or_default()
        }
        fn string_values(&self, hive: Hive, path: &str) -> Vec<String> {
            self.values
                .get(&(hive, path.to_owned()))
                .cloned()
                .unwrap_or_default()
        }
    }

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("memocampus-sync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_sync_root_registered_only_in_hkcu_needs_approval() {
        let dir = scratch_dir();
        let target = dir.join("v.sqlite3");
        let unregistered = probe_with(
            &target,
            None,
            &known_sync_roots(&FakeRegistry::default(), vec![]),
        );
        assert!(
            !unregistered.reasons.contains(&"sync_folder"),
            "{unregistered:?}"
        );

        for hive in [Hive::CurrentUser, Hive::LocalMachine] {
            let registry =
                FakeRegistry::default().register(hive, "Contoso!S-1-5-21-1!Personal", &dir);
            let roots = known_sync_roots(&registry, vec![]);
            let result = probe_with(&target, None, &roots);
            assert_eq!(
                result.verdict,
                Verdict::NeedsApproval,
                "{hive:?}: {result:?}"
            );
            assert!(result.reasons.contains(&"sync_folder"), "{hive:?}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sync_roots_from_both_hives_and_extras_are_deduplicated() {
        let dir = scratch_dir();
        let upper = PathBuf::from(dir.to_string_lossy().to_uppercase());
        let trailing = PathBuf::from(format!("{}\\", dir.display()));
        let other = PathBuf::from(r"Z:\NotMounted\Box");
        let registry = FakeRegistry::default()
            .register(Hive::LocalMachine, "A", &dir)
            .register(Hive::CurrentUser, "B", &upper)
            .register(Hive::CurrentUser, "C", &other);
        let roots = known_sync_roots(&registry, vec![trailing]);
        assert_eq!(roots.len(), 2, "{roots:?}");
        // Like `probe_with`: the temp path may be an alias (8.3 name, junction) of the real one.
        let canonical = simplify(&std::fs::canonicalize(&dir).unwrap());
        assert!(
            roots.iter().any(|root| is_within(&canonical, root)),
            "dir: {dir:?}, canonical: {canonical:?}, roots: {roots:?}"
        );
        assert!(
            roots.contains(&other),
            "unresolvable roots are kept as registered"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_rules_still_apply_with_registered_roots() {
        let dir = scratch_dir();
        let registry = FakeRegistry::default().register(Hive::CurrentUser, "A", &dir);
        let roots = known_sync_roots(&registry, vec![]);
        // Rejection still wins over the sync-folder warning.
        let inside_exe = probe_with(&dir.join("v.sqlite3"), Some(&dir), &roots);
        assert_eq!(inside_exe.verdict, Verdict::Rejected);
        assert!(inside_exe.reasons.contains(&"executable_directory"));
        assert!(inside_exe.reasons.contains(&"sync_folder"));
        // A sibling outside the registered root is unaffected.
        let sibling = scratch_dir();
        assert!(!probe_with(&sibling.join("v.sqlite3"), None, &roots)
            .reasons
            .contains(&"sync_folder"));
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&sibling).unwrap();
    }

    fn local() -> Facts {
        Facts {
            drive_type: Some(DRIVE_FIXED),
            filesystem: Some("NTFS".into()),
            volume_label: Some("Windows".into()),
            ..Facts::default()
        }
    }

    #[test]
    fn fixed_ntfs_outside_known_sync_roots_has_no_detected_issue() {
        assert_eq!(
            assess(&local()),
            Assessment {
                verdict: Verdict::NoIssueDetected,
                reasons: vec![]
            }
        );
        assert_eq!(
            assess(&Facts {
                filesystem: Some("ReFS".into()),
                ..local()
            })
            .verdict,
            Verdict::NoIssueDetected
        );
    }

    #[test]
    fn network_volatile_and_unknown_locations_are_rejected() {
        for (facts, reason) in [
            (
                Facts {
                    network_path: true,
                    ..local()
                },
                "network_path",
            ),
            (
                Facts {
                    drive_type: Some(DRIVE_REMOTE),
                    ..local()
                },
                "network_drive",
            ),
            (
                Facts {
                    drive_type: Some(DRIVE_CDROM),
                    ..local()
                },
                "optical_drive",
            ),
            (
                Facts {
                    drive_type: Some(DRIVE_RAMDISK),
                    ..local()
                },
                "ram_disk",
            ),
            (
                Facts {
                    drive_type: Some(DRIVE_UNKNOWN),
                    ..local()
                },
                "unknown_drive",
            ),
            (
                Facts {
                    drive_type: None,
                    ..local()
                },
                "unknown_drive",
            ),
            (
                Facts {
                    in_executable_dir: true,
                    ..local()
                },
                "executable_directory",
            ),
        ] {
            let result = assess(&facts);
            assert_eq!(result.verdict, Verdict::Rejected, "{reason}");
            assert!(
                result.reasons.contains(&reason),
                "{reason}: {:?}",
                result.reasons
            );
        }
        // Nothing observable at all is never treated as safe.
        assert_eq!(assess(&Facts::default()).verdict, Verdict::Rejected);
    }

    #[test]
    fn sync_removable_and_odd_filesystems_need_approval() {
        for (facts, reason) in [
            (
                Facts {
                    in_sync_root: true,
                    ..local()
                },
                "sync_folder",
            ),
            (
                Facts {
                    cloud_file_attributes: true,
                    ..local()
                },
                "cloud_files",
            ),
            (
                Facts {
                    volume_label: Some("Google Drive".into()),
                    ..local()
                },
                "google_drive",
            ),
            (
                Facts {
                    drive_type: Some(DRIVE_REMOVABLE),
                    ..local()
                },
                "removable_drive",
            ),
            (
                Facts {
                    filesystem: Some("exFAT".into()),
                    ..local()
                },
                "non_local_filesystem",
            ),
            (
                Facts {
                    filesystem: None,
                    ..local()
                },
                "volume_info_unavailable",
            ),
        ] {
            let result = assess(&facts);
            assert_eq!(result.verdict, Verdict::NeedsApproval, "{reason}");
            assert_eq!(result.reasons, vec![reason]);
        }
    }

    #[test]
    fn rejection_reasons_come_first_and_warnings_are_kept() {
        let result = assess(&Facts {
            drive_type: Some(DRIVE_REMOTE),
            in_sync_root: true,
            ..local()
        });
        assert_eq!(result.verdict, Verdict::Rejected);
        assert_eq!(result.reasons, vec!["network_drive", "sync_folder"]);
    }

    #[test]
    fn unc_forms_are_recognised_without_touching_the_network() {
        for path in [
            r"\\nas\share\v.sqlite3",
            "//nas/share/v.sqlite3",
            r"\\?\UNC\nas\share\v.sqlite3",
            r"\\?\unc\nas\s",
        ] {
            assert!(is_unc(path), "{path}");
        }
        for path in [
            r"C:\Users\me\v.sqlite3",
            r"\\?\C:\Users",
            r"\\.\C:\x",
            "relative",
        ] {
            assert!(!is_unc(path), "{path}");
        }
        assert_eq!(
            probe(Path::new(r"\\nas.invalid\share\v.sqlite3"), None).verdict,
            Verdict::Rejected
        );
    }

    #[test]
    fn simplify_and_containment_are_case_insensitive_and_component_wise() {
        assert_eq!(
            simplify(Path::new(r"\\?\C:\Users\Me")),
            PathBuf::from(r"C:\Users\Me")
        );
        assert_eq!(
            simplify(Path::new(r"\\?\UNC\nas\share")),
            PathBuf::from(r"\\nas\share")
        );
        assert!(is_within(
            Path::new(r"C:\Users\Me\OneDrive\Docs"),
            Path::new(r"c:\users\me\onedrive")
        ));
        assert!(is_within(
            Path::new(r"C:\Users\Me\OneDrive"),
            Path::new(r"C:\Users\Me\OneDrive\")
        ));
        assert!(!is_within(
            Path::new(r"C:\Users\Me\OneDriveBackup"),
            Path::new(r"C:\Users\Me\OneDrive")
        ));
        assert!(!is_within(Path::new(r"C:\x"), Path::new("")));
    }

    #[test]
    fn probe_rejects_the_executable_directory_and_unresolvable_parents() {
        let dir = std::env::temp_dir().join(format!("memocampus-loc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let inside = probe(&dir.join("v.sqlite3"), Some(&dir));
        assert!(
            inside.reasons.contains(&"executable_directory"),
            "{inside:?}"
        );
        assert_eq!(
            probe(&dir.join("missing").join("v.sqlite3"), None).verdict,
            Verdict::Rejected
        );
        // Real probe of a local temp dir: never rejected (it may still warn, e.g. on exFAT).
        assert_ne!(
            probe(&dir.join("v.sqlite3"), None).verdict,
            Verdict::Rejected
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
