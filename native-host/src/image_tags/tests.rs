use super::*;
use crate::images::tests::{configure, png};
use crate::images::{save_image, sha256_hex, SaveRequest};
use crate::vault::tests::TempDir;
use crate::vault::{create_vault, open_vault, VaultTicket};
use std::path::PathBuf;

struct Fixture {
    tmp: TempDir,
    session: Session,
    ticket: VaultTicket,
    vault: PathBuf,
    /// (blob id, sha256) of the saved images, in save order.
    images: Vec<(i64, String)>,
}

/// A fresh vault with `count` distinct images, image `i` saved at time `i + 1` (so the newest is
/// the last one), each from a normal save.
fn fixture(count: u32) -> Fixture {
    let tmp = TempDir::new();
    let env = tmp.env();
    let vault = tmp.0.join("v.sqlite3");
    create_vault(&vault).unwrap();
    configure(&env, &vault);
    let mut session = Session::new(env);
    let ticket = session.ticket().unwrap();
    let mut images = Vec::new();
    for i in 0..count {
        let bytes = png(i + 1, 1);
        let request = SaveRequest {
            save_id: format!("save-{i}"),
            sha256: sha256_hex(&bytes),
            byte_length: bytes.len() as u64,
            mime_type: "image/png".to_owned(),
            width: i + 1,
            height: 1,
            image_url: format!("https://img.test/{i}.png"),
            page_url: Some(format!("https://page.test/{i}")),
            frame_url: None,
            incognito: false,
        };
        let outcome = save_image(
            &mut session,
            &ticket,
            &request,
            Some(&bytes),
            None,
            i64::from(i) + 1,
        )
        .unwrap();
        images.push((outcome.blob_id, request.sha256));
    }
    Fixture {
        tmp,
        session,
        ticket,
        vault,
        images,
    }
}

impl Fixture {
    fn add(&mut self, image: usize, raw: &str) -> Result<bool, ImageError> {
        let (blob_id, sha) = self.images[image].clone();
        let name = normalize_tag(raw)?;
        let ticket = self.ticket.clone();
        self.session
            .write_for(&ticket, |db| add_tag(db, blob_id, &sha, &name))
    }

    fn remove(&mut self, image: usize, raw: &str) -> Result<bool, ImageError> {
        let (blob_id, sha) = self.images[image].clone();
        let name = normalize_tag(raw)?;
        let ticket = self.ticket.clone();
        self.session
            .write_for(&ticket, |db| remove_tag(db, blob_id, &sha, &name))
    }

    fn tags(&mut self, image: usize) -> Vec<String> {
        let blob_id = self.images[image].0;
        self.session
            .read(|db| tags_of(db, blob_id))
            .unwrap()
            .unwrap()
    }

    fn catalog(&mut self) -> Vec<(String, i64)> {
        self.session
            .read(|db| list_tags(db, None, MAX_TAG_PAGE))
            .unwrap()
            .unwrap()
            .0
            .into_iter()
            .map(|tag| (tag.name, tag.image_count))
            .collect()
    }

    fn found(&mut self, name: &str) -> Vec<i64> {
        self.session
            .read(|db| search_by_tag(db, name, None, MAX_PAGE_SIZE))
            .unwrap()
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.blob_id)
            .collect()
    }

    fn message(&mut self, value: Value) -> Value {
        handle(&mut self.session, &value)
    }

    fn token(&self) -> String {
        self.ticket.token.clone()
    }

    fn scalar(&self, sql: &str) -> i64 {
        open_vault(&self.vault)
            .unwrap()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }
}

fn names(tags: &[&str]) -> Vec<String> {
    tags.iter().map(|tag| (*tag).to_owned()).collect()
}

// --- Normalization ------------------------------------------------------------------------------

