//! S3-01: a vault of another schema is refused before SQLite may recover (write) it, byte for byte
//! unchanged with its journal or WAL; a schema-10 vault still recovers. Temporary vaults only.

use super::*;
use crate::vault::tests::{crashed_copy, schema_9_sql, TempDir};
use crate::vault::{
    open_checked_in, open_vault_detailed, open_vault_in, sidecar, write_config, Config,
    Environment, KeyError, KeyRequest, KeySource, Session, BEFORE_FIRST_READ,
};
use memocampus_native_host::active_vault::VaultMode;
use memocampus_native_host::schema;
use memocampus_native_host::secure_vault::secret::{CipherSalt, VaultId};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn key() -> VaultKey {
    VaultKey::from_bytes(&mut [7; 32])
}

struct TestKey;
impl KeySource for TestKey {
    fn key_for(&self, _: &KeyRequest) -> Result<VaultKey, KeyError> {
        Ok(key())
    }
}

struct WrongKey;
impl KeySource for WrongKey {
    fn key_for(&self, _: &KeyRequest) -> Result<VaultKey, KeyError> {
        Ok(VaultKey::from_bytes(&mut [8; 32]))
    }
}

const VAULT_ID: VaultId = VaultId::from_array([1; 16]);

fn rw() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
}

/// Every file of `dir` with its bytes: what must not change (nor appear, such as a `-shm`).
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

/// A connection to a new vault at `path` made from `sql`: plain, or keyed with `key()`.
fn made(path: &Path, sql: &str, encrypted: bool) -> Connection {
    fs::File::create_new(path).unwrap();
    let db = Connection::open_with_flags(path, rw()).unwrap();
    if encrypted {
        cipher::apply_new_key_to(&db, c"main", &key(), &CipherSalt::from_array([2; 16])).unwrap();
        db.execute_batch("PRAGMA cipher_compatibility = 4; PRAGMA temp_store = MEMORY;")
            .unwrap();
    }
    db.execute_batch(sql).unwrap();
    db
}

/// `dir/crash.sqlite3` (+ `-journal`) left by a crash of a vault of `sql`, alone in `dir`.
fn crash(dir: &Path, sql: &str, encrypted: bool) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let source = dir.with_extension("source.sqlite3");
    let db = made(&source, sql, encrypted);
    let target = dir.join("crash.sqlite3");
    crashed_copy(&db, &source, &target);
    drop(db);
    target
}

fn open(path: &Path, encrypted: bool) -> Result<Connection, OpenFailure> {
    if encrypted {
        open_vault_in(path, VaultMode::Encrypted, Some(VAULT_ID), &TestKey)
    } else {
        open_vault_detailed(path)
    }
}

fn restorable_pages(path: &Path) -> usize {
    restorable(path).unwrap().pages.len()
}

#[test]
fn a_schema_9_vault_with_a_hot_journal_is_refused_unchanged_through_every_way_of_opening_it() {
    for encrypted in [false, true] {
        let tmp = TempDir::new();
        let dir = tmp.0.join("vault");
        let path = crash(&dir, &schema_9_sql(), encrypted);
        let before = files(&dir);
        assert!(before.contains_key("crash.sqlite3-journal"));
        assert!(restorable_pages(&path) > 0, "the journal restores page 1");

        // Directly (both modes), and as the panel's "open" does (plain).
        let refused = open(&path, encrypted).unwrap_err();
        assert_eq!(
            (refused.kind, refused.cause.clone()),
            (VaultError::SchemaMismatch, OpenCause::UnsupportedVersion(9)),
            "{encrypted}"
        );
        assert!(!refused.needs_handle_fence());
        if !encrypted {
            assert!(refused.proves_unwritable());
            assert!(open_checked_in(&path, VaultMode::Plain, None, &TestKey).is_err());
        }
        assert_eq!(files(&dir), before, "{encrypted}");

        // Through the config, as every operation opens the active vault.
        let env = tmp.env();
        let config = if encrypted {
            Config::encrypted(path.to_string_lossy().into_owned(), false, VAULT_ID)
        } else {
            Config::new(path.to_string_lossy().into_owned(), false, VaultMode::Plain)
        };
        write_config(&env, &config).unwrap();
        let mut session = Session::with_keys(env, Box::new(TestKey));
        assert_eq!(
            session.read(|_| ()).unwrap_err(),
            VaultError::SchemaMismatch
        );
        assert_eq!(files(&dir), before, "{encrypted}");

        // Through the development override (always plain).
        if !encrypted {
            let mut overridden = Session::new(Environment {
                db_override: Some(path.clone()),
                ..tmp.env()
            });
            assert_eq!(
                overridden.read(|_| ()).unwrap_err(),
                VaultError::SchemaMismatch
            );
            assert_eq!(files(&dir), before);
        }

        // The control: SQLite itself, opened read-write, rewrites the very same files at its
        // first read (which is what the check prevents).
        let control = tmp.0.join("control");
        fs::create_dir(&control).unwrap();
        for (name, bytes) in &before {
            fs::write(control.join(name), bytes).unwrap();
        }
        let raw = if encrypted {
            cipher::open(&control.join("crash.sqlite3"), &key(), rw()).unwrap()
        } else {
            Connection::open_with_flags(control.join("crash.sqlite3"), rw()).unwrap()
        };
        let version: i64 = raw
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 9);
        drop(raw);
        assert_ne!(files(&control), before, "SQLite recovered (wrote) it");
    }
}

