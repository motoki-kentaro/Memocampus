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
        Self::seed_into(&Connection::open(&self.vault).unwrap());
    }

    fn seed_into(db: &Connection) {
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
        Self::spawn(env.command(&args))
    }

    /// Starts `command` (an HTTP server) and waits for it to report its address.
    fn spawn(mut command: Command) -> Self {
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
    fn stop(self) -> (std::process::ExitStatus, Vec<String>) {
        use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
        // SAFETY: sends a console control event to the server's own process group.
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.child.id()) };
        assert_ne!(sent, 0, "{}", std::io::Error::last_os_error());
        self.finish()
    }

    /// Waits for the server to end by itself (it was asked some other way); returns its stderr.
    fn finish(mut self) -> (std::process::ExitStatus, Vec<String>) {
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
    assert_eq!(tools.len(), 5, "{tools:?}");
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
    assert_eq!(answers[0]["result"]["tools"].as_array().unwrap().len(), 5);
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
    assert_eq!(reply.json()["result"]["tools"].as_array().unwrap().len(), 5);
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

// --- Issue #51 Slice B: an encrypted vault ------------------------------------------------------

/// Replaces the fixture's vault with an encrypted one holding the same rows, its key in the app
/// directory's key store (DPAPI, this Windows user), published to MCP.
fn encrypt(
    env: &Env,
) -> (
    memocampus_native_host::secure_vault::secret::VaultId,
    memocampus_native_host::secure_vault::secret::VaultKey,
) {
    use memocampus_native_host::secure_vault::secret::VaultKey;
    use memocampus_native_host::secure_vault::{cipher, key_store};
    fs::remove_file(&env.vault).unwrap();
    let key = VaultKey::generate().unwrap();
    let salt = memocampus_native_host::secure_vault::secret::CipherSalt::generate().unwrap();
    cipher::create(&env.vault, &key, &salt).unwrap();
    let id = memocampus_native_host::secure_vault::recovery::vault_id(&salt);
    key_store::store(&env.app, &id, &salt, &key, key_store::Write::New).unwrap();
    Env::seed_into(
        &cipher::open(
            &env.vault,
            &key,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .unwrap(),
    );
    let config = json!({
        "version": 2, "activeVault": env.vault, "mode": "encrypted", "vaultId": id.to_hex(),
        "publication": {
            "vaultToken": memocampus_native_host::active_vault::vault_token(&env.vault).unwrap(),
            "state": "confirmed",
        },
    });
    fs::write(env.app.join("config.json"), config.to_string()).unwrap();
    (id, key)
}

#[test]
fn an_encrypted_vault_answers_the_same_over_http_as_over_stdio_and_never_without_its_key() {
    let env = Env::new();
    let (id, key) = encrypt(&env);
    let bytes = fs::read(&env.vault).unwrap();
    assert!(
        !bytes.windows(6).any(|w| w == b"CANARY"),
        "encrypted at rest"
    );
    let requests = vec![
        call_body(1, "memocampus_search_visits", &json!({"query": ""})),
        call_body(2, "memocampus_get_domain_stats", &json!({})),
        call_body(
            3,
            "memocampus_get_visits_by_day",
            &json!({"date": "2026-11-05", "utc_offset": "+00:00"}),
        ),
        call_body(4, "memocampus_search_images", &json!({})),
    ];
    let over_stdio = stdio_answers(&env, &requests);
    let server = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    let over_http: Vec<Value> = requests
        .iter()
        .map(|request| post(&server, &token, request).json())
        .collect();
    for (http, stdio) in over_http.iter().zip(&over_stdio) {
        assert_eq!(without_process_keys(http), without_process_keys(stdio));
    }
    let domains = &over_http[1]["result"]["structuredContent"];
    assert_eq!(domains["total_visits"], 3, "the privacy gate applies");
    let all = format!("{over_http:?}{over_stdio:?}{}", env.log());
    for private in ["CANARY", "never.test", "nomcp.test"] {
        assert!(!all.contains(private), "{private}");
    }
    assert!(!key.appears_in(all.as_bytes()), "the key never leaves");
    assert!(!all.contains(&id.to_hex()), "nor the vault id");

    // Without this user's key: refused over HTTP at the next call, and a new server will not start.
    let entry = memocampus_native_host::secure_vault::key_store::entry_path(&env.app, &id);
    let saved = fs::read(&entry).unwrap();
    fs::remove_file(&entry).unwrap();
    let refused = post(&server, &token, &requests[0]).json();
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(refused.to_string().contains("vault_locked"), "{refused}");
    let output = env.command(&[]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("vault refused: vault_locked"));
    fs::write(&entry, saved).unwrap();
    assert_eq!(
        post(&server, &token, &requests[1]).json()["result"]["structuredContent"]["total_visits"],
        3
    );
    // Pending: refused, whatever the key.
    let mut config: Value =
        serde_json::from_slice(&fs::read(env.app.join("config.json")).unwrap()).unwrap();
    config["publication"]["state"] = "pending".into();
    fs::write(env.app.join("config.json"), config.to_string()).unwrap();
    let pending = post(&server, &token, &requests[0]).json();
    assert!(
        pending.to_string().contains("vault_publication_pending"),
        "{pending}"
    );
    assert_eq!(
        fs::read(&env.vault).unwrap(),
        bytes,
        "MCP never writes the vault"
    );
}

// --- Image thumbnails (Issue #61, Slice 2) ---------------------------------------------------------

fn png(width: u32, height: u32, len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0]);
    let mut at = 0u8;
    while bytes.len() < len {
        bytes.push(at);
        at = at.wrapping_add(37);
    }
    bytes
}

