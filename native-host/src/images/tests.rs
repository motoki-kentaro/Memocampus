use super::*;
use crate::vault::tests::TempDir;
use crate::vault::{create_vault, open_vault, switch_active, ConfigLock, Environment};
use crate::SCHEMA_SQL;
use std::path::{Path, PathBuf};

// Minimal, structurally valid files: the host checks headers, not pixels.
pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]); // depth, colour type, ..., CRC
    bytes.extend_from_slice(&[0, 0, 0, 1, b'I', b'D', b'A', b'T', 0x78, 0, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
    bytes
}

fn gif(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&[
        0, 0, 0, 0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x44, 0x01, 0, 0x3B,
    ]);
    bytes
}

fn jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
    bytes.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    bytes.extend_from_slice(&[0xFF, 0xC0, 0, 11, 8]);
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[1, 1, 0x11, 0, 0xFF, 0xD9]);
    bytes
}

fn webp(width: u32, height: u32) -> Vec<u8> {
    let mut chunk = vec![0x2f];
    chunk.extend_from_slice(&((width - 1) | (height - 1) << 14).to_le_bytes());
    chunk.push(0);
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&((4 + 8 + chunk.len()) as u32).to_le_bytes());
    bytes.extend_from_slice(b"WEBPVP8L");
    bytes.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&chunk);
    bytes
}

fn bmp(width: i32, height: i32) -> Vec<u8> {
    let mut bytes = b"BM".to_vec();
    bytes.extend_from_slice(&58u32.to_le_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0]);
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.resize(58, 0);
    bytes
}

fn avif() -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 0x1c];
    bytes.extend_from_slice(b"ftypavif\0\0\0\0avifmif1miaf");
    bytes.extend_from_slice(b"\0\0\0\x08meta");
    bytes
}

fn request(bytes: &[u8], mime: &str, width: u32, height: u32, save_id: &str) -> SaveRequest {
    SaveRequest {
        save_id: save_id.to_owned(),
        sha256: sha256_hex(bytes),
        byte_length: bytes.len() as u64,
        mime_type: mime.to_owned(),
        width,
        height,
        image_url: "https://img.test/a.png".to_owned(),
        page_url: Some("https://page.test/post".to_owned()),
        frame_url: None,
        incognito: false,
    }
}

fn thumbnail(generator: u32) -> ThumbnailInput {
    ThumbnailInput {
        mime_type: "image/png".to_owned(),
        width: 4,
        height: 2,
        generator,
        data: png(4, 2),
    }
}

struct Fixture {
    tmp: TempDir,
    env: Environment,
    session: Session,
    ticket: VaultTicket,
    vault: PathBuf,
}

pub(crate) fn configure(env: &Environment, path: &Path) {
    let lock = ConfigLock::acquire(env).unwrap();
    switch_active(env, &lock, path, false).unwrap();
}

fn fixture() -> Fixture {
    let tmp = TempDir::new();
    let env = tmp.env();
    let vault = tmp.0.join("v.sqlite3");
    create_vault(&vault).unwrap();
    configure(&env, &vault);
    let mut session = Session::new(env.clone());
    let ticket = session.ticket().unwrap();
    Fixture {
        tmp,
        env,
        session,
        ticket,
        vault,
    }
}

/// A vault as created before images existed (no image tables).
fn legacy_vault(path: &Path, version: i64) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(SCHEMA_SQL).unwrap();
    db.execute_batch(&format!(
        "PRAGMA user_version = {version};
         INSERT INTO visits VALUES ('v1', 'https://a.test/', 'A', '2026-10-01T00:00:00.000Z');
         INSERT INTO tab_snapshots VALUES ('s1', 1, 0, 'https://b.test/', 'B', 0, 0, '2026-10-02T00:00:00.000Z');"
    ))
    .unwrap();
}

fn legacy_fixture(version: i64) -> (TempDir, Session, VaultTicket, PathBuf) {
    let tmp = TempDir::new();
    let env = tmp.env();
    let vault = tmp.0.join("old.sqlite3");
    legacy_vault(&vault, version);
    configure(&env, &vault);
    let mut session = Session::new(env);
    let ticket = session.ticket().unwrap();
    (tmp, session, ticket, vault)
}

fn save(
    f: &mut Fixture,
    bytes: &[u8],
    mime: &str,
    size: (u32, u32),
    save_id: &str,
    now: i64,
) -> Result<SaveOutcome, ImageError> {
    let request = request(bytes, mime, size.0, size.1, save_id);
    save_image(&mut f.session, &f.ticket, &request, Some(bytes), None, now)
}

