//! Issue #51, Slice C: backups A/B/C/D and restore, through the side panel messages, with
//! temporary app directories and vaults only. A second app directory stands for another PC (or a
//! reinstalled Windows): it has no key store entry, no config and no policy of the first.

use super::backup::{self, Fault, Step, FAULT};
use super::*;
use crate::vault::tests::TempDir;
use crate::vault::Environment;
use memocampus_native_host::secure_vault::{cipher, container, key_store, recovery, sidecar};
use std::cell::RefCell;
use std::fs;

struct Dialog {
    dir: RefCell<Option<PathBuf>>,
    file: RefCell<Option<PathBuf>>,
    target: RefCell<Option<PathBuf>>,
}

impl Dialog {
    fn none() -> Self {
        Self {
            dir: RefCell::new(None),
            file: RefCell::new(None),
            target: RefCell::new(None),
        }
    }
}

impl Dialogs for Dialog {
    fn choose_new_vault(&self, _: &Path, _: &str) -> Option<PathBuf> {
        None
    }
    fn choose_existing_vault(&self, _: Option<&Path>) -> Option<PathBuf> {
        None
    }
    fn choose_backup_dir(&self) -> Option<PathBuf> {
        self.dir.borrow().clone()
    }
    fn choose_export_file(&self, _: &str, _: &str) -> Option<PathBuf> {
        None
    }
    fn choose_backup_file(&self) -> Option<PathBuf> {
        self.file.borrow().clone()
    }
    fn choose_restore_target(&self, name: &str) -> Option<PathBuf> {
        assert_eq!(name, "memocampus-restored.sqlite3");
        self.target.borrow().clone()
    }
}

fn send(session: &mut Session, message: Value) -> Value {
    send_with(session, &Dialog::none(), message)
}

fn send_with(session: &mut Session, dialogs: &Dialog, message: Value) -> Value {
    let mut message = message;
    message["requestId"] = "r".into();
    let response = handle(session, dialogs, &message);
    assert_eq!(response["type"], "vault_result");
    response
}

/// Environments of "this PC" (`app`) and "another PC" (`app2`) in one temporary directory.
fn env_of(tmp: &TempDir, app: &str) -> Environment {
    Environment {
        app_dir: Some(tmp.0.join(app)),
        ..tmp.env()
    }
}

const CANARIES: [&str; 6] = [
    "CANARY-url",
    "CANARY-title",
    "CANARY-image",
    "canary-tag-secret",
    "CANARY-blob",
    "CANARY-MHTML-original",
];

#[test]
fn page_management_after_plain_and_secure_restore_preserves_other_content() {
    for encrypted in [false, true] {
        let tmp = TempDir::new();
        let mut source = Session::new(tmp.env());
        let path = tmp.0.join("source.sqlite3");
        let spell = if encrypted {
            Some(create_encrypted(&mut source, &path))
        } else {
            create_plain(&mut source, &path);
            None
        };
        fill(&mut source);
        let before = active_dump(&mut source);
        let dir = tmp.0.join("backup");
        fs::create_dir(&dir).unwrap();
        let done = backup_to(&mut source, &dir, json!({}));
        assert_eq!(done["ok"], true, "{done}");
        let mut target = Session::new(env_of(&tmp, "other-app"));
        let extra = spell.map_or(json!({}), |s| json!({"recoverySpell":s}));
        let restored = restore_to(
            &mut target,
            &done["backupPath"],
            &tmp.0.join("restored.sqlite3"),
            extra,
        );
        assert_eq!(restored["ok"], true, "{restored}");
        assert_eq!(activate(&mut target, &restored)["ok"], true);
        assert_eq!(active_dump(&mut target), before);
        let mut pages = crate::page_manage::Manager::default();
        let binding = pages.handle(&mut target, &json!({"type":"page_state","requestId":"s"}));
        let mut call = |kind: &str, extra: Value| {
            let mut message = json!({"type":kind,"requestId":"p","vaultToken":binding["vaultToken"],"generation":binding["generation"]});
            message
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            pages.handle(&mut target, &message)
        };
        let listed = call("page_list", json!({}));
        assert_eq!(listed["items"].as_array().unwrap().len(), 1, "{listed}");
        let preview = call(
            "page_delete_preview",
            json!({"target":{"scope":"filter","query":""}}),
        );
        assert_eq!(preview["counts"]["events"], 1);
        assert_eq!(
            call("page_delete", json!({"reviewId":preview["reviewId"]}))["ok"],
            true
        );
        target
            .read(|db| {
                assert_eq!(vault::counts(db).unwrap().pages, 0);
                assert_eq!(vault::counts(db).unwrap().visits, 1);
                assert_eq!(crate::images::table_counts(db).unwrap().1, 1);
            })
            .unwrap();
        assert_eq!(
            active_dump(&mut source),
            before,
            "source and backup were not deleted"
        );
    }
}

/// Writes rows with canary strings into the active vault: a visit, an image blob, a saved image
/// and a tag.
fn fill(session: &mut Session) {
    session
        .write(|db| {
            db.execute_batch(&format!(
                "INSERT INTO visits VALUES ('v1', 'https://canary.test/CANARY-url', 'CANARY-title', '2026-11-09T10:00:00.000Z');
                 INSERT INTO image_blobs (id, sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data)
                   VALUES (1, '{}', 'image/png', 11, 1, 1, 0, 0, CAST('CANARY-blob' AS BLOB));
                 INSERT INTO saved_images (save_id, blob_id, image_url, saved_at, incognito, mcp_public)
                   VALUES ('s1', 1, 'https://canary.test/CANARY-image', 0, 0, 0);
                 INSERT INTO image_tags (id, name) VALUES (1, 'canary-tag-secret');
                 INSERT INTO image_tag_links VALUES (1, 1);",
                "a".repeat(64)
            ))
            ?;
            let bytes = b"MIME-Version: 1.0\r\n\r\nCANARY-MHTML-original\x00\xff";
            use sha2::Digest;
            crate::pages::insert(db, "00000000-0000-4000-8000-000000000006", &crate::pages::Metadata { url: "https://synthetic.test/CANARY-page".into(), title: "CANARY-page-title".into(), captured_at: 1, incognito: false }, bytes, &format!("{:x}", sha2::Sha256::digest(bytes)), 2).map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok(0)
        })
        .unwrap();
}

/// Every row of the vault at `path`, as text, for comparing a restored vault with its source.
fn dump(db: &rusqlite::Connection) -> Vec<String> {
    let mut rows = Vec::new();
    for (table, order) in [
        ("visits", "event_id"),
        ("image_blobs", "id"),
        ("saved_images", "id"),
        ("image_tags", "id"),
        ("image_tag_links", "blob_id"),
        ("tab_snapshots", "event_id"),
        ("page_blobs", "id"),
        ("saved_pages", "id"),
    ] {
        let mut statement = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
            .unwrap();
        let columns = statement.column_count();
        let mut query = statement.query([]).unwrap();
        while let Some(row) = query.next().unwrap() {
            let values: Vec<String> = (0..columns)
                .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                .collect();
            rows.push(format!("{table}: {}", values.join("|")));
        }
    }
    rows
}

fn active_dump(session: &mut Session) -> Vec<String> {
    session.read(dump).unwrap()
}

fn create_plain(session: &mut Session, path: &Path) {
    let created = send(session, json!({ "type": "vault_create", "path": path }));
    assert_eq!(created["ok"], true, "{created}");
}

thread_local! {
    /// The Recovery Spell of each encrypted vault the tests created, by path: what the user holds.
    static SPELLS: RefCell<std::collections::HashMap<String, String>> = RefCell::default();
}

