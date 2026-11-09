//! Issue #51 Slice A' — SQLCipher PoCs (A-1, A-4.3, A-8). Synthetic data in fresh temp directories only.
//! Run: `cargo test --release -- --nocapture --test-threads=1`
//! Optional: MC_PLAIN_FIXTURE=<path of a vault written by a plain-SQLite build> for the regression test.

use memocampus_native_host::schema;
use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mc-sv-cipher-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const MARKER: &str = "https://secret-marker.example/very-private-page";
const KEY_A: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00112233445566778899aabbccddeeff0";
const KEY_B: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

fn key(db: &Connection, hex: &str) {
    db.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";")).unwrap();
}

fn open(path: &Path, hex: Option<&str>) -> Connection {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE).unwrap();
    if let Some(hex) = hex {
        key(&db, hex);
    }
    db
}

/// A schema 7 vault (production `create_sql`), keyed when `hex` is given.
fn vault(path: &Path, hex: Option<&str>, visits: usize) -> Connection {
    let db = open(path, hex);
    db.execute_batch(&format!("BEGIN; {} COMMIT;", schema::create_sql())).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for i in 0..visits {
        tx.execute(
            "INSERT INTO visits VALUES (?1, ?2, 't', '2026-11-09T00:00:00.000Z')",
            [format!("v{i}"), format!("{MARKER}/{i}")],
        )
        .unwrap();
    }
    tx.execute(
        "INSERT INTO image_blobs (id, sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data)
         VALUES (1, ?1, 'image/png', 4, 1, 1, 0, 0, x'89504e47')",
        ["a".repeat(64)],
    )
    .unwrap();
    tx.commit().unwrap();
    db
}

fn content(db: &Connection) -> Vec<(String, String)> {
    use sha2::Digest;
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|t| {
            let mut stmt = db.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
            let n = stmt.column_count();
            let mut rows: Vec<String> = stmt
                .query_map([], |r| Ok((0..n).map(|i| format!("{:?}", r.get_ref(i).unwrap())).collect::<Vec<_>>().join("|")))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            rows.sort();
            (t, format!("{:x}", sha2::Sha256::digest(rows.join("\n"))))
        })
        .collect()
}

fn ids(db: &Connection) -> (i64, i64) {
    (
        db.query_row("PRAGMA application_id", [], |r| r.get(0)).unwrap(),
        db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap(),
    )
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn is_plain(path: &Path) -> bool {
    fs::read(path).map(|b| b.starts_with(b"SQLite format 3\0")).unwrap_or(false)
}

fn code(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(f, m) => format!("{:?}/{} {}", f.code, f.extended_code, m.clone().unwrap_or_default()),
        other => format!("{other}"),
    }
}

/// Opens `path` with each candidate key and reports which one reads it, then verifies it fully.
fn verify(path: &Path, expect: &[(&str, String)], expect_ids: (i64, i64)) -> String {
    for candidate in [None, Some(KEY_A), Some(KEY_B)] {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        if let Some(k) = candidate {
            key(&db, k);
        }
        if db.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).is_err() {
            continue;
        }
        let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        let schema = schema::check(&db).map(|_| "ok".to_string()).unwrap_or_else(|e| format!("{e:?}"));
        let same = content(&db).iter().map(|(t, h)| (t.as_str(), h.clone())).collect::<Vec<_>>() == expect;
        let got_ids = ids(&db);
        let marker = contains(&fs::read(path).unwrap(), MARKER.as_bytes());
        return format!(
            "reads with {:<6} plainHeader={} integrity={} schema::check={} ids={} content={} markerInFile={}",
            match candidate { None => "nokey", Some(KEY_A) => "KEY_A", _ => "KEY_B" },
            is_plain(path),
            integrity,
            schema,
            if got_ids == expect_ids { "kept".to_string() } else { format!("{got_ids:?}") },
            if same { "equal" } else { "DIFFERENT" },
            marker
        );
    }
    "unreadable with every candidate key".into()
}

fn expect_of(db: &Connection) -> Vec<(&'static str, String)> {
    content(db).into_iter().map(|(t, h)| (Box::leak(t.into_boxed_str()) as &str, h)).collect()
}

// ---------------------------------------------------------------------------------------------

