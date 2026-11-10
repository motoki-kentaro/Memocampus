//! Issue #51, Slice D: converting the active vault between plain and encrypted, through the side
//! panel messages. Temporary app directories and vaults only; encrypted vaults use this Windows
//! user's DPAPI through the temporary app directory's key store. A second app directory stands for
//! another PC (no key store entry, no config).
//!
//! File identities are compared with `vault_token` (volume serial and file index), never by path
//! spelling, so Windows 8.3 short names cannot make a test fail or pass by accident.

use super::backup::{self, Fault, Step, FAULT};
use super::*;
use crate::vault::tests::TempDir;
use crate::vault::{Environment, VaultKey};
use memocampus_native_host::active_vault::{self, GateError};
use memocampus_native_host::schema;
use memocampus_native_host::secure_vault::{cipher, key_store, sidecar};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs;

struct Dialog {
    target: RefCell<Option<PathBuf>>,
    asked: RefCell<Vec<(PathBuf, String, bool)>>,
}

impl Dialog {
    fn none() -> Self {
        Self {
            target: RefCell::new(None),
            asked: RefCell::new(Vec::new()),
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
        None
    }
    fn choose_export_file(&self, _: &str, _: &str) -> Option<PathBuf> {
        None
    }
    fn choose_convert_target(&self, dir: &Path, name: &str, encrypt: bool) -> Option<PathBuf> {
        self.asked
            .borrow_mut()
            .push((dir.to_path_buf(), name.to_owned(), encrypt));
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

fn env_of(tmp: &TempDir, app: &str) -> Environment {
    Environment {
        app_dir: Some(tmp.0.join(app)),
        ..tmp.env()
    }
}

fn status_of(session: &mut Session) -> Value {
    send(session, json!({ "type": "vault_status" }))["status"].clone()
}

fn token_of(session: &mut Session) -> Value {
    status_of(session)["vaultToken"].clone()
}

fn same_file(a: &Path, b: &Path) -> bool {
    let token = |path: &Path| active_vault::vault_token(path).unwrap();
    token(a) == token(b)
}

fn hash(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

/// Every file under `dir` (relative paths), sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    out.push(
                        path.strip_prefix(base)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

const IMAGE_BYTES: usize = 1 << 20;

/// Rows in every kind of table, with canaries: visits, page metadata, a navigation, a tab
/// snapshot, an image of 1 MiB with its thumbnail and a save, tags.
fn fill(session: &mut Session) {
    let image: Vec<u8> = (0..IMAGE_BYTES as u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8)
        .collect();
    session
        .write(|db| {
            db.execute_batch(
                "INSERT INTO visits VALUES ('v1', 'https://canary.test/CANARY-url', 'CANARY-title', '2026-11-10T10:00:00.000Z');
                 INSERT INTO visits VALUES ('v2', 'https://canary.test/two', 'two', '2026-11-10T10:01:00.000Z');
                 INSERT INTO page_metadata (url, event_id, title, description, captured_at)
                   VALUES ('https://canary.test/CANARY-url', 'v1', 'CANARY-meta', 'desc', '2026-11-10T10:00:00.000Z');
                 INSERT INTO navigation_events VALUES ('v2', 'v1', 'document', 'link', 0, 0, 0, 0, 0, '2026-11-10T10:01:00.000Z');
                 INSERT INTO tab_snapshots VALUES ('t1', 1, 0, 'https://canary.test/tab', 'tab', 0, 0, '2026-11-10T10:02:00.000Z');
                 INSERT INTO image_tags (id, name) VALUES (1, 'canary-tag'), (2, 'other-tag');",
            )?;
            db.execute(
                "INSERT INTO image_blobs (id, sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data)
                   VALUES (1, ?1, 'image/png', ?2, 640, 480, 1, 2, ?3)",
                (
                    format!("{:x}", Sha256::digest(&image)),
                    image.len() as i64,
                    &image,
                ),
            )?;
            db.execute_batch(
                "INSERT INTO image_thumbnails VALUES (1, 'image/webp', 64, 48, 1, CAST('CANARY-thumb' AS BLOB));
                 INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public)
                   VALUES ('s1', 1, 'https://canary.test/CANARY-image.png', 'https://canary.test/CANARY-url', 3, 0, 1);
                 INSERT INTO image_tag_links VALUES (1, 1), (1, 2);",
            )?;
            Ok(0)
        })
        .unwrap();
}

fn add_visit(session: &mut Session, id: &str) {
    session
        .write(|db| {
            db.execute(
                "INSERT INTO visits VALUES (?1, 'https://late.test/', 'late', '2026-11-10T11:00:00.000Z')",
                [id],
            )
        })
        .unwrap();
}

/// Every row of every table, in primary-key order, with the header fields: the logical content.
fn dump(db: &Connection) -> Vec<String> {
    let mut out = vec![
        format!(
            "user_version={}",
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap()
        ),
        format!(
            "application_id={}",
            db.query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0))
                .unwrap()
        ),
    ];
    for (table, order) in [
        ("visits", "event_id"),
        ("tab_snapshots", "event_id"),
        ("image_blobs", "id"),
        ("image_thumbnails", "blob_id"),
        ("saved_images", "id"),
        ("page_metadata", "url"),
        ("navigation_events", "target_visit_id"),
        ("image_tags", "id"),
        ("image_tag_links", "blob_id, tag_id"),
    ] {
        let mut statement = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let values: Vec<String> = (0..columns)
                .map(|i| match row.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Blob(bytes) => {
                        format!("blob:{:x}", Sha256::digest(bytes))
                    }
                    rusqlite::types::ValueRef::Text(text) => {
                        format!("text:{}", String::from_utf8_lossy(text))
                    }
                    value => format!("{value:?}"),
                })
                .collect();
            out.push(format!("{table}: {}", values.join("|")));
        }
    }
    out
}

