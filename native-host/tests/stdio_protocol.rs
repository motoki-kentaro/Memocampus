//! Drives the real host binary over stdio with Native Messaging framing.
//! Every test uses its own temp directory as `MEMOCAMPUS_APP_DIR`; no user data is touched.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-it-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn app_dir(&self) -> PathBuf {
        self.0.join("app")
    }

    fn default_vault(&self) -> PathBuf {
        self.app_dir().join("memocampus.sqlite3")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One host process: sends all messages, closes stdin, collects every framed response.
fn exchange_with(
    tmp: &TempDir,
    configure: impl FnOnce(&mut Command),
    messages: &[Value],
) -> Vec<Value> {
    let bodies: Vec<Vec<u8>> = messages
        .iter()
        .map(|message| serde_json::to_vec(message).unwrap())
        .collect();
    exchange_raw(tmp, configure, &bodies)
}

/// Like `exchange_with`, but sends pre-encoded bodies (to test malformed JSON).
fn exchange_raw(
    tmp: &TempDir,
    configure: impl FnOnce(&mut Command),
    bodies: &[Vec<u8>],
) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memocampus-native-host"));
    command
        .env("MEMOCAMPUS_APP_DIR", tmp.app_dir())
        .env_remove("MEMOCAMPUS_DB")
        .current_dir(&tmp.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(&mut command);
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for body in bodies {
        stdin.write_all(&(body.len() as u32).to_ne_bytes()).unwrap();
        stdin.write_all(body).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "host exited with {:?}",
        output.status
    );
    assert!(output.stderr.is_empty(), "host must not log");
    let mut stdout = output.stdout.as_slice();
    let mut responses = Vec::new();
    let mut length = [0u8; 4];
    while stdout.read_exact(&mut length).is_ok() {
        let mut body = vec![0u8; u32::from_ne_bytes(length) as usize];
        stdout.read_exact(&mut body).unwrap();
        responses.push(serde_json::from_slice(&body).unwrap());
    }
    responses
}

fn exchange(tmp: &TempDir, messages: &[Value]) -> Vec<Value> {
    exchange_with(tmp, |_| {}, messages)
}

fn create_default_vault(tmp: &TempDir) {
    let response = &exchange(
        tmp,
        &[json!({ "type": "vault_create", "requestId": "c", "useDefault": true })],
    )[0];
    assert_eq!(response["ok"], true, "{response}");
}

fn visit(id: &str, url: &str, at: &str) -> Value {
    json!({ "type": "visit", "id": id, "url": url, "title": "Rust Book", "incognito": false, "visitedAt": at })
}

fn snapshot(id: &str, url: &str, at: &str) -> Value {
    json!({ "type": "tab_snapshot", "id": id, "windowId": 1, "tabIndex": 0, "url": url, "title": "Rust Book",
            "pinned": false, "incognito": false, "recordedAt": at })
}

fn search(id: &str) -> Value {
    json!({ "type": "search_pages", "requestId": id })
}

fn row_dump(path: &Path) -> Vec<String> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = db
        .prepare("SELECT 'v|' || event_id || '|' || url || '|' || title || '|' || visited_at FROM visits UNION ALL SELECT 's|' || event_id || '|' || url || '|' || title || '|' || recorded_at FROM tab_snapshots ORDER BY 1")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.collect::<Result<_, _>>().unwrap()
}

#[test]
fn first_run_requires_an_explicit_vault_and_creates_nothing_implicitly() {
    let tmp = TempDir::new();
    let responses = exchange(
        &tmp,
        &[
            json!({ "type": "vault_status", "requestId": "s" }),
            visit("v1", "https://a.test/", "2026-10-30T10:00:00.000Z"),
            snapshot("s1", "https://a.test/", "2026-10-30T10:00:00.000Z"),
            search("q"),
        ],
    );
    assert_eq!(responses[0]["status"]["state"], "not_configured");
    assert_eq!(
        responses[1],
        json!({ "ok": false, "eventId": "v1", "error": "vault_not_configured" })
    );
    assert_eq!(
        responses[2],
        json!({ "ok": false, "eventId": "s1", "error": "vault_not_configured" })
    );
    assert_eq!(
        responses[3],
        json!({ "ok": false, "type": "search_pages_result", "requestId": "q", "error": "vault_not_configured" })
    );
    assert!(
        !tmp.app_dir().exists(),
        "no directory, config or database may appear"
    );
}