#[test]
fn c1_versions_and_compile_options() {
    let db = Connection::open_in_memory().unwrap();
    let q = |sql: &str| db.query_row(sql, [], |r| r.get::<_, String>(0)).unwrap_or_else(|e| code(&e));
    let options: Vec<String> = db
        .prepare("PRAGMA compile_options")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let wanted: Vec<&String> = options.iter().filter(|o| o.contains("TEMP_STORE") || o.contains("HAS_CODEC") || o.contains("CRYPTO") || o.contains("THREADSAFE")).collect();
    println!(
        "C-1 sqlite_version={} cipher_version={} cipher_provider={} cipher_provider_version={} options={wanted:?}",
        q("SELECT sqlite_version()"),
        q("PRAGMA cipher_version"),
        q("PRAGMA cipher_provider"),
        q("PRAGMA cipher_provider_version")
    );
    let tmp = Tmp::new("defaults");
    let db = open(&tmp.0.join("x.db"), Some(KEY_A));
    let settings: Vec<String> = ["cipher_page_size", "kdf_iter", "cipher_hmac_algorithm", "cipher_kdf_algorithm", "cipher_plaintext_header_size", "temp_store", "journal_mode"]
        .iter()
        .map(|p| format!("{p}={}", db.query_row(&format!("PRAGMA {p}"), [], |r| r.get::<_, rusqlite::types::Value>(0)).map(|v| format!("{v:?}")).unwrap_or_else(|e| code(&e))))
        .collect();
    println!("C-1 keyed defaults: {}", settings.join(" "));
}

#[test]
fn c2_plain_vault_written_by_a_plain_build_opens_unkeyed_unchanged() {
    let Ok(fixture) = std::env::var("MC_PLAIN_FIXTURE") else {
        println!("C-2 skipped: MC_PLAIN_FIXTURE not set");
        return;
    };
    let tmp = Tmp::new("plainfix");
    let copy = tmp.0.join("vault.sqlite3");
    fs::copy(&fixture, &copy).unwrap();
    let before = fs::read(&copy).unwrap();
    let db = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    let quick: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0)).unwrap();
    schema::check(&db).unwrap();
    let visits: i64 = db.query_row("SELECT count(*) FROM visits", [], |r| r.get(0)).unwrap();
    // A write through the SQLCipher build keeps it a plain SQLite file.
    db.execute("INSERT INTO visits VALUES ('new', 'https://n/', 't', 'x')", []).unwrap();
    drop(db);
    println!(
        "C-2 plain-build vault via SQLCipher build, no key: quick_check={quick} schema::check=ok ids={:?} visits={visits}; untouched before write={}; still plain after write={}",
        ids(&Connection::open(&copy).unwrap()),
        before.len() > 0,
        is_plain(&copy)
    );
    assert!(is_plain(&copy));
}

#[test]
fn c3_encrypted_vault_schema_header_and_journal() {
    let tmp = Tmp::new("enc");
    let path = tmp.0.join("vault.sqlite3");
    let db = vault(&path, Some(KEY_A), 3000);
    schema::check(&db).unwrap();
    assert_eq!(ids(&db), (schema::APP_ID, schema::SCHEMA_VERSION));
    let bytes = fs::read(&path).unwrap();
    assert!(!is_plain(&path));
    assert!(!contains(&bytes, MARKER.as_bytes()));
    let salt = bytes[..16].to_vec();

    // Rollback journal while a transaction is spilling: original pages (with the marker) go there.
    db.execute_batch("PRAGMA cache_size = 2;").unwrap();
    db.execute_batch("BEGIN; UPDATE visits SET title = 'changed';").unwrap();
    let journal = PathBuf::from(format!("{}-journal", path.display()));
    let jbytes = fs::read(&journal).unwrap_or_default();
    db.execute_batch("ROLLBACK;").unwrap();
    println!(
        "C-3 encrypted vault {} bytes: plainHeader=false, marker in file=false, schema::check ok, ids kept; journal during spill {} bytes, marker in journal={}",
        bytes.len(),
        jbytes.len(),
        contains(&jbytes, MARKER.as_bytes())
    );
    assert!(!jbytes.is_empty(), "the journal was observed");
    assert!(!contains(&jbytes, MARKER.as_bytes()), "journal pages are encrypted");
    drop(db);
    assert_eq!(fs::read(&path).unwrap()[..16], salt[..], "salt is stable across writes");
}

#[test]
fn c4_no_key_and_wrong_key_errors_and_fence_attempts() {
    let tmp = Tmp::new("wrongkey");
    let path = tmp.0.join("vault.sqlite3");
    drop(vault(&path, Some(KEY_A), 10));
    let before = fs::read(&path).unwrap();
    let mut lines = Vec::new();
    for (label, k) in [("no key", None), ("wrong key", Some(KEY_B)), ("right key", Some(KEY_A))] {
        let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        if let Some(k) = k {
            key(&db, k);
        }
        let read = db.query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0)).map_err(|e| code(&e));
        let fence = db.execute_batch("BEGIN IMMEDIATE").map_err(|e| code(&e));
        let _ = db.execute_batch("ROLLBACK");
        let schema = schema::check(&db).map_err(|e| format!("{e:?}"));
        lines.push(format!("{label}: read={read:?} BEGIN IMMEDIATE={fence:?} schema::check={}", schema.map(|_| "ok".into()).unwrap_or_else(|e| e)));
    }
    for l in &lines {
        println!("C-4 {l}");
    }
    assert_eq!(fs::read(&path).unwrap(), before, "failed opens changed nothing");
}

