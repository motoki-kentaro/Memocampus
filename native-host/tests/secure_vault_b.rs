//! Issue #51, Slice B, through the real host binary: creating an encrypted vault with a recovery
//! phrase (begin → confirm → commit, or cancel), writing to it, opening it, and recovering it with
//! the phrase where this Windows user has no key. Temporary app directories and vaults only; the
//! key store is the temporary app directory's, protected with this user's DPAPI.

use memocampus_native_host::active_vault::{self, PublicationState, VaultMode};
use memocampus_native_host::secure_vault::secret::{CipherSalt, VaultId, VaultKey};
use memocampus_native_host::secure_vault::{cipher, key_store, phrase, sidecar};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-b-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("app")).unwrap();
        Self(dir)
    }

    fn app(&self) -> PathBuf {
        self.0.join("app")
    }

    /// A second app directory: another Windows user or a reinstalled PC, as far as the key store
    /// and the config go.
    fn other_app(&self) -> PathBuf {
        let app = self.0.join("app2");
        std::fs::create_dir_all(&app).unwrap();
        app
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One host process with `app` as its application directory; also returns its raw stdout.
fn host_raw(app: &Path, messages: &[Value]) -> (Vec<Value>, Vec<u8>) {
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
    for message in messages {
        let body = serde_json::to_vec(message).unwrap();
        stdin.write_all(&(body.len() as u32).to_ne_bytes()).unwrap();
        stdin.write_all(&body).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "the host never logs");
    let mut stdout = output.stdout.as_slice();
    let (mut responses, mut length) = (Vec::new(), [0u8; 4]);
    while stdout.read_exact(&mut length).is_ok() {
        let mut body = vec![0u8; u32::from_ne_bytes(length) as usize];
        stdout.read_exact(&mut body).unwrap();
        responses.push(serde_json::from_slice(&body).unwrap());
    }
    (responses, output.stdout)
}

fn one(app: &Path, message: Value) -> Value {
    let mut message = message;
    message["requestId"] = "r".into();
    host_raw(app, &[message]).0.remove(0)
}

fn status(app: &Path) -> Value {
    one(app, json!({ "type": "vault_status" }))["status"].clone()
}

fn visit(id: &str, url: &str) -> Value {
    json!({ "type": "visit", "id": id, "url": url, "title": "CANARY title", "incognito": false,
            "visitedAt": "2026-11-09T10:00:00.000Z" })
}

fn init_policy(app: &Path) {
    assert_eq!(one(app, json!({ "type": "policy_initialize" }))["ok"], true);
}

/// Begins a creation at `target`; returns the phrase (12 words, single spaces).
fn begin(app: &Path, target: &Path) -> String {
    let begun = one(
        app,
        json!({ "type": "vault_create_encrypted", "path": target }),
    );
    assert_eq!(begun["ok"], true, "{begun}");
    let words: Vec<&str> = begun["recoveryPhrase"]
        .as_array()
        .unwrap()
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    assert_eq!(words.len(), 12);
    words.join(" ")
}

fn commit(app: &Path, phrase: &str) -> Value {
    one(
        app,
        json!({ "type": "vault_create_encrypted_commit", "recoveryPhrase": phrase }),
    )
}

/// Creates an encrypted vault at `target` and makes it active; returns its phrase.
fn create(app: &Path, target: &Path) -> String {
    let phrase = begin(app, target);
    let done = commit(app, &phrase);
    assert_eq!(done["ok"], true, "{done}");
    phrase
}

fn config(app: &Path) -> Option<active_vault::Config> {
    active_vault::read_config(app).unwrap()
}

/// The key of the active encrypted vault, from the key store (test-side check only).
fn active_key(app: &Path) -> (VaultKey, VaultId, CipherSalt) {
    let config = config(app).unwrap();
    let id = config.vault_id.unwrap();
    let cipher::Head::Salt(salt) = cipher::read_head(Path::new(&config.active_vault)).unwrap()
    else {
        panic!("not encrypted")
    };
    (key_store::unlock(app, &id, &salt).unwrap(), id, salt)
}

/// Every file under `dir`, recursively, with its bytes.
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.push((path, bytes));
        }
    }
    out
}

