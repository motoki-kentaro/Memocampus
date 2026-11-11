//! Issue #51, Slice B (Slice E key derivation), through the real host binary: creating an encrypted
//! vault with a Recovery Spell (begin → full re-entry → commit, or cancel), writing to it, opening
//! it, and recovering it with the spell where this Windows user has no key. Temporary app
//! directories and vaults only; the key store is the temporary app directory's, protected with this
//! user's DPAPI.

use memocampus_native_host::active_vault::{self, PublicationState, VaultMode};
use memocampus_native_host::secure_vault::secret::RecoverySpell;
use memocampus_native_host::secure_vault::secret::{CipherSalt, VaultId, VaultKey};
use memocampus_native_host::secure_vault::{cipher, key_store, recovery, sidecar, spell};
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
    memocampus_native_host::policy_store::PolicyStore::new(app.to_owned())
        .unwrap()
        .initialize()
        .unwrap();
}

/// Begins a creation at `target`; returns the spell (43 base64url characters).
fn begin(app: &Path, target: &Path) -> String {
    let begun = one(
        app,
        json!({ "type": "vault_create_encrypted", "path": target }),
    );
    assert_eq!(begun["ok"], true, "{begun}");
    let shown = begun["recoverySpell"].as_str().unwrap().to_owned();
    assert_eq!(shown.len(), 43);
    shown
}

fn commit(app: &Path, spell: &str) -> Value {
    one(
        app,
        json!({ "type": "vault_create_encrypted_commit", "recoverySpell": spell }),
    )
}

/// Creates an encrypted vault at `target` and makes it active; returns its spell.
fn create(app: &Path, target: &Path) -> String {
    let shown = begin(app, target);
    let done = commit(app, &shown);
    assert_eq!(done["ok"], true, "{done}");
    shown
}

/// A well-formed spell nobody was shown.
fn another_spell() -> String {
    spell::encode(&RecoverySpell::generate().unwrap()).to_string()
}

/// `text` with the character at `position` replaced by another alphabet character that keeps the
/// spell canonical (so the change reaches the envelope, not the syntax check).
fn one_character_off(text: &str, position: usize) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    chars[position] = if chars[position] == 'A' { 'Q' } else { 'A' };
    chars.into_iter().collect()
}