fn scalar(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn row_dump(path: &Path) -> Vec<String> {
    let db = open_vault(path).unwrap();
    let mut statement = db
        .prepare("SELECT event_id || url || title || visited_at FROM visits UNION ALL SELECT event_id || url || title || recorded_at FROM tab_snapshots ORDER BY 1")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.collect::<rusqlite::Result<_>>().unwrap()
}

#[test]
fn new_vaults_are_created_at_schema_2_with_image_tables() {
    let f = fixture();
    let db = open_vault(&f.vault).unwrap();
    assert_eq!(scalar(&db, "PRAGMA user_version"), IMAGE_SCHEMA_VERSION);
    assert!(tables_present(&db).unwrap());
    validate_image_schema(&db).unwrap();
}

#[test]
fn data_is_the_last_column_and_listing_uses_the_recent_index() {
    let f = fixture();
    let db = open_vault(&f.vault).unwrap();
    for table in ["image_blobs", "image_thumbnails"] {
        let last: String = db
            .query_row(
                &format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid DESC LIMIT 1"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            last, "data",
            "{table}: listing must not walk BLOB overflow pages"
        );
    }
    let plan: Vec<String> = db
        .prepare(&format!("EXPLAIN QUERY PLAN {LIST_SQL}"))
        .unwrap()
        .query_map(
            params![Option::<i64>::None, Option::<i64>::None, 10],
            |row| row.get(3),
        )
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.iter().any(|step| step.contains("image_blobs_recent")),
        "{plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|step| step.contains("TEMP B-TREE FOR ORDER BY")),
        "{plan:?}"
    );
}

#[test]
fn reading_a_schema_1_vault_never_migrates_it() {
    let (_tmp, _session, _ticket, vault) = legacy_fixture(1);
    let db = open_vault(&vault).unwrap();
    assert_eq!(list_images(&db, None, 10).unwrap().items, vec![]);
    assert_eq!(image_sources(&db, 1, None, 10).unwrap().items, vec![]);
    let bytes = png(1, 1);
    assert_eq!(
        precheck(&db, &request(&bytes, "image/png", 1, 1, "s")),
        Ok(Precheck::NeedUpload)
    );
    assert_eq!(scalar(&db, "PRAGMA user_version"), 1);
    assert!(!tables_present(&db).unwrap());
}

#[test]
fn the_first_image_save_migrates_a_schema_1_vault_without_touching_existing_rows() {
    let (_tmp, mut session, ticket, vault) = legacy_fixture(1);
    let before = row_dump(&vault);
    let bytes = png(3, 2);
    save_image(
        &mut session,
        &ticket,
        &request(&bytes, "image/png", 3, 2, "s1"),
        Some(&bytes),
        None,
        1000,
    )
    .unwrap();
    let db = open_vault(&vault).unwrap();
    assert_eq!(scalar(&db, "PRAGMA user_version"), IMAGE_SCHEMA_VERSION);
    assert_eq!(
        scalar(&db, "PRAGMA application_id"),
        0,
        "the header tag is left alone"
    );
    assert_eq!(row_dump(&vault), before);
    ensure_schema(&db).unwrap(); // idempotent
    assert_eq!(scalar(&db, "SELECT count(*) FROM image_blobs"), 1);
    assert!(crate::vault::open_vault_checked(&vault).is_ok());
}

#[test]
fn untagged_schema_0_vaults_migrate_too() {
    let (_tmp, mut session, ticket, vault) = legacy_fixture(0);
    let bytes = gif(2, 2);
    save_image(
        &mut session,
        &ticket,
        &request(&bytes, "image/gif", 2, 2, "s1"),
        Some(&bytes),
        None,
        1,
    )
    .unwrap();
    assert_eq!(
        scalar(&open_vault(&vault).unwrap(), "PRAGMA user_version"),
        3
    );
}

#[test]
fn a_failed_migration_rolls_back_completely() {
    let (_tmp, mut session, ticket, vault) = legacy_fixture(1);
    // An unrelated table that happens to be called saved_images.
    Connection::open(&vault)
        .unwrap()
        .execute_batch("CREATE TABLE saved_images (something);")
        .unwrap();
    let bytes = png(1, 1);
    let result = save_image(
        &mut session,
        &ticket,
        &request(&bytes, "image/png", 1, 1, "s1"),
        Some(&bytes),
        None,
        1,
    );
    assert_eq!(result, Err(ImageError::SchemaMismatch));
    let db = open_vault(&vault).unwrap();
    assert_eq!(scalar(&db, "PRAGMA user_version"), 1);
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM sqlite_master WHERE name = 'image_blobs'"
        ),
        0
    );
}

#[test]
fn a_newer_vault_is_neither_migrated_nor_written() {
    let mut f = fixture();
    open_vault(&f.vault)
        .unwrap()
        .execute_batch("PRAGMA user_version = 4;")
        .unwrap();
    let bytes = png(1, 1);
    assert_eq!(
        save(&mut f, &bytes, "image/png", (1, 1), "s1", 1),
        Err(ImageError::VaultNewer)
    );
    let db = open_vault(&f.vault).unwrap();
    assert_eq!(list_images(&db, None, 5), Err(ImageError::VaultNewer));
    assert_eq!(scalar(&db, "PRAGMA user_version"), 4);
    assert_eq!(scalar(&db, "SELECT count(*) FROM image_blobs"), 0);
}

