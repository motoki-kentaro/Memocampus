//! Issue #51, Slice E: the PoC experiments behind "the encrypted vault file and its Recovery Spell
//! are all a recovery needs" (`docs/secure-vault/slice-e.md` §2). Kept as tests, so that an engine
//! or crate upgrade that changes any of them fails here first. Temporary directories only; the
//! spell bytes here are fixed test vectors, never a user's.
//!
//! - P1: `VK = HKDF-SHA256(ikm = spell, salt = public salt, info = "memocampus/vault-key/v3")` is
//!   deterministic, and changes with the spell, the salt or the label.
//! - P2/P3: SQLCipher's raw key with an explicit salt (`x'<64 hex key><32 hex salt>'`) creates a
//!   database whose first 16 bytes are that salt; the file alone then gives the salt back, and the
//!   raw key (`x'<64 hex>'`) opens it with no key derivation by SQLCipher (no PBKDF2 of a
//!   passphrase). The alternatives (plaintext header, an app header in front) are measured too.
//! - P4: re-encryption into a new vault with a new spell and salt (`sqlcipher_export`), and a
//!   restore that changes the salt (and so the vault id) while the same spell still opens it.

use hkdf::Hkdf;
use memocampus_native_host::schema;
use memocampus_native_host::secure_vault::cipher::{self, Head};
use memocampus_native_host::secure_vault::secret::VaultKey;
use rusqlite::{ffi, Connection, ErrorCode, OpenFlags};
use sha2::{Digest, Sha256};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::fs;
use std::path::{Path, PathBuf};

extern "C" {
    fn sqlite3_key_v2(
        db: *mut ffi::sqlite3,
        db_name: *const c_char,
        key: *const c_void,
        key_len: c_int,
    ) -> c_int;
}

const INFO: &[u8] = b"memocampus/vault-key/v3";
const CANARY: &str = "https://canary.example/SLICE-E-CANARY";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-e-poc-{}", uuid::Uuid::new_v4()));
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn derive(spell: &[u8; 32], salt: &[u8; 16], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt), spell)
        .expand(info, &mut out)
        .unwrap();
    out
}

fn vault_key(bytes: [u8; 32]) -> VaultKey {
    let mut bytes = bytes;
    VaultKey::from_bytes(&mut bytes)
}

/// Keys `schema` of `db` with a key literal, through the C API (as `cipher::apply_key_to` does).
fn key_with(db: &Connection, schema: &CStr, literal: &str) -> c_int {
    // SAFETY: the handle belongs to `db`; the literal outlives the call.
    unsafe {
        sqlite3_key_v2(
            db.handle(),
            schema.as_ptr(),
            literal.as_ptr().cast(),
            literal.len() as c_int,
        )
    }
}

fn raw(key: &[u8; 32]) -> String {
    format!("x'{}'", hex(key))
}

fn raw_salted(key: &[u8; 32], salt: &[u8; 16]) -> String {
    format!("x'{}{}'", hex(key), hex(salt))
}

fn open_with(path: &Path, literal: &str) -> Connection {
    let db = Connection::open_with_flags(path, rw()).unwrap();
    assert_eq!(key_with(&db, c"main", literal), ffi::SQLITE_OK);
    db.execute_batch("PRAGMA cipher_compatibility = 4; PRAGMA temp_store = MEMORY;")
        .unwrap();
    db
}

fn refused(db: &Connection) -> bool {
    db.query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
        row.get::<_, i64>(0)
    })
    .is_err_and(|error| error.sqlite_error_code() == Some(ErrorCode::NotADatabase))
}

fn fill(db: &Connection) {
    db.execute_batch(&format!("BEGIN; {} ", schema::create_sql()))
        .unwrap();
    for i in 0..100 {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, 'title', '2026-11-10T00:00:00.000Z')",
            (format!("v{i:03}"), format!("{CANARY}/{i}")),
        )
        .unwrap();
    }
    let data: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8)
        .collect();
    db.execute(
        "INSERT INTO image_blobs (id, sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data) VALUES (1, ?1, 'image/png', ?2, 10, 10, 1, 1, ?3)",
        (hex(&Sha256::digest(&data)), data.len() as i64, &data),
    )
    .unwrap();
    db.execute_batch("COMMIT").unwrap();
}