#[test]
fn tags_are_trimmed_spaced_once_and_lowercased() {
    for (raw, expected) in [
        ("cat", "cat"),
        ("  Cat  ", "cat"),
        ("CAT", "cat"),
        ("Reaction   Face", "reaction face"),
        ("\tline\nbreak\r\n", "line break"),
        ("\u{3000}猫\u{3000}耳\u{3000}", "猫 耳"),
        ("ÄÖÜ", "äöü"),
        ("ＡＢＣ", "ａｂｃ"),
        ("ねこ", "ねこ"),
        ("ネコ", "ネコ"),
        ("😺", "😺"),
        ("a/b:c#d", "a/b:c#d"),
    ] {
        assert_eq!(normalize_tag(raw).as_deref(), Ok(expected), "{raw:?}");
        // Normalizing twice changes nothing.
        assert_eq!(
            normalize_tag(expected).as_deref(),
            Ok(expected),
            "{expected:?}"
        );
    }
    // No Unicode normalization: full-width and half-width forms stay different tags.
    assert_ne!(normalize_tag("ＡＢＣ"), normalize_tag("abc"));
}

#[test]
fn empty_overlong_control_and_invisible_tags_are_refused() {
    let longest = "あ".repeat(MAX_TAG_CHARS);
    assert_eq!(normalize_tag(&longest).as_deref(), Ok(longest.as_str()));
    assert_eq!(
        normalize_tag(&format!("  {longest}  ")).as_deref(),
        Ok(longest.as_str()),
        "spacing does not count"
    );
    for raw in [
        String::new(),
        " ".to_owned(),
        "\u{3000}\t\n".to_owned(),
        "あ".repeat(MAX_TAG_CHARS + 1),
        "a".repeat(MAX_TAG_INPUT_BYTES + 1),
        format!("{}{}", " ".repeat(MAX_TAG_INPUT_BYTES), "a"),
        "a\u{0000}b".to_owned(),
        "a\u{0007}b".to_owned(),
        "a\u{007F}b".to_owned(),
        "a\u{0085}".to_owned() + "\u{0001}",
        "a\u{009F}b".to_owned(),
        "a\u{200B}b".to_owned(),
        "a\u{200D}b".to_owned(),
        "a\u{202E}b".to_owned(),
        "a\u{2066}b".to_owned(),
        "\u{FEFF}a".to_owned(),
        "a\u{00AD}b".to_owned(),
    ] {
        assert_eq!(normalize_tag(&raw), Err(ImageError::InvalidTag), "{raw:?}");
    }
    // A lowercase form longer than the limit is refused even if the input fits ("İ" → "i̇").
    let dotted = "İ".repeat(MAX_TAG_CHARS);
    assert_eq!(dotted.chars().count(), MAX_TAG_CHARS);
    assert_eq!(normalize_tag(&dotted), Err(ImageError::InvalidTag));
}

#[test]
fn every_normalized_tag_passes_the_vaults_own_check() {
    let mut f = fixture(1);
    for raw in [
        "Cat",
        "  猫  耳 ",
        "ÄÖÜ",
        "ＡＢＣ",
        &"x".repeat(MAX_TAG_CHARS),
    ] {
        assert_eq!(f.add(0, raw), Ok(true), "{raw:?}");
    }
    assert_eq!(f.scalar("SELECT count(*) FROM image_tags"), 5);
}

// --- Writing and reading ------------------------------------------------------------------------

#[test]
fn adding_is_idempotent_whatever_the_spelling_and_tags_are_listed_by_name() {
    let mut f = fixture(2);
    assert_eq!(f.add(0, "reaction"), Ok(true));
    assert_eq!(f.add(0, "Reaction"), Ok(false));
    assert_eq!(f.add(0, "  REACTION "), Ok(false));
    assert_eq!(f.add(0, "cat"), Ok(true));
    assert_eq!(f.add(0, "漫画"), Ok(true));
    assert_eq!(f.add(1, "cat"), Ok(true));
    assert_eq!(f.tags(0), names(&["cat", "reaction", "漫画"]));
    assert_eq!(f.tags(1), names(&["cat"]));
    assert_eq!(f.scalar("SELECT count(*) FROM image_tag_links"), 4);
    assert_eq!(
        f.scalar("SELECT count(*) FROM image_tags"),
        3,
        "one row per tag"
    );
    assert_eq!(
        f.catalog(),
        [
            ("cat".to_owned(), 2),
            ("reaction".to_owned(), 1),
            ("漫画".to_owned(), 1)
        ]
    );
}

