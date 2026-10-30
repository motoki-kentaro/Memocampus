use super::*;
use crate::images::sha256_hex;
use crate::images::tests::{configure, png};
use crate::vault::tests::TempDir;
use crate::vault::{create_vault, open_vault, VaultError};
use std::path::PathBuf;

const SAVE_A: &str = "0b7a3f2e-1c4d-4e5f-8a9b-0c1d2e3f4a5b";
const SAVE_B: &str = "1c8b4a3f-2d5e-4f60-9bac-1d2e3f4a5b6c";

/// A PNG whose IDAT payload makes it `size` bytes long (structure only; the host does not decode).
fn big_png(width: u32, height: u32, size: usize) -> Vec<u8> {
    let head = png(width, height);
    let (prefix, iend) = head.split_at(head.len() - 12);
    let prefix = &prefix[..prefix.len() - 13]; // drop the tiny IDAT chunk
    let payload = size - prefix.len() - 12 - 12;
    let mut bytes = prefix.to_vec();
    bytes.extend_from_slice(&(payload as u32).to_be_bytes());
    bytes.extend_from_slice(b"IDAT");
    bytes.extend((0..payload).map(|i| (i % 251) as u8));
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(iend);
    assert_eq!(bytes.len(), size);
    bytes
}

struct Harness {
    tmp: TempDir,
    session: Session,
    uploads: Uploads,
    vault: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let tmp = TempDir::new();
        let vault = tmp.0.join("v.sqlite3");
        create_vault(&vault).unwrap();
        configure(&tmp.env(), &vault);
        let session = Session::new(tmp.env());
        Self {
            tmp,
            session,
            uploads: Uploads::default(),
            vault,
        }
    }

    fn send(&mut self, message: Value) -> Value {
        let reply = self
            .uploads
            .handle_at(&mut self.session, &message, 1_793_330_100_000);
        assert!(
            serde_json::to_vec(&reply).unwrap().len() < 1_000_000,
            "replies stay below Chrome's 1 MB limit"
        );
        reply
    }

    fn count(&self, table: &str) -> i64 {
        count_in(&self.vault, table)
    }
}