/// Whether two paths name the same existing file, by the host's vault token (volume serial number
/// and file index): independent of how the path is spelled (8.3 short names, case, `\\?\` prefix).
fn same_file(a: &Path, b: &Path) -> bool {
    let token = |path: &Path| {
        active_vault::vault_token(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    token(a) == token(b)
}

/// `<path>.pending`, the name a vault has while its creation awaits confirmation.
fn pending(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.pending", path.display()))
}

/// The Windows 8.3 short form of an existing path (the same path if the volume has none).
#[cfg(windows)]
fn short_path(path: &Path) -> PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buffer = vec![0u16; 4096];
    // SAFETY: `wide` is NUL-terminated and `buffer` is writable for its length.
    let length =
        unsafe { GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) }
            as usize;
    assert!(
        length > 0 && length < buffer.len(),
        "GetShortPathNameW failed"
    );
    PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]))
}

fn contains(bytes: &[u8], text: &str) -> bool {
    bytes.windows(text.len()).any(|w| w == text.as_bytes())
}

#[test]
fn an_encrypted_vault_is_created_only_after_its_phrase_is_confirmed() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let target = tmp.0.join("enc.sqlite3");
    let (begun, raw) = host_raw(
        &app,
        &[json!({ "type": "vault_create_encrypted", "requestId": "c", "path": target })],
    );
    let words: Vec<String> = begun[0]["recoveryPhrase"]
        .as_array()
        .unwrap()
        .iter()
        .map(|word| word.as_str().unwrap().to_owned())
        .collect();
    let phrase = words.join(" ");
    phrase::parse(&phrase).expect("a valid BIP-39 phrase");
    // Awaiting confirmation: nothing is active, the vault exists only under its pending name.
    assert!(config(&app).is_none());
    assert!(!target.exists());
    assert!(PathBuf::from(format!("{}.pending", target.display())).exists());
    assert!(sidecar::pending_path_for(&target).exists());
    let shown = status(&app);
    assert_eq!(shown["state"], "not_configured");
    // The host reports the canonical path, which may be spelled differently from `target` (the
    // runner's TEMP is an 8.3 short path): compare the files, not the strings.
    let creating = PathBuf::from(shown["creationPending"].as_str().unwrap());
    assert!(same_file(&pending(&creating), &pending(&target)));
    assert_eq!(shown["encryptionAvailable"], true);
    assert!(
        !shown.to_string().contains(&words[3]),
        "the phrase is shown once"
    );
    // Another creation cannot start meanwhile.
    let second = one(
        &app,
        json!({ "type": "vault_create_encrypted", "path": tmp.0.join("other.sqlite3") }),
    );
    assert_eq!(second["error"], "vault_creation_pending");
    assert!(!tmp.0.join("other.sqlite3.pending").exists());
    // A phrase other than the one shown is refused; nothing changes.
    let other = phrase::encode(
        &memocampus_native_host::secure_vault::secret::RecoverySeed::generate().unwrap(),
    );
    assert_eq!(commit(&app, &other)["error"], "recovery_phrase_rejected");
    let mut swapped = words.clone();
    swapped.swap(0, 1);
    let refused = commit(&app, &swapped.join(" "));
    assert!(
        refused["error"] == "recovery_phrase_rejected"
            || refused["error"] == "recovery_phrase_invalid",
        "{refused}"
    );
    assert!(config(&app).is_none() && !target.exists());

    let done = commit(&app, &phrase.to_ascii_uppercase());
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["status"]["state"], "ready");
    assert_eq!(done["status"]["mode"], "encrypted");
    assert_eq!(
        done["status"]["publication"], "pending",
        "MCP needs its own approval"
    );
    assert_eq!(done["status"]["creationPending"], json!(null));
    let active = config(&app).unwrap();
    assert_eq!(active.mode, VaultMode::Encrypted);
    assert!(same_file(Path::new(&active.active_vault), &target));
    assert_eq!(active.publication.unwrap().state, PublicationState::Pending);
    assert!(target.exists() && sidecar::path_for(&target).exists());
    assert!(!PathBuf::from(format!("{}.pending", target.display())).exists());
    assert!(!app.join("vault-create.json").exists());

    // Writes and reads go to the encrypted vault; nothing readable is at rest.
    let acked = one(&app, visit("v1", "https://public.test/CANARY-url"));
    assert_eq!(acked["ok"], true, "{acked}");
    let found = one(&app, json!({ "type": "search_pages" }));
    assert!(found.to_string().contains("CANARY-url"), "{found}");
    let (key, id, _) = active_key(&app);
    let db = cipher::open(&target, &key, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    drop(db);
    assert!(
        Connection::open_with_flags(&target, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row
                .get::<_, i64>(0))
            .is_err()
    );

    // Canary: no response carries the key; no file anywhere holds the key, the phrase or data in
    // the clear.
    assert!(!key.appears_in(&raw));
    let everything: Vec<(PathBuf, Vec<u8>)> = files(&tmp.0);
    for (path, bytes) in &everything {
        assert!(!key.appears_in(bytes), "{}", path.display());
        assert!(!contains(bytes, &phrase), "{}", path.display());
        assert!(
            !contains(bytes, &format!("{} {}", words[4], words[5])),
            "{}",
            path.display()
        );
        assert!(!contains(bytes, "CANARY"), "{}", path.display());
    }
    assert!(key_store::entry_path(&app, &id).exists());
}

