//! One vault per read answer: every reply that names a vault (`vaultToken`) names the vault its
//! data was read from, also when another process switches the active vault while the request is
//! answered. Drives the real handlers of image_query, image_tags, vault_items, image_original and
//! the image save protocol, with the switch placed deterministically by `Session::after_open`
//! (right after a resolution opened its vault: the window in which a `ticket()` followed by a
//! `read()` paired one vault's token with the other's data).

use crate::dialog::Dialogs;
use crate::image_protocol::Uploads;
use crate::images::tests::{configure, png};
use crate::images::{self, sha256_hex, SaveRequest, ThumbnailInput};
use crate::vault::tests::TempDir;
use crate::vault::{create_vault, open_vault, vault_token, Environment, Session};
use crate::{image_original, image_query, image_tags, vault_items};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

const URL: &str = "https://x.test/";
/// Save actions (`crypto.randomUUID()` form): `S1` is saved in both vaults, `S0` in B only.
const S0: &str = "00000000-0000-4000-8000-000000000000";
const S1: &str = "11111111-1111-4111-8111-111111111111";

/// Two vaults holding the same image P (blob 1 in both) and the same URL, with different data:
///
/// | | A | B |
/// |---|---|---|
/// | saves of P | `S1` at 1 000 (page a) | `S0` at 2 000 (page b), `S1` at 2 500 (page a) |
/// | thumbnail | 2 px wide | 4 px wide |
/// | tags of P | `alpha`, `shared` | `beta`, `shared` |
/// | visits of `URL` | 1 | 2 |
///
/// So every read below answers differently in A and in B; A is active at the start.
struct Two {
    tmp: TempDir,
    env: Environment,
    a: PathBuf,
    b: PathBuf,
}

fn image() -> Vec<u8> {
    png(3, 1)
}

fn request(save_id: &str, page: &str) -> SaveRequest {
    let bytes = image();
    SaveRequest {
        save_id: save_id.to_owned(),
        sha256: sha256_hex(&bytes),
        byte_length: bytes.len() as u64,
        mime_type: "image/png".into(),
        width: 3,
        height: 1,
        image_url: "https://img.test/p.png".into(),
        page_url: Some(page.to_owned()),
        frame_url: None,
        incognito: false,
    }
}

fn thumbnail(width: u32) -> ThumbnailInput {
    ThumbnailInput {
        mime_type: "image/png".into(),
        width,
        height: 1,
        generator: 1,
        data: png(width, 1),
    }
}

fn fill(env: &Environment, path: &Path, saves: &[(&str, &str, i64)], thumb: u32, tags: &[&str]) {
    create_vault(path).unwrap();
    configure(env, path);
    let mut session = Session::new(env.clone());
    let ticket = session.ticket().unwrap();
    for (index, (save_id, page, at)) in saves.iter().enumerate() {
        let bytes = image();
        let thumbnail = (index == 0).then(|| thumbnail(thumb));
        let outcome = images::save_image(
            &mut session,
            &ticket,
            &request(save_id, page),
            Some(&bytes),
            thumbnail.as_ref(),
            *at,
        )
        .unwrap();
        assert_eq!(outcome.blob_id, 1);
    }
    for tag in tags {
        let added = image_tags::handle(
            &mut session,
            &json!({ "type": "image_tag_add", "requestId": "t", "blobId": 1, "sha256": sha(),
                     "vaultToken": ticket.token, "tag": tag }),
        );
        assert_eq!(added["ok"], true, "{added}");
    }
    let db = open_vault(path).unwrap();
    // One visit of `URL` per save: one in A, two in B.
    for visit in 0..saves.len() {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, 't', '2026-11-03T01:00:00.000Z')",
            [format!("v{visit}"), URL.to_owned()],
        )
        .unwrap();
    }
}

