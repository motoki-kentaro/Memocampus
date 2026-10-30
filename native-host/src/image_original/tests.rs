use super::*;
use crate::images::tests::{configure, png};
use crate::images::{sha256_hex, SaveRequest};
use crate::vault::tests::TempDir;
use crate::vault::{create_vault, open_vault};
use serde_json::json;
use std::cell::RefCell;

struct Fixture {
    tmp: TempDir,
    session: Session,
    vault: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new();
    let vault = tmp.0.join("v.sqlite3");
    create_vault(&vault).unwrap();
    configure(&tmp.env(), &vault);
    let session = Session::new(tmp.env());
    Fixture {
        tmp,
        session,
        vault,
    }
}

/// A valid PNG of `size` bytes whose padding differs at every position (so reordered or
/// repeated chunks change the result).
fn big_png(width: u32, size: usize) -> Vec<u8> {
    let mut data = png(width, 1);
    let tail = data.split_off(data.len() - 12);
    let mut state = width.wrapping_mul(2_654_435_761);
    while data.len() + tail.len() < size {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        data.push(state as u8);
    }
    data.extend(tail);
    data
}

/// Saves `bytes` (a PNG `width` wide) at `now` ms; returns (blobId, sha256).
fn save(f: &mut Fixture, bytes: &[u8], width: u32, save_id: &str, now: i64) -> (i64, String) {
    let request = SaveRequest {
        save_id: save_id.to_owned(),
        sha256: sha256_hex(bytes),
        byte_length: bytes.len() as u64,
        mime_type: "image/png".into(),
        width,
        height: 1,
        image_url: "https://img.test/a.png".into(),
        page_url: Some("https://page.test/".into()),
        frame_url: None,
    };
    let ticket = f.session.ticket().unwrap();
    let outcome =
        images::save_image(&mut f.session, &ticket, &request, Some(bytes), None, now).unwrap();
    (outcome.blob_id, request.sha256)
}

fn token(f: &mut Fixture) -> String {
    f.session.ticket().unwrap().token
}

#[derive(Default)]
struct FakeDialogs {
    answer: Option<PathBuf>,
    asked: RefCell<Vec<(String, String)>>,
    /// Runs while the dialog is "open" (e.g. to switch the vault meanwhile).
    during: Option<Box<dyn Fn()>>,
}

impl Dialogs for FakeDialogs {
    fn choose_new_vault(&self, _: &Path, _: &str) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_existing_vault(&self, _: Option<&Path>) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_backup_dir(&self) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_export_file(&self, file_name: &str, extension: &str) -> Option<PathBuf> {
        self.asked
            .borrow_mut()
            .push((file_name.to_owned(), extension.to_owned()));
        if let Some(during) = &self.during {
            during();
        }
        self.answer.clone()
    }
}

fn ask_with(f: &mut Fixture, dialogs: &FakeDialogs, message: Value) -> Value {
    let mut message = message;
    message["requestId"] = "r".into();
    let reply = handle(&mut f.session, dialogs, &message);
    assert_eq!(reply["requestId"], "r");
    assert!(
        serde_json::to_vec(&reply).unwrap().len() < 1_000_000,
        "every reply stays below Chrome's 1 MB limit"
    );
    reply
}

fn ask(f: &mut Fixture, message: Value) -> Value {
    ask_with(f, &FakeDialogs::default(), message)
}

fn read_all(f: &mut Fixture, blob_id: i64, sha256: &str) -> Vec<u8> {
    let vault_token = token(f);
    let mut bytes = Vec::new();
    loop {
        let reply = ask(
            f,
            json!({ "type": "image_read", "blobId": blob_id, "sha256": sha256, "vaultToken": vault_token, "offset": bytes.len() }),
        );
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["type"], "image_read_result");
        assert_eq!(reply["offset"], bytes.len());
        let chunk = STANDARD.decode(reply["data"].as_str().unwrap()).unwrap();
        let total = reply["byteLength"].as_u64().unwrap() as usize;
        assert_eq!(chunk.len(), READ_CHUNK_BYTES.min(total - bytes.len()));
        bytes.extend(chunk);
        if bytes.len() == total {
            return bytes;
        }
    }
}