#[test]
fn a_schema_10_vault_with_a_hot_journal_still_recovers() {
    for encrypted in [false, true] {
        let tmp = TempDir::new();
        let dir = tmp.0.join("vault");
        let path = crash(&dir, &schema::create_sql(), encrypted);
        assert!(restorable_pages(&path) > 0);
        let db = open(&path, encrypted).unwrap();
        let rows: Vec<String> = db
            .prepare("SELECT event_id FROM visits")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            ["committed"],
            "rolled back to the last commit ({encrypted})"
        );
        let integrity: String = db
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        drop(db);
        assert!(
            !sidecar(&path, "-journal").exists(),
            "the journal was played back"
        );
        assert!(open(&path, encrypted).is_ok());
    }
}

#[test]
fn a_journal_that_would_restore_another_schema_is_refused_even_under_a_schema_10_header() {
    // A schema-9 crash whose file header was relabelled 10: only the journal's page 1 tells.
    let tmp = TempDir::new();
    let dir = tmp.0.join("vault");
    let path = crash(&dir, &schema_9_sql(), false);
    let mut bytes = fs::read(&path).unwrap();
    bytes[60..64].copy_from_slice(&10u32.to_be_bytes());
    fs::write(&path, &bytes).unwrap();
    let before = files(&dir);
    let refused = open_vault_detailed(&path).unwrap_err();
    assert_eq!(refused.cause, OpenCause::UnsupportedVersion(9));
    assert_eq!(files(&dir), before);

    // Encrypted: a schema-10 file beside the journal of a schema-9 vault of the same key.
    let enc = tmp.0.join("enc");
    let v9 = crash(&enc.join("v9"), &schema_9_sql(), true);
    let v10 = crash(&enc.join("v10"), &schema::create_sql(), true);
    fs::copy(sidecar(&v9, "-journal"), sidecar(&v10, "-journal")).unwrap();
    let before = files(&enc.join("v10"));
    let refused = open(&v10, true).unwrap_err();
    assert_eq!(
        (refused.kind, refused.cause, refused.mode),
        (
            VaultError::SchemaMismatch,
            OpenCause::UnsupportedVersion(9),
            VaultMode::Encrypted
        )
    );
    assert_eq!(files(&enc.join("v10")), before);
}

#[test]
fn a_sidecar_that_is_no_journal_or_wal_is_refused_and_left_alone() {
    for encrypted in [false, true] {
        let tmp = TempDir::new();
        let path = tmp.0.join("v.sqlite3");
        drop(made(&path, &schema::create_sql(), encrypted));
        fs::write(
            sidecar(&path, "-journal"),
            b"not a journal, but hot to SQLite",
        )
        .unwrap();
        let before = files(&tmp.0);
        let refused = open(&path, encrypted).unwrap_err();
        assert_eq!(
            (refused.kind, refused.cause.clone()),
            (VaultError::Invalid, OpenCause::Recovery)
        );
        assert!(refused.needs_handle_fence() && !refused.proves_unwritable());
        assert_eq!(files(&tmp.0), before, "{encrypted}");
    }
    // A file that is no SQLite database, beside a journal: refused before SQLite looks at it.
    let tmp = TempDir::new();
    let path = tmp.0.join("junk.sqlite3");
    let crashed = crash(&tmp.0.join("c"), &schema::create_sql(), false);
    fs::write(&path, vec![0x5a; 8192]).unwrap();
    fs::copy(sidecar(&crashed, "-journal"), sidecar(&path, "-journal")).unwrap();
    let before = fs::read(sidecar(&path, "-journal")).unwrap();
    assert_eq!(
        open_vault_detailed(&path).unwrap_err().cause,
        OpenCause::Recovery
    );
    assert_eq!(fs::read(sidecar(&path, "-journal")).unwrap(), before);
    assert_eq!(fs::read(&path).unwrap(), vec![0x5a; 8192]);
}