/// Public images saved from `https://<name>.public.test/`: a good thumbnail, none, a broken one,
/// the largest one, and one that is private (`mcp_public = 0`, with a thumbnail).
fn seed_thumbnails(env: &Env) {
    let db = Connection::open(&env.vault).unwrap();
    let cases: [(&str, i64, Option<Vec<u8>>, i64); 5] = [
        ("ok", 1, Some(png(320, 180, 1500)), 1),
        ("missing", 2, None, 1),
        ("broken", 3, Some(b"CANARY not an image".to_vec()), 1),
        ("limit", 4, Some(png(320, 180, 256 * 1024)), 1),
        ("private", 5, Some(png(64, 64, 700)), 0),
    ];
    for (name, n, thumbnail, public) in cases {
        db.execute(
            "INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, save_count, data)
             VALUES (printf('%064d', ?1), 'image/png', 27, 640, 360, 1, 9999999999999, 1, CAST('ORIGINAL-IMAGE-BYTES-CANARY' AS BLOB))",
            params![n],
        )
        .unwrap();
        let blob = db.last_insert_rowid();
        if let Some(data) = thumbnail {
            let (w, h) = if name == "private" {
                (64, 64)
            } else {
                (320, 180)
            };
            db.execute(
                "INSERT INTO image_thumbnails (blob_id, mime_type, width, height, generator, data) VALUES (?1, 'image/png', ?2, ?3, 1, ?4)",
                params![blob, w, h, data],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public) VALUES (?1, ?2, ?3, ?4, 1790000000000, 0, ?5)",
            params![
                format!("s-{name}"),
                blob,
                format!("https://{name}.public.test/image.png"),
                format!("https://{name}.public.test/"),
                public
            ],
        )
        .unwrap();
    }
}

