//! Drives the real host binary over stdio with Native Messaging framing.
//! Every test uses its own temp directory as `MEMOCAMPUS_APP_DIR`; no user data is touched.

use memocampus_native_host::policy_store::PolicyStore;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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

/// The path the host reports for an existing file: the temp path may be an alias (8.3 name,
/// junction) of the real one, and the host resolves it when it creates a vault.
fn real_path(path: &Path) -> String {
    let path = std::fs::canonicalize(path).unwrap();
    let path = path.to_string_lossy();
    path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
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

/// Explicit first-use step; repeating it is harmless because it never replaces the policy.
fn initialize_policy(tmp: &TempDir) {
    let response = &exchange(
        tmp,
        &[json!({ "type": "policy_initialize", "requestId": "p" })],
    )[0];
    assert!(
        response["ok"] == true || response["error"] == "policy_already_initialized",
        "{response}"
    );
}

fn create_default_vault(tmp: &TempDir) {
    initialize_policy(tmp);
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
        json!({ "ok": false, "eventId": "v1", "error": "policy_not_initialized" })
    );
    assert_eq!(
        responses[2],
        json!({ "ok": false, "eventId": "s1", "error": "policy_not_initialized" })
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
    // The page names its vault, as `image_list` does (Issue #43).
    let token = exchange(&tmp, &[json!({ "type": "image_list", "requestId": "l" })])[0]
        ["vaultToken"]
        .clone();
    assert_eq!(token.as_str().map(str::len), Some(64));
    assert_eq!(
        responses[0],
        json!({
            "ok": true, "type": "search_pages_result", "requestId": "r1", "hasMore": false, "vaultToken": token,
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
    assert_eq!(status["vaultPath"], real_path(&tmp.default_vault()));
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

    initialize_policy(&tmp);
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
        real_path(&tmp.default_vault()),
        "active vault unchanged"
    );
}

#[test]
fn development_override_writes_there_and_disables_management() {
    let tmp = TempDir::new();
    let dev = tmp.0.join("dev.sqlite3");
    initialize_policy(&tmp);
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
fn vaults_created_by_the_host_start_at_schema_4_with_image_tables() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    assert_eq!(pragma(&tmp.default_vault(), "user_version"), 7);
    assert_eq!(image_tables(&tmp.default_vault()), 3);
    // Existing operations work on it.
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
        db.execute_batch("INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public) VALUES ('a', 1, 'https://img.test/1.png', 'https://page.test/', 1, 0, 1), ('b', 1, 'data:image/png', NULL, 2, 1, 0); INSERT INTO image_thumbnails (blob_id, mime_type, width, height, generator, data) VALUES (1, 'image/webp', 1, 1, 1, x'00');").unwrap();
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
            .prepare("SELECT 'b|' || sha256 || '|' || hex(data) || '|' || save_count FROM image_blobs UNION ALL SELECT 's|' || save_id || '|' || image_url || '|' || saved_at || '|' || incognito || '|' || mcp_public FROM saved_images UNION ALL SELECT 't|' || blob_id || '|' || hex(data) FROM image_thumbnails ORDER BY 1")
            .unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(dump(&copy), dump(&tmp.default_vault()));
    assert_eq!(dump(&copy).len(), 4);
    assert_eq!(pragma(&copy, "user_version"), 7);
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

// --- Only schema 4 is ever used (Issue #12, Slice 2) ---------------------------------------------

/// The last format before schema 4, with rows that must never surface.
const SCHEMA_3: &str = "
CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL);
CREATE TABLE tab_snapshots (event_id TEXT PRIMARY KEY, window_id INTEGER NOT NULL, tab_index INTEGER NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, pinned INTEGER NOT NULL, incognito INTEGER NOT NULL, recorded_at TEXT NOT NULL);
CREATE TABLE image_blobs (id INTEGER PRIMARY KEY, sha256 TEXT NOT NULL UNIQUE CHECK (length(sha256) = 64), mime_type TEXT NOT NULL, byte_length INTEGER NOT NULL CHECK (byte_length > 0), width INTEGER NOT NULL CHECK (width > 0), height INTEGER NOT NULL CHECK (height > 0), created_at INTEGER NOT NULL, last_saved_at INTEGER NOT NULL, save_count INTEGER NOT NULL DEFAULT 1 CHECK (save_count > 0), data BLOB NOT NULL);
CREATE TABLE saved_images (id INTEGER PRIMARY KEY, save_id TEXT NOT NULL UNIQUE, blob_id INTEGER NOT NULL REFERENCES image_blobs (id) ON DELETE RESTRICT, image_url TEXT NOT NULL, page_url TEXT, frame_url TEXT, saved_at INTEGER NOT NULL, incognito INTEGER NOT NULL CHECK (incognito IN (0, 1)));
CREATE TABLE image_thumbnails (blob_id INTEGER PRIMARY KEY REFERENCES image_blobs (id) ON DELETE CASCADE, mime_type TEXT NOT NULL, width INTEGER NOT NULL CHECK (width > 0), height INTEGER NOT NULL CHECK (height > 0), generator INTEGER NOT NULL CHECK (generator > 0), data BLOB NOT NULL);
INSERT INTO visits VALUES ('old', 'https://old.test/CANARY', 'CANARY title', '2026-10-01T00:00:00.000Z');
INSERT INTO tab_snapshots VALUES ('old-s', 1, 0, 'https://old.test/CANARY-s', 'CANARY', 0, 0, '2026-10-01T00:00:00.000Z');
INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data) VALUES (printf('%064d', 1), 'image/png', 1, 1, 1, 1, 1, x'00');
INSERT INTO saved_images (save_id, blob_id, image_url, saved_at, incognito) VALUES ('old', 1, 'https://img.test/CANARY.png', 1, 0);
";

/// Replaces the active vault's file. The config keeps naming it, as after an upgrade of the host.
fn make_active_vault_from(tmp: &TempDir, sql: &str) -> PathBuf {
    initialize_policy(tmp);
    create_default_vault(tmp);
    let vault = tmp.default_vault();
    std::fs::remove_file(&vault).unwrap();
    Connection::open(&vault)
        .unwrap()
        .execute_batch(sql)
        .unwrap();
    vault
}

const VISIT_AT: &str = "2026-10-30T10:00:00.000Z";

/// `sql` without the `CREATE TABLE` statement of `table`.
fn without_table(sql: &str, table: &str) -> String {
    let start = sql.find(&format!(" CREATE TABLE {table} ")).unwrap();
    let end = start + sql[start..].find(");").unwrap() + 2;
    format!("{}{}", &sql[..start], &sql[end..])
}

/// `sql` without the `CREATE TRIGGER` statement of `name`.
fn without_trigger(sql: &str, name: &str) -> String {
    let start = sql.find(&format!(" CREATE TRIGGER {name} ")).unwrap();
    let end = start + sql[start..].find("END;").unwrap() + 4;
    format!("{}{}", &sql[..start], &sql[end..])
}

/// Schema 6 as it was created before Issue #44: schema 7 without image tags.
fn schema_6() -> String {
    let sql = without_trigger(
        &memocampus_native_host::schema::create_sql(),
        "image_tags_drop_unused",
    )
    .replace(
        " CREATE INDEX image_tag_links_by_tag ON image_tag_links (tag_id, blob_id);",
        "",
    );
    let sql = without_table(&without_table(&sql, "image_tag_links"), "image_tags");
    assert!(!sql.contains("image_tag"));
    sql.replace("PRAGMA user_version = 7;", "PRAGMA user_version = 6;")
}

/// Schema 5 as it was created before Issue #41: schema 6 without `navigation_events`.
fn schema_5() -> String {
    let sql = without_table(&schema_6(), "navigation_events").replace(
        " CREATE INDEX navigation_events_by_source ON navigation_events (source_visit_id);",
        "",
    );
    assert!(!sql.contains("navigation_events"));
    sql.replace("PRAGMA user_version = 6;", "PRAGMA user_version = 5;")
}

/// Schema 4 as it was created before Issue #22: schema 5 without `page_metadata`.
fn schema_4() -> String {
    let sql = without_table(&schema_5(), "page_metadata");
    assert!(!sql.contains("page_metadata"));
    sql.replace("PRAGMA user_version = 5;", "PRAGMA user_version = 4;")
}

#[test]
fn every_vault_other_than_schema_7_is_refused_for_every_operation_and_left_untouched() {
    use memocampus_native_host::schema::create_sql;
    let v3 = |app_id: i64, version: i64| {
        format!("{SCHEMA_3} PRAGMA application_id = {app_id}; PRAGMA user_version = {version};")
    };
    let cases = [
        ("schema 0, untagged", v3(0, 0)),
        ("schema 1", v3(1296256336, 1)),
        ("schema 2", v3(1296256336, 2)),
        ("schema 3", v3(1296256336, 3)),
        ("schema 3 relabelled 4", v3(1296256336, 4)),
        ("schema 3 untagged, relabelled 4", v3(0, 4)),
        ("schema 4 (Issue #22: no page_metadata)", schema_4()),
        ("schema 4 relabelled 5", schema_4().replace("PRAGMA user_version = 4;", "PRAGMA user_version = 5;")),
        ("schema 5 (Issue #41: no navigation_events)", schema_5()),
        ("schema 5 relabelled 6", schema_5().replace("PRAGMA user_version = 5;", "PRAGMA user_version = 6;")),
        ("schema 6 labelled 5", schema_6().replace("PRAGMA user_version = 6;", "PRAGMA user_version = 5;")),
        ("schema 6 (Issue #44: no image tags)", schema_6()),
        ("schema 6 relabelled 7", schema_6().replace("PRAGMA user_version = 6;", "PRAGMA user_version = 7;")),
        ("schema 7 labelled 6", create_sql().replace("PRAGMA user_version = 7;", "PRAGMA user_version = 6;")),
        ("future version", format!("{} PRAGMA user_version = 8;", create_sql())),
        ("untagged schema 7", format!("{} PRAGMA application_id = 0;", create_sql())),
        ("tags without the trigger that drops unused ones", without_trigger(&create_sql(), "image_tags_drop_unused")),
        ("tags without their search index", create_sql().replace(
            " CREATE INDEX image_tag_links_by_tag ON image_tag_links (tag_id, blob_id);",
            "",
        )),
        ("tag names without their CHECK", create_sql().replace(
            "name  TEXT    NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 64 AND name = trim(name) AND name = lower(name))",
            "name  TEXT    NOT NULL UNIQUE",
        )),
        ("tag links that outlive their image", create_sql().replace(
            "blob_id  INTEGER NOT NULL REFERENCES image_blobs (id) ON DELETE CASCADE,",
            "blob_id  INTEGER NOT NULL REFERENCES image_blobs (id),",
        )),
        ("navigation without its source index", create_sql().replace(
            "CREATE INDEX navigation_events_by_source ON navigation_events (source_visit_id);",
            "",
        )),
        ("navigation kind without CHECK", create_sql().replace(
            "kind             TEXT    NOT NULL CHECK (kind IN ('document', 'history_state')),",
            "kind             TEXT    NOT NULL,",
        )),
        ("navigation source without its foreign key", create_sql().replace(
            "source_visit_id  TEXT    REFERENCES visits (event_id) ON DELETE SET NULL,",
            "source_visit_id  TEXT,",
        )),
        ("metadata without its limits", create_sql().replace(
            "title          TEXT CHECK (title IS NULL OR length(title) BETWEEN 1 AND 500),",
            "title          TEXT,",
        )),
        (
            "flag with a default",
            create_sql().replace(
                "mcp_public INTEGER NOT NULL CHECK",
                "mcp_public INTEGER NOT NULL DEFAULT 0 CHECK",
            ),
        ),
        ("flag without CHECK", create_sql().replace(
            "mcp_public INTEGER NOT NULL CHECK (mcp_public IN (0, 1))",
            "mcp_public INTEGER NOT NULL",
        )),
        ("extra trigger", format!(
            "{} CREATE TRIGGER t AFTER INSERT ON visits BEGIN SELECT 1; END;",
            create_sql()
        )),
        ("missing index", create_sql().replace(
            "CREATE INDEX saved_images_by_blob ON saved_images (blob_id, saved_at DESC, id DESC);",
            "",
        )),
    ];
    for (name, sql) in cases {
        let tmp = TempDir::new();
        let vault = make_active_vault_from(&tmp, &sql);
        let before = std::fs::read(&vault).unwrap();
        let config = std::fs::read(tmp.app_dir().join("config.json")).unwrap();
        let dest = tmp.0.join("backups");
        std::fs::create_dir(&dest).unwrap();
        let bytes = png_of_size(40, 30, 5000);
        let mut messages = vec![
            visit("v", "https://a.test/", VISIT_AT),
            snapshot("s", "https://a.test/", VISIT_AT),
            search("q"),
            json!({ "type": "image_list", "requestId": "l" }),
            json!({ "type": "image_sources", "requestId": "c", "blobId": 1 }),
            json!({ "type": "image_thumbnails", "requestId": "t", "blobIds": [1] }),
            json!({ "type": "image_mcp_public_set", "requestId": "m", "savedImageId": 1, "mcpPublic": true, "vaultToken": "0".repeat(64) }),
            json!({ "type": "image_tags_get", "requestId": "tg", "blobId": 1 }),
            json!({ "type": "image_tag_add", "requestId": "ta", "blobId": 1, "sha256": "0".repeat(64), "vaultToken": "0".repeat(64), "tag": "cat" }),
            json!({ "type": "image_tag_remove", "requestId": "tr", "blobId": 1, "sha256": "0".repeat(64), "vaultToken": "0".repeat(64), "tag": "cat" }),
            json!({ "type": "image_tag_list", "requestId": "tl" }),
            json!({ "type": "image_tag_search", "requestId": "ts", "tag": "cat" }),
            json!({ "type": "vault_backup", "requestId": "b", "directory": dest }),
            json!({ "type": "vault_open", "requestId": "o", "path": vault }),
        ];
        messages.extend(upload_messages(&bytes, false).into_iter().take(1));
        let replies = exchange(&tmp, &messages);
        for reply in &replies {
            assert_eq!(reply["ok"], false, "{name}: {reply}");
            assert_eq!(reply["error"], "vault_schema_mismatch", "{name}: {reply}");
            assert!(!reply.to_string().contains("CANARY"), "{name}: {reply}");
        }
        let status = &exchange(&tmp, &[json!({ "type": "vault_status", "requestId": "s" })])[0];
        assert_eq!(status["status"]["state"], "vault_schema_mismatch", "{name}");
        assert_eq!(status["status"]["counts"], Value::Null, "{name}");

        assert_eq!(
            std::fs::read(&vault).unwrap(),
            before,
            "{name}: file changed"
        );
        assert_eq!(
            std::fs::read(tmp.app_dir().join("config.json")).unwrap(),
            config,
            "{name}"
        );
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            0,
            "{name}: backup written"
        );
        let mut neighbours: Vec<_> = std::fs::read_dir(tmp.app_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|file| file.contains("sqlite3"))
            .collect();
        neighbours.sort();
        assert_eq!(
            neighbours,
            ["memocampus.sqlite3"],
            "{name}: nothing regenerated"
        );
    }
}

#[test]
fn from_a_refused_vault_the_user_can_still_create_a_new_one_which_inherits_nothing() {
    let tmp = TempDir::new();
    let old = make_active_vault_from(
        &tmp,
        &format!("{SCHEMA_3} PRAGMA application_id = 1296256336; PRAGMA user_version = 3;"),
    );
    let before = std::fs::read(&old).unwrap();
    let new = tmp.0.join("new.sqlite3");
    let replies = exchange(
        &tmp,
        &[
            json!({ "type": "vault_create", "requestId": "c", "path": new }),
            visit("v", "https://a.test/", VISIT_AT),
            search("s"),
        ],
    );
    assert_eq!(replies[0]["ok"], true, "{}", replies[0]);
    assert_eq!(replies[0]["status"]["state"], "ready");
    assert_eq!(replies[1]["ok"], true, "{}", replies[1]);
    assert_eq!(replies[2]["items"].as_array().unwrap().len(), 1);
    assert!(!replies[2].to_string().contains("CANARY"));
    assert_eq!(pragma(&new, "user_version"), 7);
    assert_eq!(std::fs::read(&old).unwrap(), before);
    // And the old file stays refused if it is opened again.
    let reopened = &exchange(
        &tmp,
        &[json!({ "type": "vault_open", "requestId": "o", "path": old })],
    )[0];
    assert_eq!(reopened["error"], "vault_schema_mismatch");
}

#[test]
fn a_missing_or_damaged_active_vault_is_refused_and_not_regenerated() {
    let tmp = TempDir::new();
    let vault = make_active_vault_from(&tmp, "SELECT 1;");
    std::fs::remove_file(&vault).unwrap();
    let reply = &exchange(&tmp, &[search("s")])[0];
    assert_eq!(reply["error"], "vault_missing");
    assert!(!vault.exists());
    std::fs::write(&vault, "not a database at all ".repeat(100)).unwrap();
    let before = std::fs::read(&vault).unwrap();
    let reply = &exchange(&tmp, &[search("s")])[0];
    assert_eq!(reply["error"], "vault_invalid");
    assert_eq!(std::fs::read(&vault).unwrap(), before);
}

#[test]
fn images_are_saved_private_and_only_an_explicit_request_changes_one_row() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let vault = tmp.default_vault();
    let bytes = png_of_size(40, 30, 5000);
    let upload = |save_id: &str, incognito: bool, known: bool| -> Value {
        let mut messages = upload_messages(&bytes, true);
        if known {
            // The host already has these bytes: begin, then commit, without chunks.
            let commit = messages.pop().unwrap();
            messages.truncate(1);
            messages.push(commit);
        }
        messages[0]["saveId"] = save_id.into();
        messages[0]["incognito"] = incognito.into();
        for message in &mut messages[1..] {
            message["saveId"] = save_id.into();
        }
        let mut replies = exchange(&tmp, &messages);
        let last = replies.pop().unwrap();
        assert_eq!(last["ok"], true, "{last}");
        last
    };
    let first = upload("11111111-1111-4111-8111-111111111111", false, false);
    let second = upload("22222222-2222-4222-8222-222222222222", true, true);
    let (a, b) = (
        first["savedImageId"].as_i64().unwrap(),
        second["savedImageId"].as_i64().unwrap(),
    );
    assert_eq!(
        first["blobId"], second["blobId"],
        "one blob, two history rows"
    );
    let flags = || -> Vec<(i64, i64, i64)> {
        let db = Connection::open_with_flags(&vault, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        let mut statement = db
            .prepare("SELECT id, incognito, mcp_public FROM saved_images ORDER BY id")
            .unwrap();
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(flags(), [(a, 0, 0), (b, 1, 0)]);

    let sources = &exchange(
        &tmp,
        &[json!({ "type": "image_sources", "requestId": "s", "blobId": first["blobId"] })],
    )[0];
    assert_eq!(sources["items"][0]["mcpPublic"], false);
    let token = sources["vaultToken"].as_str().unwrap().to_owned();
    let set = |extra: Value| -> Value {
        let mut message =
            json!({ "type": "image_mcp_public_set", "requestId": "m", "vaultToken": token });
        for (key, value) in extra.as_object().unwrap() {
            message[key] = value.clone();
        }
        exchange(&tmp, &[message]).remove(0)
    };

    for (extra, code) in [
        (json!({ "mcpPublic": true }), "invalid_saved_image_id"),
        (
            json!({ "savedImageId": 0, "mcpPublic": true }),
            "invalid_saved_image_id",
        ),
        (
            json!({ "savedImageId": -1, "mcpPublic": true }),
            "invalid_saved_image_id",
        ),
        (
            json!({ "savedImageId": "1", "mcpPublic": true }),
            "invalid_saved_image_id",
        ),
        (
            json!({ "savedImageId": 1.5, "mcpPublic": true }),
            "invalid_saved_image_id",
        ),
        (json!({ "savedImageId": a }), "invalid_mcp_public"),
        (
            json!({ "savedImageId": a, "mcpPublic": 1 }),
            "invalid_mcp_public",
        ),
        (
            json!({ "savedImageId": a, "mcpPublic": "true" }),
            "invalid_mcp_public",
        ),
        (
            json!({ "savedImageId": a, "mcpPublic": null }),
            "invalid_mcp_public",
        ),
        (
            json!({ "savedImageId": 999, "mcpPublic": true }),
            "saved_image_not_found",
        ),
        (
            json!({ "savedImageId": a, "mcpPublic": true, "vaultToken": "x" }),
            "invalid_vault_token",
        ),
        (
            json!({ "savedImageId": a, "mcpPublic": true, "vaultToken": "0".repeat(64) }),
            "vault_switched",
        ),
    ] {
        let reply = set(extra.clone());
        assert_eq!(reply["ok"], false, "{extra}");
        assert_eq!(reply["error"], code, "{extra}");
    }
    assert_eq!(
        flags(),
        [(a, 0, 0), (b, 1, 0)],
        "no refused request changed anything"
    );

    let refused = set(json!({ "savedImageId": b, "mcpPublic": true }));
    assert_eq!(
        refused,
        json!({ "ok": false, "type": "image_mcp_public_set_result", "requestId": "m", "error": "incognito_not_publishable" }),
        "an incognito row stays private, with a fixed code"
    );
    assert_eq!(flags(), [(a, 0, 0), (b, 1, 0)]);
    assert_eq!(
        set(json!({ "savedImageId": b, "mcpPublic": false }))["ok"],
        true
    );

    let done = set(json!({ "savedImageId": a, "mcpPublic": true }));
    assert_eq!(
        done,
        json!({ "ok": true, "type": "image_mcp_public_set_result", "requestId": "m", "vaultToken": token, "savedImageId": a, "mcpPublic": true }),
        "no URL or image data in the reply"
    );
    assert_eq!(flags(), [(a, 0, 1), (b, 1, 0)]);
    assert_eq!(
        set(json!({ "savedImageId": a, "mcpPublic": false }))["ok"],
        true
    );
    assert_eq!(flags(), [(a, 0, 0), (b, 1, 0)]);
    let no_id = exchange(&tmp, &[json!({ "type": "image_mcp_public_set", "savedImageId": a, "mcpPublic": true, "vaultToken": token })]).remove(0);
    assert_eq!(no_id["error"], "invalid_request_id");
    assert_eq!(flags(), [(a, 0, 0), (b, 1, 0)]);
}

// --- Privacy policy at the recording entrance (Issue #11) ----------------------------------------

const AT: &str = "2026-10-30T10:00:00.000Z";
const AUTH_URL: &str = "https://accounts.google.com/signin/secret-path";
const TOKEN_URL: &str = "https://a.test/page?access_token=SECRET-VALUE";
const B_URL: &str = "https://mail.google.com/mail/u/0/";

fn dropped(id: &str) -> Value {
    json!({ "ok": true, "eventId": id, "dropped": "policy" })
}

fn stored(id: &str) -> Value {
    json!({ "ok": true, "eventId": id })
}

fn store_of(tmp: &TempDir) -> PolicyStore {
    PolicyStore::new(tmp.app_dir()).unwrap()
}

fn add_never_save_rule(tmp: &TempDir, rule: &str) {
    let store = store_of(tmp);
    let current = store.load().unwrap();
    let mut edited = current.policy.clone();
    edited.layers.never_save.rules.push(rule.into());
    store.update(&current, edited).unwrap();
}

/// The failure answers exactly `code`: not a success, not a drop, and no URL or title in it.
fn assert_policy_failure(response: &Value, id: &str, code: &str) {
    assert_eq!(
        response,
        &json!({ "ok": false, "eventId": id, "error": code }),
        "a failure is never a success or a drop, and carries no event data"
    );
}

#[test]
fn a_refused_url_is_acknowledged_as_dropped_and_allowed_ones_are_stored() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let responses = exchange(
        &tmp,
        &[
            visit("auth", AUTH_URL, AT),
            visit("token", TOKEN_URL, AT),
            visit("malformed", "https://a.test/?q=%FF", AT),
            visit("search", "https://www.google.com/search?q=rust", AT),
            visit("normal", "https://normal.test/", AT),
            visit("layer-b", B_URL, AT),
        ],
    );
    assert_eq!(
        responses,
        [
            dropped("auth"),
            dropped("token"),
            dropped("malformed"),
            stored("search"),
            stored("normal"),
            stored("layer-b"),
        ]
    );
    let rows = row_dump(&tmp.default_vault());
    assert_eq!(rows.len(), 3, "{rows:?}");
    let all = rows.join("\n");
    for secret in ["accounts.google.com", "SECRET-VALUE", "%FF"] {
        assert!(!all.contains(secret));
    }
}

#[test]
fn a_refused_url_is_dropped_even_when_the_event_is_otherwise_invalid() {
    // Answering `invalid_event` would have an older extension quarantine the URL.
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let incomplete = json!({ "type": "visit", "id": "bad", "url": AUTH_URL, "incognito": false });
    assert_eq!(exchange(&tmp, &[incomplete]).remove(0), dropped("bad"));
    assert_eq!(row_dump(&tmp.default_vault()), Vec::<String>::new());
}

#[test]
fn tab_snapshots_are_judged_member_by_member() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let members = [
        snapshot("s-normal", "https://normal.test/", AT),
        snapshot("s-auth", AUTH_URL, AT),
        snapshot("s-b", B_URL, AT),
        snapshot("s-token", TOKEN_URL, AT),
    ];
    let expected = [
        stored("s-normal"),
        dropped("s-auth"),
        stored("s-b"),
        dropped("s-token"),
    ];
    assert_eq!(exchange(&tmp, &members), expected);
    let vault = tmp.default_vault();
    let rows = row_dump(&vault);
    assert_eq!(
        rows,
        [
            format!("s|s-b|{B_URL}|Rust Book|{AT}"),
            format!("s|s-normal|https://normal.test/|Rust Book|{AT}"),
        ]
    );
    // Event-id idempotency holds for both outcomes.
    assert_eq!(exchange(&tmp, &members), expected);
    assert_eq!(row_dump(&vault), rows);

    // Every member refused: nothing at all is stored.
    let other = TempDir::new();
    create_default_vault(&other);
    let all = [snapshot("a", AUTH_URL, AT), snapshot("b", TOKEN_URL, AT)];
    assert_eq!(exchange(&other, &all), [dropped("a"), dropped("b")]);
    assert_eq!(row_dump(&other.default_vault()), Vec::<String>::new());
}