/// `dir/v.sqlite3`, a closed schema-10 vault (plain or keyed) with `bytes` written as its `suffix`
/// sidecar, alone in `dir`.
fn beside(dir: &Path, encrypted: bool, suffix: &str, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("v.sqlite3");
    drop(made(&path, &schema::create_sql(), encrypted));
    fs::write(sidecar(&path, suffix), bytes).unwrap();
    path
}

/// F-S3-01: a non-empty `-wal` without a WAL header (too short, another magic, an impossible page
/// size) is refused, and the vault, the WAL and the absent `-shm` stay as they were.
#[test]
fn a_non_empty_wal_without_a_wal_header_is_refused_unchanged() {
    let mut valid = [0u8; 32];
    valid[..4].copy_from_slice(&WAL_MAGIC[0].to_be_bytes());
    valid[8..12].copy_from_slice(&4096u32.to_be_bytes());
    let mut magic = valid;
    magic[3] ^= 0x10;
    let mut size = valid;
    size[8..12].copy_from_slice(&1000u32.to_be_bytes());
    let mut small = valid;
    small[8..12].copy_from_slice(&256u32.to_be_bytes());
    let cases: [(&str, &[u8]); 6] = [
        ("one byte", &[0x37]),
        ("31 bytes of a header", &valid[..31]),
        ("zero bytes, 31 of them", &[0; 31]),
        ("another magic", &magic),
        ("page size no power of two", &size),
        ("page size below 512", &small),
    ];
    for encrypted in [false, true] {
        for (name, bytes) in cases {
            let tmp = TempDir::new();
            let dir = tmp.0.join("vault");
            let path = beside(&dir, encrypted, "-wal", bytes);
            let before = files(&dir);
            let refused = open(&path, encrypted).unwrap_err();
            assert_eq!(
                (refused.kind, refused.cause.clone()),
                (VaultError::Invalid, OpenCause::Recovery),
                "{name} ({encrypted})"
            );
            assert!(refused.needs_handle_fence() && !refused.proves_unwritable());
            assert_eq!(files(&dir), before, "{name} ({encrypted})");
        }
    }
}

/// F-S3-01: a journal whose first byte is 0 but whose 28-byte header is not all zero was not left by
/// SQLite; it is refused instead of being taken for "not hot", and nothing changes.
#[test]
fn a_journal_zero_only_in_its_first_byte_is_refused_unchanged() {
    for encrypted in [false, true] {
        for sql in [schema::create_sql(), schema_9_sql()] {
            let tmp = TempDir::new();
            let dir = tmp.0.join("vault");
            let path = crash(&dir, &sql, encrypted);
            let journal = sidecar(&path, "-journal");
            let mut bytes = fs::read(&journal).unwrap();
            bytes[0] = 0;
            fs::write(&journal, &bytes).unwrap();
            let before = files(&dir);
            let refused = open(&path, encrypted).unwrap_err();
            // The sidecar is read before any header, so schema 9 is refused for it too.
            assert_eq!(
                (refused.kind, refused.cause.clone()),
                (VaultError::Invalid, OpenCause::Recovery),
                "{encrypted}"
            );
            assert_eq!(files(&dir), before, "{encrypted}");
        }
        // Only the magic's first byte cleared, the rest of the header zero; and a short zero
        // prefix (fewer than 28 bytes): neither is a zeroed SQLite header.
        let mut magic_tail = [0u8; 28];
        magic_tail[1..8].copy_from_slice(&JOURNAL_MAGIC[1..]);
        // Zero magic and `nRec` (the unsynced shape), but sizes SQLite never writes.
        let mut bad_sizes = [0u8; 28];
        bad_sizes[12..16].copy_from_slice(&[9, 9, 9, 9]);
        bad_sizes[20..24].copy_from_slice(&512u32.to_be_bytes());
        bad_sizes[24..28].copy_from_slice(&1000u32.to_be_bytes());
        // Zero magic but a record count: SQLite writes `nRec` only together with the magic.
        let mut counted = [0u8; 28];
        counted[8..12].copy_from_slice(&1u32.to_be_bytes());
        counted[20..24].copy_from_slice(&512u32.to_be_bytes());
        counted[24..28].copy_from_slice(&4096u32.to_be_bytes());
        for bytes in [
            &magic_tail[..],
            &bad_sizes[..],
            &counted[..],
            &[0u8; 27][..],
            &[0, 0, 0, 1][..],
        ] {
            let tmp = TempDir::new();
            let dir = tmp.0.join("vault");
            let path = beside(&dir, encrypted, "-journal", bytes);
            let before = files(&dir);
            assert_eq!(
                open(&path, encrypted).unwrap_err().cause,
                OpenCause::Recovery,
                "{} bytes ({encrypted})",
                bytes.len()
            );
            assert_eq!(files(&dir), before);
        }
    }
}

