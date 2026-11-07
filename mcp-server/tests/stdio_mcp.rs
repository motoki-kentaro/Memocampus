//! The real `memocampus-mcp` process over stdio, with synthetic vaults and policies in temporary
//! directories only. `MEMOCAMPUS_APP_DIR` and `LOCALAPPDATA` both point into them, so the user's
//! application directory is never read. Every private value contains `CANARY`.

use memocampus_native_host::policy_store::PolicyStore;
use memocampus_native_host::schema;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_memocampus-mcp");
const WAIT: Duration = Duration::from_secs(20);

struct Env {
    dir: PathBuf,
    vault: PathBuf,
    app: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Env {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-mcp-{}", uuid::Uuid::new_v4()));
        let app = dir.join("app");
        fs::create_dir_all(&app).unwrap();
        let vault = dir.join("vault.sqlite3");
        Connection::open(&vault)
            .unwrap()
            .execute_batch(&schema::create_sql())
            .unwrap();
        let env = Self { dir, vault, app };
        env.policy().initialize().unwrap();
        env.rule(true, "never.test");
        env.rule(false, "nomcp.test");
        env
    }

    fn policy(&self) -> PolicyStore {
        PolicyStore::new(self.app.clone()).unwrap()
    }

    /// `never_save` selects layer A (NeverSave), otherwise layer B (SaveButNoMcp).
    fn rule(&self, never_save: bool, rule: &str) {
        let store = self.policy();
        let current = store.load().unwrap();
        let mut next = current.policy.clone();
        let layer = if never_save {
            &mut next.layers.never_save
        } else {
            &mut next.layers.save_but_no_mcp
        };
        layer.rules.push(rule.into());
        store.update(&current, next).unwrap();
    }

    /// Like the Native Host: rollback journal, 3 s busy timeout.
    fn writer(&self) -> Connection {
        let db = Connection::open(&self.vault).unwrap();
        db.busy_timeout(Duration::from_secs(3)).unwrap();
        db
    }