#[test]
fn an_uninitialized_policy_stores_nothing_and_never_answers_success() {
    let tmp = TempDir::new();
    // A vault exists, but the policy was never initialized.
    let vault = tmp.0.join("v.sqlite3");
    let created = &exchange(
        &tmp,
        &[json!({ "type": "vault_create", "requestId": "c", "path": vault })],
    )[0];
    assert_eq!(created["ok"], true, "{created}");
    let responses = exchange(
        &tmp,
        &[
            visit("normal", "https://normal.test/", AT),
            visit("auth", AUTH_URL, AT),
            snapshot("snap", "https://normal.test/", AT),
            json!({ "type": "policy_status", "requestId": "s" }),
        ],
    );
    assert_policy_failure(&responses[0], "normal", "policy_not_initialized");
    assert_policy_failure(&responses[1], "auth", "policy_not_initialized");
    assert_policy_failure(&responses[2], "snap", "policy_not_initialized");
    assert_eq!(
        responses[3],
        json!({ "ok": true, "type": "policy_result", "requestId": "s", "state": "uninitialized" })
    );
    assert_eq!(row_dump(&vault), Vec::<String>::new());
    let app = tmp.app_dir();
    assert!(
        !app.join("policy.json").exists(),
        "never created implicitly"
    );
    assert!(!app.join("policy.initialized").exists());
}

#[test]
fn explicit_initialization_starts_recording_and_never_replaces_a_policy() {
    let tmp = TempDir::new();
    let responses = exchange(
        &tmp,
        &[
            json!({ "type": "policy_initialize", "requestId": "i1" }),
            json!({ "type": "policy_status", "requestId": "s" }),
            json!({ "type": "policy_initialize", "requestId": "i2" }),
            json!({ "type": "policy_status" }),
        ],
    );
    assert_eq!(responses[0]["ok"], true, "{}", responses[0]);
    assert_eq!(responses[0]["state"], "ready");
    assert_eq!(responses[0]["revision"], 1);
    assert_eq!(responses[1]["etag"], responses[0]["etag"]);
    assert_eq!(
        responses[1]["schemaVersion"], 2,
        "a new policy is version 2"
    );
    assert_eq!(responses[1]["engineVersion"], 2);
    assert!(responses[0].get("policy").is_none() && responses[1].get("policy").is_none());
    assert_eq!(
        responses[2],
        json!({ "ok": false, "type": "policy_result", "requestId": "i2", "error": "policy_already_initialized" })
    );
    assert_eq!(responses[3]["error"], "invalid_request_id");
    assert_eq!(store_of(&tmp).load().unwrap().policy.revision, 1);

    add_never_save_rule(&tmp, "mine.test");
    let before = std::fs::read(tmp.app_dir().join("policy.json")).unwrap();
    let again = exchange(
        &tmp,
        &[json!({ "type": "policy_initialize", "requestId": "i3" })],
    );
    assert_eq!(again[0]["error"], "policy_already_initialized");
    assert_eq!(
        std::fs::read(tmp.app_dir().join("policy.json")).unwrap(),
        before
    );
}

#[test]
fn every_policy_failure_stops_recording_keeps_the_files_and_recovers() {
    let at_app = |tmp: &TempDir, name: &str| tmp.app_dir().join(name);
    type Damage = fn(&Path);
    let cases: [(&str, Damage, &str); 6] = [
        (
            "policy.json",
            |p| std::fs::write(p, b"{broken").unwrap(),
            "invalid",
        ),
        (
            "policy.json",
            |p| std::fs::write(p, br#"{"schemaVersion":2}"#).unwrap(),
            "invalid",
        ),
        (
            "policy.json",
            |p| std::fs::remove_file(p).unwrap(),
            "invalid",
        ),
        (
            "policy.initialized",
            |p| std::fs::remove_file(p).unwrap(),
            "invalid",
        ),
        (
            "policy.initialized",
            |p| std::fs::write(p, b"unfinished").unwrap(),
            "invalid",
        ),
        (
            "policy.lock",
            |p| std::fs::remove_file(p).unwrap(),
            "invalid",
        ),
    ];
    for (name, damage, state) in cases {
        let tmp = TempDir::new();
        create_default_vault(&tmp);
        let path = at_app(&tmp, name);
        let original = std::fs::read(&path).unwrap();
        damage(&path);
        let after_damage = std::fs::read(&path).ok();

        let responses = exchange(
            &tmp,
            &[
                visit("normal", "https://normal.test/", AT),
                visit("auth", AUTH_URL, AT),
                snapshot("snap", "https://normal.test/", AT),
                json!({ "type": "policy_status", "requestId": "s" }),
                json!({ "type": "policy_initialize", "requestId": "i" }),
            ],
        );
        for (response, id) in responses.iter().zip(["normal", "auth", "snap"]) {
            assert_policy_failure(response, id, "policy_invalid");
        }
        assert_eq!(responses[3]["state"], state, "{name}");
        assert!(responses[3].get("policy").is_none());
        // The lone lock file is the one thing a writer may recreate; policy and marker never.
        if name != "policy.lock" {
            assert_eq!(
                responses[4]["ok"], false,
                "{name}: no reset: {}",
                responses[4]
            );
            assert_eq!(
                std::fs::read(&path).ok(),
                after_damage,
                "{name} is left as found"
            );
        }
        assert_eq!(row_dump(&tmp.default_vault()), Vec::<String>::new());

        // Restored by hand: the same event is stored, once.
        std::fs::write(&path, &original).unwrap();
        let retry = visit("normal", "https://normal.test/", AT);
        assert_eq!(
            exchange(&tmp, &[retry]).remove(0),
            stored("normal"),
            "{name}"
        );
        assert_eq!(row_dump(&tmp.default_vault()).len(), 1);
    }
}

#[test]
fn a_held_policy_lock_is_a_failure_not_a_refusal() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(tmp.app_dir().join("policy.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let responses = exchange(
        &tmp,
        &[
            visit("auth", AUTH_URL, AT),
            visit("normal", "https://normal.test/", AT),
            json!({ "type": "policy_status", "requestId": "s" }),
        ],
    );
    assert_policy_failure(&responses[0], "auth", "policy_busy");
    assert_policy_failure(&responses[1], "normal", "policy_busy");
    assert_eq!(responses[2]["state"], "busy");
    assert_eq!(row_dump(&tmp.default_vault()), Vec::<String>::new());
    drop(lock);
    let retry = visit("normal", "https://normal.test/", AT);
    assert_eq!(exchange(&tmp, &[retry]).remove(0), stored("normal"));
}

#[test]
fn the_latest_policy_applies_to_every_event_and_to_a_resend() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let url = "https://becomes-private.test/x";
    assert_eq!(
        exchange(&tmp, &[visit("v1", url, AT)]).remove(0),
        stored("v1")
    );
    add_never_save_rule(&tmp, "becomes-private.test");
    // No restart of anything: each host process reads the policy for each decision.
    assert_eq!(
        exchange(&tmp, &[visit("v2", url, AT)]).remove(0),
        dropped("v2")
    );
    assert_eq!(
        row_dump(&tmp.default_vault()).len(),
        1,
        "rows stored before the change are not rewritten"
    );
}

#[test]
fn the_policy_is_global_across_vault_switches() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    add_never_save_rule(&tmp, "private.test");
    let other = tmp.0.join("other.sqlite3");
    let created = &exchange(
        &tmp,
        &[json!({ "type": "vault_create", "requestId": "c", "path": other })],
    )[0];
    assert_eq!(created["ok"], true, "{created}");
    let opened = exchange(
        &tmp,
        &[
            visit("in-other-1", "https://private.test/", AT),
            visit("in-other-2", "https://normal.test/", AT),
        ],
    );
    assert_eq!(opened, [dropped("in-other-1"), stored("in-other-2")]);
    let rows = row_dump(&other);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("normal.test"));
    assert_eq!(row_dump(&tmp.default_vault()), Vec::<String>::new());
}

