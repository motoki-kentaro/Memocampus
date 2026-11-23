//! Explicit MHTML saves only. Never writes visits/metadata, evaluates policy or grants publication.
//!
//! `saved_pages.mcp_public` (schema 10) is the person's per-save permission: every new save writes 1
//! explicitly, a replay of the same `save_id` never touches it, and only `set_mcp_public` changes it,
//! for exactly one save. It is necessary, never sufficient, for MCP: the approvals of the vault and
//! of the Page Vault and the policy must allow it too, and nothing here reads or grants those.
//!
//! A save is named by a `SaveRef`: its row id and the random `instance` written with it. The id alone
//! is not enough, because SQLite gives a new row the id of a deleted highest one; a save deleted and
//! saved again, under the same `save_id` too, is a new instance. So a choice prepared for a save that
//! no longer exists finds nothing (`not_found`) instead of changing whichever save has its id now.
use crate::vault::{self, Session, VaultError, VaultTicket};
use rusqlite::{params, Connection, DatabaseName, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub const MAX_PAGE_BYTES: usize = 128 * 1024 * 1024;
pub const CHUNK_BYTES: usize = 1024 * 1024;
pub const MAX_URL_BYTES: usize = 32768;
pub const MAX_TITLE_BYTES: usize = 65536;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Metadata {
    pub url: String,
    pub title: String,
    pub captured_at: i64,
    pub incognito: bool,
}

pub fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id)
}

pub fn validate(meta: &Metadata) -> Result<(), &'static str> {
    if meta.incognito {
        return Err("incognito_not_allowed");
    }
    if meta.url.len() > MAX_URL_BYTES
        || meta.title.len() > MAX_TITLE_BYTES
        || meta.url.contains('\0')
        || meta.title.contains('\0')
        || meta.captured_at < 0
        || meta.captured_at > 8_640_000_000_000_000
    {
        return Err("invalid_metadata");
    }
    if !url::Url::parse(&meta.url)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
    {
        return Err("unsupported_url");
    }
    Ok(())
}

pub fn validate_size(size: usize) -> Result<(), &'static str> {
    if size == 0 || size > MAX_PAGE_BYTES {
        Err("invalid_page_size")
    } else {
        Ok(())
    }
}

/// One save as it was shown to the person: whoever lists saves for a choice (S7) hands out both
/// parts, read together, and an operation must name both. Never look the instance up by id when
/// the operation runs: that would name whichever save holds the id then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveRef {
    pub id: i64,
    pub instance: [u8; 16],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub saved_page_id: i64,
    pub blob_id: i64,
    pub sha256: String,
    pub byte_length: usize,
    pub deduplicated: bool,
    pub replayed: bool,
}

fn db_error(_: rusqlite::Error) -> &'static str {
    "db_error"
}

/// Does not return URL/title or bytes, including on collisions.
pub fn lookup(
    db: &Connection,
    save_id: &str,
    meta: &Metadata,
    sha: &str,
    size: usize,
) -> Result<Option<Receipt>, &'static str> {
    let stored = db.query_row(
        "SELECT p.id, b.id, b.sha256, b.byte_length, p.url, p.title, p.captured_at, p.incognito FROM saved_pages p JOIN page_blobs b ON b.id = p.blob_id WHERE p.save_id = ?1",
        [save_id], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,i64>(1)?, row.get::<_,String>(2)?, row.get::<_,usize>(3)?, Metadata { url: row.get(4)?, title: row.get(5)?, captured_at: row.get(6)?, incognito: row.get(7)? }))
    ).optional().map_err(db_error)?;
    let Some((saved_page_id, blob_id, hash, len, metadata)) = stored else {
        return Ok(None);
    };
    if metadata != *meta || hash != sha || len != size {
        return Err("save_id_conflict");
    }
    Ok(Some(Receipt {
        saved_page_id,
        blob_id,
        sha256: hash,
        byte_length: len,
        deduplicated: true,
        replayed: true,
    }))
}