/// Writes a recovery sidecar next to `vault` as Slices B–D did (`version` 1: 12-word phrase, 2:
/// spell envelope), naming `salt`.
fn write_earlier_sidecar(vault: &Path, version: u32, salt: &CipherSalt) {
    let scheme = match version {
        1 => "bip39-128/hkdf-sha256/xchacha20poly1305/v1",
        _ => "recovery-spell-256/hkdf-sha256/xchacha20poly1305/v2",
    };
    let value = json!({
        "version": version, "format": "memocampus-vault-recovery",
        "vaultId": VaultId::generate().unwrap().to_hex(), "cipherSalt": salt.to_hex(),
        "recovery": { "scheme": scheme, "salt": "11".repeat(32), "nonce": "22".repeat(24),
                      "wrapped": "33".repeat(48), "check": "44".repeat(32) },
    });
    std::fs::write(
        sidecar::path_for(vault),
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();
}

fn salt_of(path: &Path) -> CipherSalt {
    match cipher::read_head(path).unwrap() {
        cipher::Head::Salt(salt) => salt,
        other => panic!("{other:?}"),
    }
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
fn an_encrypted_vault_is_created_only_after_its_whole_spell_is_entered_again() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let target = tmp.0.join("enc.sqlite3");
    let (begun, raw) = host_raw(
        &app,
        &[json!({ "type": "vault_create_encrypted", "requestId": "c", "path": target })],
    );
    let shown = begun[0]["recoverySpell"].as_str().unwrap().to_owned();
    assert_eq!(shown.len(), 43);
    assert!(shown
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    spell::parse(&shown).expect("a canonical spell");
    assert!(
        begun[0].get("recoveryPhrase").is_none(),
        "no 12-word phrase any more"
    );
    // Awaiting confirmation: nothing is active, the vault exists only under its pending name.
    assert!(config(&app).is_none());
    assert!(!target.exists());
    assert!(PathBuf::from(format!("{}.pending", target.display())).exists());
    assert!(
        !sidecar::pending_path_for(&target).exists(),
        "no sidecar (Slice E)"
    );
    let now = status(&app);
    assert_eq!(now["state"], "not_configured");
    // The host reports the canonical path, which may be spelled differently from `target` (the
    // runner's TEMP is an 8.3 short path): compare the files, not the strings.
    let creating = PathBuf::from(now["creationPending"].as_str().unwrap());
    assert!(same_file(&pending(&creating), &pending(&target)));
    assert_eq!(now["encryptionAvailable"], true);
    assert!(
        !shown_status_contains(&app, &shown),
        "the spell is shown once: no status carries it"
    );
    // Another creation cannot start meanwhile.
    let second = one(
        &app,
        json!({ "type": "vault_create_encrypted", "path": tmp.0.join("other.sqlite3") }),
    );
    assert_eq!(second["error"], "vault_creation_pending");
    assert!(!tmp.0.join("other.sqlite3.pending").exists());
    // Anything but the whole spell shown is refused, and the creation stays pending for a retry.
    assert_eq!(
        commit(&app, &another_spell())["error"],
        "recovery_spell_rejected"
    );
    for position in [0, 21, 41] {
        assert_eq!(
            commit(&app, &one_character_off(&shown, position))["error"],
            "recovery_spell_rejected",
            "{position}"
        );
    }
    let short = commit(&app, &shown[..42]);
    assert_eq!(short["error"], "recovery_spell_invalid");
    assert_eq!(short["reason"], "length");
    assert_eq!(short["count"], 42);
    let partial = commit(&app, &shown[..8]);
    assert_eq!(
        partial["error"], "recovery_spell_invalid",
        "no short confirmation"
    );
    // Case-sensitive; spaces around or inside are not repaired by the host.
    let flipped: String = shown
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    assert_ne!(commit(&app, &flipped)["ok"], true);
    assert_eq!(
        commit(&app, &format!(" {shown}"))["error"],
        "recovery_spell_invalid"
    );
    let spaced = format!("{} {}", &shown[..20], &shown[21..]);
    let refused = commit(&app, &spaced);
    assert_eq!(refused["error"], "recovery_spell_invalid");
    assert_eq!(refused["reason"], "character");
    assert_eq!(refused["position"], 21);
    let missing = one(&app, json!({ "type": "vault_create_encrypted_commit" }));
    assert_eq!(missing["error"], "recovery_spell_invalid");
    for refused in [&short, &refused, &missing] {
        assert!(!refused.to_string().contains(&shown[..20]), "{refused}");
    }
    assert!(config(&app).is_none() && !target.exists());
    assert!(status(&app)["creationPending"].is_string());

    let done = commit(&app, &shown);
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
    assert!(target.exists() && !sidecar::path_for(&target).exists());
    assert!(!PathBuf::from(format!("{}.pending", target.display())).exists());
    // The vault id is the one the file's salt names.
    assert_eq!(active.vault_id, Some(recovery::vault_id(&salt_of(&target))));
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

    // Canary: no response carries the key; no file anywhere holds the key, the spell (or half of
    // it, or its standard-base64 spelling) or data in the clear.
    assert!(!key.appears_in(&raw));
    let standard = shown.replace('-', "+").replace('_', "/");
    let everything: Vec<(PathBuf, Vec<u8>)> = files(&tmp.0);
    for (path, bytes) in &everything {
        assert!(!key.appears_in(bytes), "{}", path.display());
        assert!(!contains(bytes, &shown), "{}", path.display());
        assert!(!contains(bytes, &shown[..21]), "{}", path.display());
        assert!(!contains(bytes, &shown[21..]), "{}", path.display());
        assert!(!contains(bytes, &standard), "{}", path.display());
        assert!(!contains(bytes, "CANARY"), "{}", path.display());
    }
    assert!(!shown_status_contains(&app, &shown));
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
        commit(&app, &another_spell())["error"],
        "vault_creation_not_pending"
    );
    // A new creation at the same place works, with a new spell.
    let shown = begin(&app, &target);
    assert_eq!(commit(&app, &shown)["ok"], true);
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
    let shown = begin(&app, &target);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(app.join("vault-create.json")).unwrap()).unwrap();
    let id = VaultId::parse_hex(manifest["vaultId"].as_str().unwrap()).unwrap();
    // A crash after the key entry was promoted, before the vault file was.
    std::fs::rename(
        key_store::pending_path(&app, &id),
        key_store::entry_path(&app, &id),
    )
    .unwrap();
    // The manifest still says awaiting_confirmation: the retry copes with half-done steps.
    let done = commit(&app, &shown);
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(config(&app).unwrap().vault_id, Some(id));
    assert!(!key_store::pending_path(&app, &id).exists());
    assert!(!app.join("vault-create.json").exists());
    assert_eq!(status(&app)["state"], "ready");
}