fn active_dump(session: &mut Session) -> Vec<String> {
    session.read(dump).unwrap()
}

fn plain_dump(path: &Path) -> Vec<String> {
    dump(&Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap())
}

fn create_plain(session: &mut Session, path: &Path) {
    let created = send(session, json!({ "type": "vault_create", "path": path }));
    assert_eq!(created["ok"], true, "{created}");
}

/// Creates an encrypted vault (made active) and returns its spell.
fn create_encrypted(session: &mut Session, path: &Path) -> String {
    let begun = send(
        session,
        json!({ "type": "vault_create_encrypted", "path": path }),
    );
    assert_eq!(begun["ok"], true, "{begun}");
    let spell = begun["recoverySpell"].as_str().unwrap().to_owned();
    let done = send(
        session,
        json!({ "type": "vault_create_encrypted_commit", "recoverySpell": spell }),
    );
    assert_eq!(done["ok"], true, "{done}");
    spell
}

fn start(session: &mut Session, direction: &str, target: &Path, extra: Value) -> Value {
    let mut message = json!({
        "type": "vault_convert_start",
        "direction": direction,
        "expectedVaultToken": token_of(session),
        "path": target,
    });
    if direction == "decrypt" {
        message["acknowledgePlaintext"] = true.into();
    }
    for (key, value) in extra.as_object().unwrap() {
        message[key] = value.clone();
    }
    send(session, message)
}

fn confirm(session: &mut Session, spell: &str) -> Value {
    send(
        session,
        json!({ "type": "vault_convert_confirm", "recoverySpell": spell }),
    )
}

fn activate(session: &mut Session, token: &Value) -> Value {
    send(
        session,
        json!({ "type": "vault_convert_activate", "expectedVaultToken": token }),
    )
}

/// A well-formed spell of no vault.
const OTHER_SPELL: &str = "FIGOkr1evDcTXDtnVPhoqTeIHqtUJDTffhCodcry5fs";

fn keys_of(app: &Path) -> Vec<String> {
    tree(&app.join("keys"))
}

fn not_a_database(db: &Connection) -> bool {
    db.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
        r.get::<_, i64>(0)
    })
    .is_err_and(|error| error.sqlite_error_code() == Some(ErrorCode::NotADatabase))
}

/// MCP and VisibleStore read only through this gate.
fn gate(app: &Path) -> Result<active_vault::Admitted, GateError> {
    active_vault::admit(app, None)
}

// --- D1: plain → encrypted ---------------------------------------------------------------------