/// Creates an encrypted vault and returns its spell.
fn create_encrypted(session: &mut Session, path: &Path) -> String {
    let begun = send(
        session,
        json!({ "type": "vault_create_encrypted", "path": path }),
    );
    assert_eq!(begun["ok"], true, "{begun}");
    let spell = begun["recoverySpell"].as_str().unwrap().to_owned();
    assert_eq!(spell.len(), 43);
    let done = send(
        session,
        json!({ "type": "vault_create_encrypted_commit", "recoverySpell": spell }),
    );
    assert_eq!(done["ok"], true, "{done}");
    SPELLS.with(|spells| {
        spells
            .borrow_mut()
            .insert(done["path"].as_str().unwrap().to_owned(), spell.clone())
    });
    spell
}

fn token(session: &mut Session) -> Value {
    let shown = send(session, json!({ "type": "vault_status" }));
    shown["status"]["vaultToken"].clone()
}

/// A backup of the active vault. An encrypted vault's backup (B, D) carries the spell the user
/// holds, unless `extra` sets `recoverySpell` itself.
fn backup_to(session: &mut Session, dir: &Path, extra: Value) -> Value {
    let shown = send(session, json!({ "type": "vault_status" }));
    let token = shown["status"]["vaultToken"].clone();
    let mut message =
        json!({ "type": "vault_backup", "expectedVaultToken": token, "directory": dir });
    if shown["status"]["mode"] == "encrypted" {
        let path = shown["status"]["vaultPath"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if let Some(spell) = SPELLS.with(|spells| spells.borrow().get(&path).cloned()) {
            message["recoverySpell"] = spell.into();
        }
    }
    for (key, value) in extra.as_object().unwrap() {
        message[key] = value.clone();
    }
    send(session, message)
}

fn password(text: &str) -> Value {
    json!({ "protection": "password", "password": text })
}

fn restore_to(session: &mut Session, backup: &Value, target: &Path, extra: Value) -> Value {
    let mut message = json!({ "type": "vault_restore", "backupPath": backup, "path": target });
    for (key, value) in extra.as_object().unwrap() {
        message[key] = value.clone();
    }
    send(session, message)
}

fn activate(session: &mut Session, restored: &Value) -> Value {
    send(
        session,
        json!({ "type": "vault_restore_activate", "expectedVaultToken": restored["vaultToken"] }),
    )
}

/// Whether two paths name the same existing file, by the host's file identity (`vault_token`:
/// volume serial number and file index), whatever the spelling (8.3 short names, case).
fn same_file(a: &Path, b: &Path) -> bool {
    let token = |path: &Path| {
        memocampus_native_host::active_vault::vault_token(path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    };
    token(a) == token(b)
}

/// `<path>.partial`, the name a restored vault has until it is verified and promoted.
fn partial(path: &Path) -> PathBuf {
    memocampus_native_host::secure_vault::fsutil::with_suffix(path, ".partial")
}

fn contains(bytes: &[u8], text: &str) -> bool {
    bytes.windows(text.len()).any(|w| w == text.as_bytes())
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_in(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn init_policy(env: &Environment) {
    PolicyStore::new(env.app_dir.clone().unwrap())
        .unwrap()
        .initialize()
        .unwrap();
}

// --- Format A ----------------------------------------------------------------------------------

#[test]
fn a_plain_vault_backs_up_as_plain_sqlite_and_restores_as_a_new_pending_vault() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    let original = active_dump(&mut session);
    let dir = tmp.0.join("backups");
    fs::create_dir(&dir).unwrap();
    let done = backup_to(&mut session, &dir, json!({}));
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["format"], "A");
    let backup = done["backupPath"].clone();
    assert!(backup.as_str().unwrap().ends_with(".sqlite3"));
    assert_eq!(names_in(&dir).len(), 1, "no .partial left");

    // Another PC: no config, no policy.
    let mut other = Session::new(env_of(&tmp, "app2"));
    let inspected = send(
        &mut other,
        json!({ "type": "vault_restore_inspect", "path": backup }),
    );
    assert_eq!(inspected["ok"], true, "{inspected}");
    assert_eq!(inspected["format"], "A");
    assert_eq!(inspected["needsPassword"], false);
    assert_eq!(inspected["needsSpell"], false);
    let target = tmp.0.join("restored.sqlite3");
    let restored = restore_to(&mut other, &backup, &target, json!({}));
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(restored["mode"], "plain");
    assert_eq!(restored["counts"]["visits"], 1);
    assert_eq!(restored["counts"]["pages"], 1);
    assert_eq!(restored["counts"]["pageBlobs"], 1);
    assert_eq!(restored["policyCopy"]["state"], "not_supported");
    // Restored, not active: the panel asks the user first.
    assert_eq!(restored["status"]["state"], "not_configured");
    assert_eq!(restored["status"]["restorePending"]["phase"], "restored");
    let activated = activate(&mut other, &restored);
    assert_eq!(activated["ok"], true, "{activated}");
    assert_eq!(activated["status"]["state"], "ready");
    assert_eq!(activated["status"]["publication"], "pending");
    assert_eq!(activated["status"]["restorePending"], json!(null));
    assert_eq!(active_dump(&mut other), original);
    // The source vault and the backup are unchanged and still there.
    assert!(source.exists() && Path::new(backup.as_str().unwrap()).exists());
}

// --- Format B ----------------------------------------------------------------------------------

#[test]
fn an_encrypted_vault_backs_up_encrypted_and_restores_on_another_pc_with_the_spell_only() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let mut session = Session::new(env.clone());
    let source = tmp.0.join("enc.sqlite3");
    let spell = create_encrypted(&mut session, &source);
    init_policy(&env);
    fill(&mut session);
    let original = active_dump(&mut session);
    let source_id = vault::resolve(&env).ok().and_then(|active| match active {
        Active::Configured(config) => config.vault_id,
        _ => None,
    });
    let dir = tmp.0.join("backups");
    fs::create_dir(&dir).unwrap();
    let done = backup_to(&mut session, &dir, json!({}));
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["format"], "B");
    assert_eq!(done["policyCopy"], true);
    let backup = PathBuf::from(done["backupPath"].as_str().unwrap());
    assert_eq!(backup.extension().unwrap(), "mcbackup");
    let bytes = fs::read(&backup).unwrap();
    assert_eq!(
        container::detect(&bytes[..16]),
        container::Detected::Container(container::Kind::B)
    );
    for canary in CANARIES {
        assert!(!contains(&bytes, canary), "{canary} in a B backup");
    }
    assert!(
        !contains(&bytes, "SQLite format 3"),
        "never plain SQLite inside"
    );
    assert!(!contains(&bytes, &spell));
    assert_eq!(names_in(&dir).len(), 1);
    assert!(backup::work_dir_entries(env.app_dir.as_deref().unwrap()).is_empty());

    // Another PC: no key store, no DPAPI entry for this vault, no sidecar.
    let other_env = env_of(&tmp, "app2");
    let mut other = Session::new(other_env.clone());
    let inspected = send(
        &mut other,
        json!({ "type": "vault_restore_inspect", "path": backup }),
    );
    assert_eq!(inspected["format"], "B", "{inspected}");
    assert_eq!(inspected["needsSpell"], true);
    assert_eq!(inspected["needsPassword"], false);
    assert_eq!(inspected["policyCopy"], true);
    let target = tmp.0.join("restored.sqlite3");
    // No spell, a malformed spell, another vault's spell: refused, nothing created.
    for (spell, code) in [
        (json!(null), "recovery_spell_invalid"),
        (json!("abandon abandon"), "recovery_spell_invalid"),
        (json!(format!("+{}", &spell[1..])), "recovery_spell_invalid"),
        (json!(&spell[..42]), "recovery_spell_invalid"),
        (json!(format!("{spell}=")), "recovery_spell_invalid"),
    ] {
        let refused = restore_to(
            &mut other,
            &json!(backup),
            &target,
            json!({ "recoverySpell": spell }),
        );
        assert_eq!(refused["error"], code, "{refused}");
    }
    let mut someone_else = Session::new(env_of(&tmp, "app3"));
    let other_spell = create_encrypted(&mut someone_else, &tmp.0.join("x.sqlite3"));
    let refused = restore_to(
        &mut other,
        &json!(backup),
        &target,
        json!({ "recoverySpell": other_spell }),
    );
    assert_eq!(refused["error"], "recovery_spell_rejected", "{refused}");
    assert!(!target.exists());
    assert!(!other_env
        .app_dir
        .as_ref()
        .unwrap()
        .join("vault-restore.json")
        .exists());

    // Case-sensitive: the spell in other case is not this spell.
    let refused = restore_to(
        &mut other,
        &json!(backup),
        &target,
        json!({ "recoverySpell": spell.to_uppercase() }),
    );
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .starts_with("recovery_spell_"),
        "{refused}"
    );
    assert!(!target.exists());
    let restored = restore_to(
        &mut other,
        &json!(backup),
        &target,
        json!({ "recoverySpell": spell }),
    );
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(restored["mode"], "encrypted");
    assert_eq!(restored["counts"]["visits"], 1);
    assert_eq!(restored["counts"]["images"], 1);
    // The policy copy is shown, never applied: the other PC still has no policy.
    assert_eq!(restored["policyCopy"]["state"], "valid", "{restored}");
    assert_eq!(restored["policyCopy"]["revision"], 1);
    assert!(!other_env
        .app_dir
        .as_ref()
        .unwrap()
        .join("policy.json")
        .exists());
    let activated = activate(&mut other, &restored);
    assert_eq!(activated["ok"], true, "{activated}");
    assert_eq!(activated["status"]["mode"], "encrypted");
    assert_eq!(activated["status"]["publication"], "pending");
    // A new vault id, a key in this PC's key store (DPAPI) and a new sidecar for the same spell.
    let config = match vault::resolve(&other_env).unwrap() {
        Active::Configured(config) => config,
        other => panic!("{other:?}"),
    };
    assert_ne!(config.vault_id, source_id);
    let new_id = config.vault_id.unwrap();
    let cipher::Head::Salt(salt) = cipher::read_head(&target).unwrap() else {
        panic!("not encrypted")
    };
    assert!(key_store::unlock(other_env.app_dir.as_deref().unwrap(), &new_id, &salt).is_ok());
    // The new id is the new salt's; no sidecar exists (Slice E).
    assert_eq!(recovery::vault_id(&salt), new_id);
    assert!(!sidecar::path_for(&target).exists());
    // A fresh host process unlocks it without the spell, and the content is the source's.
    let mut fresh = Session::new(other_env.clone());
    assert_eq!(active_dump(&mut fresh), original);
    // The spell still recovers the restored vault, from the file alone.
    let mut third = Session::new(env_of(&tmp, "app4"));
    let recovered = send(
        &mut third,
        json!({ "type": "vault_recover", "path": target, "recoverySpell": spell }),
    );
    assert_eq!(recovered["ok"], true, "{recovered}");
    // Nothing readable at rest in the restored vault or its metadata.
    for file in files_in(&tmp.0.join("app2"))
        .into_iter()
        .chain([target.clone()])
    {
        let bytes = fs::read(&file).unwrap();
        for canary in CANARIES {
            assert!(!contains(&bytes, canary), "{canary} in {}", file.display());
        }
    }
}

