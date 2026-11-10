//! Issue #51, Slice E, through the real host binary: **the encrypted vault file and its Recovery
//! Spell are all a recovery needs** (`docs/secure-vault/slice-e.md` §1, test 12.1). No `.mckey`,
//! no key store entry, no DPAPI data, no config and no other file of the original installation.
//! Temporary app directories and vaults only.

use memocampus_native_host::active_vault::{self, GateError};
use memocampus_native_host::secure_vault::secret::CipherSalt;
use memocampus_native_host::secure_vault::{cipher, key_store, recovery};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SAVE_ID: &str = "6a1d2c3b-5a6e-4d7f-8091-a2b3c4d5e6f8";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One host process with `app` as its application directory.
fn host(app: &Path, messages: &[Value]) -> Vec<Value> {
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
        let mut message = message.clone();
        if message.get("requestId").is_none()
            && !message["type"].as_str().unwrap().starts_with("image_save")
        {
            message["requestId"] = "r".into();
        }
        let body = serde_json::to_vec(&message).unwrap();
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
    responses
}

fn one(app: &Path, message: Value) -> Value {
    host(app, &[message]).remove(0)
}

fn status(app: &Path) -> Value {
    one(app, json!({ "type": "vault_status" }))["status"].clone()
}

fn visit(id: &str) -> Value {
    json!({ "type": "visit", "id": id, "url": format!("https://memories.test/CANARY-{id}"),
            "title": "CANARY title", "incognito": false, "visitedAt": "2026-11-10T10:00:00.000Z" })
}

/// A structurally valid PNG of exactly `size` bytes (the host checks headers, not pixels).
fn png_of_size(size: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&40u32.to_be_bytes());
    bytes.extend_from_slice(&30u32.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    let payload = size - bytes.len() - 12 - 12;
    bytes.extend_from_slice(&(payload as u32).to_be_bytes());
    bytes.extend_from_slice(b"IDAT");
    bytes.extend((0..payload).map(|i| (i * 7 % 251) as u8));
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
    bytes
}

fn save_image(app: &Path, bytes: &[u8]) {
    use base64::Engine;
    let sha256: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut messages = vec![json!({
        "type": "image_save_begin", "protocol": 2, "saveId": SAVE_ID, "sha256": sha256,
        "byteLength": bytes.len(), "mimeType": "image/png", "width": 40, "height": 30,
        "imageUrl": "https://img.test/CANARY.png", "pageUrl": "https://memories.test/page",
        "incognito": false,
    })];
    for (index, part) in bytes.chunks(1024 * 1024).enumerate() {
        messages.push(
            json!({ "type": "image_save_chunk", "saveId": SAVE_ID, "index": index,
                              "data": base64::engine::general_purpose::STANDARD.encode(part) }),
        );
    }
    messages.push(json!({ "type": "image_save_commit", "saveId": SAVE_ID }));
    let replies = host(app, &messages);
    assert_eq!(
        replies.last().unwrap()["type"],
        "image_saved",
        "{replies:?}"
    );
}

fn salt_of(path: &Path) -> CipherSalt {
    match cipher::read_head(path).unwrap() {
        cipher::Head::Salt(salt) => salt,
        other => panic!("{other:?}"),
    }
}