#[test]
fn recording_during_policy_updates_never_fakes_success_or_drops_an_allowed_url() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let app = tmp.app_dir();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let store = PolicyStore::new(app).unwrap();
            let mut updates = 0;
            while !stop.load(Ordering::Relaxed) {
                if let Ok(current) = store.load() {
                    let policy = current.policy.clone();
                    if store.update(&current, policy).is_ok() {
                        updates += 1;
                    }
                }
            }
            updates
        })
    };
    let mut delivered = std::collections::BTreeSet::new();
    for round in 0..15 {
        let events: Vec<Value> = (0..5)
            .map(|index| visit(&format!("e{index}"), "https://normal.test/", AT))
            .collect();
        for (response, event) in exchange(&tmp, &events).iter().zip(&events) {
            let id = event["id"].as_str().unwrap();
            if response["ok"] == true {
                assert_eq!(
                    response,
                    &stored(id),
                    "round {round}: an allowed URL is never dropped"
                );
                delivered.insert(id.to_owned());
            } else {
                assert_policy_failure(response, id, "policy_busy");
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    assert!(writer.join().unwrap() > 0, "the updates really ran");
    assert_eq!(
        row_dump(&tmp.default_vault()).len(),
        delivered.len(),
        "acknowledged means stored"
    );
}

#[test]
fn manual_image_saves_do_not_depend_on_the_policy() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    std::fs::remove_file(tmp.app_dir().join("policy.json")).unwrap();
    let bytes = png_of_size(40, 30, 5000);
    let responses = exchange(&tmp, &upload_messages(&bytes, true));
    assert_eq!(responses.last().unwrap()["type"], "image_saved");
    assert_eq!(image_rows(&tmp.default_vault()), (1, 1));
}

// --- Judging before the extension keeps anything (Issue #25) --------------------------------------

fn batch(items: Value) -> Value {
    json!({ "type": "policy_evaluate_batch", "requestId": "b", "items": items })
}

fn batch_error(code: &str) -> Value {
    json!({ "ok": false, "type": "policy_evaluate_batch_result", "requestId": "b", "error": code })
}

fn decisions(reply: &Value) -> Vec<(String, String)> {
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["type"], "policy_evaluate_batch_result");
    assert_eq!(reply["requestId"], "b");
    reply["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            assert_eq!(item.as_object().unwrap().len(), 2, "id and decision only");
            (
                item["id"].as_str().unwrap().to_owned(),
                item["decision"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn a_batch_judges_every_kind_of_url_and_never_echoes_one() {
    let tmp = TempDir::new();
    initialize_policy(&tmp);
    let store = store_of(&tmp);
    let current = store.load().unwrap();
    let mut edited = current.policy.clone();
    edited.layers.never_save.rules.push("blind.example".into());
    edited
        .layers
        .save_but_no_mcp
        .rules
        .push("private.example".into());
    store.update(&current, edited).unwrap();
    let reply = exchange(
        &tmp,
        &[batch(json!([
            { "id": "1", "url": "https://public.example/CANARY" },
            { "id": "a-2", "url": "https://private.example/CANARY" },
            { "id": "3", "url": "https://blind.example/CANARY" },
            { "id": "4", "url": "https://public.example/?token=CANARY" },
            { "id": "5", "url": "https://public.example/%ZZ/CANARY" },
            { "id": "6", "url": "ftp://public.example/CANARY" },
            { "id": "7", "url": "not a url CANARY" },
            { "id": "8", "url": "" },
        ]))],
    )
    .remove(0);
    let expected = [
        ("1", "allow"),
        ("a-2", "save_but_no_mcp"),
        ("3", "never_save"),
        ("4", "never_save"),
        ("5", "never_save"),
        ("6", "never_save"),
        ("7", "never_save"),
        ("8", "never_save"),
    ];
    assert_eq!(
        decisions(&reply),
        expected.map(|(id, decision)| (id.to_owned(), decision.to_owned()))
    );
    assert_eq!(reply["revision"], 2);
    assert_eq!(reply["etag"], store.load().unwrap().etag);
    let text = reply.to_string();
    for private in ["CANARY", "example", "rules", "blind"] {
        assert!(!text.contains(private), "{text}");
    }
}

#[test]
fn a_batch_can_be_empty_one_or_the_maximum_and_asks_for_a_policy_each_time() {
    let tmp = TempDir::new();
    assert_eq!(
        exchange(&tmp, &[batch(json!([]))]).remove(0),
        batch_error("policy_not_initialized"),
        "even an empty batch reports that no decision exists"
    );
    initialize_policy(&tmp);
    assert_eq!(
        decisions(&exchange(&tmp, &[batch(json!([]))]).remove(0)),
        []
    );
    assert_eq!(
        decisions(
            &exchange(
                &tmp,
                &[batch(json!([{ "id": "1", "url": "https://a.test/" }]))]
            )
            .remove(0)
        ),
        [("1".to_owned(), "allow".to_owned())]
    );
    let items = |n: usize| -> Value {
        (0..n)
            .map(|i| json!({ "id": i.to_string(), "url": format!("https://a.test/{i}") }))
            .collect()
    };
    let full = exchange(&tmp, &[batch(items(1000))]).remove(0);
    assert_eq!(decisions(&full).len(), 1000);
    assert!(
        full.to_string().len() < 100_000,
        "far below 1 MB from a host"
    );
    assert_eq!(
        exchange(&tmp, &[batch(items(1001))]).remove(0),
        batch_error("policy_batch_too_large")
    );
}

#[test]
fn a_malformed_batch_is_refused_whole() {
    let tmp = TempDir::new();
    initialize_policy(&tmp);
    let ok = json!({ "id": "1", "url": "https://a.test/CANARY" });
    for items in [
        json!(null),
        json!({}),
        json!("x"),
        json!([ok, ok]),
        json!([ok, 1]),
        json!([ok, { "id": "2" }]),
        json!([ok, { "id": "2", "url": 3 }]),
        json!([ok, { "url": "https://a.test/" }]),
        json!([ok, { "id": 2, "url": "https://a.test/" }]),
        json!([ok, { "id": "", "url": "https://a.test/" }]),
        json!([ok, { "id": "a b", "url": "https://a.test/" }]),
        json!([ok, { "id": "x".repeat(33), "url": "https://a.test/" }]),
    ] {
        assert_eq!(
            exchange(&tmp, &[batch(items.clone())]).remove(0),
            batch_error("invalid_policy_batch"),
            "{items}"
        );
    }
    let missing = json!({ "type": "policy_evaluate_batch", "requestId": "b" });
    assert_eq!(
        exchange(&tmp, &[missing]).remove(0),
        batch_error("invalid_policy_batch")
    );
    let no_request = json!({ "type": "policy_evaluate_batch", "items": [] });
    assert_eq!(
        exchange(&tmp, &[no_request]).remove(0)["error"],
        "invalid_request_id"
    );
}

#[test]
fn no_decision_is_given_without_a_valid_policy() {
    let one = batch(json!([{ "id": "1", "url": "https://a.test/CANARY" }]));
    let tmp = TempDir::new();
    assert_eq!(
        exchange(&tmp, std::slice::from_ref(&one)).remove(0),
        batch_error("policy_not_initialized")
    );
    initialize_policy(&tmp);
    let path = store_of(&tmp).policy_path();
    let good = std::fs::read(&path).unwrap();
    for broken in [
        b"{ broken".to_vec(),
        b"{}".to_vec(),
        String::from_utf8(good.clone())
            .unwrap()
            .replace("\"rules\": []", "\"rules\": [\"https://a.test/*\"]")
            .into_bytes(),
    ] {
        assert_ne!(broken, good);
        std::fs::write(&path, broken).unwrap();
        assert_eq!(
            exchange(&tmp, std::slice::from_ref(&one)).remove(0),
            batch_error("policy_invalid")
        );
    }
    std::fs::write(&path, &good).unwrap();
    // A writer holds the lock: busy after the host's short retries, not a decision.
    let lock = std::fs::File::open(tmp.app_dir().join("policy.lock")).unwrap();
    lock.try_lock().unwrap();
    assert_eq!(
        exchange(&tmp, std::slice::from_ref(&one)).remove(0),
        batch_error("policy_busy")
    );
    drop(lock);
    assert_eq!(decisions(&exchange(&tmp, &[one]).remove(0)).len(), 1);
}

#[test]
fn an_allow_is_no_permit_a_later_rule_stops_the_write() {
    const URL: &str = "https://later.test/CANARY";
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let reply = exchange(&tmp, &[batch(json!([{ "id": "1", "url": URL }]))]).remove(0);
    assert_eq!(decisions(&reply)[0].1, "allow");
    add_never_save_rule(&tmp, "later.test");
    assert_eq!(
        exchange(&tmp, &[visit("v", URL, AT), snapshot("s", URL, AT)]),
        [dropped("v"), dropped("s")]
    );
    assert_eq!(row_dump(&tmp.default_vault()), Vec::<String>::new());
    assert_eq!(
        decisions(&exchange(&tmp, &[batch(json!([{ "id": "1", "url": URL }]))]).remove(0))[0].1,
        "never_save"
    );
}

#[test]
fn canaries_blind_is_nowhere_in_the_vault_private_and_public_are_stored() {
    const BLIND: &str = "https://blind.example/?token=CANARY";
    const NAMED_BLIND: &str = "https://blind.example/CANARY";
    const PRIVATE: &str = "https://private.example/path";
    const PUBLIC: &str = "https://public.example/path";
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    add_never_save_rule(&tmp, "blind.example");
    let store = store_of(&tmp);
    let current = store.load().unwrap();
    let mut edited = current.policy.clone();
    edited
        .layers
        .save_but_no_mcp
        .rules
        .push("private.example".into());
    store.update(&current, edited).unwrap();
    assert_eq!(
        exchange(
            &tmp,
            &[
                visit("b1", BLIND, AT),
                visit("b2", NAMED_BLIND, AT),
                visit("p", PRIVATE, AT),
                visit("u", PUBLIC, AT),
            ]
        ),
        [dropped("b1"), dropped("b2"), stored("p"), stored("u")]
    );
    let bytes = std::fs::read(tmp.default_vault()).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("CANARY") && !text.contains("blind.example"));
    assert!(text.contains(PRIVATE) && text.contains(PUBLIC));
}

#[test]
fn a_batch_compiles_once_and_judges_with_that_one_policy() {
    let source = include_str!("../src/policy_gate.rs");
    let body = source
        .split("fn judge_batch")
        .nth(1)
        .unwrap()
        .split("pub fn handle")
        .next()
        .unwrap();
    // The load compiles the policy once (Slice 6); the batch judges with that compiled form.
    assert_eq!(body.matches("self.load()").count(), 1);
    assert_eq!(body.matches("compile(").count(), 0);
    assert_eq!(body.matches("snapshot.compiled.evaluate").count(), 1);
    assert!(!body.contains("self.decide") && !body.contains("policy.evaluate"));
}

// --- Policy version 2, explicit migration, status without rules (Issue #25 Slice 3) ----------

fn status(tmp: &TempDir) -> Value {
    exchange(tmp, &[json!({ "type": "policy_status", "requestId": "s" })]).remove(0)
}

fn policy_bytes(tmp: &TempDir) -> Vec<u8> {
    std::fs::read(store_of(tmp).policy_path()).unwrap()
}

/// A version 1 policy as an older Memocampus left it, then edited once (revision 2).
fn with_canary_rules(tmp: &TempDir) {
    use memocampus_native_host::policy::{Policy, RuleSpec};
    let store = store_of(tmp);
    let old = Policy {
        schema_version: 1,
        engine_version: 1,
        ..Policy::default()
    };
    std::fs::write(
        store.policy_path(),
        serde_json::to_vec_pretty(&old).unwrap(),
    )
    .unwrap();
    let current = store.load().unwrap();
    let mut edited = current.policy.clone();
    let text = |rule: &str| RuleSpec::Text(rule.into());
    edited.layers.never_save.rules = vec![text("blind-canary.test"), text("*.second-canary.test")];
    edited.layers.save_but_no_mcp.rules = vec![text("private-canary.test/x/*")];
    store.update(&current, edited).unwrap();
}

fn migrate(tmp: &TempDir, etag: Value) -> Value {
    exchange(
        tmp,
        &[json!({ "type": "policy_migrate", "requestId": "m", "etag": etag })],
    )
    .remove(0)
}

const PROBES: [&str; 8] = [
    "https://blind-canary.test/",
    "https://a.second-canary.test/",
    "https://second-canary.test/",
    "https://private-canary.test/x/1",
    "https://private-canary.test/x",
    "https://public.test/??token=x",
    "https://mail.google.com/",
    "https://public.test/",
];

fn judged(tmp: &TempDir) -> Vec<(String, String)> {
    let items: Vec<Value> = PROBES
        .iter()
        .enumerate()
        .map(|(i, url)| json!({ "id": i.to_string(), "url": url }))
        .collect();
    decisions(&exchange(tmp, &[batch(json!(items))]).remove(0))
}

#[test]
fn the_status_names_a_version_and_no_rule_and_get_returns_the_document() {
    let tmp = TempDir::new();
    initialize_policy(&tmp);
    with_canary_rules(&tmp);
    let before = policy_bytes(&tmp);
    let current = store_of(&tmp).load().unwrap();
    let reply = status(&tmp);
    assert_eq!(
        reply,
        json!({ "ok": true, "type": "policy_result", "requestId": "s", "state": "ready", "etag": current.etag,
                "revision": 2, "schemaVersion": 1, "engineVersion": 1 })
    );
    assert!(!reply.to_string().contains("canary"));
    let got = exchange(&tmp, &[json!({ "type": "policy_get", "requestId": "g" })]).remove(0);
    assert_eq!(got["ok"], true);
    assert_eq!(got["type"], "policy_get_result");
    assert_eq!(got["etag"], current.etag);
    assert_eq!(
        got["policy"],
        serde_json::to_value(&current.policy).unwrap()
    );
    assert_eq!(policy_bytes(&tmp), before, "reading changes nothing");
    assert_eq!(
        exchange(&tmp, &[json!({ "type": "policy_get" })]).remove(0)["error"],
        "invalid_request_id"
    );
    let fresh = TempDir::new();
    assert_eq!(
        exchange(&fresh, &[json!({ "type": "policy_get", "requestId": "g" })]).remove(0),
        json!({ "ok": false, "type": "policy_get_result", "requestId": "g", "error": "policy_not_initialized" })
    );
}

#[test]
fn nothing_but_the_explicit_request_migrates_a_version_1_policy() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    with_canary_rules(&tmp);
    let before = policy_bytes(&tmp);
    exchange(
        &tmp,
        &[
            json!({ "type": "policy_status", "requestId": "s" }),
            json!({ "type": "policy_get", "requestId": "g" }),
            json!({ "type": "policy_initialize", "requestId": "i" }),
            batch(json!([{ "id": "1", "url": "https://public.test/" }])),
            visit("v", "https://public.test/", AT),
            json!({ "type": "vault_status", "requestId": "vs" }),
            search("q"),
        ],
    );
    assert_eq!(policy_bytes(&tmp), before);
    assert_eq!(status(&tmp)["schemaVersion"], 1);
}

#[test]
fn an_explicit_migration_keeps_every_decision_and_is_seen_as_a_new_version() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    with_canary_rules(&tmp);
    let decided = judged(&tmp);
    let before = status(&tmp);
    let reply = migrate(&tmp, before["etag"].clone());
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["type"], "policy_migrate_result");
    assert_eq!(
        (
            reply["schemaVersion"].clone(),
            reply["engineVersion"].clone()
        ),
        (json!(2), json!(2))
    );
    assert_eq!(reply["revision"], 3);
    assert_ne!(reply["etag"], before["etag"]);
    assert!(!reply.to_string().contains("canary"));
    let after = status(&tmp);
    assert_eq!(after["etag"], reply["etag"]);
    assert_eq!(judged(&tmp), decided, "the same decision for every URL");
    let migrated = store_of(&tmp).load().unwrap().policy;
    assert_eq!(
        serde_json::to_value(&migrated.layers).unwrap(),
        json!({
            "neverSave": { "enabled": true, "rules": [
                { "kind": "pattern", "pattern": "blind-canary.test" },
                { "kind": "pattern", "pattern": "*.second-canary.test" } ] },
            "saveButNoMcp": { "enabled": true, "rules": [
                { "kind": "pattern", "pattern": "private-canary.test/x/*" } ] },
        })
    );
    // The write guard uses the migrated policy too.
    assert_eq!(
        exchange(
            &tmp,
            &[
                visit("b", "https://blind-canary.test/", AT),
                visit("p", "https://public.test/", AT)
            ]
        ),
        [dropped("b"), stored("p")]
    );
    // Again: refused, and nothing changes.
    let bytes = policy_bytes(&tmp);
    assert_eq!(
        migrate(&tmp, after["etag"].clone()),
        json!({ "ok": false, "type": "policy_migrate_result", "requestId": "m", "error": "policy_not_migratable" })
    );
    assert_eq!(policy_bytes(&tmp), bytes);
}

#[test]
fn a_migration_against_another_version_or_without_one_changes_nothing() {
    let tmp = TempDir::new();
    let fresh = migrate(&tmp, json!("x"));
    assert_eq!(fresh["error"], "policy_not_initialized");
    initialize_policy(&tmp);
    let stale = status(&tmp)["etag"].clone();
    with_canary_rules(&tmp);
    let before = (policy_bytes(&tmp), status(&tmp));
    for (etag, code) in [
        (stale, "policy_conflict"),
        (json!("0".repeat(64)), "policy_conflict"),
        (json!(""), "invalid_etag"),
        (json!(1), "invalid_etag"),
        (Value::Null, "invalid_etag"),
    ] {
        assert_eq!(
            migrate(&tmp, etag.clone()),
            json!({ "ok": false, "type": "policy_migrate_result", "requestId": "m", "error": code }),
            "{etag}"
        );
        assert_eq!((policy_bytes(&tmp), status(&tmp)), before);
    }
    assert_eq!(
        exchange(
            &tmp,
            &[json!({ "type": "policy_migrate", "etag": before.1["etag"] })]
        )
        .remove(0)["error"],
        "invalid_request_id"
    );
    // Held by a writer: refused once, not retried in the background.
    let lock = std::fs::File::open(tmp.app_dir().join("policy.lock")).unwrap();
    lock.try_lock().unwrap();
    assert_eq!(
        migrate(&tmp, before.1["etag"].clone())["error"],
        "policy_busy"
    );
    drop(lock);
    assert_eq!((policy_bytes(&tmp), status(&tmp)), before);
}

#[test]
fn a_policy_with_an_unsupported_regular_expression_is_refused_everywhere_and_left_alone() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let path = store_of(&tmp).policy_path();
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["schemaVersion"] = 2.into();
    value["engineVersion"] = 2.into();
    // Look-behind: a regex this engine cannot evaluate (Slice 5 evaluates the others).
    value["layers"]["neverSave"]["rules"] =
        json!([{ "kind": "regex", "pattern": "(?<=^https://)canary\\.test/" }]);
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        status(&tmp),
        json!({ "ok": true, "type": "policy_result", "requestId": "s", "state": "unsupported" })
    );
    assert_eq!(
        exchange(
            &tmp,
            &[batch(json!([{ "id": "1", "url": "https://public.test/" }]))]
        )
        .remove(0),
        batch_error("policy_unsupported")
    );
    assert_policy_failure(
        &exchange(&tmp, &[visit("v", "https://public.test/", AT)])[0],
        "v",
        "policy_unsupported",
    );
    assert_eq!(migrate(&tmp, json!("x"))["error"], "policy_unsupported");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(row_dump(&tmp.default_vault()), Vec::<String>::new());
    // An unknown kind is no policy at all.
    value["layers"]["neverSave"]["rules"] = json!([{ "kind": "glob", "pattern": "canary.test" }]);
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(status(&tmp)["state"], "invalid");
}

#[test]
fn a_new_install_writes_version_2_and_an_existing_version_1_is_kept() {
    let tmp = TempDir::new();
    initialize_policy(&tmp);
    let stored: Value = serde_json::from_slice(&policy_bytes(&tmp)).unwrap();
    assert_eq!(
        (
            stored["schemaVersion"].clone(),
            stored["engineVersion"].clone()
        ),
        (json!(2), json!(2))
    );
    let other = TempDir::new();
    initialize_policy(&other);
    with_canary_rules(&other);
    let before = policy_bytes(&other);
    assert_eq!(
        exchange(
            &other,
            &[json!({ "type": "policy_initialize", "requestId": "i" })]
        )
        .remove(0)["error"],
        "policy_already_initialized"
    );
    assert_eq!(policy_bytes(&other), before);
    assert_eq!(status(&other)["schemaVersion"], 1);
}