#[test]
fn created_vault_receives_writes_and_survives_host_restarts() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let url = "https://doc.rust-lang.org/book/";
    // Separate processes, as Chrome launches one host per sendNativeMessage.
    assert_eq!(
        exchange(&tmp, &[visit("v1", url, "2026-10-29T10:00:00.000Z")])[0],
        json!({ "ok": true, "eventId": "v1" })
    );
    assert_eq!(
        exchange(&tmp, &[snapshot("s1", url, "2026-10-30T10:00:00.000Z")])[0],
        json!({ "ok": true, "eventId": "s1" })
    );
    let responses = exchange(
        &tmp,
        &[
            visit("v2", "chrome://settings", "2026-10-30T11:00:00.000Z"),
            json!({ "type": "unknown_future_message" }),
            json!({ "type": "search_pages", "requestId": "r1", "query": "RUST", "limit": 10, "offset": 0 }),
            json!({ "type": "search_pages", "requestId": "r2", "limit": 0 }),
            json!({ "type": "vault_status", "requestId": "st" }),
        ],
    );
    assert_eq!(responses.len(), 5);
    // Rejected events are answered explicitly so the extension's queue never waits on silence.
    assert_eq!(
        responses[0],
        json!({ "ok": false, "eventId": "v2", "error": "invalid_url" })
    );
    assert_eq!(
        responses[1],
        json!({ "ok": false, "eventId": null, "error": "unknown_type" })
    );
    let responses = &responses[2..];
    assert_eq!(
        responses[0],
        json!({
            "ok": true, "type": "search_pages_result", "requestId": "r1", "hasMore": false,
            "items": [{
                "url": url, "title": "Rust Book",
                "lastVisitedAt": "2026-10-29T10:00:00.000Z",
                "lastCapturedAt": "2026-10-30T10:00:00.000Z",
                "lastActivityAt": "2026-10-30T10:00:00.000Z",
                "source": "both",
            }],
        })
    );
    assert_eq!(responses[1]["error"], "invalid_limit");
    let status = &responses[2]["status"];
    assert_eq!(status["state"], "ready");
    assert_eq!(
        status["vaultPath"],
        tmp.default_vault().to_string_lossy().as_ref()
    );
    assert_eq!(status["counts"], json!({ "visits": 1, "snapshots": 1 }));
}

#[test]
fn creating_over_an_existing_file_is_refused_and_the_file_kept() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    exchange(
        &tmp,
        &[visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z")],
    );
    let before = std::fs::read(tmp.default_vault()).unwrap();
    let response = &exchange(
        &tmp,
        &[json!({ "type": "vault_create", "requestId": "c", "useDefault": true })],
    )[0];
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"], "file_exists");
    assert_eq!(std::fs::read(tmp.default_vault()).unwrap(), before);
}

#[test]
fn a_vanished_vault_is_an_error_and_is_never_recreated() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    std::fs::remove_file(tmp.default_vault()).unwrap();
    let responses = exchange(
        &tmp,
        &[
            visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z"),
            search("q"),
            json!({ "type": "vault_status", "requestId": "s" }),
        ],
    );
    assert_eq!(responses[0]["error"], "vault_missing");
    assert_eq!(responses[1]["error"], "vault_missing");
    assert_eq!(responses[2]["status"]["state"], "missing");
    assert!(!tmp.default_vault().exists());
}

#[test]
fn a_legacy_database_in_the_working_directory_is_ignored_and_untouched() {
    let tmp = TempDir::new();
    // The pre-#003 host opened `memocampus.sqlite3` relative to its working directory.
    let legacy = tmp.0.join("memocampus.sqlite3");
    let db = Connection::open(&legacy).unwrap();
    db.execute_batch("CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL); CREATE TABLE tab_snapshots (event_id TEXT PRIMARY KEY, window_id INTEGER NOT NULL, tab_index INTEGER NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, pinned INTEGER NOT NULL, incognito INTEGER NOT NULL, recorded_at TEXT NOT NULL); INSERT INTO visits VALUES ('old', 'https://old.test/', 'Old', '2026-10-01T00:00:00.000Z');").unwrap();
    drop(db);
    let before = std::fs::read(&legacy).unwrap();

    assert_eq!(
        exchange(
            &tmp,
            &[visit("v0", "https://a.test/", "2026-10-29T10:00:00.000Z")]
        )[0]["error"],
        "vault_not_configured"
    );
    create_default_vault(&tmp);
    assert_eq!(
        exchange(
            &tmp,
            &[visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z")]
        )[0]["ok"],
        true
    );
    let found = &exchange(&tmp, &[search("q")])[0];
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    assert_eq!(found["items"][0]["url"], "https://a.test/");

    assert_eq!(
        std::fs::read(&legacy).unwrap(),
        before,
        "legacy DB must stay byte-identical"
    );
    assert_eq!(
        row_dump(&tmp.default_vault()),
        ["v|v1|https://a.test/|Rust Book|2026-10-29T10:00:00.000Z"]
    );
}

#[test]
fn open_switches_vaults_and_rejects_invalid_files() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    exchange(
        &tmp,
        &[visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z")],
    );
    let second = tmp.0.join("second.sqlite3");
    let created = &exchange(
        &tmp,
        &[json!({ "type": "vault_create", "requestId": "c", "path": second })],
    )[0];
    assert_eq!(created["ok"], true, "{created}");
    exchange(
        &tmp,
        &[visit("v2", "https://b.test/", "2026-10-29T11:00:00.000Z")],
    );
    assert_eq!(row_dump(&second).len(), 1);

    let junk = tmp.0.join("junk.sqlite3");
    std::fs::write(&junk, b"definitely not sqlite").unwrap();
    let refused = &exchange(
        &tmp,
        &[json!({ "type": "vault_open", "requestId": "o", "path": junk })],
    )[0];
    assert_eq!(refused["error"], "vault_invalid");
    assert_eq!(std::fs::read(&junk).unwrap(), b"definitely not sqlite");

    let reopened = &exchange(
        &tmp,
        &[json!({ "type": "vault_open", "requestId": "o", "path": tmp.default_vault() })],
    )[0];
    assert_eq!(reopened["ok"], true, "{reopened}");
    let found = &exchange(&tmp, &[search("q")])[0];
    assert_eq!(found["items"][0]["url"], "https://a.test/");
}

#[test]
fn backup_through_the_protocol_produces_a_verified_copy() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    exchange(
        &tmp,
        &[
            visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z"),
            snapshot("s1", "https://a.test/", "2026-10-29T10:00:00.000Z"),
        ],
    );
    let dest = tmp.0.join("backup dir");
    std::fs::create_dir(&dest).unwrap();
    let response = &exchange(
        &tmp,
        &[json!({ "type": "vault_backup", "requestId": "b", "directory": dest })],
    )[0];
    assert_eq!(response["ok"], true, "{response}");
    let path = PathBuf::from(response["backupPath"].as_str().unwrap());
    assert_eq!(path.parent().unwrap(), dest);
    assert_eq!(row_dump(&path), row_dump(&tmp.default_vault()));
    let status = &exchange(&tmp, &[json!({ "type": "vault_status", "requestId": "s" })])[0];
    assert_eq!(
        status["status"]["vaultPath"],
        tmp.default_vault().to_string_lossy().as_ref(),
        "active vault unchanged"
    );
}