#[test]
fn restoring_next_to_the_original_and_twice_never_collides() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let spell = create_encrypted(&mut session, &tmp.0.join("enc.sqlite3"));
    fill(&mut session);
    let done = backup_to(&mut session, &tmp.0, json!({}));
    let backup = done["backupPath"].clone();
    let first = restore_to(
        &mut session,
        &backup,
        &tmp.0.join("one.sqlite3"),
        json!({ "recoverySpell": spell }),
    );
    assert_eq!(first["ok"], true, "{first}");
    // One restore at a time: the first is waiting for the user's decision.
    let second = restore_to(
        &mut session,
        &backup,
        &tmp.0.join("two.sqlite3"),
        json!({ "recoverySpell": spell }),
    );
    assert_eq!(second["error"], "vault_restore_pending");
    assert!(!tmp.0.join("two.sqlite3").exists());
    assert_eq!(activate(&mut session, &first)["ok"], true);
    let second = restore_to(
        &mut session,
        &backup,
        &tmp.0.join("two.sqlite3"),
        json!({ "recoverySpell": spell }),
    );
    assert_eq!(second["ok"], true, "{second}");
    let id_of = |path: &Path| match cipher::read_head(path).unwrap() {
        cipher::Head::Salt(salt) => recovery::vault_id(&salt),
        other => panic!("{other:?}"),
    };
    let ids = [
        id_of(&tmp.0.join("enc.sqlite3")),
        id_of(&tmp.0.join("one.sqlite3")),
        id_of(&tmp.0.join("two.sqlite3")),
    ];
    assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
    // The restored vault is a new vault even next to its original, and the original is untouched.
    let discarded = send(&mut session, json!({ "type": "vault_restore_discard" }));
    assert_eq!(discarded["ok"], true);
    assert!(!tmp.0.join("two.sqlite3").exists());
    assert!(
        tmp.0.join("one.sqlite3").exists(),
        "the active vault is never discarded"
    );
    assert!(tmp.0.join("enc.sqlite3").exists());
}

// --- Formats C and D ---------------------------------------------------------------------------

#[test]
fn a_password_backup_of_a_plain_vault_needs_the_password_and_keeps_nothing_in_the_clear() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let mut session = Session::new(env.clone());
    create_plain(&mut session, &tmp.0.join("plain.sqlite3"));
    init_policy(&env);
    fill(&mut session);
    let original = active_dump(&mut session);
    let dir = tmp.0.join("backups");
    fs::create_dir(&dir).unwrap();
    for (extra, code) in [
        (
            json!({ "protection": "password" }),
            "backup_password_required",
        ),
        (password("short"), "backup_password_too_short"),
        (password(&"x".repeat(2000)), "backup_password_too_long"),
        (json!({ "protection": "other" }), "invalid_request"),
    ] {
        assert_eq!(backup_to(&mut session, &dir, extra)["error"], code);
    }
    let done = backup_to(&mut session, &dir, password("correct horse battery"));
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["format"], "C");
    let backup = done["backupPath"].clone();
    let bytes = fs::read(backup.as_str().unwrap()).unwrap();
    for canary in CANARIES.iter().chain(&["correct horse", "SQLite format 3"]) {
        assert!(!contains(&bytes, canary), "{canary}");
    }
    let mut other = Session::new(env_of(&tmp, "app2"));
    let inspected = send(
        &mut other,
        json!({ "type": "vault_restore_inspect", "path": backup }),
    );
    assert_eq!(inspected["format"], "C");
    assert_eq!(inspected["needsPassword"], true);
    assert_eq!(inspected["needsSpell"], false);
    assert_eq!(inspected["policyCopy"], true);
    let target = tmp.0.join("restored.sqlite3");
    for (extra, code) in [
        (json!({}), "backup_password_required"),
        (
            json!({ "password": "correct horse batterY" }),
            "backup_password_rejected",
        ),
    ] {
        let refused = restore_to(&mut other, &backup, &target, extra);
        assert_eq!(refused["error"], code, "{refused}");
        assert!(!target.exists() && !tmp.0.join("restored.sqlite3.partial").exists());
    }
    let restored = restore_to(
        &mut other,
        &backup,
        &target,
        json!({ "password": "correct horse battery" }),
    );
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(restored["format"], "C");
    assert_eq!(restored["mode"], "plain");
    assert_eq!(restored["policyCopy"]["state"], "valid");
    assert_eq!(
        activate(&mut other, &restored)["status"]["publication"],
        "pending"
    );
    assert_eq!(active_dump(&mut other), original);
    // The response carries no password.
    assert!(!restored.to_string().contains("correct horse"));
}

