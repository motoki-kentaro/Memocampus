//! What SQLite may write into a vault before `schema::check` has seen it (Issue #71 S3-01).
//!
//! A read-write connection recovers the file at its first read: a hot rollback journal is played
//! back into it (and deleted), and a WAL is opened and later checkpointed into it. That first read is
//! the one `schema::check` makes, so a vault of another schema would be changed before it is refused.
//! Before that read, therefore:
//!
//! - The file is pinned (`pin`): a handle that does not share delete, so until the connection is
//!   validated the path cannot be renamed, replaced or deleted, and what was checked is what SQLite
//!   reads (Windows; elsewhere residual risk R10, as for the switch fence).
//! - A plain vault's own header must say MCMP / schema 10, or it is refused with the error
//!   `schema::check` would give. A file without the SQLite header is left to SQLite's own error,
//!   which writes nothing as long as no journal or WAL is beside it.
//! - When a `-journal` or `-wal` exists, the file's page 1 and every page 1 the sidecar could put back
//!   (each page-1 record of the journal, each page-1 frame of the WAL, whether or not SQLite would end
//!   up using it) must all say MCMP / schema 10. Each SQLite transaction rewrites page 1 (its change
//!   counter), so a journal of any transaction holds the page 1 it would restore. An encrypted page is
//!   decrypted with the vault key by SQLCipher itself, from a one-page copy in a temporary directory
//!   (ciphertext only, removed at once).
//! - A sidecar that SQLite would act on but that has no journal or WAL layout is refused (Invalid,
//!   `OpenCause::Recovery`) instead of being left to SQLite, which would delete it.
//!
//! Not covered: a sidecar that appears after the check and before the first read, a few statements
//! later. Only a process that can write beside the vault can do that, and it can write the vault
//! itself just as well (the file is shared for writing with every SQLite connection).

use super::{OpenCause, OpenFailure, VaultError, VaultMode};
use memocampus_native_host::schema::{APP_ID, SCHEMA_VERSION};
use memocampus_native_host::secure_vault::{cipher, secret::VaultKey};
use rusqlite::OpenFlags;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// The page size of every encrypted vault (SQLCipher 4's default, never changed by Memocampus); an
/// encrypted file's header cannot tell it before decryption.
const CIPHER_PAGE_SIZE: usize = 4096;
const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
const WAL_MAGIC: [u32; 2] = [0x377f_0682, 0x377f_0683];

/// The vault file, held open from before the check until the connection is validated.
pub(super) struct Pin(File);

#[cfg(windows)]
pub(super) fn pin(path: &Path) -> io::Result<Pin> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    // Read access only, never created; SQLite opens with the same sharing, so it is not hindered.
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
        .map(Pin)
}

#[cfg(not(windows))]
pub(super) fn pin(path: &Path) -> io::Result<Pin> {
    OpenOptions::new().read(true).open(path).map(Pin)
}

/// What a header says, classified as `schema::check` classifies it.
#[derive(Debug, PartialEq, Eq)]
enum Head {
    Vault,
    Foreign(i64),
    Version(i64),
    /// Not a SQLite header (or too short to be one).
    Unknown,
}

fn classify(app_id: i64, version: i64) -> Head {
    if app_id != APP_ID && app_id != 0 {
        Head::Foreign(app_id)
    } else if app_id != APP_ID || version != SCHEMA_VERSION {
        Head::Version(version)
    } else {
        Head::Vault
    }
}

/// A plain page 1 (or its first 100 bytes).
fn plain_head(page: &[u8]) -> Head {
    if page.len() < 100 || page[..16] != *cipher::SQLITE_MAGIC {
        return Head::Unknown;
    }
    let int = |at: usize| {
        i64::from(i32::from_be_bytes([
            page[at],
            page[at + 1],
            page[at + 2],
            page[at + 3],
        ]))
    };
    classify(int(68), int(60))
}

fn refusal(head: Head) -> Result<(), OpenFailure> {
    match head {
        Head::Vault => Ok(()),
        Head::Foreign(id) => Err(OpenFailure::new(
            VaultError::Invalid,
            OpenCause::ForeignApplicationId(id),
        )),
        Head::Version(version) => Err(OpenFailure::new(
            VaultError::SchemaMismatch,
            OpenCause::UnsupportedVersion(version),
        )),
        Head::Unknown => Err(unrecoverable()),
    }
}

fn unrecoverable() -> OpenFailure {
    OpenFailure::new(VaultError::Invalid, OpenCause::Recovery)
}

