//! The real `memocampus-mcp` process over Streamable HTTP on loopback, beside the same process over
//! stdio, with synthetic vaults and policies in temporary directories only. `MEMOCAMPUS_APP_DIR`
//! and `LOCALAPPDATA` both point into them, so neither the user's application directory nor their
//! token is ever read. Every private value contains `CANARY`.

use memocampus_native_host::policy_store::PolicyStore;
use memocampus_native_host::schema;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_memocampus-mcp");
const WAIT: Duration = Duration::from_secs(20);
const LEGACY: &str = "2025-11-25";
const MODERN: &str = "2026-07-28";

/// Names `vault` as the active vault in `app/config.json`, as the Native Host writes it, with a
/// publication `state` ("confirmed" / "pending") for its file, or none (Issue #51).
fn publish(app: &Path, vault: &Path, state: Option<&str>) {
    let mut config = json!({"version": 2, "activeVault": vault, "mode": "plain"});
    if let Some(state) = state {
        config["publication"] = json!({
            "vaultToken": memocampus_native_host::active_vault::vault_token(vault).unwrap(),
            "state": state,
        });
    }
    fs::write(app.join("config.json"), config.to_string()).unwrap();
}

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
        let dir = std::env::temp_dir().join(format!("memocampus-http-{}", uuid::Uuid::new_v4()));
        let app = dir.join("app");
        fs::create_dir_all(&app).unwrap();
        let vault = dir.join("vault.sqlite3");
        Connection::open(&vault)
            .unwrap()
            .execute_batch(&schema::create_sql())
            .unwrap();
        publish(&app, &vault, Some("confirmed"));
        let env = Self { dir, vault, app };
        let store = PolicyStore::new(env.app.clone()).unwrap();
        store.initialize().unwrap();
        let current = store.load().unwrap();
        let mut next = current.policy.clone();
        next.layers.never_save.rules.push("never.test".into());
        next.layers.save_but_no_mcp.rules.push("nomcp.test".into());
        store.update(&current, next).unwrap();
        env
    }

    fn seed(&self) {
        let db = Connection::open(&self.vault).unwrap();
        let visits = [
            (
                "p1",
                "https://public.test/a",
                "Public A",
                "2026-11-05T01:00:00.000Z",
            ),
            (
                "p2",
                "https://public.test/b",
                "Public B",
                "2026-11-05T02:00:00.000Z",
            ),
            (
                "p3",
                "https://other.test/c",
                "Other C",
                "2026-11-04T03:00:00.000Z",
            ),
            (
                "n1",
                "https://never.test/CANARY",
                "CANARY never",
                "2026-11-05T04:00:00.000Z",
            ),
            (
                "m1",
                "https://nomcp.test/x",
                "CANARY nomcp",
                "2026-11-05T05:00:00.000Z",
            ),
        ];
        for (id, url, title, at) in visits {
            db.execute(
                "INSERT INTO visits VALUES (?1, ?2, ?3, ?4)",
                params![id, url, title, at],
            )
            .unwrap();
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(EXE);
        command
            .arg("--vault")
            .arg(&self.vault)
            .args(args)
            .env("MEMOCAMPUS_APP_DIR", &self.app)
            .env("LOCALAPPDATA", self.dir.join("localappdata"))
            .env_remove("MEMOCAMPUS_DB")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// As `command`, without `--vault`: following the active vault (Issue #51).
    fn command_following(&self, args: &[&str]) -> Command {
        let mut command = Command::new(EXE);
        command
            .args(args)
            .env("MEMOCAMPUS_APP_DIR", &self.app)
            .env("LOCALAPPDATA", self.dir.join("localappdata"))
            .env_remove("MEMOCAMPUS_DB")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn token(&self) -> String {
        fs::read_to_string(self.app.join("mcp-http-token")).unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.app.join("logs").join("mcp-startup.log")).unwrap_or_default()
    }
}

/// A running HTTP server.
struct Server {
    child: Child,
    port: u16,
    stderr: mpsc::Receiver<String>,
    seen: Vec<String>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn lines_of(stderr: ChildStderr) -> mpsc::Receiver<String> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if send.send(line).is_err() {
                break;
            }
        }
    });
    receive
}

impl Server {
    fn start(env: &Env, listen: &str) -> Self {
        Self::start_with(env, listen, &[])
    }

    fn start_with(env: &Env, listen: &str, extra: &[&str]) -> Self {
        let mut args = vec!["--transport", "streamable-http", "--listen", listen];
        args.extend_from_slice(extra);
        let mut command = env.command(&args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Its own process group, so a Ctrl+Break can be sent to it alone.
            command.creation_flags(0x0000_0200);
        }
        let mut child = command.spawn().unwrap();
        let stderr = lines_of(child.stderr.take().unwrap());
        let mut seen = Vec::new();
        let port = loop {
            let line = stderr
                .recv_timeout(WAIT)
                .expect("the server reports its address");
            seen.push(line.clone());
            if let Some(rest) = line.strip_prefix("memocampus-mcp: listening on http://127.0.0.1:")
            {
                break rest.strip_suffix("/mcp").unwrap().parse().unwrap();
            }
        };
        Self {
            child,
            port,
            stderr,
            seen,
        }
    }