#[test]
fn a_password_backup_of_an_encrypted_vault_needs_both_the_password_and_the_spell() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let spell = create_encrypted(&mut session, &tmp.0.join("enc.sqlite3"));
    fill(&mut session);
    let original = active_dump(&mut session);
    let done = backup_to(&mut session, &tmp.0, password("outer password!"));
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["format"], "D");
    assert_eq!(done["policyCopy"], false, "no policy initialized here");
    let backup = done["backupPath"].clone();
    let bytes = fs::read(backup.as_str().unwrap()).unwrap();
    for canary in CANARIES.iter().chain(&["outer password", "sqlcipher4"]) {
        assert!(!contains(&bytes, canary), "{canary}");
    }
    let mut other = Session::new(env_of(&tmp, "app2"));
    let inspected = send(
        &mut other,
        json!({ "type": "vault_restore_inspect", "path": backup }),
    );
    assert_eq!(inspected["format"], "D");
    assert_eq!(inspected["needsPassword"], true);
    assert_eq!(inspected["needsSpell"], true);
    assert_eq!(inspected["policyCopy"], json!(null));
    let target = tmp.0.join("restored.sqlite3");
    let mut someone_else = Session::new(env_of(&tmp, "app3"));
    let other_spell = create_encrypted(&mut someone_else, &tmp.0.join("x.sqlite3"));
    for (extra, code) in [
        (
            json!({ "password": "outer password!" }),
            "recovery_spell_invalid",
        ),
        (
            json!({ "recoverySpell": spell }),
            "backup_password_required",
        ),
        (
            json!({ "password": "wrong password!", "recoverySpell": spell }),
            "backup_password_rejected",
        ),
        (
            json!({ "password": "outer password!", "recoverySpell": other_spell }),
            "recovery_spell_rejected",
        ),
    ] {
        let refused = restore_to(&mut other, &backup, &target, extra);
        assert_eq!(refused["error"], code, "{refused}");
        assert!(!target.exists());
    }
    let restored = restore_to(
        &mut other,
        &backup,
        &target,
        json!({ "password": "outer password!", "recoverySpell": spell }),
    );
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(restored["format"], "D");
    assert_eq!(restored["policyCopy"]["state"], "absent");
    assert_eq!(activate(&mut other, &restored)["ok"], true);
    assert_eq!(active_dump(&mut other), original);
    let text = restored.to_string();
    assert!(!text.contains("outer password") && !text.contains(&spell));
}

// --- Restore safety ----------------------------------------------------------------------------

#[test]
fn a_restore_never_overwrites_and_never_touches_the_active_vault_or_the_policy() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let mut session = Session::new(env.clone());
    let active = tmp.0.join("active.sqlite3");
    create_plain(&mut session, &active);
    init_policy(&env);
    fill(&mut session);
    let done = backup_to(&mut session, &tmp.0, password("password123"));
    let backup = done["backupPath"].clone();
    let policy = fs::read(env.app_dir.as_ref().unwrap().join("policy.json")).unwrap();
    let config = fs::read(env.config_path().unwrap()).unwrap();
    let active_bytes = fs::read(&active).unwrap();
    let occupied = tmp.0.join("occupied.sqlite3");
    fs::write(&occupied, b"someone's file").unwrap();
    let partial_taken = tmp.0.join("p.sqlite3");
    fs::write(tmp.0.join("p.sqlite3.partial"), b"x").unwrap();
    for target in [&active, &occupied, &partial_taken] {
        let refused = restore_to(
            &mut session,
            &backup,
            target,
            json!({ "password": "password123" }),
        );
        assert_eq!(refused["error"], "file_exists", "{refused}");
    }
    assert_eq!(fs::read(&occupied).unwrap(), b"someone's file");
    let missing_dir = restore_to(
        &mut session,
        &backup,
        &tmp.0.join("nowhere").join("v.sqlite3"),
        json!({ "password": "password123" }),
    );
    assert_eq!(missing_dir["error"], "parent_missing");
    // A restored vault is ready, but the active vault, config and policy stay as they were.
    let restored = restore_to(
        &mut session,
        &backup,
        &tmp.0.join("new.sqlite3"),
        json!({ "password": "password123" }),
    );
    assert_eq!(restored["ok"], true, "{restored}");
    // The host reports canonical paths; the test's may be an 8.3 alias (CI's TEMP): same file?
    assert!(same_file(
        Path::new(restored["status"]["vaultPath"].as_str().unwrap()),
        &active
    ));
    assert_eq!(fs::read(env.config_path().unwrap()).unwrap(), config);
    assert_eq!(fs::read(&active).unwrap(), active_bytes);
    assert_eq!(
        fs::read(env.app_dir.as_ref().unwrap().join("policy.json")).unwrap(),
        policy,
        "the policy copy is never applied"
    );
    // Activation needs the token of the restored vault shown.
    let wrong = send(
        &mut session,
        json!({ "type": "vault_restore_activate", "expectedVaultToken": "0".repeat(64) }),
    );
    assert_eq!(wrong["error"], "vault_switched");
    let activated = activate(&mut session, &restored);
    assert_eq!(activated["status"]["publication"], "pending");
    assert_eq!(
        fs::read(env.app_dir.as_ref().unwrap().join("policy.json")).unwrap(),
        policy
    );
}

#[test]
fn a_restored_vault_stays_closed_to_mcp_even_with_a_confirmed_source_and_a_ready_policy() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let mut session = Session::new(env.clone());
    create_plain(&mut session, &tmp.0.join("plain.sqlite3"));
    init_policy(&env);
    let token = token(&mut session);
    let confirmed = send(
        &mut session,
        json!({ "type": "vault_publication_confirm", "expectedVaultToken": token }),
    );
    assert_eq!(confirmed["status"]["publication"], "confirmed");
    let done = backup_to(&mut session, &tmp.0, json!({}));
    let restored = restore_to(
        &mut session,
        &done["backupPath"],
        &tmp.0.join("r.sqlite3"),
        json!({}),
    );
    let activated = activate(&mut session, &restored);
    assert_eq!(activated["status"]["publication"], "pending");
    let config = match vault::resolve(&env).unwrap() {
        Active::Configured(config) => config,
        other => panic!("{other:?}"),
    };
    assert!(!config.is_published(restored["vaultToken"].as_str().unwrap()));
}

