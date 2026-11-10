//! Issue #51, Slice D: what the bundled SQLCipher engine does when a vault is copied between a plain
//! and an encrypted database (`docs/secure-vault/slice-d.md` §3). These are the PoC experiments,
//! kept as tests so that an engine upgrade that changes any of them fails here first. Temporary
//! directories only.

use memocampus_native_host::schema;
use memocampus_native_host::secure_vault::cipher::{self, Head};
use memocampus_native_host::secure_vault::secret::VaultKey;
use rusqlite::{Connection, ErrorCode, OpenFlags};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

const CANARY: &str = "https://canary.example/SLICE-D-CANARY";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-d-poc-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn rw() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
}

fn rw_uri() -> OpenFlags {
    rw() | OpenFlags::SQLITE_OPEN_URI
}

/// `file:` URI of a path, read-only: the attached source can never be written.
fn read_only_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file:");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for ch in text.chars() {
        match ch {
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            '%' => uri.push_str("%25"),
            _ => uri.push(ch),
        }
    }
    uri.push_str("?mode=ro");
    uri
}

/// Visits (with the canary), page metadata, an image of 1 MiB with a thumbnail, tags and links.
fn fill(db: &Connection) {
    db.execute_batch("BEGIN").unwrap();
    for i in 0..200 {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, ?3, '2026-11-10T00:00:00.000Z')",
            (
                format!("v{i:04}"),
                format!("{CANARY}/{i}"),
                format!("title {i}"),
            ),
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO page_metadata (url, event_id, title, captured_at) VALUES (?1, 'v0001', 'meta', '2026-11-10T00:00:00.000Z')",
        [format!("{CANARY}/1")],
    )
    .unwrap();
    let data: Vec<u8> = (0..1 << 20)
        .map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    db.execute(
        "INSERT INTO image_blobs (id, sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data) VALUES (1, ?1, 'image/png', ?2, 10, 10, 1, 1, ?3)",
        (hex(&Sha256::digest(&data)), data.len() as i64, &data),
    )
    .unwrap();
    db.execute(
        "INSERT INTO image_thumbnails VALUES (1, 'image/webp', 4, 4, 1, x'00010203')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO saved_images (save_id, blob_id, image_url, saved_at, incognito, mcp_public) VALUES ('s1', 1, ?1, 1, 0, 0)",
        [format!("{CANARY}/image.png")],
    )
    .unwrap();
    db.execute_batch(
        "INSERT INTO image_tags (id, name) VALUES (1, 'canary'), (2, 'other');
         INSERT INTO image_tag_links VALUES (1, 1), (1, 2);
         COMMIT;",
    )
    .unwrap();
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn plain_vault(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(&schema::create_sql()).unwrap();
    fill(&db);
    schema::check(&db).unwrap();
}

fn encrypted_vault(path: &Path, key: &VaultKey) {
    cipher::create(path, key).unwrap();
    let db = cipher::open(path, key, rw()).unwrap();
    fill(&db);
    schema::check(&db).unwrap();
}

/// Every row of every table, ordered by primary key, as text: what "the same vault" means.
fn logical(db: &Connection, schema_name: &str) -> String {
    let tables: Vec<String> = db
        .prepare(&format!(
            "SELECT name FROM {schema_name}.sqlite_schema WHERE type = 'table' ORDER BY name"
        ))
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = String::new();
    for table in tables {
        let mut statement = db
            .prepare(&format!(
                "SELECT * FROM {schema_name}.\"{table}\" ORDER BY 1, 2"
            ))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        out.push_str(&table);
        while let Some(row) = rows.next().unwrap() {
            for i in 0..columns {
                let value: rusqlite::types::Value = row.get(i).unwrap();
                out.push_str(&format!("|{value:?}"));
            }
            out.push('\n');
        }
    }
    out
}

fn file_hash(path: &Path) -> String {
    hex(&Sha256::digest(fs::read(path).unwrap()))
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn pragma_i64(db: &Connection, pragma: &str) -> i64 {
    db.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
        .unwrap()
}

fn not_a_database(db: &Connection) -> bool {
    db.query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
        row.get::<_, i64>(0)
    })
    .is_err_and(|error| error.sqlite_error_code() == Some(ErrorCode::NotADatabase))
}

