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
    json!({ "type": "visit", "id": id, "url": url, "title": "Rust Book", "visitedAt": at })
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
            json!({ "type": "visit", "id": "no-url", "title": "T", "visitedAt": "2026-10-29T10:00:00.000Z" }),
            json!({ "type": "visit", "url": "https://a.test/", "title": "T", "visitedAt": "2026-10-29T10:00:00.000Z" }),
            json!({ "type": "tab_snapshot", "id": "bad-snap", "url": "https://a.test/", "title": "T" }),
            json!({ "type": "visit", "id": "", "url": "https://a.test/", "title": "T", "visitedAt": "2026-10-29T10:00:00.000Z" }),
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