    /// Asks the server to stop as Ctrl+Break would, and waits for its exit; returns its stderr.
    #[cfg(windows)]
    fn stop(mut self) -> (std::process::ExitStatus, Vec<String>) {
        use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
        // SAFETY: sends a console control event to the server's own process group.
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.child.id()) };
        assert_ne!(sent, 0, "{}", std::io::Error::last_os_error());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < WAIT, "the server stops when asked");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut lines = std::mem::take(&mut self.seen);
        while let Ok(line) = self.stderr.recv_timeout(Duration::from_millis(500)) {
            lines.push(line);
        }
        (status, lines)
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("JSON body: {}", self.body))
    }
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = body.windows(2).position(|w| w == b"\r\n").unwrap();
        let size =
            usize::from_str_radix(std::str::from_utf8(&body[..line_end]).unwrap().trim(), 16)
                .unwrap();
        body = &body[line_end + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// One HTTP/1.1 exchange on a new connection. `headers` replace the defaults of the same name;
/// a value of `""` removes one.
fn raw(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut all: Vec<(String, String)> = vec![
        ("Host".into(), format!("127.0.0.1:{port}")),
        ("Content-Type".into(), "application/json".into()),
        (
            "Accept".into(),
            "application/json, text/event-stream".into(),
        ),
    ];
    for (name, value) in headers {
        all.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        if !value.is_empty() {
            all.push(((*name).into(), (*value).into()));
        }
    }
    let mut request = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in &all {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: Vec<(String, String)> = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.trim().to_owned(), value.trim().to_owned())
        })
        .collect();
    let mut body = bytes[split + 4..].to_vec();
    if headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"))
    {
        body = dechunk(&body);
    }
    Reply {
        status,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

/// An authorized MCP POST at the legacy (initialize-based) version.
fn post(server: &Server, token: &str, body: &Value) -> Reply {
    let auth = format!("Bearer {token}");
    raw(
        server.port,
        "POST",
        "/mcp",
        &[("Authorization", &auth), ("MCP-Protocol-Version", LEGACY)],
        &body.to_string(),
    )
}

fn call_body(id: u64, name: &str, arguments: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

/// The same calls over a stdio session, for comparison.
fn stdio_answers(env: &Env, requests: &[Value]) -> Vec<Value> {
    let mut command = env.command(&[]);
    let mut child = command.stdin(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut send = |value: &Value| {
        stdin.write_all(value.to_string().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    };
    send(
        &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": LEGACY, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
    );
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let mut answers = Vec::new();
    for request in requests {
        send(request);
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        answers.push(serde_json::from_str(&line).unwrap());
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    answers
}

/// IDs and cursors are keyed per process, so two processes give different ones for the same row.
fn without_process_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !matches!(key.as_str(), "id" | "next_cursor"))
                .map(|(key, value)| (key.clone(), without_process_keys(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_process_keys).collect()),
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            // The text content mirrors the structured content.
            Ok(inner @ Value::Object(_)) => Value::String(without_process_keys(&inner).to_string()),
            _ => value.clone(),
        },
        other => other.clone(),
    }
}

// --- Compatibility ----------------------------------------------------------------------------

#[test]
fn every_tool_answers_the_same_over_http_as_over_stdio() {
    let env = Env::new();
    env.seed();
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        call_body(2, "memocampus_search_visits", &json!({"query": "public"})),
        call_body(
            3,
            "memocampus_search_visits",
            &json!({"query": "", "limit": 1}),
        ),
        call_body(
            4,
            "memocampus_get_visits_by_day",
            &json!({"date": "2026-11-05", "utc_offset": "+00:00"}),
        ),
        call_body(5, "memocampus_get_domain_stats", &json!({"limit": 1})),
        call_body(6, "memocampus_get_domain_stats", &json!({})),
        call_body(7, "memocampus_search_images", &json!({})),
        call_body(
            8,
            "memocampus_search_visits",
            &json!({"query": "x", "limit": 0}),
        ),
        call_body(
            9,
            "memocampus_search_visits",
            &json!({"query": "x", "cursor": "CANARY"}),
        ),
        call_body(10, "not_a_tool", &json!({})),
    ];
    let over_stdio = stdio_answers(&env, &requests);
    let server = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    let mut over_http = Vec::new();
    for request in &requests {
        let reply = post(&server, &token, request);
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.header("content-type"), Some("application/json"));
        assert_eq!(
            reply.header("cache-control"),
            Some("no-store"),
            "history is not cached"
        );
        over_http.push(reply.json());
    }
    assert_eq!(over_http.len(), over_stdio.len());
    for (http, stdio) in over_http.iter().zip(&over_stdio) {
        assert_eq!(without_process_keys(http), without_process_keys(stdio));
    }
    let tools: Vec<&str> = over_http[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools.len(), 4, "{tools:?}");
    // Private rows are absent from HTTP answers and counts exactly as over stdio.
    let domains = &over_http[5]["result"]["structuredContent"];
    assert_eq!(domains["total_visits"], 3);
    assert_eq!(domains["total_domains"], 2);
    for answer in &over_http {
        assert!(!answer.to_string().contains("CANARY"), "{answer}");
        assert!(!answer.to_string().contains("never.test"), "{answer}");
        assert!(!answer.to_string().contains("nomcp.test"), "{answer}");
    }
    assert_eq!(over_http[8]["result"]["isError"], true, "an invalid cursor");
    assert_eq!(
        over_http[9]["error"]["code"], -32602,
        "an unknown tool, as over stdio"
    );
}