fn count_in(vault: &std::path::Path, table: &str) -> i64 {
    open_vault(vault)
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn begin(save_id: &str, bytes: &[u8], width: u32, height: u32) -> Value {
    json!({
        "type": "image_save_begin", "protocol": 1, "saveId": save_id,
        "sha256": sha256_hex(bytes), "byteLength": bytes.len(), "mimeType": "image/png",
        "width": width, "height": height,
        "imageUrl": "https://img.test/a.png", "pageUrl": "https://page.test/", "frameUrl": null,
    })
}

fn chunk(save_id: &str, index: usize, bytes: &[u8]) -> Value {
    json!({ "type": "image_save_chunk", "saveId": save_id, "index": index, "data": STANDARD.encode(bytes) })
}

fn commit(save_id: &str) -> Value {
    json!({ "type": "image_save_commit", "saveId": save_id })
}

/// Sends every chunk of `bytes` and returns the acks.
fn upload_all(h: &mut Harness, save_id: &str, bytes: &[u8]) -> Vec<Value> {
    bytes
        .chunks(CHUNK_BYTES)
        .enumerate()
        .map(|(i, part)| h.send(chunk(save_id, i, part)))
        .collect()
}

#[test]
fn a_multi_chunk_upload_is_stored_only_at_commit() {
    let mut h = Harness::new();
    let bytes = big_png(64, 48, 2 * CHUNK_BYTES + 12345);
    let ready = h.send(begin(SAVE_A, &bytes, 64, 48));
    assert_eq!(ready["type"], "image_save_ready");
    assert_eq!(
        (
            ready["upload"].clone(),
            ready["chunkBytes"].clone(),
            ready["maxBytes"].clone()
        ),
        (json!(true), json!(CHUNK_BYTES), json!(MAX_IMAGE_BYTES))
    );
    let acks = upload_all(&mut h, SAVE_A, &bytes);
    assert_eq!(
        acks.iter()
            .map(|a| a["received"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [
            CHUNK_BYTES as u64,
            2 * CHUNK_BYTES as u64,
            bytes.len() as u64
        ]
    );
    assert_eq!(
        h.count("image_blobs"),
        0,
        "nothing is visible before the commit"
    );
    let saved = h.send(commit(SAVE_A));
    assert_eq!(saved["type"], "image_saved", "{saved}");
    assert_eq!(
        (saved["deduplicated"].clone(), saved["replayed"].clone()),
        (json!(false), json!(false))
    );
    let stored: Vec<u8> = open_vault(&h.vault)
        .unwrap()
        .query_row("SELECT data FROM image_blobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, bytes);
    assert!(
        h.uploads.active.is_none(),
        "the buffer is released after the commit"
    );
}

#[test]
fn resending_a_committed_save_returns_the_original_result_without_new_history() {
    let mut h = Harness::new();
    let bytes = png(3, 3);
    h.send(begin(SAVE_A, &bytes, 3, 3));
    upload_all(&mut h, SAVE_A, &bytes);
    let first = h.send(commit(SAVE_A));
    // The ACK was lost; the extension retries the same save action on a new port.
    let mut retry = Uploads::default();
    let replay = retry.handle_at(&mut h.session, &begin(SAVE_A, &bytes, 3, 3), 2);
    assert_eq!(replay["type"], "image_saved");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["savedImageId"], first["savedImageId"]);
    assert!(retry.active.is_none());
    assert_eq!((h.count("image_blobs"), h.count("saved_images")), (1, 1));
}

#[test]
fn known_bytes_are_not_uploaded_again_and_only_history_is_added() {
    let mut h = Harness::new();
    let bytes = png(5, 5);
    h.send(begin(SAVE_A, &bytes, 5, 5));
    upload_all(&mut h, SAVE_A, &bytes);
    h.send(commit(SAVE_A));
    let ready = h.send(begin(SAVE_B, &bytes, 5, 5));
    assert_eq!(ready["upload"], false);
    let saved = h.send(commit(SAVE_B));
    assert_eq!(
        (saved["deduplicated"].clone(), saved["replayed"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!((h.count("image_blobs"), h.count("saved_images")), (1, 2));
    // Sending bytes anyway is a protocol error.
    let third = "2d9c5b4a-3e6f-4071-8cbd-2e3f4a5b6c7d";
    h.send(begin(third, &bytes, 5, 5));
    assert_eq!(h.send(chunk(third, 0, &bytes))["error"], "unexpected_chunk");
}

#[test]
fn a_closed_port_leaves_nothing_behind() {
    let mut h = Harness::new();
    let bytes = big_png(8, 8, CHUNK_BYTES + 10);
    h.send(begin(SAVE_A, &bytes, 8, 8));
    h.send(chunk(SAVE_A, 0, &bytes[..CHUNK_BYTES]));
    // Chrome closing the port ends the host process, dropping the upload state.
    h.uploads = Uploads::default();
    assert_eq!(h.count("image_blobs"), 0);
    assert_eq!(h.count("saved_images"), 0);
    assert_eq!(h.send(commit(SAVE_A))["error"], "no_upload");
}

#[test]
fn out_of_order_duplicate_and_malformed_chunks_abort_the_upload() {
    let bytes = big_png(8, 8, 2 * CHUNK_BYTES + 5);
    let parts: Vec<&[u8]> = bytes.chunks(CHUNK_BYTES).collect();
    let cases: Vec<(Vec<Value>, &str)> = vec![
        (vec![chunk(SAVE_A, 1, parts[1])], "chunk_out_of_order"),
        (
            vec![chunk(SAVE_A, 0, parts[0]), chunk(SAVE_A, 0, parts[0])],
            "duplicate_chunk",
        ),
        (
            vec![
                json!({ "type": "image_save_chunk", "saveId": SAVE_A, "index": 0, "data": "@@not base64@@" }),
            ],
            "invalid_base64",
        ),
        (
            vec![
                json!({ "type": "image_save_chunk", "saveId": SAVE_A, "index": 0, "data": "A".repeat(base64_len(CHUNK_BYTES) + 4) }),
            ],
            "invalid_chunk_size",
        ),
        (
            vec![chunk(SAVE_A, 0, &parts[0][..1000])],
            "invalid_chunk_size",
        ),
        (vec![chunk(SAVE_B, 0, parts[0])], "save_id_mismatch"),
        (
            vec![json!({ "type": "image_save_chunk", "saveId": SAVE_A, "data": "" })],
            "invalid_message",
        ),
    ];
    for (messages, expected) in cases {
        let mut h = Harness::new();
        h.send(begin(SAVE_A, &bytes, 8, 8));
        let last = messages.into_iter().map(|m| h.send(m)).last().unwrap();
        assert_eq!(last["error"], expected, "{last}");
        assert!(last["saveId"].as_str().is_some());
        assert!(
            h.uploads.active.is_none(),
            "{expected}: the buffer is discarded"
        );
        assert_eq!(h.send(commit(SAVE_A))["error"], "no_upload");
        assert_eq!(h.count("image_blobs"), 0);
    }
}

#[test]
fn bytes_beyond_the_declared_length_are_refused_while_receiving() {
    let mut h = Harness::new();
    let bytes = png(2, 2);
    let mut longer = bytes.clone();
    longer.extend_from_slice(&[0; 16]);
    h.send(begin(SAVE_A, &bytes, 2, 2));
    assert_eq!(
        h.send(chunk(SAVE_A, 0, &longer))["error"],
        "upload_too_large"
    );
    let mut h = Harness::new();
    h.send(begin(SAVE_A, &bytes, 2, 2));
    upload_all(&mut h, SAVE_A, &bytes);
    assert_eq!(h.send(chunk(SAVE_A, 1, &[0]))["error"], "upload_too_large");
}

#[test]
fn the_32_mib_limit_is_enforced_at_begin_before_any_buffering() {
    let mut h = Harness::new();
    let mut too_big = begin(SAVE_A, &png(1, 1), 1, 1);
    too_big["byteLength"] = json!(MAX_IMAGE_BYTES + 1);
    assert_eq!(h.send(too_big)["error"], "too_large");
    assert!(h.uploads.active.is_none());
    let mut exact = begin(SAVE_A, &png(1, 1), 1, 1);
    exact["byteLength"] = json!(MAX_IMAGE_BYTES);
    assert_eq!(h.send(exact)["upload"], true, "exactly 32 MiB is accepted");
}

#[test]
fn an_incomplete_or_corrupted_upload_is_never_committed() {
    let mut h = Harness::new();
    let bytes = big_png(8, 8, CHUNK_BYTES + 100);
    h.send(begin(SAVE_A, &bytes, 8, 8));
    h.send(chunk(SAVE_A, 0, &bytes[..CHUNK_BYTES]));
    assert_eq!(h.send(commit(SAVE_A))["error"], "incomplete_upload");

    let mut corrupted = bytes.clone();
    corrupted[CHUNK_BYTES + 50] ^= 0xff;
    h.send(begin(SAVE_A, &bytes, 8, 8));
    upload_all(&mut h, SAVE_A, &corrupted);
    assert_eq!(h.send(commit(SAVE_A))["error"], "hash_mismatch");
    assert_eq!((h.count("image_blobs"), h.count("saved_images")), (0, 0));
}

#[test]
fn begin_is_validated_strictly() {
    let mut h = Harness::new();
    let bytes = png(1, 1);
    let with = |key: &str, value: Value| {
        let mut message = begin(SAVE_A, &bytes, 1, 1);
        message[key] = value;
        message
    };
    for (message, expected) in [
        (with("protocol", json!(2)), "unsupported_protocol"),
        (with("protocol", Value::Null), "unsupported_protocol"),
        (with("saveId", json!("s1")), "invalid_save_id"),
        (
            with("saveId", json!("0B7A3F2E-1C4D-4E5F-8A9B-0C1D2E3F4A5B")),
            "invalid_save_id",
        ),
        (
            with("mimeType", json!("image/svg+xml")),
            "svg_not_supported",
        ),
        (
            with("imageUrl", json!("javascript:alert(1)")),
            "invalid_url",
        ),
        (with("width", json!(-1)), "invalid_message"),
        (with("width", json!(u64::MAX)), "invalid_dimensions"),
        (with("byteLength", json!("12")), "invalid_message"),
        (with("pageUrl", json!(42)), "invalid_message"),
    ] {
        assert_eq!(h.send(message.clone())["error"], expected, "{message}");
        assert!(h.uploads.active.is_none());
    }
    h.send(begin(SAVE_A, &bytes, 1, 1));
    assert_eq!(
        h.send(begin(SAVE_B, &bytes, 1, 1))["error"],
        "upload_in_progress"
    );
    assert!(
        h.uploads.active.is_none(),
        "a confused client loses the open upload too"
    );
}

#[test]
fn a_vault_switch_during_the_transfer_fails_the_save_in_every_vault() {
    let mut h = Harness::new();
    let other = h.tmp.0.join("other.sqlite3");
    create_vault(&other).unwrap();
    let bytes = png(6, 6);
    h.send(begin(SAVE_A, &bytes, 6, 6));
    upload_all(&mut h, SAVE_A, &bytes);
    configure(&h.tmp.env(), &other);
    let reply = h.send(commit(SAVE_A));
    assert_eq!(reply["error"], VaultError::Switched.code());
    assert_eq!(count_in(&h.vault, "image_blobs"), 0);
    assert_eq!(count_in(&other, "image_blobs"), 0);
}

#[test]
fn a_history_only_save_checked_in_one_vault_is_not_recorded_in_another() {
    let mut h = Harness::new();
    let bytes = png(7, 7);
    h.send(begin(SAVE_A, &bytes, 7, 7));
    upload_all(&mut h, SAVE_A, &bytes);
    h.send(commit(SAVE_A));
    // The precheck finds the bytes in vault A …
    assert_eq!(h.send(begin(SAVE_B, &bytes, 7, 7))["upload"], false);
    // … then vault B (without those bytes) becomes active before the commit.
    let other = h.tmp.0.join("other.sqlite3");
    create_vault(&other).unwrap();
    configure(&h.tmp.env(), &other);
    assert_eq!(h.send(commit(SAVE_B))["error"], "vault_switched");
    assert_eq!(count_in(&other, "saved_images"), 0);
    assert_eq!(count_in(&h.vault, "saved_images"), 1);
}

#[test]
fn a_thumbnail_with_a_lying_size_fails_the_whole_save() {
    let mut h = Harness::new();
    let bytes = png(16, 16);
    h.send(begin(SAVE_A, &bytes, 16, 16));
    upload_all(&mut h, SAVE_A, &bytes);
    let mut message = commit(SAVE_A);
    message["thumbnail"] = json!({ "mimeType": "image/png", "width": 9, "height": 2, "generator": 1, "data": STANDARD.encode(png(4, 2)) });
    assert_eq!(h.send(message)["error"], "invalid_thumbnail");
    assert_eq!(h.count("image_blobs"), 0);

    h.send(begin(SAVE_A, &bytes, 16, 16));
    upload_all(&mut h, SAVE_A, &bytes);
    let mut message = commit(SAVE_A);
    message["thumbnail"] = json!({ "mimeType": "image/png", "width": 4, "height": 2, "generator": 1, "data": STANDARD.encode(png(4, 2)) });
    assert_eq!(h.send(message)["type"], "image_saved");
    assert_eq!(h.count("image_thumbnails"), 1);
}

#[test]
fn abort_discards_the_upload() {
    let mut h = Harness::new();
    let bytes = png(2, 2);
    h.send(begin(SAVE_A, &bytes, 2, 2));
    assert_eq!(
        h.send(json!({ "type": "image_save_abort", "saveId": SAVE_A }))["type"],
        "image_save_aborted"
    );
    assert_eq!(h.send(chunk(SAVE_A, 0, &bytes))["error"], "no_upload");
}

#[test]
fn without_a_vault_the_save_fails_before_any_transfer() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let mut uploads = Uploads::default();
    let reply = uploads.handle_at(&mut session, &begin(SAVE_A, &png(1, 1), 1, 1), 1);
    assert_eq!(
        reply,
        json!({ "ok": false, "type": "image_save_error", "saveId": SAVE_A, "error": "vault_not_configured" })
    );
}

#[test]
fn uuid_check_matches_crypto_random_uuid_output() {
    assert!(is_uuid(SAVE_A));
    for bad in [
        "",
        "0b7a3f2e1c4d4e5f8a9b0c1d2e3f4a5b",
        "0b7a3f2e-1c4d-4e5f-8a9b-0c1d2e3f4a5",
        "0b7a3f2e-1c4d-4e5f-8a9b-0c1d2e3f4a5bz",
        "g b7a3f2e-1c4d-4e5f-8a9b-0c1d2e3f4a5b",
    ] {
        assert!(!is_uuid(bad), "{bad}");
    }
}

fn begin_with_token(save_id: &str, bytes: &[u8], token: &str) -> Value {
    let mut message = begin(save_id, bytes, 4, 4);
    message["vaultToken"] = json!(token);
    message
}

#[test]
fn replies_carry_an_opaque_vault_token_and_never_a_path() {
    let mut h = Harness::new();
    let bytes = png(4, 4);
    let ready = h.send(begin(SAVE_A, &bytes, 4, 4));
    let token = ready["vaultToken"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    let text = ready.to_string().to_lowercase();
    assert!(
        !text.contains("sqlite3") && !text.contains("memocampus-unit"),
        "no path in {text}"
    );
    upload_all(&mut h, SAVE_A, &bytes);
    assert_eq!(h.send(commit(SAVE_A))["vaultToken"], json!(token));
}

#[test]
fn a_reconnect_to_the_same_vault_may_continue_the_save_action() {
    let mut h = Harness::new();
    let bytes = png(4, 4);
    let token = h.send(begin(SAVE_A, &bytes, 4, 4))["vaultToken"]
        .as_str()
        .unwrap()
        .to_owned();
    // The port broke before any chunk; a new port (new host process) starts over with the token.
    h.uploads = Uploads::default();
    h.session = Session::new(h.tmp.env());
    assert_eq!(
        h.send(begin_with_token(SAVE_A, &bytes, &token))["type"],
        "image_save_ready"
    );
    upload_all(&mut h, SAVE_A, &bytes);
    assert_eq!(h.send(commit(SAVE_A))["type"], "image_saved");
}

#[test]
fn a_reconnect_that_finds_another_vault_active_stops_the_save() {
    let mut h = Harness::new();
    let bytes = png(4, 4);
    let token_a = h.send(begin(SAVE_A, &bytes, 4, 4))["vaultToken"]
        .as_str()
        .unwrap()
        .to_owned();
    // The connection drops; meanwhile the user switches to vault B.
    h.uploads = Uploads::default();
    let other = h.tmp.0.join("other.sqlite3");
    create_vault(&other).unwrap();
    configure(&h.tmp.env(), &other);
    let mut session = Session::new(h.tmp.env());
    let reply = h
        .uploads
        .handle_at(&mut session, &begin_with_token(SAVE_A, &bytes, &token_a), 5);
    assert_eq!(reply["error"], "vault_switched");
    assert!(h.uploads.active.is_none());
    assert_eq!(count_in(&other, "image_blobs"), 0);
    assert_eq!(count_in(&other, "saved_images"), 0);
    assert_eq!(count_in(&h.vault, "image_blobs"), 0);
}

#[test]
fn a_resend_after_a_lost_ack_follows_the_same_rule() {
    let mut h = Harness::new();
    let bytes = png(4, 4);
    h.send(begin(SAVE_A, &bytes, 4, 4));
    upload_all(&mut h, SAVE_A, &bytes);
    let token_a = h.send(commit(SAVE_A))["vaultToken"]
        .as_str()
        .unwrap()
        .to_owned();
    // Same vault: the resend learns the save was committed, without a new history row.
    let mut retry = Uploads::default();
    let replay = retry.handle_at(
        &mut h.session,
        &begin_with_token(SAVE_A, &bytes, &token_a),
        6,
    );
    assert_eq!(
        (replay["type"].clone(), replay["replayed"].clone()),
        (json!("image_saved"), json!(true))
    );
    // Another vault active: refused, even though that vault could have accepted the bytes.
    let other = h.tmp.0.join("other.sqlite3");
    create_vault(&other).unwrap();
    configure(&h.tmp.env(), &other);
    let refused = Uploads::default().handle_at(
        &mut Session::new(h.tmp.env()),
        &begin_with_token(SAVE_A, &bytes, &token_a),
        7,
    );
    assert_eq!(refused["error"], "vault_switched");
    assert_eq!(
        (
            count_in(&h.vault, "saved_images"),
            count_in(&other, "saved_images")
        ),
        (1, 0)
    );
}

#[test]
fn the_vault_token_ignores_how_the_path_is_spelled() {
    let h = Harness::new();
    let canonical = crate::vault::vault_token(&h.vault).unwrap();
    let upper = PathBuf::from(h.vault.to_string_lossy().to_uppercase());
    assert_eq!(
        crate::vault::vault_token(&upper).unwrap(),
        canonical,
        "Windows paths are case-insensitive"
    );
    let dotted = h
        .vault
        .parent()
        .unwrap()
        .join(".")
        .join(h.vault.file_name().unwrap());
    assert_eq!(crate::vault::vault_token(&dotted).unwrap(), canonical);
    // A different file is a different vault, even with the same name elsewhere.
    let twin_dir = h.tmp.0.join("twin");
    std::fs::create_dir(&twin_dir).unwrap();
    let twin = twin_dir.join("v.sqlite3");
    create_vault(&twin).unwrap();
    assert_ne!(crate::vault::vault_token(&twin).unwrap(), canonical);
    assert_eq!(
        crate::vault::vault_token(&h.tmp.0.join("missing.sqlite3")),
        Err(VaultError::Missing)
    );
}