/// Caller owns the transaction and acknowledges only after COMMIT. Incremental BLOB I/O avoids
/// passing another 128 MiB value through SQLite's bind/copy path. No transaction during transfer.
pub fn insert(
    db: &Connection,
    save_id: &str,
    meta: &Metadata,
    bytes: &[u8],
    sha: &str,
    now: i64,
) -> Result<Receipt, &'static str> {
    validate(meta)?;
    validate_size(bytes.len())?;
    if !valid_id(save_id) {
        return Err("invalid_save_id");
    }
    if format!("{:x}", Sha256::digest(bytes)) != sha {
        return Err("hash_mismatch");
    }
    if let Some(receipt) = lookup(db, save_id, meta, sha, bytes.len())? {
        return Ok(receipt);
    }
    let existing: Option<i64> = db
        .query_row(
            "SELECT id FROM page_blobs WHERE sha256 = ?1",
            [sha],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let deduplicated = existing.is_some();
    let blob_id = if let Some(id) = existing {
        id
    } else {
        db.execute("INSERT INTO page_blobs (sha256, byte_length, format, mime_type, data) VALUES (?1, ?2, 'mhtml', 'multipart/related', zeroblob(?2))", params![sha, bytes.len() as i64]).map_err(db_error)?;
        let id = db.last_insert_rowid();
        let mut blob = db
            .blob_open(DatabaseName::Main, "page_blobs", "data", id, false)
            .map_err(db_error)?;
        for chunk in bytes.chunks(CHUNK_BYTES) {
            blob.write_all(chunk).map_err(|_| "db_error")?;
        }
        blob.close().map_err(db_error)?;
        id
    };
    // A new instance for every new save (never for a replay, which returned above).
    let instance = uuid::Uuid::new_v4().into_bytes();
    db.execute("INSERT INTO saved_pages (save_id, blob_id, url, title, captured_at, saved_at, incognito, mcp_public, instance) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 1, ?7)", params![save_id, blob_id, meta.url, meta.title, meta.captured_at, now, instance]).map_err(db_error)?;
    Ok(Receipt {
        saved_page_id: db.last_insert_rowid(),
        blob_id,
        sha256: sha.to_owned(),
        byte_length: bytes.len(),
        deduplicated,
        replayed: false,
    })
}

/// The stored permission of one save, `None` if that save (id and instance) does not exist. Anything
/// but the integers 0 and 1 is refused rather than read as either, so a forged value never counts as
/// permitted.
#[cfg_attr(not(test), allow(dead_code))] // read by the publication UI and MCP of later slices (S7/S8)
pub fn mcp_public(db: &Connection, save: SaveRef) -> Result<Option<bool>, &'static str> {
    let stored = db
        .query_row(
            "SELECT mcp_public FROM saved_pages WHERE id = ?1 AND instance = ?2",
            params![save.id, save.instance],
            |row| row.get::<_, rusqlite::types::Value>(0),
        )
        .optional()
        .map_err(db_error)?;
    match stored {
        None => Ok(None),
        Some(rusqlite::types::Value::Integer(0)) => Ok(Some(false)),
        Some(rusqlite::types::Value::Integer(1)) => Ok(Some(true)),
        Some(_) => Err("invalid_mcp_public"),
    }
}

/// Records the person's choice for exactly one save (never its shared blob or other saves of it) of
/// the vault named by `ticket`. Under the write lock the vault must still be that ticket's: the same
/// configured path (`write_for`) and the same file there (`vault_token`), so a choice never lands in
/// another vault put at the same path. Within it, the one statement that writes matches the save by
/// id and instance together, in the same IMMEDIATE transaction: a save that was deleted (and maybe
/// replaced by another one under its id or `save_id`) fails with `not_found` and nothing changes.
#[cfg_attr(not(test), allow(dead_code))] // the per-save setting UI is Slice S7
pub fn set_mcp_public(
    session: &mut Session,
    ticket: &VaultTicket,
    save: SaveRef,
    mcp_public: bool,
) -> Result<(), &'static str> {
    session
        .write_for(ticket, |db| {
            if vault::vault_token(std::path::Path::new(&ticket.path))? != ticket.token {
                return Err(SetError::Vault(VaultError::Switched));
            }
            match db.execute(
                "UPDATE saved_pages SET mcp_public = ?3 WHERE id = ?1 AND instance = ?2",
                params![save.id, save.instance, mcp_public],
            ) {
                Ok(1) => Ok(()),
                Ok(_) => Err(SetError::NotFound),
                Err(_) => Err(SetError::Vault(VaultError::Db)),
            }
        })
        .map_err(|error| match error {
            SetError::NotFound => "not_found",
            SetError::Vault(error) => error.code(),
        })
}

enum SetError {
    NotFound,
    Vault(VaultError),
}

impl From<VaultError> for SetError {
    fn from(error: VaultError) -> Self {
        Self::Vault(error)
    }
}