#[test]
fn a_large_original_reads_back_byte_for_byte_in_bounded_chunks() {
    let mut f = fixture();
    let original = big_png(7, 3 * READ_CHUNK_BYTES + 12_345);
    let (blob_id, sha256) = save(&mut f, &original, 7, "a", 1_000);
    let bytes = read_all(&mut f, blob_id, &sha256);
    assert_eq!(bytes, original);
    assert_eq!(sha256_hex(&bytes), sha256);
}

#[test]
fn a_32_mib_original_is_read_in_64_chunks_below_the_reply_limit() {
    let mut f = fixture();
    let original = big_png(9, images::MAX_IMAGE_BYTES);
    let (blob_id, sha256) = save(&mut f, &original, 9, "a", 1_000);
    let bytes = read_all(&mut f, blob_id, &sha256);
    assert_eq!(bytes.len(), images::MAX_IMAGE_BYTES);
    assert_eq!(images::MAX_IMAGE_BYTES / READ_CHUNK_BYTES, 64);
    assert!(bytes == original);
}

#[test]
fn reads_name_the_vault_and_the_bytes_they_mean() {
    let mut f = fixture();
    let original = png(3, 1);
    let (blob_id, sha256) = save(&mut f, &original, 3, "a", 1_000);
    let vault_token = token(&mut f);
    let base = json!({ "type": "image_read", "blobId": blob_id, "sha256": sha256, "vaultToken": vault_token, "offset": 0 });
    let with = |key: &str, value: Value| {
        let mut message = base.clone();
        message[key] = value;
        message
    };
    for (message, error) in [
        (with("vaultToken", "f".repeat(64).into()), "vault_switched"),
        (with("vaultToken", Value::Null), "invalid_vault_token"),
        (with("sha256", "0".repeat(64).into()), "blob_missing"),
        (with("sha256", "XYZ".into()), "invalid_hash"),
        (with("blobId", 999.into()), "blob_missing"),
        (with("blobId", 0.into()), "invalid_blob_id"),
        (with("offset", original.len().into()), "invalid_offset"),
        (with("offset", (-1).into()), "invalid_offset"),
    ] {
        let reply = ask(&mut f, message);
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], error);
        assert!(reply.get("data").is_none());
    }
    let mut no_id = base.clone();
    no_id.as_object_mut().unwrap().remove("requestId");
    let reply = handle(&mut f.session, &FakeDialogs::default(), &no_id);
    assert_eq!(reply["error"], "invalid_request_id");
}

#[test]
fn after_a_vault_switch_an_old_request_gets_nothing_from_the_new_vault() {
    let mut f = fixture();
    let (blob_id, sha256) = save(&mut f, &png(3, 1), 3, "a", 1_000);
    let old_token = token(&mut f);
    // The new vault has an image with the same blob id but other bytes.
    let other = f.tmp.0.join("w.sqlite3");
    create_vault(&other).unwrap();
    configure(&f.tmp.env(), &other);
    let (new_id, new_sha) = save(&mut f, &png(5, 1), 5, "b", 2_000);
    assert_eq!(new_id, blob_id);
    let reply = ask(
        &mut f,
        json!({ "type": "image_read", "blobId": blob_id, "sha256": sha256, "vaultToken": old_token, "offset": 0 }),
    );
    assert_eq!(reply["error"], "vault_switched");
    // Even with the new vault's token, the old image's hash does not match the new bytes.
    let new_token = token(&mut f);
    let reply = ask(
        &mut f,
        json!({ "type": "image_read", "blobId": blob_id, "sha256": sha256, "vaultToken": new_token, "offset": 0 }),
    );
    assert_eq!(reply["error"], "blob_missing");
    assert_eq!(read_all(&mut f, new_id, &new_sha), png(5, 1));
}

