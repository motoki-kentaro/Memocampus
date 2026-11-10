//! Keyed SQLCipher connections.
//!
//! The key is handed to SQLCipher through `sqlite3_key_v2`, as its raw-key literal `x'<64 hex>'`,
//! built in a buffer that is zeroed afterwards. It never appears in SQL text, so no statement,
//! trace, error message or log can carry it, and no quoting is involved. SQLCipher keeps the key in
//! the connection's codec until the connection closes: the key's lifetime is the connection's.
//!
//! `sqlite3_key_v2` exists only in SQLCipher builds: a binary linked against plain SQLite fails to
//! link, so there is no build in which keying silently does nothing. `is_available` checks the
//! engine at run time as well, before a vault is created.
//!
//! Settings applied to every keyed connection, before anything is read:
//! - `cipher_compatibility = 4`: the SQLCipher 4 format, whatever a later default becomes;
//! - `temp_store = MEMORY`: temporary b-trees (sorts, `VACUUM`) never reach the disk in the clear.

use super::fsutil;
use super::secret::{CipherSalt, VaultKey};
use crate::schema;
use rusqlite::{ffi, Connection, OpenFlags};
use std::ffi::{c_char, c_int, c_void};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;
use zeroize::Zeroizing;

extern "C" {
    /// SQLCipher's keying entry point (sqlite3.h with SQLITE_HAS_CODEC).
    fn sqlite3_key_v2(
        db: *mut ffi::sqlite3,
        db_name: *const c_char,
        key: *const c_void,
        key_len: c_int,
    ) -> c_int;
}

pub const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// What the first 16 bytes of a file say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Head {
    /// A plain SQLite database.
    PlainSqlite,
    /// Anything else of at least 16 bytes: an encrypted vault starts with its salt.
    Salt(CipherSalt),
    /// Fewer than 16 bytes: neither.
    TooShort,
}

pub fn read_head(path: &Path) -> io::Result<Head> {
    let mut file = File::open(path)?;
    let mut head = [0u8; 16];
    let mut read = 0;
    while read < head.len() {
        match file.read(&mut head[read..])? {
            0 => return Ok(Head::TooShort),
            n => read += n,
        }
    }
    Ok(if &head == SQLITE_MAGIC {
        Head::PlainSqlite
    } else {
        Head::Salt(CipherSalt::from_array(head))
    })
}

/// True when this connection's engine is SQLCipher.
pub fn is_available(db: &Connection) -> bool {
    db.query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0))
        .is_ok_and(|version| !version.is_empty())
}

#[derive(Debug)]
pub enum KeyingError {
    /// The engine is not SQLCipher.
    Unavailable,
    Sqlite(rusqlite::Error),
}

impl From<rusqlite::Error> for KeyingError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// Keys a freshly opened connection. Must be the first thing done with it.
pub fn apply_key(db: &Connection, key: &VaultKey) -> Result<(), KeyingError> {
    apply_key_to(db, c"main", key)?;
    db.execute_batch("PRAGMA cipher_compatibility = 4; PRAGMA temp_store = MEMORY;")?;
    Ok(())
}

/// Keys the database `schema` of `db` (`main`, or one just attached without a `KEY` clause by a
/// connection whose main database has no key), before anything of it is read. The key never
/// appears in SQL text. Issue #51 Slice D: an attached database is keyed this way, never with
/// `ATTACH … KEY`, whose key would be SQL text. The caller sets `cipher_compatibility` for it.
pub fn apply_key_to(
    db: &Connection,
    schema: &std::ffi::CStr,
    key: &VaultKey,
) -> Result<(), KeyingError> {
    if !is_available(db) {
        return Err(KeyingError::Unavailable);
    }
    let mut literal = Zeroizing::new(Vec::with_capacity(67));
    literal.extend_from_slice(b"x'");
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for byte in key.expose() {
        literal.push(DIGITS[usize::from(byte >> 4)]);
        literal.push(DIGITS[usize::from(byte & 15)]);
    }
    literal.push(b'\'');
    // SAFETY: the handle belongs to `db`, which outlives the call; `schema` is NUL-terminated; the
    // key buffer is valid for `literal.len()` bytes. SQLCipher copies the key into its codec.
    let rc = unsafe {
        sqlite3_key_v2(
            db.handle(),
            schema.as_ptr(),
            literal.as_ptr().cast(),
            literal.len() as c_int,
        )
    };
    // `literal` (the only copy of the key in text form) is zeroed when it drops.
    if rc != ffi::SQLITE_OK {
        return Err(KeyingError::Sqlite(rusqlite::Error::SqliteFailure(
            ffi::Error::new(rc),
            None,
        )));
    }
    Ok(())
}

/// Opens `path` (never creating it) and keys the connection. Nothing of the file is read yet: the
/// caller's first read is what tells a refused key (`SQLITE_NOTADB`) from success.
pub fn open(path: &Path, key: &VaultKey, flags: OpenFlags) -> Result<Connection, KeyingError> {
    let flags = flags - OpenFlags::SQLITE_OPEN_CREATE - OpenFlags::SQLITE_OPEN_URI;
    let db = Connection::open_with_flags(path, flags)?;
    apply_key(&db, key)?;
    Ok(db)
}

#[derive(Debug)]
pub enum CreateError {
    /// The target or one of its journal files already exists.
    Exists,
    ParentMissing,
    Unavailable,
    Io,
    Db,
}