#[test]
fn a_cursor_from_one_http_request_continues_in_the_next() {
    let env = Env::new();
    env.seed();
    let server = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    let first = post(
        &server,
        &token,
        &call_body(
            1,
            "memocampus_search_visits",
            &json!({"query": "", "limit": 2}),
        ),
    );
    let first = &first.json()["result"]["structuredContent"];
    assert_eq!(
        (first["total"].clone(), first["has_more"].clone()),
        (json!(3), json!(true))
    );
    let cursor = first["next_cursor"].as_str().unwrap();
    let rest = post(
        &server,
        &token,
        &call_body(
            2,
            "memocampus_search_visits",
            &json!({"query": "", "limit": 2, "cursor": cursor}),
        ),
    );
    let rest = &rest.json()["result"]["structuredContent"];
    assert_eq!(rest["visits"].as_array().unwrap().len(), 1);
    assert_eq!(rest["has_more"], false);
}

// --- Protocol ---------------------------------------------------------------------------------

#[test]
fn the_legacy_lifecycle_is_served_statelessly_without_sessions() {
    let env = Env::new();
    let server = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    let auth = format!("Bearer {token}");
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": LEGACY, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}});
    let reply = raw(
        server.port,
        "POST",
        "/mcp",
        &[("Authorization", &auth)],
        &init.to_string(),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["result"]["protocolVersion"], LEGACY);
    assert_eq!(
        reply.json()["result"]["serverInfo"]["name"],
        "memocampus-mcp"
    );
    assert_eq!(reply.header("mcp-session-id"), None, "no session is minted");
    for older in ["2025-06-18", "2025-03-26"] {
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": older, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}});
        let reply = raw(
            server.port,
            "POST",
            "/mcp",
            &[("Authorization", &auth)],
            &init.to_string(),
        );
        assert_eq!(reply.json()["result"]["protocolVersion"], older);
    }
    let note = post(
        &server,
        &token,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    assert_eq!((note.status, note.body.as_str()), (202, ""));
    // GET (no standalone stream) and DELETE (no session) are not offered.
    for method in ["GET", "DELETE"] {
        let reply = raw(
            server.port,
            method,
            "/mcp",
            &[("Authorization", &auth), ("Accept", "text/event-stream")],
            "",
        );
        assert_eq!(reply.status, 405, "{method}");
        assert_eq!(reply.header("allow"), Some("POST"), "{method}");
    }
    // A session ID a client sends is ignored, not honoured or rejected.
    let reply = raw(
        server.port,
        "POST",
        "/mcp",
        &[
            ("Authorization", &auth),
            ("MCP-Protocol-Version", LEGACY),
            ("Mcp-Session-Id", "made-up"),
        ],
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string(),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.header("mcp-session-id"), None);
}

#[test]
fn the_2026_07_28_requests_carry_their_own_version_and_are_checked_against_their_headers() {
    let env = Env::new();
    env.seed();
    let server = Server::start(&env, "127.0.0.1:0");
    let auth = format!("Bearer {}", env.token());
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    let modern = |method: &str, name: Option<&str>, body: &Value| {
        let mut headers = vec![
            ("Authorization", auth.as_str()),
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", method),
        ];
        if let Some(name) = name {
            headers.push(("Mcp-Name", name));
        }
        raw(server.port, "POST", "/mcp", &headers, &body.to_string())
    };
    let discover = modern(
        "server/discover",
        None,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": meta}}),
    );
    assert_eq!(discover.status, 200, "{}", discover.body);
    let versions = discover.json()["result"]["supportedVersions"].clone();
    assert!(
        versions.as_array().unwrap().contains(&json!(MODERN)),
        "{versions}"
    );
    assert!(
        versions.as_array().unwrap().contains(&json!(LEGACY)),
        "{versions}"
    );

    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "memocampus_get_domain_stats", "arguments": {"limit": 1}, "_meta": meta}});
    let reply = modern("tools/call", Some("memocampus_get_domain_stats"), &call);
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json()["result"]["structuredContent"]["total_visits"],
        3
    );

    let mismatch = modern("tools/call", Some("memocampus_search_visits"), &call);
    assert_eq!(mismatch.status, 400);
    assert_eq!(mismatch.json()["error"]["code"], -32020, "HeaderMismatch");
    let missing = modern("tools/call", None, &call);
    assert_eq!(missing.status, 400);
    assert_eq!(missing.json()["error"]["code"], -32020);

    let unknown =
        json!({"jsonrpc": "2.0", "id": 3, "method": "nope/nope", "params": {"_meta": meta}});
    let reply = modern("nope/nope", None, &unknown);
    assert_eq!(reply.status, 404, "{}", reply.body);
    assert_eq!(reply.json()["error"]["code"], -32601);
}