#[test]
fn originals_are_stored_byte_for_byte_with_their_metadata() {
    let mut f = fixture();
    let bytes = jpeg(640, 480);
    let mut request = request(&bytes, "image/jpeg", 640, 480, "s1");
    request.frame_url = Some("https://frame.test/".to_owned());
    let outcome = save_image(
        &mut f.session,
        &f.ticket,
        &request,
        Some(&bytes),
        None,
        1_793_330_100_123,
    )
    .unwrap();
    assert!(!outcome.deduplicated && !outcome.replayed);
    let db = open_vault(&f.vault).unwrap();
    let stored: (Vec<u8>, String, i64, i64, i64, i64, String) = db
        .query_row(
            "SELECT data, mime_type, byte_length, width, height, created_at, sha256 FROM image_blobs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .unwrap();
    assert_eq!(stored.0, bytes);
    assert_eq!(
        (stored.1.as_str(), stored.2, stored.3, stored.4, stored.5),
        (
            "image/jpeg",
            bytes.len() as i64,
            640,
            480,
            1_793_330_100_123
        )
    );
    assert_eq!(stored.6, sha256_hex(&bytes));
    let history: (String, String, String, i64) = db
        .query_row(
            "SELECT image_url, page_url, frame_url, saved_at FROM saved_images",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        history,
        (
            "https://img.test/a.png".to_owned(),
            "https://page.test/post".to_owned(),
            "https://frame.test/".to_owned(),
            1_793_330_100_123
        )
    );
}

#[test]
fn identical_bytes_share_one_blob_and_every_save_action_keeps_its_own_history() {
    let mut f = fixture();
    let bytes = png(8, 8);
    let first = save(&mut f, &bytes, "image/png", (8, 8), "save-a", 1000).unwrap();
    let mut other_page = request(&bytes, "image/png", 8, 8, "save-b");
    other_page.page_url = Some("https://elsewhere.test/".to_owned());
    let second = save_image(
        &mut f.session,
        &f.ticket,
        &other_page,
        Some(&bytes),
        None,
        2000,
    )
    .unwrap();
    assert_eq!(second.blob_id, first.blob_id);
    assert!(second.deduplicated && !second.replayed);
    assert_ne!(second.saved_image_id, first.saved_image_id);

    let db = open_vault(&f.vault).unwrap();
    assert_eq!(scalar(&db, "SELECT count(*) FROM image_blobs"), 1);
    assert_eq!(scalar(&db, "SELECT count(*) FROM saved_images"), 2);
    assert_eq!(scalar(&db, "SELECT save_count FROM image_blobs"), 2);
    assert_eq!(scalar(&db, "SELECT last_saved_at FROM image_blobs"), 2000);
    assert_eq!(
        scalar(&db, "SELECT created_at FROM image_blobs"),
        1000,
        "first save time is kept"
    );
    let sources = image_sources(&db, first.blob_id, None, 10).unwrap();
    let pages: Vec<_> = sources
        .items
        .iter()
        .map(|s| s.page_url.as_deref())
        .collect();
    assert_eq!(
        pages,
        [
            Some("https://elsewhere.test/"),
            Some("https://page.test/post")
        ]
    );
}

#[test]
fn retrying_the_same_save_action_never_duplicates_history() {
    let mut f = fixture();
    let bytes = webp(5, 7);
    let first = save(&mut f, &bytes, "image/webp", (5, 7), "retry-me", 10).unwrap();
    let replay = save(&mut f, &bytes, "image/webp", (5, 7), "retry-me", 99).unwrap();
    assert_eq!(
        (replay.blob_id, replay.saved_image_id, replay.replayed),
        (first.blob_id, first.saved_image_id, true)
    );
    let db = open_vault(&f.vault).unwrap();
    assert_eq!(scalar(&db, "SELECT count(*) FROM saved_images"), 1);
    assert_eq!(scalar(&db, "SELECT save_count FROM image_blobs"), 1);
    assert_eq!(scalar(&db, "SELECT last_saved_at FROM image_blobs"), 10);
    // The same save id for different bytes is a client bug, not a new save.
    let other = png(1, 1);
    assert_eq!(
        save(&mut f, &other, "image/png", (1, 1), "retry-me", 100),
        Err(ImageError::SaveIdConflict)
    );
    assert_eq!(scalar(&db, "SELECT count(*) FROM image_blobs"), 1);
}

#[test]
fn precheck_separates_replays_known_blobs_and_new_uploads() {
    let mut f = fixture();
    let bytes = png(2, 3);
    let db = open_vault(&f.vault).unwrap();
    assert_eq!(
        precheck(&db, &request(&bytes, "image/png", 2, 3, "a")),
        Ok(Precheck::NeedUpload)
    );
    let saved = save(&mut f, &bytes, "image/png", (2, 3), "a", 1).unwrap();
    assert_eq!(
        precheck(&db, &request(&bytes, "image/png", 2, 3, "a")),
        Ok(Precheck::AlreadySaved(SaveOutcome {
            replayed: true,
            deduplicated: true,
            ..saved
        }))
    );
    assert_eq!(
        precheck(&db, &request(&bytes, "image/png", 2, 3, "b")),
        Ok(Precheck::BlobExists {
            blob_id: saved.blob_id
        })
    );
    // A known blob is recorded without sending the bytes again ...
    let without_bytes = save_image(
        &mut f.session,
        &f.ticket,
        &request(&bytes, "image/png", 2, 3, "b"),
        None,
        None,
        2,
    )
    .unwrap();
    assert!(without_bytes.deduplicated && !without_bytes.replayed);
    // ... but an unknown one cannot be.
    let unknown = png(9, 9);
    let result = save_image(
        &mut f.session,
        &f.ticket,
        &request(&unknown, "image/png", 9, 9, "c"),
        None,
        None,
        3,
    );
    assert_eq!(result, Err(ImageError::BlobMissing));
    assert_eq!(scalar(&db, "SELECT count(*) FROM saved_images"), 2);
}

#[test]
fn every_supported_format_is_detected_from_its_bytes() {
    let cases: [(Vec<u8>, ImageFormat, (u32, u32)); 6] = [
        (png(3, 4), ImageFormat::Png, (3, 4)),
        (jpeg(30, 20), ImageFormat::Jpeg, (30, 20)),
        (gif(5, 6), ImageFormat::Gif, (5, 6)),
        (webp(7, 8), ImageFormat::Webp, (7, 8)),
        (bmp(9, -10), ImageFormat::Bmp, (9, 10)),
        (avif(), ImageFormat::Avif, (11, 12)),
    ];
    for (bytes, format, (w, h)) in cases {
        assert_eq!(sniff(&bytes), Ok(format));
        let request = request(&bytes, format.mime(), w, h, "x");
        assert_eq!(
            validate_original(&request, &bytes),
            Ok(format),
            "{format:?}"
        );
    }
    let extensions: Vec<_> = [
        ImageFormat::Png,
        ImageFormat::Jpeg,
        ImageFormat::Gif,
        ImageFormat::Webp,
        ImageFormat::Avif,
        ImageFormat::Bmp,
    ]
    .iter()
    .map(|f| f.extension())
    .collect();
    assert_eq!(extensions, ["png", "jpg", "gif", "webp", "avif", "bmp"]);
}

#[test]
fn the_declared_mime_type_is_never_trusted() {
    let bytes = jpeg(4, 4);
    // A server or page claiming PNG for JPEG bytes.
    assert_eq!(
        validate_original(&request(&bytes, "image/png", 4, 4, "x"), &bytes),
        Err(ImageError::MimeMismatch)
    );
    let svg = b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>".to_vec();
    assert_eq!(sniff(&svg), Err(ImageError::SvgNotSupported));
    assert_eq!(
        validate_original(&request(&svg, "image/png", 1, 1, "x"), &svg),
        Err(ImageError::SvgNotSupported)
    );
    assert_eq!(
        validate_request(&request(&svg, "image/svg+xml", 1, 1, "x")),
        Err(ImageError::SvgNotSupported)
    );
    let html = b"<!doctype html><html><body>not an image</body></html>".to_vec();
    assert_eq!(sniff(&html), Err(ImageError::UnsupportedFormat));
    assert_eq!(
        validate_request(&request(&html, "text/html", 1, 1, "x")),
        Err(ImageError::UnsupportedFormat)
    );
}

#[test]
fn corrupt_truncated_or_inconsistent_originals_are_rejected() {
    let bytes = png(10, 10);
    let ok = request(&bytes, "image/png", 10, 10, "x");
    assert_eq!(
        validate_original(&ok, &bytes[..bytes.len() - 3]),
        Err(ImageError::LengthMismatch)
    );
    let truncated = &bytes[..bytes.len() - 12];
    assert_eq!(
        validate_original(&request(truncated, "image/png", 10, 10, "x"), truncated),
        Err(ImageError::Truncated)
    );
    let mut flipped = bytes.clone();
    flipped[20] ^= 1;
    assert_eq!(
        validate_original(&ok, &flipped),
        Err(ImageError::HashMismatch)
    );
    assert_eq!(
        validate_original(&request(&bytes, "image/png", 11, 10, "x"), &bytes),
        Err(ImageError::DimensionMismatch)
    );
    let gif_cut = &gif(2, 2)[..20];
    assert_eq!(
        validate_original(&request(gif_cut, "image/gif", 2, 2, "x"), gif_cut),
        Err(ImageError::Truncated)
    );
    let mut webp_bad = webp(2, 2);
    webp_bad.push(0);
    assert_eq!(
        validate_original(&request(&webp_bad, "image/webp", 2, 2, "x"), &webp_bad),
        Err(ImageError::Truncated)
    );
    assert_eq!(
        validate_original(&request(&[], "image/png", 1, 1, "x"), &[]),
        Err(ImageError::Empty)
    );
}

#[test]
fn the_32_mib_limit_applies_to_raw_bytes_in_metadata_and_content() {
    let mut declared = request(&png(1, 1), "image/png", 1, 1, "x");
    declared.byte_length = MAX_IMAGE_BYTES as u64 + 1;
    assert_eq!(
        validate_request(&declared).unwrap_err(),
        ImageError::TooLarge
    );
    declared.byte_length = MAX_IMAGE_BYTES as u64;
    assert!(
        validate_request(&declared).is_ok(),
        "exactly 32 MiB is allowed"
    );
    let oversized = vec![0u8; MAX_IMAGE_BYTES + 1];
    assert_eq!(
        validate_original(&declared, &oversized),
        Err(ImageError::TooLarge)
    );
}

#[test]
fn metadata_is_validated_before_anything_is_stored() {
    let bytes = png(1, 1);
    let base = request(&bytes, "image/png", 1, 1, "ok-id");
    type Mutation = Box<dyn Fn(&mut SaveRequest)>;
    let cases: Vec<(Mutation, ImageError)> = vec![
        (
            Box::new(|r| r.save_id = String::new()),
            ImageError::InvalidSaveId,
        ),
        (
            Box::new(|r| r.save_id = "bad id/..".into()),
            ImageError::InvalidSaveId,
        ),
        (
            Box::new(|r| r.save_id = "x".repeat(65)),
            ImageError::InvalidSaveId,
        ),
        (
            Box::new(|r| r.sha256 = "abc".into()),
            ImageError::InvalidHash,
        ),
        (
            Box::new(|r| r.sha256 = r.sha256.to_uppercase()),
            ImageError::InvalidHash,
        ),
        (Box::new(|r| r.byte_length = 0), ImageError::Empty),
        (Box::new(|r| r.width = 0), ImageError::InvalidDimensions),
        (
            Box::new(|r| r.height = MAX_IMAGE_EDGE + 1),
            ImageError::InvalidDimensions,
        ),
        (
            Box::new(|r| r.image_url = "javascript:alert(1)".into()),
            ImageError::InvalidUrl,
        ),
        (
            Box::new(|r| r.image_url = "file:///C:/secret.png".into()),
            ImageError::InvalidUrl,
        ),
        (
            Box::new(|r| r.image_url = "data:image/png;base64,AAAA".into()),
            ImageError::InvalidUrl,
        ),
        (
            Box::new(|r| r.image_url = format!("https://a.test/{}", "x".repeat(MAX_URL_CHARS))),
            ImageError::InvalidUrl,
        ),
        (
            Box::new(|r| r.page_url = Some("chrome://settings".into())),
            ImageError::InvalidUrl,
        ),
        (
            Box::new(|r| r.frame_url = Some("not a url".into())),
            ImageError::InvalidUrl,
        ),
        (
            Box::new(|r| r.mime_type = "image/tiff".into()),
            ImageError::UnsupportedFormat,
        ),
    ];
    for (mutate, expected) in cases {
        let mut request = base.clone();
        mutate(&mut request);
        assert_eq!(
            validate_request(&request).unwrap_err(),
            expected,
            "{request:?}"
        );
    }
    for url in [
        "data:image/png",
        "blob:https://x.test/0b5f-uuid",
        "https://cdn.test/i.png?name=small",
    ] {
        let mut request = base.clone();
        request.image_url = url.into();
        assert!(validate_request(&request).is_ok(), "{url}");
    }
}

#[test]
fn thumbnails_are_validated_and_only_replaced_by_a_newer_recipe() {
    let mut f = fixture();
    let bytes = png(16, 16);
    let mut big = thumbnail(1);
    big.data.resize(MAX_THUMBNAIL_BYTES + 1, 0);
    let bad_thumbnails = [
        ThumbnailInput {
            data: b"<svg></svg>".to_vec(),
            ..thumbnail(1)
        },
        ThumbnailInput {
            mime_type: "image/gif".into(),
            data: gif(4, 2),
            ..thumbnail(1)
        },
        ThumbnailInput {
            mime_type: "image/jpeg".into(),
            ..thumbnail(1)
        },
        big,
        thumbnail(0),
        ThumbnailInput {
            width: MAX_THUMBNAIL_EDGE + 1,
            ..thumbnail(1)
        },
    ];
    for bad in &bad_thumbnails {
        let result = save_image(
            &mut f.session,
            &f.ticket,
            &request(&bytes, "image/png", 16, 16, "t"),
            Some(&bytes),
            Some(bad),
            1,
        );
        assert_eq!(result, Err(ImageError::InvalidThumbnail));
    }
    let db = open_vault(&f.vault).unwrap();
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM image_blobs"),
        0,
        "a bad thumbnail stores nothing"
    );

    let with = |f: &mut Fixture, id: &str, generator: u32, now: i64| {
        save_image(
            &mut f.session,
            &f.ticket,
            &request(&bytes, "image/png", 16, 16, id),
            Some(&bytes),
            Some(&thumbnail(generator)),
            now,
        )
        .unwrap();
    };
    with(&mut f, "t1", 2, 1);
    with(&mut f, "t2", 1, 2);
    assert_eq!(
        scalar(&db, "SELECT generator FROM image_thumbnails"),
        2,
        "an older recipe does not overwrite"
    );
    with(&mut f, "t3", 3, 3);
    assert_eq!(scalar(&db, "SELECT generator FROM image_thumbnails"), 3);
    // A save without a thumbnail is still a complete save of the original.
    let other = gif(3, 3);
    save(&mut f, &other, "image/gif", (3, 3), "no-thumb", 4).unwrap();
    let page = list_images(&db, None, 10).unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|i| i.has_thumbnail)
            .collect::<Vec<_>>(),
        [false, true]
    );
}