/// What "the same memories" means: the schema, every visit, every image blob (its bytes hashed),
/// every saved image, read with the key `app`'s key store holds for the vault at `path`.
fn memories(app: &Path, path: &Path) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let salt = salt_of(path);
    let key = key_store::unlock(app, &recovery::vault_id(&salt), &salt).unwrap();
    let db = cipher::open(path, &key, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let rows = |db: &Connection, sql: &str| -> Vec<String> {
        let mut statement = db.prepare(sql).unwrap();
        let names = statement.column_count();
        statement
            .query_map([], |row| {
                Ok((0..names)
                    .map(|i| format!("{:?}", row.get::<_, rusqlite::types::Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let blobs: Vec<String> = {
        let mut statement = db
            .prepare("SELECT id, mime_type, byte_length, data FROM image_blobs ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| {
                let data: Vec<u8> = row.get(3)?;
                Ok(format!(
                    "{}|{}|{}|{:x}",
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    Sha256::digest(&data)
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    (
        rows(
            &db,
            "SELECT type, name, sql FROM sqlite_schema ORDER BY type, name",
        ),
        rows(&db, "SELECT * FROM visits ORDER BY event_id"),
        blobs,
        rows(&db, "SELECT * FROM saved_images ORDER BY save_id"),
    )
}

/// Every file under `dir`, recursively.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn the_vault_file_and_its_spell_alone_bring_the_memories_back() {
    let tmp = TempDir::new();
    let app = tmp.dir("original-app");
    assert_eq!(
        one(&app, json!({ "type": "policy_initialize" }))["ok"],
        true
    );
    let vault = tmp.dir("original").join("memocampus.sqlite3");

    // 1–2. A new encrypted vault; the 43-character spell is what the user keeps.
    let begun = one(
        &app,
        json!({ "type": "vault_create_encrypted", "path": vault }),
    );
    let spell = begun["recoverySpell"].as_str().unwrap().to_owned();
    assert_eq!(spell.len(), 43);
    let done = one(
        &app,
        json!({ "type": "vault_create_encrypted_commit", "recoverySpell": spell }),
    );
    assert_eq!(done["ok"], true, "{done}");

    // 3. Visits and an image (a 1.5 MiB blob, in chunks).
    for id in ["m1", "m2", "m3"] {
        assert_eq!(one(&app, visit(id))["ok"], true);
    }
    let image = png_of_size(1536 * 1024 + 321);
    save_image(&app, &image);
    let original = memories(&app, &vault);
    assert_eq!(original.1.len(), 3);
    assert_eq!(original.2.len(), 1);
    assert!(original.2[0].ends_with(&format!("{:x}", Sha256::digest(&image))));
    assert_eq!(original.3.len(), 1);

    // 5. No `.mckey` was ever written, anywhere.
    assert!(files(&tmp.0)
        .iter()
        .all(|path| !path.to_string_lossy().contains(".mckey")));

    // 4, 6. Another installation: an empty app directory (no key store, no DPAPI entry, no
    // config, no policy) and **one file**: the vault, copied alone under another name.
    let isolated = tmp.dir("isolated ふっかつ #1");
    let copy = isolated.join("my memories (copy).db");
    std::fs::copy(&vault, &copy).unwrap();
    assert_eq!(
        files(&isolated),
        std::slice::from_ref(&copy),
        "exactly one file"
    );
    let fresh = tmp.dir("fresh-app");
    assert!(files(&fresh).is_empty());
    let opened = one(&fresh, json!({ "type": "vault_open", "path": copy }));
    assert_eq!(opened["error"], "key_unavailable", "{opened}");
    assert_eq!(opened["recoveryAvailable"], true);
    let recovered = one(
        &fresh,
        json!({ "type": "vault_recover", "path": copy, "recoverySpell": spell }),
    );
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(recovered["status"]["state"], "ready");
    assert_eq!(recovered["status"]["mode"], "encrypted");

    // 8. MCP reads nothing until the user confirms publication.
    assert_eq!(recovered["status"]["publication"], "pending");
    assert_eq!(
        active_vault::admit(&fresh, None).unwrap_err(),
        GateError::PublicationPending
    );

    // 7. The same schema, visits, image blob (byte for byte) and saved image.
    assert_eq!(memories(&fresh, &copy), original);
    let found = one(&fresh, json!({ "type": "search_pages" }));
    assert!(found.to_string().contains("CANARY-m2"), "{found}");
    // From now on this installation opens it without the spell (a new DPAPI entry).
    assert_eq!(status(&fresh)["state"], "ready");
    assert!(key_store::entry_path(&fresh, &recovery::vault_id(&salt_of(&copy))).exists());
}

#[test]
fn losing_the_key_store_of_this_windows_user_loses_nothing_but_convenience() {
    let tmp = TempDir::new();
    let app = tmp.dir("app");
    assert_eq!(
        one(&app, json!({ "type": "policy_initialize" }))["ok"],
        true
    );
    let vault = tmp.0.join("vault.sqlite3");
    let begun = one(
        &app,
        json!({ "type": "vault_create_encrypted", "path": vault }),
    );
    let spell = begun["recoverySpell"].as_str().unwrap().to_owned();
    let done = one(
        &app,
        json!({ "type": "vault_create_encrypted_commit", "recoverySpell": spell }),
    );
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(one(&app, visit("k1"))["ok"], true);
    let published = one(
        &app,
        json!({ "type": "vault_publication_confirm", "expectedVaultToken": status(&app)["vaultToken"] }),
    );
    assert_eq!(published["status"]["publication"], "confirmed");
    let before = memories(&app, &vault);

    // The DPAPI key store is gone (a reinstalled profile keeping the app directory, a cleanup).
    std::fs::remove_dir_all(app.join("keys")).unwrap();
    let shown = status(&app);
    assert_eq!(shown["state"], "key_unavailable");
    assert_eq!(shown["recoveryAvailable"], true);
    let refused = one(&app, visit("k2"));
    assert_eq!(refused["ok"], false, "never acknowledged without the key");
    // A wrong spell changes nothing; the right one restores daily use, publication pending again.
    let wrong = one(
        &app,
        json!({ "type": "vault_recover", "path": vault,
                "recoverySpell": format!("{}{}", if spell.starts_with('A') { "B" } else { "A" }, &spell[1..]) }),
    );
    assert_ne!(wrong["ok"], true);
    assert!(!app.join("keys").exists() || files(&app.join("keys")).is_empty());
    let recovered = one(
        &app,
        json!({ "type": "vault_recover", "path": vault, "recoverySpell": spell }),
    );
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(recovered["status"]["publication"], "pending");
    assert_eq!(memories(&app, &vault), before);
    assert_eq!(one(&app, visit("k2"))["ok"], true);
}

#[test]
fn a_recovery_that_died_after_storing_the_key_finishes_on_retry_and_never_duplicates() {
    use memocampus_native_host::secure_vault::spell;
    let tmp = TempDir::new();
    let app = tmp.dir("app");
    assert_eq!(
        one(&app, json!({ "type": "policy_initialize" }))["ok"],
        true
    );
    let vault = tmp.0.join("vault.sqlite3");
    let begun = one(
        &app,
        json!({ "type": "vault_create_encrypted", "path": vault }),
    );
    let typed = begun["recoverySpell"].as_str().unwrap().to_owned();
    let done = one(
        &app,
        json!({ "type": "vault_create_encrypted_commit", "recoverySpell": typed }),
    );
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(one(&app, visit("c1"))["ok"], true);
    // Another installation where a recovery stored its key and died before switching: the entry
    // exists, no config names the vault.
    let fresh = tmp.dir("fresh");
    let salt = salt_of(&vault);
    let id = recovery::vault_id(&salt);
    let key = recovery::vault_key(&spell::parse(&typed).unwrap(), &salt);
    key_store::store(&fresh, &id, &salt, &key, key_store::Write::New).unwrap();
    let entry = std::fs::read(key_store::entry_path(&fresh, &id)).unwrap();
    assert_eq!(status(&fresh)["state"], "not_configured");
    // The retry (a second identical request, as after a timeout) finishes it; the entry is kept.
    for _ in 0..2 {
        let recovered = one(
            &fresh,
            json!({ "type": "vault_recover", "path": vault, "recoverySpell": typed }),
        );
        assert_eq!(recovered["ok"], true, "{recovered}");
        assert_eq!(recovered["status"]["publication"], "pending");
    }
    assert_eq!(
        std::fs::read(key_store::entry_path(&fresh, &id)).unwrap(),
        entry
    );
    assert_eq!(
        files(&fresh.join("keys")).len(),
        1,
        "one entry, never a second"
    );
    assert_eq!(memories(&fresh, &vault), memories(&app, &vault));
}