/// The pages a journal or WAL beside the vault could write into its page 1.
#[derive(Debug, Default)]
struct Restorable {
    /// A `-journal` or `-wal` exists (SQLite would look at it).
    exists: bool,
    page_size: Option<usize>,
    pages: Vec<Vec<u8>>,
}

/// Reads up to `buf.len()` bytes at `at`; fewer only at the end of the file.
fn read_at(mut file: &File, at: u64, buf: &mut [u8]) -> io::Result<usize> {
    file.seek(SeekFrom::Start(at))?;
    let mut read = 0;
    while read < buf.len() {
        match file.read(&mut buf[read..])? {
            0 => break,
            n => read += n,
        }
    }
    Ok(read)
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn valid_size(size: u32, min: u32) -> bool {
    size.is_power_of_two() && (min..=65536).contains(&size)
}

/// Every page-1 record of a hot rollback journal, segment by segment as SQLite plays it back, but
/// without stopping at a bad checksum or at an unsynced record count (`nRec` 0): more than SQLite
/// would use, never less.
///
/// SQLite calls a journal hot as soon as its first byte is nonzero and ignores it otherwise. Of the
/// 28-byte header (magic 8, `nRec` 4, checksum salt 4, original size 4, sector size 4, page size 4)
/// only the two shapes SQLite itself leaves with a zero first byte are taken for "not hot" (F-S3-01):
///
/// - all 28 bytes zero: a journal finalized by zeroing its header (`journal_mode=PERSIST`);
/// - magic and `nRec` zero, the rest a valid header: a journal whose transaction has not been synced
///   yet (SQLite writes the magic only when it syncs, before the first write to the file), as it is
///   while another connection is in its write transaction, or after a crash before that sync.
///
/// Anything else with a zero first byte, a header shorter than 28 bytes, or a nonzero first byte
/// without the magic and valid sizes, is refused. An empty journal is nothing.
fn journal(file: &File, len: u64, found: &mut Restorable) -> Result<(), OpenFailure> {
    let io = |error: io::Error| OpenFailure::from_io(&error);
    let mut header = [0u8; 28];
    let read = read_at(file, 0, &mut header).map_err(io)?;
    if read == 0 {
        return Ok(());
    }
    if read < header.len() {
        return Err(unrecoverable());
    }
    let (sector, page) = (be32(&header[20..]), be32(&header[24..]));
    let sizes = valid_size(sector, 32) && valid_size(page, 512);
    if header[..12].iter().all(|&byte| byte == 0) {
        return if sizes || header[12..].iter().all(|&byte| byte == 0) {
            Ok(())
        } else {
            Err(unrecoverable())
        };
    }
    if header[..8] != JOURNAL_MAGIC || !sizes {
        return Err(unrecoverable());
    }
    let (sector, page) = (u64::from(sector), u64::from(page));
    let record = page + 8;
    let mut at = 0;
    loop {
        if read_at(file, at, &mut header).map_err(io)? < header.len()
            || header[..8] != JOURNAL_MAGIC
        {
            break; // SQLite stops here too (only the first header must be valid).
        }
        let count = be32(&header[8..]);
        let first = at + sector;
        let to_end = count == 0 || count == u32::MAX;
        let records = if to_end {
            len.saturating_sub(first) / record
        } else {
            u64::from(count)
        };
        let mut pgno = [0u8; 4];
        for index in 0..records {
            let offset = first + index * record;
            if offset + record > len {
                break;
            }
            read_at(file, offset, &mut pgno).map_err(io)?;
            if be32(&pgno) == 1 {
                let mut image = vec![0u8; page as usize];
                read_at(file, offset + 4, &mut image).map_err(io)?;
                found.pages.push(image);
            }
        }
        if to_end {
            break;
        }
        at = (first + records * record).div_ceil(sector) * sector;
    }
    found.page_size = Some(page as usize);
    Ok(())
}

/// Every page-1 frame of a WAL, committed or not. An empty WAL holds nothing. A non-empty one whose
/// header is no WAL header (shorter than 32 bytes, another magic, an impossible page size) is refused:
/// SQLite would still switch the connection to WAL mode over it and later reset or delete it, so it is
/// not left to SQLite (F-S3-01).
fn wal(file: &File, len: u64, found: &mut Restorable) -> Result<(), OpenFailure> {
    let io = |error: io::Error| OpenFailure::from_io(&error);
    let mut header = [0u8; 32];
    let read = read_at(file, 0, &mut header).map_err(io)?;
    if read == 0 {
        return Ok(());
    }
    if read < header.len()
        || !WAL_MAGIC.contains(&be32(&header))
        || !valid_size(be32(&header[8..]), 512)
    {
        return Err(unrecoverable());
    }
    let page = u64::from(be32(&header[8..]));
    let mut frame = [0u8; 4];
    let mut at = 32;
    while at + 24 + page <= len {
        read_at(file, at, &mut frame).map_err(io)?;
        if be32(&frame) == 1 {
            let mut image = vec![0u8; page as usize];
            read_at(file, at + 24, &mut image).map_err(io)?;
            found.pages.push(image);
        }
        at += 24 + page;
    }
    found.page_size.get_or_insert(page as usize);
    Ok(())
}

fn restorable(path: &Path) -> Result<Restorable, OpenFailure> {
    let mut found = Restorable::default();
    for (suffix, read) in [
        (
            "-journal",
            journal as fn(&File, u64, &mut Restorable) -> Result<(), OpenFailure>,
        ),
        ("-wal", wal),
    ] {
        let file = match File::open(super::sidecar(path, suffix)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(OpenFailure::from_io(&error)),
        };
        found.exists = true;
        let len = file
            .metadata()
            .map_err(|error| OpenFailure::from_io(&error))?
            .len();
        read(&file, len, &mut found)?;
    }
    Ok(found)
}

/// Refuses, before SQLite has read anything of `path`, a vault that recovering would change although
/// it is not a vault of this schema (see the module documentation). `key`: the vault key of an
/// encrypted vault, `None` for a plain one.
pub(super) fn check(pin: &Pin, path: &Path, key: Option<&VaultKey>) -> Result<(), OpenFailure> {
    let found = restorable(path)?;
    match key {
        None => {
            let mut own = [0u8; 100];
            let read =
                read_at(&pin.0, 0, &mut own).map_err(|error| OpenFailure::from_io(&error))?;
            match plain_head(&own[..read]) {
                // No sidecar: SQLite reads and writes nothing, and names the file itself.
                Head::Unknown if !found.exists => Ok(()),
                head => refusal(head),
            }?;
            found
                .pages
                .iter()
                .try_for_each(|page| refusal(plain_head(page)))
        }
        Some(_) if !found.exists => Ok(()),
        Some(key) => {
            let encrypted = |failure: OpenFailure| failure.in_mode(VaultMode::Encrypted);
            let mut own = vec![0u8; found.page_size.unwrap_or(CIPHER_PAGE_SIZE)];
            let read = read_at(&pin.0, 0, &mut own)
                .map_err(|error| encrypted(OpenFailure::from_io(&error)))?;
            own.truncate(read);
            // The file's own page: a key SQLCipher refuses is `DecryptFailed`, as when opening.
            refusal(decrypted_head(&own, key)?).map_err(encrypted)?;
            for page in &found.pages {
                // A page of the sidecar that the (accepted) key does not open is no vault page.
                let head = decrypted_head(page, key).map_err(|_| encrypted(unrecoverable()))?;
                refusal(head).map_err(encrypted)?;
            }
            Ok(())
        }
    }
}

/// The header of an encrypted page 1, read by SQLCipher with `key` from a one-page copy. Only
/// ciphertext is written, into a directory of its own that is removed again.
fn decrypted_head(page: &[u8], key: &VaultKey) -> Result<Head, OpenFailure> {
    let failed = |error: io::Error| OpenFailure::from_io(&error).in_mode(VaultMode::Encrypted);
    let dir = std::env::temp_dir().join(format!(
        "memocampus-page1-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir(&dir).map_err(failed)?;
    let result = (|| {
        let copy = dir.join("page1");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&copy)
            .and_then(|mut file| file.write_all(page))
            .map_err(failed)?;
        let sqlite =
            |error: rusqlite::Error| OpenFailure::from_sqlite_in(&error, VaultMode::Encrypted);
        let db = cipher::open(
            &copy,
            key,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| match error {
            cipher::KeyingError::Unavailable => OpenFailure::key(super::KeyError::NotSupported),
            cipher::KeyingError::Sqlite(error) => sqlite(error),
        })?;
        // The copy is shorter than the database its header describes; only the header is read.
        db.execute_batch("PRAGMA writable_schema = ON")
            .map_err(sqlite)?;
        let app_id: i64 = db
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .map_err(sqlite)?;
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(sqlite)?;
        Ok(classify(app_id, version))
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests;
