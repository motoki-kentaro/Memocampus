//! Slice D (Issue #51): copying a vault between a plain and an encrypted database, for the
//! conversion of a vault's encryption state (`docs/secure-vault/slice-d.md`); Slice E: also from an
//! encrypted database into a new one under another key and salt (re-issuing the Recovery Spell,
//! restoring a backup under a new vault id; `docs/secure-vault/slice-e.md`).
//!
//! One connection holds both ends: the **source is always attached read-only** (`file:` URI with
//! `mode=ro`), so no statement of the copy can change it, and the target is written in **one
//! transaction** that also holds the source's read lock: the copy is one snapshot of the source,
//! however other processes write to it meanwhile.
//!
//! | Direction | `main` | attached |
//! |---|---|---|
//! | encrypt (plain → encrypted) | the target, keyed with the new vault key | `src`: the plain source, `KEY ''` |
//! | decrypt (encrypted → plain) | the source, keyed (`sqlite3_key_v2`), read-only | `dst`: the plain target, `KEY ''` |
//! | rekey (encrypted → encrypted, Slice E) | the source, keyed, read-only | `dst`: the target, attached `KEY ''`, then keyed with its key and salt (`sqlite3_key_v2`) |
//!
//! An encrypted target is always keyed with its key **and** its salt (`cipher::apply_new_key_to`):
//! a new file gets exactly that salt as its first 16 bytes, and an existing one (a refresh) opens
//! only if its salt is that one (PoC E P2/P3, P4).
//!
//! The refresh of a re-keyed copy (both ends encrypted, Slice E) cannot attach either end: an
//! existing encrypted database is refused by `ATTACH` before it could be keyed, and `ATTACH … KEY`
//! would put the key in SQL text. It uses **two connections** instead, each keyed through the C API:
//! the source read-only in a read transaction (its lock held throughout), the target in one write
//! transaction; rows are copied with bound values and compared across the two connections before
//! the target commits.
//!
//! Keys only ever reach SQLCipher through `cipher::apply_key` (`sqlite3_key_v2`), never as SQL
//! text. (An encrypted database attached without a key is refused by `ATTACH` itself, before it
//! could be keyed: PoC P2; hence the encrypted source is `main` when decrypting.)
//!
//! - `export`: the first copy into a new, empty file: `sqlcipher_export`, then the source's
//!   `user_version` and `application_id`, which `sqlcipher_export` of this SQLCipher build does not
//!   copy (PoC P1).
//! - `refresh`: the copy into a target that already holds a copy (when the converted vault is
//!   activated, so that writes made to the source since the conversion are not lost): every table
//!   emptied and filled again with `INSERT … SELECT`. The target keeps its file, salt and key.
//!
//! Both end with `compare`, still inside the transaction: the schemas (every object's SQL text) and
//! both header fields are equal, and every table holds exactly the same rows (read in primary-key
//! order from both ends at once, one row at a time). Any difference rolls the copy back.
//!
//! The source's file identity (`vault_token`) is checked once its read lock is held: SQLite holds
//! the file open without `FILE_SHARE_DELETE` (Windows), so it cannot be replaced or renamed until
//! the copy ends. Nothing here logs; errors carry no path, no key and no row.

use crate::active_vault;
use crate::schema;
use crate::secure_vault::cipher::{self, Head};
use crate::secure_vault::secret::{CipherSalt, VaultKey};
use rusqlite::types::Value;
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::path::Path;
use std::time::Duration;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a copy failed. Nothing of the vault's content or keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyError {
    /// The source is not the file identified when the conversion began.
    SourceChanged,
    /// A lock could not be had in time (another process writes or holds the file).
    Busy,
    /// The disk (or quota) is full.
    DiskFull,
    /// A key was refused (the file is not the database the key belongs to).
    Keying,
    /// The copy is not identical to the source, or the target is not what it must be.
    Mismatch,
    /// The source or target could not be opened or read.
    Io,
    /// Any other SQLite failure.
    Db,
}

impl CopyError {
    fn of(error: &rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy,
            Some(ErrorCode::DiskFull) => Self::DiskFull,
            Some(ErrorCode::NotADatabase) => Self::Keying,
            Some(
                ErrorCode::CannotOpen | ErrorCode::SystemIoFailure | ErrorCode::PermissionDenied,
            ) => Self::Io,
            _ => Self::Db,
        }
    }
}

impl From<rusqlite::Error> for CopyError {
    fn from(error: rusqlite::Error) -> Self {
        Self::of(&error)
    }
}