#[test]
fn a_failure_inside_the_transaction_leaves_no_blob_history_or_counter_change() {
    let mut f = fixture();
    let keep = png(2, 2);
    save(&mut f, &keep, "image/png", (2, 2), "keep", 1).unwrap();
    open_vault(&f.vault)
        .unwrap()
        .execute_batch("CREATE TRIGGER fail_history BEFORE INSERT ON saved_images BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;")
        .unwrap();
    let fresh = png(3, 3);
    assert_eq!(
        save(&mut f, &fresh, "image/png", (3, 3), "new", 2),
        Err(ImageError::Db)
    );
    assert_eq!(
        save(&mut f, &keep, "image/png", (2, 2), "dup", 3),
        Err(ImageError::Db)
    );
    let db = open_vault(&f.vault).unwrap();
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM image_blobs"),
        1,
        "the new blob was rolled back"
    );
    assert_eq!(
        scalar(&db, "SELECT save_count FROM image_blobs"),
        1,
        "the counter update was rolled back"
    );
    assert_eq!(scalar(&db, "SELECT last_saved_at FROM image_blobs"), 1);
    assert_eq!(scalar(&db, "SELECT count(*) FROM saved_images"), 1);
}

#[test]
fn foreign_keys_are_enforced_on_vault_connections() {
    let f = fixture();
    let db = open_vault(&f.vault).unwrap();
    let orphan = db.execute(
        "INSERT INTO saved_images (save_id, blob_id, image_url, saved_at, incognito) VALUES ('o', 999, 'https://a.test/', 1, 0)",
        [],
    );
    assert!(orphan.is_err());
}