#[test]
fn d1_a_plain_vault_becomes_a_new_encrypted_vault_only_through_spell_and_activation() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let app = env.app_dir.clone().unwrap();
    let mut session = Session::new(env.clone());
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    let before = active_dump(&mut session);
    let source_token = token_of(&mut session);
    let source_hash = hash(&source);
    let target = tmp.0.join("encrypted.sqlite3");

    let started = start(&mut session, "encrypt", &target, json!({}));
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["phase"], "verified");
    assert_eq!(started["direction"], "encrypt");
    assert_eq!(
        started["counts"],
        json!({ "visits": 2, "snapshots": 1, "images": 1 })
    );
    let spell = started["recoverySpell"].as_str().unwrap().to_owned();
    assert_eq!(spell.len(), 43);
    // Nothing is a usable vault yet: no target, only the hidden work file and pending keys.
    assert!(!target.exists());
    let files = tree(&tmp.0);
    let work: Vec<&String> = files.iter().filter(|f| f.ends_with(".partial")).collect();
    assert_eq!(work.len(), 1, "{files:?}");
    assert!(work[0].starts_with(".encrypted.sqlite3."), "{files:?}");
    assert!(files.contains(&"encrypted.sqlite3.mckey.pending".to_owned()));
    let (new_id, _) = sidecar::identity(&sidecar::pending_path_for(&target)).unwrap();
    assert!(key_store::pending_path(&app, &new_id).exists());
    assert!(!key_store::entry_path(&app, &new_id).exists());
    // The active vault is unchanged; the panel sees the conversion waiting for the spell.
    let shown = status_of(&mut session);
    assert!(same_file(
        Path::new(shown["vaultPath"].as_str().unwrap()),
        &source
    ));
    assert_eq!(shown["mode"], "plain");
    assert_eq!(shown["conversionPending"]["phase"], "verified");
    assert_eq!(shown["conversionPending"]["direction"], "encrypt");
    // The spell is in no file: not in the manifest, the sidecar or anywhere under the folders.
    for file in tree(&tmp.0) {
        let bytes = fs::read(tmp.0.join(&file)).unwrap();
        assert!(!bytes.windows(43).any(|w| w == spell.as_bytes()), "{file}");
    }

    // Out of order: activation and keeping need the confirmed spell.
    let token = started["vaultToken"].clone();
    assert_eq!(
        activate(&mut session, &token)["error"],
        "vault_convert_spell_unconfirmed"
    );
    assert_eq!(
        send(&mut session, json!({ "type": "vault_convert_keep" }))["error"],
        "vault_convert_spell_unconfirmed"
    );
    // A wrong or malformed spell changes nothing; the user may try again.
    assert_eq!(
        confirm(&mut session, OTHER_SPELL)["error"],
        "recovery_spell_rejected"
    );
    assert_eq!(
        confirm(&mut session, &spell[..42])["error"],
        "recovery_spell_invalid"
    );
    let mut swapped = spell.clone().into_bytes();
    swapped.swap(0, 1);
    if swapped != spell.as_bytes() {
        let swapped = String::from_utf8(swapped).unwrap();
        assert_ne!(confirm(&mut session, &swapped)["ok"], true);
    }
    assert!(!target.exists());
    let confirmed = confirm(&mut session, &spell);
    assert_eq!(confirmed["ok"], true, "{confirmed}");
    assert_eq!(confirmed["phase"], "converted");
    assert!(target.exists() && sidecar::path_for(&target).exists());
    // Still not active; the source is byte for byte what it was.
    let shown = status_of(&mut session);
    assert!(same_file(
        Path::new(shown["vaultPath"].as_str().unwrap()),
        &source
    ));
    assert_eq!(hash(&source), source_hash);

    // The new vault: SQLCipher with a salt, refused without its key or with another key, and
    // logically the source.
    let Ok(cipher::Head::Salt(salt)) = cipher::read_head(&target) else {
        panic!("not encrypted")
    };
    let sidecar_found = sidecar::read(&sidecar::path_for(&target)).unwrap();
    assert_eq!(sidecar_found.cipher_salt, salt);
    let key = key_store::unlock(&app, &sidecar_found.vault_id, &salt).unwrap();
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    assert_eq!(dump(&cipher::open(&target, &key, flags).unwrap()), before);
    assert!(not_a_database(
        &Connection::open_with_flags(&target, flags).unwrap()
    ));
    assert!(not_a_database(
        &cipher::open(&target, &VaultKey::generate().unwrap(), flags).unwrap()
    ));
    let bytes = fs::read(&target).unwrap();
    assert!(!bytes.windows(6).any(|w| w == b"CANARY"));

    // Writes to the source after the conversion reach the new vault at activation.
    add_visit(&mut session, "after-conversion");
    let activated = activate(&mut session, &token);
    assert_eq!(activated["ok"], true, "{activated}");
    let shown = &activated["status"];
    assert!(same_file(
        Path::new(shown["vaultPath"].as_str().unwrap()),
        &target
    ));
    assert_eq!(shown["mode"], "encrypted");
    assert_eq!(shown["publication"], "pending");
    assert_eq!(shown["conversionPending"], json!(null));
    assert_ne!(shown["vaultToken"], source_token);
    let config = active_vault::read_config(&app).unwrap().unwrap();
    assert_eq!(config.vault_id, Some(sidecar_found.vault_id));
    assert_eq!(gate(&app).unwrap_err(), GateError::PublicationPending);
    let after = active_dump(&mut session);
    assert!(after.iter().any(|row| row.contains("after-conversion")));
    assert_eq!(after.len(), before.len() + 1);
    // The source still holds everything, untouched by the activation.
    assert_eq!(plain_dump(&source), after);
    assert!(fs::read(sidecar::path_for(&target)).is_ok());

    // The spell recovers the new vault where this user's key store is not available.
    let mut other = Session::new(env_of(&tmp, "app2"));
    let recovered = send(
        &mut other,
        json!({ "type": "vault_recover", "path": target, "recoverySpell": spell }),
    );
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(recovered["status"]["publication"], "pending");
    assert_eq!(active_dump(&mut other), after);
}