/// The contracts F-S3-01 keeps: an empty WAL, an empty journal and a journal SQLite finalized by
/// zeroing its header (`journal_mode=PERSIST`) are nothing to recover, and the vault opens.
#[test]
fn an_empty_wal_and_a_journal_that_is_not_hot_still_open() {
    for encrypted in [false, true] {
        for (suffix, bytes) in [
            ("-wal", &[][..]),
            ("-journal", &[][..]),
            ("-journal", &[0; 28][..]),
        ] {
            let tmp = TempDir::new();
            let path = beside(&tmp.0.join("vault"), encrypted, suffix, bytes);
            assert!(restorable(&path).unwrap().pages.is_empty());
            assert!(
                open(&path, encrypted).is_ok(),
                "{suffix} {} ({encrypted})",
                bytes.len()
            );
        }

        // A journal exactly as SQLite leaves it in PERSIST mode after a commit: zeroed header,
        // stale records behind it.
        let tmp = TempDir::new();
        let dir = tmp.0.join("vault");
        fs::create_dir_all(&dir).unwrap();
        let source = tmp.0.join("source.sqlite3");
        let db = made(&source, &schema::create_sql(), encrypted);
        db.execute_batch(
            "PRAGMA journal_mode = PERSIST;
             INSERT INTO visits VALUES ('persisted', 'https://p.test/', 't', '2026-11-23T00:00:00.000Z');",
        )
        .unwrap();
        let journal = fs::read(sidecar(&source, "-journal")).unwrap();
        assert!(journal.len() > 28, "stale records remain");
        assert_eq!(journal[..28], [0; 28], "SQLite zeroed the header");
        assert!(journal[28..].iter().any(|&byte| byte != 0));
        drop(db);
        let path = dir.join("v.sqlite3");
        fs::copy(&source, &path).unwrap();
        fs::write(sidecar(&path, "-journal"), &journal).unwrap();
        assert!(restorable(&path).unwrap().pages.is_empty());
        let db = open(&path, encrypted).unwrap();
        let n: i64 = db
            .query_row(
                "SELECT count(*) FROM visits WHERE event_id = 'persisted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{encrypted}");
        drop(db);

        // A journal as SQLite has it while another connection is in its write transaction (or
        // after a crash before that transaction synced): magic and `nRec` still zero, the rest
        // of the header written. Nothing of it was ever written to the file.
        let tmp = TempDir::new();
        let dir = tmp.0.join("vault");
        fs::create_dir_all(&dir).unwrap();
        let source = tmp.0.join("source.sqlite3");
        let db = made(&source, &schema::create_sql(), encrypted);
        db.execute_batch(
            "BEGIN EXCLUSIVE;
             INSERT INTO visits VALUES ('pending', 'https://p.test/', 't', '2026-11-23T00:00:00.000Z');",
        )
        .unwrap();
        let path = dir.join("v.sqlite3");
        fs::copy(&source, &path).unwrap();
        let journal = fs::read(sidecar(&source, "-journal")).unwrap();
        db.execute_batch("ROLLBACK").unwrap();
        drop(db);
        assert_eq!(
            journal[..12],
            [0; 12],
            "SQLite has not written the magic yet"
        );
        assert!(journal[12..28].iter().any(|&byte| byte != 0));
        fs::write(sidecar(&path, "-journal"), &journal).unwrap();
        assert!(restorable(&path).unwrap().pages.is_empty());
        let db = open(&path, encrypted).unwrap();
        let n: i64 = db
            .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 0, "{encrypted}");
    }
}

#[test]
fn an_encrypted_vault_with_a_journal_and_a_wrong_key_is_refused_unchanged() {
    let tmp = TempDir::new();
    let dir = tmp.0.join("vault");
    let path = crash(&dir, &schema::create_sql(), true);
    let before = files(&dir);
    let refused =
        open_vault_in(&path, VaultMode::Encrypted, Some(VAULT_ID), &WrongKey).unwrap_err();
    assert_eq!(refused.kind, VaultError::DecryptFailed);
    assert_eq!(files(&dir), before);
}