#[test]
fn another_windows_user_recovers_with_the_spell_and_only_with_it() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let target = tmp.0.join("enc.sqlite3");
    let shown = create(&app, &target);
    assert_eq!(
        one(&app, visit("v1", "https://public.test/kept"))["ok"],
        true
    );
    let vault_bytes = std::fs::read(&target).unwrap();

    // A fresh profile: no key store entry, no config.
    let other = tmp.other_app();
    init_policy(&other);
    let opened = one(&other, json!({ "type": "vault_open", "path": target }));
    assert_eq!(opened["error"], "key_unavailable", "{opened}");
    assert_eq!(opened["recoveryAvailable"], true);
    assert!(config(&other).is_none(), "nothing changes");

    let recover = |spell: &str| {
        one(
            &other,
            json!({ "type": "vault_recover", "path": target, "recoverySpell": spell }),
        )
    };
    let refused = recover(&shown[..42]);
    assert_eq!(refused["error"], "recovery_spell_invalid");
    assert_eq!(refused["reason"], "length");
    let refused = recover(&format!("{}A", shown));
    assert_eq!(refused["reason"], "length");
    assert_eq!(refused["count"], 44);
    for (position, bad) in [(6, '+'), (7, '/'), (42, '='), (30, '.')] {
        let mut chars: Vec<char> = shown.chars().collect();
        chars[position] = bad;
        let refused = recover(&chars.into_iter().collect::<String>());
        assert_eq!(refused["error"], "recovery_spell_invalid");
        assert_eq!(refused["reason"], "character", "{bad}");
        assert_eq!(refused["position"], position + 1);
    }
    let full_width: String = shown
        .chars()
        .enumerate()
        .map(|(i, c)| if i == 3 { '\u{FF21}' } else { c })
        .collect();
    assert_eq!(recover(&full_width)["reason"], "character");
    // A last character with its unused bits set is not rounded to the spell.
    let mut chars: Vec<char> = shown.chars().collect();
    let canonical = "AEIMQUYcgkosw048";
    let last = chars[42];
    assert!(canonical.contains(last));
    chars[42] = "BFJNRVZdhlptx159"
        .chars()
        .nth(canonical.find(last).unwrap())
        .unwrap();
    assert_eq!(
        recover(&chars.into_iter().collect::<String>())["reason"],
        "non_canonical"
    );
    // Single-character changes that stay canonical derive another key, refused by the vault.
    for position in [0, 10, 20, 30, 41] {
        assert_eq!(
            recover(&one_character_off(&shown, position))["error"],
            "recovery_spell_rejected",
            "{position}"
        );
    }
    assert_eq!(
        recover(&another_spell())["error"],
        "recovery_spell_rejected"
    );
    // Nothing was written by any failed attempt.
    assert!(config(&other).is_none());
    assert!(
        !other.join("keys").exists() || std::fs::read_dir(other.join("keys")).unwrap().count() == 0
    );
    assert_eq!(std::fs::read(&target).unwrap(), vault_bytes);

    // The host takes the spell as is: surrounding spaces are the panel's to trim.
    assert_eq!(
        recover(&format!("  {shown}  "))["error"],
        "recovery_spell_invalid"
    );
    let recovered = recover(&shown);
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
    // From now on it opens without the spell.
    assert_eq!(status(&other)["state"], "ready");
}