fn two() -> Two {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
    fill(
        &env,
        &a,
        &[(S1, "https://a.test/", 1_000)],
        2,
        &["alpha", "shared"],
    );
    fill(
        &env,
        &b,
        &[
            (S0, "https://b.test/", 2_000),
            (S1, "https://a.test/", 2_500),
        ],
        4,
        &["beta", "shared"],
    );
    configure(&env, &a);
    Two { tmp, env, a, b }
}

fn sha() -> String {
    sha256_hex(&image())
}

fn token(path: &Path) -> String {
    vault_token(path).unwrap()
}

/// A session whose first resolution of the active vault is followed at once by a switch to `to`
/// (as if another host process switched it): the request in flight must stay wholly in the vault
/// it resolved, and the next request is wholly `to`'s.
fn switching(t: &Two, to: &Path) -> Session {
    let mut session = Session::new(t.env.clone());
    let (env, to) = (t.env.clone(), to.to_owned());
    let mut done = false;
    session.after_open = Some(Box::new(move || {
        if !done {
            done = true;
            configure(&env, &to);
        }
    }));
    session
}

/// Every read handler that names its vault, with one request each.
#[derive(Clone, Copy, Debug)]
enum Read {
    ImageList,
    ImageThumbnails,
    ImageSources,
    ImageTagsGet,
    ImageTagList,
    ImageTagSearch,
    UrlDeletePreview,
    ImageDeletePreview,
}

const READS: [Read; 8] = [
    Read::ImageList,
    Read::ImageThumbnails,
    Read::ImageSources,
    Read::ImageTagsGet,
    Read::ImageTagList,
    Read::ImageTagSearch,
    Read::UrlDeletePreview,
    Read::ImageDeletePreview,
];

fn ask(session: &mut Session, read: Read) -> Value {
    let message = match read {
        Read::ImageList => json!({ "type": "image_list" }),
        Read::ImageThumbnails => json!({ "type": "image_thumbnails", "blobIds": [1] }),
        Read::ImageSources => json!({ "type": "image_sources", "blobId": 1 }),
        Read::ImageTagsGet => json!({ "type": "image_tags_get", "blobId": 1 }),
        Read::ImageTagList => json!({ "type": "image_tag_list" }),
        Read::ImageTagSearch => json!({ "type": "image_tag_search", "tag": "shared" }),
        Read::UrlDeletePreview => json!({ "type": "url_delete_preview", "url": URL }),
        Read::ImageDeletePreview => {
            json!({ "type": "image_delete_preview", "blobId": 1, "sha256": sha() })
        }
    };
    let mut message = message;
    message["requestId"] = "r".into();
    match read {
        Read::ImageList | Read::ImageThumbnails | Read::ImageSources => {
            image_query::handle(session, &message)
        }
        Read::ImageTagsGet | Read::ImageTagList | Read::ImageTagSearch => {
            image_tags::handle(session, &message)
        }
        Read::UrlDeletePreview | Read::ImageDeletePreview => vault_items::handle(session, &message),
    }
}

/// The answer of `read` in `vault`, asked with nothing switching (the reference).
fn plain(t: &Two, vault: &Path, read: Read) -> Value {
    configure(&t.env, vault);
    let answer = ask(&mut Session::new(t.env.clone()), read);
    assert_eq!(answer["ok"], true, "{read:?}: {answer}");
    assert_eq!(answer["vaultToken"], token(vault), "{read:?}");
    answer
}

fn without_token(answer: &Value) -> Value {
    let mut answer = answer.clone();
    answer.as_object_mut().unwrap().remove("vaultToken");
    answer
}

#[test]
fn the_fixture_tells_the_vaults_apart_in_every_read() {
    // Otherwise the tests below could not see data of one vault under the other's token.
    let t = two();
    for read in READS {
        let (a, b) = (plain(&t, &t.a, read), plain(&t, &t.b, read));
        assert_ne!(without_token(&a), without_token(&b), "{read:?}");
    }
}

