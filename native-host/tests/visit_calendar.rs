//! Issue #5, Slice 1 through the real host binary: `visit_days` and the date range of `search_pages`.
//! Each test has its own temp directory as `MEMOCAMPUS_APP_DIR` and a synthetic vault; no user data,
//! policy or Chrome profile is touched.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-days-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn app_dir(&self) -> PathBuf {
        self.0.join("app")
    }

    fn vault(&self) -> PathBuf {
        self.app_dir().join("memocampus.sqlite3")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One host process, as Chrome starts one per message: every reply.
fn exchange(tmp: &TempDir, messages: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_memocampus-native-host"))
        .env("MEMOCAMPUS_APP_DIR", tmp.app_dir())
        .env_remove("MEMOCAMPUS_DB")
        .current_dir(&tmp.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for message in messages {
        let body = serde_json::to_vec(message).unwrap();
        stdin.write_all(&(body.len() as u32).to_ne_bytes()).unwrap();
        stdin.write_all(&body).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "host must not log");
    let mut stdout = output.stdout.as_slice();
    let mut replies = Vec::new();
    let mut length = [0u8; 4];
    while stdout.read_exact(&mut length).is_ok() {
        let mut body = vec![0u8; u32::from_ne_bytes(length) as usize];
        stdout.read_exact(&mut body).unwrap();
        replies.push(serde_json::from_slice(&body).unwrap());
    }
    replies
}

fn call(tmp: &TempDir, message: Value) -> Value {
    exchange(tmp, &[message]).remove(0)
}

/// November 2026 in Tokyo (UTC+9): 31 bounds, the start of each local day and of December 1.
fn november_tokyo() -> Vec<String> {
    (0..=30)
        .map(|n| {
            let day = n + 1;
            if day == 1 {
                "2026-10-31T15:00:00.000Z".to_owned()
            } else {
                format!("2026-11-{:02}T15:00:00.000Z", day - 1)
            }
        })
        .collect()
}

fn days(bounds: &[String]) -> Value {
    json!({ "type": "visit_days", "requestId": "d", "bounds": bounds })
}

fn fill(path: &Path, visits: &[(&str, &str, &str, &str)]) {
    let db = Connection::open(path).unwrap();
    for (id, url, title, at) in visits {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, ?3, ?4)",
            params![id, url, title, at],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO tab_snapshots VALUES ('s', 1, 0, 'https://snap.test/', 'Snap', 0, 0, '2026-11-08T01:00:00.000Z')",
        [],
    )
    .unwrap();
}

fn setup() -> TempDir {
    let tmp = TempDir::new();
    memocampus_native_host::policy_store::PolicyStore::new(tmp.app_dir())
        .unwrap()
        .initialize()
        .unwrap();
    assert_eq!(
        call(
            &tmp,
            json!({ "type": "vault_create", "requestId": "c", "useDefault": true })
        )["ok"],
        true
    );
    fill(
        &tmp.vault(),
        &[
            ("a1", "https://a.test/", "A old", "2026-11-06T10:00:00.000Z"),
            ("a2", "https://a.test/", "A day", "2026-11-07T15:00:00.000Z"),
            ("a3", "https://a.test/", "", "2026-11-08T03:00:00.000Z"),
            ("b1", "https://b.test/", "B", "2026-11-08T14:59:59.999Z"),
            ("x1", "https://bad.test/", "Bad", "2026-11-08T24:00:00.000Z"),
            ("x2", "https://bad.test/", "Bad", "2026-02-30T10:00:00.000Z"),
        ],
    );
    tmp
}

#[test]
fn the_host_names_the_calendar_and_checks_bounds_before_any_vault() {
    let tmp = TempDir::new();
    let capabilities = call(
        &tmp,
        json!({ "type": "host_capabilities", "requestId": "h" }),
    );
    assert!(capabilities["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!("visit_calendar_v1")));
    // No vault and no policy: a malformed request says so, a valid one says there is no vault.
    let bad = call(
        &tmp,
        json!({ "type": "visit_days", "requestId": "d", "bounds": ["x"] }),
    );
    assert_eq!(
        bad,
        json!({ "ok": false, "type": "visit_days_result", "requestId": "d", "error": "invalid_bounds" })
    );
    let no_vault = call(&tmp, days(&november_tokyo()));
    assert_eq!(
        no_vault,
        json!({ "ok": false, "type": "visit_days_result", "requestId": "d", "error": "vault_not_configured" })
    );
    let no_id = call(
        &tmp,
        json!({ "type": "visit_days", "bounds": november_tokyo() }),
    );
    assert_eq!(no_id["error"], "invalid_request_id");
    assert_eq!(no_id["requestId"], Value::Null);
}

#[test]
fn days_are_counted_from_visits_only_and_name_the_vault_of_the_search() {
    let tmp = setup();
    let answer = call(&tmp, days(&november_tokyo()));
    assert_eq!(answer["ok"], true, "{answer}");
    let counts: Vec<u64> = serde_json::from_value(answer["counts"].clone()).unwrap();
    assert_eq!(counts.len(), 30);
    // Nov 1..30 in Tokyo: a2 is Nov 8 00:00, a3 and b1 Nov 8 too; a1 Nov 6 19:00; no snapshot, no
    // invalid time.
    let nonzero: Vec<(usize, u64)> = counts
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(i, c)| (i + 1, *c))
        .collect();
    assert_eq!(nonzero, [(6, 1), (8, 3)]);
    let search = call(&tmp, json!({ "type": "search_pages", "requestId": "s" }));
    assert_eq!(answer["vaultToken"], search["vaultToken"]);
    assert_eq!(answer["vaultToken"].as_str().map(str::len), Some(64));
    // The rows are untouched.
    let rows: i64 = Connection::open(tmp.vault())
        .unwrap()
        .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 6);
}

#[test]
fn a_day_search_through_the_host_has_the_day_only_and_without_a_range_nothing_changed() {
    let tmp = setup();
    let day = call(
        &tmp,
        json!({ "type": "search_pages", "requestId": "s", "visitedFrom": "2026-11-07T15:00:00.000Z", "visitedTo": "2026-11-08T15:00:00.000Z" }),
    );
    assert_eq!(
        day["items"],
        json!([
            { "url": "https://b.test/", "title": "B", "lastVisitedAt": "2026-11-08T14:59:59.999Z", "lastCapturedAt": null, "lastActivityAt": "2026-11-08T14:59:59.999Z", "source": "visit" },
            { "url": "https://a.test/", "title": "A day", "lastVisitedAt": "2026-11-08T03:00:00.000Z", "lastCapturedAt": null, "lastActivityAt": "2026-11-08T03:00:00.000Z", "source": "visit" },
        ])
    );
    assert_eq!(day["hasMore"], false);
    let with_query = call(
        &tmp,
        json!({ "type": "search_pages", "requestId": "s", "query": "b.test", "visitedFrom": "2026-11-07T15:00:00.000Z", "visitedTo": "2026-11-08T15:00:00.000Z" }),
    );
    assert_eq!(with_query["items"].as_array().unwrap().len(), 1);
    // Without a range, every URL as before: the snapshot and the invalid times included.
    let all = call(&tmp, json!({ "type": "search_pages", "requestId": "s" }));
    let urls: Vec<&str> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["url"].as_str().unwrap())
        .collect();
    assert_eq!(
        urls,
        [
            "https://bad.test/",
            "https://b.test/",
            "https://a.test/",
            "https://snap.test/"
        ]
    );
    for bad in [
        json!({ "type": "search_pages", "requestId": "s", "visitedFrom": "2026-11-07T15:00:00.000Z" }),
        json!({ "type": "search_pages", "requestId": "s", "visitedFrom": "2026-11-08T15:00:00.000Z", "visitedTo": "2026-11-07T15:00:00.000Z" }),
    ] {
        assert_eq!(call(&tmp, bad)["error"], "invalid_bounds");
    }
}

#[test]
fn after_a_vault_switch_the_counts_and_their_token_are_the_other_vault_s() {
    let tmp = setup();
    let first = call(&tmp, days(&november_tokyo()));
    let other = tmp.0.join("second.sqlite3");
    assert_eq!(
        call(
            &tmp,
            json!({ "type": "vault_create", "requestId": "c", "path": other })
        )["ok"],
        true
    );
    let db = Connection::open(&other).unwrap();
    db.execute(
        "INSERT INTO visits VALUES ('z', 'https://z.test/', 'Z', '2026-11-20T03:00:00.000Z')",
        [],
    )
    .unwrap();
    drop(db);
    let second = call(&tmp, days(&november_tokyo()));
    assert_ne!(first["vaultToken"], second["vaultToken"]);
    let counts: Vec<u64> = serde_json::from_value(second["counts"].clone()).unwrap();
    assert_eq!(counts.iter().sum::<u64>(), 1);
    assert_eq!(counts[19], 1, "Nov 20 in Tokyo, of the new vault only");
    let search = call(&tmp, json!({ "type": "search_pages", "requestId": "s" }));
    assert_eq!(search["vaultToken"], second["vaultToken"]);
}