/// A version 2 policy whose compact form is `target` bytes long, all of it pattern rules.
fn policy_of_compact_length(target: usize) -> memocampus_native_host::policy::Policy {
    use memocampus_native_host::policy::Policy;
    let rule = |n: usize, len: usize| format!("a.test/{n:04}{}/*", "x".repeat(len));
    let mut policy = Policy::default();
    let length = |policy: &Policy| serde_json::to_string(policy).unwrap().len();
    // Enough full rules to reach the target, then the last one shortened to hit it exactly.
    let base = length(&policy);
    let mut probe = policy.clone();
    probe.layers.never_save.rules = vec![rule(0, 1990).into(), rule(1, 1990).into()];
    let two = length(&probe);
    probe.layers.never_save.rules.pop();
    let per_rule = two - length(&probe);
    let count = (target - base) / per_rule + 2;
    for n in 0..count {
        let layer = if n < 1000 {
            &mut policy.layers.never_save
        } else {
            &mut policy.layers.save_but_no_mcp
        };
        layer.rules.push(rule(n, 1990).into());
    }
    // Shorten rules from the end, each by at most its whole padding, until the length is exact.
    let mut over = length(&policy) - target;
    let mut n = count;
    while over > 0 {
        n -= 1;
        let cut = over.min(1990);
        let layer = if n < 1000 {
            &mut policy.layers.never_save
        } else {
            &mut policy.layers.save_but_no_mcp
        };
        layer.rules[n % 1000] = rule(n, 1990 - cut).into();
        over -= cut;
    }
    assert_eq!(length(&policy), target);
    policy
}

#[test]
fn a_document_too_large_for_chrome_is_refused_without_its_content() {
    let tmp = TempDir::new();
    initialize_policy(&tmp);
    let store = store_of(&tmp);
    for (compact, fits) in [
        (990_000, true),
        (999_000, true),
        (1_000_100, false),
        (1_010_000, false),
    ] {
        let current = store.load().unwrap();
        let mut candidate = policy_of_compact_length(compact);
        candidate.revision = current.policy.revision;
        store.update(&current, candidate).unwrap();
        let before = policy_bytes(&tmp);
        assert!(
            before.len() <= 1_048_576,
            "the file itself is within its own limit"
        );
        let replies = exchange(
            &tmp,
            &[
                json!({ "type": "policy_get", "requestId": "g" }),
                json!({ "type": "policy_status", "requestId": "s" }),
            ],
        );
        let text = replies[0].to_string();
        if fits {
            assert_eq!(replies[0]["ok"], true, "{compact}");
            assert!(text.len() <= 1_000_000, "{compact}: {}", text.len());
        } else {
            assert_eq!(
                replies[0],
                json!({ "ok": false, "type": "policy_get_result", "requestId": "g", "error": "policy_too_large" }),
                "{compact}"
            );
            assert!(!text.contains("a.test"));
        }
        assert_eq!(replies[1]["state"], "ready", "the host keeps serving");
        assert_eq!(policy_bytes(&tmp), before, "and changes nothing");
    }
}

#[test]
fn the_new_patterns_and_collapsed_slashes_hold_in_the_batch_and_again_before_the_vault() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let store = store_of(&tmp);
    let current = store.load().unwrap();
    assert_eq!(current.policy.schema_version, 2);
    let mut edited = current.policy.clone();
    edited.layers.never_save.rules = vec![
        "blind.example/secret".into(),
        "blind.example/img/*.png".into(),
        "blind.example/dir/*".into(),
    ];
    edited.layers.save_but_no_mcp.rules = vec!["private.example/file-*.jpg".into()];
    store.update(&current, edited).unwrap();
    let urls = [
        ("1", "https://blind.example/secret"),
        ("2", "https://blind.example//secret"),
        ("3", "https://blind.example/img/CANARY.png"),
        ("4", "https://blind.example///img//CANARY.png"),
        ("5", "https://blind.example//dir/CANARY"),
        ("6", "https://blind.example/secret/CANARY"),
        ("7", "https://blind.example/img/sub/CANARY.png"),
        ("8", "https://private.example/file-CANARY.jpg"),
        ("9", "https://private.example//file-.jpg"),
        ("10", "https://private.example/file-CANARY/x.jpg"),
    ];
    let expected = [
        "never_save",
        "never_save",
        "never_save",
        "never_save",
        "never_save",
        "allow",
        "allow",
        "save_but_no_mcp",
        "save_but_no_mcp",
        "allow",
    ];
    let items: Vec<Value> = urls
        .iter()
        .map(|(id, url)| json!({ "id": id, "url": url }))
        .collect();
    let reply = exchange(&tmp, &[batch(json!(items))]).remove(0);
    assert_eq!(
        decisions(&reply),
        urls.iter()
            .zip(expected)
            .map(|((id, _), decision)| (id.to_string(), decision.to_owned()))
            .collect::<Vec<_>>()
    );
    assert!(!reply.to_string().contains("CANARY"));
    // A precheck is no permission: the host judges each event again before the vault.
    let events: Vec<Value> = urls.iter().map(|(id, url)| visit(id, url, AT)).collect();
    let acks = exchange(&tmp, &events);
    for ((id, _), (ack, decision)) in urls.iter().zip(acks.iter().zip(expected)) {
        let want = if decision == "never_save" {
            dropped(id)
        } else {
            stored(id)
        };
        assert_eq!(ack, &want, "{id}");
    }
    let rows = row_dump(&tmp.default_vault()).join("\n");
    for (_, url) in &urls[..5] {
        assert!(!rows.contains(&format!("|{url}|")), "{url}");
    }
    assert_eq!(rows.lines().count(), 5, "{rows}");
}

#[test]
fn a_version_1_policy_also_refuses_a_path_hidden_behind_doubled_slashes() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    with_canary_rules(&tmp);
    let reply = exchange(
        &tmp,
        &[batch(json!([
            { "id": "1", "url": "https://private-canary.test/x/1" },
            { "id": "2", "url": "https://private-canary.test//x/1" },
            { "id": "3", "url": "https://private-canary.test//x" },
            { "id": "4", "url": "https://public.test//login" },
        ]))],
    )
    .remove(0);
    assert_eq!(
        decisions(&reply),
        [
            ("1", "save_but_no_mcp"),
            ("2", "save_but_no_mcp"),
            ("3", "allow"),
            ("4", "never_save"),
        ]
        .map(|(id, decision)| (id.to_owned(), decision.to_owned()))
    );
    let v1 = std::fs::read(store_of(&tmp).policy_path()).unwrap();
    assert!(String::from_utf8_lossy(&v1).contains("\"schemaVersion\": 1"));
}

fn regex_rule(pattern: &str) -> memocampus_native_host::policy::RuleSpec {
    use memocampus_native_host::policy::{RuleSpec, TypedRule};
    RuleSpec::Typed(TypedRule::Regex {
        pattern: pattern.into(),
    })
}

#[test]
fn regex_rules_decide_in_the_batch_and_again_before_the_vault() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let store = store_of(&tmp);
    let current = store.load().unwrap();
    let mut edited = current.policy.clone();
    edited.layers.never_save.rules = vec![regex_rule("[?&]q=CANARY-blind(?:&|$)")];
    edited.layers.save_but_no_mcp.rules = vec![
        regex_rule(r"^https://private\.example/"),
        "blind-too.example".into(),
    ];
    store.update(&current, edited).unwrap();
    let urls = [
        ("1", "https://public.example/?q=CANARY-blind"),
        ("2", "https://private.example/?q=CANARY-blind"),
        ("3", "https://private.example/notes"),
        ("4", "https://blind-too.example/?a=1&q=CANARY-blind"),
        ("5", "https://blind-too.example/x"),
        ("6", "https://public.example/?q=CANARY-blinder"),
        ("7", "https://PRIVATE.example:443/x"),
    ];
    let expected = [
        "never_save",
        "never_save",
        "save_but_no_mcp",
        "never_save",
        "save_but_no_mcp",
        "allow",
        "save_but_no_mcp",
    ];
    let items: Vec<Value> = urls
        .iter()
        .map(|(id, url)| json!({ "id": id, "url": url }))
        .collect();
    let reply = exchange(&tmp, &[batch(json!(items))]).remove(0);
    assert_eq!(
        decisions(&reply),
        urls.iter()
            .zip(expected)
            .map(|((id, _), decision)| (id.to_string(), decision.to_owned()))
            .collect::<Vec<_>>()
    );
    assert_eq!(reply["revision"], 2);
    assert!(!reply.to_string().contains("CANARY"));
    let events: Vec<Value> = urls.iter().map(|(id, url)| visit(id, url, AT)).collect();
    let acks = exchange(&tmp, &events);
    for ((id, _), (ack, decision)) in urls.iter().zip(acks.iter().zip(expected)) {
        let want = if decision == "never_save" {
            dropped(id)
        } else {
            stored(id)
        };
        assert_eq!(ack, &want, "{id}");
    }
    let rows = row_dump(&tmp.default_vault()).join("\n");
    assert!(!rows.contains("CANARY-blind|"), "{rows}");
    assert!(!rows.contains("CANARY-blind&"), "{rows}");
    assert_eq!(rows.lines().count(), 4, "{rows}");

    // A precheck is no permission: a regex added after it refuses the event before the vault.
    let late = "https://public.example/late/CANARY-late";
    let precheck = exchange(&tmp, &[batch(json!([{ "id": "l", "url": late }]))]).remove(0);
    assert_eq!(decisions(&precheck), [("l".into(), "allow".into())]);
    let current = store.load().unwrap();
    let mut edited = current.policy.clone();
    edited.layers.never_save.rules.push(regex_rule("/late/"));
    store.update(&current, edited).unwrap();
    assert_eq!(exchange(&tmp, &[visit("l", late, AT)]), [dropped("l")]);
    assert!(!row_dump(&tmp.default_vault())
        .join("\n")
        .contains("CANARY-late"));
    let after = exchange(&tmp, &[batch(json!([{ "id": "l", "url": late }]))]).remove(0);
    assert_eq!(decisions(&after), [("l".into(), "never_save".into())]);
    assert_eq!(after["revision"], 3);
    assert_ne!(after["etag"], precheck["etag"]);
}

#[test]
fn an_unusable_regex_stops_every_judgement_and_is_never_echoed() {
    let many: Vec<Value> = (0..129)
        .map(|n| json!({ "kind": "regex", "pattern": format!("CANARY{n}") }))
        .collect();
    for (rules, code) in [
        (
            json!([{ "kind": "regex", "pattern": "(?<=CANARY)x" }]),
            "policy_unsupported",
        ),
        (
            json!([{ "kind": "regex", "pattern": "(CANARY" }]),
            "policy_invalid",
        ),
        (
            json!([{ "kind": "regex", "pattern": "\\w{100}CANARY" }]),
            "policy_invalid",
        ),
        (
            json!([{ "kind": "regex", "pattern": "CANARY".repeat(700) }]),
            "policy_invalid",
        ),
        (json!(many), "policy_invalid"),
    ] {
        for enabled in [true, false] {
            let tmp = TempDir::new();
            create_default_vault(&tmp);
            let store = store_of(&tmp);
            let good = std::fs::read(store.policy_path()).unwrap();
            let mut value: Value = serde_json::from_slice(&good).unwrap();
            value["layers"]["neverSave"]["rules"] = rules.clone();
            value["layers"]["neverSave"]["enabled"] = enabled.into();
            // The store refuses to write it, and leaves the file as it was.
            let current = store.load().unwrap();
            let candidate: memocampus_native_host::policy::Policy =
                serde_json::from_value(value.clone()).unwrap();
            assert!(store.update(&current, candidate).is_err());
            assert_eq!(std::fs::read(store.policy_path()).unwrap(), good);
            // Written by hand, it stops recording: no decision, no drop, nothing stored.
            std::fs::write(store.policy_path(), serde_json::to_vec(&value).unwrap()).unwrap();
            let replies = exchange(
                &tmp,
                &[
                    batch(json!([{ "id": "1", "url": "https://public.test/" }])),
                    visit("v", "https://public.test/CANARY-visit", AT),
                    json!({ "type": "policy_status", "requestId": "s" }),
                ],
            );
            assert_eq!(replies[0], batch_error(code), "{code}");
            assert_policy_failure(&replies[1], "v", code);
            let state = code.trim_start_matches("policy_");
            assert_eq!(
                replies[2],
                json!({ "ok": true, "type": "policy_result", "requestId": "s", "state": state })
            );
            for reply in &replies {
                assert!(!reply.to_string().contains("CANARY"), "{reply}");
            }
            assert!(row_dump(&tmp.default_vault()).is_empty());
            // A valid policy again, in a new host process, records again.
            std::fs::write(store.policy_path(), &good).unwrap();
            assert_eq!(
                exchange(&tmp, &[visit("w", "https://public.test/", AT)]),
                [stored("w")]
            );
        }
    }
}

// --- Policy management API (Issue #25 Slice 6) -----------------------------------------------

fn call(tmp: &TempDir, message: Value) -> Value {
    exchange(tmp, &[message]).remove(0)
}

/// The active policy as `policy_get` returns it, and its ETag.
fn active(tmp: &TempDir) -> (Value, String) {
    let reply = call(tmp, json!({ "type": "policy_get", "requestId": "g" }));
    assert_eq!(reply["ok"], true, "{reply}");
    (
        reply["policy"].clone(),
        reply["etag"].as_str().unwrap().to_owned(),
    )
}

fn regex_json(pattern: &str) -> Value {
    json!({ "kind": "regex", "pattern": pattern })
}

fn pattern_json(pattern: &str) -> Value {
    json!({ "kind": "pattern", "pattern": pattern })
}

fn with_never(mut policy: Value, rules: Value) -> Value {
    policy["layers"]["neverSave"]["rules"] = rules;
    policy
}

fn refusal(result: &str, id: &str, code: &str) -> Value {
    json!({ "ok": false, "type": result, "requestId": id, "error": code })
}

#[test]
fn validate_judges_the_whole_draft_rule_by_rule_and_changes_nothing() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let (policy, _) = active(&tmp);
    let before = policy_bytes(&tmp);
    let validate = |draft: Value| {
        call(
            &tmp,
            json!({ "type": "policy_validate", "requestId": "v", "policy": draft }),
        )
    };
    assert_eq!(
        validate(policy.clone()),
        json!({ "ok": true, "type": "policy_validate_result", "requestId": "v", "schemaVersion": 2, "engineVersion": 2 })
    );
    let many: Vec<Value> = (0..129)
        .map(|n| regex_json(&format!("CANARY{n}")))
        .collect();
    let mut schema_3 = policy.clone();
    schema_3["schemaVersion"] = 3.into();
    let mut revision_0 = policy.clone();
    revision_0["revision"] = 0.into();
    let mut disabled = with_never(
        policy.clone(),
        json!([pattern_json("ok.test"), pattern_json("CANARY.test/*/x")]),
    );
    disabled["layers"]["neverSave"]["enabled"] = false.into();
    let mut private = policy.clone();
    private["layers"]["saveButNoMcp"]["rules"] =
        json!([pattern_json("ok.test"), regex_json("(CANARY")]);
    let mut unknown_field = policy.clone();
    unknown_field["CANARY"] = true.into();
    let at = |layer: &str, index: usize, code: &str| json!({ "layer": layer, "ruleIndex": index, "code": code });
    let only = |code: &str| json!({ "code": code });
    for (draft, error, errors) in [
        (schema_3, "policy_invalid", json!([only("invalid_version")])),
        (
            revision_0,
            "policy_invalid",
            json!([only("invalid_revision")]),
        ),
        (
            disabled,
            "policy_invalid",
            json!([at("neverSave", 1, "invalid_pattern")]),
        ),
        (
            private,
            "policy_invalid",
            json!([at("saveButNoMcp", 1, "invalid_regex")]),
        ),
        (
            with_never(policy.clone(), json!([regex_json("(?<=CANARY)x")])),
            "policy_unsupported",
            json!([at("neverSave", 0, "unsupported_regex")]),
        ),
        (
            with_never(
                policy.clone(),
                json!([regex_json("(?<=CANARY)x"), regex_json("(CANARY")]),
            ),
            "policy_invalid",
            json!([
                at("neverSave", 0, "unsupported_regex"),
                at("neverSave", 1, "invalid_regex")
            ]),
        ),
        (
            with_never(policy.clone(), json!([regex_json("\\w{100}CANARY")])),
            "policy_invalid",
            json!([at("neverSave", 0, "regex_limit")]),
        ),
        (
            with_never(policy.clone(), json!(many)),
            "policy_invalid",
            json!([only("too_many_regex_rules")]),
        ),
        (
            with_never(policy.clone(), json!(["CANARY.test"])),
            "policy_invalid",
            json!([at("neverSave", 0, "invalid_rule_form")]),
        ),
        (
            with_never(
                policy.clone(),
                json!([pattern_json("ok.test"), { "kind": "glob", "pattern": "CANARY" }]),
            ),
            "policy_invalid",
            json!([at("neverSave", 1, "invalid_rule_form")]),
        ),
        (
            unknown_field,
            "policy_invalid",
            json!([only("invalid_policy_document")]),
        ),
    ] {
        let reply = validate(draft);
        assert_eq!(
            reply,
            json!({ "ok": false, "type": "policy_validate_result", "requestId": "v", "error": error, "errors": errors })
        );
        assert!(!reply.to_string().contains("CANARY"), "{reply}");
    }
    for missing in [
        json!({ "type": "policy_validate", "requestId": "v" }),
        json!({ "type": "policy_validate", "requestId": "v", "policy": "CANARY" }),
        json!({ "type": "policy_validate", "requestId": "v", "policy": [] }),
    ] {
        assert_eq!(
            call(&tmp, missing),
            refusal("policy_validate_result", "v", "invalid_policy_draft")
        );
    }
    let mut huge = policy.clone();
    huge["layers"]["neverSave"]["rules"] =
        json!(vec![
            pattern_json(&format!("a.test/{}/*", "x".repeat(2000)));
            600
        ]);
    assert_eq!(
        validate(huge),
        refusal("policy_validate_result", "v", "policy_draft_too_large")
    );
    assert_eq!(policy_bytes(&tmp), before, "validation writes nothing");
}

