//! The file side of a Page export. Windows gives no handle from the Save dialog, so identity is
//! pinned with handles taken immediately after it returns:
//!
//! - the folder is opened without `FILE_SHARE_DELETE` and addressed by its resolved real path, so
//!   it (and every ancestor) cannot be renamed, removed or re-pointed through a junction;
//! - a file that already has the chosen name is opened with `DELETE` access and without
//!   `FILE_SHARE_WRITE`/`FILE_SHARE_DELETE`: from then on nobody else can change, remove, rename
//!   or replace it, and every later step acts on that handle, never on the path;
//! - the temporary file is created exclusively under a random name, marked delete-on-close while
//!   it is written (a killed host leaves no plaintext behind), and renamed by handle onto the
//!   final name only if that name is free.
//!
//! Replacing: the pinned old file is renamed aside by handle, the new one is renamed in (never
//! replacing anything), verified by reading the final path, and only then is the old one deleted.
//! Any failure puts the old file back; if that is impossible it stays beside the target as
//! `<name>.<id>.memocampus-old`. Nothing is ever overwritten by path.
use super::{fault, reserved};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub type Result<T> = std::result::Result<T, &'static str>;

pub struct Destination {
    _folder: File,
    folder: PathBuf,
    name: OsString,
    existing: Option<File>,
}
pub struct Temp {
    file: Option<File>,
    path: PathBuf,
    installed: bool,
}
pub struct Outcome {
    pub file_name: String,
    /// The new file is complete and verified, but the replaced one could not be removed and is
    /// still beside it as `*.memocampus-old`.
    pub previous_left: bool,
}

/// The dialog normally returns none of these. Checked because the real path is a verbatim one
/// (`\\?\`), which Windows does not normalise: `a.` or `NUL.mhtml` would really be created.
fn valid_name(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        !name.is_empty()
            && name.encode_utf16().count() <= 255
            && !name.ends_with(['.', ' '])
            && !name.chars().any(|c| {
                c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            })
            && !reserved(name)
    })
}
fn code(error: &io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::AlreadyExists {
        "export_file_appeared"
    } else {
        "export_failed"
    }
}
fn hex_digest(file: &mut impl Read) -> io::Result<(u64, String)> {
    let mut buffer = zeroize::Zeroizing::new(vec![0u8; crate::pages::CHUNK_BYTES]);
    let (mut hash, mut total) = (Sha256::new(), 0u64);
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            return Ok((total, format!("{:x}", hash.finalize())));
        }
        total += n as u64;
        hash.update(&buffer[..n]);
    }
}

impl Destination {
    pub fn observe(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .filter(|name| valid_name(name))
            .ok_or("export_invalid_target")?;
        let parent = path
            .parent()
            .filter(|parent| parent.is_absolute())
            .ok_or("export_invalid_target")?;
        let handle = sys::open_folder(parent).map_err(|_| "parent_missing")?;
        if !handle.metadata().is_ok_and(|meta| meta.is_dir()) {
            return Err("parent_missing");
        }
        let folder = sys::real_path(&handle, parent).map_err(|_| "export_failed")?;
        let target = folder.join(name);
        let existing = match sys::open_pinned(&target) {
            Ok(file) => {
                let meta = file.metadata().map_err(|_| "export_failed")?;
                if meta.is_dir() || sys::reparse_point(&meta) {
                    return Err("export_invalid_target");
                }
                if meta.permissions().readonly() {
                    return Err("export_target_readonly");
                }
                Some(file)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) if sys::in_use(&error) => return Err("export_target_busy"),
            Err(_) => return Err("export_failed"),
        };
        Ok(Self {
            _folder: handle,
            folder,
            name: name.to_owned(),
            existing,
        })
    }
    /// Exclusive, unpredictable (122 random bits) and never following an existing name.
    pub fn create_temp(&self) -> Result<Temp> {
        let path = self
            .folder
            .join(format!(".memocampus-{}.tmp", uuid::Uuid::new_v4().simple()));
        if fault("temp", 0) {
            return Err("export_failed");
        }
        let file = sys::create_exclusive(&path).map_err(|_| "export_failed")?;
        let temp = Temp {
            file: Some(file),
            path,
            installed: false,
        };
        // Gone when the handle closes, whatever happens to this process.
        sys::delete_on_close(temp.file(), true).map_err(|_| "export_failed")?;
        Ok(temp)
    }
    /// Puts the completely written temporary file at the final name and verifies the file found
    /// there. On any error the final name holds what it held before (or see the module comment).
    pub fn install(&self, mut temp: Temp, sha256: &str, length: u64) -> Result<Outcome> {
        let target = self.folder.join(&self.name);
        temp.file().sync_all().map_err(|_| "export_failed")?;
        if fault("finalize", 0) || sys::delete_on_close(temp.file(), false).is_err() {
            return Err("export_failed");
        }
        let aside = self.existing.as_ref().map(|_| {
            let id = uuid::Uuid::new_v4().simple().to_string();
            let mut name = self.name.clone();
            name.push(format!(".{}.memocampus-old", &id[..12]));
            self.folder.join(name)
        });
        if let (Some(old), Some(aside)) = (&self.existing, &aside) {
            sys::rename(old, aside).map_err(|_| "export_failed")?;
        }
        // From here an error must first put the pinned old file back.
        let restore = |error: &'static str| match (&self.existing, &aside) {
            (Some(old), Some(_)) if sys::rename(old, &target).is_err() => "export_recovery_needed",
            _ => error,
        };
        let moved = if fault("install", 0) {
            Err(io::Error::other("injected"))
        } else {
            sys::rename(temp.file(), &target)
        };
        if let Err(error) = moved {
            return Err(restore(code(&error)));
        }
        // The file at the final path, read back: it must be the one this handle holds (which
        // nobody else can write) and carry the original's length and SHA-256.
        let verified = !fault("verify", 0)
            && sys::real_path(temp.file(), &target).is_ok_and(|path| path == target)
            && sys::open_shared(&target)
                .and_then(|mut file| hex_digest(&mut file))
                .is_ok_and(|(total, hash)| total == length && hash == sha256);
        if !verified {
            // Remove the unverified file (ours, by handle), then put the old one back.
            let removed = sys::delete_on_close(temp.file(), true).is_ok();
            temp.file = None;
            return Err(if removed {
                restore("export_verify_failed")
            } else {
                "export_recovery_needed"
            });
        }
        temp.installed = true;
        let previous_left = self
            .existing
            .as_ref()
            .is_some_and(|old| fault("discard", 0) || sys::delete_on_close(old, true).is_err());
        Ok(Outcome {
            file_name: self.name.to_string_lossy().into_owned(),
            previous_left,
        })
    }
}