#[test]
fn a_vault_without_image_tables_has_no_originals_and_is_not_migrated() {
    let tmp = TempDir::new();
    let vault = tmp.0.join("legacy.sqlite3");
    rusqlite::Connection::open(&vault)
        .unwrap()
        .execute_batch(crate::SCHEMA_SQL)
        .unwrap();
    configure(&tmp.env(), &vault);
    let mut f = Fixture {
        session: Session::new(tmp.env()),
        tmp,
        vault,
    };
    let vault_token = token(&mut f);
    for message in [
        json!({ "type": "image_read", "blobId": 1, "sha256": "a".repeat(64), "vaultToken": vault_token, "offset": 0 }),
        json!({ "type": "image_thumbnail_put", "blobId": 1, "sha256": "a".repeat(64), "vaultToken": vault_token, "thumbnail": thumbnail_json(4) }),
    ] {
        assert_eq!(ask(&mut f, message)["error"], "blob_missing");
    }
    let db = open_vault(&f.vault).unwrap();
    assert_eq!(images::table_counts(&db).unwrap(), None);
}

// --- Thumbnails ----------------------------------------------------------------------------

fn thumbnail_json(width: u32) -> Value {
    json!({ "mimeType": "image/png", "width": width, "height": 1, "generator": 1, "data": STANDARD.encode(png(width, 1)) })
}

type Snapshot = (Vec<u8>, i64, i64, i64, Vec<(String, i64)>);

/// Original bytes, save count, dates and history: what a thumbnail must never change.
fn snapshot(f: &Fixture, blob_id: i64) -> Snapshot {
    let db = open_vault(&f.vault).unwrap();
    let (data, count, created, last) = db
        .query_row(
            "SELECT data, save_count, created_at, last_saved_at FROM image_blobs WHERE id = ?1",
            [blob_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    let mut statement = db
        .prepare("SELECT save_id, saved_at FROM saved_images ORDER BY id")
        .unwrap();
    let history = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    (data, count, created, last, history)
}

#[test]
fn a_rebuilt_thumbnail_is_added_once_and_changes_nothing_else() {
    let mut f = fixture();
    let original = png(30, 1);
    let (blob_id, sha256) = save(&mut f, &original, 30, "a", 1_000);
    save(&mut f, &original, 30, "b", 2_000);
    let before = snapshot(&f, blob_id);
    let vault_token = token(&mut f);
    let put = |width| json!({ "type": "image_thumbnail_put", "blobId": blob_id, "sha256": sha256, "vaultToken": vault_token, "thumbnail": thumbnail_json(width) });
    let reply = ask(&mut f, put(4));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["stored"], true);
    // A second rebuild (another panel, a retry) keeps the first one.
    let reply = ask(&mut f, put(6));
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["stored"], false);
    let stored = images::thumbnail(&open_vault(&f.vault).unwrap(), blob_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.width, 4);
    assert_eq!(snapshot(&f, blob_id), before);
}

#[test]
fn thumbnails_are_refused_for_another_vault_other_bytes_or_bad_data() {
    let mut f = fixture();
    let (blob_id, sha256) = save(&mut f, &png(30, 1), 30, "a", 1_000);
    let vault_token = token(&mut f);
    let base = json!({ "type": "image_thumbnail_put", "blobId": blob_id, "sha256": sha256, "vaultToken": vault_token, "thumbnail": thumbnail_json(4) });
    let with = |key: &str, value: Value| {
        let mut message = base.clone();
        message[key] = value;
        message
    };
    let svg = json!({ "mimeType": "image/png", "width": 4, "height": 1, "generator": 1, "data": STANDARD.encode("<svg xmlns='http://www.w3.org/2000/svg'/>") });
    let wrong_size = json!({ "mimeType": "image/png", "width": 5, "height": 1, "generator": 1, "data": STANDARD.encode(png(4, 1)) });
    for (message, error) in [
        (with("vaultToken", "e".repeat(64).into()), "vault_switched"),
        (with("sha256", "0".repeat(64).into()), "blob_missing"),
        (with("thumbnail", svg), "invalid_thumbnail"),
        (with("thumbnail", wrong_size), "invalid_thumbnail"),
        (with("thumbnail", Value::Null), "invalid_thumbnail"),
    ] {
        let reply = ask(&mut f, message);
        assert_eq!(reply["error"], error);
    }
    assert_eq!(
        images::thumbnail(&open_vault(&f.vault).unwrap(), blob_id).unwrap(),
        None
    );
}

