use super::*;
use crate::images::tests::{configure, png};
use crate::images::{sha256_hex, SaveRequest, ThumbnailInput, MAX_URL_CHARS};
use crate::vault::tests::TempDir;
use crate::vault::{create_vault, open_vault};
use std::path::PathBuf;

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

fn save(
    f: &mut Fixture,
    width: u32,
    save_id: &str,
    now: i64,
    url_len: usize,
    thumbnail: Option<ThumbnailInput>,
) {
    let bytes = png(width, 1);
    let request = SaveRequest {
        save_id: save_id.to_owned(),
        sha256: sha256_hex(&bytes),
        byte_length: bytes.len() as u64,
        mime_type: "image/png".into(),
        width,
        height: 1,
        image_url: format!("https://img.test/{}", "x".repeat(url_len)),
        page_url: Some(format!("https://page.test/{}", "y".repeat(url_len))),
        frame_url: None,
        incognito: false,
    };
    let ticket = f.session.ticket().unwrap();
    images::save_image(
        &mut f.session,
        &ticket,
        &request,
        Some(&bytes),
        thumbnail.as_ref(),
        now,
    )
    .unwrap();
}

fn thumb(width: u32, padding: usize) -> ThumbnailInput {
    let mut data = png(width, 1);
    // Pad inside the PNG (before IEND) so it stays a valid PNG of the given size.
    let tail = data.split_off(data.len() - 12);
    data.extend(std::iter::repeat_n(0u8, padding));
    data.extend(tail);
    ThumbnailInput {
        mime_type: "image/png".into(),
        width,
        height: 1,
        generator: 1,
        data,
    }
}

fn ask(f: &mut Fixture, message: Value) -> Value {
    let mut message = message;
    message["requestId"] = "r".into();
    let reply = handle(&mut f.session, &message);
    let size = serde_json::to_vec(&reply).unwrap().len();
    assert!(
        size < RESPONSE_BUDGET,
        "reply of {size} bytes exceeds the budget"
    );
    assert_eq!(reply["requestId"], "r");
    reply
}