#[test]
fn recovery_needs_the_vault_file_and_its_spell_only_and_refuses_anything_altered() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let a = tmp.0.join("a.sqlite3");
    let b = tmp.0.join("b.sqlite3");
    let spell_a = create(&app, &a);
    let spell_b = create(&app, &b);
    assert_eq!(one(&app, visit("v1", "https://public.test/b"))["ok"], true);
    let other = tmp.other_app();
    let recover = |path: &Path, spell: &str| {
        one(
            &other,
            json!({ "type": "vault_recover", "path": path, "recoverySpell": spell }),
        )
    };
    // Another vault's spell, or the right spell for another file: refused.
    assert_eq!(recover(&a, &spell_b)["error"], "recovery_spell_rejected");
    assert_eq!(recover(&b, &spell_a)["error"], "recovery_spell_rejected");
    let saved = std::fs::read(&b).unwrap();
    let altered = tmp.0.join("altered.sqlite3");
    // The salt (the public input of the key) altered: another key, refused, never misread.
    let mut bytes = saved.clone();
    bytes[5] ^= 0x40;
    std::fs::write(&altered, &bytes).unwrap();
    assert_eq!(
        recover(&altered, &spell_b)["error"],
        "recovery_spell_rejected"
    );
    // A page altered: SQLCipher's page authentication refuses it before anything is stored.
    for position in [4096 + 200, saved.len() - 300] {
        let mut bytes = saved.clone();
        bytes[position] ^= 0x01;
        std::fs::write(&altered, &bytes).unwrap();
        let refused = recover(&altered, &spell_b);
        assert_eq!(refused["ok"], false, "{position}: {refused}");
    }
    // Truncated to a partial page: not a vault at all.
    std::fs::write(&altered, &saved[..saved.len() - 100]).unwrap();
    assert_eq!(recover(&altered, &spell_b)["error"], "vault_invalid");
    // Nothing was written by any refusal.
    assert!(config(&other).is_none());
    assert!(
        !other.join("keys").exists() || std::fs::read_dir(other.join("keys")).unwrap().count() == 0
    );
    // A stray sidecar of another salt beside the vault plays no part.
    write_earlier_sidecar(&b, 2, &CipherSalt::from_array([7; 16]));
    let recovered = recover(&b, &spell_b);
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(recovered["status"]["publication"], "pending");
    // A plain vault is not recovered.
    let plain = tmp.0.join("plain.sqlite3");
    assert_eq!(
        one(&other, json!({ "type": "vault_create", "path": plain }))["ok"],
        true
    );
    assert_eq!(recover(&plain, &spell_a)["error"], "vault_not_encrypted");
}
#[test]
fn a_wrong_spell_never_replaces_a_key_entry_and_a_right_one_repairs_a_damaged_entry() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let target = tmp.0.join("enc.sqlite3");
    let right = create(&app, &target);
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
    let wrong = another_spell();
    let recover = |spell: &str| {
        one(
            &app,
            json!({ "type": "vault_recover", "path": target, "recoverySpell": spell }),
        )
    };
    assert_eq!(recover(&wrong)["error"], "recovery_spell_rejected");
    assert_eq!(
        std::fs::read(&entry).unwrap(),
        damaged,
        "never overwritten by a wrong spell"
    );
    let repaired = recover(&right);
    assert_eq!(repaired["ok"], true, "{repaired}");
    assert_eq!(
        repaired["status"]["publication"], "pending",
        "a recovered vault starts pending"
    );
    assert_ne!(std::fs::read(&entry).unwrap(), damaged);
    assert_eq!(one(&app, visit("v1", "https://public.test/x"))["ok"], true);
}

#[test]
fn an_encrypted_vault_is_backed_up_only_encrypted_and_switches_both_ways() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let plain = tmp.0.join("plain.sqlite3");
    assert_eq!(
        one(&app, json!({ "type": "vault_create", "path": plain }))["ok"],
        true
    );
    let target = tmp.0.join("enc.sqlite3");
    let shown = create(&app, &target);
    let token = status(&app)["vaultToken"].clone();
    let dir = tmp.0.join("backups");
    std::fs::create_dir_all(&dir).unwrap();
    // Slice C: a normal backup of an encrypted vault is format B, still encrypted (Slice B refused
    // it, `vault_backup_encrypted_unsupported`, rather than write a plain copy).
    let backed_up = one(
        &app,
        json!({ "type": "vault_backup", "directory": dir, "expectedVaultToken": token,
                "recoverySpell": shown }),
    );
    assert_eq!(backed_up["format"], "B", "{backed_up}");
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(names.len(), 1, "one complete file, nothing partial");
    let bytes = std::fs::read(names[0].path()).unwrap();
    assert!(!contains(&bytes, "SQLite format 3"), "never a plain copy");
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

    let shown = begin(&app, &short);
    let creating = PathBuf::from(status(&app)["creationPending"].as_str().unwrap());
    assert_ne!(
        creating, short,
        "spelled differently: a string comparison would fail"
    );
    assert!(same_file(&pending(&creating), &pending(&short)));
    assert!(same_file(&pending(&creating), &pending(&long)));

    let done = commit(&app, &shown);
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