impl Temp {
    fn file(&self) -> &File {
        self.file.as_ref().expect("open until dropped")
    }
}
impl Write for Temp {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.as_mut().expect("open until dropped").write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for Temp {
    /// Removes only the file this export created, and only while it is not the final file.
    fn drop(&mut self) {
        if self.installed {
            return;
        }
        if let Some(file) = self.file.take() {
            if sys::delete_on_close(&file, true).is_err() {
                drop(file);
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::{
        ffi::OsString,
        fs::{File, Metadata, OpenOptions},
        io,
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        },
        path::{Path, PathBuf},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, FileRenameInfo, GetFinalPathNameByHandleW, SetFileInformationByHandle,
        DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
        FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
    };

    /// Others may still list it and create files in it; none may rename or delete it.
    pub fn open_folder(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | SYNCHRONIZE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }
    /// The file that has the chosen name now (the link itself if it is one). Others may read it.
    pub fn open_pinned(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .access_mode(DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    pub fn create_exclusive(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    /// Read-only view of the final path while the writing handle is still held.
    pub fn open_shared(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    pub fn reparse_point(meta: &Metadata) -> bool {
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    /// ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION: another program has the file open.
    pub fn in_use(error: &io::Error) -> bool {
        matches!(error.raw_os_error(), Some(32 | 33))
    }
    /// The path with every link and junction resolved (`\\?\C:\…`).
    pub fn real_path(file: &File, _hint: &Path) -> io::Result<PathBuf> {
        let mut buffer = vec![0u16; 512];
        loop {
            // SAFETY: the buffer is valid for the length passed; the handle is open.
            let len = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle(),
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    0,
                )
            } as usize;
            if len == 0 {
                return Err(io::Error::last_os_error());
            }
            if len < buffer.len() {
                return Ok(PathBuf::from(OsString::from_wide(&buffer[..len])));
            }
            buffer.resize(len + 1, 0);
        }
    }
    /// Renames the open file itself. Never replaces: an existing `to` fails with `AlreadyExists`.
    pub fn rename(file: &File, to: &Path) -> io::Result<()> {
        let name: Vec<u16> = to.as_os_str().encode_wide().collect();
        let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        let size = offset + (name.len() + 1) * 2;
        // Zeroed: ReplaceIfExists = FALSE, RootDirectory = null, NUL-terminated name. u64 units
        // give the alignment FILE_RENAME_INFO needs.
        let mut buffer = vec![0u64; size.div_ceil(8)];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: `buffer` is at least `size` bytes and aligned; the name fits after `offset`.
        let done = unsafe {
            (&raw mut (*info).FileNameLength).write((name.len() * 2) as u32);
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                info.cast::<u8>().add(offset).cast::<u16>(),
                name.len(),
            );
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfo,
                info.cast(),
                size as u32,
            )
        };
        if done == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    /// Sets or clears "delete when the last handle closes" on the open file itself.
    pub fn delete_on_close(file: &File, delete: bool) -> io::Result<()> {
        let info = FILE_DISPOSITION_INFO { DeleteFile: delete };
        // SAFETY: `info` is a valid FILE_DISPOSITION_INFO of the size passed.
        let done = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&raw const info).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if done == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// The product ships for Windows only. Without handle pinning the export is refused, not weakened.
#[cfg(not(windows))]
mod sys {
    use std::{
        fs::{File, Metadata, OpenOptions},
        io,
        path::{Path, PathBuf},
    };
    pub fn open_folder(path: &Path) -> io::Result<File> {
        File::open(path)
    }
    pub fn open_pinned(_path: &Path) -> io::Result<File> {
        Err(io::Error::other("unsupported"))
    }
    pub fn create_exclusive(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
    }
    pub fn open_shared(path: &Path) -> io::Result<File> {
        File::open(path)
    }
    pub fn reparse_point(meta: &Metadata) -> bool {
        meta.file_type().is_symlink()
    }
    pub fn in_use(_error: &io::Error) -> bool {
        false
    }
    pub fn real_path(_file: &File, hint: &Path) -> io::Result<PathBuf> {
        hint.canonicalize()
    }
    pub fn rename(_file: &File, _to: &Path) -> io::Result<()> {
        Err(io::Error::other("unsupported"))
    }
    pub fn delete_on_close(_file: &File, _delete: bool) -> io::Result<()> {
        Err(io::Error::other("unsupported"))
    }
}

#[cfg(test)]
pub fn tests_valid_name(name: &str) -> bool {
    valid_name(OsStr::new(name))
}