#[test]
fn d1_a_conversion_that_is_kept_leaves_the_active_vault_and_opens_later() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    let target = tmp.0.join("kept.sqlite3");
    let started = start(&mut session, "encrypt", &target, json!({}));
    let spell = started["recoverySpell"].as_str().unwrap().to_owned();
    // While pending, the copy cannot be opened as a vault (it would take writes behind the
    // conversion's back), neither its work file.
    assert_eq!(confirm(&mut session, &spell)["ok"], true);
    let opened = send(
        &mut session,
        json!({ "type": "vault_open", "path": target }),
    );
    assert_eq!(opened["error"], "vault_convert_pending", "{opened}");
    let kept = send(&mut session, json!({ "type": "vault_convert_keep" }));
    assert_eq!(kept["ok"], true, "{kept}");
    let shown = &kept["status"];
    assert!(same_file(
        Path::new(shown["vaultPath"].as_str().unwrap()),
        &source
    ));
    assert_eq!(shown["conversionPending"], json!(null));
    // Now an ordinary encrypted vault of this user.
    let opened = send(
        &mut session,
        json!({ "type": "vault_open", "path": target }),
    );
    assert_eq!(opened["ok"], true, "{opened}");
    assert_eq!(opened["status"]["mode"], "encrypted");
    assert_eq!(opened["status"]["publication"], "pending");
}

// --- D2: encrypted → plain ---------------------------------------------------------------------

#[test]
fn d2_an_encrypted_vault_becomes_a_plain_vault_only_with_the_users_acknowledgement() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let app = env.app_dir.clone().unwrap();
    let mut session = Session::new(env.clone());
    let source = tmp.0.join("enc.sqlite3");
    let spell = create_encrypted(&mut session, &source);
    fill(&mut session);
    let before = active_dump(&mut session);
    let source_status = status_of(&mut session);
    let source_hash = hash(&source);
    let sidecar_hash = hash(&sidecar::path_for(&source));
    let keys_before: Vec<(String, String)> = keys_of(&app)
        .into_iter()
        .map(|k| (k.clone(), hash(&app.join("keys").join(&k))))
        .collect();
    let target = tmp.0.join("plain.sqlite3");
    let files_before = tree(&tmp.0);

    // No acknowledgement of the plaintext on disk: refused before anything is chosen or written.
    let dialog = Dialog::none();
    let refused = send_with(
        &mut session,
        &dialog,
        json!({ "type": "vault_convert_start", "direction": "decrypt", "expectedVaultToken": source_status["vaultToken"] }),
    );
    assert_eq!(refused["error"], "vault_convert_plaintext_unacknowledged");
    for wrong in [json!(false), json!("true"), json!(1)] {
        let refused = send_with(
            &mut session,
            &dialog,
            json!({ "type": "vault_convert_start", "direction": "decrypt",
                    "expectedVaultToken": source_status["vaultToken"], "acknowledgePlaintext": wrong }),
        );
        assert_eq!(refused["error"], "vault_convert_plaintext_unacknowledged");
    }
    assert!(dialog.asked.borrow().is_empty());
    assert_eq!(tree(&tmp.0), files_before);

    // Acknowledged; the dialog starts next to the source with a distinct name.
    let dialog = Dialog::none();
    *dialog.target.borrow_mut() = Some(target.clone());
    let started = send_with(
        &mut session,
        &dialog,
        json!({ "type": "vault_convert_start", "direction": "decrypt",
                "expectedVaultToken": source_status["vaultToken"], "acknowledgePlaintext": true }),
    );
    assert_eq!(started["ok"], true, "{started}");
    let asked = dialog.asked.borrow().clone();
    assert_eq!(asked.len(), 1);
    // A folder has no vault token: compared by its canonical path.
    assert_eq!(
        fs::canonicalize(&asked[0].0).unwrap(),
        fs::canonicalize(&tmp.0).unwrap()
    );
    assert_eq!(asked[0].1, "enc-plain.sqlite3");
    assert!(!asked[0].2);
    assert_eq!(started["phase"], "converted");
    assert_eq!(
        started["recoverySpell"],
        json!(null),
        "a plain vault has no spell"
    );
    assert_eq!(
        started["counts"],
        json!({ "visits": 2, "snapshots": 1, "images": 1 })
    );
    // A standard SQLite file holding the same rows (in the clear, as acknowledged).
    assert_eq!(
        cipher::read_head(&target).unwrap(),
        cipher::Head::PlainSqlite
    );
    let plain = Connection::open_with_flags(&target, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    schema::check(&plain).unwrap();
    drop(plain);
    assert_eq!(plain_dump(&target), before);
    let bytes = fs::read(&target).unwrap();
    assert!(bytes.windows(10).any(|w| w == b"CANARY-url"));
    // The source, its sidecar and every key store entry are unchanged; it is still active.
    assert_eq!(hash(&source), source_hash);
    assert_eq!(hash(&sidecar::path_for(&source)), sidecar_hash);
    let keys_now: Vec<(String, String)> = keys_of(&app)
        .into_iter()
        .map(|k| (k.clone(), hash(&app.join("keys").join(&k))))
        .collect();
    assert_eq!(keys_now, keys_before);
    let shown = status_of(&mut session);
    assert_eq!(shown["vaultToken"], source_status["vaultToken"]);
    assert_eq!(shown["mode"], "encrypted");
    assert_eq!(shown["conversionPending"]["direction"], "decrypt");
    assert!(
        !sidecar::path_for(&target).exists(),
        "no spell for a plain vault"
    );
    // No spell step for a plain conversion.
    assert_eq!(
        confirm(&mut session, &spell)["error"],
        "vault_convert_invalid_state"
    );

    add_visit(&mut session, "after-conversion");
    let activated = activate(&mut session, &started["vaultToken"]);
    assert_eq!(activated["ok"], true, "{activated}");
    let shown = &activated["status"];
    assert!(same_file(
        Path::new(shown["vaultPath"].as_str().unwrap()),
        &target
    ));
    assert_eq!(shown["mode"], "plain");
    assert_eq!(shown["publication"], "pending");
    assert_ne!(shown["vaultToken"], source_status["vaultToken"]);
    assert_eq!(
        active_vault::read_config(&app).unwrap().unwrap().vault_id,
        None
    );
    assert_eq!(gate(&app).unwrap_err(), GateError::PublicationPending);
    let after = active_dump(&mut session);
    assert!(after.iter().any(|row| row.contains("after-conversion")));
    // The encrypted source is kept and still recoverable with its spell (another PC).
    assert!(source.exists() && sidecar::path_for(&source).exists());
    let mut other = Session::new(env_of(&tmp, "app2"));
    let recovered = send(
        &mut other,
        json!({ "type": "vault_recover", "path": source, "recoverySpell": spell }),
    );
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(active_dump(&mut other), after);
}