#[test]
fn removing_takes_one_link_is_safe_when_absent_and_drops_a_tag_no_image_has() {
    let mut f = fixture(2);
    f.add(0, "cat").unwrap();
    f.add(1, "cat").unwrap();
    f.add(0, "dog").unwrap();
    assert_eq!(f.remove(0, "never-given"), Ok(false));
    assert_eq!(f.remove(1, "dog"), Ok(false), "another image's tag");
    assert_eq!(f.tags(0), names(&["cat", "dog"]));
    assert_eq!(f.remove(0, "CAT"), Ok(true));
    assert_eq!(f.remove(0, "cat"), Ok(false), "already removed");
    assert_eq!(f.tags(0), names(&["dog"]));
    assert_eq!(f.catalog(), [("cat".to_owned(), 1), ("dog".to_owned(), 1)]);
    assert_eq!(f.remove(0, "dog"), Ok(true));
    assert_eq!(
        f.catalog(),
        [("cat".to_owned(), 1)],
        "dog is gone with its last link"
    );
    assert_eq!(f.remove(1, "cat"), Ok(true));
    assert!(f.catalog().is_empty());
    assert_eq!(f.scalar("SELECT count(*) FROM image_tags"), 0);
    // Given again, it comes back as a new tag.
    assert_eq!(f.add(1, "cat"), Ok(true));
    assert_eq!(f.catalog(), [("cat".to_owned(), 1)]);
}

#[test]
fn an_image_that_is_unknown_or_now_other_bytes_is_missing_and_nothing_is_written() {
    let mut f = fixture(1);
    let (blob_id, sha) = f.images[0].clone();
    let ticket = f.ticket.clone();
    let other = "f".repeat(64);
    for (id, hash) in [(blob_id + 100, sha.as_str()), (blob_id, other.as_str())] {
        assert_eq!(
            f.session
                .write_for(&ticket, |db| add_tag(db, id, hash, "cat")),
            Err(ImageError::BlobMissing)
        );
        assert_eq!(
            f.session
                .write_for(&ticket, |db| remove_tag(db, id, hash, "cat")),
            Err(ImageError::BlobMissing)
        );
    }
    assert_eq!(
        f.session.read(|db| tags_of(db, blob_id + 100)).unwrap(),
        Err(ImageError::BlobMissing)
    );
    assert_eq!(f.scalar("SELECT count(*) FROM image_tags"), 0);
}

#[test]
fn an_image_has_at_most_the_limit_of_tags_and_a_refused_one_leaves_no_trace() {
    let mut f = fixture(2);
    for i in 0..MAX_TAGS_PER_IMAGE {
        assert_eq!(f.add(0, &format!("t{i:02}")), Ok(true));
    }
    assert_eq!(f.add(0, "one-more"), Err(ImageError::TooManyTags));
    assert_eq!(f.add(0, "T00"), Ok(false), "one it has is still fine");
    assert_eq!(f.tags(0).len(), MAX_TAGS_PER_IMAGE);
    assert_eq!(
        f.scalar("SELECT count(*) FROM image_tags WHERE name = 'one-more'"),
        0,
        "the new tag row was rolled back"
    );
    assert_eq!(f.add(1, "one-more"), Ok(true), "the limit is per image");
    f.remove(0, "t00").unwrap();
    assert_eq!(f.add(0, "one-more"), Ok(true));
}

#[test]
fn search_finds_one_tags_images_newest_first_in_list_form_and_pages_stably() {
    let mut f = fixture(5);
    for image in [0, 2, 3, 4] {
        f.add(image, "cat").unwrap();
    }
    f.add(1, "dog").unwrap();
    let ids = |f: &Fixture, order: &[usize]| -> Vec<i64> {
        order.iter().map(|i| f.images[*i].0).collect()
    };
    assert_eq!(f.found("cat"), ids(&f, &[4, 3, 2, 0]));
    assert_eq!(f.found("dog"), ids(&f, &[1]));
    assert!(f.found("unknown").is_empty());
    // Items are exactly what image_list gives for the same images.
    let listed = f
        .session
        .read(|db| images::list_images(db, None, MAX_PAGE_SIZE))
        .unwrap()
        .unwrap()
        .items;
    let found = f
        .session
        .read(|db| search_by_tag(db, "cat", None, MAX_PAGE_SIZE))
        .unwrap()
        .unwrap()
        .items;
    for item in &found {
        assert!(listed.contains(item), "{item:?}");
    }
    assert_eq!(found[0].latest_image_url, "https://img.test/4.png");
    // Pages of two, resumed at the cursor; a tag given meanwhile to an older image is not skipped.
    let first = f
        .session
        .read(|db| search_by_tag(db, "cat", None, 2))
        .unwrap()
        .unwrap();
    assert_eq!(
        first.items.iter().map(|i| i.blob_id).collect::<Vec<_>>(),
        ids(&f, &[4, 3])
    );
    f.add(1, "cat").unwrap();
    let rest = f
        .session
        .read(|db| search_by_tag(db, "cat", first.next, 2))
        .unwrap()
        .unwrap();
    assert_eq!(
        rest.items.iter().map(|i| i.blob_id).collect::<Vec<_>>(),
        ids(&f, &[2, 1])
    );
    let last = f
        .session
        .read(|db| search_by_tag(db, "cat", rest.next, 2))
        .unwrap()
        .unwrap();
    assert_eq!(
        last.items.iter().map(|i| i.blob_id).collect::<Vec<_>>(),
        ids(&f, &[0])
    );
    assert!(last.next.is_none());
}