#[test]
fn the_list_is_metadata_only_newest_first_and_names_its_vault() {
    let mut f = fixture();
    for i in 1..=3 {
        save(&mut f, i, &format!("s{i}"), i64::from(i), 10, None);
    }
    let reply = ask(&mut f, json!({ "type": "image_list" }));
    assert_eq!(reply["type"], "image_list_result");
    assert_eq!(reply["ok"], true);
    let widths: Vec<_> = reply["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["width"].as_i64().unwrap())
        .collect();
    assert_eq!(widths, [3, 2, 1]);
    assert!(
        reply["items"][0].get("data").is_none(),
        "no image bytes in the list"
    );
    assert_eq!(reply["vaultToken"].as_str().unwrap().len(), 64);
    assert_eq!(reply["next"], Value::Null);
}

#[test]
fn a_page_that_would_exceed_the_budget_ends_early_and_resumes_exactly() {
    let mut f = fixture();
    // 60 images whose two URLs are ~16 KB together: a full page would be ~1 MB.
    for i in 1..=60 {
        save(
            &mut f,
            i,
            &format!("s{i}"),
            i64::from(i),
            MAX_URL_CHARS - 20,
            None,
        );
    }
    let mut seen = Vec::new();
    let mut cursor = Value::Null;
    let mut pages = 0;
    loop {
        let reply = ask(
            &mut f,
            json!({ "type": "image_list", "cursor": cursor, "limit": 60 }),
        );
        pages += 1;
        seen.extend(
            reply["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["width"].as_i64().unwrap()),
        );
        cursor = reply["next"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert!(pages > 1, "the budget split the page");
    assert_eq!(
        seen,
        (1..=60).rev().collect::<Vec<i64>>(),
        "nothing lost or repeated across the split"
    );
}

#[test]
fn thumbnails_are_returned_within_the_budget_and_the_rest_deferred() {
    let mut f = fixture();
    for i in 1..=6 {
        save(
            &mut f,
            i,
            &format!("s{i}"),
            i64::from(i),
            10,
            Some(thumb(i, 200 * 1024)),
        );
    }
    save(&mut f, 7, "no-thumb", 7, 10, None);
    let ids: Vec<i64> = (1..=7).collect();
    let reply = ask(
        &mut f,
        json!({ "type": "image_thumbnails", "blobIds": ids }),
    );
    let items = reply["items"].as_array().unwrap();
    let deferred: Vec<i64> = reply["deferred"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    assert!(
        !deferred.is_empty(),
        "~1.6 MB of thumbnails cannot fit one reply"
    );
    let returned: Vec<i64> = items
        .iter()
        .map(|i| i["blobId"].as_i64().unwrap())
        .collect();
    let mut all = returned.clone();
    all.extend(&deferred);
    assert_eq!(
        all, ids,
        "every id is either answered or deferred, in order"
    );
    let first = &items[0];
    assert_eq!(first["mimeType"], "image/png");
    assert_eq!(
        decode_base64(first["data"].as_str().unwrap()).len(),
        thumb(1, 200 * 1024).data.len()
    );
    // The deferred ones come back on request; the image without a thumbnail is "missing", not dropped.
    let rest = ask(
        &mut f,
        json!({ "type": "image_thumbnails", "blobIds": [7] }),
    );
    assert_eq!(rest["items"][0], json!({ "blobId": 7, "missing": true }));
}

fn decode_base64(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap()
}

#[test]
fn a_stored_thumbnail_that_is_not_a_plain_raster_is_reported_missing() {
    let mut f = fixture();
    save(&mut f, 2, "s", 1, 10, Some(thumb(2, 0)));
    // Tamper with the stored thumbnail (it is derived data and must not be trusted blindly).
    open_vault(&f.vault)
        .unwrap()
        .execute(
            "UPDATE image_thumbnails SET data = CAST('<svg onload=alert(1)>' AS BLOB)",
            [],
        )
        .unwrap();
    let reply = ask(
        &mut f,
        json!({ "type": "image_thumbnails", "blobIds": [1] }),
    );
    assert_eq!(reply["items"][0], json!({ "blobId": 1, "missing": true }));
}

#[test]
fn history_is_paged_newest_first() {
    let mut f = fixture();
    for i in 0..25 {
        let bytes = png(5, 5);
        let request = SaveRequest {
            save_id: format!("h{i}"),
            sha256: sha256_hex(&bytes),
            byte_length: bytes.len() as u64,
            mime_type: "image/png".into(),
            width: 5,
            height: 5,
            image_url: format!("https://img.test/{i}"),
            page_url: None,
            frame_url: None,
            incognito: false,
        };
        let ticket = f.session.ticket().unwrap();
        images::save_image(&mut f.session, &ticket, &request, Some(&bytes), None, i).unwrap();
    }
    let first = ask(&mut f, json!({ "type": "image_sources", "blobId": 1 }));
    assert_eq!(first["items"].as_array().unwrap().len(), 20);
    assert_eq!(first["items"][0]["imageUrl"], "https://img.test/24");
    let rest = ask(
        &mut f,
        json!({ "type": "image_sources", "blobId": 1, "cursor": first["next"] }),
    );
    assert_eq!(rest["items"].as_array().unwrap().len(), 5);
    assert_eq!(rest["next"], Value::Null);
}

#[test]
fn malformed_requests_and_missing_vaults_are_errors() {
    let mut f = fixture();
    for (message, code) in [
        (
            json!({ "type": "image_thumbnails", "blobIds": [] }),
            "invalid_blob_ids",
        ),
        (
            json!({ "type": "image_thumbnails", "blobIds": (1..=17).collect::<Vec<_>>() }),
            "invalid_blob_ids",
        ),
        (
            json!({ "type": "image_thumbnails", "blobIds": [0] }),
            "invalid_blob_id",
        ),
        (
            json!({ "type": "image_thumbnails", "blobIds": ["1"] }),
            "invalid_blob_id",
        ),
        (
            json!({ "type": "image_list", "limit": 61 }),
            "invalid_limit",
        ),
        (
            json!({ "type": "image_list", "cursor": { "id": 1 } }),
            "invalid_cursor",
        ),
        (json!({ "type": "image_sources" }), "invalid_blob_id"),
    ] {
        let reply = ask(&mut f, message.clone());
        assert_eq!(
            (reply["ok"].clone(), reply["error"].clone()),
            (json!(false), json!(code)),
            "{message}"
        );
    }
    let no_id = handle(&mut f.session, &json!({ "type": "image_list" }));
    assert_eq!(no_id["error"], "invalid_request_id");

    let empty = TempDir::new();
    let mut session = Session::new(empty.env());
    let reply = handle(
        &mut session,
        &json!({ "type": "image_list", "requestId": "r" }),
    );
    assert_eq!(reply["error"], "vault_not_configured");
    drop(f.tmp);
}
