use super::*;
use memocampus_native_host::schema;
use rusqlite::Connection;

const A: &str = "https://a.test/page?x=1";
const B: &str = "https://a.test/page?x=2";
const C: &str = "https://c.test/";
const T: &str = "2026-11-07T10:00:00.000Z";

fn vault() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(&schema::create_sql()).unwrap();
    db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db
}

fn visit(db: &Connection, id: &str, url: &str) {
    db.execute(
        "INSERT INTO visits VALUES (?1, ?2, 'title', ?3)",
        params![id, url, T],
    )
    .unwrap();
}

fn navigation(db: &Connection, target: &str, source: Option<&str>) {
    db.execute(
        "INSERT INTO navigation_events VALUES (?1, ?2, 'document', 'link', 0, 0, 0, 0, 0, ?3)",
        params![target, source, T],
    )
    .unwrap();
}

/// A: three visits (a1 -> a2 within A, a3), a snapshot, metadata. B and C: other URLs, linked to A.
fn urls() -> Connection {
    let db = vault();
    for (id, url) in [
        ("a1", A),
        ("a2", A),
        ("a3", A),
        ("b1", B),
        ("c1", C),
        ("c2", C),
    ] {
        visit(&db, id, url);
    }
    navigation(&db, "a2", Some("a1")); // inside A: removed with it
    navigation(&db, "b1", Some("a3")); // A -> B: kept, source cleared
    navigation(&db, "a3", Some("c1")); // C -> A: removed with A's visit
    navigation(&db, "c2", Some("c1")); // C -> C: untouched
    db.execute_batch(&format!(
        "INSERT INTO tab_snapshots VALUES ('s1', 1, 0, '{A}', 'A', 0, 0, '{T}');
         INSERT INTO tab_snapshots VALUES ('s2', 1, 1, '{C}', 'C', 0, 0, '{T}');
         INSERT INTO page_metadata (url, event_id, title, captured_at) VALUES ('{A}', 'a3', 'A', '{T}');
         INSERT INTO page_metadata (url, event_id, title, captured_at) VALUES ('{B}', 'b1', 'B', '{T}');"
    ))
    .unwrap();
    db
}

fn dump(db: &Connection) -> Vec<String> {
    let mut statement = db
        .prepare(
            "SELECT 'v ' || event_id || ' ' || url FROM visits
             UNION ALL SELECT 's ' || event_id || ' ' || url FROM tab_snapshots
             UNION ALL SELECT 'p ' || url || ' ' || event_id FROM page_metadata
             UNION ALL SELECT 'n ' || target_visit_id || ' <- ' || coalesce(source_visit_id, 'NULL') FROM navigation_events
             ORDER BY 1",
        )
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn integrity(db: &Connection) {
    let result: String = db
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(result, "ok");
    let broken: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
}

#[test]
fn deleting_a_url_removes_all_of_it_and_nothing_else() {
    let mut db = urls();
    db.execute(
        "INSERT INTO image_blobs VALUES (1, ?1, 'image/png', 3, 1, 1, 0, 0, 1, x'010203')",
        ["a".repeat(64)],
    )
    .unwrap();
    db.execute(
        "INSERT INTO saved_images VALUES (1, 's', 1, 'https://img.test/1.png', ?1, NULL, 0, 0, 1)",
        [A],
    )
    .unwrap();
    let expected = UrlCounts {
        visits: 3,
        tab_snapshots: 1,
        page_metadata: 1,
        navigations: 2,
        navigation_sources_cleared: 1,
        image_saves_kept: 1,
    };
    assert_eq!(url_counts(&db, A).unwrap(), expected, "the preview");
    let tx = db.transaction().unwrap();
    assert_eq!(delete_url(&tx, A).unwrap(), expected, "what was deleted");
    tx.commit().unwrap();
    assert_eq!(
        dump(&db),
        [
            "n b1 <- NULL".to_owned(),
            "n c2 <- c1".to_owned(),
            format!("p {B} b1"),
            format!("s s2 {C}"),
            format!("v b1 {B}"),
            format!("v c1 {C}"),
            format!("v c2 {C}"),
        ],
        "B (same page, other query) and C are untouched; B's navigation lost only its source"
    );
    let images: i64 = db
        .query_row(
            "SELECT count(*) FROM saved_images JOIN image_blobs b ON b.id = blob_id",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(images, 1, "an image saved from the page stays");
    assert_eq!(
        url_counts(&db, A).unwrap(),
        UrlCounts {
            image_saves_kept: 1,
            ..UrlCounts::default()
        }
    );
    integrity(&db);
    // Searching no longer finds it, and the remaining rows are still found.
    let page = crate::search::search_pages(
        &db,
        &crate::search::SearchRequest {
            query: "a.test".into(),
            limit: 50,
            offset: 0,
        },
    )
    .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|hit| hit.url.as_str())
            .collect::<Vec<_>>(),
        [B]
    );
    // Again: nothing left, a no-op.
    let tx = db.transaction().unwrap();
    assert_eq!(
        delete_url(&tx, A).unwrap(),
        UrlCounts {
            image_saves_kept: 1,
            ..UrlCounts::default()
        }
    );
}