#[test]
fn malformed_requests_are_refused_and_leave_the_server_working() {
    let env = Env::new();
    env.seed();
    let server = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    let auth = format!("Bearer {token}");
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    let a = auth.as_str();
    for (headers, body, status) in [
        (
            vec![("Authorization", a), ("Content-Type", "text/plain")],
            list.as_str(),
            415,
        ),
        (
            vec![("Authorization", a), ("Content-Type", "")],
            list.as_str(),
            415,
        ),
        (
            vec![("Authorization", a), ("Accept", "application/json")],
            list.as_str(),
            406,
        ),
        (
            vec![("Authorization", a), ("Accept", "")],
            list.as_str(),
            406,
        ),
        (
            vec![("Authorization", a), ("MCP-Protocol-Version", "1999-01-01")],
            list.as_str(),
            400,
        ),
        (
            vec![("Authorization", a), ("MCP-Protocol-Version", LEGACY)],
            "{not json",
            415,
        ),
        (
            vec![("Authorization", a), ("MCP-Protocol-Version", LEGACY)],
            "[]",
            415,
        ),
    ] {
        let reply = raw(server.port, "POST", "/mcp", &headers, body);
        assert_eq!(reply.status, status, "{headers:?} {body}: {}", reply.body);
    }
    let big = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{{\"x\":\"{}\"}}}}",
        "a".repeat(100_000)
    );
    let reply = raw(
        server.port,
        "POST",
        "/mcp",
        &[("Authorization", a), ("MCP-Protocol-Version", LEGACY)],
        &big,
    );
    assert_eq!(reply.status, 413);
    let unknown = post(
        &server,
        &token,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "nope/nope"}),
    );
    assert_eq!(unknown.json()["error"]["code"], -32601);
    let bad = post(
        &server,
        &token,
        &call_body(3, "memocampus_search_visits", &json!({"limit": -1})),
    );
    assert_eq!(bad.json()["result"]["isError"], true);
    let unknown_field = post(
        &server,
        &token,
        &call_body(
            4,
            "memocampus_search_visits",
            &json!({"query": "", "sql": "x"}),
        ),
    );
    assert_eq!(unknown_field.json()["result"]["isError"], true);
    let ok = post(
        &server,
        &token,
        &call_body(5, "memocampus_get_domain_stats", &json!({})),
    );
    assert_eq!(ok.json()["result"]["structuredContent"]["total_visits"], 3);
}

// --- Security ---------------------------------------------------------------------------------

#[test]
fn only_the_bearer_token_from_the_token_file_opens_the_endpoint() {
    let env = Env::new();
    env.seed();
    let server = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    assert_eq!(token.len(), 64);
    assert!(token
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    let call = call_body(1, "memocampus_search_visits", &json!({"query": ""})).to_string();
    let wrong = format!("Bearer {}", "0".repeat(64));
    let short = format!("Bearer {}", &token[..63]);
    let basic = format!("Basic {token}");
    let bare = token.clone();
    let lower = format!("bearer {token}");
    for auth in [
        "",
        wrong.as_str(),
        short.as_str(),
        basic.as_str(),
        bare.as_str(),
        "Bearer ",
    ] {
        let reply = raw(
            server.port,
            "POST",
            "/mcp",
            &[("Authorization", auth), ("MCP-Protocol-Version", LEGACY)],
            &call,
        );
        assert_eq!(reply.status, 401, "{auth:?}");
        assert_eq!(
            reply.header("www-authenticate"),
            Some("Bearer realm=\"memocampus\"")
        );
        assert!(
            !reply.body.contains("public.test"),
            "nothing is read before the token is checked"
        );
    }
    // Two Authorization headers, one of them right, in either order: refused.
    let good = format!("Bearer {token}");
    for (first, second) in [(&wrong, &good), (&good, &wrong), (&good, &good)] {
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: {LEGACY}\r\nAuthorization: {first}\r\nAuthorization: {second}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{call}",
            server.port,
            call.len()
        );
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        assert!(text.starts_with("HTTP/1.1 401"), "{text}");
    }
    // The token in the URL is never read: the path with a query does not exist.
    let in_query = raw(
        server.port,
        "POST",
        &format!("/mcp?access_token={token}"),
        &[],
        &call,
    );
    assert_eq!(in_query.status, 404);
    let in_query = raw(
        server.port,
        "POST",
        &format!("/mcp?access_token={token}"),
        &[("Authorization", &good)],
        &call,
    );
    assert_eq!(in_query.status, 404, "no query string at all");
    for path in ["/", "/mcp/", "/MCP", "/sse", "/messages"] {
        assert_eq!(
            raw(
                server.port,
                "POST",
                path,
                &[("Authorization", &good)],
                &call
            )
            .status,
            404,
            "{path}"
        );
    }
    let ok = raw(
        server.port,
        "POST",
        "/mcp",
        &[("Authorization", &lower), ("MCP-Protocol-Version", LEGACY)],
        &call,
    );
    assert_eq!(ok.status, 200, "the scheme is case-insensitive");
    assert_eq!(ok.json()["result"]["structuredContent"]["total"], 3);
    // The token is in no output and no log line.
    for text in server
        .seen
        .iter()
        .map(String::as_str)
        .chain([env.log().as_str()])
    {
        assert!(!text.contains(&token), "{text}");
    }
    assert!(env.log().contains("\"outcome\":\"unauthorized\""));
}