#[test]
fn a_save_never_lands_in_a_vault_other_than_the_one_it_started_in() {
    let mut f = fixture();
    let other = f.tmp.0.join("other.sqlite3");
    create_vault(&other).unwrap();
    // The user switches vaults while the image is still being transferred.
    configure(&f.env, &other);
    let bytes = png(4, 4);
    assert_eq!(
        save(&mut f, &bytes, "image/png", (4, 4), "s", 1),
        Err(ImageError::Vault(VaultError::Switched))
    );
    for path in [&f.vault, &other] {
        assert_eq!(
            scalar(
                &open_vault(path).unwrap(),
                "SELECT count(*) FROM image_blobs"
            ),
            0
        );
    }
    // A ticket taken now names the new vault, and the save then lands there.
    let new_ticket = f.session.ticket().unwrap();
    assert_eq!(new_ticket.path, other.to_string_lossy());
    save_image(
        &mut f.session,
        &new_ticket,
        &request(&bytes, "image/png", 4, 4, "s"),
        Some(&bytes),
        None,
        2,
    )
    .unwrap();
    assert_eq!(
        scalar(
            &open_vault(&other).unwrap(),
            "SELECT count(*) FROM image_blobs"
        ),
        1
    );
    assert_eq!(
        scalar(
            &open_vault(&f.vault).unwrap(),
            "SELECT count(*) FROM image_blobs"
        ),
        0
    );
}