#[test]
fn a_rule_is_tested_alone_by_the_production_matcher() {
    let tmp = TempDir::new();
    let test = |rule: Value, url: &str| {
        call(
            &tmp,
            json!({ "type": "policy_test_rule", "requestId": "r", "rule": rule, "url": url }),
        )
    };
    let matched = |rule: Value, url: &str| {
        let reply = test(rule, url);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply.as_object().unwrap().len(), 4, "{reply}");
        reply["matched"].as_bool().unwrap()
    };
    for (rule, url, expected) in [
        (
            pattern_json("example.com/private"),
            "https://example.com/private",
            true,
        ),
        (
            pattern_json("example.com/private"),
            "https://example.com/private/",
            false,
        ),
        (
            pattern_json("example.com/images/*.png"),
            "https://example.com/images/a.png",
            true,
        ),
        (
            pattern_json("example.com/images/*.png"),
            "https://example.com/images/b/a.png",
            false,
        ),
        (pattern_json("*.example.com"), "https://example.com/", true),
        (
            pattern_json("*.example.com"),
            "https://a.b.example.com/",
            true,
        ),
        (
            pattern_json("*.example.com"),
            "https://example.com.evil.test/",
            false,
        ),
        (
            pattern_json("example.com/private/*"),
            "https://example.com//private/a",
            true,
        ),
        (
            pattern_json("example.com/private/*"),
            "https://example.com/private%2Fa",
            true,
        ),
        (
            regex_json("[?&]q=secret(?:&|$)"),
            "https://x.test/?a=1&q=secret",
            true,
        ),
        (regex_json("a%2Fb"), "https://x.test/a%2Fb", true),
        (regex_json("a/b"), "https://x.test/a%2Fb", false),
        (
            regex_json(r"^https://example\.com/"),
            "https://example.com./x",
            true,
        ),
        (
            regex_json(r"^https://example\.com/"),
            "blob:https://example.com/x",
            true,
        ),
        (
            regex_json("^HTTPS://EXAMPLE"),
            "HTTPS://EXAMPLE.COM/",
            false,
        ),
        // No decision: a credential the builtins would refuse does not match an unrelated rule.
        (
            pattern_json("other.test"),
            "https://example.com/?token=x",
            false,
        ),
    ] {
        assert_eq!(matched(rule.clone(), url), expected, "{rule} {url}");
    }
    for (rule, url, code) in [
        (
            pattern_json("CANARY.test/*/x"),
            "https://a.test/",
            "invalid_pattern",
        ),
        (regex_json("(CANARY"), "https://a.test/", "invalid_regex"),
        (
            regex_json("(?<=CANARY)x"),
            "https://a.test/",
            "unsupported_regex",
        ),
        (
            regex_json("\\w{21}CANARY"),
            "https://a.test/",
            "regex_limit",
        ),
        (json!("CANARY.test"), "https://a.test/", "invalid_rule"),
        (
            json!({ "kind": "glob", "pattern": "CANARY" }),
            "https://a.test/",
            "invalid_rule",
        ),
        (
            json!({ "kind": "pattern", "pattern": "a.test", "x": 1 }),
            "https://a.test/",
            "invalid_rule",
        ),
        (pattern_json("a.test"), "not a url CANARY", "invalid_url"),
        (pattern_json("a.test"), "ftp://a.test/CANARY", "invalid_url"),
        (
            pattern_json("a.test"),
            "https://a.test/%FFCANARY",
            "invalid_url",
        ),
    ] {
        let reply = test(rule, url);
        assert_eq!(reply, refusal("policy_test_rule_result", "r", code));
    }
    let no_url = call(
        &tmp,
        json!({ "type": "policy_test_rule", "requestId": "r", "rule": pattern_json("a.test") }),
    );
    assert_eq!(
        no_url,
        refusal("policy_test_rule_result", "r", "invalid_url")
    );
    assert!(
        store_of(&tmp).never_initialized(),
        "testing a rule needs and writes no policy"
    );
}

#[test]
fn a_draft_decides_a_url_and_permits_nothing() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    // The active policy refuses blind.example; the draft, made from it, does not.
    add_never_save_rule(&tmp, "blind.example");
    let (policy, _) = active(&tmp);
    let before = policy_bytes(&tmp);
    let mut draft = with_never(policy.clone(), json!([regex_json("CANARY-blind")]));
    draft["layers"]["saveButNoMcp"]["rules"] = json!([
        pattern_json("private.example"),
        regex_json("^https://mixed\\.example/")
    ]);
    let decide = |draft: &Value, url: &str| {
        call(
            &tmp,
            json!({ "type": "policy_test_url", "requestId": "u", "policy": draft, "url": url }),
        )
    };
    for (url, decision) in [
        ("https://public.example/", "allow"),
        ("https://blind.example/", "allow"),
        ("https://private.example/x", "save_but_no_mcp"),
        ("https://mixed.example/x", "save_but_no_mcp"),
        ("https://public.example/CANARY-blind", "never_save"),
        ("https://private.example/CANARY-blind", "never_save"),
        ("https://accounts.google.com/signin", "never_save"),
        ("https://public.example/?token=CANARY", "never_save"),
        ("not a url CANARY", "never_save"),
    ] {
        let reply = decide(&draft, url);
        assert_eq!(
            reply,
            json!({ "ok": true, "type": "policy_test_url_result", "requestId": "u", "decision": decision }),
            "{url}"
        );
    }
    let invalid = with_never(policy.clone(), json!([regex_json("(CANARY")]));
    let reply = decide(&invalid, "https://public.example/");
    assert_eq!(reply["error"], "policy_invalid");
    assert_eq!(
        reply["errors"],
        json!([{ "layer": "neverSave", "ruleIndex": 0, "code": "invalid_regex" }])
    );
    assert!(!reply.to_string().contains("CANARY"));
    assert_eq!(
        call(
            &tmp,
            json!({ "type": "policy_test_url", "requestId": "u", "policy": draft })
        ),
        refusal("policy_test_url_result", "u", "invalid_url")
    );
    // An allow of the draft is no permit: the stored policy still refuses the URL.
    assert_eq!(
        exchange(&tmp, &[visit("b", "https://blind.example/", AT)]),
        [dropped("b")]
    );
    assert_eq!(policy_bytes(&tmp), before, "testing a draft writes nothing");
    assert!(row_dump(&tmp.default_vault()).is_empty());
}

#[test]
fn a_preset_is_a_draft_that_neither_starts_nor_changes_anything() {
    let tmp = TempDir::new();
    let preset = call(
        &tmp,
        json!({ "type": "policy_preset", "requestId": "p", "presetId": "default" }),
    );
    let bundled: Value =
        serde_json::from_str(memocampus_native_host::policy::DEFAULT_JSON).unwrap();
    assert_eq!(
        preset,
        json!({ "ok": true, "type": "policy_preset_result", "requestId": "p", "presetId": "default",
                "schemaVersion": 2, "engineVersion": 2, "policy": bundled })
    );
    for id in [
        json!("privacy-focused"),
        json!("DEFAULT"),
        json!(1),
        Value::Null,
    ] {
        assert_eq!(
            call(
                &tmp,
                json!({ "type": "policy_preset", "requestId": "p", "presetId": id })
            ),
            refusal("policy_preset_result", "p", "unknown_preset")
        );
    }
    assert!(store_of(&tmp).never_initialized(), "no policy was created");
    assert_eq!(status(&tmp)["state"], "uninitialized");
    // After setup, a preset is never put back into the stored policy.
    create_default_vault(&tmp);
    add_never_save_rule(&tmp, "kept.example");
    let before = policy_bytes(&tmp);
    call(
        &tmp,
        json!({ "type": "policy_preset", "requestId": "p", "presetId": "default" }),
    );
    assert_eq!(policy_bytes(&tmp), before);
}

#[test]
fn apply_replaces_the_policy_of_its_etag_all_or_nothing() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let (policy, etag) = active(&tmp);
    let before = policy_bytes(&tmp);
    let apply = |draft: &Value, etag: Option<&str>| {
        let mut message = json!({ "type": "policy_apply", "requestId": "a", "policy": draft });
        if let Some(etag) = etag {
            message["expectedEtag"] = etag.into();
        }
        call(&tmp, message)
    };
    let edited = with_never(
        policy.clone(),
        json!([
            pattern_json("c.example"),
            regex_json("CANARY-applied"),
            pattern_json("a.example")
        ]),
    );
    assert_eq!(
        apply(&edited, None),
        refusal("policy_apply_result", "a", "invalid_etag")
    );
    assert_eq!(
        apply(&edited, Some("")),
        refusal("policy_apply_result", "a", "invalid_etag")
    );
    assert_eq!(
        apply(&edited, Some("stale")),
        refusal("policy_apply_result", "a", "policy_conflict")
    );
    let mut v1 = edited.clone();
    (v1["schemaVersion"], v1["engineVersion"]) = (1.into(), 1.into());
    v1["layers"]["neverSave"]["rules"] = json!(["c.example"]);
    assert_eq!(
        apply(&v1, Some(&etag)),
        refusal("policy_apply_result", "a", "policy_version_mismatch")
    );
    let mut ahead = edited.clone();
    ahead["revision"] = 5.into();
    assert_eq!(
        apply(&ahead, Some(&etag)),
        refusal("policy_apply_result", "a", "policy_conflict")
    );
    for bad in [
        regex_json("(CANARY"),
        regex_json("\\w{100}CANARY"),
        regex_json("(?<=CANARY)x"),
        pattern_json("CANARY/*/x"),
    ] {
        let draft = with_never(policy.clone(), json!([pattern_json("ok.example"), bad]));
        let reply = apply(&draft, Some(&etag));
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["errors"][0]["ruleIndex"], 1, "{reply}");
        assert!(!reply.to_string().contains("CANARY"));
    }
    assert_eq!(
        policy_bytes(&tmp),
        before,
        "every refusal left the file as it was"
    );

    let applied = apply(&edited, Some(&etag));
    assert_eq!(applied["ok"], true, "{applied}");
    assert_eq!(applied["revision"], 2);
    let (written, new_etag) = active(&tmp);
    assert_eq!(applied["etag"], new_etag.as_str());
    assert_ne!(new_etag, etag);
    let mut expected = edited.clone();
    expected["revision"] = 2.into();
    assert_eq!(
        written, expected,
        "every rule, in order and layer, and nothing else"
    );
    // The same ETag again is stale now.
    assert_eq!(
        apply(&edited, Some(&etag)),
        refusal("policy_apply_result", "a", "policy_conflict")
    );
    // The extension sees a new ETag and revision, and the store judges with the new policy.
    let reply = call(
        &tmp,
        batch(json!([{ "id": "1", "url": "https://x.test/CANARY-applied" }])),
    );
    assert_eq!(decisions(&reply), [("1".into(), "never_save".into())]);
    assert_eq!(
        (reply["etag"].as_str(), reply["revision"].as_u64()),
        (Some(new_etag.as_str()), Some(2))
    );
    assert_eq!(
        exchange(&tmp, &[visit("v", "https://x.test/CANARY-applied", AT)]),
        [dropped("v")]
    );
    // An explicit change may also allow what was refused; it is the user's own choice.
    let mut relaxed = policy.clone();
    relaxed["revision"] = 2.into();
    assert_eq!(apply(&relaxed, Some(&new_etag))["revision"], 3);
    assert_eq!(
        exchange(&tmp, &[visit("w", "https://x.test/CANARY-applied", AT)]),
        [stored("w")]
    );
    // Nothing comes back of what was refused before.
    let rows = row_dump(&tmp.default_vault());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].starts_with("v|w|"));
}

#[test]
fn apply_needs_a_policy_and_never_migrates() {
    let tmp = TempDir::new();
    let (policy, _) = (
        serde_json::from_str::<Value>(memocampus_native_host::policy::DEFAULT_JSON).unwrap(),
        (),
    );
    let message =
        json!({ "type": "policy_apply", "requestId": "a", "expectedEtag": "e", "policy": policy });
    assert_eq!(
        call(&tmp, message.clone()),
        refusal("policy_apply_result", "a", "policy_not_initialized")
    );
    assert!(store_of(&tmp).never_initialized());
    // A version 1 policy stays version 1: a version 2 draft is refused, a version 1 one applied.
    create_default_vault(&tmp);
    with_canary_rules(&tmp);
    let (v1, etag) = active(&tmp);
    assert_eq!(v1["schemaVersion"], 1);
    let mut as_v2 = policy.clone();
    as_v2["revision"] = v1["revision"].clone();
    let mut message =
        json!({ "type": "policy_apply", "requestId": "a", "expectedEtag": etag, "policy": as_v2 });
    assert_eq!(
        call(&tmp, message.clone()),
        refusal("policy_apply_result", "a", "policy_version_mismatch")
    );
    let mut edited = v1.clone();
    edited["layers"]["neverSave"]["rules"] = json!(["only.example"]);
    message["policy"] = edited;
    let reply = call(&tmp, message);
    assert_eq!(
        (reply["ok"].as_bool(), reply["schemaVersion"].as_u64()),
        (Some(true), Some(1)),
        "{reply}"
    );
}

#[test]
fn concurrent_applies_of_one_etag_store_exactly_one() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let (policy, etag) = active(&tmp);
    let replies: Vec<Value> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|n| {
                let draft = with_never(policy.clone(), json!([pattern_json(&format!("writer{n}.example"))]));
                let message = json!({ "type": "policy_apply", "requestId": "a", "expectedEtag": etag, "policy": draft });
                let tmp = &tmp;
                scope.spawn(move || call(tmp, message))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let winners: Vec<&Value> = replies.iter().filter(|r| r["ok"] == true).collect();
    assert_eq!(winners.len(), 1, "{replies:?}");
    for loser in replies.iter().filter(|r| r["ok"] != true) {
        assert!(
            ["policy_conflict", "policy_busy"].contains(&loser["error"].as_str().unwrap()),
            "{loser}"
        );
    }
    let (written, stored_etag) = active(&tmp);
    assert_eq!(written["revision"], 2);
    assert_eq!(winners[0]["etag"], stored_etag.as_str());
    assert_eq!(
        written["layers"]["neverSave"]["rules"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn start_initializes_a_new_install_once_with_a_usable_vault_and_a_valid_draft() {
    let tmp = TempDir::new();
    let mut draft: Value =
        serde_json::from_str(memocampus_native_host::policy::DEFAULT_JSON).unwrap();
    draft["revision"] = 7.into();
    draft["layers"]["neverSave"]["rules"] = json!([pattern_json("blind.example")]);
    let start = |draft: &Value| {
        call(
            &tmp,
            json!({ "type": "policy_start", "requestId": "s", "policy": draft }),
        )
    };
    // Before it, nothing is recorded and nothing is kept for later.
    let early = exchange(&tmp, &[visit("e", "https://early.example/", AT)]);
    assert_policy_failure(&early[0], "e", "policy_not_initialized");
    // No vault: recording cannot start.
    assert_eq!(
        start(&draft),
        refusal("policy_start_result", "s", "vault_not_configured")
    );
    assert!(store_of(&tmp).never_initialized());
    let created = call(
        &tmp,
        json!({ "type": "vault_create", "requestId": "c", "useDefault": true }),
    );
    assert_eq!(created["ok"], true, "{created}");
    assert!(store_of(&tmp).never_initialized(), "a vault starts nothing");
    let invalid = with_never(draft.clone(), json!([regex_json("(CANARY")]));
    let reply = start(&invalid);
    assert_eq!(reply["error"], "policy_invalid");
    assert!(!reply.to_string().contains("CANARY"));
    let mut v1 = draft.clone();
    (v1["schemaVersion"], v1["engineVersion"]) = (1.into(), 1.into());
    v1["layers"]["neverSave"]["rules"] = json!(["blind.example"]);
    assert_eq!(
        start(&v1),
        refusal("policy_start_result", "s", "policy_version_mismatch")
    );
    assert_eq!(
        call(&tmp, json!({ "type": "policy_start", "requestId": "s" })),
        refusal("policy_start_result", "s", "invalid_policy_draft")
    );
    assert!(
        store_of(&tmp).never_initialized(),
        "no refusal wrote anything"
    );

    let started = start(&draft);
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["revision"], 1, "the host sets the first revision");
    let (written, _) = active(&tmp);
    let mut expected = draft.clone();
    expected["revision"] = 1.into();
    assert_eq!(written, expected);
    // A new host process finds it started, and records with it.
    assert_eq!(status(&tmp)["state"], "ready");
    assert_eq!(
        exchange(
            &tmp,
            &[
                visit("b", "https://blind.example/", AT),
                visit("n", "https://normal.example/", AT)
            ]
        ),
        [dropped("b"), stored("n")]
    );
    let rows = row_dump(&tmp.default_vault());
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].contains("early.example"));
    // Once only: never over an existing policy.
    let before = policy_bytes(&tmp);
    assert_eq!(
        start(&draft),
        refusal("policy_start_result", "s", "policy_already_initialized")
    );
    assert_eq!(policy_bytes(&tmp), before);
    let other = TempDir::new();
    create_default_vault(&other);
    assert_eq!(
        call(
            &other,
            json!({ "type": "policy_start", "requestId": "s", "policy": draft })
        ),
        refusal("policy_start_result", "s", "policy_already_initialized")
    );
    // An existing install is told it is initialized, before anything about its vault.
    let no_vault = TempDir::new();
    initialize_policy(&no_vault);
    assert_eq!(
        call(
            &no_vault,
            json!({ "type": "policy_start", "requestId": "s", "policy": draft })
        ),
        refusal("policy_start_result", "s", "policy_already_initialized")
    );
}