#[test]
fn development_override_writes_there_and_disables_management() {
    let tmp = TempDir::new();
    let dev = tmp.0.join("dev.sqlite3");
    // Prepared as a valid vault through the host itself, then used via MEMOCAMPUS_DB.
    let created = &exchange(
        &tmp,
        &[json!({ "type": "vault_create", "requestId": "c", "path": dev })],
    )[0];
    assert_eq!(created["ok"], true);
    let responses = exchange_with(
        &tmp,
        |command| {
            command.env("MEMOCAMPUS_DB", &dev);
        },
        &[
            visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z"),
            json!({ "type": "vault_create", "requestId": "c", "useDefault": true }),
            json!({ "type": "vault_status", "requestId": "s" }),
        ],
    );
    assert_eq!(responses[0], json!({ "ok": true, "eventId": "v1" }));
    assert_eq!(responses[1]["error"], "management_disabled");
    assert_eq!(responses[2]["status"]["overridden"], true);
    assert_eq!(row_dump(&dev).len(), 1);

    let missing = exchange_with(
        &tmp,
        |command| {
            command.env("MEMOCAMPUS_DB", tmp.0.join("absent.sqlite3"));
        },
        &[visit("v2", "https://a.test/", "2026-10-29T10:00:00.000Z")],
    );
    assert_eq!(missing[0]["error"], "vault_missing");
    assert!(
        !tmp.0.join("absent.sqlite3").exists(),
        "the override never creates a database either"
    );
}

#[test]
fn rejected_events_are_answered_and_do_not_block_later_events() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let responses = exchange(
        &tmp,
        &[
            visit("bad-url", "javascript:alert(1)", "2026-10-29T10:00:00.000Z"),
            json!({ "type": "visit", "id": "no-url", "title": "T", "incognito": false, "visitedAt": "2026-10-29T10:00:00.000Z" }),
            json!({ "type": "visit", "url": "https://a.test/", "title": "T", "incognito": false, "visitedAt": "2026-10-29T10:00:00.000Z" }),
            json!({ "type": "tab_snapshot", "id": "bad-snap", "url": "https://a.test/", "title": "T", "incognito": false }),
            json!({ "type": "visit", "id": "", "url": "https://a.test/", "title": "T", "incognito": false, "visitedAt": "2026-10-29T10:00:00.000Z" }),
            visit("good", "https://good.test/", "2026-10-29T11:00:00.000Z"),
        ],
    );
    assert_eq!(
        responses,
        [
            json!({ "ok": false, "eventId": "bad-url", "error": "invalid_url" }),
            json!({ "ok": false, "eventId": "no-url", "error": "invalid_event" }),
            json!({ "ok": false, "eventId": null, "error": "invalid_event" }),
            json!({ "ok": false, "eventId": "bad-snap", "error": "invalid_event" }),
            json!({ "ok": false, "eventId": "", "error": "invalid_event" }),
            json!({ "ok": true, "eventId": "good" }),
        ]
    );
    assert_eq!(
        row_dump(&tmp.default_vault()),
        ["v|good|https://good.test/|Rust Book|2026-10-29T11:00:00.000Z"]
    );
}

#[test]
fn undecodable_json_gets_a_structured_error_and_the_host_keeps_serving() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let good = serde_json::to_vec(&visit(
        "good",
        "https://good.test/",
        "2026-10-29T11:00:00.000Z",
    ))
    .unwrap();
    let responses = exchange_raw(
        &tmp,
        |_| {},
        &[b"{not json".to_vec(), vec![0xff, 0xfe], good],
    );
    assert_eq!(
        responses,
        [
            json!({ "ok": false, "eventId": null, "error": "invalid_message" }),
            json!({ "ok": false, "eventId": null, "error": "invalid_message" }),
            json!({ "ok": true, "eventId": "good" }),
        ]
    );
}

#[test]
fn resending_after_a_lost_ack_does_not_duplicate_rows() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let event = visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z");
    let shot = snapshot("s1", "https://a.test/", "2026-10-29T10:00:00.000Z");
    // Each resend is a new host process, exactly as when the first ACK never reached Chrome.
    for _ in 0..3 {
        let responses = exchange(&tmp, &[event.clone(), shot.clone()]);
        assert_eq!(responses[0], json!({ "ok": true, "eventId": "v1" }));
        assert_eq!(responses[1], json!({ "ok": true, "eventId": "s1" }));
    }
    assert_eq!(row_dump(&tmp.default_vault()).len(), 2);
}