#[test]
fn c5_backup_matrix() {
    let tmp = Tmp::new("matrix");
    let plain = tmp.0.join("plain.sqlite3");
    let enc = tmp.0.join("enc.sqlite3");
    let p = vault(&plain, None, 2000);
    let e = vault(&enc, Some(KEY_A), 2000);
    let expect_p = expect_of(&p);
    let expect_e = expect_of(&e);
    assert_eq!(expect_p, expect_e, "same synthetic data");
    let ids_ok = (schema::APP_ID, schema::SCHEMA_VERSION);
    let mut report = Vec::new();

    let backup_api = |src: &Connection, dest: &Path, dest_key: Option<&str>| -> Result<(), String> {
        let mut d = open(dest, dest_key);
        let b = Backup::new(src, &mut d).map_err(|e| code(&e))?;
        b.run_to_completion(1000, Duration::ZERO, None).map_err(|e| code(&e))
    };
    let export = |src: &Connection, dest: &Path, dest_key: &str| -> Result<(), String> {
        let k = if dest_key.is_empty() { "''".to_string() } else { format!("\"x'{dest_key}'\"") };
        src.execute_batch(&format!("ATTACH DATABASE '{}' AS t KEY {k}; SELECT sqlcipher_export('t'); DETACH DATABASE t;", dest.display()))
            .map_err(|e| code(&e))
    };
    // Finding: sqlcipher_export copies schema and rows but not the header fields `application_id` and
    // `user_version`. The conversion sets them from the source explicitly, then verifies.
    let export_with_ids = |src: &Connection, dest: &Path, dest_key: &str| -> Result<(), String> {
        let k = if dest_key.is_empty() { "''".to_string() } else { format!("\"x'{dest_key}'\"") };
        let (app, ver) = ids(src);
        src.execute_batch(&format!(
            "ATTACH DATABASE '{}' AS t KEY {k}; SELECT sqlcipher_export('t'); PRAGMA t.application_id = {app}; PRAGMA t.user_version = {ver}; DETACH DATABASE t;",
            dest.display()
        ))
        .map_err(|e| code(&e))
    };
    let vacuum_into = |src: &Connection, dest: &Path| -> Result<(), String> {
        src.execute("VACUUM INTO ?1", [dest.to_str().unwrap()]).map(|_| ()).map_err(|e| code(&e))
    };

    let cases: Vec<(&str, Box<dyn Fn(&Path) -> Result<(), String>>, &Vec<(&str, String)>)> = vec![
        ("Backup API   plain  -> plain        ", Box::new(|d| backup_api(&p, d, None)), &expect_p),
        ("Backup API   cipher -> cipher (KEY_A)", Box::new(|d| backup_api(&e, d, Some(KEY_A))), &expect_e),
        ("Backup API   cipher -> cipher (KEY_B)", Box::new(|d| backup_api(&e, d, Some(KEY_B))), &expect_e),
        ("Backup API   plain  -> cipher (KEY_A)", Box::new(|d| backup_api(&p, d, Some(KEY_A))), &expect_p),
        ("Backup API   cipher -> plain        ", Box::new(|d| backup_api(&e, d, None)), &expect_e),
        ("export       plain  -> cipher (KEY_A)", Box::new(|d| export(&p, d, KEY_A)), &expect_p),
        ("export       cipher -> plain        ", Box::new(|d| export(&e, d, "")), &expect_e),
        ("export       cipher -> cipher (KEY_B)", Box::new(|d| export(&e, d, KEY_B)), &expect_e),
        ("export+ids   plain  -> cipher (KEY_A)", Box::new(|d| export_with_ids(&p, d, KEY_A)), &expect_p),
        ("export+ids   cipher -> plain        ", Box::new(|d| export_with_ids(&e, d, "")), &expect_e),
        ("export+ids   cipher -> cipher (KEY_B)", Box::new(|d| export_with_ids(&e, d, KEY_B)), &expect_e),
        ("VACUUM INTO  plain                  ", Box::new(|d| vacuum_into(&p, d)), &expect_p),
        ("VACUUM INTO  cipher(KEY_A)          ", Box::new(|d| vacuum_into(&e, d)), &expect_e),
    ];
    for (i, (label, run, expect)) in cases.iter().enumerate() {
        let dest = tmp.0.join(format!("dest-{i}.sqlite3"));
        let outcome = match run(&dest) {
            Ok(()) => verify(&dest, expect, ids_ok),
            Err(err) => format!("FAILED: {err}"),
        };
        report.push(format!("{label} | {outcome}"));
    }
    for line in &report {
        println!("C-5 {line}");
    }
    // The paths the design relies on must be faithful.
    for needle in ["export       plain  -> cipher (KEY_A)", "export       cipher -> plain        ", "export       cipher -> cipher (KEY_B)"] {
        let line = report.iter().find(|l| l.starts_with(needle)).unwrap();
        assert!(line.contains("ids=(0, 0)") && line.contains("content=equal"), "plain export drops the ids: {line}");
    }
    for needle in ["export+ids   plain  -> cipher (KEY_A)", "export+ids   cipher -> plain        ", "export+ids   cipher -> cipher (KEY_B)"] {
        let line = report.iter().find(|l| l.starts_with(needle)).unwrap();
        assert!(line.contains("integrity=ok") && line.contains("schema::check=ok") && line.contains("content=equal"), "{line}");
    }
}