/// P1: plain → encrypted with `sqlcipher_export`. The keyed target is `main`; the plain source is
/// attached read-only (URI `mode=ro`, `KEY ''`). One transaction: the export sees one snapshot.
#[test]
fn p1_plain_to_encrypted_with_sqlcipher_export() {
    let tmp = TempDir::new();
    let source = tmp.0.join("plain.sqlite3");
    plain_vault(&source);
    let before = file_hash(&source);
    let target = tmp.0.join("encrypted.partial");
    fs::File::create_new(&target).unwrap();
    let key = VaultKey::generate().unwrap();
    {
        let db = Connection::open_with_flags(&target, rw_uri()).unwrap();
        cipher::apply_key(&db, &key).unwrap();
        db.execute("ATTACH DATABASE ?1 AS src KEY ''", [read_only_uri(&source)])
            .unwrap();
        db.execute_batch("BEGIN; SELECT sqlcipher_export('main', 'src');")
            .unwrap();
        // SQLCipher 4.5.7's export does not copy the header fields: set from the source.
        assert_eq!(pragma_i64(&db, "main.user_version"), 0);
        assert_eq!(pragma_i64(&db, "main.application_id"), 0);
        assert_eq!(pragma_i64(&db, "src.user_version"), schema::SCHEMA_VERSION);
        db.execute_batch(&format!(
            "PRAGMA main.user_version = {}; PRAGMA main.application_id = {}; COMMIT;",
            schema::SCHEMA_VERSION,
            schema::APP_ID
        ))
        .unwrap();
        assert_eq!(logical(&db, "main"), logical(&db, "src"));
        // The source is attached read-only: no write can reach it.
        let refused = db.execute("DELETE FROM src.visits", []).unwrap_err();
        assert_eq!(refused.sqlite_error_code(), Some(ErrorCode::ReadOnly));
    }
    assert_eq!(file_hash(&source), before, "the source is unchanged");
    assert!(matches!(cipher::read_head(&target).unwrap(), Head::Salt(_)));
    let bytes = fs::read(&target).unwrap();
    assert!(!bytes.windows(13).any(|w| w == b"SLICE-D-CANAR"));
    // Opens with its key only; application_id and user_version came along; fully intact.
    let db = cipher::open(&target, &key, rw()).unwrap();
    schema::check(&db).unwrap();
    assert_eq!(pragma_i64(&db, "application_id"), schema::APP_ID);
    assert_eq!(pragma_i64(&db, "user_version"), schema::SCHEMA_VERSION);
    let integrity: String = db
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(db);
    assert!(not_a_database(
        &cipher::open(&target, &VaultKey::generate().unwrap(), rw()).unwrap()
    ));
    assert!(not_a_database(
        &Connection::open_with_flags(&target, rw()).unwrap()
    ));
    assert_eq!(names(&tmp.0), ["encrypted.partial", "plain.sqlite3"]);
}

/// P2: encrypted → plain. An encrypted database attached without a key is refused by `ATTACH`
/// itself (it reads the schema at once), so it cannot be keyed afterwards, and `ATTACH … KEY` would
/// put the key in SQL. Instead the encrypted source is `main`, opened read-only (`mode=ro`) and
/// keyed with `sqlite3_key_v2`; the plain target is attached (`KEY ''`) and stays writable.
#[test]
fn p2_encrypted_to_plain_with_the_key_never_in_sql() {
    let tmp = TempDir::new();
    let source = tmp.0.join("encrypted.sqlite3");
    let key = VaultKey::generate().unwrap();
    encrypted_vault(&source, &key);
    let before = file_hash(&source);
    let target = tmp.0.join("plain.partial");
    fs::File::create_new(&target).unwrap();
    {
        let refused = Connection::open_with_flags(&target, rw_uri()).unwrap();
        let error = refused
            .execute("ATTACH DATABASE ?1 AS src", [read_only_uri(&source)])
            .unwrap_err();
        assert_eq!(error.sqlite_error_code(), Some(ErrorCode::NotADatabase));
        drop(refused);

        let db = Connection::open_with_flags(read_only_uri(&source), rw_uri()).unwrap();
        cipher::apply_key(&db, &key).unwrap();
        assert!(db.is_readonly(rusqlite::DatabaseName::Main).unwrap());
        db.execute(
            "ATTACH DATABASE ?1 AS dst KEY ''",
            [target.to_string_lossy()],
        )
        .unwrap();
        assert!(!db
            .is_readonly(rusqlite::DatabaseName::Attached("dst"))
            .unwrap());
        db.execute_batch("BEGIN; SELECT sqlcipher_export('dst', 'main');")
            .unwrap();
        db.execute_batch(&format!(
            "PRAGMA dst.user_version = {}; PRAGMA dst.application_id = {}; COMMIT;",
            schema::SCHEMA_VERSION,
            schema::APP_ID
        ))
        .unwrap();
        assert_eq!(logical(&db, "dst"), logical(&db, "main"));
        let refused = db.execute("DELETE FROM main.visits", []).unwrap_err();
        assert_eq!(refused.sqlite_error_code(), Some(ErrorCode::ReadOnly));
    }
    assert_eq!(file_hash(&source), before);
    assert_eq!(cipher::read_head(&target).unwrap(), Head::PlainSqlite);
    let db = Connection::open_with_flags(&target, rw()).unwrap();
    schema::check(&db).unwrap();
    assert_eq!(pragma_i64(&db, "application_id"), schema::APP_ID);
    let integrity: String = db
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(db);
    assert_eq!(names(&tmp.0), ["encrypted.sqlite3", "plain.partial"]);
}

