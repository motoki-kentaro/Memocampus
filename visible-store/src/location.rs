//! Vault location: only a file on a fixed local drive is accepted. This is an allowlist because
//! SQLite's locks cannot be trusted on a network file, and "not recognizably remote" is no proof.

use crate::VisibleError;
use std::path::{Component, Path, PathBuf, Prefix};

/// Where the final path of a file lies, after junctions, symbolic links and drive mappings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Volume {
    Fixed,
    Other,
}

/// Resolves `path` to its final form, which is then the only spelling used to open the vault.
pub(crate) fn resolve_local(path: &Path) -> Result<PathBuf, VisibleError> {
    if !path.is_absolute() {
        return Err(VisibleError::VaultUnavailable);
    }
    // Refused before any file access, so a read never touches a network path.
    if is_unc(path) {
        return Err(VisibleError::VaultUnsupported);
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| VisibleError::VaultUnavailable)?;
    match volume_of(&canonical) {
        Volume::Fixed => Ok(canonical),
        Volume::Other => Err(VisibleError::VaultUnsupported),
    }
}

pub(crate) fn is_unc(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(
                prefix.kind(),
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(..)
            )
    )
}

#[cfg(windows)]
fn volume_of(canonical: &Path) -> Volume {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    let Some(Component::Prefix(prefix)) = canonical.components().next() else {
        return Volume::Other;
    };
    let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = prefix.kind() else {
        return Volume::Other;
    };
    let root: Vec<u16> = std::ffi::OsString::from(format!("{}:\\", letter as char))
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: `root` is a valid NUL-terminated UTF-16 string that outlives the call.
    volume_of_drive_type(unsafe { GetDriveTypeW(root.as_ptr()) })
}

/// Unknown, removable, remote, CD-ROM and RAM disk are all refused.
#[cfg(windows)]
pub(crate) fn volume_of_drive_type(drive_type: u32) -> Volume {
    // Winbase.h; windows-sys defines it in a module this package has no other use for.
    const DRIVE_FIXED: u32 = 3;
    if drive_type == DRIVE_FIXED {
        Volume::Fixed
    } else {
        Volume::Other
    }
}

/// Memocampus runs on Windows; no local filesystem check exists elsewhere.
#[cfg(not(windows))]
fn volume_of(_: &Path) -> Volume {
    Volume::Other
}