/// Creates a vault at `path` keyed with `key` and the explicit `salt`.
fn create(path: &Path, key: &[u8; 32], salt: &[u8; 16]) {
    let db = Connection::open_with_flags(path, rw() | OpenFlags::SQLITE_OPEN_CREATE).unwrap();
    assert_eq!(
        key_with(&db, c"main", &raw_salted(key, salt)),
        ffi::SQLITE_OK
    );
    db.execute_batch("PRAGMA cipher_compatibility = 4; PRAGMA temp_store = MEMORY;")
        .unwrap();
    fill(&db);
    schema::check(&db).unwrap();
}

fn head_salt(path: &Path) -> [u8; 16] {
    match cipher::read_head(path).unwrap() {
        Head::Salt(salt) => *salt.as_bytes(),
        other => panic!("{other:?}"),
    }
}

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
    let schema_sql: Vec<String> = db
        .prepare(&format!(
            "SELECT type || name || ifnull(sql, '') FROM {schema_name}.sqlite_schema ORDER BY name"
        ))
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    out.push_str(&schema_sql.join("\n"));
    out
}

/// P1: deterministic, and every input matters.
#[test]
fn p1_the_vault_key_is_a_function_of_the_spell_and_the_public_salt() {
    let spell = [0x11u8; 32];
    let salt = [0x22u8; 16];
    let vk = derive(&spell, &salt, INFO);
    assert_eq!(vk, derive(&spell, &salt, INFO), "deterministic");
    let mut other_spell = spell;
    other_spell[31] ^= 1;
    let mut other_salt = salt;
    other_salt[0] ^= 1;
    assert_ne!(vk, derive(&other_spell, &salt, INFO));
    assert_ne!(vk, derive(&spell, &other_salt, INFO));
    assert_ne!(vk, derive(&spell, &salt, b"memocampus/vault-key/v4"));
    assert_ne!(
        vk,
        derive(&spell, &salt, b"memocampus/recovery-spell/v2/kek")
    );
    assert_ne!(&vk, &spell, "the spell is never the key");
    // A fixed vector, so that the derivation can never change silently.
    assert_eq!(
        hex(&vk),
        hex(&{
            let prk = {
                use hmac::{Hmac, Mac};
                let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&salt).unwrap();
                mac.update(&spell);
                mac.finalize().into_bytes()
            };
            use hmac::{Hmac, Mac};
            let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&prk).unwrap();
            mac.update(INFO);
            mac.update(&[1]);
            let t1: [u8; 32] = mac.finalize().into_bytes().into();
            t1
        }),
        "RFC 5869 by hand: HKDF-Extract then the first block of HKDF-Expand"
    );
}