/// P2': a wrongly keyed (or unkeyed) encrypted source is refused at the first read: never a silent
/// empty export. The target stays empty.
#[test]
fn p2_an_unkeyed_or_wrongly_keyed_source_is_refused() {
    let tmp = TempDir::new();
    let source = tmp.0.join("encrypted.sqlite3");
    let key = VaultKey::generate().unwrap();
    encrypted_vault(&source, &key);
    for wrong in [None, Some(VaultKey::generate().unwrap())] {
        let target = tmp
            .0
            .join(format!("t{}.partial", uuid::Uuid::new_v4().simple()));
        fs::File::create_new(&target).unwrap();
        let db = Connection::open_with_flags(read_only_uri(&source), rw_uri()).unwrap();
        if let Some(wrong) = &wrong {
            cipher::apply_key(&db, wrong).unwrap();
        }
        let error = db
            .execute(
                "ATTACH DATABASE ?1 AS dst KEY ''",
                [target.to_string_lossy()],
            )
            .and_then(|_| {
                db.execute_batch("BEGIN; SELECT sqlcipher_export('dst', 'main'); COMMIT;")
            })
            .unwrap_err();
        assert_eq!(error.sqlite_error_code(), Some(ErrorCode::NotADatabase));
        drop(db);
        assert_eq!(fs::metadata(&target).unwrap().len(), 0);
    }
}