#[test]
fn a_cancelled_or_abandoned_creation_leaves_nothing_and_never_touches_the_active_vault() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let plain = tmp.0.join("plain.sqlite3");
    assert_eq!(
        one(&app, json!({ "type": "vault_create", "path": plain }))["ok"],
        true
    );
    let before = std::fs::read(app.join("config.json")).unwrap();
    let target = tmp.0.join("enc.sqlite3");
    begin(&app, &target);
    // The plain vault stays active and writable meanwhile.
    assert_eq!(one(&app, visit("p1", "https://public.test/a"))["ok"], true);
    let cancelled = one(&app, json!({ "type": "vault_create_encrypted_cancel" }));
    assert_eq!(cancelled["ok"], true, "{cancelled}");
    assert_eq!(std::fs::read(app.join("config.json")).unwrap(), before);
    let left: Vec<String> = files(&tmp.0)
        .into_iter()
        .map(|(path, _)| path.file_name().unwrap().to_string_lossy().into_owned())
        .filter(|name| {
            name.starts_with("enc.")
                || name.ends_with(".pending")
                || name.ends_with(".json.pending")
        })
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(std::fs::read_dir(app.join("keys")).map_or(true, |dir| dir.count() == 0));
    assert!(!app.join("vault-create.json").exists());
    // The confirmation of a cancelled creation is refused.
    assert_eq!(
        commit(
            &app,
            &phrase::encode(
                &memocampus_native_host::secure_vault::secret::RecoverySeed::generate().unwrap()
            )
        )["error"],
        "vault_creation_not_pending"
    );
    // A new creation at the same place works.
    let phrase = begin(&app, &target);
    assert_eq!(commit(&app, &phrase)["ok"], true);
    // An existing file is never a target.
    let refused = one(
        &app,
        json!({ "type": "vault_create_encrypted", "path": plain }),
    );
    assert_eq!(refused["error"], "file_exists");
}

#[test]
fn a_commit_interrupted_after_promotion_completes_on_retry_with_the_same_key() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let target = tmp.0.join("enc.sqlite3");
    let phrase = begin(&app, &target);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(app.join("vault-create.json")).unwrap()).unwrap();
    let id = VaultId::parse_hex(manifest["vaultId"].as_str().unwrap()).unwrap();
    // A crash after the key entry and the sidecar were promoted, before the vault file was.
    std::fs::rename(
        key_store::pending_path(&app, &id),
        key_store::entry_path(&app, &id),
    )
    .unwrap();
    std::fs::rename(
        sidecar::pending_path_for(&target),
        sidecar::path_for(&target),
    )
    .unwrap();
    // The manifest still says awaiting_confirmation: the retry copes with half-done steps.
    let done = commit(&app, &phrase);
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(config(&app).unwrap().vault_id, Some(id));
    assert!(!key_store::pending_path(&app, &id).exists());
    assert!(!app.join("vault-create.json").exists());
    assert_eq!(status(&app)["state"], "ready");
}

