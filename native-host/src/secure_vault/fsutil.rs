//! Small file and encoding helpers shared by the key store, the sidecar and the host.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 15)] as char);
    }
    out
}

/// Lower-case hexadecimal only (as `hex` writes it); anything else is `None`.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2)
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// `path` with `suffix` appended to its file name (`a.sqlite3` + `.mckey`).
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Reads a small regular file, refusing anything larger than `limit` bytes.
pub fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too large"));
    }
    Ok(bytes)
}

/// Writes `bytes` to a file that must not exist yet, flushed to disk. On failure the file this
/// call created is removed.
pub fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    result
}

/// Writes `bytes` under a unique temporary name beside `path`, flushes it and renames it to `path`
/// without replacing anything (`AlreadyExists` if `path` exists). Nothing is left behind on failure.
pub fn publish_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = with_suffix(path, &format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    write_new(&temp, bytes)?;
    let result = rename_no_replace(&temp, path);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Like `publish_new`, but replaces an existing `path` in one rename.
pub fn publish_replacing(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = with_suffix(path, &format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    write_new(&temp, bytes)?;
    let result = fs::rename(&temp, path);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Moves `from` to `to`, failing with `AlreadyExists` instead of replacing `to`.
#[cfg(windows)]
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<u16>>()
    };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both strings are NUL-terminated and live for the call. No
    // MOVEFILE_REPLACE_EXISTING: an existing destination makes the call fail.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_refuses_anything_else() {
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
        assert_eq!(unhex("00ab10"), Some(vec![0, 0xab, 0x10]));
        for bad in ["0", "AB", "zz", "0x00"] {
            assert_eq!(unhex(bad), None);
        }
    }

    #[test]
    fn publishing_never_replaces_unless_asked() {
        let dir = std::env::temp_dir().join(format!("mc-fsutil-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("entry.json");
        publish_new(&path, b"one").unwrap();
        assert_eq!(
            publish_new(&path, b"two").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&path).unwrap(), b"one");
        publish_replacing(&path, b"three").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"three");
        let left: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert_eq!(left.len(), 1, "no temp file is left");
        assert!(read_bounded(&path, 2).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