#[test]
fn the_spell_of_an_earlier_vault_never_recovers_a_converted_one() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let original = tmp.0.join("enc.sqlite3");
    let old_spell = create_encrypted(&mut session, &original);
    fill(&mut session);
    let plain = tmp.0.join("plain.sqlite3");
    let started = start(&mut session, "decrypt", &plain, json!({}));
    assert_eq!(activate(&mut session, &started["vaultToken"])["ok"], true);
    let again = tmp.0.join("again.sqlite3");
    let started = start(&mut session, "encrypt", &again, json!({}));
    let new_spell = started["recoverySpell"].as_str().unwrap().to_owned();
    assert_ne!(new_spell, old_spell);
    assert_eq!(
        confirm(&mut session, &old_spell)["error"],
        "recovery_spell_rejected"
    );
    assert_eq!(confirm(&mut session, &new_spell)["ok"], true);
    assert_eq!(activate(&mut session, &started["vaultToken"])["ok"], true);
    let mut other = Session::new(env_of(&tmp, "app2"));
    let refused = send(
        &mut other,
        json!({ "type": "vault_recover", "path": again, "recoverySpell": old_spell }),
    );
    assert_eq!(refused["error"], "recovery_spell_rejected");
    // Each vault answers to its own spell only; the ids differ.
    let a = sidecar::read(&sidecar::path_for(&original)).unwrap();
    let b = sidecar::read(&sidecar::path_for(&again)).unwrap();
    assert_ne!(a.vault_id, b.vault_id);
}

// --- Refusals ----------------------------------------------------------------------------------