#[test]
fn a_switch_while_a_read_is_answered_never_mixes_vaults() {
    // The invariant: `vaultToken` is the token of the vault the answer's data was read from.
    let t = two();
    for read in READS {
        let (a, b) = (plain(&t, &t.a, read), plain(&t, &t.b, read));
        for (from, to, first, second) in [(&t.a, &t.b, &a, &b), (&t.b, &t.a, &b, &a)] {
            configure(&t.env, from);
            let mut session = switching(&t, to);
            // The request in flight: wholly the vault it resolved (its data and its token).
            assert_eq!(&ask(&mut session, read), first, "{read:?} in flight");
            // The switch counts from the next request on: wholly the new vault.
            assert_eq!(&ask(&mut session, read), second, "{read:?} next");
        }
    }
}

#[test]
fn a_read_that_fails_after_the_vault_was_opened_answers_no_token_and_no_data() {
    for read in READS {
        let t = two();
        let mut session = Session::new(t.env.clone());
        let a = t.a.clone();
        let mut done = false;
        // Another connection removes what the read needs, once, after this one opened the vault.
        session.after_open = Some(Box::new(move || {
            if std::mem::replace(&mut done, true) {
                return;
            }
            open_vault(&a)
                .unwrap()
                .execute_batch(
                    "PRAGMA foreign_keys = OFF;
                     DROP TABLE image_tag_links; DROP TABLE image_tags; DROP TABLE image_thumbnails;
                     DROP TABLE saved_images; DROP TABLE image_blobs;
                     DROP TABLE navigation_events; DROP TABLE page_metadata; DROP TABLE visits;",
                )
                .unwrap();
        }));
        let answer = ask(&mut session, read);
        assert_eq!(answer["ok"], false, "{read:?}: {answer}");
        assert_eq!(answer["error"], "db_error", "{read:?}");
        let fields: Vec<&str> = answer
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            ["error", "ok", "requestId", "type"],
            "{read:?}: no token, no data"
        );
    }
}

#[test]
fn no_vault_or_a_missing_one_is_the_error_it_was() {
    for read in READS {
        let tmp = TempDir::new();
        let answer = ask(&mut Session::new(tmp.env()), read);
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["error"], "vault_not_configured", "{read:?}");
        assert!(answer.get("vaultToken").is_none());

        let t = two();
        std::fs::remove_file(&t.a).unwrap();
        let answer = ask(&mut Session::new(t.env.clone()), read);
        assert_eq!(answer["error"], "vault_missing", "{read:?}");
        assert!(answer.get("vaultToken").is_none());
    }
}