/// P3: the attached source, opened read-only by a connection whose main database is the source,
/// cannot take the export (the target would have to be attached to a read-only connection).
#[test]
fn p3_a_read_only_connection_cannot_write_an_attached_target() {
    let tmp = TempDir::new();
    let source = tmp.0.join("plain.sqlite3");
    plain_vault(&source);
    let target = tmp.0.join("t.partial");
    fs::File::create_new(&target).unwrap();
    let db = Connection::open_with_flags(
        &source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    db.execute(
        "ATTACH DATABASE ?1 AS dst KEY ''",
        [target.to_string_lossy()],
    )
    .unwrap();
    let error = db
        .execute_batch("SELECT sqlcipher_export('dst', 'main')")
        .unwrap_err();
    // Reported as a generic error whose message names the read-only database.
    assert!(error.to_string().contains("readonly"), "{error}");
}

/// P4: a source in WAL mode whose last commits are only in its `-wal` (no checkpoint) is exported
/// with them: the export reads through SQLite, unlike Kind C's page streaming.
#[test]
fn p4_committed_wal_content_is_exported() {
    let tmp = TempDir::new();
    let source = tmp.0.join("wal.sqlite3");
    plain_vault(&source);
    let writer = Connection::open(&source).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    writer
        .execute(
            "INSERT INTO visits VALUES ('wal-only', 'https://wal.example/', 'wal', '2026-11-10T00:00:00.000Z')",
            [],
        )
        .unwrap();
    assert!(fs::metadata(tmp.0.join("wal.sqlite3-wal")).unwrap().len() > 0);
    let target = tmp.0.join("t.partial");
    fs::File::create_new(&target).unwrap();
    let key = VaultKey::generate().unwrap();
    let db = Connection::open_with_flags(&target, rw_uri()).unwrap();
    cipher::apply_key(&db, &key).unwrap();
    db.execute("ATTACH DATABASE ?1 AS src KEY ''", [read_only_uri(&source)])
        .unwrap();
    db.execute_batch("BEGIN; SELECT sqlcipher_export('main', 'src'); COMMIT;")
        .unwrap();
    let found: i64 = db
        .query_row(
            "SELECT count(*) FROM main.visits WHERE event_id = 'wal-only'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
    // The target is not in WAL mode: the export copies data, not the journal mode.
    let journal: String = db
        .query_row("PRAGMA main.journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "delete");
}

/// P5: a writer of the source while the export's read transaction is open (rollback journal): it
/// cannot commit until the export ends, so the export is one snapshot; afterwards it commits.
#[test]
fn p5_a_concurrent_writer_waits_for_the_export_and_is_not_lost_from_the_source() {
    let tmp = TempDir::new();
    let source = tmp.0.join("plain.sqlite3");
    plain_vault(&source);
    let target = tmp.0.join("t.partial");
    fs::File::create_new(&target).unwrap();
    let key = VaultKey::generate().unwrap();
    let db = Connection::open_with_flags(&target, rw_uri()).unwrap();
    cipher::apply_key(&db, &key).unwrap();
    db.execute("ATTACH DATABASE ?1 AS src KEY ''", [read_only_uri(&source)])
        .unwrap();
    db.execute_batch("BEGIN; SELECT sqlcipher_export('main', 'src');")
        .unwrap();
    let writer = Connection::open(&source).unwrap();
    writer.busy_timeout(std::time::Duration::ZERO).unwrap();
    let blocked = writer.execute(
        "INSERT INTO visits VALUES ('late', 'https://late.example/', 'late', '2026-11-10T00:00:00.000Z')",
        [],
    );
    assert_eq!(
        blocked.unwrap_err().sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy)
    );
    db.execute_batch("COMMIT").unwrap();
    writer
        .execute(
            "INSERT INTO visits VALUES ('late', 'https://late.example/', 'late', '2026-11-10T00:00:00.000Z')",
            [],
        )
        .unwrap();
    // The export is the snapshot before the late write; the late write is only in the source.
    let in_target: i64 = db
        .query_row(
            "SELECT count(*) FROM main.visits WHERE event_id = 'late'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(in_target, 0);
}

/// P6: the Backup API still refuses a plain ↔ encrypted copy (Slice C §3.1, P1), in both directions.
#[test]
fn p6_the_backup_api_refuses_plain_encrypted_copies() {
    let tmp = TempDir::new();
    let plain = tmp.0.join("plain.sqlite3");
    plain_vault(&plain);
    let key = VaultKey::generate().unwrap();
    let encrypted = tmp.0.join("enc.sqlite3");
    encrypted_vault(&encrypted, &key);

    let source = Connection::open_with_flags(&plain, rw()).unwrap();
    let target_path = tmp.0.join("a.partial");
    fs::File::create_new(&target_path).unwrap();
    let mut target = cipher::open(&target_path, &key, rw()).unwrap();
    let refused =
        rusqlite::backup::Backup::new(&source, &mut target).and_then(|backup| backup.step(-1));
    assert!(refused.is_err());

    let source = cipher::open(&encrypted, &key, rw()).unwrap();
    let target_path = tmp.0.join("b.partial");
    fs::File::create_new(&target_path).unwrap();
    let mut target = Connection::open_with_flags(&target_path, rw()).unwrap();
    let refused =
        rusqlite::backup::Backup::new(&source, &mut target).and_then(|backup| backup.step(-1));
    assert!(refused.is_err());
}

/// P7: rows copied with `INSERT … SELECT` into a database that already holds the schema (keyed or
/// plain), in one transaction: the same result as the export, and the target keeps its salt. This
/// is what refreshing an already converted (salted, sidecar-bound) vault needs.
#[test]
fn p7_insert_select_refresh_keeps_the_target_salt() {
    let tmp = TempDir::new();
    let source = tmp.0.join("plain.sqlite3");
    plain_vault(&source);
    let key = VaultKey::generate().unwrap();
    let target = tmp.0.join("t.partial");
    let salt = cipher::create(&target, &key).unwrap();
    let db = Connection::open_with_flags(&target, rw_uri()).unwrap();
    cipher::apply_key(&db, &key).unwrap();
    db.execute("ATTACH DATABASE ?1 AS src KEY ''", [read_only_uri(&source)])
        .unwrap();
    let tables = [
        "visits",
        "tab_snapshots",
        "image_blobs",
        "saved_images",
        "image_thumbnails",
        "page_metadata",
        "navigation_events",
        "image_tags",
        "image_tag_links",
    ];
    for _ in 0..2 {
        let mut batch = String::from("BEGIN IMMEDIATE;");
        for table in tables {
            batch.push_str(&format!("DELETE FROM main.{table};"));
        }
        for table in tables {
            batch.push_str(&format!(
                "INSERT INTO main.{table} SELECT * FROM src.{table};"
            ));
        }
        batch.push_str("COMMIT;");
        db.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        db.execute_batch(&batch).unwrap();
        assert_eq!(logical(&db, "main"), logical(&db, "src"));
    }
    drop(db);
    assert_eq!(cipher::read_head(&target).unwrap(), Head::Salt(salt));
}

// --- The engine (`secure_vault::convert`) ------------------------------------------------------

use memocampus_native_host::active_vault::vault_token;
use memocampus_native_host::secure_vault::convert::{self, CopyError, Ends};

/// E1: both directions, under a folder name with Japanese characters, with the source's bytes
/// and folder unchanged; plain → encrypted → plain gives back the same rows.
#[test]
fn e1_export_both_ways_keeps_every_row_and_never_touches_the_source() {
    let tmp = TempDir::new();
    let dir = tmp.0.join("記憶 #1 %");
    fs::create_dir_all(&dir).unwrap();
    let plain = dir.join("plain.sqlite3");
    plain_vault(&plain);
    let before = file_hash(&plain);
    let key = VaultKey::generate().unwrap();
    let encrypted = dir.join("enc.partial");
    fs::File::create_new(&encrypted).unwrap();
    convert::export(&Ends {
        source: &plain,
        source_key: None,
        source_token: &vault_token(&plain).unwrap(),
        target: &encrypted,
        target_key: Some(&key),
    })
    .unwrap();
    convert::verify(&encrypted, Some(&key)).unwrap();
    assert_eq!(convert::verify(&encrypted, None), Err(CopyError::Mismatch));
    assert_eq!(
        convert::verify(&encrypted, Some(&VaultKey::generate().unwrap())),
        Err(CopyError::Keying)
    );
    assert_eq!(file_hash(&plain), before);
    let back = dir.join("back.partial");
    fs::File::create_new(&back).unwrap();
    let encrypted_before = file_hash(&encrypted);
    convert::export(&Ends {
        source: &encrypted,
        source_key: Some(&key),
        source_token: &vault_token(&encrypted).unwrap(),
        target: &back,
        target_key: None,
    })
    .unwrap();
    convert::verify(&back, None).unwrap();
    assert_eq!(file_hash(&encrypted), encrypted_before);
    let a = Connection::open(&plain).unwrap();
    let b = Connection::open(&back).unwrap();
    assert_eq!(logical(&a, "main"), logical(&b, "main"));
    assert_eq!(
        names(&dir),
        ["back.partial", "enc.partial", "plain.sqlite3"]
    );
}

/// E2: a source that is not the identified file is refused, and the target stays empty.
#[test]
fn e2_another_source_file_is_refused_before_anything_is_copied() {
    let tmp = TempDir::new();
    let plain = tmp.0.join("plain.sqlite3");
    plain_vault(&plain);
    let other = tmp.0.join("other.sqlite3");
    plain_vault(&other);
    let target = tmp.0.join("t.partial");
    fs::File::create_new(&target).unwrap();
    let key = VaultKey::generate().unwrap();
    let result = convert::export(&Ends {
        source: &plain,
        source_key: None,
        source_token: &vault_token(&other).unwrap(),
        target: &target,
        target_key: Some(&key),
    });
    assert_eq!(result, Err(CopyError::SourceChanged));
    assert!(convert::verify(&target, Some(&key)).is_err());
}

/// E3: refresh brings later writes of the source into the converted copy, which keeps its salt;
/// a target whose rows differ from the source is detected by `compare`.
#[test]
fn e3_refresh_catches_up_with_the_source() {
    let tmp = TempDir::new();
    let plain = tmp.0.join("plain.sqlite3");
    plain_vault(&plain);
    let token = vault_token(&plain).unwrap();
    let key = VaultKey::generate().unwrap();
    let target = tmp.0.join("t.partial");
    fs::File::create_new(&target).unwrap();
    let ends = Ends {
        source: &plain,
        source_key: None,
        source_token: &token,
        target: &target,
        target_key: Some(&key),
    };
    convert::export(&ends).unwrap();
    let Head::Salt(salt) = cipher::read_head(&target).unwrap() else {
        panic!("encrypted")
    };
    let writer = Connection::open(&plain).unwrap();
    writer
        .execute_batch(
            "INSERT INTO visits VALUES ('later', 'https://later.example/', 'later', '2026-11-10T00:00:00.000Z');
             DELETE FROM image_tag_links WHERE tag_id = 2;
             UPDATE page_metadata SET title = 'changed';",
        )
        .unwrap();
    {
        let db = cipher::open(&target, &key, rw()).unwrap();
        db.execute(
            "ATTACH DATABASE ?1 AS src KEY ''",
            [convert::read_only_uri(&plain)],
        )
        .unwrap();
        assert_eq!(
            convert::compare(&db, "main", "src"),
            Err(CopyError::Mismatch)
        );
    }
    convert::refresh(&ends).unwrap();
    convert::verify(&target, Some(&key)).unwrap();
    assert_eq!(cipher::read_head(&target).unwrap(), Head::Salt(salt));
    let db = cipher::open(&target, &key, rw()).unwrap();
    db.execute(
        "ATTACH DATABASE ?1 AS src KEY ''",
        [convert::read_only_uri(&plain)],
    )
    .unwrap();
    convert::compare(&db, "main", "src").unwrap();
    let tags: i64 = db
        .query_row("SELECT count(*) FROM main.image_tags", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tags, 1, "the unused tag went with its last link");
}

/// E4: a source with a hot journal (a writer died mid-transaction) is first rolled back by an
/// ordinary read-write open (as the host opens the active vault before a conversion); the copy is
/// then the last committed state, never the half-written one.
#[test]
fn e4_a_hot_journal_is_rolled_back_before_the_copy() {
    let tmp = TempDir::new();
    let live = tmp.0.join("live.sqlite3");
    plain_vault(&live);
    let committed = {
        let db = Connection::open(&live).unwrap();
        logical(&db, "main")
    };
    let crashed = tmp.0.join("crashed");
    fs::create_dir_all(&crashed).unwrap();
    {
        let db = Connection::open(&live).unwrap();
        db.execute_batch(
            "PRAGMA cache_size = 1; BEGIN; DELETE FROM visits; DELETE FROM image_thumbnails;",
        )
        .unwrap();
        let blob = vec![7u8; 200_000];
        db.execute(
            "INSERT INTO image_blobs (id, sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data) VALUES (9, ?1, 'image/png', 1, 1, 1, 1, 1, ?2)",
            ("9".repeat(64), &blob),
        )
        .unwrap();
        // The state a crash leaves: database pages partly overwritten, the journal still there.
        assert!(
            fs::metadata(tmp.0.join("live.sqlite3-journal"))
                .unwrap()
                .len()
                > 0
        );
        fs::copy(&live, crashed.join("v.sqlite3")).unwrap();
        fs::copy(
            tmp.0.join("live.sqlite3-journal"),
            crashed.join("v.sqlite3-journal"),
        )
        .unwrap();
        db.execute_batch("ROLLBACK").unwrap();
    }
    let source = crashed.join("v.sqlite3");
    // As `Session::ticket` does before a conversion: a read-write open, which rolls back.
    let opened = Connection::open(&source).unwrap();
    assert_eq!(logical(&opened, "main"), committed);
    drop(opened);
    assert!(!crashed.join("v.sqlite3-journal").exists());
    let key = VaultKey::generate().unwrap();
    let target = crashed.join("t.partial");
    fs::File::create_new(&target).unwrap();
    convert::export(&Ends {
        source: &source,
        source_key: None,
        source_token: &vault_token(&source).unwrap(),
        target: &target,
        target_key: Some(&key),
    })
    .unwrap();
    let db = cipher::open(&target, &key, rw()).unwrap();
    assert_eq!(logical(&db, "main"), committed);
}

/// E5: a source held exclusively by another process (an open handle without sharing) cannot be
/// read: the copy fails, nothing usable is written.
#[cfg(windows)]
#[test]
fn e5_a_source_locked_by_another_process_is_refused() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = TempDir::new();
    let plain = tmp.0.join("plain.sqlite3");
    plain_vault(&plain);
    let token = vault_token(&plain).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&plain)
        .unwrap();
    let target = tmp.0.join("t.partial");
    fs::File::create_new(&target).unwrap();
    let key = VaultKey::generate().unwrap();
    let result = convert::export(&Ends {
        source: &plain,
        source_key: None,
        source_token: &token,
        target: &target,
        target_key: Some(&key),
    });
    assert!(result.is_err());
    drop(held);
    assert!(convert::verify(&target, Some(&key)).is_err());
}