#[test]
fn the_tag_list_counts_images_and_pages_by_name() {
    let mut f = fixture(3);
    for (image, tag) in [(0, "b"), (1, "b"), (2, "b"), (0, "a"), (1, "c"), (2, "d")] {
        f.add(image, tag).unwrap();
    }
    let page = |f: &mut Fixture, after: Option<&str>| {
        let after = after.map(str::to_owned);
        f.session
            .read(|db| list_tags(db, after.as_deref(), 2))
            .unwrap()
            .unwrap()
    };
    let (items, next) = page(&mut f, None);
    assert_eq!(
        items,
        [
            TagCount {
                name: "a".into(),
                image_count: 1
            },
            TagCount {
                name: "b".into(),
                image_count: 3
            }
        ]
    );
    assert_eq!(next.as_deref(), Some("b"));
    let (items, next) = page(&mut f, next.as_deref());
    assert_eq!(
        items.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["c", "d"]
    );
    assert_eq!(next, None);
}

#[test]
fn a_removed_image_takes_its_tags_and_leaves_a_consistent_vault() {
    // The host has no command that removes an image; this is the vault's own guarantee for one.
    let mut f = fixture(2);
    f.add(0, "only-here").unwrap();
    f.add(0, "shared").unwrap();
    f.add(1, "shared").unwrap();
    let blob = f.images[0].0;
    let db = open_vault(&f.vault).unwrap();
    db.execute_batch(&format!(
        "BEGIN; DELETE FROM saved_images WHERE blob_id = {blob}; DELETE FROM image_blobs WHERE id = {blob}; COMMIT;"
    ))
    .unwrap();
    drop(db);
    assert_eq!(f.catalog(), [("shared".to_owned(), 1)]);
    assert_eq!(f.tags(1), names(&["shared"]));
    assert_eq!(f.scalar("SELECT count(*) FROM image_tag_links"), 1);
    assert_eq!(f.scalar("SELECT count(*) FROM pragma_foreign_key_check"), 0);
    let integrity: String = open_vault(&f.vault)
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    // An image with tags cannot lose its bytes while save history still names them.
    let db = open_vault(&f.vault).unwrap();
    assert!(db
        .execute("DELETE FROM image_blobs WHERE id = ?1", [f.images[1].0])
        .is_err());
}

#[test]
fn tags_change_nothing_else_about_an_image() {
    let mut f = fixture(2);
    let snapshot = |f: &Fixture| -> String {
        let db = open_vault(&f.vault).unwrap();
        let mut statement = db
            .prepare("SELECT b.id || b.sha256 || b.last_saved_at || b.save_count || s.mcp_public || s.incognito || s.saved_at FROM image_blobs b JOIN saved_images s ON s.blob_id = b.id ORDER BY b.id")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows.join("|")
    };
    let before = snapshot(&f);
    let listed_before = f
        .session
        .read(|db| images::list_images(db, None, MAX_PAGE_SIZE))
        .unwrap()
        .unwrap();
    f.add(0, "cat").unwrap();
    f.add(1, "dog").unwrap();
    f.remove(1, "dog").unwrap();
    assert_eq!(snapshot(&f), before);
    let listed_after = f
        .session
        .read(|db| images::list_images(db, None, MAX_PAGE_SIZE))
        .unwrap()
        .unwrap();
    assert_eq!(listed_after, listed_before, "the list order is not touched");
}