/// P2/P3: an explicit salt is written as the first 16 bytes, the raw key opens the file with no
/// other input, and nothing but the right key and salt does.
#[test]
fn p2_p3_the_salt_lives_in_the_file_and_the_raw_key_opens_it() {
    let tmp = TempDir::new();
    let spell = [0x33u8; 32];
    let salt = [0x44u8; 16];
    let vk = derive(&spell, &salt, INFO);
    let path = tmp.0.join("vault.sqlite3");
    create(&path, &vk, &salt);
    assert_eq!(
        head_salt(&path),
        salt,
        "the explicit salt is the file header"
    );
    let bytes = fs::read(&path).unwrap();
    assert!(!bytes.windows(6).any(|w| w == b"CANARY"));
    assert!(!bytes.windows(32).any(|w| w == vk), "no key in the file");
    assert!(
        !bytes.windows(32).any(|w| w == spell),
        "no spell in the file"
    );

    // Reopened from the file alone: salt from the head, key from the spell and that salt.
    let derived = derive(&spell, &head_salt(&path), INFO);
    let db = open_with(&path, &raw(&derived));
    let salt_pragma: String = db
        .query_row("PRAGMA cipher_salt", [], |row| row.get(0))
        .unwrap();
    assert_eq!(salt_pragma, hex(&salt));
    let count: i64 = db
        .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 100);
    let ok: String = db
        .query_row("PRAGMA cipher_integrity_check", [], |row| row.get(0))
        .unwrap_or_else(|_| "ok".into());
    assert_eq!(ok, "ok");
    drop(db);
    // The same key with its salt given explicitly also opens it; another salt does not.
    assert!(!refused(&open_with(&path, &raw_salted(&derived, &salt))));
    assert!(refused(&open_with(
        &path,
        &raw_salted(&derived, &[0x45; 16])
    )));
    // Raw key vs passphrase: the same 64 hex digits as a passphrase are a different key
    // (SQLCipher would run PBKDF2 on them), so they never open the vault.
    assert!(refused(&open_with(&path, &hex(&derived))));
    // A wrong spell, or the key of the right spell with a wrong salt: refused.
    assert!(refused(&open_with(
        &path,
        &raw(&derive(&[0x34; 32], &salt, INFO))
    )));
    assert!(refused(&open_with(
        &path,
        &raw(&derive(&spell, &[0x45; 16], INFO))
    )));
    assert_eq!(fs::read(&path).unwrap(), bytes, "refusals change nothing");

    // Moved, renamed and copied alone into another directory: still opens.
    let elsewhere = TempDir::new();
    let moved = elsewhere.0.join("ふっかつ #1 % copy.db");
    fs::copy(&path, &moved).unwrap();
    assert_eq!(fs::read_dir(&elsewhere.0).unwrap().count(), 1, "one file");
    let db = open_with(&moved, &raw(&derive(&spell, &head_salt(&moved), INFO)));
    schema::check(&db).unwrap();
    // The project's `cipher::open` takes the derived key as it is.
    drop(db);
    let db = cipher::open(&moved, &vault_key(derived), rw()).unwrap();
    schema::check(&db).unwrap();
}

/// P2: tampering is detected; a changed salt is a different key, never a silent misread.
#[test]
fn p2_tampering_with_the_salt_or_a_page_is_refused() {
    let tmp = TempDir::new();
    let spell = [0x55u8; 32];
    let salt = [0x66u8; 16];
    let vk = derive(&spell, &salt, INFO);
    let path = tmp.0.join("vault.sqlite3");
    create(&path, &vk, &salt);
    let original = fs::read(&path).unwrap();

    // A salt byte changed: the derived key changes and so does SQLCipher's HMAC key.
    let mut bytes = original.clone();
    bytes[3] ^= 0x80;
    fs::write(&path, &bytes).unwrap();
    assert!(refused(&open_with(
        &path,
        &raw(&derive(&spell, &head_salt(&path), INFO))
    )));
    assert!(refused(&open_with(&path, &raw(&vk))), "the old key neither");

    // A byte of the last page changed: the page HMAC fails when that page is read.
    let mut bytes = original.clone();
    let last = bytes.len() - 100;
    bytes[last] ^= 1;
    fs::write(&path, &bytes).unwrap();
    let db = open_with(&path, &raw(&vk));
    let read = db.query_row("SELECT sum(length(data)) FROM image_blobs", [], |row| {
        row.get::<_, i64>(0)
    });
    let check: Vec<String> = db
        .prepare("PRAGMA cipher_integrity_check")
        .and_then(|mut s| {
            s.query_map([], |row| row.get(0))
                .map(|rows| rows.map(Result::unwrap).collect())
        })
        .unwrap_or_default();
    let quick = db.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0));
    assert!(
        read.is_err() || !check.is_empty() || quick.is_err(),
        "a damaged page is detected ({read:?} {check:?} {quick:?})"
    );
}