#[test]
fn files_that_are_not_backups_are_refused_and_nothing_is_created() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("empty", Vec::new(), "backup_unsupported_format"),
        ("short", b"MCVB".to_vec(), "backup_unsupported_format"),
        ("random", vec![0x5a; 8192], "backup_unsupported_format"),
        (
            "other-app.sqlite3",
            {
                let path = tmp.0.join("tmp-db");
                let db = rusqlite::Connection::open(&path).unwrap();
                db.execute_batch("PRAGMA application_id = 1234; CREATE TABLE t (x)")
                    .unwrap();
                drop(db);
                fs::read(&path).unwrap()
            },
            "backup_vault_invalid",
        ),
        (
            "untagged.sqlite3",
            {
                let path = tmp.0.join("tmp-db2");
                let db = rusqlite::Connection::open(&path).unwrap();
                db.execute_batch("CREATE TABLE t (x)").unwrap();
                drop(db);
                fs::read(&path).unwrap()
            },
            "backup_schema_mismatch",
        ),
        (
            "v4",
            {
                let mut bytes = container::MAGIC.to_vec();
                bytes.extend_from_slice(&4u16.to_le_bytes());
                bytes.extend_from_slice(&[b'B', 0, 2, 0, 0, 0, b'{', b'}']);
                bytes
            },
            "backup_unsupported_version",
        ),
        // B and D of earlier slices (12-word phrase, spell envelope): named as such, before any
        // secret is asked for (Slice E).
        (
            "v1-b",
            {
                let mut bytes = container::MAGIC.to_vec();
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&[b'B', 0, 2, 0, 0, 0, b'{', b'}']);
                bytes
            },
            "backup_legacy_unsupported",
        ),
        (
            "v1-d",
            {
                let mut bytes = container::MAGIC.to_vec();
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&[b'D', 0, 2, 0, 0, 0, b'{', b'}']);
                bytes
            },
            "backup_legacy_unsupported",
        ),
        (
            "v2-b",
            {
                let mut bytes = container::MAGIC.to_vec();
                bytes.extend_from_slice(&2u16.to_le_bytes());
                bytes.extend_from_slice(&[b'B', 0, 2, 0, 0, 0, b'{', b'}']);
                bytes
            },
            "backup_legacy_unsupported",
        ),
        (
            "v2-d",
            {
                let mut bytes = container::MAGIC.to_vec();
                bytes.extend_from_slice(&2u16.to_le_bytes());
                bytes.extend_from_slice(&[b'D', 0, 2, 0, 0, 0, b'{', b'}']);
                bytes
            },
            "backup_legacy_unsupported",
        ),
        (
            "huge-header",
            {
                let mut bytes = container::MAGIC.to_vec();
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&[b'C', 0]);
                bytes.extend_from_slice(&u32::MAX.to_le_bytes());
                bytes
            },
            "backup_malformed",
        ),
    ];
    for (name, bytes, code) in cases {
        let path = tmp.0.join(name);
        fs::write(&path, &bytes).unwrap();
        let inspected = send(
            &mut session,
            json!({ "type": "vault_restore_inspect", "path": path }),
        );
        assert_eq!(inspected["error"], code, "{name}: {inspected}");
        let restored = restore_to(
            &mut session,
            &json!(path),
            &tmp.0.join("target.sqlite3"),
            json!({ "password": "password123" }),
        );
        assert_eq!(restored["error"], code, "{name}");
        assert!(!tmp.0.join("target.sqlite3").exists());
        assert!(!tmp.0.join("target.sqlite3.partial").exists());
        assert!(!tmp.0.join("app").join("vault-restore.json").exists());
    }
    // A bare encrypted vault file (the vault itself, not a backup) is not a backup either.
    let mut enc = Session::new(env_of(&tmp, "app2"));
    create_encrypted(&mut enc, &tmp.0.join("enc.sqlite3"));
    let refused = send(
        &mut session,
        json!({ "type": "vault_restore_inspect", "path": tmp.0.join("enc.sqlite3") }),
    );
    assert_eq!(refused["error"], "backup_unsupported_format");
}

#[test]
fn a_damaged_backup_is_refused_after_streaming_and_leaves_nothing() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let spell = create_encrypted(&mut session, &tmp.0.join("enc.sqlite3"));
    fill(&mut session);
    let b = backup_to(&mut session, &tmp.0, json!({}));
    let d = backup_to(&mut session, &tmp.0, password("password123"));
    for (done, code) in [(&b, "backup_tampered"), (&d, "backup_tampered")] {
        let path = PathBuf::from(done["backupPath"].as_str().unwrap());
        let mut bytes = fs::read(&path).unwrap();
        let at = bytes.len() - 200;
        bytes[at] ^= 1;
        let damaged = tmp.0.join("damaged.mcbackup");
        fs::write(&damaged, &bytes).unwrap();
        let mut other = Session::new(env_of(&tmp, "app2"));
        let target = tmp.0.join("t.sqlite3");
        let refused = restore_to(
            &mut other,
            &json!(damaged),
            &target,
            json!({ "password": "password123", "recoverySpell": spell }),
        );
        assert_eq!(refused["error"], code, "{refused}");
        assert!(!target.exists() && !tmp.0.join("t.sqlite3.partial").exists());
        assert!(!sidecar::pending_path_for(&target).exists());
        let keys = tmp.0.join("app2").join("keys");
        assert!(!keys.exists() || names_in(&keys).is_empty());
        // Cut short, or with bytes appended.
        bytes[at] ^= 1;
        for variant in [
            bytes[..bytes.len() - 1].to_vec(),
            [bytes.clone(), vec![0]].concat(),
        ] {
            fs::write(&damaged, &variant).unwrap();
            let refused = restore_to(
                &mut other,
                &json!(damaged),
                &target,
                json!({ "password": "password123", "recoverySpell": spell }),
            );
            assert_eq!(refused["error"], "backup_incomplete", "{refused}");
        }
        fs::remove_file(&damaged).unwrap();
    }
}

// --- Fault injection: backup -------------------------------------------------------------------

fn with_fault<T>(step: Step, fault: Fault, f: impl FnOnce() -> T) -> T {
    backup::reset_faults();
    FAULT.set(Some((step, fault)));
    let result = f();
    backup::reset_faults();
    result
}

#[test]
fn a_backup_failing_at_any_step_leaves_no_file_and_reports_no_success() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let mut session = Session::new(env.clone());
    create_encrypted(&mut session, &tmp.0.join("enc.sqlite3"));
    fill(&mut session);
    let app = env.app_dir.clone().unwrap();
    for (round, (protection, steps)) in [
        (
            json!({}),
            vec![
                Step::Metadata,
                Step::Snapshot,
                Step::Write,
                Step::Verify,
                Step::BeforeRename,
            ],
        ),
        (
            password("password123"),
            vec![
                Step::Snapshot,
                Step::Write,
                Step::Verify,
                Step::BeforeRename,
            ],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        for step in steps {
            for fault in [Fault::Error, Fault::DiskFull] {
                let dir = tmp.0.join(format!("b{round}-{step:?}-{fault:?}"));
                fs::create_dir(&dir).unwrap();
                let failed = with_fault(step, fault, || {
                    backup_to(&mut session, &dir, protection.clone())
                });
                assert_eq!(failed["ok"], false, "{step:?} {fault:?}");
                if fault == Fault::DiskFull && matches!(step, Step::Write | Step::Snapshot) {
                    assert_eq!(failed["error"], "disk_full", "{step:?}");
                }
                assert!(
                    names_in(&dir).is_empty(),
                    "{step:?} {fault:?}: {:?}",
                    names_in(&dir)
                );
                assert!(backup::work_dir_entries(&app).is_empty(), "{step:?}");
            }
        }
    }
    // The vault is unchanged and still backs up.
    let dir = tmp.0.join("after");
    fs::create_dir(&dir).unwrap();
    assert_eq!(backup_to(&mut session, &dir, json!({}))["ok"], true);
}