#[test]
fn bad_requests_and_targets_are_refused_without_side_effects() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let app = env.app_dir.clone().unwrap();
    let mut session = Session::new(env);
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    let token = token_of(&mut session);
    let taken = tmp.0.join("taken.sqlite3");
    fs::write(&taken, b"someone else's file").unwrap();
    fs::write(tmp.0.join("journal.sqlite3-journal"), b"stale").unwrap();
    let files = tree(&tmp.0);
    let source_hash = hash(&source);
    let upper = PathBuf::from(source.to_string_lossy().to_uppercase());
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({ "direction": "encrypt", "expectedVaultToken": token, "path": source }),
            "vault_convert_same_as_source",
        ),
        (
            json!({ "direction": "encrypt", "expectedVaultToken": token, "path": upper }),
            "vault_convert_same_as_source",
        ),
        (
            json!({ "direction": "encrypt", "expectedVaultToken": token, "path": taken }),
            "file_exists",
        ),
        (
            json!({ "direction": "encrypt", "expectedVaultToken": token, "path": tmp.0.join("journal.sqlite3") }),
            "file_exists",
        ),
        (
            json!({ "direction": "encrypt", "expectedVaultToken": token, "path": tmp.0.join("missing").join("v.sqlite3") }),
            "parent_missing",
        ),
        (
            json!({ "direction": "encrypt", "expectedVaultToken": token, "path": "relative.sqlite3" }),
            "invalid_path",
        ),
        (
            json!({ "direction": "encrypt", "expectedVaultToken": token, "path": r"\\nas.invalid\share\v.sqlite3" }),
            "location_rejected",
        ),
        (
            json!({ "direction": "encrypt", "expectedVaultToken": token }),
            "cancelled",
        ),
        (
            json!({ "direction": "decrypt", "expectedVaultToken": token, "acknowledgePlaintext": true, "path": tmp.0.join("x.sqlite3") }),
            "vault_convert_mode_mismatch",
        ),
        (
            json!({ "direction": "sideways", "expectedVaultToken": token, "path": tmp.0.join("x.sqlite3") }),
            "invalid_request",
        ),
        (
            json!({ "expectedVaultToken": token, "path": tmp.0.join("x.sqlite3") }),
            "invalid_request",
        ),
        (
            json!({ "direction": "encrypt", "expectedVaultToken": "0".repeat(64), "path": tmp.0.join("x.sqlite3") }),
            "vault_switched",
        ),
        (
            json!({ "direction": "encrypt", "expectedVaultToken": "nope", "path": tmp.0.join("x.sqlite3") }),
            "invalid_vault_token",
        ),
    ];
    for (fields, code) in cases {
        let mut message = fields.clone();
        message["type"] = "vault_convert_start".into();
        let response = send(&mut session, message);
        assert_eq!(response["error"], code, "{fields} → {response}");
        assert_eq!(tree(&tmp.0), files, "{fields}");
        assert!(!app.join(convert::MANIFEST).exists());
    }
    assert_eq!(hash(&source), source_hash);
    assert_eq!(fs::read(&taken).unwrap(), b"someone else's file");
}

#[test]
fn one_operation_at_a_time_and_out_of_order_requests_are_refused() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let app = env.app_dir.clone().unwrap();
    let mut session = Session::new(env);
    for kind in ["vault_convert_confirm", "vault_convert_keep"] {
        let response = send(
            &mut session,
            json!({ "type": kind, "recoverySpell": OTHER_SPELL }),
        );
        assert_eq!(response["error"], "vault_convert_not_pending", "{kind}");
    }
    assert_eq!(
        activate(&mut session, &json!("0".repeat(64)))["error"],
        "vault_convert_not_pending"
    );
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    let keys_before = keys_of(&app);
    let files_before = tree(&tmp.0);
    let started = start(&mut session, "encrypt", &tmp.0.join("a.sqlite3"), json!({}));
    assert_eq!(started["ok"], true, "{started}");
    // A second conversion, an encrypted creation, a restore: all wait for this one.
    let second = start(&mut session, "encrypt", &tmp.0.join("b.sqlite3"), json!({}));
    assert_eq!(second["error"], "vault_convert_pending");
    let created = send(
        &mut session,
        json!({ "type": "vault_create_encrypted", "path": tmp.0.join("c.sqlite3") }),
    );
    assert_eq!(created["error"], "vault_convert_pending");
    let restored = send(
        &mut session,
        json!({ "type": "vault_restore", "backupPath": source, "path": tmp.0.join("d.sqlite3") }),
    );
    assert_eq!(restored["error"], "vault_convert_pending");
    // The work file is never opened as a vault.
    let work = tree(&tmp.0)
        .into_iter()
        .find(|f| f.ends_with(".partial"))
        .unwrap();
    let opened = send(
        &mut session,
        json!({ "type": "vault_open", "path": tmp.0.join(&work) }),
    );
    assert_eq!(opened["error"], "vault_work_file");
    // A token that is not this conversion's.
    let active_token = token_of(&mut session);
    let wrong = activate(&mut session, &active_token);
    assert_eq!(wrong["error"], "vault_switched");
    // Discarding removes everything this conversion made, and only that.
    let discarded = send(&mut session, json!({ "type": "vault_convert_discard" }));
    assert_eq!(discarded["ok"], true, "{discarded}");
    assert_eq!(discarded["status"]["conversionPending"], json!(null));
    assert_eq!(tree(&tmp.0), files_before);
    assert_eq!(keys_of(&app), keys_before);
    assert!(!app.join(convert::MANIFEST).exists());
    let _ = started;

    // While an encrypted creation waits for its spell, no conversion starts.
    let begun = send(
        &mut session,
        json!({ "type": "vault_create_encrypted", "path": tmp.0.join("e.sqlite3") }),
    );
    assert_eq!(begun["ok"], true);
    let refused = start(&mut session, "encrypt", &tmp.0.join("f.sqlite3"), json!({}));
    assert_eq!(refused["error"], "vault_creation_pending");
}