#[test]
fn another_windows_user_recovers_with_the_phrase_and_only_with_it() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let target = tmp.0.join("enc.sqlite3");
    let phrase = create(&app, &target);
    assert_eq!(
        one(&app, visit("v1", "https://public.test/kept"))["ok"],
        true
    );
    let vault_bytes = std::fs::read(&target).unwrap();
    let sidecar_bytes = std::fs::read(sidecar::path_for(&target)).unwrap();

    // A fresh profile: no key store entry, no config.
    let other = tmp.other_app();
    init_policy(&other);
    let opened = one(&other, json!({ "type": "vault_open", "path": target }));
    assert_eq!(opened["error"], "key_unavailable", "{opened}");
    assert_eq!(opened["recoveryAvailable"], true);
    assert!(config(&other).is_none(), "nothing changes");

    let recover = |phrase: &str| {
        one(
            &other,
            json!({ "type": "vault_recover", "path": target, "recoveryPhrase": phrase }),
        )
    };
    let words: Vec<&str> = phrase.split(' ').collect();
    let refused = recover(&words[..11].join(" "));
    assert_eq!(refused["error"], "recovery_phrase_invalid");
    assert_eq!(refused["reason"], "word_count");
    let mut unknown = words.clone();
    unknown[6] = "memocampus";
    let refused = recover(&unknown.join(" "));
    assert_eq!(refused["reason"], "unknown_word");
    assert_eq!(refused["position"], 7);
    // Single-word changes: checksum failures or, when they pass it, refused by the envelope.
    let list = bip39::Language::English.word_list();
    let (mut checksum, mut rejected) = (0, 0);
    for replacement in list.iter().take(40) {
        if *replacement == words[11] {
            continue;
        }
        let mut changed = words.clone();
        changed[11] = replacement;
        match recover(&changed.join(" "))["error"].as_str().unwrap() {
            "recovery_phrase_invalid" => checksum += 1,
            "recovery_phrase_rejected" => rejected += 1,
            other => panic!("{other}"),
        }
    }
    assert!(checksum > 0 && rejected > 0, "{checksum} {rejected}");
    let wrong = phrase::encode(
        &memocampus_native_host::secure_vault::secret::RecoverySeed::generate().unwrap(),
    );
    assert_eq!(recover(&wrong)["error"], "recovery_phrase_rejected");
    // Nothing was written by any failed attempt.
    assert!(config(&other).is_none());
    assert!(
        !other.join("keys").exists() || std::fs::read_dir(other.join("keys")).unwrap().count() == 0
    );
    assert_eq!(std::fs::read(&target).unwrap(), vault_bytes);
    assert_eq!(
        std::fs::read(sidecar::path_for(&target)).unwrap(),
        sidecar_bytes
    );

    let recovered = recover(&format!("  {}  ", phrase.replace(' ', "\n")));
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(recovered["status"]["state"], "ready");
    assert_eq!(recovered["status"]["publication"], "pending");
    let active = config(&other).unwrap();
    assert_eq!(active.mode, VaultMode::Encrypted);
    let (key, _, _) = active_key(&other);
    let (original, _, _) = active_key(&app);
    assert!(
        key.same_as(&original),
        "the same vault key, re-protected for this profile"
    );
    assert_ne!(
        std::fs::read(key_store::entry_path(&other, &active.vault_id.unwrap())).unwrap(),
        std::fs::read(key_store::entry_path(&app, &active.vault_id.unwrap())).unwrap(),
        "a new DPAPI blob"
    );
    let found = one(&other, json!({ "type": "search_pages" }));
    assert!(found.to_string().contains("public.test/kept"), "{found}");
    // From now on it opens without the phrase.
    assert_eq!(status(&other)["state"], "ready");
}