#[test]
fn origins_and_foreign_hosts_are_refused_before_the_token_is_looked_at() {
    let env = Env::new();
    let server = Server::start(&env, "127.0.0.1:0");
    let good = format!("Bearer {}", env.token());
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    let port = server.port;
    let own = format!("http://127.0.0.1:{port}");
    let localhost_origin = format!("http://localhost:{port}");
    for origin in [
        "http://evil.test",
        own.as_str(),
        localhost_origin.as_str(),
        "null",
        "https://chatgpt.com",
    ] {
        for auth in ["", good.as_str()] {
            let reply = raw(
                port,
                "POST",
                "/mcp",
                &[
                    ("Authorization", auth),
                    ("Origin", origin),
                    ("MCP-Protocol-Version", LEGACY),
                ],
                &list,
            );
            assert_eq!(reply.status, 403, "{origin} {auth:?}");
            assert_eq!(reply.header("access-control-allow-origin"), None);
            assert_eq!(reply.json()["error"]["code"], -32600);
        }
    }
    // A CORS preflight gets no CORS grant.
    let preflight = raw(
        port,
        "OPTIONS",
        "/mcp",
        &[
            ("Origin", "http://evil.test"),
            ("Access-Control-Request-Method", "POST"),
        ],
        "",
    );
    assert_eq!(preflight.status, 403);
    assert_eq!(preflight.header("access-control-allow-origin"), None);
    let rebinding = format!("evil.test:{port}");
    let other_port = format!("127.0.0.1:{}", port.wrapping_add(1));
    let localhost = format!("localhost:{port}");
    let upper = format!("LOCALHOST:{port}");
    let leading_zero = format!("127.0.0.1:0{port}");
    for (host, status) in [
        (rebinding.as_str(), 403),
        ("127.0.0.1", 403),
        (other_port.as_str(), 403),
        (leading_zero.as_str(), 403),
        ("", 403),
        (localhost.as_str(), 200),
        (upper.as_str(), 200),
    ] {
        let reply = raw(
            port,
            "POST",
            "/mcp",
            &[
                ("Host", host),
                ("Authorization", &good),
                ("MCP-Protocol-Version", LEGACY),
            ],
            &list,
        );
        assert_eq!(reply.status, status, "Host {host:?}: {}", reply.body);
    }
}

#[test]
fn only_loopback_addresses_are_listened_on_and_a_used_port_is_a_clear_error() {
    let env = Env::new();
    for listen in [
        "0.0.0.0:0",
        "[::]:0",
        "192.168.0.10:8765",
        "localhost:8765",
        "127.0.0.2:8765",
    ] {
        let output = env
            .command(&["--transport", "streamable-http", "--listen", listen])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{listen}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim(),
            "memocampus-mcp: --listen must be 127.0.0.1:<port> or [::1]:<port>",
            "{listen}"
        );
    }
    let output = env.command(&["--listen", "127.0.0.1:0"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2), "HTTP is never implied");
    let output = env.command(&["--transport", "http"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));

    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let listen = format!("127.0.0.1:{port}");
    let output = env
        .command(&["--transport", "streamable-http", "--listen", &listen])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("memocampus-mcp: cannot listen: address_in_use"),);
    assert!(
        env.log().contains("\"error\":\"address_in_use\""),
        "{}",
        env.log()
    );
    drop(taken);

    // A running server holds its port exclusively.
    let server = Server::start(&env, "127.0.0.1:0");
    let listen = format!("127.0.0.1:{}", server.port);
    let output = env
        .command(&["--transport", "streamable-http", "--listen", &listen])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(std::net::TcpListener::bind(("127.0.0.1", server.port)).is_err());
}

#[test]
fn an_unusable_token_file_refuses_the_start_and_is_never_rewritten() {
    let env = Env::new();
    fs::write(env.app.join("mcp-http-token"), "not a token").unwrap();
    let output = env
        .command(&["--transport", "streamable-http", "--listen", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "memocampus-mcp: cannot start: token_file_invalid"
    );
    assert_eq!(
        fs::read_to_string(env.app.join("mcp-http-token")).unwrap(),
        "not a token"
    );
    // stdio needs no token and is unaffected.
    let answers = stdio_answers(
        &env,
        &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})],
    );
    assert_eq!(answers[0]["result"]["tools"].as_array().unwrap().len(), 4);
}

// --- Concurrency and lifecycle ----------------------------------------------------------------