#[test]
fn backups_carry_tags_and_check_their_counts() {
    let mut f = fixture(2);
    f.add(0, "cat").unwrap();
    f.add(1, "cat").unwrap();
    f.add(1, "猫").unwrap();
    let dest = f.tmp.0.join("backups");
    std::fs::create_dir(&dest).unwrap();
    let db = open_vault(&f.vault).unwrap();
    let backup = crate::vault::backup_vault(&db, &dest, std::time::SystemTime::now()).unwrap();
    let copy = open_vault(&backup).unwrap();
    let tags: Vec<(String, i64)> = copy
        .prepare("SELECT t.name, l.blob_id FROM image_tag_links l JOIN image_tags t ON t.id = l.tag_id ORDER BY 1, 2")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        tags,
        [
            ("cat".to_owned(), f.images[0].0),
            ("cat".to_owned(), f.images[1].0),
            ("猫".to_owned(), f.images[1].0)
        ]
    );
}

// --- Messages -----------------------------------------------------------------------------------

fn write_message(f: &Fixture, kind: &str, image: usize, tag: &str) -> Value {
    json!({
        "type": kind, "requestId": "r", "blobId": f.images[image].0,
        "sha256": f.images[image].1, "vaultToken": f.token(), "tag": tag,
    })
}

#[test]
fn messages_add_remove_and_read_tags_with_the_stated_replies() {
    let mut f = fixture(2);
    let blob = f.images[0].0;
    let token = f.token();
    let add = write_message(&f, "image_tag_add", 0, "  Reaction  ");
    let reply = f.message(add.clone());
    assert_eq!(
        reply,
        json!({ "ok": true, "type": "image_tag_add_result", "requestId": "r", "vaultToken": token,
                "blobId": blob, "tag": "reaction", "added": true, "tags": ["reaction"] })
    );
    assert_eq!(f.message(add)["added"], false);
    f.message(write_message(&f, "image_tag_add", 0, "Cat"));
    let reply = f.message(json!({ "type": "image_tags_get", "requestId": "g", "blobId": blob }));
    assert_eq!(
        reply,
        json!({ "ok": true, "type": "image_tags_get_result", "requestId": "g", "vaultToken": token,
                "blobId": blob, "tags": ["cat", "reaction"] })
    );
    let reply = f.message(json!({ "type": "image_tag_list", "requestId": "l" }));
    assert_eq!(
        reply,
        json!({ "ok": true, "type": "image_tag_list_result", "requestId": "l", "vaultToken": token,
                "items": [{ "name": "cat", "imageCount": 1 }, { "name": "reaction", "imageCount": 1 }],
                "next": null })
    );
    let reply = f.message(json!({ "type": "image_tag_list", "requestId": "l", "limit": 1 }));
    assert_eq!(reply["next"], "cat");
    let reply = f.message(
        json!({ "type": "image_tag_list", "requestId": "l", "cursor": "cat", "limit": 1 }),
    );
    assert_eq!(
        reply["items"],
        json!([{ "name": "reaction", "imageCount": 1 }])
    );
    let reply = f.message(json!({ "type": "image_tag_search", "requestId": "s", "tag": "CAT" }));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["type"], "image_tag_search_result");
    assert_eq!(reply["tag"], "cat");
    assert_eq!(reply["vaultToken"], token);
    assert_eq!(reply["items"].as_array().unwrap().len(), 1);
    assert_eq!(reply["items"][0]["blobId"], blob);
    assert_eq!(reply["items"][0]["sha256"], f.images[0].1);
    assert_eq!(reply["next"], Value::Null);
    let remove = write_message(&f, "image_tag_remove", 0, "CAT");
    let reply = f.message(remove.clone());
    assert_eq!(
        reply,
        json!({ "ok": true, "type": "image_tag_remove_result", "requestId": "r", "vaultToken": token,
                "blobId": blob, "tag": "cat", "removed": true, "tags": ["reaction"] })
    );
    assert_eq!(f.message(remove)["removed"], false);
}