#[test]
fn a_thumbnail_for_the_old_vault_is_not_written_after_a_switch() {
    let mut f = fixture();
    let (blob_id, sha256) = save(&mut f, &png(30, 1), 30, "a", 1_000);
    let old_token = token(&mut f);
    let other = f.tmp.0.join("w.sqlite3");
    create_vault(&other).unwrap();
    configure(&f.tmp.env(), &other);
    save(&mut f, &png(30, 1), 30, "b", 2_000); // same bytes, same id in the new vault
    let reply = ask(
        &mut f,
        json!({ "type": "image_thumbnail_put", "blobId": blob_id, "sha256": sha256, "vaultToken": old_token, "thumbnail": thumbnail_json(4) }),
    );
    assert_eq!(reply["error"], "vault_switched");
    for vault in [&f.vault, &other] {
        assert_eq!(
            images::thumbnail(&open_vault(vault).unwrap(), blob_id).unwrap(),
            None
        );
    }
}

// --- Export --------------------------------------------------------------------------------

fn export_message(f: &mut Fixture, blob_id: i64, sha256: &str) -> Value {
    let vault_token = token(f);
    json!({ "type": "image_export", "blobId": blob_id, "sha256": sha256, "vaultToken": vault_token })
}

#[test]
fn export_suggests_the_last_save_time_and_real_extension_and_writes_the_exact_bytes() {
    let mut f = fixture();
    let original = big_png(11, READ_CHUNK_BYTES + 99);
    let (blob_id, sha256) = save(&mut f, &original, 11, "a", 1_730_000_000_123);
    save(&mut f, &original, 11, "b", 1_730_000_500_999);
    let out = f.tmp.0.join("out");
    fs::create_dir_all(&out).unwrap();
    let dialogs = FakeDialogs {
        answer: Some(out.join("picked.png")),
        ..FakeDialogs::default()
    };
    let message = export_message(&mut f, blob_id, &sha256);
    let reply = ask_with(&mut f, &dialogs, message);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["type"], "image_export_result");
    assert_eq!(reply["fileName"], "picked.png");
    assert!(
        !reply
            .to_string()
            .contains(&out.to_string_lossy().replace('\\', "\\\\")),
        "the folder never goes back to the extension"
    );
    assert_eq!(
        dialogs.asked.borrow().as_slice(),
        [("1730000500.png".to_owned(), "png".to_owned())]
    );
    assert!(fs::read(out.join("picked.png")).unwrap() == original);
    assert_eq!(
        fs::read_dir(&out).unwrap().count(),
        1,
        "no temporary file is left"
    );
}

#[test]
fn the_suggested_name_uses_epoch_seconds() {
    assert_eq!(export_file_name(1_730_000_000_999, "jpg"), "1730000000.jpg");
    assert_eq!(export_file_name(999, "webp"), "0.webp");
}

#[test]
fn cancelling_the_dialog_is_a_normal_outcome_and_writes_nothing() {
    let mut f = fixture();
    let (blob_id, sha256) = save(&mut f, &png(3, 1), 3, "a", 1_000);
    let before = fs::read_dir(&f.tmp.0).unwrap().count();
    let message = export_message(&mut f, blob_id, &sha256);
    let reply = ask(&mut f, message);
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["cancelled"], true);
    assert!(reply.get("fileName").is_none());
    assert_eq!(fs::read_dir(&f.tmp.0).unwrap().count(), before);
}

#[test]
fn an_existing_file_confirmed_in_the_dialog_is_replaced_completely() {
    let mut f = fixture();
    let original = png(3, 1);
    let (blob_id, sha256) = save(&mut f, &original, 3, "a", 1_000);
    let target = f.tmp.0.join("existing.png");
    fs::write(&target, vec![7u8; 10_000]).unwrap();
    let dialogs = FakeDialogs {
        answer: Some(target.clone()),
        ..FakeDialogs::default()
    };
    let message = export_message(&mut f, blob_id, &sha256);
    assert_eq!(ask_with(&mut f, &dialogs, message)["ok"], true);
    assert_eq!(fs::read(&target).unwrap(), original);
}