/// Streaming integrity verification for backups/restores: never materializes whole originals.
pub fn verify(db: &Connection) -> rusqlite::Result<()> {
    let mut statement = db.prepare("SELECT id, sha256, byte_length FROM page_blobs")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, usize>(2)?,
        ))
    })?;
    let mut buffer = vec![0u8; CHUNK_BYTES];
    for row in rows {
        let (id, sha, len) = row?;
        let mut blob = db.blob_open(DatabaseName::Main, "page_blobs", "data", id, true)?;
        let mut hash = Sha256::new();
        let mut size = 0;
        loop {
            let n = blob
                .read(&mut buffer)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            if n == 0 {
                break;
            }
            size += n;
            hash.update(&buffer[..n]);
        }
        if size != len || validate_size(size).is_err() || format!("{:x}", hash.finalize()) != sha {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    let broken: i64 = db.query_row("SELECT count(*) FROM saved_pages p LEFT JOIN page_blobs b ON b.id = p.blob_id WHERE b.id IS NULL", [], |row| row.get(0))?;
    if broken != 0 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

#[cfg(test)]
mod mcp_public_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use memocampus_native_host::schema;
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&schema::create_sql()).unwrap();
        db.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        db
    }
    fn meta() -> Metadata {
        Metadata {
            url: "https://synthetic.test/private?token=dummy".into(),
            title: "合成ページ".into(),
            captured_at: 1,
            incognito: false,
        }
    }
    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    #[test]
    fn immutable_dedup_separate_events_idempotency_and_full_metadata_collisions() {
        let mut db = db();
        let bytes = b"MIME-Version: 1.0\r\n\0synthetic\xff";
        let sha = hash(bytes);
        let meta = meta();
        let id = uuid::Uuid::new_v4().to_string();
        {
            let tx = db.transaction().unwrap();
            insert(&tx, &id, &meta, bytes, &sha, 2).unwrap();
            tx.commit().unwrap();
        }
        let tx = db.transaction().unwrap();
        assert!(insert(&tx, &id, &meta, bytes, &sha, 3).unwrap().replayed);
        let other = insert(
            &tx,
            &uuid::Uuid::new_v4().to_string(),
            &meta,
            bytes,
            &sha,
            3,
        )
        .unwrap();
        assert!(other.deduplicated);
        for modified in [
            Metadata {
                title: "other".into(),
                ..meta.clone()
            },
            Metadata {
                url: "https://other.test/".into(),
                ..meta.clone()
            },
            Metadata {
                captured_at: 2,
                ..meta.clone()
            },
        ] {
            assert_eq!(
                insert(&tx, &id, &modified, bytes, &sha, 3).unwrap_err(),
                "save_id_conflict"
            );
        }
        assert_eq!(
            insert(&tx, &id, &meta, b"changed", &hash(b"changed"), 3).unwrap_err(),
            "save_id_conflict"
        );
        tx.commit().unwrap();
        verify(&db).unwrap();
        let stored: Vec<u8> = db
            .query_row("SELECT data FROM page_blobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, bytes);
        for (table, expected) in [
            ("saved_pages", 2),
            ("page_blobs", 1),
            ("visits", 0),
            ("page_metadata", 0),
            ("saved_images", 0),
        ] {
            let n: i64 = db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, expected);
        }
    }
    #[test]
    fn failed_event_and_commit_roll_back_blob() {
        let mut db = db();
        let bytes = b"synthetic";
        let id = uuid::Uuid::new_v4().to_string();
        // Failure after BLOB write, then an actual COMMIT failure from a deferred FK.
        db.execute_batch("CREATE TEMP TRIGGER reject_page BEFORE INSERT ON saved_pages BEGIN SELECT RAISE(ABORT, 'synthetic'); END;").unwrap();
        {
            let tx = db.transaction().unwrap();
            assert!(insert(&tx, &id, &meta(), bytes, &hash(bytes), 2).is_err());
        }
        db.execute_batch("DROP TRIGGER reject_page; CREATE TABLE fail_commit (id INTEGER REFERENCES visits(event_id) DEFERRABLE INITIALLY DEFERRED);").unwrap();
        {
            let tx = db.transaction().unwrap();
            insert(&tx, &id, &meta(), bytes, &hash(bytes), 2).unwrap();
            tx.execute("INSERT INTO fail_commit VALUES (42)", [])
                .unwrap();
            assert!(tx.commit().is_err());
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM page_blobs", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM saved_pages", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn sizes_and_schema_seven_are_refused_without_modification() {
        assert!(validate_size(0).is_err());
        assert!(validate_size(MAX_PAGE_BYTES).is_ok());
        assert!(validate_size(MAX_PAGE_BYTES + 1).is_err());
        let db = db();
        db.execute_batch("PRAGMA user_version = 7").unwrap();
        assert!(matches!(
            schema::check(&db),
            Err(schema::SchemaError::UnsupportedVersion(7))
        ));
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            7
        );
    }
    #[test]
    fn exact_128_mib_incremental_store_and_snapshot() {
        let mut db = db();
        let bytes = vec![0x53; MAX_PAGE_BYTES];
        let sha = hash(&bytes);
        let start = std::time::Instant::now();
        let tx = db.transaction().unwrap();
        insert(
            &tx,
            &uuid::Uuid::new_v4().to_string(),
            &meta(),
            &bytes,
            &sha,
            2,
        )
        .unwrap();
        tx.commit().unwrap();
        verify(&db).unwrap();
        eprintln!(
            "synthetic Page 128MiB store+verify {:?}; input={} bytes; incremental block={} bytes",
            start.elapsed(),
            bytes.len(),
            CHUNK_BYTES
        );
    }
}