/// P2 alternatives: SQLCipher's plaintext header leaves the salt out of the file (it must then be
/// stored elsewhere), and a non-SQLite prefix in front of the file makes it unreadable by
/// SQLCipher. Measured to reject them.
#[test]
fn p2_alternatives_lose_the_salt_or_the_sqlcipher_format() {
    let tmp = TempDir::new();
    let spell = [0x77u8; 32];
    let salt = [0x88u8; 16];
    let vk = derive(&spell, &salt, INFO);
    let path = tmp.0.join("header.sqlite3");
    {
        let db = Connection::open_with_flags(&path, rw() | OpenFlags::SQLITE_OPEN_CREATE).unwrap();
        assert_eq!(
            key_with(&db, c"main", &raw_salted(&vk, &salt)),
            ffi::SQLITE_OK
        );
        db.execute_batch(
            "PRAGMA cipher_compatibility = 4; PRAGMA cipher_plaintext_header_size = 32;",
        )
        .unwrap();
        fill(&db);
    }
    let bytes = fs::read(&path).unwrap();
    assert_eq!(
        &bytes[..16],
        cipher::SQLITE_MAGIC,
        "the header is plain SQLite"
    );
    assert!(
        !bytes[..64].windows(16).any(|w| w == salt),
        "the salt is not in the file: it would need a sidecar or a fixed location"
    );
    // Measured on SQLCipher 4.5.7: with a plaintext header the explicit salt is not kept at all;
    // the salt in use is the constant SQLite magic, the same for every vault. A per-vault public
    // salt would then have to live outside the file (a sidecar again), so this option is rejected.
    let db = open_with(&path, &raw(&vk));
    db.execute_batch("PRAGMA cipher_plaintext_header_size = 32;")
        .unwrap();
    assert!(!refused(&db));
    let salt_seen: String = db
        .query_row("PRAGMA cipher_salt", [], |row| row.get(0))
        .unwrap();
    assert_eq!(salt_seen, hex(cipher::SQLITE_MAGIC));
    assert_ne!(salt_seen, hex(&salt));
    drop(db);

    // An application header in front: not a SQLCipher file any more (tools and SQLCipher refuse).
    let prefixed = tmp.0.join("prefixed.sqlite3");
    let plain = tmp.0.join("plain-salt.sqlite3");
    create(&plain, &vk, &salt);
    let mut with_prefix = b"MEMOCAMPUS-V3\0\0\0".to_vec();
    with_prefix.extend(fs::read(&plain).unwrap());
    fs::write(&prefixed, &with_prefix).unwrap();
    assert!(refused(&open_with(&prefixed, &raw(&vk))));
}

/// Exports `source` (keyed with `source_key`) into a new vault at `target` keyed with `key` and
/// `salt`. The source is the read-only `main`, the target attached and keyed through the C API.
fn export(source: &Path, source_key: &[u8; 32], target: &Path, key: &[u8; 32], salt: &[u8; 16]) {
    fs::File::create_new(target).unwrap();
    // Read-only by URI, so that the attached target stays writable.
    let db = Connection::open_with_flags(read_only_uri(source), rw() | OpenFlags::SQLITE_OPEN_URI)
        .unwrap();
    assert_eq!(key_with(&db, c"main", &raw(source_key)), ffi::SQLITE_OK);
    db.execute_batch("PRAGMA cipher_compatibility = 4; PRAGMA temp_store = MEMORY;")
        .unwrap();
    db.execute(
        "ATTACH DATABASE ?1 AS dst KEY ''",
        [target.to_str().unwrap()],
    )
    .unwrap();
    assert_eq!(
        key_with(&db, c"dst", &raw_salted(key, salt)),
        ffi::SQLITE_OK
    );
    db.execute_batch(
        "PRAGMA dst.cipher_compatibility = 4; BEGIN; SELECT sqlcipher_export('dst', 'main');",
    )
    .unwrap();
    let version: i64 = db
        .query_row("PRAGMA main.user_version", [], |row| row.get(0))
        .unwrap();
    let app: i64 = db
        .query_row("PRAGMA main.application_id", [], |row| row.get(0))
        .unwrap();
    db.execute_batch(&format!(
        "PRAGMA dst.user_version = {version}; PRAGMA dst.application_id = {app}; COMMIT; DETACH DATABASE dst;"
    ))
    .unwrap();
}