    fn command(&self, vault: &Path) -> Command {
        let mut command = Command::new(EXE);
        command
            .arg("--vault")
            .arg(vault)
            .env("MEMOCAMPUS_APP_DIR", &self.app)
            .env("LOCALAPPDATA", self.dir.join("localappdata"))
            .env_remove("MEMOCAMPUS_DB")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn start(&self) -> Client {
        Client::start(self.command(&self.vault), "2025-06-18")
    }
}

fn visit(db: &Connection, id: &str, url: &str, title: &str, at: &str) {
    db.execute(
        "INSERT INTO visits VALUES (?1, ?2, ?3, ?4)",
        params![id, url, title, at],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn meta(
    db: &Connection,
    url: &str,
    id: &str,
    title: Option<&str>,
    description: Option<&str>,
    og_title: Option<&str>,
    og_description: Option<&str>,
    site: Option<&str>,
) {
    db.execute(
        "INSERT INTO page_metadata (url, event_id, title, description, og_title, og_description, og_site_name, og_image_url, favicon_url, captured_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'https://cdn.public.test/og.png', 'https://cdn.public.test/favicon.ico', '2026-11-05T00:00:00.000Z')",
        params![url, id, title, description, og_title, og_description, site],
    )
    .unwrap();
}

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    /// Every line the server wrote to stdout.
    seen: Vec<String>,
    next_id: u64,
    protocol: String,
}

impl Client {
    fn spawn(mut command: Command) -> Self {
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            seen: Vec::new(),
            next_id: 1,
            protocol: String::new(),
        }
    }

    fn start(command: Command, version: &str) -> Self {
        let mut client = Self::spawn(command);
        let init = client.request(
            "initialize",
            json!({"protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        client.protocol = init["result"]["protocolVersion"]
            .as_str()
            .unwrap()
            .to_owned();
        client.notify("notifications/initialized");
        client
    }

    fn line(&mut self, text: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(text.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.line(&json!({"jsonrpc": "2.0", "method": method}).to_string());
    }

    fn receive(&mut self) -> Value {
        let line = self.lines.recv_timeout(WAIT).expect("a reply in time");
        self.seen.push(line.clone());
        serde_json::from_str(&line).expect("stdout carries JSON-RPC only")
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.line(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        );
        loop {
            let reply = self.receive();
            if reply["id"] == json!(id) {
                assert_eq!(reply["jsonrpc"], "2.0");
                return reply;
            }
        }
    }

    /// The tool's structured answer, or its error text as JSON, with `isError`.
    fn call(&mut self, name: &str, arguments: Value) -> (bool, Value) {
        let reply = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &reply["result"];
        assert!(result.is_object(), "{reply}");
        let is_error = result["isError"] == json!(true);
        let text = result["content"][0]["text"].as_str().unwrap();
        if is_error {
            let error = serde_json::from_str(text).unwrap_or_else(|_| json!({"text": text}));
            (true, error)
        } else {
            let structured = result["structuredContent"].clone();
            assert_eq!(
                serde_json::from_str::<Value>(text).unwrap(),
                structured,
                "text mirrors the structured content"
            );
            (false, structured)
        }
    }

    fn ok(&mut self, name: &str, arguments: Value) -> Value {
        let (is_error, value) = self.call(name, arguments);
        assert!(!is_error, "{name}: {value}");
        value
    }

    fn error(&mut self, name: &str, arguments: Value) -> Value {
        let (is_error, value) = self.call(name, arguments);
        assert!(is_error, "{name}: expected an error, got {value}");
        value
    }

    /// Closes stdin and waits for a clean exit; returns stderr.
    fn finish(mut self) -> String {
        drop(self.stdin.take());
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                break;
            }
            assert!(
                started.elapsed() < WAIT,
                "the server exits when its client goes away"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        for line in &self.seen {
            let value: Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["jsonrpc"], "2.0", "{line}");
        }
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        stderr
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn urls(list: &Value) -> Vec<String> {
    list["visits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["url"].as_str().unwrap().to_owned())
        .collect()
}

fn fingerprint(env: &Env) -> (Vec<u8>, std::time::SystemTime, Vec<String>) {
    let mut names: Vec<String> = fs::read_dir(&env.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    (
        fs::read(&env.vault).unwrap(),
        fs::metadata(&env.vault).unwrap().modified().unwrap(),
        names,
    )
}

// --- Protocol ---------------------------------------------------------------------------------

#[test]
fn the_handshake_negotiates_every_version_claude_clients_use_and_lists_four_read_only_tools() {
    let env = Env::new();
    for (asked, agreed) in [
        ("2024-11-05", "2024-11-05"),
        ("2025-03-26", "2025-03-26"),
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        // Unknown, or without an initialize handshake: the newest version that has one.
        ("2099-01-01", "2025-11-25"),
    ] {
        let mut client = Client::start(env.command(&env.vault), asked);
        assert_eq!(client.protocol, agreed, "{asked}");
        let tools = client.request("tools/list", json!({}));
        let tools = tools["result"]["tools"].as_array().unwrap();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "memocampus_get_domain_stats",
                "memocampus_get_visits_by_day",
                "memocampus_search_images",
                "memocampus_search_visits"
            ]
        );
        for tool in tools {
            assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
            assert_eq!(tool["annotations"]["destructiveHint"], false, "{tool}");
            assert_eq!(tool["annotations"]["openWorldHint"], false, "{tool}");
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert_eq!(tool["outputSchema"]["type"], "object");
        }
        let search = tools
            .iter()
            .find(|t| t["name"] == "memocampus_search_visits")
            .unwrap();
        assert_eq!(search["inputSchema"]["required"], json!(["query"]));
        assert_eq!(
            search["inputSchema"]["properties"]["query"]["maxLength"],
            200
        );
        assert_eq!(search["inputSchema"]["properties"]["limit"]["maximum"], 50);
        let stats = tools
            .iter()
            .find(|t| t["name"] == "memocampus_get_domain_stats")
            .unwrap();
        assert!(stats["description"]
            .as_str()
            .unwrap()
            .contains("does not record time spent"));
        let stderr = client.finish();
        assert_eq!(stderr, "", "nothing is logged");
    }
}

#[test]
fn malformed_messages_unknown_tools_and_bad_arguments_leave_the_server_working() {
    let env = Env::new();
    visit(
        &env.writer(),
        "1",
        "https://public.test/",
        "Public",
        "2026-11-05T00:00:00.000Z",
    );
    let mut client = env.start();
    client.line("{not json");
    client.line("[]");
    client.line(&json!({"jsonrpc": "2.0", "id": 900, "method": "no/such/method"}).to_string());
    // Each unreadable line gets a JSON-RPC error without an id; then the next request is answered.
    let mut unreadable = 0;
    let unknown_method = loop {
        let reply = client.receive();
        if reply["id"].is_null() {
            assert!(reply["error"]["code"].is_i64(), "{reply}");
            unreadable += 1;
            continue;
        }
        break reply;
    };
    assert!(unreadable <= 2, "{unreadable}");
    assert_eq!(unknown_method["id"], 900);
    assert!(unknown_method["error"]["code"].is_i64(), "{unknown_method}");
    let unknown_tool = client.request(
        "tools/call",
        json!({"name": "memocampus_delete_everything", "arguments": {}}),
    );
    assert!(unknown_tool["error"].is_object(), "{unknown_tool}");

    // Types the schema rules out are refused by deserialization, before any read.
    for arguments in [
        json!({}),
        json!({"query": 5}),
        json!({"query": "x", "limit": "10"}),
        json!({"query": "x", "limit": -1}),
        json!({"query": "x", "limit": 1.5}),
        json!({"query": "x", "limit": 4_294_967_296u64}),
        json!({"query": "x", "extra": true}),
        json!({"query": "x", "cursor": 3}),
    ] {
        let error = client.error("memocampus_search_visits", arguments.clone());
        assert!(
            error.to_string().contains("deserialize"),
            "{arguments}: {error}"
        );
    }
    // Values the schema allows but the tool refuses: a fixed code naming the argument.
    for (arguments, argument) in [
        (json!({"query": "x", "limit": 0}), "limit"),
        (json!({"query": "x", "limit": 51}), "limit"),
        (json!({"query": "x".repeat(201)}), "query"),
        (json!({"query": "x", "date_from": "yesterday"}), "date_from"),
        (
            json!({"query": "x", "date_from": "2026-02-30"}),
            "date_from",
        ),
        (
            json!({"query": "x", "date_to": "2026-11-05T00:00:00Z"}),
            "date_to",
        ),
    ] {
        assert_eq!(
            client.error("memocampus_search_visits", arguments.clone()),
            json!({"error": "invalid_argument", "argument": argument}),
            "{arguments}"
        );
    }
    // Checked by VisibleStore: an instant of the right length but not a real one, an empty range.
    for arguments in [
        json!({"query": "x", "date_from": "2026-11-05T25:00:00.000Z"}),
        json!({"query": "x", "date_from": "2026-11-06", "date_to": "2026-11-04"}),
    ] {
        assert_eq!(
            client.error("memocampus_search_visits", arguments),
            json!({"error": "invalid_argument"})
        );
    }
    for cursor in ["", "x", "1.visit_0.0", &"9".repeat(201)] {
        assert_eq!(
            client.error(
                "memocampus_search_visits",
                json!({"query": "", "cursor": cursor})
            ),
            json!({"error": "invalid_cursor"}),
            "{cursor}"
        );
    }
    for (arguments, argument) in [
        (json!({"date": "2026-11-31"}), "date"),
        (json!({"date": "11/05/2026"}), "date"),
        (
            json!({"date": "2026-11-05", "utc_offset": "Asia/Tokyo"}),
            "utc_offset",
        ),
        (
            json!({"date": "2026-11-05", "utc_offset": "+15:00"}),
            "utc_offset",
        ),
        (json!({"date": "2026-11-05", "limit": 0}), "limit"),
    ] {
        assert_eq!(
            client.error("memocampus_get_visits_by_day", arguments),
            json!({"error": "invalid_argument", "argument": argument})
        );
    }
    assert_eq!(
        client.error("memocampus_get_domain_stats", json!({"limit": 101})),
        json!({"error": "invalid_argument", "argument": "limit"})
    );
    assert_eq!(
        client.error("memocampus_search_images", json!({"limit": 51})),
        json!({"error": "invalid_argument", "argument": "limit"})
    );
    // Still answering.
    assert_eq!(
        urls(&client.ok("memocampus_search_visits", json!({"query": ""}))),
        ["https://public.test/"]
    );
    assert_eq!(client.finish(), "");
}

#[test]
fn an_empty_result_is_a_normal_answer() {
    let env = Env::new();
    let mut client = env.start();
    let list = client.ok("memocampus_search_visits", json!({"query": "nothing here"}));
    assert_eq!(list["visits"], json!([]));
    assert_eq!(
        (
            list["total"].clone(),
            list["has_more"].clone(),
            list["next_cursor"].clone()
        ),
        (json!(0), json!(false), Value::Null)
    );
    let stats = client.ok("memocampus_get_domain_stats", json!({}));
    assert_eq!(
        (stats["domains"].clone(), stats["total_visits"].clone()),
        (json!([]), json!(0))
    );
    let images = client.ok("memocampus_search_images", json!({}));
    assert_eq!(images["images"], json!([]));
    let day = client.ok(
        "memocampus_get_visits_by_day",
        json!({"date": "2026-11-05", "utc_offset": "+09:00"}),
    );
    assert_eq!(day["total"], 0);
    client.finish();
}

#[test]
fn the_server_refuses_to_start_without_a_usable_vault_and_creates_nothing() {
    let env = Env::new();
    let run = |args: &[&std::ffi::OsStr]| {
        let output = Command::new(EXE)
            .args(args)
            .env("MEMOCAMPUS_APP_DIR", &env.app)
            .env("LOCALAPPDATA", env.dir.join("localappdata"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };
    let missing = env.dir.join("missing.sqlite3");
    let (code, stdout, stderr) = run(&["--vault".as_ref(), missing.as_os_str()]);
    assert_eq!((code, stdout.as_str()), (Some(2), ""));
    assert_eq!(
        stderr.trim(),
        "memocampus-mcp: vault refused: vault_unavailable"
    );
    assert!(!missing.exists());

    // Schema 4: the shape before Issue #22, refused like every other version.
    let old = env.dir.join("schema4.sqlite3");
    let sql = schema::create_sql();
    let start = sql.find(" CREATE TABLE page_metadata").unwrap();
    let end = start + sql[start..].find(");").unwrap() + 2;
    Connection::open(&old)
        .unwrap()
        .execute_batch(&format!(
            "{}{} PRAGMA user_version = 4;",
            &sql[..start],
            &sql[end..]
        ))
        .unwrap();
    let before = fs::read(&old).unwrap();
    let (code, stdout, stderr) = run(&["--vault".as_ref(), old.as_os_str()]);
    assert_eq!(
        (code, stdout.as_str(), stderr.trim()),
        (
            Some(2),
            "",
            "memocampus-mcp: vault refused: vault_schema_mismatch"
        )
    );
    assert_eq!(fs::read(&old).unwrap(), before);

    // Schema 5: the shape before Issue #41 (no navigation_events), refused the same way.
    let old = env.dir.join("schema5.sqlite3");
    let start = sql.find(" CREATE TABLE navigation_events").unwrap();
    let end = start + sql[start..].find(");").unwrap() + 2;
    let schema_5 = format!("{}{}", &sql[..start], &sql[end..])
        .replace(
            " CREATE INDEX navigation_events_by_source ON navigation_events (source_visit_id);",
            "",
        )
        .replace("PRAGMA user_version = 6;", "PRAGMA user_version = 5;");
    assert!(!schema_5.contains("navigation_events"));
    Connection::open(&old)
        .unwrap()
        .execute_batch(&schema_5)
        .unwrap();
    let before = fs::read(&old).unwrap();
    let (code, stdout, stderr) = run(&["--vault".as_ref(), old.as_os_str()]);
    assert_eq!(
        (code, stdout.as_str(), stderr.trim()),
        (
            Some(2),
            "",
            "memocampus-mcp: vault refused: vault_schema_mismatch"
        )
    );
    assert_eq!(fs::read(&old).unwrap(), before);

    // WAL is refused (the host uses a rollback journal), and nothing is converted.
    let wal = env.dir.join("wal.sqlite3");
    fs::copy(&env.vault, &wal).unwrap();
    Connection::open(&wal)
        .unwrap()
        .query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
        .unwrap();
    let (code, _, stderr) = run(&["--vault".as_ref(), wal.as_os_str()]);
    assert_eq!(
        (code, stderr.trim()),
        (Some(2), "memocampus-mcp: vault refused: vault_unsupported")
    );

    let junk = env.dir.join("junk.sqlite3");
    fs::write(&junk, vec![b'x'; 4096]).unwrap();
    let (code, _, stderr) = run(&["--vault".as_ref(), junk.as_os_str()]);
    assert_eq!(
        (code, stderr.trim()),
        (
            Some(2),
            "memocampus-mcp: vault refused: vault_schema_mismatch"
        )
    );

    for args in [
        &[][..],
        &["--vault".as_ref()],
        &["--vault".as_ref(), "vault.sqlite3".as_ref()],
        &["vault.sqlite3".as_ref()],
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!((code, stdout.as_str()), (Some(2), ""), "{args:?}");
        assert!(stderr.starts_with("memocampus-mcp: "), "{stderr}");
    }
    // The directory holds only what the test made.
    let mut names: Vec<String> = fs::read_dir(&env.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "app",
            "junk.sqlite3",
            "schema4.sqlite3",
            "schema5.sqlite3",
            "vault.sqlite3",
            "wal.sqlite3"
        ]
    );
}

// --- Search -----------------------------------------------------------------------------------

#[test]
fn visits_are_found_by_url_title_and_every_metadata_text_field() {
    let env = Env::new();
    let db = env.writer();
    visit(
        &db,
        "1",
        "https://news.public.test/article?id=7",
        "記事タイトル",
        "2026-11-05T01:00:00.000Z",
    );
    meta(
        &db,
        "https://news.public.test/article?id=7",
        "1",
        Some("Doc Title"),
        Some("Plain description"),
        Some("紅葉の京都"),
        Some("OG description text"),
        Some("Public News"),
    );
    visit(
        &db,
        "2",
        "https://plain.public.test/",
        "No metadata",
        "2026-11-05T02:00:00.000Z",
    );
    let mut client = env.start();
    for query in [
        "news.public",
        "記事",
        "Doc Title",
        "Plain desc",
        "紅葉",
        "OG description",
        "Public News",
        "PUBLIC NEWS",
    ] {
        let list = client.ok("memocampus_search_visits", json!({"query": query}));
        assert_eq!(
            urls(&list),
            ["https://news.public.test/article?id=7"],
            "{query}"
        );
        let item = &list["visits"][0];
        assert_eq!(item["og_title"], "紅葉の京都");
        assert_eq!(item["page_title"], "Doc Title");
        assert_eq!(item["title"], "記事タイトル");
        assert_eq!(item["og_site_name"], "Public News");
        assert_eq!(item["og_image_url"], "https://cdn.public.test/og.png");
        assert_eq!(item["favicon_url"], "https://cdn.public.test/favicon.ico");
        assert_eq!(item["metadata_captured_at"], "2026-11-05T00:00:00.000Z");
        assert_eq!(item["visited_at"], "2026-11-05T01:00:00.000Z");
        assert!(item["id"].as_str().unwrap().starts_with("visit_"));
    }
    let plain = client.ok("memocampus_search_visits", json!({"query": "No metadata"}));
    let item = &plain["visits"][0];
    for field in [
        "page_title",
        "description",
        "og_title",
        "og_description",
        "og_site_name",
        "og_image_url",
        "favicon_url",
        "metadata_captured_at",
    ] {
        assert_eq!(item[field], Value::Null, "{field}");
    }
    // Literal text: wildcards match only themselves.
    assert_eq!(
        client.ok("memocampus_search_visits", json!({"query": "%"}))["total"],
        0
    );
    assert_eq!(
        client.ok("memocampus_search_visits", json!({"query": "_"}))["total"],
        0
    );
    // Exactly 200 characters is accepted, 201 is not.
    assert_eq!(
        client.ok(
            "memocampus_search_visits",
            json!({"query": "あ".repeat(200)})
        )["total"],
        0
    );
    client.finish();
}

#[test]
fn repeated_visits_of_a_url_are_separate_results_with_the_same_metadata() {
    let env = Env::new();
    let db = env.writer();
    for (id, at) in [
        ("a", "2026-11-01T00:00:00.000Z"),
        ("b", "2026-11-02T00:00:00.000Z"),
        ("c", "2026-11-03T00:00:00.000Z"),
    ] {
        visit(&db, id, "https://again.public.test/", "Again", at);
    }
    meta(
        &db,
        "https://again.public.test/",
        "b",
        None,
        None,
        Some("Again OG"),
        None,
        None,
    );
    let mut client = env.start();
    let list = client.ok("memocampus_search_visits", json!({"query": "Again OG"}));
    assert_eq!(list["total"], 3);
    let times: Vec<&str> = list["visits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["visited_at"].as_str().unwrap())
        .collect();
    assert_eq!(
        times,
        [
            "2026-11-03T00:00:00.000Z",
            "2026-11-02T00:00:00.000Z",
            "2026-11-01T00:00:00.000Z"
        ]
    );
    assert!(list["visits"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["og_title"] == "Again OG"));
    client.finish();
}

#[test]
fn date_bounds_take_local_days_or_exact_instants() {
    let env = Env::new();
    let db = env.writer();
    for (id, at) in [
        ("before", "2026-11-04T14:59:59.999Z"),
        ("first", "2026-11-04T15:00:00.000Z"),
        ("last", "2026-11-05T14:59:59.999Z"),
        ("after", "2026-11-05T15:00:00.000Z"),
    ] {
        visit(&db, id, &format!("https://public.test/{id}"), "t", at);
    }
    let mut client = env.start();
    let range = |client: &mut Client, from: &str, to: &str| {
        urls(&client.ok(
            "memocampus_search_visits",
            json!({"query": "", "date_from": from, "date_to": to}),
        ))
    };
    // Exact instants: inclusive start, exclusive end.
    assert_eq!(
        range(
            &mut client,
            "2026-11-04T15:00:00.000Z",
            "2026-11-05T14:59:59.999Z"
        ),
        ["https://public.test/first"]
    );
    // Explicit offset: the local day 2026-11-05 at +09:00, boundaries included and excluded.
    let day = client.ok(
        "memocampus_get_visits_by_day",
        json!({"date": "2026-11-05", "utc_offset": "+09:00"}),
    );
    assert_eq!(
        urls(&day),
        ["https://public.test/last", "https://public.test/first"]
    );
    assert_eq!(
        (
            day["from"].as_str(),
            day["to"].as_str(),
            day["total"].as_u64()
        ),
        (
            Some("2026-11-04T15:00:00.000Z"),
            Some("2026-11-05T15:00:00.000Z"),
            Some(2)
        )
    );
    let west = client.ok(
        "memocampus_get_visits_by_day",
        json!({"date": "2026-11-04", "utc_offset": "-05:00"}),
    );
    assert_eq!(
        urls(&west),
        ["https://public.test/first", "https://public.test/before"]
    );
    // Default: this computer's zone. The answer says which offset and bounds were used, and the
    // same bounds as a range give the same visits.
    let local = client.ok(
        "memocampus_get_visits_by_day",
        json!({"date": "2026-11-05"}),
    );
    let (from, to) = (
        local["from"].as_str().unwrap().to_owned(),
        local["to"].as_str().unwrap().to_owned(),
    );
    assert_eq!(range(&mut client, &from, &to), urls(&local));
    // A date as a range bound is that local day, the upper one included.
    let as_dates = urls(&client.ok(
        "memocampus_search_visits",
        json!({"query": "", "date_from": "2026-11-05", "date_to": "2026-11-05"}),
    ));
    assert_eq!(as_dates, urls(&local));
    client.finish();
}

#[test]
fn pages_continue_with_their_cursor_and_large_rows_end_a_page_early() {
    let env = Env::new();
    let db = env.writer();
    for n in 0..45 {
        visit(
            &db,
            &format!("v{n:02}"),
            &format!("https://page.public.test/{n}"),
            "Paged",
            &format!("2026-11-05T00:00:{:02}.000Z", n % 30),
        );
    }
    let mut client = env.start();
    let mut seen = Vec::new();
    let mut cursor = Value::Null;
    let mut pages = 0;
    loop {
        let mut arguments = json!({"query": "Paged", "limit": 20});
        if !cursor.is_null() {
            arguments["cursor"] = cursor.clone();
        }
        let page = client.ok("memocampus_search_visits", arguments);
        assert_eq!(page["total"], 45);
        pages += 1;
        seen.extend(urls(&page));
        cursor = page["next_cursor"].clone();
        assert_eq!(page["has_more"], json!(!cursor.is_null()));
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(pages, 3);
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!((seen.len(), unique.len()), (45, 45));
    // A cursor of another server process is refused.
    let page = client.ok(
        "memocampus_search_visits",
        json!({"query": "Paged", "limit": 1}),
    );
    let foreign = page["next_cursor"].as_str().unwrap().to_owned();
    client.finish();
    let mut other = env.start();
    assert_eq!(
        other.error(
            "memocampus_search_visits",
            json!({"query": "Paged", "cursor": foreign})
        ),
        json!({"error": "invalid_cursor"})
    );
    other.finish();

    // Rows too large for one answer: URLs and titles are cut and marked, and the page stops at
    // the size budget with a cursor to the rest.
    let env = Env::new();
    let db = env.writer();
    for n in 0..30 {
        let url = format!("https://big.public.test/{n:02}/{}", "u".repeat(6000));
        visit(
            &db,
            &format!("b{n:02}"),
            &url,
            &"長".repeat(5000),
            &format!("2026-11-05T00:01:{n:02}.000Z"),
        );
    }
    let mut client = env.start();
    let first = client.ok(
        "memocampus_search_visits",
        json!({"query": "big.public", "limit": 50}),
    );
    let count = first["visits"].as_array().unwrap().len();
    assert!(count > 0 && count < 30, "{count}");
    assert_eq!(first["has_more"], true);
    let item = &first["visits"][0];
    assert_eq!(item["truncated"], true);
    assert_eq!(item["url"].as_str().unwrap().chars().count(), 2000);
    assert_eq!(item["title"].as_str().unwrap().chars().count(), 2000);
    let size = serde_json::to_string(&first["visits"]).unwrap().len();
    assert!(size <= 200 * 1024, "{size}");
    let mut total = count;
    let mut cursor = first["next_cursor"].clone();
    while !cursor.is_null() {
        let page = client.ok(
            "memocampus_search_visits",
            json!({"query": "big.public", "limit": 50, "cursor": cursor}),
        );
        total += page["visits"].as_array().unwrap().len();
        cursor = page["next_cursor"].clone();
    }
    assert_eq!(total, 30);
    client.finish();
}

// --- Domains ----------------------------------------------------------------------------------

#[test]
fn domains_count_visits_of_public_pages_only() {
    let env = Env::new();
    let db = env.writer();
    for (id, url, at) in [
        (
            "1",
            "https://www.example.test/a",
            "2026-11-01T00:00:00.000Z",
        ),
        (
            "2",
            "https://www.example.test/a",
            "2026-11-02T00:00:00.000Z",
        ),
        (
            "3",
            "https://docs.example.test/",
            "2026-11-02T00:00:00.000Z",
        ),
        ("4", "https://other.test/", "2026-11-03T00:00:00.000Z"),
        ("5", "https://other.test/", "2026-10-01T00:00:00.000Z"),
        (
            "p1",
            "https://never.test/CANARY",
            "2026-11-02T00:00:00.000Z",
        ),
        (
            "p2",
            "https://nomcp.test/CANARY",
            "2026-11-02T00:00:00.000Z",
        ),
        (
            "p3",
            "https://www.example.test/?access_token=CANARY",
            "2026-11-02T00:00:00.000Z",
        ),
    ] {
        visit(&db, id, url, "T", at);
    }
    let mut client = env.start();
    let stats = client.ok(
        "memocampus_get_domain_stats",
        json!({"date_from": "2026-11-01T00:00:00.000Z", "date_to": "2026-11-04T00:00:00.000Z"}),
    );
    assert_eq!(
        stats["domains"],
        json!([
            {"domain": "www.example.test", "visit_count": 2},
            {"domain": "docs.example.test", "visit_count": 1},
            {"domain": "other.test", "visit_count": 1}
        ])
    );
    assert_eq!(
        (
            stats["total_visits"].clone(),
            stats["total_domains"].clone(),
            stats["has_more"].clone()
        ),
        (json!(4), json!(3), json!(false))
    );
    assert_eq!(
        (stats["from"].as_str(), stats["to"].as_str()),
        (
            Some("2026-11-01T00:00:00.000Z"),
            Some("2026-11-04T00:00:00.000Z")
        )
    );
    let all = client.ok("memocampus_get_domain_stats", json!({"limit": 1}));
    assert_eq!(
        all["domains"],
        json!([{"domain": "other.test", "visit_count": 2}])
    );
    assert_eq!(
        (
            all["total_visits"].clone(),
            all["total_domains"].clone(),
            all["has_more"].clone()
        ),
        (json!(5), json!(3), json!(true))
    );
    let stderr = client.finish();
    assert!(!stderr.contains("CANARY"));
}

// --- Privacy ----------------------------------------------------------------------------------

/// Private rows of every kind; each has `CANARY` in its URL, title or metadata.
fn seed_canaries(env: &Env) {
    let db = env.writer();
    let at = "2026-11-05T03:00:00.000Z";
    visit(
        &db,
        "pub",
        "https://public.test/page",
        "Public page",
        "2026-11-05T01:00:00.000Z",
    );
    meta(
        &db,
        "https://public.test/page",
        "pub",
        Some("Public"),
        Some("Visible description"),
        Some("Visible OG"),
        None,
        Some("Public Site"),
    );
    for (id, url, title) in [
        ("never", "https://never.test/CANARY-a", "CANARY never"),
        ("nomcp", "https://nomcp.test/CANARY-b", "CANARY nomcp"),
        (
            "cred",
            "https://public.test/?access_token=CANARY",
            "CANARY credential",
        ),
        ("auth", "https://public.test/login/CANARY", "CANARY login"),
        ("user", "https://u:CANARY@public.test/", "CANARY userinfo"),
        ("bad", "not a url CANARY", "CANARY malformed"),
    ] {
        visit(&db, id, url, title, at);
        meta(
            &db,
            url,
            id,
            Some("CANARY page"),
            Some("CANARY description"),
            Some("CANARY og"),
            Some("CANARY og description"),
            Some("CANARY site"),
        );
    }
    // A private page whose URL and title say nothing; only its og:description holds the word.
    visit(&db, "quiet", "https://nomcp.test/plain", "Plain", at);
    meta(
        &db,
        "https://nomcp.test/plain",
        "quiet",
        None,
        None,
        None,
        Some("secret-word CANARY"),
        None,
    );
    // One image blob with one public save and private saves of every kind.
    db.execute(
        "INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, save_count, data)
         VALUES (printf('%064d', 7), 'image/png', 11, 40, 30, 1, 9999999999999, 99, CAST('CANARY-data' AS BLOB))",
        [],
    )
    .unwrap();
    let blob = db.last_insert_rowid();
    for (save, image, page, incognito, public, at) in [
        (
            "s-public",
            "https://cdn.public.test/one.png",
            Some("https://public.test/page"),
            0,
            1,
            1_790_000_000_000i64,
        ),
        (
            "s-flag",
            "https://cdn.public.test/CANARY-flag.png",
            Some("https://public.test/CANARY"),
            0,
            0,
            1_790_000_100_000,
        ),
        (
            "s-never",
            "https://cdn.public.test/two.png",
            Some("https://never.test/CANARY"),
            0,
            1,
            1_790_000_200_000,
        ),
        (
            "s-nomcp",
            "https://nomcp.test/CANARY.png",
            None,
            0,
            1,
            1_790_000_300_000,
        ),
        (
            "s-incognito",
            "https://cdn.public.test/CANARY-incognito.png",
            None,
            1,
            1,
            1_790_000_400_000,
        ),
    ] {
        db.execute(
            "INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![save, blob, image, page, at, incognito, public],
        )
        .unwrap();
    }
}

#[test]
fn no_private_value_reaches_any_answer_count_page_or_log() {
    let env = Env::new();
    seed_canaries(&env);
    let mut client = env.start();
    let mut answers = Vec::new();
    for query in [
        "CANARY",
        "secret-word",
        "never.test",
        "nomcp",
        "access_token",
        "login",
        "",
        "public",
    ] {
        let list = client.ok(
            "memocampus_search_visits",
            json!({"query": query, "limit": 1}),
        );
        answers.push(list.to_string());
        if query.is_empty() || query == "public" {
            assert_eq!(
                (
                    list["total"].clone(),
                    list["has_more"].clone(),
                    list["next_cursor"].clone()
                ),
                (json!(1), json!(false), Value::Null),
                "{query}"
            );
        } else {
            assert_eq!(
                (
                    list["total"].clone(),
                    list["visits"].clone(),
                    list["next_cursor"].clone()
                ),
                (json!(0), json!([]), Value::Null),
                "{query}"
            );
        }
    }
    let day = client.ok(
        "memocampus_get_visits_by_day",
        json!({"date": "2026-11-05", "utc_offset": "+00:00", "limit": 1}),
    );
    assert_eq!(
        (day["total"].clone(), day["has_more"].clone()),
        (json!(1), json!(false))
    );
    answers.push(day.to_string());
    let stats = client.ok("memocampus_get_domain_stats", json!({}));
    assert_eq!(
        stats["domains"],
        json!([{"domain": "public.test", "visit_count": 1}])
    );
    assert_eq!(
        (
            stats["total_visits"].clone(),
            stats["total_domains"].clone()
        ),
        (json!(1), json!(1))
    );
    answers.push(stats.to_string());
    let images = client.ok("memocampus_search_images", json!({}));
    assert_eq!(images["total"], 1);
    let image = &images["images"][0];
    assert_eq!(
        (
            image["public_save_count"].clone(),
            image["first_saved_at"].clone(),
            image["last_saved_at"].clone()
        ),
        (
            json!(1),
            json!("2026-09-21T14:13:20.000Z"),
            json!("2026-09-21T14:13:20.000Z")
        )
    );
    assert_eq!(
        (
            image["latest_image_url"].as_str(),
            image["latest_page_url"].as_str()
        ),
        (
            Some("https://cdn.public.test/one.png"),
            Some("https://public.test/page")
        )
    );
    assert!(
        image.get("sha256").is_none(),
        "SHA-256 is not published (Issue #12, D4)"
    );
    for query in ["CANARY", "two.png", "nomcp", "incognito"] {
        let found = client.ok("memocampus_search_images", json!({"query": query}));
        assert_eq!(
            (found["total"].clone(), found["images"].clone()),
            (json!(0), json!([])),
            "{query}"
        );
        answers.push(found.to_string());
    }
    answers.push(images.to_string());
    let stderr = client.finish();
    let lines = client_lines(&env);
    // The diagnostic log of both sessions holds no value at all: no query, URL, title or path.
    let log = fs::read_to_string(env.app.join("logs").join("mcp-startup.log")).unwrap();
    assert!(log.contains("\"mcp_tool_call\""), "{log}");
    for value in [
        "secret-word",
        "never.test",
        "access_token",
        "public",
        "https://",
    ] {
        assert!(!log.contains(value), "{value}: {log}");
    }
    assert!(!log.contains(env.vault.to_string_lossy().as_ref()));
    for text in answers.iter().chain(lines.iter()).chain([&stderr, &log]) {
        assert!(!text.contains("CANARY"), "{text}");
        for id in ["never", "nomcp", "cred", "auth", "quiet"] {
            assert!(!text.contains(&format!("\"{id}\"")), "{text}");
        }
    }
}

/// A fresh session's whole stdout for the same requests, raw.
fn client_lines(env: &Env) -> Vec<String> {
    let mut client = env.start();
    client.ok("memocampus_search_visits", json!({"query": "CANARY"}));
    client.ok("memocampus_get_domain_stats", json!({}));
    client.ok("memocampus_search_images", json!({"query": ""}));
    client.error(
        "memocampus_search_visits",
        json!({"query": "CANARY", "cursor": "CANARY"}),
    );
    let lines = client.seen.clone();
    client.finish();
    lines
}

#[test]
fn policy_changes_apply_from_the_next_call_and_failures_close_everything() {
    let env = Env::new();
    let db = env.writer();
    for (id, host) in [
        ("a", "keep.test"),
        ("b", "later-nomcp.test"),
        ("c", "later-never.test"),
    ] {
        visit(
            &db,
            id,
            &format!("https://{host}/"),
            "T",
            "2026-11-05T00:00:00.000Z",
        );
        meta(
            &db,
            &format!("https://{host}/"),
            id,
            None,
            None,
            Some(&format!("OG {host}")),
            None,
            None,
        );
    }
    let mut client = env.start();
    let hosts = |client: &mut Client| {
        let mut list = urls(&client.ok("memocampus_search_visits", json!({"query": ""})));
        list.sort();
        list
    };
    assert_eq!(
        hosts(&mut client),
        [
            "https://keep.test/",
            "https://later-never.test/",
            "https://later-nomcp.test/"
        ]
    );
    env.rule(false, "later-nomcp.test");
    assert_eq!(
        hosts(&mut client),
        ["https://keep.test/", "https://later-never.test/"]
    );
    assert_eq!(
        client.ok(
            "memocampus_search_visits",
            json!({"query": "OG later-nomcp"})
        )["total"],
        0
    );
    env.rule(true, "later-never.test");
    assert_eq!(hosts(&mut client), ["https://keep.test/"]);
    assert_eq!(
        client.ok("memocampus_get_domain_stats", json!({}))["total_visits"],
        1
    );

    let policy = env.app.join("policy.json");
    let good = fs::read(&policy).unwrap();
    let closed = |client: &mut Client| {
        for (name, arguments) in [
            ("memocampus_search_visits", json!({"query": ""})),
            (
                "memocampus_get_visits_by_day",
                json!({"date": "2026-11-05"}),
            ),
            ("memocampus_get_domain_stats", json!({})),
            ("memocampus_search_images", json!({})),
        ] {
            assert_eq!(
                client.error(name, arguments),
                json!({"error": "policy_unavailable"}),
                "{name}"
            );
        }
    };
    fs::write(&policy, b"{ not a policy").unwrap();
    closed(&mut client);
    fs::remove_file(&policy).unwrap();
    closed(&mut client);
    fs::write(&policy, &good).unwrap();
    assert_eq!(hosts(&mut client), ["https://keep.test/"]);
    assert_eq!(client.finish(), "");
}

// --- Vault and concurrency --------------------------------------------------------------------

#[test]
fn a_session_never_changes_the_vault_or_leaves_files_beside_it() {
    let env = Env::new();
    seed_canaries(&env);
    let before = fingerprint(&env);
    let mut client = env.start();
    for _ in 0..3 {
        client.ok("memocampus_search_visits", json!({"query": ""}));
        client.ok(
            "memocampus_get_visits_by_day",
            json!({"date": "2026-11-05"}),
        );
        client.ok("memocampus_get_domain_stats", json!({}));
        client.ok("memocampus_search_images", json!({}));
    }
    client.finish();
    assert_eq!(fingerprint(&env), before);
    let journal: String = Connection::open(&env.vault)
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "delete");
}

#[test]
fn a_vault_that_disappears_or_is_replaced_is_reported_and_never_served_from_memory() {
    let env = Env::new();
    visit(
        &env.writer(),
        "1",
        "https://public.test/old",
        "Old",
        "2026-11-05T00:00:00.000Z",
    );
    let mut client = env.start();
    assert_eq!(
        client.ok("memocampus_search_visits", json!({"query": ""}))["total"],
        1
    );
    let moved = env.dir.join("moved.sqlite3");
    fs::rename(&env.vault, &moved).unwrap();
    for (name, arguments) in [
        ("memocampus_search_visits", json!({"query": ""})),
        ("memocampus_get_domain_stats", json!({})),
        ("memocampus_search_images", json!({})),
    ] {
        assert_eq!(
            client.error(name, arguments),
            json!({"error": "vault_unavailable"}),
            "{name}"
        );
    }
    assert!(!env.vault.exists(), "nothing is created in its place");
    fs::write(&env.vault, vec![0u8; 4096]).unwrap();
    assert_eq!(
        client.error("memocampus_search_visits", json!({"query": ""})),
        json!({"error": "vault_schema_mismatch"})
    );
    fs::remove_file(&env.vault).unwrap();
    fs::rename(&moved, &env.vault).unwrap();
    assert_eq!(
        client.ok("memocampus_search_visits", json!({"query": ""}))["total"],
        1
    );
    assert_eq!(client.finish(), "");
}

#[test]
fn host_writes_continue_while_the_server_reads_and_no_read_sees_half_a_transaction() {
    let env = Env::new();
    {
        let db = env.writer();
        db.execute_batch("BEGIN").unwrap();
        for n in 0..5000 {
            visit(
                &db,
                &format!("seed{n}"),
                &format!("https://s{}.public.test/{n}", n % 50),
                "Seed",
                "2026-11-04T00:00:00.000Z",
            );
        }
        db.execute_batch("COMMIT").unwrap();
    }
    let mut client = env.start();
    let vault = env.vault.clone();
    let (stop, stopped) = mpsc::channel();
    let (half_written, half_ready) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let (finished, done) = mpsc::channel();
    let writer = {
        std::thread::spawn(move || {
            // Each transaction adds a pair of visits, as one commit.
            let db = Connection::open(&vault).unwrap();
            db.busy_timeout(Duration::from_secs(3)).unwrap();
            let mut commits = 0u32;
            // A failed reader drops `stop`, which also ends the writer.
            while matches!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                db.execute_batch("BEGIN IMMEDIATE").unwrap();
                for half in 0..2 {
                    visit(
                        &db,
                        &format!("w{commits}-{half}"),
                        &format!("https://w.public.test/{commits}/{half}"),
                        "Written",
                        "2026-11-05T00:00:00.000Z",
                    );
                    if commits == 0 && half == 0 {
                        half_written.send(()).unwrap();
                        resumed
                            .recv_timeout(WAIT)
                            .expect("reader releases the writer");
                    }
                }
                db.execute_batch("COMMIT").unwrap();
                commits += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
            finished.send(commits).unwrap();
        })
    };
    // Prove a read completes while a write transaction is open and half inserted,
    // without relying on the scheduler to make the two operations overlap.
    half_ready
        .recv_timeout(WAIT)
        .expect("writer starts a transaction");
    assert_eq!(
        client.ok(
            "memocampus_search_visits",
            json!({"query": "Written", "limit": 1})
        )["total"],
        0
    );
    resume.send(()).unwrap();
    let mut last = 0;
    for _ in 0..20 {
        let list = client.ok(
            "memocampus_search_visits",
            json!({"query": "Written", "limit": 1}),
        );
        let total = list["total"].as_u64().unwrap();
        assert_eq!(total % 2, 0, "a whole transaction or none of it");
        assert!(total >= last, "{total} < {last}");
        last = total;
        let stats = client.ok("memocampus_get_domain_stats", json!({"limit": 1}));
        assert!(stats["total_visits"].as_u64().unwrap() >= 5000 + total);
    }
    stop.send(()).unwrap();
    // busy_timeout bounds SQLite's lock waits, not total transaction wall time.
    // Use the harness's completion deadline to detect a stuck writer instead.
    let commits = done
        .recv_timeout(WAIT)
        .expect("writer finishes after reads");
    writer.join().unwrap();
    assert!(commits > 0);
    assert_eq!(
        client.ok(
            "memocampus_search_visits",
            json!({"query": "Written", "limit": 1})
        )["total"],
        u64::from(commits) * 2
    );
    assert_eq!(client.finish(), "");
}

#[test]
fn a_short_lock_is_waited_for_and_a_long_one_is_an_error_the_server_recovers_from() {
    let env = Env::new();
    visit(
        &env.writer(),
        "1",
        "https://public.test/",
        "P",
        "2026-11-05T00:00:00.000Z",
    );
    let mut client = env.start();
    let hold = |duration: Duration| {
        let vault = env.vault.clone();
        let (ready, locked) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let db = Connection::open(&vault).unwrap();
            db.execute_batch("BEGIN EXCLUSIVE").unwrap();
            ready.send(()).unwrap();
            std::thread::sleep(duration);
            db.execute_batch("COMMIT").unwrap();
        });
        locked.recv().unwrap();
        handle
    };
    let short = hold(Duration::from_millis(800));
    let started = Instant::now();
    assert_eq!(
        client.ok("memocampus_search_visits", json!({"query": ""}))["total"],
        1
    );
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "it waited for the lock"
    );
    short.join().unwrap();
    let long = hold(Duration::from_secs(5));
    assert_eq!(
        client.error("memocampus_search_visits", json!({"query": ""})),
        json!({"error": "vault_unavailable"})
    );
    long.join().unwrap();
    assert_eq!(
        client.ok("memocampus_search_visits", json!({"query": ""}))["total"],
        1
    );
    assert_eq!(client.finish(), "");
}

#[test]
fn requests_in_flight_are_answered_and_closing_stdin_ends_the_server() {
    let env = Env::new();
    {
        let db = env.writer();
        db.execute_batch("BEGIN").unwrap();
        for n in 0..3000 {
            visit(
                &db,
                &format!("v{n}"),
                &format!("https://public.test/{n}"),
                "T",
                "2026-11-05T00:00:00.000Z",
            );
        }
        db.execute_batch("COMMIT").unwrap();
    }
    let mut client = env.start();
    // Several calls at once, as a client may send them.
    for id in 100..104 {
        client.line(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "memocampus_get_domain_stats", "arguments": {}}}).to_string());
    }
    let mut answered = std::collections::HashSet::new();
    while answered.len() < 4 {
        let reply = client.receive();
        assert_eq!(
            reply["result"]["structuredContent"]["total_visits"], 3000,
            "{reply}"
        );
        answered.insert(reply["id"].as_u64().unwrap());
    }
    assert_eq!(client.finish(), "");

    // Killed by its client mid-session: the vault is unchanged and opens normally.
    let before = fingerprint(&env);
    let mut client = env.start();
    client.line(&json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "memocampus_search_visits", "arguments": {"query": ""}}}).to_string());
    client.child.kill().unwrap();
    client.child.wait().unwrap();
    drop(client);
    assert_eq!(fingerprint(&env), before);
    let check: String = Connection::open(&env.vault)
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
}