/// Creates a new encrypted vault at `path` (which must not exist) holding the current schema, keyed
/// with `key`, and returns its salt. Nothing is left behind on failure.
pub fn create(path: &Path, key: &VaultKey) -> Result<CipherSalt, CreateError> {
    if ["-journal", "-wal", "-shm"]
        .iter()
        .any(|suffix| fsutil::with_suffix(path, suffix).exists())
    {
        return Err(CreateError::Exists);
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => drop(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(CreateError::Exists)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(CreateError::ParentMissing)
        }
        Err(_) => return Err(CreateError::Io),
    }
    let result = (|| {
        let db = open(
            path,
            key,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| match error {
            KeyingError::Unavailable => CreateError::Unavailable,
            KeyingError::Sqlite(_) => CreateError::Db,
        })?;
        db.execute_batch(&format!("BEGIN; {} COMMIT;", schema::create_sql()))
            .map_err(|_| CreateError::Db)?;
        schema::check(&db).map_err(|_| CreateError::Db)?;
        db.close().map_err(|_| CreateError::Db)?;
        OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|_| CreateError::Io)?;
        match read_head(path) {
            Ok(Head::Salt(salt)) => Ok(salt),
            _ => Err(CreateError::Db),
        }
    })();
    if result.is_err() {
        // Only what this call created exclusively.
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(fsutil::with_suffix(path, "-journal"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ErrorCode;

    fn dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mc-cipher-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rw() -> OpenFlags {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
    }

    fn not_a_database(result: rusqlite::Result<i64>) -> bool {
        result.is_err_and(|error| error.sqlite_error_code() == Some(ErrorCode::NotADatabase))
    }

    #[test]
    fn the_engine_is_sqlcipher_4() {
        let db = Connection::open_in_memory().unwrap();
        assert!(is_available(&db));
        let version: String = db
            .query_row("PRAGMA cipher_version", [], |row| row.get(0))
            .unwrap();
        assert!(version.starts_with("4."), "{version}");
    }

    #[test]
    fn an_encrypted_vault_opens_with_its_key_only_and_holds_no_plaintext() {
        let dir = dir();
        let path = dir.join("v.sqlite3");
        let key = VaultKey::generate().unwrap();
        let salt = create(&path, &key).unwrap();
        assert_eq!(read_head(&path).unwrap(), Head::Salt(salt));
        let marker = "https://canary.example/CANARY-plaintext";
        {
            let db = open(&path, &key, rw()).unwrap();
            schema::check(&db).unwrap();
            db.execute(
                "INSERT INTO visits VALUES ('v1', ?1, 'CANARY title', '2026-11-09T00:00:00.000Z')",
                [marker],
            )
            .unwrap();
            let journal: String = db
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "delete");
        }
        let bytes = fs::read(&path).unwrap();
        assert!(!bytes.windows(marker.len()).any(|w| w == marker.as_bytes()));
        assert!(!bytes.windows(6).any(|w| w == b"CANARY"));
        assert!(!key.appears_in(&bytes));

        // Reopened with the key: the row is there; the salt did not change.
        let db = open(&path, &key, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let url: String = db
            .query_row("SELECT url FROM visits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(url, marker);
        drop(db);
        assert_eq!(read_head(&path).unwrap(), Head::Salt(salt));

        // Wrong key, no key: SQLITE_NOTADB at the first read, file unchanged.
        let wrong = open(&path, &VaultKey::generate().unwrap(), rw()).unwrap();
        assert!(not_a_database(wrong.query_row(
            "SELECT count(*) FROM sqlite_master",
            [],
            |row| row.get(0)
        )));
        drop(wrong);
        let plain = Connection::open_with_flags(&path, rw()).unwrap();
        assert!(not_a_database(plain.query_row(
            "SELECT count(*) FROM sqlite_master",
            [],
            |row| row.get(0)
        )));
        drop(plain);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_plain_vault_keyed_is_refused_and_creation_never_overwrites() {
        let dir = dir();
        let plain = dir.join("plain.sqlite3");
        Connection::open(&plain)
            .unwrap()
            .execute_batch(&schema::create_sql())
            .unwrap();
        assert_eq!(read_head(&plain).unwrap(), Head::PlainSqlite);
        let before = fs::read(&plain).unwrap();
        let key = VaultKey::generate().unwrap();
        let keyed = open(&plain, &key, rw()).unwrap();
        assert!(not_a_database(keyed.query_row(
            "SELECT count(*) FROM sqlite_master",
            [],
            |row| row.get(0)
        )));
        drop(keyed);
        assert_eq!(fs::read(&plain).unwrap(), before);
        assert!(matches!(create(&plain, &key), Err(CreateError::Exists)));
        assert_eq!(fs::read(&plain).unwrap(), before);
        // Opening never creates a missing file.
        assert!(open(&dir.join("missing.sqlite3"), &key, rw()).is_err());
        assert!(!dir.join("missing.sqlite3").exists());
        assert!(matches!(
            create(&dir.join("no-such-dir").join("v.sqlite3"), &key),
            Err(CreateError::ParentMissing)
        ));
        fs::write(dir.join("short"), b"abc").unwrap();
        assert_eq!(read_head(&dir.join("short")).unwrap(), Head::TooShort);
        fs::remove_dir_all(&dir).unwrap();
    }
}