// --- Failures, crashes, restarts ---------------------------------------------------------------

fn with_fault<T>(step: Step, fault: Fault, f: impl FnOnce() -> T) -> T {
    backup::reset_faults();
    FAULT.set(Some((step, fault)));
    let result = f();
    backup::reset_faults();
    result
}

#[test]
fn a_failed_conversion_leaves_nothing_and_a_crashed_one_is_never_usable_until_finished() {
    for direction in ["encrypt", "decrypt"] {
        let steps: &[Step] = match direction {
            "encrypt" => &[Step::ConvertCopy, Step::ConvertVerify, Step::ConvertKeys],
            _ => &[Step::ConvertCopy, Step::ConvertVerify, Step::ConvertPromote],
        };
        for &step in steps {
            for fault in [Fault::Error, Fault::DiskFull, Fault::Crash] {
                let tmp = TempDir::new();
                let env = tmp.env();
                let app = env.app_dir.clone().unwrap();
                let mut session = Session::new(env.clone());
                let source = tmp.0.join("source.sqlite3");
                if direction == "encrypt" {
                    create_plain(&mut session, &source);
                } else {
                    create_encrypted(&mut session, &source);
                }
                fill(&mut session);
                let source_hash = hash(&source);
                let before = active_dump(&mut session);
                let keys_before = keys_of(&app);
                let files_before = tree(&tmp.0);
                let target = tmp.0.join("target.sqlite3");
                let case = format!("{direction} {step:?} {fault:?}");
                let failed = with_fault(step, fault, || {
                    start(&mut session, direction, &target, json!({}))
                });
                assert_eq!(failed["ok"], false, "{case}: {failed}");
                if fault == Fault::DiskFull {
                    assert_eq!(failed["error"], "disk_full", "{case}");
                }
                assert!(failed.get("recoverySpell").is_none(), "{case}");
                // Never a vault under the target name, never a change of the active vault or source.
                assert!(!target.exists(), "{case}");
                assert_eq!(hash(&source), source_hash, "{case}");
                let mut restarted = Session::new(env.clone());
                assert_eq!(active_dump(&mut restarted), before, "{case}");
                let shown = status_of(&mut restarted);
                assert!(same_file(
                    Path::new(shown["vaultPath"].as_str().unwrap()),
                    &source
                ));
                if fault != Fault::Crash {
                    assert_eq!(tree(&tmp.0), files_before, "{case}");
                    assert_eq!(keys_of(&app), keys_before, "{case}");
                    assert_eq!(shown["conversionPending"], json!(null), "{case}");
                    continue;
                }
                // A process that died: the conversion is shown, its work file is not a vault.
                let pending = &shown["conversionPending"];
                assert_eq!(pending["direction"], direction, "{case}");
                let expected_phase = if step == Step::ConvertPromote {
                    "verified"
                } else {
                    "converting"
                };
                assert_eq!(pending["phase"], expected_phase, "{case}");
                if let Some(work) = tree(&tmp.0).into_iter().find(|f| f.ends_with(".partial")) {
                    let opened = send(
                        &mut restarted,
                        json!({ "type": "vault_open", "path": tmp.0.join(work) }),
                    );
                    assert_eq!(opened["error"], "vault_work_file", "{case}");
                }
                if step == Step::ConvertPromote {
                    // Verified before the crash: activation finishes the promotion.
                    let activated = activate(&mut restarted, &pending["vaultToken"]);
                    assert_eq!(activated["ok"], true, "{case}: {activated}");
                    assert!(same_file(
                        Path::new(activated["status"]["vaultPath"].as_str().unwrap()),
                        &target
                    ));
                    continue;
                }
                assert_eq!(
                    activate(&mut restarted, &json!("0".repeat(64)))["error"],
                    "vault_convert_incomplete",
                    "{case}"
                );
                if direction == "encrypt" {
                    assert_eq!(
                        confirm(&mut restarted, OTHER_SPELL)["error"],
                        "vault_convert_incomplete",
                        "{case}"
                    );
                }
                let discarded = send(&mut restarted, json!({ "type": "vault_convert_discard" }));
                assert_eq!(discarded["ok"], true, "{case}");
                assert_eq!(tree(&tmp.0), files_before, "{case}");
                assert_eq!(keys_of(&app), keys_before, "{case}");
                assert_eq!(hash(&source), source_hash, "{case}");
            }
        }
    }
}