#[test]
fn a_backup_process_dying_leaves_only_partial_files_which_never_pass_for_backups() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let mut session = Session::new(env.clone());
    let spell = create_encrypted(&mut session, &tmp.0.join("enc.sqlite3"));
    fill(&mut session);
    let app = env.app_dir.clone().unwrap();
    for step in [
        Step::Snapshot,
        Step::Write,
        Step::Verify,
        Step::BeforeRename,
    ] {
        let dir = tmp.0.join(format!("crash-{step:?}"));
        fs::create_dir(&dir).unwrap();
        let failed = with_fault(step, Fault::Crash, || {
            backup_to(&mut session, &dir, json!({}))
        });
        assert_eq!(failed["ok"], false);
        // Only hidden `.partial` names (never a `.mcbackup`) are left; their content is ciphertext.
        for name in names_in(&dir) {
            assert!(
                name.starts_with(".memocampus-backup-") && name.ends_with(".partial"),
                "{name}"
            );
            let bytes = fs::read(dir.join(&name)).unwrap();
            for canary in CANARIES {
                assert!(!contains(&bytes, canary));
            }
        }
        assert!(
            !backup::work_dir_entries(&app).is_empty(),
            "the dead process's work file stays"
        );
        // The next backup removes work files whose owner is gone, and succeeds.
        let next = backup_to(&mut session, &dir, json!({}));
        assert_eq!(next["ok"], true, "{next}");
        assert!(backup::work_dir_entries(&app).is_empty(), "{step:?}");
    }
    // Death right after the final rename: the backup is complete and restorable.
    let dir = tmp.0.join("crash-after-rename");
    fs::create_dir(&dir).unwrap();
    let failed = with_fault(Step::AfterRename, Fault::Crash, || {
        backup_to(&mut session, &dir, json!({}))
    });
    assert_eq!(failed["ok"], false);
    let finished: Vec<String> = names_in(&dir)
        .into_iter()
        .filter(|name| name.ends_with(".mcbackup"))
        .collect();
    assert_eq!(finished.len(), 1, "{:?}", names_in(&dir));
    assert!(names_in(&dir)
        .iter()
        .all(|name| !name.ends_with(".partial")));
    let mut other = Session::new(env_of(&tmp, "app2"));
    let restored = restore_to(
        &mut other,
        &json!(dir.join(&finished[0])),
        &tmp.0.join("from-crash.sqlite3"),
        json!({ "recoverySpell": spell }),
    );
    assert_eq!(restored["ok"], true, "{restored}");
}

// --- Fault injection: restore ------------------------------------------------------------------

#[test]
fn a_restore_failing_at_any_step_removes_everything_it_created() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let spell = create_encrypted(&mut session, &tmp.0.join("enc.sqlite3"));
    fill(&mut session);
    let backups = [
        backup_to(&mut session, &tmp.0, json!({}))["backupPath"].clone(),
        backup_to(&mut session, &tmp.0, password("password123"))["backupPath"].clone(),
    ];
    let other_env = env_of(&tmp, "app2");
    let mut other = Session::new(other_env.clone());
    let app2 = other_env.app_dir.clone().unwrap();
    let target = tmp.0.join("restored.sqlite3");
    for backup in &backups {
        for step in [
            Step::RestoreWrite,
            Step::RestoreVerify,
            Step::RestoreRekey,
            Step::RestoreKeys,
            Step::RestorePromote,
        ] {
            for fault in [Fault::Error, Fault::DiskFull] {
                let refused = with_fault(step, fault, || {
                    restore_to(
                        &mut other,
                        backup,
                        &target,
                        json!({ "password": "password123", "recoverySpell": spell }),
                    )
                });
                assert_eq!(refused["ok"], false, "{step:?}");
                if fault == Fault::DiskFull && step == Step::RestoreWrite {
                    assert_eq!(refused["error"], "disk_full");
                }
                for leftover in [
                    target.clone(),
                    tmp.0.join("restored.sqlite3.partial"),
                    tmp.0.join("restored.sqlite3.payload.partial"),
                    sidecar::path_for(&target),
                    sidecar::pending_path_for(&target),
                    app2.join("vault-restore.json"),
                    app2.join("config.json"),
                ] {
                    assert!(
                        !leftover.exists(),
                        "{step:?} {fault:?}: {}",
                        leftover.display()
                    );
                }
                let keys = app2.join("keys");
                assert!(!keys.exists() || names_in(&keys).is_empty(), "{step:?}");
            }
        }
    }
}

#[test]
fn a_restore_interrupted_by_a_crash_is_reported_and_can_be_discarded_or_finished() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let spell = create_encrypted(&mut session, &tmp.0.join("enc.sqlite3"));
    fill(&mut session);
    let original = active_dump(&mut session);
    let backup = backup_to(&mut session, &tmp.0, json!({}))["backupPath"].clone();
    let other_env = env_of(&tmp, "app2");
    let app2 = other_env.app_dir.clone().unwrap();
    let target = tmp.0.join("restored.sqlite3");
    let args = json!({ "recoverySpell": spell });
    // Crashes before verification, before the re-encryption under the new salt and while the keys
    // are written: discard, then restore again.
    for step in [
        Step::RestoreWrite,
        Step::RestoreVerify,
        Step::RestoreRekey,
        Step::RestoreKeys,
    ] {
        let mut other = Session::new(other_env.clone());
        let failed = with_fault(step, Fault::Crash, || {
            restore_to(&mut other, &backup, &target, args.clone())
        });
        assert_eq!(failed["ok"], false);
        let shown = send(&mut other, json!({ "type": "vault_status" }));
        assert_eq!(
            shown["status"]["restorePending"]["phase"], "restoring",
            "{step:?}"
        );
        // The target itself does not exist yet: the file written so far (the payload work file,
        // then the re-encrypted `.partial`) names the same file under both spellings of the path
        // (the host's canonical one, the test's possibly 8.3 one).
        let pending = PathBuf::from(shown["status"]["restorePending"]["path"].as_str().unwrap());
        let payload = |path: &Path| PathBuf::from(format!("{}.payload.partial", path.display()));
        let written = if partial(&target).exists() {
            same_file(&partial(&pending), &partial(&target))
        } else {
            same_file(&payload(&pending), &payload(&target))
        };
        assert!(written, "{step:?}");
        let activate_early = send(
            &mut other,
            json!({ "type": "vault_restore_activate", "expectedVaultToken": "0".repeat(64) }),
        );
        assert_eq!(activate_early["error"], "vault_restore_incomplete");
        let again = restore_to(&mut other, &backup, &tmp.0.join("x.sqlite3"), args.clone());
        assert_eq!(again["error"], "vault_restore_pending");
        let discarded = send(&mut other, json!({ "type": "vault_restore_discard" }));
        assert_eq!(discarded["ok"], true);
        assert_eq!(discarded["status"]["restorePending"], json!(null));
        assert!(!tmp.0.join("restored.sqlite3.partial").exists() && !target.exists());
        assert!(!tmp.0.join("restored.sqlite3.payload.partial").exists());
        let keys = app2.join("keys");
        assert!(!keys.exists() || names_in(&keys).is_empty(), "{step:?}");
        assert!(!sidecar::pending_path_for(&target).exists());
    }
    // A crash just before the promotion: verified files are there, activation finishes it.
    let mut other = Session::new(other_env.clone());
    let failed = with_fault(Step::RestorePromote, Fault::Crash, || {
        restore_to(&mut other, &backup, &target, args.clone())
    });
    assert_eq!(failed["ok"], false);
    let shown = send(&mut other, json!({ "type": "vault_status" }));
    let pending = shown["status"]["restorePending"].clone();
    assert_eq!(pending["phase"], "verified");
    assert!(!target.exists() && tmp.0.join("restored.sqlite3.partial").exists());
    assert!(!tmp.0.join("restored.sqlite3.payload.partial").exists());
    let activated = activate(&mut other, &pending);
    assert_eq!(activated["ok"], true, "{activated}");
    assert_eq!(activated["status"]["publication"], "pending");
    assert!(target.exists() && !tmp.0.join("restored.sqlite3.partial").exists());
    assert_eq!(active_dump(&mut other), original);
    // Activating again is harmless: nothing is pending any more.
    assert_eq!(
        activate(&mut other, &pending)["error"],
        "vault_restore_not_pending"
    );
}