#[test]
fn recovery_metadata_that_is_missing_swapped_or_altered_fails_closed() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let a = tmp.0.join("a.sqlite3");
    let b = tmp.0.join("b.sqlite3");
    let phrase_a = create(&app, &a);
    create(&app, &b);
    let other = tmp.other_app();
    let recover = |path: &Path, phrase: &str| {
        one(
            &other,
            json!({ "type": "vault_recover", "path": path, "recoveryPhrase": phrase }),
        )
    };
    let sidecar_a = sidecar::path_for(&a);
    let saved = std::fs::read(&sidecar_a).unwrap();
    // Missing.
    std::fs::remove_file(&sidecar_a).unwrap();
    assert_eq!(recover(&a, &phrase_a)["error"], "recovery_metadata_missing");
    // B's sidecar beside A: another salt.
    std::fs::copy(sidecar::path_for(&b), &sidecar_a).unwrap();
    assert_eq!(
        recover(&a, &phrase_a)["error"],
        "recovery_metadata_mismatch"
    );
    // Altered wrapped key: the right phrase, damaged metadata.
    let mut value: Value = serde_json::from_slice(&saved).unwrap();
    let wrapped = value["recovery"]["wrapped"].as_str().unwrap().to_owned();
    let flipped = format!(
        "{}{}",
        if wrapped.starts_with('0') { "1" } else { "0" },
        &wrapped[1..]
    );
    value["recovery"]["wrapped"] = flipped.into();
    std::fs::write(&sidecar_a, value.to_string()).unwrap();
    assert_eq!(recover(&a, &phrase_a)["error"], "recovery_metadata_damaged");
    // Truncated.
    std::fs::write(&sidecar_a, &saved[..saved.len() / 2]).unwrap();
    assert_eq!(recover(&a, &phrase_a)["error"], "recovery_metadata_damaged");
    // A vault id swapped inside the sidecar: the envelope is bound to it.
    let mut value: Value = serde_json::from_slice(&saved).unwrap();
    value["vaultId"] = VaultId::generate().unwrap().to_hex().into();
    std::fs::write(&sidecar_a, value.to_string()).unwrap();
    assert_eq!(recover(&a, &phrase_a)["error"], "recovery_phrase_rejected");
    assert!(config(&other).is_none());
    std::fs::write(&sidecar_a, &saved).unwrap();
    assert_eq!(recover(&a, &phrase_a)["ok"], true);
    // A plain vault is not recovered.
    let plain = tmp.0.join("plain.sqlite3");
    assert_eq!(
        one(&other, json!({ "type": "vault_create", "path": plain }))["ok"],
        true
    );
    assert_eq!(recover(&plain, &phrase_a)["error"], "vault_not_encrypted");
}

#[test]
fn a_wrong_phrase_never_replaces_a_key_entry_and_a_right_one_repairs_a_damaged_entry() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let target = tmp.0.join("enc.sqlite3");
    let phrase = create(&app, &target);
    let id = config(&app).unwrap().vault_id.unwrap();
    let entry = key_store::entry_path(&app, &id);
    // The entry is damaged (as an entry from another profile would be unusable).
    let mut value: Value = serde_json::from_slice(&std::fs::read(&entry).unwrap()).unwrap();
    value["kcv"] = "00".repeat(32).into();
    std::fs::write(&entry, value.to_string()).unwrap();
    let damaged = std::fs::read(&entry).unwrap();
    let shown = status(&app);
    assert_eq!(shown["state"], "key_unavailable");
    assert_eq!(shown["recoveryAvailable"], true);
    // Writes are refused with a temporary error, never acknowledged.
    let refused = one(&app, visit("v1", "https://public.test/x"));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["error"], "key_unavailable");
    let wrong = phrase::encode(
        &memocampus_native_host::secure_vault::secret::RecoverySeed::generate().unwrap(),
    );
    let recover = |phrase: &str| {
        one(
            &app,
            json!({ "type": "vault_recover", "path": target, "recoveryPhrase": phrase }),
        )
    };
    assert_eq!(recover(&wrong)["error"], "recovery_phrase_rejected");
    assert_eq!(
        std::fs::read(&entry).unwrap(),
        damaged,
        "never overwritten by a wrong phrase"
    );
    let repaired = recover(&phrase);
    assert_eq!(repaired["ok"], true, "{repaired}");
    assert_eq!(
        repaired["status"]["publication"], "pending",
        "a recovered vault starts pending"
    );
    assert_ne!(std::fs::read(&entry).unwrap(), damaged);
    assert_eq!(one(&app, visit("v1", "https://public.test/x"))["ok"], true);
}