#[test]
fn a_switch_and_switch_back_still_rejects_a_ticket_only_if_the_vault_differs() {
    let mut f = fixture();
    let other = f.tmp.0.join("other.sqlite3");
    create_vault(&other).unwrap();
    configure(&f.env, &other);
    configure(&f.env, &f.vault);
    // The vault the transfer started in is active again: committing there is correct.
    let bytes = png(4, 4);
    assert!(save(&mut f, &bytes, "image/png", (4, 4), "s", 1).is_ok());
}

#[test]
fn listing_is_newest_first_keyset_paged_and_stable_under_inserts() {
    let mut f = fixture();
    for i in 0..130u32 {
        let bytes = png(i + 1, 1);
        save(
            &mut f,
            &bytes,
            "image/png",
            (i + 1, 1),
            &format!("s{i}"),
            1_000 + i64::from(i / 2),
        )
        .unwrap();
    }
    let db = open_vault(&f.vault).unwrap();
    let first = list_images(&db, None, 50).unwrap();
    assert_eq!(first.items.len(), 50);
    assert_eq!(first.items[0].width, 130, "newest first");
    // A new save between page requests must not shift or duplicate the next pages.
    let late = gif(1, 1);
    save(&mut f, &late, "image/gif", (1, 1), "late", 9_999).unwrap();
    let second = list_images(&db, first.next, 50).unwrap();
    let third = list_images(&db, second.next, 50).unwrap();
    assert!(third.next.is_none());
    let widths: Vec<i64> = [&first, &second, &third]
        .iter()
        .flat_map(|page| page.items.iter().map(|item| item.width))
        .collect();
    assert_eq!(
        widths,
        (1..=130).rev().collect::<Vec<i64>>(),
        "every image exactly once, ties by id"
    );
    assert_eq!(
        list_images(&db, None, 1).unwrap().items[0].mime_type,
        "image/gif"
    );
    // Saving an old image again moves it to the front.
    let old = png(1, 1);
    save(&mut f, &old, "image/png", (1, 1), "again", 20_000).unwrap();
    let top = list_images(&db, None, 2).unwrap();
    assert_eq!((top.items[0].width, top.items[0].save_count), (1, 2));
    assert_eq!(
        list_images(&db, None, 0).unwrap().items.len(),
        1,
        "limit is clamped to at least 1"
    );
    assert_eq!(
        list_images(&db, None, 10_000).unwrap().items.len(),
        MAX_PAGE_SIZE as usize
    );
}