#[test]
fn the_management_messages_share_one_connection_and_survive_bad_requests() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let (policy, _) = active(&tmp);
    let replies = exchange(
        &tmp,
        &[
            json!({ "type": "policy_validate", "policy": policy }),
            json!({ "type": "policy_validate", "requestId": 1, "policy": policy }),
            json!({ "type": "policy_test_rule", "requestId": "1", "rule": pattern_json("a.test"), "url": "https://a.test/" }),
            json!({ "type": "policy_test_url", "requestId": "2", "policy": policy, "url": "https://a.test/" }),
            json!({ "type": "policy_preset", "requestId": "3", "presetId": "default" }),
            json!({ "type": "policy_validate", "requestId": "4", "policy": policy }),
            json!({ "type": "policy_reset", "requestId": "5" }),
            json!({ "type": "policy_apply", "requestId": "6" }),
        ],
    );
    assert_eq!(
        replies[0],
        json!({ "ok": false, "type": "policy_validate_result", "requestId": null, "error": "invalid_request_id" })
    );
    assert_eq!(replies[1]["error"], "invalid_request_id");
    for (reply, id) in replies[2..6].iter().zip(["1", "2", "3", "4"]) {
        assert_eq!(
            (reply["ok"].as_bool(), reply["requestId"].as_str()),
            (Some(true), Some(id)),
            "{reply}"
        );
    }
    assert_eq!(
        replies[6],
        json!({ "ok": false, "eventId": null, "error": "unknown_type" })
    );
    assert_eq!(
        replies[7],
        refusal("policy_apply_result", "6", "invalid_etag")
    );
}

#[cfg(windows)]
#[test]
fn a_failed_write_applies_nothing() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let (policy, etag) = active(&tmp);
    let before = policy_bytes(&tmp);
    let draft = with_never(policy, json!([pattern_json("new.example")]));
    let message =
        json!({ "type": "policy_apply", "requestId": "a", "expectedEtag": etag, "policy": draft });
    // Shared for reading only: the replacement of the file fails.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(store_of(&tmp).policy_path())
        .unwrap();
    assert_eq!(
        call(&tmp, message.clone()),
        refusal("policy_apply_result", "a", "policy_unavailable")
    );
    drop(held);
    assert_eq!(policy_bytes(&tmp), before);
    assert_eq!(
        call(&tmp, message)["revision"],
        2,
        "the same ETag still applies once"
    );
}

fn domain_json(pattern: &str) -> Value {
    json!({ "kind": "domainWildcard", "pattern": pattern })
}

#[test]
fn a_domain_wildcard_is_tested_alone_by_the_production_matcher() {
    let tmp = TempDir::new();
    let test = |rule: Value, url: &str| {
        call(
            &tmp,
            json!({ "type": "policy_test_rule", "requestId": "r", "rule": rule, "url": url }),
        )
    };
    for (url, expected) in [
        ("https://amazon.co.jp/", true),
        ("https://www.amazon.co.jp/", true),
        ("https://seller.amazon.co.jp/", true),
        ("https://a.b.amazon.co.jp/", true),
        ("https://WWW.AMAZON.CO.JP:8443/x?y#z", true),
        ("https://u:p@www.amazon.co.jp./", true),
        ("https://fake-amazon.co.jp/", false),
        ("https://amazon.co.jp.evil.com/", false),
        ("https://notamazon.co.jp/", false),
        ("https://evil.com/www.amazon.co.jp", false),
    ] {
        let reply = test(domain_json("*.amazon.co.jp"), url);
        assert_eq!(
            reply,
            json!({ "ok": true, "type": "policy_test_rule_result", "requestId": "r", "matched": expected }),
            "{url}"
        );
    }
    for (rule, url, code) in [
        // Restricted by the list and not approved: refused, but approvable (see below).
        (
            "*.co.jp",
            "https://amazon.co.jp/",
            "domain_wildcard_not_approved",
        ),
        ("*.com", "https://a.com/", "domain_wildcard_not_approved"),
        (
            "*.github.io",
            "https://a.github.io/",
            "domain_wildcard_not_approved",
        ),
        (
            "*.CANARY.test",
            "https://a.CANARY.test/",
            "domain_wildcard_not_approved",
        ),
        (
            "*.CANARY.com/*",
            "https://a.CANARY.com/",
            "invalid_domain_wildcard",
        ),
        (
            " *.CANARY.com",
            "https://a.CANARY.com/",
            "invalid_domain_wildcard",
        ),
        ("*.amazon.co.jp", "not a url CANARY", "invalid_url"),
        (
            "*.amazon.co.jp",
            "https://amazon.co.jp/%FFCANARY",
            "invalid_url",
        ),
    ] {
        let reply = test(domain_json(rule), url);
        assert_eq!(
            reply,
            refusal("policy_test_rule_result", "r", code),
            "{rule}"
        );
    }
    assert!(
        store_of(&tmp).never_initialized(),
        "testing a rule needs and writes no policy"
    );
}

#[test]
fn domain_wildcards_decide_alike_in_validation_tests_apply_the_batch_and_recording() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let (policy, etag) = active(&tmp);
    let before = policy_bytes(&tmp);

    // Validation names the rule a public suffix makes invalid, and echoes nothing of it.
    let mut invalid = with_never(policy.clone(), json!([domain_json("*.blind-zone.com")]));
    invalid["layers"]["saveButNoMcp"]["enabled"] = false.into();
    invalid["layers"]["saveButNoMcp"]["rules"] = json!([
        domain_json("*.private-zone.com"),
        domain_json("*.CANARY.github.io.")
    ]);
    let reply = call(
        &tmp,
        json!({ "type": "policy_validate", "requestId": "v", "policy": invalid }),
    );
    assert_eq!(reply["error"], "policy_invalid", "{reply}");
    assert_eq!(
        reply["errors"],
        json!([{ "layer": "saveButNoMcp", "ruleIndex": 1, "code": "invalid_domain_wildcard" }])
    );
    assert!(!reply.to_string().contains("CANARY"));

    let mut draft = with_never(
        policy.clone(),
        json!([
            domain_json("*.blind-zone.com"),
            pattern_json("both-zone.com")
        ]),
    );
    draft["layers"]["saveButNoMcp"]["rules"] = json!([
        domain_json("*.private-zone.com"),
        domain_json("*.both-zone.com")
    ]);
    let urls = [
        ("1", "https://blind-zone.com/", "never_save"),
        ("2", "https://www.blind-zone.com/a", "never_save"),
        ("3", "https://a.b.BLIND-ZONE.com:444/", "never_save"),
        ("4", "https://fake-blind-zone.com/", "allow"),
        ("5", "https://blind-zone.com.evil.com/", "allow"),
        ("6", "https://a.private-zone.com/notes", "save_but_no_mcp"),
        ("7", "https://both-zone.com/", "never_save"),
        ("8", "https://x.both-zone.com/", "save_but_no_mcp"),
        ("9", "https://public-zone.com/", "allow"),
    ];
    for (_, url, decision) in urls {
        let reply = call(
            &tmp,
            json!({ "type": "policy_test_url", "requestId": "u", "policy": draft, "url": url }),
        );
        assert_eq!(reply["decision"], decision, "{url} {reply}");
    }
    let validated = call(
        &tmp,
        json!({ "type": "policy_validate", "requestId": "v", "policy": draft }),
    );
    assert_eq!(validated["ok"], true, "{validated}");
    assert_eq!(policy_bytes(&tmp), before, "checking writes nothing");

    let applied = call(
        &tmp,
        json!({ "type": "policy_apply", "requestId": "a", "policy": draft, "expectedEtag": etag }),
    );
    assert_eq!(applied["ok"], true, "{applied}");
    let (written, _) = active(&tmp);
    assert_eq!(
        written["layers"], draft["layers"],
        "stored as written: kind and text"
    );
    // A new process reads the applied policy: the batch and the store judge with it.
    let items: Vec<Value> = urls
        .iter()
        .map(|(id, url, _)| json!({ "id": id, "url": url }))
        .collect();
    let reply = exchange(&tmp, &[batch(json!(items))]).remove(0);
    assert_eq!(
        decisions(&reply),
        urls.iter()
            .map(|(id, _, decision)| (id.to_string(), decision.to_string()))
            .collect::<Vec<_>>()
    );
    let events: Vec<Value> = urls.iter().map(|(id, url, _)| visit(id, url, AT)).collect();
    let acks = exchange(&tmp, &events);
    for ((id, _, decision), ack) in urls.iter().zip(&acks) {
        let want = if *decision == "never_save" {
            dropped(id)
        } else {
            stored(id)
        };
        assert_eq!(ack, &want, "{id}");
    }
    let rows = row_dump(&tmp.default_vault()).join("\n");
    for refused in ["//blind-zone.com/", ".blind-zone.com/", "BLIND-ZONE"] {
        assert!(!rows.contains(refused), "{rows}");
    }
    assert!(!rows.contains("//both-zone.com"), "{rows}");
    assert_eq!(rows.lines().count(), 5, "{rows}");
}

// --- PSL override: approved domains (docs/issue-025-policy-engine.md, PSL override) ---

fn psl_json() -> Value {
    json!({ "source": "publicsuffix/list", "sourceCommit": "8437e5262f13305f94087abe699b4fe2be1d1de8", "snapshotDate": "2026-10-22" })
}

fn domain_call(tmp: &TempDir, kind: &str, fields: Value) -> Value {
    let mut message = json!({ "type": kind, "requestId": "d" });
    for (key, value) in fields.as_object().unwrap() {
        message[key] = value.clone();
    }
    call(tmp, message)
}

fn approve(tmp: &TempDir, pattern: &str, domain: &str) -> Value {
    domain_call(
        tmp,
        "policy_domain_approve",
        json!({ "rule": domain_json(pattern), "domain": domain }),
    )
}

fn approved_domains(tmp: &TempDir) -> Vec<(String, Value)> {
    let reply = domain_call(tmp, "policy_domain_approvals", json!({}));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["psl"], psl_json());
    reply["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["domain"].as_str().unwrap().to_owned(),
                item["inUse"].clone(),
            )
        })
        .collect()
}

#[test]
fn the_check_tells_syntax_from_the_list_and_names_the_snapshot() {
    let tmp = TempDir::new();
    let check = |pattern: &str| {
        domain_call(
            &tmp,
            "policy_domain_check",
            json!({ "rule": domain_json(pattern) }),
        )
    };
    let result = |fields: Value| {
        let mut reply = json!({ "ok": true, "type": "policy_domain_check_result", "requestId": "d", "psl": psl_json() });
        for (key, value) in fields.as_object().unwrap() {
            reply[key] = value.clone();
        }
        reply
    };
    assert_eq!(
        check("*.Amazon.co.jp"),
        result(json!({ "status": "valid", "domain": "amazon.co.jp" }))
    );
    for (pattern, domain, reason) in [
        ("*.CO.JP", "co.jp", "public_suffix"),
        ("*.github.io", "github.io", "public_suffix"),
        ("*.example.newtld", "example.newtld", "unknown_suffix"),
        ("*.corp.internal", "corp.internal", "unknown_suffix"),
        ("*.kawasaki.jp", "kawasaki.jp", "delegated_boundary"),
        ("*.amazonaws.com", "amazonaws.com", "delegated_boundary"),
        ("*.www.example.com", "www.example.com", "not_registrable"),
    ] {
        assert_eq!(
            check(pattern),
            result(
                json!({ "status": "restricted", "reason": reason, "domain": domain, "approved": false })
            ),
            "{pattern}"
        );
    }
    for hard in [
        "*.CANARY.co.jp/*",
        "https://*.CANARY.jp",
        "*.CANARY.jp:443",
        "www.*.CANARY.jp",
        "*",
    ] {
        assert_eq!(
            check(hard),
            result(json!({ "status": "invalid" })),
            "{hard}"
        );
    }
    for rule in [
        json!({ "kind": "pattern", "pattern": "*.co.jp" }),
        json!({ "kind": "regex", "pattern": "co" }),
        json!("*.co.jp"),
        json!({ "kind": "domainWildcard" }),
    ] {
        assert_eq!(
            domain_call(&tmp, "policy_domain_check", json!({ "rule": rule })),
            refusal("policy_domain_check_result", "d", "invalid_rule")
        );
    }
    assert!(
        store_of(&tmp).never_initialized(),
        "checking writes nothing"
    );
    assert!(!tmp.app_dir().join("settings.sqlite3").exists());
}

#[test]
fn an_approval_is_explicit_per_domain_judged_again_and_kept_across_processes() {
    let tmp = TempDir::new();
    // Bad syntax, a registrable domain and a domain other than the one shown are refused.
    for (pattern, domain, code) in [
        ("*.co.jp/*", "co.jp", "invalid_domain_wildcard"),
        ("https://*.co.jp", "co.jp", "invalid_domain_wildcard"),
        ("*.amazon.co.jp", "amazon.co.jp", "domain_not_restricted"),
        ("*.co.jp", "ne.jp", "domain_mismatch"),
        ("*.co.jp", "CO.JP", "domain_mismatch"),
        ("*.co.jp", "*.co.jp", "domain_mismatch"),
    ] {
        assert_eq!(
            approve(&tmp, pattern, domain),
            refusal("policy_domain_approve_result", "d", code),
            "{pattern} {domain}"
        );
    }
    assert_eq!(
        domain_call(
            &tmp,
            "policy_domain_approve",
            json!({ "rule": domain_json("*.co.jp") })
        ),
        refusal("policy_domain_approve_result", "d", "invalid_domain")
    );
    assert_eq!(
        domain_call(
            &tmp,
            "policy_domain_approve",
            json!({ "rule": pattern_json("*.co.jp"), "domain": "co.jp" })
        ),
        refusal("policy_domain_approve_result", "d", "invalid_rule")
    );
    assert!(approved_domains(&tmp).is_empty());

    let first = approve(&tmp, "*.CO.JP", "co.jp");
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["domain"], "co.jp");
    let again = approve(&tmp, "*.co.jp", "co.jp");
    assert_eq!(
        again["approvedAt"], first["approvedAt"],
        "one approval per domain"
    );
    let unicode = approve(&tmp, "*.個人.香港", "xn--gmqw5a.xn--j6w193g");
    assert_eq!(unicode["ok"], true, "{unicode}");
    // Each request is its own host process: the approvals are what was stored.
    assert_eq!(
        approved_domains(&tmp),
        [
            ("co.jp".to_owned(), json!(false)),
            ("xn--gmqw5a.xn--j6w193g".to_owned(), json!(false))
        ],
        "no policy stored: nothing uses them"
    );
    let check = domain_call(
        &tmp,
        "policy_domain_check",
        json!({ "rule": domain_json("*.xn--gmqw5a.xn--j6w193g") }),
    );
    assert_eq!(
        check["approved"], true,
        "Unicode and Punycode are one domain"
    );
    let other = domain_call(
        &tmp,
        "policy_domain_check",
        json!({ "rule": domain_json("*.ne.jp") }),
    );
    assert_eq!(
        other["approved"], false,
        "an approval of co.jp is none of ne.jp"
    );

    // The rule test now matches with the approval, and only for that domain.
    let test = |pattern: &str, url: &str| {
        call(
            &tmp,
            json!({ "type": "policy_test_rule", "requestId": "r", "rule": domain_json(pattern), "url": url }),
        )
    };
    assert_eq!(test("*.co.jp", "https://shop.co.jp/")["matched"], true);
    assert_eq!(test("*.co.jp", "https://shop.ne.jp/")["matched"], false);
    assert_eq!(
        test("*.ne.jp", "https://shop.ne.jp/"),
        refusal(
            "policy_test_rule_result",
            "r",
            "domain_wildcard_not_approved"
        )
    );
    assert!(
        store_of(&tmp).never_initialized(),
        "approving starts nothing"
    );
}

#[test]
fn an_approved_domain_is_judged_alike_by_drafts_apply_the_batch_and_recording() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let (policy, etag) = active(&tmp);
    let before = policy_bytes(&tmp);
    let mut draft = policy.clone();
    draft["layers"]["neverSave"]["rules"] = json!([domain_json("*.blind.newtld")]);
    draft["layers"]["saveButNoMcp"]["rules"] = json!([domain_json("*.co.jp")]);
    let validate = || {
        call(
            &tmp,
            json!({ "type": "policy_validate", "requestId": "v", "policy": draft }),
        )
    };
    let reply = validate();
    assert_eq!(reply["error"], "policy_invalid");
    assert_eq!(
        reply["errors"],
        json!([
            { "layer": "neverSave", "ruleIndex": 0, "code": "domain_wildcard_not_approved" },
            { "layer": "saveButNoMcp", "ruleIndex": 0, "code": "domain_wildcard_not_approved" }
        ])
    );
    let applied = call(
        &tmp,
        json!({ "type": "policy_apply", "requestId": "a", "policy": draft, "expectedEtag": etag }),
    );
    assert_eq!(applied["ok"], false, "not approved: not applied");
    assert_eq!(policy_bytes(&tmp), before);

    assert_eq!(approve(&tmp, "*.blind.newtld", "blind.newtld")["ok"], true);
    assert_eq!(approve(&tmp, "*.co.jp", "co.jp")["ok"], true);
    assert_eq!(policy_bytes(&tmp), before, "approving changes no policy");
    assert_eq!(validate()["ok"], true);
    let urls = [
        ("1", "https://blind.newtld/", "never_save"),
        ("2", "https://a.b.blind.newtld/x", "never_save"),
        ("3", "https://shop.co.jp/", "save_but_no_mcp"),
        ("4", "https://blind.newtld.co.jp/", "save_but_no_mcp"),
        ("5", "https://shop.ne.jp/", "allow"),
        ("6", "https://notblind.newtld/", "allow"),
    ];
    for (_, url, decision) in urls {
        let reply = call(
            &tmp,
            json!({ "type": "policy_test_url", "requestId": "u", "policy": draft, "url": url }),
        );
        assert_eq!(reply["decision"], decision, "{url}");
    }
    let applied = call(
        &tmp,
        json!({ "type": "policy_apply", "requestId": "a", "policy": draft, "expectedEtag": etag }),
    );
    assert_eq!(applied["ok"], true, "{applied}");
    let items: Vec<Value> = urls
        .iter()
        .map(|(id, url, _)| json!({ "id": id, "url": url }))
        .collect();
    let reply = exchange(&tmp, &[batch(json!(items))]).remove(0);
    assert_eq!(
        decisions(&reply),
        urls.iter()
            .map(|(id, _, d)| (id.to_string(), d.to_string()))
            .collect::<Vec<_>>()
    );
    let events: Vec<Value> = urls.iter().map(|(id, url, _)| visit(id, url, AT)).collect();
    for ((id, _, decision), ack) in urls.iter().zip(exchange(&tmp, &events)) {
        assert_eq!(
            ack,
            if *decision == "never_save" {
                dropped(id)
            } else {
                stored(id)
            },
            "{id}"
        );
    }
    let rows = row_dump(&tmp.default_vault()).join("\n");
    assert!(
        !rows.contains("//blind.newtld/") && !rows.contains(".blind.newtld/"),
        "{rows}"
    );

    // The active policy needs both approvals: neither can be revoked.
    assert_eq!(
        approved_domains(&tmp),
        [
            ("blind.newtld".to_owned(), json!(true)),
            ("co.jp".to_owned(), json!(true))
        ]
    );
    for domain in ["co.jp", "blind.newtld"] {
        assert_eq!(
            domain_call(&tmp, "policy_domain_revoke", json!({ "domain": domain })),
            refusal("policy_domain_revoke_result", "d", "domain_in_use"),
            "{domain}"
        );
    }
    let (active_now, etag_now) = active(&tmp);
    assert_eq!(status(&tmp)["state"], "ready");

    // Without the rule, the approval goes, and the rule is refused again.
    let mut without = active_now.clone();
    without["layers"]["saveButNoMcp"]["rules"] = json!([]);
    let applied = call(
        &tmp,
        json!({ "type": "policy_apply", "requestId": "a", "policy": without, "expectedEtag": etag_now }),
    );
    assert_eq!(applied["ok"], true, "{applied}");
    assert_eq!(
        domain_call(&tmp, "policy_domain_revoke", json!({ "domain": "co.jp" })),
        json!({ "ok": true, "type": "policy_domain_revoke_result", "requestId": "d", "domain": "co.jp", "revoked": true })
    );
    assert_eq!(
        approved_domains(&tmp),
        [("blind.newtld".to_owned(), json!(true))]
    );
    let reply = call(
        &tmp,
        json!({ "type": "policy_test_rule", "requestId": "r", "rule": domain_json("*.co.jp"), "url": "https://shop.co.jp/" }),
    );
    assert_eq!(reply["error"], "domain_wildcard_not_approved");
    for bad in [
        json!("CO.JP"),
        json!("*.co.jp"),
        json!(""),
        json!(1),
        json!(null),
    ] {
        assert_eq!(
            domain_call(&tmp, "policy_domain_revoke", json!({ "domain": bad })),
            refusal("policy_domain_revoke_result", "d", "invalid_domain")
        );
    }
}