/// P4: a new spell and salt give a new vault with the same content; only the new spell opens it;
/// the source is untouched. The same holds for a restore that changes only the salt.
#[test]
fn p4_reencryption_and_a_resalted_restore() {
    let tmp = TempDir::new();
    let (old_spell, old_salt) = ([0x91u8; 32], [0x92u8; 16]);
    let old_key = derive(&old_spell, &old_salt, INFO);
    let source = tmp.0.join("old.sqlite3");
    create(&source, &old_key, &old_salt);
    let source_bytes = fs::read(&source).unwrap();
    let expected = logical(&open_with(&source, &raw(&old_key)), "main");

    // Re-issue: new spell, new salt.
    let (new_spell, new_salt) = ([0xa1u8; 32], [0xa2u8; 16]);
    let new_key = derive(&new_spell, &new_salt, INFO);
    let target = tmp.0.join("new.sqlite3");
    export(&source, &old_key, &target, &new_key, &new_salt);
    assert_eq!(head_salt(&target), new_salt);
    assert_eq!(fs::read(&source).unwrap(), source_bytes, "source unchanged");
    let reopened = open_with(
        &target,
        &raw(&derive(&new_spell, &head_salt(&target), INFO)),
    );
    schema::check(&reopened).unwrap();
    assert_eq!(logical(&reopened, "main"), expected);
    let ok: String = reopened
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    drop(reopened);
    assert!(refused(&open_with(
        &target,
        &raw(&derive(&old_spell, &head_salt(&target), INFO))
    )));
    assert!(
        refused(&open_with(&target, &raw(&old_key))),
        "old VK never reused"
    );
    // The old spell still opens the old vault (no revocation of copies).
    assert!(!refused(&open_with(
        &source,
        &raw(&derive(&old_spell, &head_salt(&source), INFO))
    )));

    // Restore with a new salt (a new vault id): same spell, new salt, new key; the spell opens it.
    let restored = tmp.0.join("restored.sqlite3");
    let restored_salt = [0xb2u8; 16];
    export(
        &source,
        &old_key,
        &restored,
        &derive(&old_spell, &restored_salt, INFO),
        &restored_salt,
    );
    let db = open_with(
        &restored,
        &raw(&derive(&old_spell, &head_salt(&restored), INFO)),
    );
    assert_eq!(logical(&db, "main"), expected);
}

/// P4: committed WAL frames of the source are part of the export.
#[test]
fn p4_wal_commits_are_exported() {
    let tmp = TempDir::new();
    let (spell, salt) = ([0xc1u8; 32], [0xc2u8; 16]);
    let key = derive(&spell, &salt, INFO);
    let source = tmp.0.join("wal.sqlite3");
    create(&source, &key, &salt);
    let writer = open_with(&source, &raw(&key));
    let mode: String = writer
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer
        .execute(
            "INSERT INTO visits VALUES ('wal-1', ?1, 'wal', '2026-11-10T00:00:00.000Z')",
            [format!("{CANARY}/wal")],
        )
        .unwrap();
    assert!(fs::metadata(tmp.0.join("wal.sqlite3-wal")).unwrap().len() > 0);
    let target = tmp.0.join("target.sqlite3");
    let (new_spell, new_salt) = ([0xd1u8; 32], [0xd2u8; 16]);
    export(
        &source,
        &key,
        &target,
        &derive(&new_spell, &new_salt, INFO),
        &new_salt,
    );
    drop(writer);
    let db = open_with(&target, &raw(&derive(&new_spell, &new_salt, INFO)));
    let found: i64 = db
        .query_row(
            "SELECT count(*) FROM visits WHERE event_id = 'wal-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
    let mode: String = db
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete", "the new vault is not in WAL mode");
}