#[test]
fn listing_reports_the_latest_source_and_paginates_history() {
    let mut f = fixture();
    let bytes = png(2, 2);
    save(&mut f, &bytes, "image/png", (2, 2), "a", 1).unwrap();
    let mut later = request(&bytes, "image/png", 2, 2, "b");
    later.image_url = "data:image/png".into();
    later.page_url = None;
    save_image(&mut f.session, &f.ticket, &later, Some(&bytes), None, 2).unwrap();
    let db = open_vault(&f.vault).unwrap();
    let item = &list_images(&db, None, 5).unwrap().items[0];
    assert_eq!(
        (
            item.latest_image_url.as_str(),
            item.latest_page_url.as_deref(),
            item.save_count
        ),
        ("data:image/png", None, 2)
    );
    let page = image_sources(&db, item.blob_id, None, 1).unwrap();
    assert_eq!(page.items.len(), 1);
    let rest = image_sources(&db, item.blob_id, page.next, 1).unwrap();
    assert_eq!(
        rest.items[0].page_url.as_deref(),
        Some("https://page.test/post")
    );
    assert!(rest.next.is_none());
}

#[test]
fn table_counts_are_absent_before_migration_and_exact_after() {
    let (_tmp, _session, _ticket, vault) = legacy_fixture(1);
    assert_eq!(table_counts(&open_vault(&vault).unwrap()).unwrap(), None);
    let mut f = fixture();
    let bytes = png(5, 5);
    save_image(
        &mut f.session,
        &f.ticket,
        &request(&bytes, "image/png", 5, 5, "a"),
        Some(&bytes),
        Some(&thumbnail(1)),
        1,
    )
    .unwrap();
    save(&mut f, &bytes, "image/png", (5, 5), "b", 2).unwrap();
    assert_eq!(
        table_counts(&open_vault(&f.vault).unwrap()).unwrap(),
        Some((1, 2, 1))
    );
}