// --- Startup diagnostics log ------------------------------------------------------------------

fn log_file(env: &Env) -> PathBuf {
    env.app.join("logs").join("mcp-startup.log")
}

/// The log lines of one process, each parsed as JSON.
fn log_events(env: &Env, pid: u32) -> Vec<Value> {
    let text = fs::read_to_string(log_file(env)).unwrap_or_default();
    text.lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("one JSON object per line"))
        .filter(|event| event["pid"] == json!(pid))
        .collect()
}

fn names(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["event"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_session_is_logged_from_start_to_exit_in_a_file_and_stdout_stays_protocol_only() {
    let env = Env::new();
    let mut client = env.start();
    let pid = client.child.id();
    client.ok("memocampus_get_domain_stats", json!({"limit": 1}));
    let unknown = client.request("tools/call", json!({"name": "not_a_tool", "arguments": {}}));
    assert_eq!(unknown["error"]["code"], -32602);
    let stderr = client.finish();
    assert_eq!(stderr, "", "nothing on stderr for a normal session");
    // `finish` checked that every stdout line is JSON-RPC.
    let all = log_events(&env, pid);
    // A send is logged when it completes, which may come after the next message is received:
    // only its presence is fixed, not its place.
    let sent: Vec<&Value> = all
        .iter()
        .filter(|event| event["event"] == "mcp_error_response_sent")
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["code"], -32602, "only the error code");
    let events: Vec<Value> = all
        .iter()
        .filter(|event| event["event"] != "mcp_error_response_sent")
        .cloned()
        .collect();
    assert_eq!(
        names(&events),
        [
            "process_start",
            "startup_context",
            "vault_init_start",
            "vault_init_success",
            "mcp_transport_start",
            "mcp_initialize_received",
            "mcp_initialize_response_flushed",
            "mcp_service_started",
            "mcp_initialized",
            "mcp_tool_call",
            "mcp_tool_call",
            "stdin_eof",
            "mcp_receive_end",
            "mcp_transport_close",
            "process_exit",
        ]
    );
    for event in &all {
        assert!(event["ts"].as_str().unwrap().ends_with('Z'), "{event}");
        assert!(event["elapsed_ms"].is_u64(), "{event}");
        assert_eq!(event["ok"], json!(true), "{event}");
    }
    let context = &events[1];
    assert_eq!(context["mode"], "mcp");
    assert_eq!(context["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(context["argc"], 2);
    assert_eq!(
        context["env_set"],
        json!({"LOCALAPPDATA": true, "MEMOCAMPUS_APP_DIR": true})
    );
    assert!(context["exe"].as_str().unwrap().contains("memocampus-mcp"));
    if cfg!(windows) {
        assert_eq!(
            (&context["stdin"], &context["stdout"], &context["stderr"]),
            (&json!("pipe"), &json!("pipe"), &json!("pipe"))
        );
        assert_eq!(context["parent_pid"], json!(std::process::id()));
    }
    assert_eq!(
        (&events[5]["protocol_version"], &events[5]["client_name"]),
        (&json!("2025-06-18"), &json!("test"))
    );
    assert_eq!(events[9]["tool"], "memocampus_get_domain_stats");
    assert_eq!(
        events[10]["tool"], "other",
        "a name the server does not define"
    );
    assert_eq!(events[14]["reason"], "closed");
    let text = fs::read_to_string(log_file(&env)).unwrap();
    let vault = env.vault.to_string_lossy();
    assert!(!text.contains(vault.as_ref()), "no vault path");
    assert!(!text.contains("not_a_tool") && !text.contains("\"limit\""));
}

#[test]
fn a_refused_start_is_logged_with_its_stage_and_os_error_but_not_the_path() {
    let env = Env::new();
    let missing = env.dir.join("CANARY-missing.sqlite3");
    let child = env.command(&missing).spawn().unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let events = log_events(&env, pid);
    assert_eq!(
        names(&events),
        [
            "process_start",
            "startup_context",
            "vault_init_start",
            "vault_init_failure",
            "process_error",
        ]
    );
    let failure = &events[3];
    assert_eq!(
        (&failure["ok"], &failure["step"], &failure["error"]),
        (
            &json!(false),
            &json!("vault_check"),
            &json!("vault_unavailable")
        )
    );
    assert_eq!(failure["vault_file"]["exists"], false);
    if cfg!(windows) {
        assert_eq!(failure["vault_file"]["os_error"], 2, "ERROR_FILE_NOT_FOUND");
    }
    assert_eq!(
        (&events[4]["stage"], &events[4]["exit_code"]),
        (&json!("vault_init"), &json!(2))
    );

    let child = Command::new(EXE)
        .arg("--vault")
        .arg("CANARY.sqlite3")
        .env("MEMOCAMPUS_APP_DIR", &env.app)
        .env("LOCALAPPDATA", env.dir.join("localappdata"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(2));
    let events = log_events(&env, pid);
    assert_eq!(events.last().unwrap()["stage"], "arguments");
    assert_eq!(events.last().unwrap()["error"], "not_absolute");
    let text = fs::read_to_string(log_file(&env)).unwrap();
    assert!(!text.contains("CANARY"), "{text}");
}

#[test]
fn a_log_that_cannot_be_written_changes_nothing_for_the_client() {
    let env = Env::new();
    // A file where the log directory would be: neither the directory nor the log can be made.
    fs::write(env.app.join("logs"), b"not a directory").unwrap();
    let mut client = env.start();
    let stats = client.ok("memocampus_get_domain_stats", json!({}));
    assert_eq!(stats["total_visits"], 0);
    assert_eq!(client.finish(), "");
    assert_eq!(fs::read(env.app.join("logs")).unwrap(), b"not a directory");

    // No application directory at all: nothing is written anywhere, and the server works.
    let mut command = env.command(&env.vault);
    command
        .env_remove("MEMOCAMPUS_APP_DIR")
        .env_remove("LOCALAPPDATA");
    let output = command.stdin(Stdio::null()).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "no policy without the app dir"
    );
    assert!(output.stdout.is_empty());
}