#[test]
fn a_crash_while_promoting_the_confirmed_vault_is_finished_by_the_next_confirmation() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let mut session = Session::new(env.clone());
    create_plain(&mut session, &tmp.0.join("plain.sqlite3"));
    fill(&mut session);
    let target = tmp.0.join("enc.sqlite3");
    let started = start(&mut session, "encrypt", &target, json!({}));
    let spell = started["recoverySpell"].as_str().unwrap().to_owned();
    let crashed = with_fault(Step::ConvertPromote, Fault::Crash, || {
        confirm(&mut session, &spell)
    });
    assert_eq!(crashed["ok"], false);
    // The restarted host: the spell the user kept finishes it.
    let mut restarted = Session::new(env);
    assert_eq!(
        status_of(&mut restarted)["conversionPending"]["phase"],
        "verified"
    );
    let done = confirm(&mut restarted, &spell);
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(activate(&mut restarted, &started["vaultToken"])["ok"], true);
    assert!(same_file(
        Path::new(status_of(&mut restarted)["vaultPath"].as_str().unwrap()),
        &target
    ));
}

#[test]
fn activation_fails_closed_when_the_source_was_replaced_or_the_last_copy_fails() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let app = env.app_dir.clone().unwrap();
    let mut session = Session::new(env.clone());
    let source = tmp.0.join("plain.sqlite3");
    create_plain(&mut session, &source);
    fill(&mut session);
    let target = tmp.0.join("enc.sqlite3");
    let started = start(&mut session, "encrypt", &target, json!({}));
    let spell = started["recoverySpell"].as_str().unwrap().to_owned();
    assert_eq!(confirm(&mut session, &spell)["ok"], true);
    let token = started["vaultToken"].clone();
    let config_before = fs::read(app.join("config.json")).unwrap();
    let target_hash = hash(&target);

    // The last copy fails (an I/O error in the fence): nothing switches, the copy is unchanged.
    let failed = with_fault(Step::ConvertRefresh, Fault::Error, || {
        activate(&mut session, &token)
    });
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(fs::read(app.join("config.json")).unwrap(), config_before);
    assert_eq!(hash(&target), target_hash);
    assert_eq!(
        status_of(&mut session)["conversionPending"]["phase"],
        "converted"
    );

    // Another process put another vault where the source was: the conversion refuses to switch.
    session.reset();
    fs::rename(&source, tmp.0.join("moved.sqlite3")).unwrap();
    vault::create_vault(&source).unwrap();
    let refused = activate(&mut session, &token);
    assert_eq!(
        refused["error"], "vault_convert_source_changed",
        "{refused}"
    );
    assert_eq!(fs::read(app.join("config.json")).unwrap(), config_before);
    assert_eq!(hash(&target), target_hash);
    // Keeping it is still possible: a snapshot of the source at conversion time.
    let kept = send(&mut session, json!({ "type": "vault_convert_keep" }));
    assert_eq!(kept["ok"], true, "{kept}");
    assert!(target.exists());
}

#[test]
fn a_switch_of_the_active_vault_during_a_conversion_neither_mixes_vaults_nor_loses_the_source() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let a = tmp.0.join("a.sqlite3");
    create_plain(&mut session, &a);
    fill(&mut session);
    let a_rows = active_dump(&mut session);
    let target = tmp.0.join("a-encrypted.sqlite3");
    let started = start(&mut session, "encrypt", &target, json!({}));
    let spell = started["recoverySpell"].as_str().unwrap().to_owned();
    // The user makes another vault active meanwhile (another panel, another process).
    let b = tmp.0.join("b.sqlite3");
    create_plain(&mut session, &b);
    add_visit(&mut session, "only-in-b");
    assert_eq!(confirm(&mut session, &spell)["ok"], true);
    let activated = activate(&mut session, &started["vaultToken"]);
    assert_eq!(activated["ok"], true, "{activated}");
    assert!(same_file(
        Path::new(activated["status"]["vaultPath"].as_str().unwrap()),
        &target
    ));
    // The converted vault holds A, never B; A and B are kept as they were.
    assert_eq!(active_dump(&mut session), a_rows);
    assert_eq!(plain_dump(&a), a_rows);
    assert!(plain_dump(&b).iter().any(|row| row.contains("only-in-b")));
}

#[test]
fn the_override_disables_conversion() {
    let tmp = TempDir::new();
    let env = Environment {
        db_override: Some(tmp.0.join("dev.sqlite3")),
        ..tmp.env()
    };
    let mut session = Session::new(env);
    for kind in [
        "vault_convert_start",
        "vault_convert_confirm",
        "vault_convert_activate",
        "vault_convert_keep",
        "vault_convert_discard",
    ] {
        let response = send(&mut session, json!({ "type": kind }));
        assert_eq!(response["error"], "management_disabled", "{kind}");
    }
}