#[test]
fn backups_carry_images_and_verify_their_counts() {
    let mut f = fixture();
    let bytes = jpeg(12, 9);
    save_image(
        &mut f.session,
        &f.ticket,
        &request(&bytes, "image/jpeg", 12, 9, "a"),
        Some(&bytes),
        Some(&thumbnail(1)),
        7,
    )
    .unwrap();
    let dest = f.tmp.0.join("backups");
    std::fs::create_dir(&dest).unwrap();
    let db = open_vault(&f.vault).unwrap();
    let backup = crate::vault::backup_vault(&db, &dest, std::time::SystemTime::now()).unwrap();
    let copy = open_vault(&backup).unwrap();
    assert_eq!(table_counts(&copy).unwrap(), Some((1, 1, 1)));
    let data: Vec<u8> = copy
        .query_row("SELECT data FROM image_blobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(data, bytes);
}

#[test]
fn error_codes_are_stable_and_distinct() {
    let errors = [
        ImageError::Vault(VaultError::Switched),
        ImageError::VaultNewer,
        ImageError::SchemaMismatch,
        ImageError::InvalidSaveId,
        ImageError::InvalidHash,
        ImageError::InvalidUrl,
        ImageError::Empty,
        ImageError::TooLarge,
        ImageError::LengthMismatch,
        ImageError::HashMismatch,
        ImageError::SvgNotSupported,
        ImageError::UnsupportedFormat,
        ImageError::MimeMismatch,
        ImageError::InvalidDimensions,
        ImageError::DimensionMismatch,
        ImageError::Truncated,
        ImageError::InvalidThumbnail,
        ImageError::SaveIdConflict,
        ImageError::BlobMissing,
        ImageError::Db,
    ];
    let codes: Vec<&str> = errors.iter().map(|error| error.code()).collect();
    let unique: std::collections::HashSet<_> = codes.iter().collect();
    assert_eq!(unique.len(), codes.len(), "{codes:?}");
    assert_eq!(
        ImageError::Vault(VaultError::Switched).code(),
        "vault_switched"
    );
    assert_eq!(ImageError::TooLarge.code(), "too_large");
    assert_eq!(ImageError::SvgNotSupported.code(), "svg_not_supported");
}

#[test]
fn a_thumbnail_whose_header_size_differs_from_the_declared_size_is_rejected() {
    let mut f = fixture();
    let bytes = png(16, 16);
    let lying = ThumbnailInput {
        width: 5,
        ..thumbnail(1)
    }; // the PNG header says 4x2
    let result = save_image(
        &mut f.session,
        &f.ticket,
        &request(&bytes, "image/png", 16, 16, "t"),
        Some(&bytes),
        Some(&lying),
        1,
    );
    assert_eq!(result, Err(ImageError::InvalidThumbnail));
    assert_eq!(validate_thumbnail(&thumbnail(1)), Ok(()));
    assert_eq!(
        scalar(
            &open_vault(&f.vault).unwrap(),
            "SELECT count(*) FROM image_blobs"
        ),
        0
    );
}

// --- The `incognito` column ------------------------------------------------------------------------

/// Replaces `saved_images` with the given definition, as an older or foreign vault would have it.
fn replace_history_table(vault: &Path, definition: &str, version: i64) {
    Connection::open(vault)
        .unwrap()
        .execute_batch(&format!(
            "DROP TABLE saved_images; CREATE TABLE saved_images ({definition}); PRAGMA user_version = {version};"
        ))
        .unwrap();
}

const V2_HISTORY: &str = "id INTEGER PRIMARY KEY, save_id TEXT NOT NULL UNIQUE, blob_id INTEGER NOT NULL, image_url TEXT NOT NULL, page_url TEXT, frame_url TEXT, saved_at INTEGER NOT NULL";

#[test]
fn a_new_vault_requires_an_explicit_origin_of_zero_or_one() {
    let f = fixture();
    let db = open_vault(&f.vault).unwrap();
    assert_eq!(scalar(&db, "PRAGMA user_version"), 3);
    db.execute_batch("INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data) VALUES (printf('%064d', 1), 'image/png', 1, 1, 1, 1, 1, x'00');").unwrap();
    let insert = |columns: &str, values: &str| {
        db.execute_batch(&format!(
            "INSERT INTO saved_images (save_id, blob_id, image_url, saved_at{columns}) VALUES (lower(hex(randomblob(8))), 1, 'https://a.test/', 1{values})"
        ))
    };
    assert!(insert("", "").is_err(), "no default");
    assert!(insert(", incognito", ", NULL").is_err());
    assert!(insert(", incognito", ", 2").is_err());
    assert!(insert(", incognito", ", -1").is_err());
    assert!(insert(", incognito", ", 'x'").is_err());
    assert_eq!(scalar(&db, "SELECT count(*) FROM saved_images"), 0);
    insert(", incognito", ", 0").unwrap();
    insert(", incognito", ", 1").unwrap();
    assert_eq!(scalar(&db, "SELECT count(*) FROM saved_images"), 2);
}

#[test]
fn image_tables_without_a_required_origin_are_refused_for_reading_and_writing() {
    let without_column = V2_HISTORY.to_owned();
    let nullable = format!("{V2_HISTORY}, incognito INTEGER");
    let with_default = format!("{V2_HISTORY}, incognito INTEGER NOT NULL DEFAULT 0");
    for definition in [without_column, nullable, with_default] {
        let mut f = fixture();
        replace_history_table(&f.vault, &definition, 2);
        let bytes = png(1, 1);
        assert_eq!(
            save(&mut f, &bytes, "image/png", (1, 1), "s1", 1),
            Err(ImageError::SchemaMismatch),
            "{definition}"
        );
        let db = open_vault(&f.vault).unwrap();
        assert_eq!(
            list_images(&db, None, 10).unwrap_err(),
            ImageError::SchemaMismatch
        );
        assert_eq!(
            image_sources(&db, 1, None, 10).unwrap_err(),
            ImageError::SchemaMismatch
        );
        // Refused, not converted.
        assert_eq!(scalar(&db, "PRAGMA user_version"), 2);
        assert_eq!(
            scalar(&db, "SELECT count(*) FROM pragma_table_info('saved_images') WHERE name = 'incognito' AND dflt_value IS NULL AND \"notnull\" = 1"),
            0
        );
    }
}