#[test]
fn malformed_requests_are_refused_with_a_code_and_change_nothing() {
    let mut f = fixture(1);
    let good = write_message(&f, "image_tag_add", 0, "cat");
    let with = |key: &str, value: Value| {
        let mut message = good.clone();
        message[key] = value;
        message
    };
    let without = |key: &str| {
        let mut message = good.clone();
        message.as_object_mut().unwrap().remove(key);
        message
    };
    for kind in ["image_tag_add", "image_tag_remove"] {
        let cases = [
            (without("requestId"), "invalid_request_id"),
            (with("requestId", json!("")), "invalid_request_id"),
            (without("blobId"), "invalid_blob_id"),
            (with("blobId", json!(0)), "invalid_blob_id"),
            (with("blobId", json!("1")), "invalid_blob_id"),
            (without("sha256"), "invalid_hash"),
            (with("sha256", json!("ABC")), "invalid_hash"),
            (without("vaultToken"), "invalid_vault_token"),
            (with("vaultToken", json!("0".repeat(64))), "vault_switched"),
            (without("tag"), "invalid_tag"),
            (with("tag", json!(7)), "invalid_tag"),
            (with("tag", json!("   ")), "invalid_tag"),
            (with("tag", json!("a\u{200B}b")), "invalid_tag"),
            (
                with("tag", json!("x".repeat(MAX_TAG_CHARS + 1))),
                "invalid_tag",
            ),
            (with("blobId", json!(999)), "blob_missing"),
            (with("sha256", json!("e".repeat(64))), "blob_missing"),
        ];
        for (mut message, code) in cases {
            message["type"] = json!(kind);
            let reply = f.message(message.clone());
            assert_eq!(reply["ok"], false, "{message}");
            assert_eq!(reply["error"], code, "{message}");
            assert_eq!(reply["type"], format!("{kind}_result"));
            assert_eq!(reply.as_object().unwrap().len(), 4, "{reply}");
        }
    }
    for (message, code) in [
        (
            json!({ "type": "image_tags_get", "requestId": "g" }),
            "invalid_blob_id",
        ),
        (
            json!({ "type": "image_tags_get", "requestId": "g", "blobId": 999 }),
            "blob_missing",
        ),
        (
            json!({ "type": "image_tag_list", "requestId": "l", "limit": 0 }),
            "invalid_limit",
        ),
        (
            json!({ "type": "image_tag_list", "requestId": "l", "limit": 201 }),
            "invalid_limit",
        ),
        (
            json!({ "type": "image_tag_list", "requestId": "l", "cursor": 3 }),
            "invalid_cursor",
        ),
        (
            json!({ "type": "image_tag_list", "requestId": "l", "cursor": "Cat" }),
            "invalid_cursor",
        ),
        (
            json!({ "type": "image_tag_search", "requestId": "s" }),
            "invalid_tag",
        ),
        (
            json!({ "type": "image_tag_search", "requestId": "s", "tag": "" }),
            "invalid_tag",
        ),
        (
            json!({ "type": "image_tag_search", "requestId": "s", "tag": "a", "limit": 61 }),
            "invalid_limit",
        ),
        (
            json!({ "type": "image_tag_search", "requestId": "s", "tag": "a", "cursor": { "id": 1 } }),
            "invalid_cursor",
        ),
    ] {
        let reply = f.message(message.clone());
        assert_eq!(reply["ok"], false, "{message}");
        assert_eq!(reply["error"], code, "{message}");
    }
    assert_eq!(f.scalar("SELECT count(*) FROM image_tags"), 0);
    assert_eq!(f.scalar("SELECT count(*) FROM image_tag_links"), 0);
}

#[test]
fn a_write_for_a_vault_that_is_no_longer_active_lands_nowhere() {
    let mut f = fixture(1);
    let message = write_message(&f, "image_tag_add", 0, "cat");
    // Another vault becomes active: the request still names the first one.
    let other = f.tmp.0.join("other.sqlite3");
    create_vault(&other).unwrap();
    configure(&f.session.env.clone(), &other);
    let reply = f.message(message);
    assert_eq!(reply["error"], "vault_switched", "{reply}");
    assert_eq!(f.scalar("SELECT count(*) FROM image_tags"), 0);
    let other_tags: i64 = open_vault(&other)
        .unwrap()
        .query_row("SELECT count(*) FROM image_tags", [], |row| row.get(0))
        .unwrap();
    assert_eq!(other_tags, 0);
}
