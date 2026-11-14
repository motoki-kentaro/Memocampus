//! Real Native Messaging framing/processes, synthetic temp APP_DIR/Vault only. No Chrome or registry.
use base64::{engine::general_purpose::STANDARD, Engine};
use memocampus_native_host::{policy_store::PolicyStore, schema};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

struct Fixture {
    root: PathBuf,
    app: PathBuf,
    db: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("memocampus-page-process-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let app = root.join("app");
        let db = app.join("memocampus.sqlite3");
        let f = Self { root, app, db };
        let response = f
            .host()
            .send(json!({"type":"vault_create","requestId":"create","useDefault":true}));
        assert_eq!(response["ok"], true, "{response}");
        f
    }
    fn host(&self) -> Host {
        let mut child = Command::new(env!("CARGO_BIN_EXE_memocampus-native-host"))
            .env("MEMOCAMPUS_APP_DIR", &self.app)
            .env_remove("MEMOCAMPUS_DB")
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (send, received) = mpsc::channel();
        std::thread::spawn(move || loop {
            let mut header = [0; 4];
            if stdout.read_exact(&mut header).is_err() {
                break;
            }
            let len = u32::from_ne_bytes(header) as usize;
            assert!(len < 1024 * 1024);
            let mut bytes = vec![0; len];
            if stdout.read_exact(&mut bytes).is_err() {
                break;
            }
            if send.send(serde_json::from_slice(&bytes).unwrap()).is_err() {
                break;
            }
        });
        Host { child, received }
    }
    fn counts(&self) -> (i64, i64, i64) {
        let db = Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        db.busy_timeout(Duration::from_secs(15)).unwrap();
        schema::check(&db).unwrap();
        db.query_row("SELECT (SELECT count(*) FROM page_blobs),(SELECT count(*) FROM saved_pages),(SELECT count(*) FROM visits)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
struct Host {
    child: Child,
    received: Receiver<Value>,
}
impl Host {
    fn send(&mut self, message: Value) -> Value {
        self.write(message);
        self.received
            .recv_timeout(Duration::from_secs(15))
            .expect("bounded host response")
    }
    fn write(&mut self, message: Value) {
        let bytes = serde_json::to_vec(&message).unwrap();
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin
            .write_all(&(bytes.len() as u32).to_ne_bytes())
            .unwrap();
        stdin.write_all(&bytes).unwrap();
        stdin.flush().unwrap();
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn meta() -> Value {
    json!({"url":"https://accounts.google.com/signin?access_token=SYNTHETIC_ONLY","title":"SYNTHETIC private page","capturedAt":1,"incognito":false})
}
fn open(host: &mut Host) -> Value {
    let reply=host.send(json!({"type":"page_save_open","protocol":1,"saveId":uuid::Uuid::new_v4().to_string(),"metadata":meta()}));
    assert_eq!(reply["type"], "page_save_opened", "{reply}");
    reply
}
fn message(binding: &Value, kind: &str, extra: Value) -> Value {
    let mut message = json!({"type":kind,"saveId":binding["saveId"],"vaultToken":binding["vaultToken"],"generation":binding["generation"]});
    message
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    message
}

#[test]
fn explicit_page_save_bypasses_policy_but_never_creates_a_visit_and_lost_ack_uses_status() {
    let f = Fixture::new();
    PolicyStore::new(f.app.clone())
        .unwrap()
        .initialize()
        .unwrap();
    let automatic=f.host().send(json!({"type":"visit","id":"synthetic-visit","url":meta()["url"],"title":"synthetic","visitedAt":"2026-11-01T00:00:00.000Z","incognito":false}));
    assert_eq!(automatic["dropped"], "policy");
    let mut host = f.host();
    let binding = open(&mut host);
    let raw = b"MIME-Version: 1.0\r\nContent-Type: multipart/related\r\n\r\nSYNTHETIC\0\xff";
    assert_eq!(
        host.send(message(
            &binding,
            "page_save_begin",
            json!({"byteLength":raw.len()})
        ))["type"],
        "page_save_ready"
    );
    assert_eq!(
        host.send(message(
            &binding,
            "page_save_chunk",
            json!({"index":0,"data":STANDARD.encode(raw)})
        ))["type"],
        "page_save_chunk_ack"
    );
    let prepared = host.send(message(&binding, "page_save_prepare", json!({})));
    assert_eq!(f.counts(), (0, 0, 0));
    // Do not consume the COMMIT ACK. Observe the durable event, terminate that port, then query
    // status on a new host: never resend COMMIT or recapture.
    host.write(message(
        &binding,
        "page_save_commit",
        json!({"sha256":prepared["sha256"]}),
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while f.counts().1 == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(host);
    let status = f.host().send(message(
        &binding,
        "page_save_status",
        json!({"metadata":meta(),"byteLength":raw.len(),"sha256":prepared["sha256"]}),
    ));
    assert_eq!(status["type"], "page_saved");
    assert_eq!(status["replayed"], true);
    assert_eq!(f.counts(), (1, 1, 0));
    let db = Connection::open_with_flags(&f.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        db.query_row("SELECT data FROM page_blobs", [], |r| r
            .get::<_, Vec<u8>>(0))
            .unwrap(),
        raw
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM page_metadata", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn worker_port_loss_and_invalid_page_frame_leave_no_partial_persistence_or_stuck_lock() {
    let f = Fixture::new();
    {
        let mut host = f.host();
        let binding = open(&mut host);
        assert_eq!(
            host.send(message(
                &binding,
                "page_save_begin",
                json!({"byteLength":1})
            ))["ok"],
            true
        );
        host.send(message(
            &binding,
            "page_save_chunk",
            json!({"index":0,"data":"eA=="}),
        ));
    }
    assert_eq!(f.counts(), (0, 0, 0));
    let mut host = f.host();
    let _binding = open(&mut host);
    // Only a length header: the Page port must reject it before allocating/reading a body.
    host.child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&(2u32 * 1024 * 1024 + 1).to_ne_bytes())
        .unwrap();
    assert!(host.received.recv_timeout(Duration::from_secs(5)).is_err());
    drop(host);
    assert_eq!(f.counts(), (0, 0, 0));
    let mut recovered = f.host();
    open(&mut recovered);
}

#[test]
fn manual_page_save_does_not_require_initializing_or_modifying_policy() {
    let f = Fixture::new();
    assert!(!f.app.join("policy.json").exists());
    let mut host = f.host();
    let binding = open(&mut host);
    assert_eq!(
        host.send(message(
            &binding,
            "page_save_begin",
            json!({"byteLength":1})
        ))["ok"],
        true
    );
    host.send(message(
        &binding,
        "page_save_chunk",
        json!({"index":0,"data":"eA=="}),
    ));
    let prepared = host.send(message(&binding, "page_save_prepare", json!({})));
    assert_eq!(
        host.send(message(
            &binding,
            "page_save_commit",
            json!({"sha256":prepared["sha256"]})
        ))["type"],
        "page_saved"
    );
    assert!(!f.app.join("policy.json").exists());
    assert_eq!(f.counts(), (1, 1, 0));
}