/// The key and salt of an encrypted target.
#[derive(Clone, Copy)]
pub struct Keyed<'a> {
    pub key: &'a VaultKey,
    pub salt: &'a CipherSalt,
}

/// The two ends of a copy. At least one is encrypted: `source_key` alone when decrypting,
/// `target_key` alone when encrypting, both when re-keying.
pub struct Ends<'a> {
    pub source: &'a Path,
    pub source_key: Option<&'a VaultKey>,
    /// The `vault_token` the source must still have.
    pub source_token: &'a str,
    pub target: &'a Path,
    pub target_key: Option<Keyed<'a>>,
}

/// A `file:` URI naming `path`, opened read-only. Only `%`, `?` and `#` are escaped: SQLite takes
/// every other character (UTF-8 included) literally.
pub fn read_only_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file:");
    if !text.starts_with('/') {
        uri.push('/'); // `file:/C:/…`: SQLite on Windows drops the slash before a drive letter
    }
    for ch in text.chars() {
        match ch {
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            _ => uri.push(ch),
        }
    }
    uri.push_str("?mode=ro");
    uri
}

/// The connection holding both ends, and the schema names of target and source in it.
struct Pair {
    db: Connection,
    target: &'static str,
    source: &'static str,
}

fn connect(ends: &Ends) -> Result<Pair, CopyError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    let failed = |error| match error {
        cipher::KeyingError::Unavailable => CopyError::Keying,
        cipher::KeyingError::Sqlite(error) => CopyError::of(&error),
    };
    let keyed = |db: &Connection, key: &VaultKey| cipher::apply_key(db, key).map_err(failed);
    let keyed_target = |db: &Connection, schema: &std::ffi::CStr, target: Keyed| {
        cipher::apply_new_key_to(db, schema, target.key, target.salt).map_err(failed)?;
        let name = schema.to_str().map_err(|_| CopyError::Mismatch)?;
        db.execute_batch(&format!(
            "PRAGMA {name}.cipher_compatibility = 4; PRAGMA temp_store = MEMORY;"
        ))?;
        Ok::<_, CopyError>(())
    };
    let pair = match (ends.source_key, ends.target_key) {
        (None, Some(target)) => {
            // The target exists (created empty by the caller): opening never creates a file here.
            let db = Connection::open_with_flags(ends.target, flags)?;
            keyed_target(&db, c"main", target)?;
            db.execute(
                "ATTACH DATABASE ?1 AS src KEY ''",
                [read_only_uri(ends.source)],
            )?;
            Pair {
                db,
                target: "main",
                source: "src",
            }
        }
        (Some(key), None) => {
            let db = Connection::open_with_flags(read_only_uri(ends.source), flags)?;
            keyed(&db, key)?;
            db.execute(
                "ATTACH DATABASE ?1 AS dst KEY ''",
                [ends.target.to_string_lossy()],
            )?;
            Pair {
                db,
                target: "dst",
                source: "main",
            }
        }
        (Some(key), Some(target)) => {
            let db = Connection::open_with_flags(read_only_uri(ends.source), flags)?;
            keyed(&db, key)?;
            db.execute(
                "ATTACH DATABASE ?1 AS dst KEY ''",
                [ends.target.to_string_lossy()],
            )?;
            keyed_target(&db, c"dst", target)?;
            Pair {
                db,
                target: "dst",
                source: "main",
            }
        }
        (None, None) => return Err(CopyError::Mismatch),
    };
    pair.db.busy_timeout(BUSY_TIMEOUT)?;
    // Temporary b-trees (none expected) stay in memory; constraints are the source's to have kept.
    pair.db
        .execute_batch("PRAGMA temp_store = MEMORY; PRAGMA foreign_keys = OFF;")?;
    Ok(pair)
}

/// Takes the source's read lock and checks its identity under it.
fn pin_source(pair: &Pair, ends: &Ends) -> Result<(), CopyError> {
    pair.db.query_row(
        &format!("SELECT count(*) FROM {}.sqlite_schema", pair.source),
        [],
        |row| row.get::<_, i64>(0),
    )?;
    match active_vault::vault_token(ends.source) {
        Ok(token) if token == ends.source_token => Ok(()),
        Ok(_) => Err(CopyError::SourceChanged),
        Err(_) => Err(CopyError::Io),
    }
}