#[test]
fn approvals_that_cannot_be_read_approve_nothing_and_recording_stops() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let (policy, etag) = active(&tmp);
    assert_eq!(approve(&tmp, "*.co.jp", "co.jp")["ok"], true);
    let mut draft = policy.clone();
    draft["layers"]["saveButNoMcp"]["rules"] = json!([domain_json("*.co.jp")]);
    let applied = call(
        &tmp,
        json!({ "type": "policy_apply", "requestId": "a", "policy": draft, "expectedEtag": etag }),
    );
    assert_eq!(applied["ok"], true, "{applied}");
    assert_eq!(
        exchange(&tmp, &[visit("a", "https://public.test/", AT)]),
        [stored("a")]
    );

    let settings = tmp.app_dir().join("settings.sqlite3");
    let kept = std::fs::read(&settings).unwrap();
    std::fs::write(&settings, b"CANARY damaged").unwrap();
    assert_eq!(
        status(&tmp)["state"],
        "invalid",
        "the active policy needs an approval it cannot read"
    );
    let reply = exchange(&tmp, &[visit("b", "https://public.test/b", AT)]).remove(0);
    assert_eq!(
        reply["ok"], false,
        "no decision: not stored, not acknowledged as dropped: {reply}"
    );
    assert_eq!(
        call(
            &tmp,
            batch(json!([{ "id": "1", "url": "https://public.test/" }]))
        ),
        batch_error("policy_invalid")
    );
    let test = call(
        &tmp,
        json!({ "type": "policy_test_rule", "requestId": "r", "rule": domain_json("*.co.jp"), "url": "https://shop.co.jp/" }),
    );
    assert_eq!(test["error"], "domain_wildcard_not_approved");
    assert_eq!(
        domain_call(&tmp, "policy_domain_approvals", json!({})),
        refusal(
            "policy_domain_approvals_result",
            "d",
            "approvals_unavailable"
        )
    );
    assert_eq!(
        approve(&tmp, "*.ne.jp", "ne.jp"),
        refusal("policy_domain_approve_result", "d", "approvals_unavailable")
    );
    assert_eq!(
        std::fs::read(&settings).unwrap(),
        b"CANARY damaged",
        "never written over"
    );
    assert!(!row_dump(&tmp.default_vault())
        .join("\n")
        .contains("public.test/b"));

    // Restored, everything works again with the same policy.
    std::fs::write(&settings, kept).unwrap();
    assert_eq!(status(&tmp)["state"], "ready");
    assert_eq!(
        exchange(&tmp, &[visit("c", "https://public.test/c", AT)]),
        [stored("c")]
    );
}

// --- Page metadata (Issue #22) -------------------------------------------------------------------

const PAGE_AT: &str = "2026-11-05T01:00:00.000Z";

fn page_visit(id: &str, url: &str, page: Value) -> Value {
    let mut event = visit(id, url, AT);
    event["page"] = page;
    event
}

fn full_page(at: &str, title: &str) -> Value {
    json!({
        "capturedAt": at, "title": title, "description": "Description CANARY",
        "ogTitle": "OG title", "ogDescription": "OG description", "ogSiteName": "Site",
        "ogImageUrl": "https://cdn.test/og.png?size=large&token=keep", "faviconUrl": "https://site.test/favicon.ico",
    })
}

/// Every row of `page_metadata` with its rowid, in URL order.
fn metadata_rows(tmp: &TempDir) -> Vec<Vec<Value>> {
    let db = Connection::open_with_flags(
        tmp.default_vault(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = db
        .prepare("SELECT rowid, url, event_id, title, description, og_title, og_description, og_site_name, og_image_url, favicon_url, captured_at FROM page_metadata ORDER BY url")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            (0..11)
                .map(|index| {
                    Ok(match row.get_ref(index)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => json!(n),
                        other => json!(other.as_str().unwrap()),
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows
}

fn vault_contains(tmp: &TempDir, needle: &[u8]) -> bool {
    std::fs::read(tmp.default_vault())
        .unwrap()
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn page_metadata_is_stored_with_its_visit_and_kept_apart_from_the_title() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let url = "https://public.test/article?id=7&utm=keep";
    let replies = exchange(
        &tmp,
        &[page_visit("v1", url, full_page(PAGE_AT, "Document title"))],
    );
    assert_eq!(replies, [stored("v1")], "the acknowledgement is unchanged");
    let rows = metadata_rows(&tmp);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0][1..],
        [
            json!(url),
            json!("v1"),
            json!("Document title"),
            json!("Description CANARY"),
            json!("OG title"),
            json!("OG description"),
            json!("Site"),
            json!("https://cdn.test/og.png?size=large&token=keep"),
            json!("https://site.test/favicon.ico"),
            json!(PAGE_AT)
        ]
    );
    // The visit keeps the tab title it always had.
    assert!(row_dump(&tmp.default_vault())
        .join("\n")
        .contains("v|v1|https://public.test/article?id=7&utm=keep|Rust Book|"));
}

#[test]
fn a_new_capture_of_a_url_updates_its_row_in_place_and_null_means_absent() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let url = "https://public.test/a";
    exchange(&tmp, &[page_visit("v1", url, full_page(PAGE_AT, "First"))]);
    let first = metadata_rows(&tmp);
    // The page lost its OGP: only the title is left.
    let later = json!({ "capturedAt": "2026-11-05T02:00:00.000Z", "title": "Second" });
    assert_eq!(
        exchange(&tmp, &[page_visit("v2", url, later)]),
        [stored("v2")]
    );
    let rows = metadata_rows(&tmp);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0][0], first[0][0],
        "updated in place, not deleted and inserted"
    );
    assert_eq!(
        rows[0][1..],
        [
            json!(url),
            json!("v2"),
            json!("Second"),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            json!("2026-11-05T02:00:00.000Z")
        ]
    );
    // A visit whose page could not be read leaves the row alone.
    assert_eq!(exchange(&tmp, &[visit("v3", url, AT)]), [stored("v3")]);
    assert_eq!(metadata_rows(&tmp), rows);
}

#[test]
fn a_late_or_resent_capture_never_replaces_a_newer_one() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let url = "https://public.test/a";
    let newer = page_visit("new", url, full_page("2026-11-05T03:00:00.000Z", "Newer"));
    let older = page_visit("old", url, full_page("2026-11-05T02:59:59.999Z", "Older"));
    assert_eq!(
        exchange(&tmp, &[newer.clone(), older]),
        [stored("new"), stored("old")]
    );
    let rows = metadata_rows(&tmp);
    assert_eq!(
        (rows[0][2].clone(), rows[0][3].clone()),
        (json!("new"), json!("Newer"))
    );
    // A resend after a lost acknowledgement: same visit, same row, nothing duplicated.
    assert_eq!(
        exchange(&tmp, &[newer.clone(), newer]),
        [stored("new"), stored("new")]
    );
    assert_eq!(metadata_rows(&tmp), rows);
    let visits = row_dump(&tmp.default_vault());
    assert_eq!(
        visits
            .iter()
            .filter(|row| row.starts_with("v|new|"))
            .count(),
        1
    );
}

#[test]
fn metadata_follows_only_the_visit_stored_with_exactly_that_url() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    exchange(&tmp, &[visit("v1", "https://public.test/a", AT)]);
    // The same event id again, now claiming another URL: the visit is not stored again, and the
    // metadata joins neither URL.
    assert_eq!(
        exchange(
            &tmp,
            &[page_visit(
                "v1",
                "https://public.test/b",
                full_page(PAGE_AT, "Other")
            )]
        ),
        [stored("v1")]
    );
    assert_eq!(metadata_rows(&tmp), Vec::<Vec<Value>>::new());
    exchange(
        &tmp,
        &[page_visit(
            "v2",
            "https://public.test/b",
            full_page(PAGE_AT, "B"),
        )],
    );
    let db = Connection::open(tmp.default_vault()).unwrap();
    let broken: i64 = db
        .query_row(
            "SELECT count(*) FROM pragma_foreign_key_check('page_metadata')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(broken, 0);
    let joined: i64 = db
        .query_row(
            "SELECT count(*) FROM page_metadata m JOIN visits v ON v.event_id = m.event_id AND v.url = m.url",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(joined, 1);
    // The vault itself refuses a row without its visit, and values outside the limits.
    db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    assert!(db
        .execute(
            "INSERT INTO page_metadata (url, event_id, captured_at) VALUES ('https://x.test/', 'none', ?1)",
            [PAGE_AT]
        )
        .is_err());
    assert!(db
        .execute("UPDATE page_metadata SET title = ?1", ["x".repeat(501)])
        .is_err());
    assert!(db
        .execute("UPDATE page_metadata SET title = ''", [])
        .is_err());
    assert!(db
        .execute("UPDATE page_metadata SET captured_at = 'soon'", [])
        .is_err());
}

#[test]
fn bad_metadata_is_ignored_and_the_visit_is_still_stored() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let bad = [
        json!("CANARY"),
        json!([1]),
        json!({ "title": "CANARY no time" }),
        json!({ "capturedAt": "yesterday", "title": "CANARY" }),
        json!({ "capturedAt": PAGE_AT, "title": ["CANARY"] }),
        json!({ "capturedAt": PAGE_AT, "faviconUrl": 1, "title": "CANARY" }),
    ];
    let events: Vec<Value> = bad
        .iter()
        .enumerate()
        .map(|(index, page)| {
            page_visit(
                &format!("v{index}"),
                &format!("https://public.test/{index}"),
                page.clone(),
            )
        })
        .collect();
    let replies = exchange(&tmp, &events);
    assert_eq!(replies.len(), bad.len());
    for (index, reply) in replies.iter().enumerate() {
        assert_eq!(reply, &stored(&format!("v{index}")));
    }
    assert_eq!(metadata_rows(&tmp), Vec::<Vec<Value>>::new());
    assert!(!vault_contains(&tmp, b"CANARY"));
}

#[test]
fn the_host_cleans_page_text_and_keeps_only_web_urls() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let page = json!({
        "capturedAt": PAGE_AT,
        "title": "  A\u{0}\tB \n\n C\u{7}  ",
        "description": "長".repeat(5000),
        "ogTitle": "   ",
        "ogSiteName": "",
        "ogDescription": null,
        "ogImageUrl": "javascript:alert(1)",
        "faviconUrl": "/relative.ico",
    });
    assert_eq!(
        exchange(&tmp, &[page_visit("v", "https://public.test/", page)]),
        [stored("v")]
    );
    let row = &metadata_rows(&tmp)[0];
    assert_eq!(row[3], json!("A B C"));
    assert_eq!(row[4].as_str().unwrap().chars().count(), 1000);
    for index in [5, 6, 7, 8, 9] {
        assert_eq!(row[index], Value::Null, "column {index}");
    }
}

#[test]
fn metadata_is_judged_with_its_visit_by_the_policy() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    add_never_save_rule(&tmp, "blind.test");
    {
        let store = store_of(&tmp);
        let current = store.load().unwrap();
        let mut edited = current.policy.clone();
        edited
            .layers
            .save_but_no_mcp
            .rules
            .push("private.test".into());
        store.update(&current, edited).unwrap();
    }
    let replies = exchange(
        &tmp,
        &[
            page_visit(
                "b",
                "https://blind.test/x",
                full_page(PAGE_AT, "Blind CANARY"),
            ),
            page_visit("p", "https://private.test/x", full_page(PAGE_AT, "Private")),
        ],
    );
    assert_eq!(replies, [dropped("b"), stored("p")]);
    let rows = metadata_rows(&tmp);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], json!("https://private.test/x"));
    assert!(!vault_contains(&tmp, b"Blind CANARY"));
    assert!(!vault_contains(&tmp, b"blind.test"));

    // Without a usable policy nothing is stored, metadata included.
    std::fs::write(store_of(&tmp).policy_path(), b"{").unwrap();
    let reply = &exchange(
        &tmp,
        &[page_visit(
            "n",
            "https://public.test/n",
            full_page(PAGE_AT, "No policy"),
        )],
    )[0];
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(metadata_rows(&tmp).len(), 1);
    assert!(!vault_contains(&tmp, b"No policy"));
}

// --- Navigation facts (Issue #41, Slice 1) ------------------------------------------------------

const NAV_AT: &str = "2026-11-06T12:00:00.000Z";

fn navigation(source: Option<&str>) -> Value {
    json!({
        "sourceVisitId": source, "kind": "document", "transitionType": "link",
        "clientRedirect": false, "serverRedirect": false, "forwardBack": false,
        "fromAddressBar": false, "newTab": false, "occurredAt": NAV_AT
    })
}

fn visit_with(id: &str, url: &str, navigation: Value) -> Value {
    let mut event = visit(id, url, AT);
    event["navigation"] = navigation;
    event
}

fn nav_ack(id: &str, result: &str) -> Value {
    json!({ "ok": true, "eventId": id, "navigation": result })
}

/// `target|source|kind|type|flags|occurred_at`, one per row, sorted.
fn navigation_rows(tmp: &TempDir) -> Vec<String> {
    let db = Connection::open_with_flags(
        tmp.default_vault(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = db
        .prepare("SELECT target_visit_id || '|' || ifnull(source_visit_id, 'NULL') || '|' || kind || '|' || transition_type || '|' || client_redirect || server_redirect || forward_back || from_address_bar || new_tab || '|' || occurred_at FROM navigation_events ORDER BY 1")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.collect::<Result<_, _>>().unwrap()
}

#[test]
fn navigations_are_stored_with_their_visit_and_resends_change_nothing() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let mut spa = navigation(Some("b"));
    spa["kind"] = json!("history_state");
    spa["forwardBack"] = json!(true);
    let mut typed = navigation(None);
    typed["transitionType"] = json!("typed");
    typed["fromAddressBar"] = json!(true);
    let mut branch = navigation(Some("a"));
    branch["newTab"] = json!(true);
    branch["serverRedirect"] = json!(true);
    branch["clientRedirect"] = json!(true);
    let mut reload = navigation(Some("r1"));
    reload["transitionType"] = json!("reload");
    let events = [
        visit("a", "https://a.test/", AT),
        visit_with("b", "https://b.test/", branch),
        visit_with("c", "https://b.test/#/next", spa),
        visit_with("d", "https://d.test/", typed),
        visit_with("r1", "https://r.test/", navigation(None)),
        // The same URL again is another visit, and may be the source of the next one.
        visit_with("r2", "https://r.test/", reload),
    ];
    let expected_acks = [
        stored("a"),
        nav_ack("b", "stored"),
        nav_ack("c", "stored"),
        nav_ack("d", "stored"),
        nav_ack("r1", "stored"),
        nav_ack("r2", "stored"),
    ];
    let expected_rows = [
        format!("b|a|document|link|11001|{NAV_AT}"),
        format!("c|b|history_state|link|00100|{NAV_AT}"),
        format!("d|NULL|document|typed|00010|{NAV_AT}"),
        format!("r1|NULL|document|link|00000|{NAV_AT}"),
        format!("r2|r1|document|reload|00000|{NAV_AT}"),
    ];
    // Each round is a new host process, as when an ACK was lost and the queue resends.
    for _ in 0..3 {
        assert_eq!(exchange(&tmp, &events), expected_acks);
        assert_eq!(navigation_rows(&tmp), expected_rows);
        assert_eq!(row_dump(&tmp.default_vault()).len(), 6);
    }
    // A resend that differs keeps what was stored first and still reports it as stored.
    let mut changed = visit_with("d", "https://d.test/", navigation(Some("a")));
    changed["navigation"]["transitionType"] = json!("auto_bookmark");
    assert_eq!(exchange(&tmp, &[changed]), [nav_ack("d", "stored")]);
    assert_eq!(navigation_rows(&tmp), expected_rows);
}

#[test]
fn a_source_that_is_missing_late_or_never_save_is_null_and_save_but_no_mcp_is_kept() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let store = store_of(&tmp);
    let current = store.load().unwrap();
    let mut edited = current.policy.clone();
    edited
        .layers
        .save_but_no_mcp
        .rules
        .push("quiet.test".into());
    store.update(&current, edited).unwrap();
    assert_eq!(
        exchange(
            &tmp,
            &[
                visit("hidden", "https://becomes-hidden.test/CANARY", AT),
                visit("quiet", "https://quiet.test/page", AT),
            ]
        ),
        [stored("hidden"), stored("quiet")]
    );
    add_never_save_rule(&tmp, "becomes-hidden.test");
    let replies = exchange(
        &tmp,
        &[
            visit_with("t1", "https://t.test/1", navigation(Some("hidden"))),
            visit_with("t2", "https://t.test/2", navigation(Some("quiet"))),
            visit_with("t3", "https://t.test/3", navigation(Some("never-sent"))),
            // The source arrives after its target: the target keeps NULL, nothing is patched later.
            visit_with("t4", "https://t.test/4", navigation(Some("late"))),
            visit("late", "https://late.test/", AT),
        ],
    );
    assert_eq!(
        replies,
        [
            nav_ack("t1", "stored"),
            nav_ack("t2", "stored"),
            nav_ack("t3", "stored"),
            nav_ack("t4", "stored"),
            stored("late"),
        ]
    );
    let expected = [
        format!("t1|NULL|document|link|00000|{NAV_AT}"),
        format!("t2|quiet|document|link|00000|{NAV_AT}"),
        format!("t3|NULL|document|link|00000|{NAV_AT}"),
        format!("t4|NULL|document|link|00000|{NAV_AT}"),
    ];
    assert_eq!(navigation_rows(&tmp), expected);
    // Resending t4 now that its source exists still changes nothing.
    assert_eq!(
        exchange(
            &tmp,
            &[visit_with(
                "t4",
                "https://t.test/4",
                navigation(Some("late"))
            )]
        ),
        [nav_ack("t4", "stored")]
    );
    assert_eq!(navigation_rows(&tmp), expected);
    // No reply names a URL, and the navigation rows hold none.
    assert!(!format!("{replies:?}").contains("CANARY"));
    assert!(!navigation_rows(&tmp).join("\n").contains("hidden"));
}