#[test]
fn concurrent_requests_are_all_answered_while_the_host_writes() {
    let env = Env::new();
    env.seed();
    let server = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    let port = server.port;
    let writer = {
        let vault = env.vault.clone();
        std::thread::spawn(move || {
            let db = Connection::open(&vault).unwrap();
            db.busy_timeout(Duration::from_secs(3)).unwrap();
            for n in 0..50 {
                db.execute(
                    "INSERT INTO visits VALUES (?1, ?2, 'W', '2026-11-05T06:00:00.000Z')",
                    params![format!("w{n}"), format!("https://written.test/{n}")],
                )
                .unwrap();
            }
        })
    };
    let workers: Vec<_> = (0..12)
        .map(|n| {
            let token = token.clone();
            std::thread::spawn(move || {
                let auth = format!("Bearer {token}");
                let body = call_body(n, "memocampus_get_domain_stats", &json!({}));
                let reply = raw(
                    port,
                    "POST",
                    "/mcp",
                    &[("Authorization", &auth), ("MCP-Protocol-Version", LEGACY)],
                    &body.to_string(),
                );
                assert_eq!(reply.status, 200, "{}", reply.body);
                let reply = reply.json();
                assert_eq!(reply["id"], n);
                reply["result"]["structuredContent"]["total_visits"]
                    .as_u64()
                    .unwrap()
            })
        })
        .collect();
    for worker in workers {
        let total = worker.join().unwrap();
        assert!((3..=53).contains(&total), "{total}");
    }
    writer.join().unwrap();
    let reply = post(
        &server,
        &token,
        &call_body(99, "memocampus_get_domain_stats", &json!({})),
    );
    assert_eq!(
        reply.json()["result"]["structuredContent"]["total_visits"],
        53
    );
    let check: String = Connection::open(&env.vault)
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
}

#[cfg(windows)]
#[test]
fn the_server_stops_on_ctrl_break_and_starts_again_on_the_same_port_with_the_same_token() {
    let env = Env::new();
    env.seed();
    let server = Server::start(&env, "127.0.0.1:0");
    let port = server.port;
    let token = env.token();
    assert!(server
        .seen
        .iter()
        .any(|line| line.contains("bearer token created in")));
    let ok = post(
        &server,
        &token,
        &call_body(1, "memocampus_get_domain_stats", &json!({"limit": 1})),
    );
    assert_eq!(ok.status, 200);
    let pid = server.child.id();
    let (status, stderr) = server.stop();
    assert!(status.success(), "{status}");
    assert_eq!(
        stderr.last().map(String::as_str),
        Some("memocampus-mcp: stopped (ctrl_break)")
    );
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the port is closed"
    );
    let log = env.log();
    let events: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["pid"] == json!(pid))
        .collect();
    let names: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    for expected in [
        "http_token_ready",
        "http_listening",
        "http_request",
        "http_shutdown",
        "process_exit",
    ] {
        assert!(names.contains(&expected), "{names:?}");
    }
    assert_eq!(events.last().unwrap()["reason"], "ctrl_break");
    for text in stderr.iter().map(String::as_str).chain([log.as_str()]) {
        assert!(!text.contains(&token));
        assert!(!text.contains("public.test"), "{text}");
        assert!(
            !text.contains(env.vault.to_string_lossy().as_ref()),
            "{text}"
        );
    }

    let again = Server::start(&env, &format!("127.0.0.1:{port}"));
    assert_eq!(again.port, port);
    assert!(again
        .seen
        .iter()
        .any(|line| line.contains("bearer token read from")));
    assert_eq!(env.token(), token, "the token is kept across restarts");
    let ok = post(
        &again,
        &token,
        &call_body(2, "memocampus_get_domain_stats", &json!({"limit": 1})),
    );
    assert_eq!(ok.status, 200);
    let (status, _) = again.stop();
    assert!(status.success());
}

// --- Extra Host (a TLS proxy in front, e.g. Tailscale Serve) ---------------------------------

const TS_HOST: &str = "i137.taild78809.ts.net:8443";