#[test]
fn a_delete_confirmed_on_a_preview_never_lands_in_the_vault_switched_to() {
    // The preview answered in flight names A (with A's counts); B is active by then. A delete that
    // names that preview's token is refused, and neither vault loses anything.
    let t = two();
    let mut session = switching(&t, &t.b);
    let preview = ask(&mut session, Read::UrlDeletePreview);
    assert_eq!(preview["vaultToken"], token(&t.a));
    assert_eq!(preview["counts"]["visits"], 1, "A's count: {preview}");
    let deleted = vault_items::handle(
        &mut session,
        &json!({ "type": "url_delete", "requestId": "d", "url": URL, "vaultToken": preview["vaultToken"] }),
    );
    assert_eq!(deleted["ok"], false);
    assert_eq!(deleted["error"], "vault_switched");

    configure(&t.env, &t.a);
    let mut session = switching(&t, &t.b);
    let preview = ask(&mut session, Read::ImageDeletePreview);
    assert_eq!(preview["vaultToken"], token(&t.a));
    let deleted = vault_items::handle(
        &mut session,
        &json!({ "type": "image_delete", "requestId": "d", "blobId": 1, "sha256": sha(),
                 "vaultToken": preview["vaultToken"] }),
    );
    assert_eq!(deleted["error"], "vault_switched");

    for (vault, visits, saves) in [(&t.a, 1, 1), (&t.b, 2, 2)] {
        let db = open_vault(vault).unwrap();
        let count = |table: &str| -> i64 {
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        assert_eq!((count("visits"), count("saved_images")), (visits, saves));
    }
}

struct ExportTo {
    path: PathBuf,
    asked: RefCell<Vec<String>>,
}

impl Dialogs for ExportTo {
    fn choose_new_vault(&self, _: &Path, _: &str) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_existing_vault(&self, _: Option<&Path>) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_backup_dir(&self) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_export_file(&self, file_name: &str, _: &str) -> Option<PathBuf> {
        self.asked.borrow_mut().push(file_name.to_owned());
        Some(self.path.clone())
    }
}

#[test]
fn image_read_and_export_read_the_vault_they_name_or_refuse() {
    let t = two();
    let dialogs = ExportTo {
        path: t.tmp.0.join("out.png"),
        asked: RefCell::default(),
    };
    let read = |token: &str| {
        json!({ "type": "image_read", "requestId": "r", "blobId": 1, "sha256": sha(),
                "vaultToken": token, "offset": 0 })
    };
    let export = |token: &str| {
        json!({ "type": "image_export", "requestId": "e", "blobId": 1, "sha256": sha(),
                "vaultToken": token })
    };
    // Named A, switched to B in the window: A's bytes under A's token.
    let mut session = switching(&t, &t.b);
    let answer = image_original::handle(&mut session, &dialogs, &read(&token(&t.a)));
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["vaultToken"], token(&t.a));
    // Now B is active: a request that names A is refused, one that names B is B's.
    let answer = image_original::handle(&mut session, &dialogs, &read(&token(&t.a)));
    assert_eq!(answer["error"], "vault_switched");
    let answer = image_original::handle(&mut session, &dialogs, &read(&token(&t.b)));
    assert_eq!(answer["vaultToken"], token(&t.b));

    // The export's file name is the last save time of the vault it read: A's (1 s), then B's
    // (2.5 s), never B's under A's token.
    configure(&t.env, &t.a);
    let mut session = switching(&t, &t.b);
    let answer = image_original::handle(&mut session, &dialogs, &export(&token(&t.a)));
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["vaultToken"], token(&t.a));
    let answer = image_original::handle(&mut session, &dialogs, &export(&token(&t.a)));
    assert_eq!(answer["error"], "vault_switched");
    let answer = image_original::handle(&mut session, &dialogs, &export(&token(&t.b)));
    assert_eq!(answer["vaultToken"], token(&t.b), "{answer}");
    assert_eq!(*dialogs.asked.borrow(), ["1.png", "2.png"]);
}

#[test]
fn a_save_begin_never_answers_another_vaults_save_under_its_token() {
    // `S1` is saved in both vaults with different outcomes (saved image 1 in A, 2 in B). A resend
    // of it answers the stored outcome; that outcome must be of the vault its token names.
    let t = two();
    let bytes = image();
    let begin = json!({
        "type": "image_save_begin", "protocol": 2, "saveId": S1,
        "sha256": sha(), "byteLength": bytes.len(), "mimeType": "image/png", "width": 3, "height": 1,
        "imageUrl": "https://img.test/p.png", "pageUrl": "https://a.test/", "frameUrl": null,
        "incognito": false,
    });
    for (from, to, then) in [(&t.a, &t.b, 2), (&t.b, &t.a, 1)] {
        configure(&t.env, from);
        let mut session = switching(&t, to);
        let mut uploads = Uploads::default();
        // The ticket was taken in `from`, the lookup found `to` active: refused, nothing answered.
        let answer = uploads.handle(&mut session, &begin);
        assert_eq!(answer["ok"], false, "{answer}");
        assert_eq!(answer["error"], "vault_switched");
        assert!(answer.get("vaultToken").is_none() && answer.get("savedImageId").is_none());
        // The resend: wholly the vault active now.
        let answer = uploads.handle(&mut session, &begin);
        assert_eq!(answer["type"], "image_saved", "{answer}");
        assert_eq!(answer["vaultToken"], token(to));
        assert_eq!(answer["savedImageId"], then);
    }
}