#[test]
fn the_url_is_matched_exactly_never_as_a_pattern() {
    let mut db = vault();
    for (id, url) in [
        ("1", "https://a.test/%"),
        ("2", "https://a.test/x"),
        ("3", "https://a.test/_"),
        ("4", "HTTPS://A.TEST/x"),
        ("5", "https://a.test/x/"),
    ] {
        visit(&db, id, url);
    }
    let tx = db.transaction().unwrap();
    assert_eq!(delete_url(&tx, "https://a.test/%").unwrap().visits, 1);
    assert_eq!(delete_url(&tx, "https://a.test/x").unwrap().visits, 1);
    tx.commit().unwrap();
    assert_eq!(
        dump(&db),
        [
            "v 3 https://a.test/_",
            "v 4 HTTPS://A.TEST/x",
            "v 5 https://a.test/x/"
        ]
    );
}

#[test]
fn a_url_deletion_that_fails_part_way_leaves_everything() {
    let mut db = urls();
    let before = dump(&db);
    // The last statement fails: the visits and metadata deleted before it must come back.
    db.execute_batch(
        "CREATE TEMP TRIGGER fail BEFORE DELETE ON tab_snapshots BEGIN SELECT RAISE(ABORT, 'x'); END;",
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    assert!(delete_url(&tx, A).is_err());
    drop(tx);
    assert_eq!(dump(&db), before);
    integrity(&db);
}

fn image(db: &Connection, id: i64, sha: char, saves: i64, thumbnail: bool) {
    db.execute(
        "INSERT INTO image_blobs VALUES (?1, ?2, 'image/png', 3, 1, 1, 0, 0, ?3, x'010203')",
        params![id, sha.to_string().repeat(64), saves],
    )
    .unwrap();
    for n in 0..saves {
        db.execute(
            "INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public)
             VALUES (?1, ?2, 'https://img.test/x.png', 'https://page.test/', ?3, 0, 1)",
            params![format!("{id}-{n}"), id, n],
        )
        .unwrap();
    }
    if thumbnail {
        db.execute(
            "INSERT INTO image_thumbnails VALUES (?1, 'image/webp', 1, 1, 1, x'00')",
            [id],
        )
        .unwrap();
    }
}

fn tag(db: &Connection, blob: i64, name: &str) {
    crate::image_tags::add_tag(
        db,
        blob,
        &db.query_row(
            "SELECT sha256 FROM image_blobs WHERE id = ?1",
            [blob],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        name,
    )
    .unwrap();
}

fn image_dump(db: &Connection) -> Vec<String> {
    let mut statement = db
        .prepare(
            "SELECT 'b ' || id FROM image_blobs
             UNION ALL SELECT 's ' || save_id FROM saved_images
             UNION ALL SELECT 'th ' || blob_id FROM image_thumbnails
             UNION ALL SELECT 'l ' || blob_id || ' ' || t.name FROM image_tag_links JOIN image_tags t ON t.id = tag_id
             UNION ALL SELECT 't ' || name FROM image_tags
             ORDER BY 1",
        )
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn images() -> Connection {
    let db = urls();
    image(&db, 1, 'a', 3, true);
    image(&db, 2, 'b', 1, true);
    tag(&db, 1, "only-one");
    tag(&db, 1, "shared");
    tag(&db, 2, "shared");
    db
}

#[test]
fn deleting_an_image_removes_its_saves_bytes_thumbnail_and_unused_tags_only() {
    let mut db = images();
    let urls_before = dump(&db);
    let sha = "a".repeat(64);
    let expected = ImageCounts {
        saves: 3,
        thumbnails: 1,
        tag_links: 2,
        tags_removed: 1,
    };
    assert_eq!(image_counts(&db, 1, &sha).unwrap(), (3, expected));
    let tx = db.transaction().unwrap();
    assert_eq!(delete_image(&tx, 1, &sha).unwrap(), expected);
    tx.commit().unwrap();
    assert_eq!(
        image_dump(&db),
        ["b 2", "l 2 shared", "s 2-0", "t shared", "th 2"],
        "image 2 and the tag it still has are untouched"
    );
    assert_eq!(dump(&db), urls_before, "no visit or URL is touched");
    integrity(&db);
    // Gone: the image list and the tag search no longer show it, and it cannot be deleted again.
    let list = crate::images::list_images(&db, None, 60).unwrap();
    assert_eq!(
        list.items
            .iter()
            .map(|item| item.blob_id)
            .collect::<Vec<_>>(),
        [2]
    );
    let tx = db.transaction().unwrap();
    assert!(matches!(
        delete_image(&tx, 1, &sha),
        Err(ImageError::BlobMissing)
    ));
}

#[test]
fn an_image_is_deleted_only_with_its_own_hash() {
    let mut db = images();
    let before = image_dump(&db);
    let tx = db.transaction().unwrap();
    for (id, sha) in [
        (1, "b".repeat(64)),
        (2, "a".repeat(64)),
        (3, "a".repeat(64)),
    ] {
        assert!(matches!(
            image_counts(&tx, id, &sha),
            Err(ImageError::BlobMissing)
        ));
        assert!(matches!(
            delete_image(&tx, id, &sha),
            Err(ImageError::BlobMissing)
        ));
    }
    tx.commit().unwrap();
    assert_eq!(image_dump(&db), before);
}

#[test]
fn an_image_deletion_that_fails_part_way_leaves_everything() {
    let mut db = images();
    let before = image_dump(&db);
    db.execute_batch(
        "CREATE TEMP TRIGGER fail BEFORE DELETE ON image_thumbnails BEGIN SELECT RAISE(ABORT, 'x'); END;",
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    assert!(delete_image(&tx, 1, &"a".repeat(64)).is_err());
    drop(tx);
    assert_eq!(image_dump(&db), before, "the saves deleted first are back");
    integrity(&db);
}

#[test]
fn requests_are_checked_before_any_vault_is_opened() {
    use crate::vault::{tests::TempDir, Environment, Session};
    let tmp = TempDir::new();
    // No vault is configured: a request that is malformed says so, never "vault_not_configured".
    let mut session = Session::new(Environment {
        app_dir: Some(tmp.0.join("app")),
        db_override: None,
        exe_dir: None,
    });
    let token = "0".repeat(64);
    let sha = "a".repeat(64);
    let cases = [
        (json!({ "type": "url_delete" }), "invalid_request_id"),
        (
            json!({ "type": "url_delete", "requestId": "r", "vaultToken": token }),
            "invalid_url",
        ),
        (
            json!({ "type": "url_delete", "requestId": "r", "url": "", "vaultToken": token }),
            "invalid_url",
        ),
        (
            json!({ "type": "url_delete", "requestId": "r", "url": 1, "vaultToken": token }),
            "invalid_url",
        ),
        (
            json!({ "type": "url_delete", "requestId": "r", "url": "x".repeat(MAX_URL_CHARS + 1), "vaultToken": token }),
            "invalid_url",
        ),
        (
            json!({ "type": "url_delete", "requestId": "r", "url": A }),
            "invalid_vault_token",
        ),
        (
            json!({ "type": "url_delete", "requestId": "r", "url": A, "vaultToken": "x" }),
            "invalid_vault_token",
        ),
        (
            json!({ "type": "url_delete_preview", "requestId": "r" }),
            "invalid_url",
        ),
        (
            json!({ "type": "image_delete", "requestId": "r", "sha256": sha, "vaultToken": token }),
            "invalid_blob_id",
        ),
        (
            json!({ "type": "image_delete", "requestId": "r", "blobId": 0, "sha256": sha, "vaultToken": token }),
            "invalid_blob_id",
        ),
        (
            json!({ "type": "image_delete", "requestId": "r", "blobId": 1, "sha256": "A".repeat(64), "vaultToken": token }),
            "invalid_hash",
        ),
        (
            json!({ "type": "image_delete", "requestId": "r", "blobId": 1, "sha256": sha }),
            "invalid_vault_token",
        ),
        (
            json!({ "type": "image_delete_preview", "requestId": "r", "blobId": 1 }),
            "invalid_hash",
        ),
        (
            json!({ "type": "url_delete", "requestId": "r", "url": A, "vaultToken": token }),
            "vault_not_configured",
        ),
    ];
    for (message, code) in cases {
        let reply = handle(&mut session, &message);
        assert_eq!(reply["ok"], false, "{message}");
        assert_eq!(reply["error"], code, "{message}");
        assert!(reply.get("irreversible").is_none());
    }
}