/// `dir/wal.sqlite3` + `-wal` of a vault of `sql` in WAL mode, with `after` committed in the WAL
/// only (never checkpointed).
fn wal_copy(dir: &Path, sql: &str, after: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let source = dir.with_extension("source.sqlite3");
    let db = made(&source, sql, false);
    db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    db.execute_batch(after).unwrap();
    let target = dir.join("wal.sqlite3");
    fs::copy(&source, &target).unwrap();
    fs::copy(sidecar(&source, "-wal"), sidecar(&target, "-wal")).unwrap();
    drop(db);
    target
}

#[test]
fn wal_frames_count_like_the_file_itself() {
    let tmp = TempDir::new();
    let insert = "INSERT INTO visits VALUES ('in-wal', 'https://wal.test/', 't', '2026-11-23T00:00:00.000Z');";
    // Schema 9 in WAL mode: refused, and SQLite never created its `-shm` or checkpointed.
    let v9 = wal_copy(&tmp.0.join("v9"), &schema_9_sql(), insert);
    let before = files(&tmp.0.join("v9"));
    assert_eq!(
        open_vault_detailed(&v9).unwrap_err().cause,
        OpenCause::UnsupportedVersion(9)
    );
    assert_eq!(files(&tmp.0.join("v9")), before);
    // A schema-10 file whose WAL commits another version.
    let relabelled = wal_copy(
        &tmp.0.join("relabelled"),
        &schema::create_sql(),
        "PRAGMA user_version = 9;",
    );
    assert!(restorable_pages(&relabelled) > 0);
    let before = files(&tmp.0.join("relabelled"));
    assert_eq!(
        open_vault_detailed(&relabelled).unwrap_err().cause,
        OpenCause::UnsupportedVersion(9)
    );
    assert_eq!(files(&tmp.0.join("relabelled")), before);
    // Schema 10 in WAL mode opens, with what only the WAL holds.
    let v10 = wal_copy(&tmp.0.join("v10"), &schema::create_sql(), insert);
    let db = open_vault_detailed(&v10).unwrap();
    let n: i64 = db
        .query_row(
            "SELECT count(*) FROM visits WHERE event_id = 'in-wal'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_refused_vault_replaced_afterwards_was_never_written_and_its_successor_opens() {
    let tmp = TempDir::new();
    let dir = tmp.0.join("vault");
    let path = crash(&dir, &schema_9_sql(), false);
    let old = files(&dir);
    assert!(open_vault_detailed(&path).is_err());
    // The person moves the old vault (and its journal) away and puts a new one at the path.
    let aside = tmp.0.join("aside");
    fs::create_dir(&aside).unwrap();
    for name in old.keys() {
        fs::rename(dir.join(name), aside.join(name)).unwrap();
    }
    drop(made(&path, &schema::create_sql(), false));
    assert!(open_vault_detailed(&path).is_ok());
    assert_eq!(files(&aside), old, "the refused files were never written");
}

/// Between the check and the first read, the checked file cannot be swapped for another one:
/// renaming over it, deleting or moving it fails while it is pinned, and the connection reads the
/// file that was checked.
#[cfg(windows)]
#[test]
fn the_checked_file_cannot_be_swapped_before_the_first_read() {
    use std::{cell::RefCell, rc::Rc};
    for encrypted in [false, true] {
        let tmp = TempDir::new();
        let path = tmp.0.join("v.sqlite3");
        let db = made(&path, &schema::create_sql(), encrypted);
        db.execute(
            "INSERT INTO visits VALUES ('checked', 'https://a.test/', 't', '2026-11-23T00:00:00.000Z')",
            [],
        )
        .unwrap();
        drop(db);
        // A schema-9 vault with a hot journal, ready to be put in its place.
        let other = crash(&tmp.0.join("other"), &schema_9_sql(), encrypted);
        let other_files = files(&tmp.0.join("other"));
        let attempts = Rc::new(RefCell::new(Vec::new()));
        let (seen, from, at, aside) = (
            attempts.clone(),
            other.clone(),
            path.clone(),
            tmp.0.join("moved.sqlite3"),
        );
        BEFORE_FIRST_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let mut seen = seen.borrow_mut();
                seen.push(fs::rename(&from, &at).is_ok());
                seen.push(fs::remove_file(&at).is_ok());
                seen.push(fs::rename(&at, &aside).is_ok());
            }))
        });
        let opened = open(&path, encrypted);
        BEFORE_FIRST_READ.with(|hook| *hook.borrow_mut() = None);
        let db = opened.unwrap();
        assert_eq!(*attempts.borrow(), [false, false, false], "{encrypted}");
        let id: String = db
            .query_row("SELECT event_id FROM visits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(id, "checked");
        drop(db);
        assert_eq!(files(&tmp.0.join("other")), other_files);
    }
}