#[test]
fn an_invalid_navigation_keeps_its_visit_and_is_reported_as_invalid() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    exchange(&tmp, &[visit("src", "https://src.test/", AT)]);
    let changed = |key: &str, value: Value| {
        let mut nav = navigation(Some("src"));
        nav[key] = value;
        nav
    };
    let mut extra = navigation(Some("src"));
    extra["sourceUrl"] = json!("https://src.test/");
    let mut missing = navigation(Some("src"));
    missing.as_object_mut().unwrap().remove("newTab");
    let cases = [
        Value::Null,
        json!("link"),
        json!([]),
        extra,
        missing,
        changed("transitionType", json!("auto_toplevel")),
        changed("kind", json!("fragment")),
        changed("forwardBack", json!(1)),
        changed("occurredAt", json!("2026-02-30T12:00:00.000Z")),
        changed("occurredAt", json!("2026-11-06T12:00:00Z")),
        changed("sourceVisitId", json!("")),
        changed("sourceVisitId", json!(7)),
        // Its own id (the last case is bad-12).
        changed("sourceVisitId", json!("bad-12")),
    ];
    let events: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(index, nav)| visit_with(&format!("bad-{index}"), "https://t.test/", nav.clone()))
        .collect();
    assert_eq!(events.len(), 13);
    let replies = exchange(&tmp, &events);
    for (index, reply) in replies.iter().enumerate() {
        assert_eq!(
            reply,
            &nav_ack(&format!("bad-{index}"), "invalid"),
            "case {index}"
        );
    }
    assert_eq!(navigation_rows(&tmp), Vec::<String>::new());
    assert_eq!(row_dump(&tmp.default_vault()).len(), 1 + cases.len());
    // A visit that is itself invalid is refused as before; its navigation changes nothing.
    let mut broken = visit_with("broken", "https://t.test/", navigation(None));
    broken["title"] = json!(1);
    assert_eq!(
        exchange(&tmp, &[broken]),
        [json!({ "ok": false, "eventId": "broken", "error": "invalid_event" })]
    );
}

#[test]
fn a_navigation_never_attaches_to_an_id_stored_with_another_url() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    assert_eq!(
        exchange(&tmp, &[visit("t", "https://first.test/", AT)]),
        [stored("t")]
    );
    let replies = exchange(
        &tmp,
        &[visit_with("t", "https://second.test/", navigation(None))],
    );
    assert_eq!(replies, [nav_ack("t", "invalid")]);
    assert_eq!(navigation_rows(&tmp), Vec::<String>::new());
    assert_eq!(
        row_dump(&tmp.default_vault()),
        [format!("v|t|https://first.test/|Rust Book|{AT}")]
    );
}

#[test]
fn privacy_refusals_and_policy_failures_keep_every_navigation_out() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    exchange(&tmp, &[visit("src", "https://src.test/", AT)]);
    add_never_save_rule(&tmp, "never.test");
    let mut private = visit_with("p", "https://never.test/CANARY", navigation(Some("src")));
    let mut incognito = visit_with("i", "https://src.test/CANARY", navigation(Some("src")));
    incognito["incognito"] = json!(true);
    let replies = exchange(&tmp, &[private.clone(), incognito]);
    assert_eq!(replies[0], dropped("p"));
    assert_refused_without_data(&replies[1], "i");
    assert_eq!(navigation_rows(&tmp), Vec::<String>::new());
    assert_eq!(row_dump(&tmp.default_vault()).len(), 1);

    // No policy decision: nothing is stored, the failure is temporary, and a resend recovers.
    let policy = tmp.app_dir().join("policy.json");
    let good = std::fs::read(&policy).unwrap();
    std::fs::write(&policy, b"{broken").unwrap();
    let event = visit_with("t", "https://t.test/", navigation(Some("src")));
    assert_policy_failure(
        &exchange(&tmp, std::slice::from_ref(&event))[0],
        "t",
        "policy_invalid",
    );
    assert_eq!(navigation_rows(&tmp), Vec::<String>::new());
    assert_eq!(row_dump(&tmp.default_vault()).len(), 1);
    std::fs::write(&policy, &good).unwrap();
    assert_eq!(exchange(&tmp, &[event]), [nav_ack("t", "stored")]);
    assert_eq!(
        navigation_rows(&tmp),
        [format!("t|src|document|link|00000|{NAV_AT}")]
    );
    // Still refused after recovery.
    private["id"] = json!("p2");
    assert_eq!(exchange(&tmp, &[private]), [dropped("p2")]);
    assert_eq!(navigation_rows(&tmp).len(), 1);
}

#[test]
fn a_failed_write_keeps_neither_the_visit_nor_its_navigation() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    exchange(&tmp, &[visit("src", "https://src.test/", AT)]);
    let event = visit_with("t", "https://t.test/", navigation(Some("src")));
    let lock = Connection::open(tmp.default_vault()).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let reply = exchange(&tmp, std::slice::from_ref(&event)).remove(0);
    assert_eq!(
        reply,
        json!({ "ok": false, "eventId": "t", "error": "vault_busy" })
    );
    assert_eq!(reply.get("navigation"), None);
    lock.execute_batch("ROLLBACK;").unwrap();
    assert_eq!(navigation_rows(&tmp), Vec::<String>::new());
    assert_eq!(row_dump(&tmp.default_vault()).len(), 1);
    assert_eq!(exchange(&tmp, &[event]), [nav_ack("t", "stored")]);
    assert_eq!(navigation_rows(&tmp).len(), 1);
}

#[test]
fn concurrent_hosts_store_every_navigation_once_and_keep_the_keys_sound() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    exchange(&tmp, &[visit("root", "https://root.test/", AT)]);
    let workers = 4;
    let per_worker = 15;
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let tmp = &tmp;
            scope.spawn(move || {
                let events: Vec<Value> = (0..per_worker)
                    .map(|n| {
                        // Each chain starts at the shared root, or at another worker's first visit,
                        // which may or may not be stored yet.
                        let source = match n {
                            0 if worker % 2 == 0 => "root".to_owned(),
                            0 => format!("w{}-0", worker - 1),
                            n => format!("w{worker}-{}", n - 1),
                        };
                        visit_with(
                            &format!("w{worker}-{n}"),
                            &format!("https://w{worker}.test/{n}"),
                            navigation(Some(&source)),
                        )
                    })
                    .collect();
                for event in &events {
                    // Like the durable queue, finish this event before sending its successor.
                    // Vault contention is temporary; retry the identical payload, never a
                    // whole batch whose successors could lose their not-yet-stored source.
                    let id = event["id"].as_str().unwrap();
                    for delivery in 0..2 {
                        // The second delivery models a lost ACK and must also be idempotent.
                        for attempt in 1..=10 {
                            let replies = exchange(tmp, std::slice::from_ref(event));
                            assert_eq!(replies.len(), 1);
                            let reply = &replies[0];
                            if reply == &nav_ack(id, "stored") {
                                break;
                            }
                            assert_eq!(
                                reply,
                                &json!({ "ok": false, "eventId": id, "error": "vault_busy" }),
                                "unexpected ACK for {id}, delivery {delivery}, attempt {attempt}"
                            );
                            assert!(attempt < 10, "vault_busy retry limit reached for {id}");
                        }
                    }
                }
            });
        }
    });
    assert_eq!(navigation_rows(&tmp).len(), workers * per_worker);
    let db = Connection::open(tmp.default_vault()).unwrap();
    let visits: i64 = db
        .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
        .unwrap();
    assert_eq!(visits, (1 + workers * per_worker) as i64);
    let broken: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
    let integrity: String = db
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    // Within a chain, and from the root, the source was always stored first, so it is kept.
    let kept: i64 = db
        .query_row(
            "SELECT count(*) FROM navigation_events n JOIN visits s ON s.event_id = n.source_visit_id",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        kept >= (workers * (per_worker - 1) + workers / 2) as i64,
        "{kept}"
    );
}

#[test]
fn backups_carry_the_navigations_and_count_them() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    exchange(
        &tmp,
        &[
            visit("a", "https://a.test/", AT),
            visit_with("b", "https://b.test/", navigation(Some("a"))),
            visit_with("c", "https://c.test/", navigation(None)),
        ],
    );
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
            .prepare("SELECT target_visit_id || '|' || ifnull(source_visit_id, 'NULL') FROM navigation_events ORDER BY 1")
            .unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(dump(&copy), ["b|a", "c|NULL"]);
    assert_eq!(dump(&copy), dump(&tmp.default_vault()));
    assert_eq!(pragma(&copy, "user_version"), 7);
}

#[test]
fn the_host_names_its_capabilities_without_a_vault_or_a_policy() {
    let tmp = TempDir::new();
    let replies = exchange(
        &tmp,
        &[
            json!({ "type": "host_capabilities", "requestId": "h" }),
            json!({ "type": "host_capabilities" }),
        ],
    );
    assert_eq!(
        replies,
        [
            json!({ "ok": true, "type": "host_capabilities_result", "requestId": "h", "capabilities": ["navigation_v1", "vault_items_v1", "visit_calendar_v1"] }),
            json!({ "ok": false, "type": "host_capabilities_result", "requestId": null, "error": "invalid_request_id" }),
        ]
    );
    assert!(!tmp.app_dir().exists(), "nothing created");
}

/// Issue #44, Slice 1 end to end over the real host: images saved by the existing flow, tagged
/// afterwards, read back, searched, untagged, backed up, and one image removed from the file. The
/// vault stays consistent throughout and the image flow answers exactly as before.
#[test]
fn image_tags_over_the_real_host_are_stored_consistently_and_leave_images_unchanged() {
    let tmp = TempDir::new();
    create_default_vault(&tmp);
    let vault = tmp.default_vault();
    assert_eq!(pragma(&vault, "user_version"), 7);
    let first = png_of_size(40, 30, 5000);
    let second = png_of_size(40, 30, 6000);
    let saved = exchange(&tmp, &upload_messages(&first, true));
    let token = saved.last().unwrap()["vaultToken"]
        .as_str()
        .unwrap()
        .to_owned();
    exchange(
        &tmp,
        &upload_as(
            &second,
            "7c0e8f4a-1b2d-4e3f-9a5b-6c7d8e9f0a1b",
            Some(&token),
        ),
    );
    let listed = exchange(&tmp, &[json!({ "type": "image_list", "requestId": "l" })]).remove(0);
    let items = listed["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    let (newer, older) = (&items[0], &items[1]);
    let blobs_before = blob_state(&vault);
    let tag = |kind: &str, image: &Value, name: &str| {
        json!({ "type": kind, "requestId": format!("{kind}-{name}"), "blobId": image["blobId"],
                "sha256": image["sha256"], "vaultToken": token, "tag": name })
    };

    // One process: tags given, one twice in another spelling, read back.
    let replies = exchange(
        &tmp,
        &[
            tag("image_tag_add", older, "Reaction"),
            tag("image_tag_add", older, "漫画"),
            tag("image_tag_add", newer, "漫画"),
            tag("image_tag_add", newer, "  REACTION  "),
            tag("image_tag_add", newer, "reaction"),
            json!({ "type": "image_tags_get", "requestId": "g", "blobId": newer["blobId"] }),
        ],
    );
    for reply in &replies {
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["vaultToken"], json!(token));
    }
    assert_eq!(
        replies
            .iter()
            .take(5)
            .map(|r| r["added"].clone())
            .collect::<Vec<_>>(),
        [
            json!(true),
            json!(true),
            json!(true),
            json!(true),
            json!(false)
        ]
    );
    assert_eq!(replies[3]["tag"], "reaction");
    assert_eq!(replies[5]["tags"], json!(["reaction", "漫画"]));

    // Another process: the tag list, search and refusals.
    let replies = exchange(
        &tmp,
        &[
            json!({ "type": "image_tag_list", "requestId": "l" }),
            json!({ "type": "image_tag_search", "requestId": "s", "tag": "漫画" }),
            json!({ "type": "image_tag_search", "requestId": "s2", "tag": "unknown" }),
            tag("image_tag_add", older, "a\u{0007}b"),
            tag("image_tag_add", older, &"x".repeat(65)),
            json!({ "type": "image_tag_add", "requestId": "w", "blobId": older["blobId"], "sha256": newer["sha256"], "vaultToken": token, "tag": "x" }),
            json!({ "type": "image_tag_add", "requestId": "v", "blobId": older["blobId"], "sha256": older["sha256"], "vaultToken": "0".repeat(64), "tag": "x" }),
        ],
    );
    assert_eq!(
        replies[0]["items"],
        json!([{ "name": "reaction", "imageCount": 2 }, { "name": "漫画", "imageCount": 2 }])
    );
    assert_eq!(replies[1]["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        replies[1]["items"][0], *newer,
        "search items are image_list items"
    );
    assert_eq!(replies[1]["items"][1], *older);
    assert_eq!(replies[2]["items"], json!([]));
    for (reply, code) in replies[3..].iter().zip([
        "invalid_tag",
        "invalid_tag",
        "blob_missing",
        "vault_switched",
    ]) {
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(reply["error"], code, "{reply}");
    }

    // Removal: absent is fine, the last link takes the tag.
    let replies = exchange(
        &tmp,
        &[
            tag("image_tag_remove", older, "never"),
            tag("image_tag_remove", older, "REACTION"),
            tag("image_tag_remove", newer, "reaction"),
            json!({ "type": "image_tag_list", "requestId": "l" }),
        ],
    );
    assert_eq!(replies[0]["removed"], false);
    assert_eq!(replies[1]["removed"], true);
    assert_eq!(replies[1]["tags"], json!(["漫画"]));
    assert_eq!(replies[2]["removed"], true);
    assert_eq!(
        replies[3]["items"],
        json!([{ "name": "漫画", "imageCount": 2 }])
    );

    // The images and the existing image flow are exactly as before.
    assert_eq!(blob_state(&vault), blobs_before);
    let listed_again =
        exchange(&tmp, &[json!({ "type": "image_list", "requestId": "l" })]).remove(0);
    assert_eq!(listed_again["items"], listed["items"]);
    assert_eq!(
        read_back(
            &tmp,
            &older["blobId"],
            older["sha256"].as_str().unwrap(),
            &token
        )
        .unwrap(),
        first
    );

    // A backup carries the tags and is verified.
    let dest = tmp.0.join("backups");
    std::fs::create_dir(&dest).unwrap();
    let response = &exchange(
        &tmp,
        &[json!({ "type": "vault_backup", "requestId": "b", "directory": dest })],
    )[0];
    assert_eq!(response["ok"], true, "{response}");
    let copy = PathBuf::from(response["backupPath"].as_str().unwrap());
    let tags = |path: &Path| -> Vec<String> {
        let db =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut statement = db
            .prepare("SELECT t.name || '|' || l.blob_id FROM image_tag_links l JOIN image_tags t ON t.id = l.tag_id ORDER BY 1")
            .unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(tags(&copy), tags(&vault));
    assert_eq!(tags(&copy).len(), 2);

    // An image removed from the file takes its tags; the vault stays consistent and usable.
    {
        let db = Connection::open(&vault).unwrap();
        db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        let id = older["blobId"].as_i64().unwrap();
        db.execute_batch(&format!(
            "BEGIN; DELETE FROM saved_images WHERE blob_id = {id}; DELETE FROM image_blobs WHERE id = {id}; COMMIT;"
        ))
        .unwrap();
        let integrity: String = db
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let broken: i64 = db
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(broken, 0);
    }
    let replies = exchange(
        &tmp,
        &[
            json!({ "type": "image_tag_list", "requestId": "l" }),
            json!({ "type": "image_tags_get", "requestId": "g", "blobId": older["blobId"] }),
            json!({ "type": "image_tag_search", "requestId": "s", "tag": "漫画" }),
        ],
    );
    assert_eq!(
        replies[0]["items"],
        json!([{ "name": "漫画", "imageCount": 1 }])
    );
    assert_eq!(replies[1]["error"], "blob_missing");
    assert_eq!(replies[2]["items"].as_array().unwrap().len(), 1);
    assert_eq!(replies[2]["items"][0]["blobId"], newer["blobId"]);
}