#[test]
fn an_explicitly_allowed_host_is_accepted_and_still_needs_the_token() {
    let env = Env::new();
    env.seed();
    // By default the proxy's Host is refused, as before.
    let plain = Server::start(&env, "127.0.0.1:0");
    let good = format!("Bearer {}", env.token());
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    let reply = raw(
        plain.port,
        "POST",
        "/mcp",
        &[
            ("Host", TS_HOST),
            ("Authorization", &good),
            ("MCP-Protocol-Version", LEGACY),
        ],
        &list,
    );
    assert_eq!(reply.status, 403);
    drop(plain);

    let server = Server::start_with(&env, "127.0.0.1:0", &["--allow-host", TS_HOST]);
    assert!(server.seen.iter().any(|line| line
        == &format!("memocampus-mcp: also accepting Host {TS_HOST} (from --allow-host)")));
    let port = server.port;
    let token = env.token();
    let good = format!("Bearer {token}");
    let wrong = format!("Bearer {}", "0".repeat(64));
    let headers = |host: &'static str, auth: &'static str| -> Vec<(&'static str, &'static str)> {
        vec![
            ("Host", host),
            ("Authorization", auth),
            ("MCP-Protocol-Version", LEGACY),
        ]
    };
    let good: &'static str = Box::leak(good.into_boxed_str());
    let wrong: &'static str = Box::leak(wrong.into_boxed_str());

    // Allowed Host without a token, or with a wrong one: 401, not 403.
    for auth in ["", wrong] {
        let reply = raw(port, "POST", "/mcp", &headers(TS_HOST, auth), &list);
        assert_eq!(reply.status, 401, "{auth:?}");
        assert_eq!(
            reply.header("www-authenticate"),
            Some("Bearer realm=\"memocampus\"")
        );
    }
    // Allowed Host and the token: initialize and tools/list over the proxy's Host.
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": LEGACY, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}});
    let reply = raw(
        port,
        "POST",
        "/mcp",
        &[("Host", TS_HOST), ("Authorization", good)],
        &init.to_string(),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json()["result"]["serverInfo"]["name"],
        "memocampus-mcp"
    );
    let reply = raw(port, "POST", "/mcp", &headers(TS_HOST, good), &list);
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["result"]["tools"].as_array().unwrap().len(), 4);
    let reply = raw(
        port,
        "POST",
        "/mcp",
        &headers("I137.TAILD78809.TS.NET:8443", good),
        &list,
    );
    assert_eq!(reply.status, 200, "names are case-insensitive");
    let call = call_body(2, "memocampus_get_domain_stats", &json!({}));
    let reply = raw(
        port,
        "POST",
        "/mcp",
        &headers(TS_HOST, good),
        &call.to_string(),
    );
    assert_eq!(
        reply.json()["result"]["structuredContent"]["total_visits"],
        3,
        "the same policy gate"
    );
    // Loopback keeps working.
    let loopback: &'static str = Box::leak(format!("127.0.0.1:{port}").into_boxed_str());
    assert_eq!(
        raw(port, "POST", "/mcp", &headers(loopback, good), &list).status,
        200
    );

    // Only that exact name and port.
    let own_port: &'static str =
        Box::leak(format!("i137.taild78809.ts.net:{port}").into_boxed_str());
    for host in [
        "i137.taild78809.ts.net",
        "i137.taild78809.ts.net:443",
        "i137.taild78809.ts.net:8444",
        own_port,
        "other.taild78809.ts.net:8443",
        "x.i137.taild78809.ts.net:8443",
        "taild78809.ts.net:8443",
        "i137.taild78809.ts.net.:8443",
        "i137.taild78809.ts.net:08443",
        "evil.test:8443",
        "*.ts.net:8443",
    ] {
        for auth in ["", good] {
            let reply = raw(port, "POST", "/mcp", &headers(host, auth), &list);
            assert!(
                matches!(reply.status, 400 | 403),
                "Host {host:?} {auth:?}: {}",
                reply.status
            );
            assert_ne!(reply.status, 401, "Host is checked before the token");
        }
    }
    // Two Host headers, even both allowed: refused.
    for second in [TS_HOST, loopback] {
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {TS_HOST}\r\nHost: {second}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: {LEGACY}\r\nAuthorization: {good}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{list}",
            list.len()
        );
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        assert!(
            text.starts_with("HTTP/1.1 400") || text.starts_with("HTTP/1.1 403"),
            "{text}"
        );
    }
    // Origin is refused as before, with the allowed Host and the token.
    for origin in [
        "https://i137.taild78809.ts.net:8443",
        "https://chatgpt.com",
        "null",
    ] {
        let mut with_origin = headers(TS_HOST, good);
        with_origin.push(("Origin", origin));
        let reply = raw(port, "POST", "/mcp", &with_origin, &list);
        assert_eq!(reply.status, 403, "{origin}");
    }
    // Nothing of the host name or token in the log; only how many extra hosts there are.
    let log = env.log();
    assert!(
        !log.contains("taild78809") && !log.contains(&token),
        "{log}"
    );
    assert!(log.contains("\"extra_hosts\":1"), "{log}");
}