#[test]
fn a_full_disk_while_restoring_is_reported_as_such() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    create_plain(&mut session, &tmp.0.join("plain.sqlite3"));
    fill(&mut session);
    let backup = backup_to(&mut session, &tmp.0, password("password123"))["backupPath"].clone();
    let refused = with_fault(Step::RestoreWrite, Fault::DiskFull, || {
        restore_to(
            &mut session,
            &backup,
            &tmp.0.join("r.sqlite3"),
            json!({ "password": "password123" }),
        )
    });
    assert_eq!(refused["error"], "disk_full", "{refused}");
    assert!(!tmp.0.join("r.sqlite3.partial").exists());
}

#[test]
fn a_backup_names_the_vault_shown_and_refuses_another() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    create_encrypted(&mut session, &tmp.0.join("a.sqlite3"));
    let shown = token(&mut session);
    create_plain(&mut session, &tmp.0.join("b.sqlite3"));
    let refused = send(
        &mut session,
        json!({ "type": "vault_backup", "expectedVaultToken": shown, "directory": tmp.0, "protection": "password", "password": "password123" }),
    );
    assert_eq!(refused["error"], "vault_switched");
    assert!(names_in(&tmp.0)
        .iter()
        .all(|name| !name.contains("memocampus-backup")));
}

#[test]
fn a_larger_vault_round_trips_through_every_container_format() {
    let tmp = TempDir::new();
    let mut plain = Session::new(tmp.env());
    create_plain(&mut plain, &tmp.0.join("plain.sqlite3"));
    let mut encrypted = Session::new(env_of(&tmp, "app-enc"));
    let spell = create_encrypted(&mut encrypted, &tmp.0.join("enc.sqlite3"));
    // About 24 MiB of image data: hundreds of 64 KiB chunks, several Backup API pages per row.
    let blob: Vec<u8> = (0..(3 << 20))
        .map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    for session in [&mut plain, &mut encrypted] {
        session
            .write(|db| {
                for id in 1..=8i64 {
                    db.execute(
                        "INSERT INTO image_blobs (id, sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data) VALUES (?1, ?2, 'image/png', ?3, 1, 1, 0, 0, ?4)",
                        rusqlite::params![id, format!("{id:064x}"), blob.len() as i64, &blob],
                    )?;
                }
                Ok(0)
            })
            .unwrap();
    }
    let expected_plain = active_dump(&mut plain);
    let expected_encrypted = active_dump(&mut encrypted);
    let cases = [
        (false, password("large password"), "C"),
        (true, json!({}), "B"),
        (true, password("large password"), "D"),
    ];
    for (index, (is_encrypted, protection, format)) in cases.into_iter().enumerate() {
        let (session, expected) = if is_encrypted {
            (&mut encrypted, &expected_encrypted)
        } else {
            (&mut plain, &expected_plain)
        };
        let done = backup_to(session, &tmp.0, protection);
        assert_eq!(done["format"], format, "{done}");
        let size = fs::metadata(done["backupPath"].as_str().unwrap())
            .unwrap()
            .len();
        assert!(size > 24 << 20, "{size}");
        let mut other = Session::new(env_of(&tmp, &format!("restore-{index}")));
        let target = tmp.0.join(format!("restored-{index}.sqlite3"));
        let restored = restore_to(
            &mut other,
            &done["backupPath"],
            &target,
            json!({ "password": "large password", "recoverySpell": spell }),
        );
        assert_eq!(restored["ok"], true, "{restored}");
        assert_eq!(activate(&mut other, &restored)["ok"], true);
        assert!(
            active_dump(&mut other) == *expected,
            "{format}: content differs"
        );
    }
}

// --- Review finding C-01: no plaintext on disk while a format C backup is made ------------------

/// Every file under `root` except `skip` that holds a plaintext SQLite header or a canary.
fn plaintext_files(root: &Path, skip: &[PathBuf]) -> Vec<String> {
    files_in(root)
        .into_iter()
        .filter(|path| !skip.contains(path))
        .filter(|path| {
            let bytes = fs::read(path).unwrap_or_default();
            contains(&bytes, "SQLite format 3") || CANARIES.iter().any(|c| contains(&bytes, c))
        })
        .map(|path| path.display().to_string())
        .collect()
}

#[test]
fn a_password_backup_of_a_plain_vault_writes_no_plaintext_to_disk_at_any_step() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    let original = active_dump(&mut session);
    let dir = tmp.0.join("backups");
    fs::create_dir(&dir).unwrap();
    // The source vault (and its journal) is the only plaintext allowed on disk.
    let skip = vec![
        source.clone(),
        memocampus_native_host::secure_vault::fsutil::with_suffix(&source, "-journal"),
    ];
    let seen = std::rc::Rc::new(RefCell::new(Vec::<(Step, Vec<String>)>::new()));
    let (root, record, skipped) = (tmp.0.clone(), seen.clone(), skip.clone());
    backup::reset_faults();
    backup::ON_STEP.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |step: Step| {
            if matches!(
                step,
                Step::Metadata | Step::Snapshot | Step::Verify | Step::BeforeRename
            ) {
                record
                    .borrow_mut()
                    .push((step, plaintext_files(&root, &skipped)));
            }
        }))
    });
    let done = backup_to(&mut session, &dir, password("password123"));
    backup::reset_faults();
    assert_eq!(done["format"], "C", "{done}");
    let seen = seen.borrow();
    let steps: Vec<Step> = seen.iter().map(|(step, _)| *step).collect();
    assert_eq!(
        steps,
        [
            Step::Metadata,
            Step::Snapshot,
            Step::Verify,
            Step::BeforeRename
        ]
    );
    for (step, found) in seen.iter() {
        assert!(found.is_empty(), "{step:?}: plaintext on disk: {found:?}");
    }
    assert!(backup::work_dir_entries(tmp.env().app_dir.as_deref().unwrap()).is_empty());
    // And the backup restores to the same content.
    let mut other = Session::new(env_of(&tmp, "app2"));
    let restored = restore_to(
        &mut other,
        &done["backupPath"],
        &tmp.0.join("restored.sqlite3"),
        json!({ "password": "password123" }),
    );
    assert_eq!(activate(&mut other, &restored)["ok"], true, "{restored}");
    assert_eq!(active_dump(&mut other), original);
}

#[test]
fn a_format_c_backup_dying_at_any_step_leaves_only_ciphertext() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let mut session = Session::new(env.clone());
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    let skip = vec![source.clone()];
    for step in [
        Step::Snapshot,
        Step::Write,
        Step::Verify,
        Step::BeforeRename,
    ] {
        let dir = tmp.0.join(format!("crash-{step:?}"));
        fs::create_dir(&dir).unwrap();
        let failed = with_fault(step, Fault::Crash, || {
            backup_to(&mut session, &dir, password("password123"))
        });
        assert_eq!(failed["ok"], false);
        let found = plaintext_files(&tmp.0, &skip);
        assert!(found.is_empty(), "{step:?}: {found:?}");
        assert!(backup::work_dir_entries(env.app_dir.as_deref().unwrap()).is_empty());
    }
}