/// True if the status of `app` (as the panel would read it) mentions `secret` anywhere.
fn shown_status_contains(app: &Path, secret: &str) -> bool {
    status(app).to_string().contains(secret)
}

#[test]
fn a_vault_of_an_earlier_scheme_is_refused_as_unsupported_and_left_as_it_is() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    // As Slices B–D made it: a random key (not derived from any spell), its own salt, a
    // `.mckey` beside it and a version 1 key store entry under a random vault id.
    let target = tmp.0.join("old.sqlite3");
    let key = VaultKey::generate().unwrap();
    let salt = CipherSalt::generate().unwrap();
    cipher::create(&target, &key, &salt).unwrap();
    let old_id = VaultId::generate().unwrap();
    for version in [1, 2] {
        write_earlier_sidecar(&target, version, &salt);
        let sidecar_bytes = std::fs::read(sidecar::path_for(&target)).unwrap();
        let vault_bytes = std::fs::read(&target).unwrap();
        let opened = one(&app, json!({ "type": "vault_open", "path": target }));
        assert_eq!(opened["error"], "vault_legacy_unsupported", "{opened}");
        assert_eq!(opened["recoveryAvailable"], false);
        let recovered = one(
            &app,
            json!({ "type": "vault_recover", "path": target, "recoverySpell": another_spell() }),
        );
        assert_eq!(
            recovered["error"], "vault_legacy_unsupported",
            "{recovered}"
        );
        assert!(config(&app).is_none());
        // Nothing is changed, migrated or deleted.
        assert_eq!(std::fs::read(&target).unwrap(), vault_bytes);
        assert_eq!(
            std::fs::read(sidecar::path_for(&target)).unwrap(),
            sidecar_bytes
        );
    }
    // Without its sidecar, a version 1 key store entry naming its salt identifies it as well.
    std::fs::remove_file(sidecar::path_for(&target)).unwrap();
    let keys = app.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let entry = keys.join(format!("{}.json", old_id.to_hex()));
    std::fs::write(
        &entry,
        json!({ "version": 1, "vaultId": old_id.to_hex(), "cipherSalt": salt.to_hex(),
                "protector": "dpapi-user", "protected": "AAAA", "kcv": "00".repeat(32) })
        .to_string(),
    )
    .unwrap();
    let entry_bytes = std::fs::read(&entry).unwrap();
    let opened = one(&app, json!({ "type": "vault_open", "path": target }));
    assert_eq!(opened["error"], "vault_legacy_unsupported", "{opened}");
    // An earlier config naming it: the status says so, and nothing is offered for it.
    std::fs::write(
        app.join("config.json"),
        serde_json::to_vec(&active_vault::Config::encrypted(
            target.to_string_lossy().into_owned(),
            false,
            old_id,
        ))
        .unwrap(),
    )
    .unwrap();
    let now = status(&app);
    assert_eq!(now["state"], "vault_legacy_unsupported", "{now}");
    assert_eq!(now["recoveryAvailable"], false);
    assert_eq!(now["publication"], "pending");
    let refused = one(&app, visit("v1", "https://public.test/x"));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(std::fs::read(&entry).unwrap(), entry_bytes, "never removed");
    // A spell of this scheme is never taken for its key.
    let _ = key;
}
#[test]
fn a_creation_begun_by_an_earlier_scheme_cannot_complete_and_is_discarded() {
    let tmp = TempDir::new();
    let app = tmp.app();
    init_policy(&app);
    let target = tmp.0.join("enc.sqlite3");
    let shown = begin(&app, &target);
    // As an earlier build recorded it (manifest version 1, with a sidecar): unreadable here.
    let path = app.join("vault-create.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["version"] = 1.into();
    std::fs::write(&path, manifest.to_string()).unwrap();
    let refused = commit(&app, &shown);
    assert_eq!(refused["error"], "vault_creation_damaged", "{refused}");
    assert!(config(&app).is_none() && !target.exists());
    assert_eq!(
        one(&app, json!({ "type": "vault_create_encrypted_cancel" }))["ok"],
        true
    );
    assert!(!path.exists());
    // Its files cannot be proven to be that creation's: they stay, under pending names that can
    // never become a vault, and the next creation picks another place.
    assert!(pending(&target).exists() && !target.exists());
    create(&app, &tmp.0.join("enc2.sqlite3"));
}
