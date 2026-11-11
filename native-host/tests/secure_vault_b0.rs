//! Issue #51, Slice B0, through the real host binary: the version 1 → 2 config migration at start
//! (PO decision P3), publication decisions over Native Messaging, and many host processes
//! switching the active vault at once (B0-5). Temp directories as `MEMOCAMPUS_APP_DIR` only.

use memocampus_native_host::active_vault::{self, Config, PublicationState, VaultMode};
use memocampus_native_host::policy_store::PolicyStore;
use memocampus_native_host::schema;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-b0-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("app")).unwrap();
        Self(dir)
    }

    fn app(&self) -> PathBuf {
        self.0.join("app")
    }

    fn vault(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        Connection::open(&path)
            .unwrap()
            .execute_batch(&schema::create_sql())
            .unwrap();
        path
    }

    fn config(&self) -> Config {
        active_vault::read_config(&self.app()).unwrap().unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn host(tmp: &TempDir, messages: &[Value]) -> Vec<Value> {
    host_in(&tmp.0, messages)
}

/// One host process with `<dir>/app` as its application directory.
fn host_in(dir: &Path, messages: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_memocampus-native-host"))
        .env("MEMOCAMPUS_APP_DIR", dir.join("app"))
        .env_remove("MEMOCAMPUS_DB")
        .current_dir(dir)
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
    let (mut responses, mut length) = (Vec::new(), [0u8; 4]);
    while stdout.read_exact(&mut length).is_ok() {
        let mut body = vec![0u8; u32::from_ne_bytes(length) as usize];
        stdout.read_exact(&mut body).unwrap();
        responses.push(serde_json::from_slice(&body).unwrap());
    }
    responses
}

fn status(tmp: &TempDir) -> Value {
    host(tmp, &[json!({ "type": "vault_status", "requestId": "s" })])[0]["status"].clone()
}

fn legacy(tmp: &TempDir, vault: &Path) {
    std::fs::write(
        tmp.app().join("config.json"),
        json!({"version": 1, "activeVault": vault, "locationApproved": false}).to_string(),
    )
    .unwrap();
}

#[test]
fn a_host_from_before_issue_51_keeps_mcp_access_for_its_active_vault_if_the_policy_is_ready() {
    let tmp = TempDir::new();
    let a = tmp.vault("a.sqlite3");
    let vault_before = std::fs::read(&a).unwrap();
    PolicyStore::new(tmp.app()).unwrap().initialize().unwrap();
    legacy(&tmp, &a);
    // Any message starts the host; a policy check is the most frequent one.
    host(
        &tmp,
        &[json!({ "type": "host_capabilities", "requestId": "c" })],
    );
    let config = tmp.config();
    assert_eq!(config.version, active_vault::CONFIG_VERSION);
    assert_eq!(config.mode, VaultMode::Plain);
    let publication = config.publication.unwrap();
    assert_eq!(publication.state, PublicationState::Confirmed);
    assert_eq!(
        publication.vault_token,
        active_vault::vault_token(&a).unwrap()
    );
    assert_eq!(publication.policy_revision, Some(1));
    assert_eq!(status(&tmp)["publication"], "confirmed");
    assert_eq!(
        std::fs::read(&a).unwrap(),
        vault_before,
        "the vault is not changed"
    );
}

#[test]
fn without_a_ready_policy_the_migrated_vault_waits_for_the_user() {
    let tmp = TempDir::new();
    let a = tmp.vault("a.sqlite3");
    legacy(&tmp, &a);
    let shown = status(&tmp);
    assert_eq!(shown["publication"], "pending");
    assert_eq!(
        tmp.config().publication.unwrap().state,
        PublicationState::Pending
    );
    assert!(
        !tmp.app().join("policy.json").exists(),
        "the policy is never initialized"
    );
    // Once the user has a policy, confirming the vault shown publishes it.
    PolicyStore::new(tmp.app()).unwrap().initialize().unwrap();
    let confirmed = &host(
        &tmp,
        &[
            json!({ "type": "vault_publication_confirm", "requestId": "p", "expectedVaultToken": shown["vaultToken"] }),
        ],
    )[0];
    assert_eq!(confirmed["ok"], true, "{confirmed}");
    assert_eq!(confirmed["status"]["publication"], "confirmed");
}

#[test]
fn a_damaged_config_is_reported_and_never_migrated_or_rewritten() {
    let tmp = TempDir::new();
    let path = tmp.app().join("config.json");
    std::fs::write(&path, br#"{"version":1,"activeVault":"#).unwrap();
    assert_eq!(status(&tmp)["state"], "config_invalid");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        br#"{"version":1,"activeVault":"#
    );
}

/// Several host processes switch the active vault at the same time while this process reads the
/// config: every switch either completes or is refused (`vault_busy`), and the config is always
/// one complete, valid version 2 file.
#[test]
fn concurrent_host_processes_switching_vaults_never_leave_a_partial_config() {
    let tmp = TempDir::new();
    let (a, b) = (tmp.vault("a.sqlite3"), tmp.vault("b.sqlite3"));
    host(
        &tmp,
        &[json!({ "type": "vault_open", "requestId": "o", "path": a })],
    );
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (app, stop) = (tmp.app(), stop.clone());
        std::thread::spawn(move || {
            let mut reads = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                match active_vault::read_config(&app) {
                    Ok(Some(config)) => {
                        assert_eq!(config.version, active_vault::CONFIG_VERSION);
                        reads += 1;
                    }
                    Err(active_vault::ConfigError::Io) => {}
                    other => panic!("{other:?}"),
                }
            }
            reads
        })
    };
    let processes: Vec<_> = (0..8)
        .map(|i| {
            let target = if i % 2 == 0 { b.clone() } else { a.clone() };
            let dir = tmp.0.clone();
            std::thread::spawn(move || {
                host_in(
                    &dir,
                    &[json!({ "type": "vault_open", "requestId": "o", "path": target })],
                )[0]
                .clone()
            })
        })
        .collect();
    for process in processes {
        let reply = process.join().unwrap();
        assert!(
            reply["ok"] == true || reply["error"] == "vault_busy",
            "{reply}"
        );
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
    let config = tmp.config();
    let active = PathBuf::from(&config.active_vault);
    assert!(active.ends_with("a.sqlite3") || active.ends_with("b.sqlite3"));
    let publication = config.publication.unwrap();
    assert_eq!(
        publication.vault_token,
        active_vault::vault_token(&active).unwrap()
    );
    assert_eq!(publication.state, PublicationState::Pending);
    let leftovers: Vec<_> = std::fs::read_dir(tmp.app())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// --- F-02: policy edits keep the publication, an initialization withdraws it -------------------

fn one(tmp: &TempDir, message: Value) -> Value {
    host(tmp, &[message]).remove(0)
}

/// The active vault `a`, confirmed for MCP over Native Messaging, with an initialized policy.
fn published(tmp: &TempDir) -> (PathBuf, String) {
    let a = tmp.vault("a.sqlite3");
    assert_eq!(
        one(
            tmp,
            json!({ "type": "policy_initialize", "requestId": "p" })
        )["ok"],
        true
    );
    let opened = one(
        tmp,
        json!({ "type": "vault_open", "requestId": "o", "path": a }),
    );
    let token = opened["status"]["vaultToken"].as_str().unwrap().to_owned();
    let confirmed = one(
        tmp,
        json!({ "type": "vault_publication_confirm", "requestId": "c", "expectedVaultToken": token }),
    );
    assert_eq!(
        confirmed["status"]["publication"], "confirmed",
        "{confirmed}"
    );
    (a, token)
}

fn lose_policy(tmp: &TempDir) {
    for name in ["policy.json", "policy.initialized"] {
        std::fs::remove_file(tmp.app().join(name)).unwrap();
    }
}

fn publication(tmp: &TempDir) -> (String, PublicationState) {
    let publication = tmp.config().publication.unwrap();
    (publication.vault_token, publication.state)
}

#[test]
fn ordinary_policy_edits_keep_the_vault_published() {
    let tmp = TempDir::new();
    let (_, token) = published(&tmp);
    // One URL hidden from MCP, then published again (the side panel's item actions).
    for action in ["save_but_no_mcp", "publish"] {
        let url = "https://forum.test/thread/1";
        let preview = one(
            &tmp,
            json!({ "type": "policy_item_preview", "requestId": "p", "url": url, "action": action }),
        );
        let applied = one(
            &tmp,
            json!({ "type": "policy_item_apply", "requestId": "a", "url": url, "action": action, "expectedEtag": preview["etag"] }),
        );
        assert_eq!(applied["ok"], true, "{applied}");
    }
    // The whole policy stored again from the settings page.
    let current = one(&tmp, json!({ "type": "policy_get", "requestId": "g" }));
    let applied = one(
        &tmp,
        json!({ "type": "policy_apply", "requestId": "a", "expectedEtag": current["etag"], "policy": current["policy"] }),
    );
    assert_eq!(applied["ok"], true, "{applied}");
    assert_eq!(applied["revision"], 4);
    // Asking to initialize an initialized policy changes nothing.
    assert_eq!(
        one(
            &tmp,
            json!({ "type": "policy_initialize", "requestId": "i" })
        )["error"],
        "policy_already_initialized"
    );
    assert_eq!(publication(&tmp), (token, PublicationState::Confirmed));
    assert_eq!(status(&tmp)["publication"], "confirmed");
}

#[test]
fn initializing_or_starting_a_policy_again_withdraws_the_publication() {
    for start in [false, true] {
        let tmp = TempDir::new();
        let (_, token) = published(&tmp);
        lose_policy(&tmp);
        let reply = if start {
            let draft: Value =
                serde_json::from_str(memocampus_native_host::policy::DEFAULT_JSON).unwrap();
            one(
                &tmp,
                json!({ "type": "policy_start", "requestId": "s", "policy": draft }),
            )
        } else {
            one(
                &tmp,
                json!({ "type": "policy_initialize", "requestId": "i" }),
            )
        };
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["revision"], 1);
        assert_eq!(
            publication(&tmp),
            (token.clone(), PublicationState::Pending),
            "start={start}"
        );
        let shown = status(&tmp);
        assert_eq!(shown["publication"], "pending");
        assert_eq!(shown["vaultToken"], token.as_str());
        // A host restart does not bring the approval back.
        host(
            &tmp,
            &[json!({ "type": "host_capabilities", "requestId": "c" })],
        );
        assert_eq!(
            publication(&tmp),
            (token.clone(), PublicationState::Pending)
        );
        // The user confirms again, under the new policy.
        let again = one(
            &tmp,
            json!({ "type": "vault_publication_confirm", "requestId": "c", "expectedVaultToken": token }),
        );
        assert_eq!(again["status"]["publication"], "confirmed");
        assert_eq!(again["status"]["publicationPolicyRevision"], 1);
    }
}

/// Another process holds the config lock for longer than the host waits: the initialization is
/// refused with nothing written, and its retry ends pending.
#[cfg(windows)]
#[test]
fn an_initialization_that_cannot_withdraw_writes_no_policy() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = TempDir::new();
    let (_, token) = published(&tmp);
    lose_policy(&tmp);
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(tmp.app().join("config.lock"))
        .unwrap();
    let refused = one(
        &tmp,
        json!({ "type": "policy_initialize", "requestId": "i" }),
    );
    drop(held);
    assert_eq!(refused["error"], "policy_busy", "{refused}");
    assert!(!tmp.app().join("policy.json").exists());
    assert!(!tmp.app().join("policy.initialized").exists());
    assert_eq!(
        one(
            &tmp,
            json!({ "type": "policy_initialize", "requestId": "i" })
        )["ok"],
        true
    );
    assert_eq!(publication(&tmp), (token, PublicationState::Pending));
}