#[test]
fn an_extra_host_that_is_not_one_exact_name_and_port_refuses_the_start() {
    let env = Env::new();
    let message = "memocampus-mcp: --allow-host must be one exact <dns name>:<port> (no wildcard, no IP address), each at most once, at most 8";
    for bad in [
        "*.ts.net:8443",
        "*",
        "*:8443",
        "i137.taild78809.ts.net",
        "i137.taild78809.ts.net:0",
        "i137.taild78809.ts.net:65536",
        "i137.taild78809.ts.net:abc",
        "https://i137.taild78809.ts.net:8443",
        "i137.taild78809.ts.net:8443/mcp",
        "host_name.ts.net:8443",
        "0.0.0.0:8443",
        "localhost:8443",
        "",
    ] {
        let output = env
            .command(&[
                "--transport",
                "streamable-http",
                "--listen",
                "127.0.0.1:0",
                "--allow-host",
                bad,
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{bad:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim(),
            message,
            "{bad:?}"
        );
    }
    let twice = env
        .command(&[
            "--transport",
            "streamable-http",
            "--listen",
            "127.0.0.1:0",
            "--allow-host",
            TS_HOST,
            "--allow-host",
            TS_HOST,
        ])
        .output()
        .unwrap();
    assert_eq!(twice.status.code(), Some(2));
    let stdio = env.command(&["--allow-host", TS_HOST]).output().unwrap();
    assert_eq!(stdio.status.code(), Some(2), "never with stdio");
    assert_eq!(
        String::from_utf8(stdio.stderr).unwrap().trim(),
        "memocampus-mcp: --allow-host needs --transport streamable-http"
    );
    assert!(
        !env.app.join("mcp-http-token").exists(),
        "refused before anything is created"
    );
}

// --- Issue #51, Slice B0: the same gate on both transports -------------------------------------

/// `stdio_answers` for a server without `--vault`.
fn stdio_following(env: &Env, requests: &[Value]) -> Vec<Value> {
    let mut child = env
        .command_following(&[])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut send = |value: &Value| {
        stdin.write_all(value.to_string().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    };
    send(
        &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": LEGACY, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
    );
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let mut answers = Vec::new();
    for request in requests {
        send(request);
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        answers.push(serde_json::from_str(&line).unwrap());
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    answers
}

/// An HTTP server without `--vault`.
fn start_following(env: &Env) -> Server {
    let mut command =
        env.command_following(&["--transport", "streamable-http", "--listen", "127.0.0.1:0"]);
    let mut child = command.spawn().unwrap();
    let stderr = lines_of(child.stderr.take().unwrap());
    let mut seen = Vec::new();
    let port = loop {
        let line = stderr
            .recv_timeout(WAIT)
            .expect("the server reports its address");
        seen.push(line.clone());
        if let Some(rest) = line.strip_prefix("memocampus-mcp: listening on http://127.0.0.1:") {
            break rest.strip_suffix("/mcp").unwrap().parse().unwrap();
        }
    };
    Server {
        child,
        port,
        stderr,
        seen,
    }
}

/// The `{"error": code}` of each failed tool call.
fn tool_errors(answers: &[Value]) -> Vec<Value> {
    answers
        .iter()
        .map(|answer| {
            assert_eq!(answer["result"]["isError"], true, "{answer}");
            serde_json::from_str(answer["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
        })
        .collect()
}

#[test]
fn both_transports_follow_the_same_active_vault_and_refuse_alike() {
    let env = Env::new();
    env.seed();
    let b = env.dir.join("b.sqlite3");
    let db = Connection::open(&b).unwrap();
    db.execute_batch(&schema::create_sql()).unwrap();
    db.execute(
        "INSERT INTO visits VALUES ('b1', 'https://b.public.test/', 'B', '2026-11-05T00:00:00.000Z')",
        [],
    )
    .unwrap();
    drop(db);
    let requests = vec![
        call_body(1, "memocampus_search_visits", &json!({"query": ""})),
        call_body(2, "memocampus_get_domain_stats", &json!({})),
        call_body(3, "memocampus_search_images", &json!({})),
        call_body(
            4,
            "memocampus_get_visits_by_day",
            &json!({"date": "2026-11-05", "utc_offset": "+00:00"}),
        ),
    ];
    // Long-running HTTP servers, started while A is the active, confirmed vault.
    let following = start_following(&env);
    let pinned = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    let over_http = |server: &Server| -> Vec<Value> {
        requests
            .iter()
            .map(|request| {
                let reply = post(server, &token, request);
                assert_eq!(reply.status, 200, "{}", reply.body);
                reply.json()
            })
            .collect()
    };
    let same = |left: &[Value], right: &[Value]| {
        assert_eq!(left.len(), right.len());
        for (left, right) in left.iter().zip(right) {
            assert_eq!(without_process_keys(left), without_process_keys(right));
        }
    };

    // A active and confirmed: the same answers everywhere.
    let a_stdio = stdio_following(&env, &requests);
    same(&over_http(&following), &a_stdio);
    same(&over_http(&pinned), &a_stdio);
    assert_eq!(a_stdio[1]["result"]["structuredContent"]["total_visits"], 3);

    // Switched to B and confirmed: the following servers read B, the one pinned to A nothing.
    publish(&env.app, &b, Some("confirmed"));
    let b_stdio = stdio_following(&env, &requests);
    same(&over_http(&following), &b_stdio);
    assert_eq!(b_stdio[1]["result"]["structuredContent"]["total_visits"], 1);
    for answer in tool_errors(&over_http(&pinned)) {
        assert_eq!(answer, json!({"error": "vault_not_active"}));
    }

    // B pending: both transports refuse every tool with the same code and no data.
    publish(&env.app, &b, Some("pending"));
    let http = tool_errors(&over_http(&following));
    assert_eq!(http.len(), requests.len());
    for answer in &http {
        assert_eq!(*answer, json!({"error": "vault_publication_pending"}));
    }
    // (A stdio server refuses to start in this state; see stdio_mcp.rs.)
    let stdio = env.command_following(&[]).output().unwrap();
    assert_eq!(stdio.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(stdio.stderr).unwrap().trim(),
        "memocampus-mcp: vault refused: vault_publication_pending"
    );
}
