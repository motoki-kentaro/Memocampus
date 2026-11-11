//! Issue #51, Slice D, through the real host binary, one process per message as Chrome runs it:
//! converting the active vault plain → encrypted (start, spell re-entry, activation) and
//! encrypted → plain, with visits recorded through the ordinary `visit` message in between.
//! Temporary app directories and vaults only.

use memocampus_native_host::active_vault::{self, GateError};
use memocampus_native_host::secure_vault::{cipher, sidecar};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-d-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("app")).unwrap();
        Self(dir)
    }

    fn app(&self) -> PathBuf {
        self.0.join("app")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One host process; returns its response and its raw stdout.
fn one_raw(app: &Path, message: Value) -> (Value, Vec<u8>) {
    let mut message = message;
    message["requestId"] = "r".into();
    let mut child = Command::new(env!("CARGO_BIN_EXE_memocampus-native-host"))
        .env("MEMOCAMPUS_APP_DIR", app)
        .env_remove("MEMOCAMPUS_DB")
        .current_dir(app)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let body = serde_json::to_vec(&message).unwrap();
    stdin.write_all(&(body.len() as u32).to_ne_bytes()).unwrap();
    stdin.write_all(&body).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "the host never logs");
    let mut stdout = output.stdout.as_slice();
    let mut length = [0u8; 4];
    stdout.read_exact(&mut length).unwrap();
    let mut body = vec![0u8; u32::from_ne_bytes(length) as usize];
    stdout.read_exact(&mut body).unwrap();
    (serde_json::from_slice(&body).unwrap(), output.stdout)
}

fn one(app: &Path, message: Value) -> Value {
    one_raw(app, message).0
}

fn status(app: &Path) -> Value {
    one(app, json!({ "type": "vault_status" }))["status"].clone()
}

fn visit(app: &Path, id: &str) {
    let done = one(
        app,
        json!({ "type": "visit", "id": id, "url": format!("https://canary.test/CANARY-{id}"),
                "title": "CANARY title", "incognito": false, "visitedAt": "2026-11-10T10:00:00.000Z" }),
    );
    assert_eq!(done["ok"], true, "{done}");
}

fn token(path: &Path) -> String {
    active_vault::vault_token(path).unwrap()
}

fn visits_of(db: &rusqlite::Connection) -> Vec<String> {
    let mut statement = db
        .prepare("SELECT event_id FROM visits ORDER BY event_id")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_plain_vault_is_encrypted_and_back_across_host_processes() {
    let tmp = TempDir::new();
    let app = tmp.app();
    memocampus_native_host::policy_store::PolicyStore::new(app.clone())
        .unwrap()
        .initialize()
        .unwrap();
    let plain = tmp.0.join("plain.sqlite3");
    assert_eq!(
        one(&app, json!({ "type": "vault_create", "path": plain }))["ok"],
        true
    );
    visit(&app, "a1");
    visit(&app, "a2");
    let shown = status(&app);
    // Publish the plain vault, so that the converted one is seen to start pending regardless.
    let published = one(
        &app,
        json!({ "type": "vault_publication_confirm", "expectedVaultToken": shown["vaultToken"] }),
    );
    assert_eq!(published["status"]["publication"], "confirmed");
    assert!(active_vault::admit(&app, None).is_ok());

    // D1, each message in its own process.
    let encrypted = tmp.0.join("encrypted.sqlite3");
    let (started, raw) = one_raw(
        &app,
        json!({ "type": "vault_convert_start", "direction": "encrypt",
                "expectedVaultToken": shown["vaultToken"], "path": encrypted }),
    );
    assert_eq!(started["ok"], true, "{started}");
    let spell = started["recoverySpell"].as_str().unwrap().to_owned();
    assert_eq!(spell.len(), 43);
    assert_eq!(
        raw.windows(43).filter(|w| *w == spell.as_bytes()).count(),
        1,
        "the spell appears once, in this response"
    );
    // Recording goes on into the source meanwhile; the status shows no spell.
    visit(&app, "a3");
    let (pending, raw) = one_raw(&app, json!({ "type": "vault_status" }));
    assert!(!raw.windows(43).any(|w| w == spell.as_bytes()));
    assert_eq!(pending["status"]["conversionPending"]["phase"], "verified");
    assert_eq!(pending["status"]["mode"], "plain");
    assert!(
        active_vault::admit(&app, None).is_ok(),
        "the source stays published"
    );
    let confirmed = one(
        &app,
        json!({ "type": "vault_convert_confirm", "recoverySpell": spell }),
    );
    assert_eq!(confirmed["ok"], true, "{confirmed}");
    visit(&app, "a4");
    let activated = one(
        &app,
        json!({ "type": "vault_convert_activate", "expectedVaultToken": started["vaultToken"] }),
    );
    assert_eq!(activated["ok"], true, "{activated}");
    assert_eq!(activated["status"]["mode"], "encrypted");
    assert_eq!(activated["status"]["publication"], "pending");
    assert_eq!(
        active_vault::admit(&app, None).unwrap_err(),
        GateError::PublicationPending,
        "MCP reads nothing of the new vault until the user confirms it"
    );
    visit(&app, "a5");
    // Every visit is in the new vault: those before, during and after the conversion.
    let config = active_vault::read_config(&app).unwrap().unwrap();
    assert_eq!(token(Path::new(&config.active_vault)), token(&encrypted));
    let key = memocampus_native_host::secure_vault::key_store::unlock(
        &app,
        &config.vault_id.unwrap(),
        &match cipher::read_head(&encrypted).unwrap() {
            cipher::Head::Salt(salt) => salt,
            other => panic!("{other:?}"),
        },
    )
    .unwrap();
    let db = cipher::open(&encrypted, &key, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(visits_of(&db), ["a1", "a2", "a3", "a4", "a5"]);
    drop(db);
    // The source holds what it had at the switch; nothing after.
    let source = rusqlite::Connection::open(&plain).unwrap();
    assert_eq!(visits_of(&source), ["a1", "a2", "a3", "a4"]);
    drop(source);

    // D2, back to plain, acknowledged.
    let back = tmp.0.join("back.sqlite3");
    let shown = status(&app);
    let started = one(
        &app,
        json!({ "type": "vault_convert_start", "direction": "decrypt", "acknowledgePlaintext": true,
                "expectedVaultToken": shown["vaultToken"], "path": back }),
    );
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["phase"], "converted");
    visit(&app, "a6");
    let activated = one(
        &app,
        json!({ "type": "vault_convert_activate", "expectedVaultToken": started["vaultToken"] }),
    );
    assert_eq!(activated["ok"], true, "{activated}");
    assert_eq!(activated["status"]["mode"], "plain");
    assert_eq!(activated["status"]["publication"], "pending");
    let db = rusqlite::Connection::open(&back).unwrap();
    assert_eq!(visits_of(&db), ["a1", "a2", "a3", "a4", "a5", "a6"]);
    // Both earlier vaults are kept; no sidecar was ever written (Slice E).
    assert!(plain.exists() && encrypted.exists());
    assert!(!sidecar::path_for(&encrypted).exists());
}