#[test]
fn c6_raw_ciphertext_copy_under_shared_is_a_keyed_snapshot() {
    let tmp = Tmp::new("raw");
    let path = tmp.0.join("vault.sqlite3");
    let db = vault(&path, Some(KEY_A), 3000);
    let writer = open(&path, Some(KEY_A));
    writer.busy_timeout(Duration::from_millis(100)).unwrap();
    db.execute_batch("BEGIN").unwrap();
    let expect = expect_of(&db);
    let attempt = writer.execute_batch("BEGIN IMMEDIATE; INSERT INTO visits VALUES ('w', 'https://w/', 't', 'x'); COMMIT;");
    let bytes = fs::read(&path).unwrap();
    db.execute_batch("COMMIT").unwrap();
    let _ = writer.execute_batch("ROLLBACK");
    let copy = tmp.0.join("copy.sqlite3");
    fs::write(&copy, &bytes).unwrap();
    let outcome = verify(&copy, &expect, (schema::APP_ID, schema::SCHEMA_VERSION));
    println!(
        "C-6 raw copy under SHARED ({} bytes; writer commit -> {:?}; salt equal={}): {outcome}",
        bytes.len(),
        attempt.as_ref().map_err(code),
        fs::read(&path).unwrap()[..16] == bytes[..16]
    );
    assert!(attempt.is_err());
    assert!(outcome.contains("KEY_A") && outcome.contains("integrity=ok") && outcome.contains("content=equal") && outcome.contains("markerInFile=false"), "{outcome}");
}

/// Child half of c7: exports and aborts the process part-way (progress handler).
#[test]
#[ignore]
fn c7_child_export_then_abort() {
    let dir = PathBuf::from(std::env::var("MC_CRASH_DIR").unwrap());
    let src = open(&dir.join("src.sqlite3"), Some(KEY_A));
    let mut n = 0u32;
    src.progress_handler(1000, Some(move || {
        n += 1;
        if n == 40 {
            std::process::abort();
        }
        false
    }));
    let target = dir.join("target.partial");
    src.execute_batch(&format!("ATTACH DATABASE '{}' AS t KEY ''; SELECT sqlcipher_export('t');", target.display())).unwrap();
    panic!("export finished before the abort point; raise the visit count");
}

#[test]
fn c7_crash_during_export_leaves_the_source_intact() {
    let tmp = Tmp::new("crash");
    let src_path = tmp.0.join("src.sqlite3");
    let src = vault(&src_path, Some(KEY_A), 20000);
    let expect = expect_of(&src);
    drop(src);
    let before = fs::read(&src_path).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "c7_child_export_then_abort", "--test-threads=1"])
        .env("MC_CRASH_DIR", &tmp.0)
        .output()
        .unwrap();
    let files: BTreeSet<String> = fs::read_dir(&tmp.0).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    let target = tmp.0.join("target.partial");
    let target_state = if target.exists() {
        let size = fs::metadata(&target).unwrap().len();
        let plain_marker = contains(&fs::read(&target).unwrap(), MARKER.as_bytes());
        let db = Connection::open_with_flags(&target, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        let after = db.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map_err(|e| code(&e));
        format!("{size} bytes, plaintext marker in partial={plain_marker}, objects after reopen (hot journal rolled back)={after:?}")
    } else {
        "absent".into()
    };
    let unchanged = fs::read(&src_path).unwrap() == before;
    let outcome = verify(&src_path, &expect, (schema::APP_ID, schema::SCHEMA_VERSION));
    println!(
        "C-7 child exit={:?} (aborted={}); files={files:?}; target {target_state}; source bytes unchanged={unchanged}; source: {outcome}",
        status.status.code(),
        !status.status.success()
    );
    assert!(!status.status.success(), "the child crashed mid-export: {}", String::from_utf8_lossy(&status.stdout));
    assert!(unchanged && outcome.contains("integrity=ok") && outcome.contains("content=equal"));
}