#[test]
fn a_file_that_appears_after_the_dialog_is_never_overwritten() {
    let tmp = TempDir::new();
    let target = tmp.0.join("late.png");
    // The dialog closed when no file existed (replace = false); then someone created it.
    fs::write(&target, b"theirs").unwrap();
    assert_eq!(
        write_export(&target, b"ours", false),
        Err("export_file_appeared")
    );
    assert_eq!(fs::read(&target).unwrap(), b"theirs");
    assert_eq!(
        fs::read_dir(&tmp.0).unwrap().count(),
        1,
        "the temporary file is removed"
    );
}

#[test]
fn a_failed_replace_leaves_the_existing_file_untouched() {
    let tmp = TempDir::new();
    let target = tmp.0.join("locked.png");
    fs::write(&target, b"keep me").unwrap();
    let mut permissions = fs::metadata(&target).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&target, permissions.clone()).unwrap();
    let result = write_export(&target, b"new bytes", true);
    assert_eq!(result, Err("export_failed"));
    assert_eq!(fs::read(&target).unwrap(), b"keep me");
    assert_eq!(
        fs::read_dir(&tmp.0).unwrap().count(),
        1,
        "the temporary file is removed"
    );
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&target, permissions).unwrap();
}

#[test]
fn a_folder_or_a_missing_folder_is_not_a_target() {
    let tmp = TempDir::new();
    assert_eq!(
        write_export(&tmp.0, b"x", true),
        Err("export_invalid_target")
    );
    assert_eq!(
        write_export(&tmp.0.join("missing").join("a.png"), b"x", false),
        Err("parent_missing")
    );
}

#[test]
fn export_writes_the_listed_vaults_image_even_if_the_vault_switches_during_the_dialog() {
    let mut f = fixture();
    let original = png(3, 1);
    let (blob_id, sha256) = save(&mut f, &original, 3, "a", 1_000);
    let other = f.tmp.0.join("w.sqlite3");
    create_vault(&other).unwrap();
    let env = f.tmp.env();
    let target = f.tmp.0.join("a.png");
    let dialogs = FakeDialogs {
        answer: Some(target.clone()),
        during: Some(Box::new(move || configure(&env, &other))),
        ..FakeDialogs::default()
    };
    let message = export_message(&mut f, blob_id, &sha256);
    assert_eq!(ask_with(&mut f, &dialogs, message)["ok"], true);
    assert_eq!(fs::read(&target).unwrap(), original);
}

#[test]
fn export_for_another_vault_or_other_bytes_opens_no_dialog() {
    let mut f = fixture();
    let (blob_id, sha256) = save(&mut f, &png(3, 1), 3, "a", 1_000);
    let dialogs = FakeDialogs {
        answer: Some(f.tmp.0.join("never.png")),
        ..FakeDialogs::default()
    };
    let mut message = export_message(&mut f, blob_id, &sha256);
    message["vaultToken"] = "d".repeat(64).into();
    assert_eq!(
        ask_with(&mut f, &dialogs, message)["error"],
        "vault_switched"
    );
    let message = export_message(&mut f, blob_id, &"1".repeat(64));
    assert_eq!(ask_with(&mut f, &dialogs, message)["error"], "blob_missing");
    assert!(dialogs.asked.borrow().is_empty());
    assert!(!f.tmp.0.join("never.png").exists());
}

#[test]
fn export_never_changes_the_vault() {
    let mut f = fixture();
    let original = png(3, 1);
    let (blob_id, sha256) = save(&mut f, &original, 3, "a", 1_000);
    let before = snapshot(&f, blob_id);
    let dialogs = FakeDialogs {
        answer: Some(f.tmp.0.join("a.png")),
        ..FakeDialogs::default()
    };
    let message = export_message(&mut f, blob_id, &sha256);
    assert_eq!(ask_with(&mut f, &dialogs, message)["ok"], true);
    assert_eq!(snapshot(&f, blob_id), before);
}
