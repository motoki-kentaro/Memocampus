//! Explicit MHTML saves only. Never writes visits/metadata, evaluates policy or grants publication.
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
    db.execute("INSERT INTO saved_pages (save_id, blob_id, url, title, captured_at, saved_at, incognito) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)", params![save_id, blob_id, meta.url, meta.title, meta.captured_at, now]).map_err(db_error)?;
    Ok(Receipt {
        saved_page_id: db.last_insert_rowid(),
        blob_id,
        sha256: sha.to_owned(),
        byte_length: bytes.len(),
        deduplicated,
        replayed: false,
    })
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