#[test]
fn an_encrypted_vault_is_never_backed_up_in_the_clear_and_switches_both_ways() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let plain = tmp.0.join("plain.sqlite3");
    assert_eq!(
        one(&app, json!({ "type": "vault_create", "path": plain }))["ok"],
        true
    );
    let target = tmp.0.join("enc.sqlite3");
    create(&app, &target);
    let token = status(&app)["vaultToken"].clone();
    let dir = tmp.0.join("backups");
    std::fs::create_dir_all(&dir).unwrap();
    let refused = one(
        &app,
        json!({ "type": "vault_backup", "directory": dir, "expectedVaultToken": token }),
    );
    assert_eq!(refused["error"], "vault_backup_encrypted_unsupported");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "nothing written"
    );
    // Publication decisions work as for a plain vault.
    let confirmed = one(
        &app,
        json!({ "type": "vault_publication_confirm", "expectedVaultToken": token }),
    );
    assert_eq!(
        confirmed["status"]["publication"], "confirmed",
        "{confirmed}"
    );
    // To the plain vault and back: the encrypted vault opens with its key, decision kept for the
    // same file.
    assert_eq!(
        one(&app, json!({ "type": "vault_open", "path": plain }))["ok"],
        true
    );
    assert_eq!(config(&app).unwrap().mode, VaultMode::Plain);
    assert_eq!(config(&app).unwrap().vault_id, None);
    let back = one(&app, json!({ "type": "vault_open", "path": target }));
    assert_eq!(back["ok"], true, "{back}");
    assert_eq!(back["status"]["mode"], "encrypted");
    // A plain file opened as before; a junk file is still not a vault.
    let junk = tmp.0.join("junk.sqlite3");
    std::fs::write(&junk, b"definitely not sqlite, and long enough").unwrap();
    assert_eq!(
        one(&app, json!({ "type": "vault_open", "path": junk }))["error"],
        "vault_invalid"
    );
}

#[test]
fn an_unreadable_creation_record_can_still_be_discarded_and_blocks_nothing_else() {
    let tmp = TempDir::new();
    let app = tmp.app();
    std::fs::write(app.join("vault-create.json"), b"{ damaged").unwrap();
    assert_eq!(status(&app)["creationPending"], "");
    let target = tmp.0.join("enc.sqlite3");
    let refused = one(
        &app,
        json!({ "type": "vault_create_encrypted", "path": target }),
    );
    assert_eq!(refused["error"], "vault_creation_damaged");
    assert_eq!(
        one(&app, json!({ "type": "vault_create_encrypted_cancel" }))["ok"],
        true
    );
    assert!(!app.join("vault-create.json").exists());
    assert_eq!(status(&app)["creationPending"], json!(null));
    init_policy(&app);
    create(&app, &target);
}

#[cfg(windows)]
#[test]
fn a_vault_named_by_its_8_3_short_path_is_the_same_file_and_a_copy_is_not() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let long_dir = tmp.0.join("Memocampus long directory name");
    std::fs::create_dir_all(&long_dir).unwrap();
    let short_dir = short_path(&long_dir);
    if short_dir.file_name() == long_dir.file_name() {
        eprintln!("SKIPPED: this volume generates no 8.3 short names");
        return;
    }
    let (short, long) = (short_dir.join("enc.sqlite3"), long_dir.join("enc.sqlite3"));
    assert_ne!(short, long);

    let phrase = begin(&app, &short);
    let creating = PathBuf::from(status(&app)["creationPending"].as_str().unwrap());
    assert_ne!(
        creating, short,
        "spelled differently: a string comparison would fail"
    );
    assert!(same_file(&pending(&creating), &pending(&short)));
    assert!(same_file(&pending(&creating), &pending(&long)));

    let done = commit(&app, &phrase);
    assert_eq!(done["ok"], true, "{done}");
    let active = PathBuf::from(config(&app).unwrap().active_vault);
    assert!(same_file(&active, &short) && same_file(&active, &long));
    // Another file, even a byte-for-byte copy in the same directory, is not the same file.
    let copy = long_dir.join("copy.sqlite3");
    std::fs::copy(&long, &copy).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), std::fs::read(&long).unwrap());
    assert!(!same_file(&active, &copy));
    assert!(!same_file(&active, &short_path(&copy)));
}