/// Runs `copy` in one transaction holding both ends, then `compare`; commits only if both pass.
fn transaction(
    ends: &Ends,
    copy: impl FnOnce(&Pair) -> Result<(), CopyError>,
) -> Result<(), CopyError> {
    let pair = connect(ends)?;
    pair.db.execute_batch("BEGIN")?;
    let result = pin_source(&pair, ends)
        .and_then(|()| copy(&pair))
        .and_then(|()| compare(&pair.db, pair.target, pair.source));
    match result {
        Ok(()) => {
            pair.db.execute_batch("COMMIT")?;
            pair.db.close().map_err(|(_, error)| CopyError::of(&error))
        }
        Err(error) => {
            let _ = pair.db.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn header(db: &Connection, schema_name: &str, field: &str) -> Result<i64, CopyError> {
    Ok(
        db.query_row(&format!("PRAGMA {schema_name}.{field}"), [], |row| {
            row.get(0)
        })?,
    )
}

/// The first copy, into the empty file `ends.target`.
pub fn export(ends: &Ends) -> Result<(), CopyError> {
    transaction(ends, |pair| {
        pair.db.query_row(
            "SELECT sqlcipher_export(?1, ?2)",
            [pair.target, pair.source],
            |_| Ok(()),
        )?;
        for field in ["user_version", "application_id"] {
            let value = header(&pair.db, pair.source, field)?;
            pair.db
                .execute_batch(&format!("PRAGMA {}.{field} = {value}", pair.target))?;
        }
        Ok(())
    })
}

/// A new copy into `ends.target`, which already holds the same schema: every table is emptied and
/// filled again from the source.
pub fn refresh(ends: &Ends) -> Result<(), CopyError> {
    if let (Some(source_key), Some(target_key)) = (ends.source_key, ends.target_key) {
        return refresh_rekeyed(ends, source_key, target_key);
    }
    transaction(ends, |pair| {
        let tables = tables(&pair.db, pair.source)?;
        if tables != self::tables(&pair.db, pair.target)? {
            return Err(CopyError::Mismatch);
        }
        for table in &tables {
            pair.db
                .execute_batch(&format!("DELETE FROM {}.\"{table}\"", pair.target))?;
        }
        for table in &tables {
            pair.db.execute_batch(&format!(
                "INSERT INTO {}.\"{table}\" SELECT * FROM {}.\"{table}\"",
                pair.target, pair.source
            ))?;
        }
        Ok(())
    })
}

/// `refresh` with both ends encrypted: two connections (see the module documentation).
fn refresh_rekeyed(ends: &Ends, source_key: &VaultKey, target_key: Keyed) -> Result<(), CopyError> {
    let failed = |error| match error {
        cipher::KeyingError::Unavailable => CopyError::Keying,
        cipher::KeyingError::Sqlite(error) => CopyError::of(&error),
    };
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let source = Connection::open_with_flags(
        read_only_uri(ends.source),
        flags | OpenFlags::SQLITE_OPEN_URI,
    )?;
    cipher::apply_key(&source, source_key).map_err(failed)?;
    let target = Connection::open_with_flags(ends.target, flags)?;
    cipher::apply_new_key_to(&target, c"main", target_key.key, target_key.salt).map_err(failed)?;
    target.execute_batch(
        "PRAGMA cipher_compatibility = 4; PRAGMA temp_store = MEMORY; PRAGMA foreign_keys = OFF;",
    )?;
    for db in [&source, &target] {
        db.busy_timeout(BUSY_TIMEOUT)?;
    }
    source.execute_batch("BEGIN")?;
    let result = (|| {
        // The source's read lock, then its identity under it (as `pin_source`).
        source.query_row("SELECT count(*) FROM main.sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })?;
        match active_vault::vault_token(ends.source) {
            Ok(token) if token == ends.source_token => {}
            Ok(_) => return Err(CopyError::SourceChanged),
            Err(_) => return Err(CopyError::Io),
        }
        target.execute_batch("BEGIN IMMEDIATE")?;
        let copied = (|| {
            let names = tables(&source, "main")?;
            if names != tables(&target, "main")? {
                return Err(CopyError::Mismatch);
            }
            for table in &names {
                target.execute_batch(&format!("DELETE FROM main.\"{table}\""))?;
            }
            for table in &names {
                let mut select = source.prepare(&format!("SELECT * FROM main.\"{table}\""))?;
                let columns = select.column_count();
                let placeholders = (1..=columns)
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut insert = target.prepare(&format!(
                    "INSERT INTO main.\"{table}\" VALUES ({placeholders})"
                ))?;
                let mut rows = select.query([])?;
                while let Some(row) = rows.next()? {
                    let values = (0..columns)
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<Result<Vec<_>, _>>()?;
                    insert.execute(rusqlite::params_from_iter(values))?;
                }
            }
            compare_between((&target, "main"), (&source, "main"))
        })();
        match copied {
            Ok(()) => Ok(target.execute_batch("COMMIT")?),
            Err(error) => {
                let _ = target.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    })();
    let _ = source.execute_batch("ROLLBACK");
    result?;
    target.close().map_err(|(_, error)| CopyError::of(&error))?;
    source.close().map_err(|(_, error)| CopyError::of(&error))
}

/// The ordinary tables of a schema, by name. Table names come from the database, never from a
/// request; a name that would need quoting is refused.
fn tables(db: &Connection, schema_name: &str) -> Result<Vec<String>, CopyError> {
    let mut statement = db.prepare(&format!(
        "SELECT name FROM {schema_name}.sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name"
    ))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if names.iter().any(|name| {
        name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }) {
        return Err(CopyError::Mismatch);
    }
    Ok(names)
}

/// Every object's definition (`sql` as SQLite stores it), in a fixed order.
fn definitions(
    db: &Connection,
    schema_name: &str,
) -> Result<Vec<(String, String, Option<String>)>, CopyError> {
    let mut statement = db.prepare(&format!(
        "SELECT type, name, sql FROM {schema_name}.sqlite_schema ORDER BY type, name"
    ))?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The columns a table is ordered by: its primary key (rowid when it has none).
fn order_of(db: &Connection, schema_name: &str, table: &str) -> Result<String, CopyError> {
    let mut statement = db.prepare(&format!("PRAGMA {schema_name}.table_info(\"{table}\")"))?;
    let mut key: Vec<(i64, String)> = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(5)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(pk, _)| *pk > 0)
        .collect();
    key.sort();
    if key.is_empty() {
        return Ok("rowid".into());
    }
    Ok(key
        .into_iter()
        .map(|(_, column)| format!("\"{}\"", column.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", "))
}

/// The target holds exactly the source: same definitions, same header fields, same rows.
pub fn compare(db: &Connection, target: &str, source: &str) -> Result<(), CopyError> {
    compare_between((db, target), (db, source))
}

/// `compare` of a target and a source schema, each on its own connection or on the same one.
fn compare_between(
    (target_db, target): (&Connection, &str),
    (source_db, source): (&Connection, &str),
) -> Result<(), CopyError> {
    if definitions(target_db, target)? != definitions(source_db, source)? {
        return Err(CopyError::Mismatch);
    }
    for field in ["user_version", "application_id"] {
        if header(target_db, target, field)? != header(source_db, source, field)? {
            return Err(CopyError::Mismatch);
        }
    }
    for table in tables(source_db, source)? {
        let order = order_of(source_db, source, &table)?;
        let mut from = source_db.prepare(&format!(
            "SELECT * FROM {source}.\"{table}\" ORDER BY {order}"
        ))?;
        let mut into = target_db.prepare(&format!(
            "SELECT * FROM {target}.\"{table}\" ORDER BY {order}"
        ))?;
        let columns = from.column_count();
        if into.column_count() != columns {
            return Err(CopyError::Mismatch);
        }
        let mut a = from.query([])?;
        let mut b = into.query([])?;
        loop {
            match (a.next()?, b.next()?) {
                (None, None) => break,
                (Some(x), Some(y)) => {
                    for i in 0..columns {
                        if x.get::<_, Value>(i)? != y.get::<_, Value>(i)? {
                            return Err(CopyError::Mismatch);
                        }
                    }
                }
                _ => return Err(CopyError::Mismatch),
            }
        }
    }
    Ok(())
}

/// A finished copy, opened on its own: the expected kind of file (SQLCipher starting with the
/// expected salt, or plain SQLite), the key opening it, a full `integrity_check` and the exact
/// schema of this version.
pub fn verify(target: &Path, key: Option<Keyed>) -> Result<(), CopyError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let head = cipher::read_head(target).map_err(|_| CopyError::Io)?;
    let db = match (head, key) {
        (Head::Salt(found), Some(Keyed { key, salt })) if found == *salt => {
            cipher::open(target, key, flags).map_err(|error| match error {
                cipher::KeyingError::Unavailable => CopyError::Keying,
                cipher::KeyingError::Sqlite(error) => CopyError::of(&error),
            })?
        }
        (Head::PlainSqlite, None) => Connection::open_with_flags(target, flags)?,
        _ => return Err(CopyError::Mismatch),
    };
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(CopyError::Mismatch);
    }
    schema::check(&db).map_err(|_| CopyError::Mismatch)?;
    db.close().map_err(|(_, error)| CopyError::of(&error))
}