/// A stdio session whose calls may depend on earlier answers (ids are keyed per process).
fn stdio_session<T>(env: &Env, run: impl FnOnce(&mut dyn FnMut(Value) -> Value) -> T) -> T {
    let mut command = env.command(&[]);
    let mut child = command.stdin(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut send = |value: Value| -> Value {
        stdin.write_all(value.to_string().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        if value.get("id").is_none() {
            return Value::Null;
        }
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    send(
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": LEGACY, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
    );
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let value = run(&mut send);
    drop(stdin);
    assert!(child.wait().unwrap().success());
    value
}

/// What one transport answers for the thumbnail of every case, in a fixed order.
fn thumbnail_answers(call: &mut dyn FnMut(Value) -> Value) -> Vec<Value> {
    let mut next = 100;
    let mut tool = |call: &mut dyn FnMut(Value) -> Value, name: &str, arguments: Value| {
        next += 1;
        call(call_body(next, name, &arguments))
    };
    let mut ids = Vec::new();
    for name in ["ok", "missing", "broken", "limit", "private"] {
        let list = tool(
            call,
            "memocampus_search_images",
            json!({"query": format!("{name}.public.test")}),
        );
        ids.push(list["result"]["structuredContent"]["images"][0]["id"].clone());
    }
    assert_eq!(
        ids[4],
        Value::Null,
        "a private image is not found by search"
    );
    let unknown = json!("image_0123456789abcdef0123456789abcdef");
    let mut answers = Vec::new();
    for id in [
        ids[0].clone(),
        ids[1].clone(),
        ids[2].clone(),
        ids[3].clone(),
        unknown,
        json!("not an id"),
        json!(""),
    ] {
        answers.push(tool(
            call,
            "memocampus_get_image_thumbnail",
            json!({"image_id": id}),
        ));
    }
    answers
}

#[test]
fn the_thumbnail_tool_is_listed_and_answers_the_same_over_http_as_over_stdio() {
    let env = Env::new();
    seed_thumbnails(&env);
    let over_stdio = stdio_session(&env, |send| thumbnail_answers(send));
    let server = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    let mut http = |body: Value| {
        let reply = post(&server, &token, &body);
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    };
    let listed = http(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 5);
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "memocampus_get_image_thumbnail")
        .expect("listed over HTTP");
    assert_eq!(tool["inputSchema"]["required"], json!(["image_id"]));
    assert!(tool.get("outputSchema").is_none());
    let over_http = thumbnail_answers(&mut http);

    assert_eq!(over_http.len(), over_stdio.len());
    for (index, (h, s)) in over_http.iter().zip(&over_stdio).enumerate() {
        // The ids inside the metadata differ per process; everything else is identical.
        let strip = |answer: &Value| -> Value {
            let mut answer = answer["result"].clone();
            if let Some(text) = answer["content"][0]["text"].as_str() {
                if let Ok(mut info) = serde_json::from_str::<Value>(text) {
                    if info.get("image_id").is_some() {
                        info["image_id"] = json!("<id>");
                    }
                    answer["content"][0]["text"] = json!(info.to_string());
                }
            }
            answer
        };
        assert_eq!(strip(h), strip(s), "case {index}");
    }
    // ok: one text block and one image block; the bytes are the stored ones.
    let ok = &over_http[0]["result"];
    assert_ne!(ok["isError"], true, "{ok}");
    assert_eq!(ok["content"].as_array().unwrap().len(), 2);
    assert_eq!(ok["content"][1]["type"], "image");
    assert_eq!(ok["content"][1]["mimeType"], "image/png");
    assert!(ok.get("structuredContent").is_none());
    // Failures: fixed codes, one text block, no image.
    assert_ne!(
        over_http[3]["result"]["isError"], true,
        "the largest is sent"
    );
    assert_eq!(
        over_http[3]["result"]["content"].as_array().unwrap().len(),
        2
    );
    let errors: Vec<Value> = [1, 2, 4, 5, 6]
        .into_iter()
        .map(|index| {
            let result = &over_http[index]["result"];
            assert_eq!(result["isError"], true, "{result}");
            assert_eq!(result["content"].as_array().unwrap().len(), 1);
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
        })
        .collect();
    assert_eq!(
        errors,
        [
            json!({"error": "thumbnail_missing"}),
            json!({"error": "thumbnail_invalid"}),
            json!({"error": "not_found"}),
            json!({"error": "invalid_argument", "argument": "image_id"}),
            json!({"error": "invalid_argument", "argument": "image_id"}),
        ]
    );
    let all = serde_json::to_string(&over_http).unwrap();
    for needle in ["ORIGINAL", "CANARY", "never.test", "SELECT"] {
        assert!(!all.contains(needle), "{needle}");
    }
    // The wire size of the largest answer, as measured over HTTP.
    let largest = serde_json::to_string(&over_http[3]).unwrap().len();
    println!("HTTP largest thumbnail answer: {largest} bytes");
    assert!(largest <= 349_528 + 4 * 1024, "{largest}");
}

#[test]
fn the_thumbnail_tool_needs_the_token_and_a_local_origin_like_every_other_call() {
    let env = Env::new();
    seed_thumbnails(&env);
    let server = Server::start(&env, "127.0.0.1:0");
    let token = env.token();
    let list = post(
        &server,
        &token,
        &call_body(
            1,
            "memocampus_search_images",
            &json!({"query": "ok.public.test"}),
        ),
    )
    .json();
    let id = list["result"]["structuredContent"]["images"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let call = call_body(
        2,
        "memocampus_get_image_thumbnail",
        &json!({"image_id": id}),
    )
    .to_string();
    let good = format!("Bearer {token}");
    let wrong = format!("Bearer {}", "0".repeat(64));
    for auth in ["", wrong.as_str()] {
        let reply = raw(
            server.port,
            "POST",
            "/mcp",
            &[("Authorization", auth), ("MCP-Protocol-Version", LEGACY)],
            &call,
        );
        assert_eq!(reply.status, 401, "{auth:?}");
        assert!(!reply.body.contains("\"image\""), "{}", reply.body);
        assert!(!reply.body.contains("data"), "{}", reply.body);
    }
    for auth in ["", good.as_str()] {
        let reply = raw(
            server.port,
            "POST",
            "/mcp",
            &[
                ("Authorization", auth),
                ("Origin", "http://evil.test"),
                ("MCP-Protocol-Version", LEGACY),
            ],
            &call,
        );
        assert_eq!(reply.status, 403, "{auth:?}");
        assert!(!reply.body.contains("\"image\""), "{}", reply.body);
    }
    let reply = raw(
        server.port,
        "POST",
        "/mcp",
        &[("Authorization", &good), ("MCP-Protocol-Version", LEGACY)],
        &call,
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["result"]["content"][1]["type"], "image");
}

// --- The management channel (Issue #67, Slice 1) ---------------------------------------------------
//
// A supervisor (the Windows tray) that started the server holds the write end of an anonymous pipe
// whose read end the server inherited. These tests use the real `memocampus-mcp` process, synthetic
// vaults only, and never a process or port they did not start.

#[cfg(windows)]
mod management {
    use super::*;
    use memocampus_tray::settings::{Settings, VaultMode};
    use memocampus_tray::state::{
        ErrorKind, LastExit, RestartPolicy, ServerState, Snapshot, VaultStatus,
    };
    use memocampus_tray::supervisor::{Command as Order, Supervisor, Timings};
    use memocampus_tray::winproc::WinLauncher;
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    /// `(read handle value, write end)`. The read end is inheritable (the server gets it); the write
    /// end is not, so no child can ever hold it. With `inherit_write` both ends are inheritable: only
    /// for the test that hands the server the wrong end.
    fn pipe(inherit_write: bool) -> (usize, usize, File) {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: live out-pointers and a live attribute block; the handles are ours.
        unsafe {
            assert_ne!(CreatePipe(&mut read, &mut write, &attributes, 0), 0);
            if !inherit_write {
                assert_ne!(SetHandleInformation(write, HANDLE_FLAG_INHERIT, 0), 0);
            }
            (read as usize, write as usize, File::from_raw_handle(write))
        }
    }

    fn alive(pid: u32) -> bool {
        // SAFETY: a query about a process by id with the least access; the handle is closed.
        unsafe {
            let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if handle.is_null() {
                return false;
            }
            let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
            CloseHandle(handle);
            running
        }
    }

    fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        let until = Instant::now() + WAIT;
        while !condition() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn managed(env: &Env, handle: usize) -> Server {
        Server::spawn(env.command(&[
            "--transport",
            "streamable-http",
            "--listen",
            "127.0.0.1:0",
            "--management-pipe-handle",
            &handle.to_string(),
        ]))
    }

    /// Runs a managed server, lets `act` use the write end, and returns how the server ended.
    fn ended_by(
        env: &Env,
        act: impl FnOnce(File) -> Option<File>,
    ) -> (std::process::ExitStatus, Vec<String>, u32, u16, usize) {
        let (read, _, write) = pipe(false);
        let server = managed(env, read);
        let (pid, port) = (server.child.id(), server.port);
        let keep = act(write);
        let (status, stderr) = server.finish();
        drop(keep);
        (status, stderr, pid, port, read)
    }

    fn events_of(env: &Env, pid: u32) -> Vec<Value> {
        env.log()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|event| event["pid"] == json!(pid))
            .collect()
    }

    fn assert_ended_gracefully(
        env: &Env,
        (status, stderr, pid, port, handle): (
            std::process::ExitStatus,
            Vec<String>,
            u32,
            u16,
            usize,
        ),
        reason: &str,
    ) {
        assert!(status.success(), "{status}");
        assert_eq!(
            stderr.last().map(String::as_str),
            Some(format!("memocampus-mcp: stopped ({reason})").as_str()),
            "{stderr:?}"
        );
        // Through the same shutdown as Ctrl+C: the port is closed, the shutdown is logged with its reason.
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_err(),
            "the port is closed"
        );
        let events = events_of(env, pid);
        let names: Vec<&str> = events
            .iter()
            .map(|e| e["event"].as_str().unwrap())
            .collect();
        for expected in [
            "management_ipc_ready",
            "http_listening",
            "http_shutdown",
            "process_exit",
        ] {
            assert!(names.contains(&expected), "{names:?}");
        }
        let shutdown = events
            .iter()
            .find(|e| e["event"] == "http_shutdown")
            .unwrap();
        assert_eq!(shutdown["reason"], reason);
        assert_eq!(shutdown["ok"], true, "connections drained");
        // The handle value is a number in the server's handle table, not a secret, and still it is
        // in neither stderr nor the log.
        let value = handle.to_string();
        for text in stderr
            .iter()
            .map(String::as_str)
            .chain([env.log().as_str()])
        {
            assert!(
                !text.contains(&format!("management-pipe-handle {value}"))
                    && !text.contains(&format!("\"{value}\"")),
                "{text}"
            );
        }
    }

    #[test]
    fn a_stop_request_on_the_management_pipe_stops_the_server_through_the_normal_shutdown() {
        let env = Env::new();
        env.seed();
        let ended = ended_by(&env, |mut write| {
            write.write_all(b"S").unwrap();
            Some(write)
        });
        assert_ended_gracefully(&env, ended, "management_stop");
    }

    #[test]
    fn losing_the_supervisor_stops_the_server_too() {
        let env = Env::new();
        env.seed();
        // The write end closes with nothing written: the supervisor is gone.
        let ended = ended_by(&env, |write| {
            drop(write);
            None
        });
        assert_ended_gracefully(&env, ended, "management_closed");
    }

    #[test]
    fn anything_but_the_stop_byte_stops_the_server_as_invalid() {
        for byte in [b"X", b"s", b"\n"] {
            let env = Env::new();
            env.seed();
            let ended = ended_by(&env, |mut write| {
                write.write_all(byte).unwrap();
                Some(write)
            });
            assert_ended_gracefully(&env, ended, "management_invalid");
        }
    }

    #[test]
    fn a_handle_that_is_not_a_readable_management_pipe_refuses_the_start() {
        let env = Env::new();
        env.seed();
        // A value that is no handle of the server's, and the write end of a real pipe.
        let (_, write_value, _keep) = pipe(true);
        for value in ["99996", &write_value.to_string()] {
            let out = env
                .command(&[
                    "--transport",
                    "streamable-http",
                    "--listen",
                    "127.0.0.1:0",
                    "--management-pipe-handle",
                    value,
                ])
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(2), "{value}");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains("cannot start: management_not_a_pipe"),
                "{stderr}"
            );
            assert!(!stderr.contains("listening"), "nothing was bound: {stderr}");
            assert!(!stderr.contains(value), "the value is not echoed");
            assert!(out.stdout.is_empty());
        }
        assert!(env.log().contains("\"stage\":\"management\""));
    }

    #[test]
    fn the_management_flag_is_refused_without_http_and_when_malformed() {
        let env = Env::new();
        for (args, text) in [
            (
                vec!["--management-pipe-handle", "8"],
                "--management-pipe-handle needs --transport streamable-http",
            ),
            (
                vec!["--transport", "stdio", "--management-pipe-handle", "8"],
                "--management-pipe-handle needs --transport streamable-http",
            ),
            (
                vec![
                    "--transport",
                    "streamable-http",
                    "--management-pipe-handle",
                    "0x8",
                ],
                "--management-pipe-handle must be one decimal handle value",
            ),
        ] {
            let out = env.command(&args).output().unwrap();
            assert_eq!(out.status.code(), Some(2), "{args:?}");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.contains(text), "{stderr}");
            assert!(out.stdout.is_empty(), "stdout stays free for the protocol");
        }
    }

    #[test]
    fn a_managed_server_has_no_stop_tool_and_no_stop_endpoint() {
        let env = Env::new();
        env.seed();
        let (read, _, mut write) = pipe(false);
        let server = managed(&env, read);
        let token = env.token();
        let listed = post(
            &server,
            &token,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .json();
        let mut names: Vec<String> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "memocampus_get_domain_stats",
                "memocampus_get_image_thumbnail",
                "memocampus_get_visits_by_day",
                "memocampus_search_images",
                "memocampus_search_visits"
            ]
        );
        for tool in ["memocampus_stop", "stop", "shutdown", "management_stop"] {
            let reply = post(&server, &token, &call_body(2, tool, &json!({}))).json();
            assert!(
                reply.get("error").is_some() || reply["result"]["isError"] == true,
                "{tool}: {reply}"
            );
        }
        let auth = format!("Bearer {token}");
        for path in ["/stop", "/shutdown", "/management", "/mcp/stop", "/quit"] {
            let reply = raw(server.port, "POST", path, &[("Authorization", &auth)], "S");
            assert_eq!(reply.status, 404, "{path}");
            let reply = raw(server.port, "GET", path, &[("Authorization", &auth)], "");
            assert_eq!(reply.status, 404, "{path}");
        }
        let mut server = server;
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "none of that stopped it"
        );
        let ok = post(
            &server,
            &token,
            &call_body(3, "memocampus_get_domain_stats", &json!({"limit": 1})),
        );
        assert_eq!(ok.status, 200);
        write.write_all(b"S").unwrap();
        let (status, _) = server.finish();
        assert!(status.success());
    }

    #[test]
    fn without_the_flag_the_server_stops_as_before_and_ignores_pipes_it_was_not_given() {
        let env = Env::new();
        env.seed();
        // A pipe exists in this process and is inheritable, but the server was not told about it.
        let (_read, _, mut write) = pipe(false);
        let server = Server::start(&env, "127.0.0.1:0");
        write.write_all(b"S").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let mut server = server;
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "no flag, no management"
        );
        let (status, stderr) = server.stop();
        assert!(status.success());
        assert_eq!(
            stderr.last().map(String::as_str),
            Some("memocampus-mcp: stopped (ctrl_break)")
        );
        assert!(!env.log().contains("management_ipc_ready"));
    }

    // ---- The tray's supervisor against the real server ----

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn launcher(env: &Env) -> WinLauncher {
        let mut launcher = WinLauncher::new(PathBuf::from(EXE));
        launcher.env = vec![
            ("MEMOCAMPUS_APP_DIR".into(), Some(env.app.clone().into())),
            (
                "LOCALAPPDATA".into(),
                Some(env.dir.join("localappdata").into()),
            ),
            ("MEMOCAMPUS_DB".into(), None),
        ];
        launcher
    }

    fn supervise(env: &Env, settings: Settings) -> Supervisor {
        Supervisor::spawn(
            Box::new(launcher(env)),
            Box::new(move || Ok(settings.clone())),
            Timings::standard(),
            RestartPolicy::standard(),
            Box::new(|_| {}),
        )
    }

    fn wait_for(
        supervisor: &Supervisor,
        what: &str,
        condition: impl Fn(&Snapshot) -> bool,
    ) -> Snapshot {
        let mut last = supervisor.snapshot();
        wait_until(what, || {
            last = supervisor.snapshot();
            condition(&last)
        });
        last
    }

    fn settings_on(port: u16) -> Settings {
        Settings {
            listen_port: port,
            ..Settings::default()
        }
    }

    #[test]
    fn the_supervisor_runs_the_real_server_and_stops_it_over_the_channel() {
        let env = Env::new();
        env.seed();
        let port = free_port();
        let supervisor = supervise(&env, settings_on(port));
        supervisor.send(Order::Start);
        let up = wait_for(&supervisor, "running", |s| s.state == ServerState::Running);
        assert_eq!(up.port, Some(port));
        // Up is all the tray knows: it does not claim the vault is readable.
        assert_eq!(up.vault, VaultStatus::Unknown);
        let pid = up.pid.unwrap();
        let ok = raw(
            port,
            "POST",
            "/mcp",
            &[
                ("Authorization", &format!("Bearer {}", env.token())),
                ("MCP-Protocol-Version", LEGACY),
            ],
            &call_body(1, "memocampus_get_domain_stats", &json!({"limit": 1})).to_string(),
        );
        assert_eq!(ok.status, 200, "the managed server serves as any other");

        supervisor.send(Order::Stop);
        let stopped = wait_for(&supervisor, "stopped", |s| s.state == ServerState::Stopped);
        assert_eq!(
            stopped.last_exit,
            Some(LastExit::Graceful),
            "left by itself, not terminated"
        );
        wait_until("the process to be gone", || !alive(pid));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
        let events = events_of(&env, pid);
        let shutdown = events
            .iter()
            .find(|e| e["event"] == "http_shutdown")
            .unwrap();
        assert_eq!(shutdown["reason"], "management_stop");
        // The server's own record of how it was started: no console window and not attached to any
        // console, so there is nothing that could be shown.
        let started = events
            .iter()
            .find(|e| e["event"] == "startup_context")
            .unwrap();
        println!("startup_context of the supervised server: {started}");
        assert_eq!(started["console_window"], false, "{started}");
        assert_eq!(started["console_processes"], 0, "{started}");

        // And it starts again on the same port.
        supervisor.send(Order::Start);
        wait_for(&supervisor, "running again", |s| {
            s.state == ServerState::Running
        });
        supervisor.shutdown();
        wait_until("the second server to be gone", || {
            TcpStream::connect(("127.0.0.1", port)).is_err()
        });
    }

    #[test]
    fn an_unpublished_or_missing_vault_is_an_error_not_a_running_server() {
        for (state, code) in [
            (Some("pending"), "vault_publication_pending"),
            (None, "vault_publication_pending"),
        ] {
            let env = Env::new();
            env.seed();
            publish(&env.app, &env.vault, state);
            let supervisor = supervise(&env, settings_on(free_port()));
            supervisor.send(Order::Start);
            let end = wait_for(&supervisor, "error", |s| {
                matches!(s.state, ServerState::Error(_))
            });
            assert_eq!(
                end.state,
                ServerState::Error(ErrorKind::VaultRefused(code.into()))
            );
            assert_eq!(end.vault, VaultStatus::Refused(code.into()));
            assert_eq!(end.pid, None);
        }
        // No config.json at all: nothing is created for it.
        let env = Env::new();
        fs::remove_file(env.app.join("config.json")).unwrap();
        let before: Vec<_> = fs::read_dir(&env.app)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let supervisor = supervise(&env, settings_on(free_port()));
        supervisor.send(Order::Start);
        let end = wait_for(&supervisor, "error", |s| {
            matches!(s.state, ServerState::Error(_))
        });
        assert_eq!(
            end.state,
            ServerState::Error(ErrorKind::VaultRefused("vault_not_configured".into()))
        );
        let mut after: Vec<_> = fs::read_dir(&env.app)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let mut before = before;
        before.sort();
        after.sort();
        // Only the diagnostic log directory may appear; no config, no vault, no token.
        after.retain(|name| name != "logs");
        before.retain(|name| name != "logs");
        assert_eq!(before, after);
        assert!(!env.app.join("config.json").exists());
    }

    #[test]
    fn a_pinned_vault_does_not_widen_what_the_server_may_read() {
        let env = Env::new();
        env.seed();
        // The pinned file is the active, confirmed vault: it runs.
        let port = free_port();
        let supervisor = supervise(
            &env,
            Settings {
                listen_port: port,
                vault_mode: VaultMode::Pinned,
                pinned_vault_path: Some(env.vault.to_string_lossy().into_owned()),
                ..Settings::default()
            },
        );
        supervisor.send(Order::Start);
        wait_for(&supervisor, "running", |s| s.state == ServerState::Running);
        supervisor.shutdown();

        // Another file that is not the active vault is refused, however it is pinned.
        let other = env.dir.join("other.sqlite3");
        fs::copy(&env.vault, &other).unwrap();
        let supervisor = supervise(
            &env,
            Settings {
                listen_port: free_port(),
                vault_mode: VaultMode::Pinned,
                pinned_vault_path: Some(other.to_string_lossy().into_owned()),
                ..Settings::default()
            },
        );
        supervisor.send(Order::Start);
        let end = wait_for(&supervisor, "error", |s| {
            matches!(s.state, ServerState::Error(_))
        });
        assert_eq!(
            end.state,
            ServerState::Error(ErrorKind::VaultRefused("vault_not_active".into()))
        );
    }

    #[test]
    fn a_taken_port_is_an_error_and_the_holder_is_left_alone() {
        let env = Env::new();
        env.seed();
        let holder = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = holder.local_addr().unwrap().port();
        let supervisor = supervise(&env, settings_on(port));
        supervisor.send(Order::Start);
        let end = wait_for(&supervisor, "error", |s| {
            matches!(s.state, ServerState::Error(_))
        });
        assert_eq!(end.state, ServerState::Error(ErrorKind::PortInUse));
        assert_eq!(
            end.vault,
            VaultStatus::Unknown,
            "a port problem says nothing about the vault"
        );
        // Whoever held the port still holds it, untouched; no other port was taken.
        let probe = TcpStream::connect(("127.0.0.1", port)).unwrap();
        drop(probe);
        assert!(holder.accept().is_ok());
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            matches!(
                supervisor.snapshot().state,
                ServerState::Error(ErrorKind::PortInUse)
            ),
            "no retry"
        );
    }

    #[test]
    fn a_server_started_by_someone_else_on_the_port_is_not_stopped_by_the_supervisor() {
        let env = Env::new();
        env.seed();
        // Another server of the same program holds the port (this is "the user's own" server).
        let outsider = Server::start(&env, "127.0.0.1:0");
        let port = outsider.port;
        let supervisor = supervise(&env, settings_on(port));
        supervisor.send(Order::Start);
        let end = wait_for(&supervisor, "error", |s| {
            matches!(s.state, ServerState::Error(_))
        });
        assert_eq!(end.state, ServerState::Error(ErrorKind::PortInUse));
        supervisor.send(Order::Stop);
        supervisor.shutdown();
        let mut outsider = outsider;
        assert!(
            outsider.child.try_wait().unwrap().is_none(),
            "the other server was not touched"
        );
        let ok = post(
            &outsider,
            &env.token(),
            &call_body(1, "memocampus_get_domain_stats", &json!({"limit": 1})),
        );
        assert_eq!(ok.status, 200);
        let (status, _) = outsider.stop();
        assert!(status.success());
    }

    /// The tray side of the orphan test: a separate process that runs the supervisor and then waits
    /// to be killed. It does nothing unless the parent test sets its variables.
    #[test]
    fn the_stand_in_tray_process_for_the_orphan_test() {
        let (Ok(app), Ok(local), Ok(port), Ok(info)) = (
            std::env::var("MEMOCAMPUS_TEST_TRAY_APP"),
            std::env::var("MEMOCAMPUS_TEST_TRAY_LOCAL"),
            std::env::var("MEMOCAMPUS_TEST_TRAY_PORT"),
            std::env::var("MEMOCAMPUS_TEST_TRAY_INFO"),
        ) else {
            return;
        };
        let mut launcher = WinLauncher::new(PathBuf::from(EXE));
        launcher.env = vec![
            ("MEMOCAMPUS_APP_DIR".into(), Some(app.into())),
            ("LOCALAPPDATA".into(), Some(local.into())),
            ("MEMOCAMPUS_DB".into(), None),
        ];
        let settings = settings_on(port.parse().unwrap());
        let supervisor = Supervisor::spawn(
            Box::new(launcher),
            Box::new(move || Ok(settings.clone())),
            Timings::standard(),
            RestartPolicy::standard(),
            Box::new(|_| {}),
        );
        supervisor.send(Order::Start);
        let up = wait_for(&supervisor, "running", |s| s.state == ServerState::Running);
        fs::write(info, up.pid.unwrap().to_string()).unwrap();
        std::thread::sleep(Duration::from_secs(3600));
    }

    #[test]
    fn killing_the_tray_process_leaves_no_server_behind() {
        let env = Env::new();
        env.seed();
        let port = free_port();
        let info = env.dir.join("tray-info.txt");
        let mut tray = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "management::the_stand_in_tray_process_for_the_orphan_test",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MEMOCAMPUS_TEST_TRAY_APP", &env.app)
            .env("MEMOCAMPUS_TEST_TRAY_LOCAL", env.dir.join("localappdata"))
            .env("MEMOCAMPUS_TEST_TRAY_PORT", port.to_string())
            .env("MEMOCAMPUS_TEST_TRAY_INFO", &info)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_until("the tray's server", || {
            info.exists() && !fs::read_to_string(&info).unwrap_or_default().is_empty()
        });
        let pid: u32 = fs::read_to_string(&info).unwrap().trim().parse().unwrap();
        assert!(alive(pid));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        // As a crash or "End task" would: no cleanup runs.
        tray.kill().unwrap();
        let _ = tray.wait();
        wait_until("the server to be gone", || !alive(pid));
        wait_until("the port to be free", || {
            TcpStream::connect(("127.0.0.1", port)).is_err()
        });
    }
}