fn pragma(path: &Path, name: &str) -> i64 {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap()
}

fn image_tables(path: &Path) -> i64 {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('image_blobs', 'saved_images', 'image_thumbnails')",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn vaults_created_by_the_host_start_at_schema_3_with_image_tables() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    assert_eq!(pragma(&tmp.default_vault(), "user_version"), 3);
    assert_eq!(image_tables(&tmp.default_vault()), 3);
    // Existing operations keep working on a schema 3 vault.
    assert_eq!(
        exchange(
            &tmp,
            &[visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z")]
        )[0],
        json!({ "ok": true, "eventId": "v1" })
    );
    assert_eq!(
        exchange(&tmp, &[search("q")])[0]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn ordinary_operations_never_migrate_a_schema_1_vault() {
    let tmp = TempDir::new();
    let old = tmp.0.join("before-images.sqlite3");
    let db = Connection::open(&old).unwrap();
    db.execute_batch("CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL); CREATE TABLE tab_snapshots (event_id TEXT PRIMARY KEY, window_id INTEGER NOT NULL, tab_index INTEGER NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, pinned INTEGER NOT NULL, incognito INTEGER NOT NULL, recorded_at TEXT NOT NULL); PRAGMA application_id = 1296256336; PRAGMA user_version = 1;").unwrap();
    drop(db);
    let opened = &exchange(
        &tmp,
        &[json!({ "type": "vault_open", "requestId": "o", "path": old })],
    )[0];
    assert_eq!(opened["ok"], true, "{opened}");
    let responses = exchange(
        &tmp,
        &[
            visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z"),
            snapshot("s1", "https://a.test/", "2026-10-29T10:00:00.000Z"),
            search("q"),
            json!({ "type": "vault_status", "requestId": "s" }),
        ],
    );
    assert_eq!(responses[0]["ok"], true);
    assert_eq!(responses[1]["ok"], true);
    assert_eq!(responses[2]["items"][0]["source"], "both");
    assert_eq!(responses[3]["status"]["state"], "ready");
    let dest = tmp.0.join("backups");
    std::fs::create_dir(&dest).unwrap();
    let backup = &exchange(
        &tmp,
        &[json!({ "type": "vault_backup", "requestId": "b", "directory": dest })],
    )[0];
    assert_eq!(backup["ok"], true, "{backup}");
    assert_eq!(
        pragma(&old, "user_version"),
        1,
        "no image operation ran, so no migration"
    );
    assert_eq!(image_tables(&old), 0);
}

#[test]
fn backups_of_a_vault_with_images_are_complete_and_verified() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    {
        let db = Connection::open(tmp.default_vault()).unwrap();
        db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        db.execute(
            "INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, save_count, data) VALUES (?1, 'image/png', 4, 1, 1, 1, 2, 2, x'89504e47')",
            ["0".repeat(64)],
        )
        .unwrap();
        db.execute_batch("INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito) VALUES ('a', 1, 'https://img.test/1.png', 'https://page.test/', 1, 0), ('b', 1, 'data:image/png', NULL, 2, 1); INSERT INTO image_thumbnails (blob_id, mime_type, width, height, generator, data) VALUES (1, 'image/webp', 1, 1, 1, x'00');").unwrap();
    }
    let dest = tmp.0.join("backups");
    std::fs::create_dir(&dest).unwrap();
    let response = &exchange(
        &tmp,
        &[json!({ "type": "vault_backup", "requestId": "b", "directory": dest })],
    )[0];
    assert_eq!(response["ok"], true, "{response}");
    let copy = PathBuf::from(response["backupPath"].as_str().unwrap());
    let dump = |path: &Path| -> Vec<String> {
        let db =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut statement = db
            .prepare("SELECT 'b|' || sha256 || '|' || hex(data) || '|' || save_count FROM image_blobs UNION ALL SELECT 's|' || save_id || '|' || image_url || '|' || saved_at || '|' || incognito FROM saved_images UNION ALL SELECT 't|' || blob_id || '|' || hex(data) FROM image_thumbnails ORDER BY 1")
            .unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(dump(&copy), dump(&tmp.default_vault()));
    assert_eq!(dump(&copy).len(), 4);
    assert_eq!(pragma(&copy, "user_version"), 3);
}

const SAVE_ID: &str = "4f1d2c3b-5a6e-4d7f-8091-a2b3c4d5e6f7";

/// A structurally valid PNG of exactly `size` bytes (the host checks headers, not pixels).
fn png_of_size(width: u32, height: u32, size: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    let payload = size - bytes.len() - 12 - 12;
    bytes.extend_from_slice(&(payload as u32).to_be_bytes());
    bytes.extend_from_slice(b"IDAT");
    bytes.extend((0..payload).map(|i| (i % 253) as u8));
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
    bytes
}

fn upload_messages(bytes: &[u8], commit: bool) -> Vec<Value> {
    use base64::Engine;
    use sha2::Digest;
    let sha256: String = sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut messages = vec![json!({
        "type": "image_save_begin", "protocol": 2, "saveId": SAVE_ID, "sha256": sha256,
        "byteLength": bytes.len(), "mimeType": "image/png", "width": 40, "height": 30,
        "imageUrl": "https://img.test/x.png", "pageUrl": "https://page.test/thread/1", "incognito": false,
    })];
    for (index, part) in bytes.chunks(1024 * 1024).enumerate() {
        messages.push(json!({ "type": "image_save_chunk", "saveId": SAVE_ID, "index": index, "data": base64::engine::general_purpose::STANDARD.encode(part) }));
    }
    if commit {
        messages.push(json!({ "type": "image_save_commit", "saveId": SAVE_ID }));
    }
    messages
}

fn image_rows(path: &Path) -> (i64, i64) {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.query_row(
        "SELECT (SELECT count(*) FROM image_blobs), (SELECT count(*) FROM saved_images)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

#[test]
fn an_image_upload_over_one_host_process_is_committed_atomically() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let bytes = png_of_size(40, 30, 2 * 1024 * 1024 + 777);

    // Chrome closes the port before the commit: nothing becomes visible.
    let partial = exchange(&tmp, &upload_messages(&bytes, false));
    assert_eq!(partial[0]["type"], "image_save_ready");
    assert!(partial[1..]
        .iter()
        .all(|reply| reply["type"] == "image_save_chunk_ack"));
    assert_eq!(image_rows(&tmp.default_vault()), (0, 0));

    // A full upload in a new process, interleaved with an ordinary message.
    let mut messages = upload_messages(&bytes, true);
    messages.push(visit("v1", "https://a.test/", "2026-10-29T10:00:00.000Z"));
    let replies = exchange(&tmp, &messages);
    let saved = &replies[replies.len() - 2];
    assert_eq!(saved["type"], "image_saved", "{saved}");
    assert_eq!(saved["replayed"], false);
    assert_eq!(
        replies.last().unwrap(),
        &json!({ "ok": true, "eventId": "v1" })
    );
    for reply in &replies {
        assert!(
            serde_json::to_vec(reply).unwrap().len() < 1_000_000,
            "host replies stay below 1 MB"
        );
    }
    assert_eq!(image_rows(&tmp.default_vault()), (1, 1));
    let stored: Vec<u8> = Connection::open(tmp.default_vault())
        .unwrap()
        .query_row("SELECT data FROM image_blobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, bytes);

    // The answer was lost and the extension retries the same save action: no second history row.
    let retry = exchange(&tmp, &upload_messages(&bytes, true)[..1]);
    assert_eq!(retry[0]["type"], "image_saved");
    assert_eq!(retry[0]["replayed"], true);
    assert_eq!(image_rows(&tmp.default_vault()), (1, 1));

    // Existing commands keep working after image traffic.
    assert_eq!(
        exchange(&tmp, &[search("q")])[0]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn an_image_save_without_a_vault_fails_visibly() {
    let tmp = TempDir::new();
    let reply = &exchange(&tmp, &upload_messages(&png_of_size(40, 30, 200), true)[..1])[0];
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["type"], "image_save_error");
    assert_eq!(reply["error"], "vault_not_configured");
    assert!(!tmp.app_dir().exists());
}

#[test]
fn the_image_tab_queries_answer_from_the_real_host_within_the_size_limit() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let bytes = png_of_size(40, 30, 5000);
    let saved = exchange(&tmp, &upload_messages(&bytes, true));
    let token = saved.last().unwrap()["vaultToken"]
        .as_str()
        .unwrap()
        .to_owned();
    let replies = exchange(
        &tmp,
        &[
            json!({ "type": "image_list", "requestId": "l" }),
            json!({ "type": "image_thumbnails", "requestId": "t", "blobIds": [1] }),
            json!({ "type": "image_sources", "requestId": "s", "blobId": 1 }),
        ],
    );
    for reply in &replies {
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(
            reply["vaultToken"],
            json!(token),
            "every answer names the vault"
        );
        assert!(serde_json::to_vec(reply).unwrap().len() < 1_000_000);
    }
    assert_eq!(replies[0]["items"][0]["blobId"], 1);
    assert!(replies[0]["items"][0].get("data").is_none());
    assert_eq!(
        replies[1]["items"][0],
        json!({ "blobId": 1, "missing": true }),
        "saved without a thumbnail"
    );
    assert_eq!(
        replies[2]["items"][0]["pageUrl"],
        "https://page.test/thread/1"
    );
}

#[test]
fn originals_are_read_back_in_chunks_from_the_real_host_unchanged() {
    use base64::Engine;
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let bytes = png_of_size(40, 30, 1024 * 1024 + 4321);
    let saved = exchange(&tmp, &upload_messages(&bytes, true));
    let saved = saved.last().unwrap();
    let token = saved["vaultToken"].as_str().unwrap().to_owned();
    let list = &exchange(&tmp, &[json!({ "type": "image_list", "requestId": "l" })])[0];
    let sha256 = list["items"][0]["sha256"].as_str().unwrap().to_owned();
    let mut read = Vec::new();
    for round in 0.. {
        assert!(round < 10, "the read ends");
        let reply = &exchange(
            &tmp,
            &[
                json!({ "type": "image_read", "requestId": "r", "blobId": saved["blobId"], "sha256": sha256, "vaultToken": token, "offset": read.len() }),
            ],
        )[0];
        assert_eq!(reply["ok"], true, "{reply}");
        assert!(serde_json::to_vec(reply).unwrap().len() < 1_000_000);
        read.extend(
            base64::engine::general_purpose::STANDARD
                .decode(reply["data"].as_str().unwrap())
                .unwrap(),
        );
        if read.len() as u64 == reply["byteLength"].as_u64().unwrap() {
            break;
        }
    }
    assert_eq!(read, bytes);
    let stale = &exchange(
        &tmp,
        &[
            json!({ "type": "image_read", "requestId": "r", "blobId": saved["blobId"], "sha256": sha256, "vaultToken": "0".repeat(64), "offset": 0 }),
        ],
    )[0];
    assert_eq!(stale["error"], "vault_switched");
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `upload_messages` for another save action of the same bytes.
fn upload_as(bytes: &[u8], save_id: &str, vault_token: Option<&str>) -> Vec<Value> {
    let mut messages = upload_messages(bytes, true);
    for message in &mut messages {
        message["saveId"] = save_id.into();
    }
    if let Some(token) = vault_token {
        messages[0]["vaultToken"] = token.into();
    }
    messages
}

/// Reads an original back with one host process per chunk, as the side panel does.
fn read_back(tmp: &TempDir, blob_id: &Value, sha256: &str, token: &str) -> Result<Vec<u8>, Value> {
    use base64::Engine;
    let mut bytes = Vec::new();
    loop {
        let reply = exchange(
            tmp,
            &[json!({ "type": "image_read", "requestId": "r", "blobId": blob_id, "sha256": sha256, "vaultToken": token, "offset": bytes.len() })],
        )
        .remove(0);
        if reply["ok"] != true {
            return Err(reply);
        }
        assert!(serde_json::to_vec(&reply).unwrap().len() < 1_000_000);
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(reply["data"].as_str().unwrap())
            .unwrap();
        assert!(!chunk.is_empty() && chunk.len() <= 512 * 1024);
        bytes.extend(chunk);
        if bytes.len() as u64 == reply["byteLength"].as_u64().unwrap() {
            return Ok(bytes);
        }
    }
}

fn blob_state(path: &Path) -> Vec<String> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = db
        .prepare("SELECT 'b|' || id || '|' || sha256 || '|' || hex(data) || '|' || save_count || '|' || created_at || '|' || last_saved_at FROM image_blobs UNION ALL SELECT 's|' || save_id || '|' || blob_id || '|' || saved_at FROM saved_images UNION ALL SELECT 't|' || blob_id || '|' || hex(data) FROM image_thumbnails ORDER BY 1")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.collect::<Result<_, _>>().unwrap()
}

/// Slice 1–4 end to end over the real host: save → dedup and idempotent history → list → chunked
/// read-back → thumbnail rebuild → backup → vault switch and back. Export writes the same bytes
/// through the OS dialog, which cannot run here; `image_original::tests` covers it with a fake dialog.
#[test]
fn an_image_survives_save_list_read_rebuild_backup_and_vault_switches_unchanged() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let vault_a = tmp.default_vault();
    let original = png_of_size(40, 30, 3 * 1024 * 1024 + 4321);
    let original_sha = hex_sha256(&original);

    // 1. Save (one port = one process); the answer is "lost" and the action retried: no duplicate.
    let saved = exchange(&tmp, &upload_messages(&original, true));
    let saved = saved.last().unwrap().clone();
    assert_eq!(saved["type"], "image_saved", "{saved}");
    let token_a = saved["vaultToken"].as_str().unwrap().to_owned();
    let retry = exchange(&tmp, &upload_as(&original, SAVE_ID, Some(&token_a)));
    assert_eq!(retry[0]["replayed"], true);

    // 2. The same bytes saved again from another action: no upload, one blob, two history rows.
    let mut messages = upload_as(&original, "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d", None);
    // Told `upload: false`, the extension commits without sending chunks.
    messages.retain(|message| message["type"] != "image_save_chunk");
    let again = exchange(&tmp, &messages);
    assert_eq!(again[0]["type"], "image_save_ready");
    assert_eq!(
        again[0]["upload"], false,
        "deduplicated before any bytes move"
    );
    let again = again.last().unwrap();
    assert_eq!(again["type"], "image_saved", "{again}");
    assert_eq!(again["deduplicated"], true);
    assert_eq!(image_rows(&vault_a), (1, 2));

    // 3. The Image tab lists it with the original's hash and no thumbnail.
    let list = exchange(&tmp, &[json!({ "type": "image_list", "requestId": "l" })]).remove(0);
    let item = &list["items"][0];
    assert_eq!(item["sha256"], original_sha.as_str());
    assert_eq!(item["saveCount"], 2);
    assert_eq!(item["hasThumbnail"], false);
    let blob_id = item["blobId"].clone();

    // 4. Read back in chunks: identical to what the page served.
    let read = read_back(&tmp, &blob_id, &original_sha, &token_a).unwrap();
    assert_eq!(hex_sha256(&read), original_sha);
    assert!(read == original);

    // 5. Rebuild the missing thumbnail: only the thumbnail is added.
    let before = blob_state(&vault_a);
    let thumbnail = png_of_size(20, 15, 400);
    let put = exchange(
        &tmp,
        &[json!({ "type": "image_thumbnail_put", "requestId": "t", "blobId": blob_id, "sha256": original_sha, "vaultToken": token_a,
                  "thumbnail": { "mimeType": "image/png", "width": 20, "height": 15, "generator": 1,
                                 "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &thumbnail) } })],
    )
    .remove(0);
    assert_eq!(put["stored"], true, "{put}");
    let after = blob_state(&vault_a);
    assert_eq!(after.len(), before.len() + 1);
    assert!(
        before.iter().all(|row| after.contains(row)),
        "nothing else changed"
    );
    let list = exchange(&tmp, &[json!({ "type": "image_list", "requestId": "l" })]).remove(0);
    assert_eq!(list["items"][0]["hasThumbnail"], true);

    // 6. A backup holds the image, its history and thumbnail completely.
    let backups = tmp.0.join("backups");
    std::fs::create_dir(&backups).unwrap();
    let backup = exchange(
        &tmp,
        &[json!({ "type": "vault_backup", "requestId": "b", "directory": backups })],
    )
    .remove(0);
    let copy = PathBuf::from(backup["backupPath"].as_str().unwrap());
    assert_eq!(blob_state(&copy), blob_state(&vault_a));

    // 7. Switch to another vault: the old token and the old image are refused, nothing mixes in.
    let vault_b = tmp.0.join("second.sqlite3");
    let created = exchange(
        &tmp,
        &[json!({ "type": "vault_create", "requestId": "c", "path": vault_b })],
    )
    .remove(0);
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(
        read_back(&tmp, &blob_id, &original_sha, &token_a).unwrap_err()["error"],
        "vault_switched"
    );
    let list_b = exchange(&tmp, &[json!({ "type": "image_list", "requestId": "l" })]).remove(0);
    assert_eq!(list_b["items"], json!([]));
    let token_b = list_b["vaultToken"].as_str().unwrap().to_owned();
    assert_ne!(token_b, token_a);
    // A retry of a save bound to vault A is refused in vault B.
    let refused = exchange(
        &tmp,
        &upload_as(
            &original,
            "1b2c3d4e-5f60-4718-8293-a4b5c6d7e8f9",
            Some(&token_a),
        ),
    );
    assert_eq!(refused[0]["error"], "vault_switched");
    assert_eq!(image_rows(&vault_b), (0, 0));

    // 8. Back to vault A: same identity, same image, untouched by the detour.
    let reopened = exchange(
        &tmp,
        &[json!({ "type": "vault_open", "requestId": "o", "path": vault_a })],
    )
    .remove(0);
    assert_eq!(reopened["ok"], true, "{reopened}");
    assert!(read_back(&tmp, &blob_id, &original_sha, &token_a).unwrap() == original);
    assert_eq!(blob_state(&vault_a), after);
}

fn undeclared_variants(kind: fn(&str, &str, &str) -> Value) -> Vec<Value> {
    let url = "https://secret.test/private";
    let at = "2026-10-30T10:00:00.000Z";
    let mut events: Vec<Value> = [json!(true), json!("false"), json!(0), Value::Null]
        .into_iter()
        .enumerate()
        .map(|(index, incognito)| {
            let mut event = kind(&format!("secret-{index}"), url, at);
            event["incognito"] = incognito;
            event
        })
        .collect();
    let mut missing = kind("secret-missing", url, at);
    missing.as_object_mut().unwrap().remove("incognito");
    events.push(missing);
    events
}

fn assert_refused_without_data(response: &Value, id: &str) {
    assert_eq!(
        response,
        &json!({ "ok": false, "eventId": id, "error": "incognito_not_allowed" }),
        "the answer carries no URL or title"
    );
}

#[test]
fn events_not_declared_normal_are_never_stored() {
    for kind in [visit as fn(&str, &str, &str) -> Value, snapshot] {
        let tmp = TempDir::new();
        create_default_vault(&tmp);
        let events = undeclared_variants(kind);
        let responses = exchange(&tmp, &events);
        for (event, response) in events.iter().zip(&responses) {
            assert_refused_without_data(response, event["id"].as_str().unwrap());
        }
        assert_eq!(row_dump(&tmp.default_vault()), Vec::<String>::new());
    }
}

#[test]
fn the_incognito_check_comes_before_any_other_refusal() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    // Otherwise the extension would quarantine these with their URL as invalid events.
    let mut bad_url = snapshot("a", "javascript:alert(1)", "2026-10-30T10:00:00.000Z");
    bad_url["incognito"] = json!(true);
    let incomplete = json!({ "type": "visit", "id": "b", "url": "https://secret.test/" });
    let responses = exchange(&tmp, &[bad_url, incomplete]);
    assert_refused_without_data(&responses[0], "a");
    assert_refused_without_data(&responses[1], "b");
}

#[test]
fn a_mixed_snapshot_stores_exactly_the_normal_tabs_once_and_the_acks_match() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let at = "2026-10-30T10:00:00.000Z";
    let mut secret = snapshot("secret", "https://secret.test/private", at);
    secret["incognito"] = json!(true);
    let events = [
        snapshot("normal-1", "https://a.test/", at),
        secret,
        snapshot("normal-2", "https://b.test/", at),
    ];
    let responses = exchange(&tmp, &events);
    assert_eq!(responses[0], json!({ "ok": true, "eventId": "normal-1" }));
    assert_refused_without_data(&responses[1], "secret");
    assert_eq!(responses[2], json!({ "ok": true, "eventId": "normal-2" }));
    let expected = vec![
        format!("s|normal-1|https://a.test/|Rust Book|{at}"),
        format!("s|normal-2|https://b.test/|Rust Book|{at}"),
    ];
    let vault = tmp.default_vault();
    assert_eq!(row_dump(&vault), expected);
    // A resend after a lost ACK is still idempotent.
    assert_eq!(exchange(&tmp, &events), responses);
    assert_eq!(row_dump(&vault), expected);
}

#[test]
fn a_failed_write_stores_nothing_and_a_refusal_does_not_depend_on_the_vault() {
    let tmp = TempDir::new();
    let at = "2026-10-30T10:00:00.000Z";
    let mut secret = visit("secret", "https://secret.test/private", at);
    secret["incognito"] = json!(true);
    let events = [secret, visit("v", "https://a.test/", at)];
    // No vault at all: the refusal stands, the normal event stays a temporary failure.
    let responses = exchange(&tmp, &events);
    assert_refused_without_data(&responses[0], "secret");
    assert_eq!(responses[1]["ok"], false);
    assert_ne!(responses[1]["error"], "incognito_not_allowed");
    assert!(
        !tmp.default_vault().exists(),
        "nothing is created implicitly"
    );

    // A vault locked by another writer: no partial row, and the retry stores it once.
    create_default_vault(&tmp);
    let vault = tmp.default_vault();
    let lock = Connection::open(&vault).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let responses = exchange(&tmp, &events);
    assert_refused_without_data(&responses[0], "secret");
    assert_eq!(responses[1]["ok"], false);
    lock.execute_batch("ROLLBACK;").unwrap();
    assert_eq!(row_dump(&vault), Vec::<String>::new());
    let responses = exchange(&tmp, &events);
    assert_refused_without_data(&responses[0], "secret");
    assert_eq!(responses[1], json!({ "ok": true, "eventId": "v" }));
    assert_eq!(row_dump(&vault).len(), 1);
}

#[test]
fn an_incognito_image_is_saved_and_a_visit_from_incognito_still_is_not() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let vault = tmp.default_vault();
    let bytes = png_of_size(40, 30, 2 * 1024 * 1024 + 7);
    let mut messages = upload_messages(&bytes, true);
    messages[0]["incognito"] = json!(true);
    let saved = exchange(&tmp, &messages).pop().unwrap();
    assert_eq!(saved["type"], "image_saved", "{saved}");
    assert_eq!(image_rows(&vault), (1, 1));
    let sources = exchange(
        &tmp,
        &[json!({ "type": "image_sources", "requestId": "s", "blobId": saved["blobId"] })],
    )
    .remove(0);
    assert_eq!(sources["items"][0]["incognito"], true, "{sources}");

    let mut secret = visit("secret", "https://secret.test/", "2026-10-30T10:00:00.000Z");
    secret["incognito"] = json!(true);
    assert_eq!(
        exchange(&tmp, &[secret]).remove(0)["error"],
        "incognito_not_allowed"
    );
    assert_eq!(row_dump(&vault), Vec::<String>::new());
}

/// A vault as the previous version made it: one image, no `incognito` column, schema 2.
fn make_schema_2_vault(tmp: &TempDir) -> PathBuf {
    create_default_vault(tmp);
    let vault = tmp.default_vault();
    Connection::open(&vault)
        .unwrap()
        .execute_batch(
            "DROP TABLE saved_images;
             CREATE TABLE saved_images (id INTEGER PRIMARY KEY, save_id TEXT NOT NULL UNIQUE, blob_id INTEGER NOT NULL, image_url TEXT NOT NULL, page_url TEXT, frame_url TEXT, saved_at INTEGER NOT NULL);
             INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data) VALUES (printf('%064d', 1), 'image/png', 1, 1, 1, 1, 1, x'00');
             INSERT INTO saved_images (save_id, blob_id, image_url, saved_at) VALUES ('old', 1, 'https://img.test/old.png', 1);
             PRAGMA user_version = 2;",
        )
        .unwrap();
    vault
}

#[test]
fn a_schema_2_vault_stops_images_with_an_explicit_error_and_is_left_unchanged() {
    let tmp = TempDir::new();
    let vault = make_schema_2_vault(&tmp);
    let state = |path: &Path| -> (i64, i64, i64) {
        let db =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        db.query_row(
            "SELECT (SELECT user_version FROM pragma_user_version), (SELECT count(*) FROM saved_images), (SELECT count(*) FROM pragma_table_info('saved_images') WHERE name = 'incognito')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(state(&vault), (2, 1, 0));

    let bytes = png_of_size(40, 30, 5000);
    for reply in [
        exchange(&tmp, &[json!({ "type": "image_list", "requestId": "l" })]).remove(0),
        exchange(
            &tmp,
            &[json!({ "type": "image_sources", "requestId": "s", "blobId": 1 })],
        )
        .remove(0),
        exchange(&tmp, &upload_messages(&bytes, false)[..1]).remove(0),
    ] {
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(reply["error"], "image_schema_mismatch", "{reply}");
    }
    assert_eq!(state(&vault), (2, 1, 0));

    // URL history and a backup of the old vault keep working until the PO replaces it.
    let visited = exchange(
        &tmp,
        &[visit("v", "https://a.test/", "2026-10-30T10:00:00.000Z")],
    );
    assert_eq!(visited[0], json!({ "ok": true, "eventId": "v" }));
    let backups = tmp.0.join("backups");
    std::fs::create_dir(&backups).unwrap();
    let backup = exchange(
        &tmp,
        &[json!({ "type": "vault_backup", "requestId": "b", "directory": backups })],
    )
    .remove(0);
    assert_eq!(backup["ok"], true, "{backup}");
}