#[test]
fn a_format_c_snapshot_is_one_consistent_state_while_others_write() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    let original = active_dump(&mut session);
    let late = "INSERT INTO visits VALUES ('late', 'https://late.test/', 'late', '2026-11-10T00:00:00.000Z')";
    let writer_path = source.clone();
    let attempts = std::rc::Rc::new(RefCell::new(Vec::new()));
    let record = attempts.clone();
    backup::reset_faults();
    backup::ON_STEP.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |step: Step| {
            if step == Step::Snapshot {
                // Another process writes while the pages are being read.
                let other = rusqlite::Connection::open(&writer_path).unwrap();
                other
                    .busy_timeout(std::time::Duration::from_millis(200))
                    .unwrap();
                let result = other.execute(late, []);
                record
                    .borrow_mut()
                    .push(result.map_err(|e| e.sqlite_error_code()));
            }
        }))
    });
    let done = backup_to(&mut session, &tmp.0, password("password123"));
    backup::reset_faults();
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(
        *attempts.borrow(),
        [Err(Some(rusqlite::ErrorCode::DatabaseBusy))],
        "no writer commits while the snapshot is read"
    );
    // The writer commits afterwards; the backup holds the state before it.
    assert_eq!(
        rusqlite::Connection::open(&source)
            .unwrap()
            .execute(late, [])
            .unwrap(),
        1
    );
    let mut other = Session::new(env_of(&tmp, "app2"));
    let restored = restore_to(
        &mut other,
        &done["backupPath"],
        &tmp.0.join("restored.sqlite3"),
        json!({ "password": "password123" }),
    );
    activate(&mut other, &restored);
    assert_eq!(active_dump(&mut other), original);
}

#[test]
fn a_plain_vault_in_wal_mode_is_refused_for_format_c_and_nothing_is_written() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    session.reset();
    let wal = rusqlite::Connection::open(&source).unwrap();
    let mode: String = wal
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let dir = tmp.0.join("backups");
    fs::create_dir(&dir).unwrap();
    let refused = backup_to(&mut session, &dir, password("password123"));
    assert_eq!(refused["error"], "backup_wal_unsupported", "{refused}");
    assert!(names_in(&dir).is_empty());
    // The normal backup (Backup API) still includes what is only in the WAL.
    wal.execute(
        "INSERT INTO visits VALUES ('w1', 'https://wal.test/', 'in the wal', '2026-11-10T00:00:00.000Z')",
        [],
    )
    .unwrap();
    let done = backup_to(&mut session, &dir, json!({}));
    assert_eq!(done["format"], "A", "{done}");
    let copy = rusqlite::Connection::open(done["backupPath"].as_str().unwrap()).unwrap();
    let count: i64 = copy
        .query_row(
            "SELECT count(*) FROM visits WHERE event_id = 'w1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

// --- Review finding C-03: B and D need the spell of this very vault ---------------------------

#[test]
fn b_and_d_backups_need_the_spell_that_recovers_this_vault() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let mut session = Session::new(env.clone());
    let source = tmp.0.join("enc.sqlite3");
    let right = create_encrypted(&mut session, &source);
    fill(&mut session);
    let mut someone_else = Session::new(env_of(&tmp, "app-other"));
    let other_vault = tmp.0.join("other.sqlite3");
    let other_spell = create_encrypted(&mut someone_else, &other_vault);
    let dir = tmp.0.join("backups");
    fs::create_dir(&dir).unwrap();
    // Two characters swapped (a typo that keeps the spell well-formed when both are inner).
    let mut chars: Vec<char> = right.chars().collect();
    let first = (0..41).find(|&i| chars[i] != chars[i + 1]).unwrap();
    chars.swap(first, first + 1);
    let swapped: String = chars.into_iter().collect();
    for protection in [json!({}), password("password123")] {
        for (typed, code) in [
            (json!(null), "recovery_spell_invalid"),
            (json!("abandon abandon abandon"), "recovery_spell_invalid"),
            (json!(other_spell), "recovery_spell_rejected"),
            (json!(swapped), "recovery_spell_rejected"),
            (json!(&right[..42]), "recovery_spell_invalid"),
            (json!(right.to_uppercase()), "recovery_spell_"),
        ] {
            let mut extra = protection.clone();
            extra["recoverySpell"] = typed.clone();
            let refused = backup_to(&mut session, &dir, extra);
            let error = refused["error"].as_str().unwrap_or_default();
            assert!(error.starts_with(code), "{typed}: {refused}");
            assert!(
                names_in(&dir).is_empty(),
                "nothing is written for a refused spell"
            );
            assert!(!refused.to_string().contains(&right[..20]));
        }
    }
    // Sidecars play no part (Slice E): a stray `.mckey` naming another salt changes nothing.
    fs::write(
        sidecar::path_for(&source),
        fs::read(tmp.0.join("app-other").join("config.json")).unwrap_or_default(),
    )
    .unwrap();
    // The right spell: B and D are made, and both restore with that spell.
    let original_rows = active_dump(&mut session);
    for (index, protection) in [json!({}), password("password123")].into_iter().enumerate() {
        let done = backup_to(&mut session, &dir, protection);
        assert_eq!(done["ok"], true, "{done}");
        let bytes = fs::read(done["backupPath"].as_str().unwrap()).unwrap();
        assert!(!contains(&bytes, &right));
        assert!(!done.to_string().contains(&right[..20]));
        let mut fresh = Session::new(env_of(&tmp, &format!("restore-{index}")));
        let restored = restore_to(
            &mut fresh,
            &done["backupPath"],
            &tmp.0.join(format!("restored-{index}.sqlite3")),
            json!({ "password": "password123", "recoverySpell": right }),
        );
        assert_eq!(activate(&mut fresh, &restored)["ok"], true, "{restored}");
        assert_eq!(active_dump(&mut fresh), original_rows);
    }
    // Nothing about the spell is stored by the backups: not in the app directory either.
    for file in files_in(env.app_dir.as_deref().unwrap()) {
        assert!(
            !contains(&fs::read(&file).unwrap(), &right),
            "{}",
            file.display()
        );
    }
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

#[cfg(windows)]
#[test]
fn a_restore_target_named_by_its_8_3_short_path_is_the_same_file_and_a_copy_is_not() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    create_plain(&mut session, &tmp.0.join("plain.sqlite3"));
    fill(&mut session);
    let backup = backup_to(&mut session, &tmp.0, json!({}))["backupPath"].clone();
    let long_dir = tmp.0.join("Memocampus restore long directory");
    fs::create_dir(&long_dir).unwrap();
    let short_dir = short_path(&long_dir);
    if short_dir.file_name() == long_dir.file_name() {
        eprintln!("SKIPPED: this volume generates no 8.3 short names");
        return;
    }
    let (short, long) = (short_dir.join("r.sqlite3"), long_dir.join("r.sqlite3"));
    let restored = restore_to(&mut session, &backup, &short, json!({}));
    assert_eq!(restored["ok"], true, "{restored}");
    let reported = PathBuf::from(restored["path"].as_str().unwrap());
    let pending = PathBuf::from(
        restored["status"]["restorePending"]["path"]
            .as_str()
            .unwrap(),
    );
    assert_ne!(
        reported, short,
        "spelled differently: a string comparison would fail"
    );
    for path in [&reported, &pending] {
        assert!(same_file(path, &short) && same_file(path, &long));
    }
    // A byte-for-byte copy beside it is another file, under either spelling.
    let copy = long_dir.join("copy.sqlite3");
    fs::copy(&long, &copy).unwrap();
    assert_eq!(fs::read(&copy).unwrap(), fs::read(&long).unwrap());
    assert!(!same_file(&reported, &copy) && !same_file(&reported, &short_path(&copy)));
    // Activated: the active vault is that file; the source vault is still there.
    let activated = activate(&mut session, &restored);
    let active = PathBuf::from(activated["status"]["vaultPath"].as_str().unwrap());
    assert!(same_file(&active, &short) && !same_file(&active, &copy));
    assert!(!same_file(&active, &tmp.0.join("plain.sqlite3")));
}
