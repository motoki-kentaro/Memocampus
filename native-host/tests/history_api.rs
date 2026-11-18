//! History Slice 1 through the real host binary: `history_list`, `history_days`, `history_visit`.
//! Each test has its own temp directory as `MEMOCAMPUS_APP_DIR` and a synthetic vault; no user data,
//! policy or Chrome profile is touched.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-history-{}", uuid::Uuid::new_v4()));
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

fn november_tokyo() -> Vec<String> {
    (0..=30)
        .map(|n| {
            if n == 0 {
                "2026-10-31T15:00:00.000Z".to_owned()
            } else {
                format!("2026-11-{:02}T15:00:00.000Z", n)
            }
        })
        .collect()
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
    let db = Connection::open(tmp.vault()).unwrap();
    for (id, url, title, at) in [
        (
            "a1",
            "https://github.com/x",
            "A one",
            "2026-11-07T15:00:00.000Z",
        ),
        (
            "a2",
            "https://github.com/x",
            "A two",
            "2026-11-08T03:00:00.000Z",
        ),
        (
            "g1",
            "https://gist.github.com/y",
            "G",
            "2026-11-08T04:00:00.000Z",
        ),
        ("b1", "https://b.test/", "B", "2026-11-08T14:59:59.999Z"),
        ("n1", "https://b.test/next", "N", "2026-11-08T15:00:00.000Z"),
        ("x1", "https://bad.test/", "Bad", "2026-11-08T24:00:00.000Z"),
    ] {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, ?3, ?4)",
            params![id, url, title, at],
        )
        .unwrap();
    }
    db.execute("INSERT INTO navigation_events (target_visit_id, source_visit_id, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at) VALUES ('g1', 'a2', 'document', 'link', 0, 0, 0, 0, 0, '2026-11-08T04:00:00.000Z')", []).unwrap();
    tmp
}

const FROM: &str = "2026-11-07T15:00:00.000Z";
const TO: &str = "2026-11-08T15:00:00.000Z";

#[test]
fn the_host_names_history_and_refuses_bad_requests_before_any_vault() {
    let tmp = TempDir::new();
    let capabilities = call(
        &tmp,
        json!({ "type": "host_capabilities", "requestId": "h" }),
    );
    assert!(
        capabilities["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("history_v1")),
        "{capabilities}"
    );
    let replies = exchange(
        &tmp,
        &[
            json!({ "type": "history_list", "requestId": "1", "visitedFrom": FROM, "visitedTo": TO, "mode": "regex", "domain": "(" }),
            json!({ "type": "history_list", "requestId": "2", "visitedFrom": FROM, "visitedTo": TO }),
            json!({ "type": "history_days", "requestId": "3", "bounds": ["x"] }),
            json!({ "type": "history_visit", "requestId": "4", "id": "a" }),
        ],
    );
    let errors: Vec<&str> = replies
        .iter()
        .map(|r| r["error"].as_str().unwrap())
        .collect();
    assert_eq!(
        errors,
        [
            "invalid_regex",
            "vault_not_configured",
            "invalid_bounds",
            "vault_not_configured"
        ]
    );
    assert_eq!(replies[1]["requestId"], "2");
    assert!(
        !tmp.app_dir().exists(),
        "no vault, no config is created by a read"
    );
}

#[test]
fn list_days_and_visit_over_stdio_agree_and_name_one_vault() {
    let tmp = setup();
    let before = std::fs::read(tmp.vault()).unwrap();
    let list = |extra: Value| {
        let mut message = json!({ "type": "history_list", "requestId": "l", "visitedFrom": FROM, "visitedTo": TO });
        message
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        call(&tmp, message)
    };
    let all = list(json!({}));
    assert_eq!(all["ok"], true, "{all}");
    let listed: Vec<&str> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        listed,
        ["b1", "g1", "a2", "a1"],
        "the day only (its lower bound is in, n1 at the upper bound is out); x1 has no valid time"
    );
    assert_eq!(
        (
            all["total"].clone(),
            all["pages"].clone(),
            all["size"].clone()
        ),
        (json!(4), json!(1), json!(50))
    );
    let exact = list(json!({ "domain": "GitHub.com", "mode": "exact" }));
    assert_eq!(exact["total"], 2, "no subdomain");
    let pattern = list(json!({ "domain": "github", "mode": "regex" }));
    assert_eq!(pattern["total"], 3);
    let month = call(
        &tmp,
        json!({ "type": "history_days", "requestId": "d", "bounds": november_tokyo(), "domain": "github", "mode": "regex" }),
    );
    assert_eq!(month["vaultToken"], all["vaultToken"]);
    assert_eq!(month["counts"][7], pattern["total"], "day 8 of the month");
    assert_eq!(month["total"], 3);
    let visit = call(
        &tmp,
        json!({ "type": "history_visit", "requestId": "v", "id": "g1" }),
    );
    assert_eq!(
        (
            visit["source"]["state"].clone(),
            visit["source"]["visit"]["id"].clone()
        ),
        (json!("present"), json!("a2"))
    );
    let source = call(
        &tmp,
        json!({ "type": "history_visit", "requestId": "v", "id": "a2" }),
    );
    assert_eq!(source["targets"][0]["id"], "g1");
    assert_eq!(source["source"]["state"], "unknown");
    assert_eq!(
        call(
            &tmp,
            json!({ "type": "history_visit", "requestId": "v", "id": "nope" })
        )["error"],
        "visit_not_found"
    );
    assert_eq!(
        std::fs::read(tmp.vault()).unwrap(),
        before,
        "reading never changes the vault file"
    );
}
