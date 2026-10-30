//! Image vault storage (schema v3): original image bytes deduplicated by SHA-256, an append-only
//! save history, and regenerable thumbnails.
//!
//! - `image_blobs` holds each distinct original exactly once (`sha256` UNIQUE). The bytes are never
//!   rewritten. `data` is the last column so listing queries never walk a BLOB's overflow pages.
//! - `saved_images` gets one row per save action. `save_id` (chosen by the client for one action)
//!   makes retries of that action idempotent; a different action saving the same bytes adds a row.
//!   `incognito` belongs to the row, not to the image: the same bytes can have rows of both kinds.
//!   It has no default, so a save whose origin is unknown cannot be stored as a normal one.
//! - `image_thumbnails` is derived data and may be dropped and rebuilt from the originals.
//!
//! Everything a save writes happens in one transaction through `Session::write_for`, so a failure
//! leaves nothing behind and the write can only land in the vault the transfer started against.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::vault::{Session, VaultError, VaultTicket};

pub const IMAGE_SCHEMA_VERSION: i64 = 3;
/// Limit for one original image, counted in raw bytes (before any Base64 encoding).
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_THUMBNAIL_BYTES: usize = 256 * 1024;
pub const MAX_THUMBNAIL_EDGE: u32 = 1024;
pub const MAX_IMAGE_EDGE: u32 = 65_535;
pub const MAX_URL_CHARS: usize = 8192;
pub const MAX_PAGE_SIZE: u32 = 60;

pub const IMAGE_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS image_blobs (
  id            INTEGER PRIMARY KEY,
  sha256        TEXT    NOT NULL UNIQUE CHECK (length(sha256) = 64),
  mime_type     TEXT    NOT NULL,
  byte_length   INTEGER NOT NULL CHECK (byte_length > 0),
  width         INTEGER NOT NULL CHECK (width > 0),
  height        INTEGER NOT NULL CHECK (height > 0),
  created_at    INTEGER NOT NULL,
  last_saved_at INTEGER NOT NULL,
  save_count    INTEGER NOT NULL DEFAULT 1 CHECK (save_count > 0),
  data          BLOB    NOT NULL
);
CREATE INDEX IF NOT EXISTS image_blobs_recent ON image_blobs (last_saved_at DESC, id DESC);
CREATE TABLE IF NOT EXISTS saved_images (
  id         INTEGER PRIMARY KEY,
  save_id    TEXT    NOT NULL UNIQUE,
  blob_id    INTEGER NOT NULL REFERENCES image_blobs (id) ON DELETE RESTRICT,
  image_url  TEXT    NOT NULL,
  page_url   TEXT,
  frame_url  TEXT,
  saved_at   INTEGER NOT NULL,
  incognito  INTEGER NOT NULL CHECK (incognito IN (0, 1))
);
CREATE INDEX IF NOT EXISTS saved_images_by_blob ON saved_images (blob_id, saved_at DESC, id DESC);
CREATE TABLE IF NOT EXISTS image_thumbnails (
  blob_id    INTEGER PRIMARY KEY REFERENCES image_blobs (id) ON DELETE CASCADE,
  mime_type  TEXT    NOT NULL,
  width      INTEGER NOT NULL CHECK (width > 0),
  height     INTEGER NOT NULL CHECK (height > 0),
  generator  INTEGER NOT NULL CHECK (generator > 0),
  data       BLOB    NOT NULL
);
";

const IMAGE_TABLES: [(&str, &[&str]); 3] = [
    (
        "image_blobs",
        &[
            "id",
            "sha256",
            "mime_type",
            "byte_length",
            "width",
            "height",
            "created_at",
            "last_saved_at",
            "save_count",
            "data",
        ],
    ),
    (
        "saved_images",
        &[
            "id",
            "save_id",
            "blob_id",
            "image_url",
            "page_url",
            "frame_url",
            "saved_at",
            "incognito",
        ],
    ),
    (
        "image_thumbnails",
        &[
            "blob_id",
            "mime_type",
            "width",
            "height",
            "generator",
            "data",
        ],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    Vault(VaultError),
    VaultNewer,
    SchemaMismatch,
    InvalidSaveId,
    InvalidHash,
    InvalidUrl,
    Empty,
    TooLarge,
    LengthMismatch,
    HashMismatch,
    SvgNotSupported,
    UnsupportedFormat,
    MimeMismatch,
    InvalidDimensions,
    DimensionMismatch,
    Truncated,
    InvalidThumbnail,
    SaveIdConflict,
    BlobMissing,
    Db,
}

impl ImageError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Vault(error) => error.code(),
            Self::VaultNewer => "vault_newer",
            Self::SchemaMismatch => "image_schema_mismatch",
            Self::InvalidSaveId => "invalid_save_id",
            Self::InvalidHash => "invalid_hash",
            Self::InvalidUrl => "invalid_url",
            Self::Empty => "empty_image",
            Self::TooLarge => "too_large",
            Self::LengthMismatch => "length_mismatch",
            Self::HashMismatch => "hash_mismatch",
            Self::SvgNotSupported => "svg_not_supported",
            Self::UnsupportedFormat => "unsupported_format",
            Self::MimeMismatch => "mime_mismatch",
            Self::InvalidDimensions => "invalid_dimensions",
            Self::DimensionMismatch => "dimension_mismatch",
            Self::Truncated => "truncated_image",
            Self::InvalidThumbnail => "invalid_thumbnail",
            Self::SaveIdConflict => "save_id_conflict",
            Self::BlobMissing => "blob_missing",
            Self::Db => "db_error",
        }
    }
}

impl From<VaultError> for ImageError {
    fn from(error: VaultError) -> Self {
        Self::Vault(error)
    }
}

impl From<rusqlite::Error> for ImageError {
    fn from(_: rusqlite::Error) -> Self {
        // The SQLite message may echo data, so only a fixed code leaves the host.
        Self::Db
    }
}

// --- Format detection ------------------------------------------------------------------------

/// Image formats accepted in v1. SVG (active content) and anything unrecognised are rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Avif,
    Bmp,
}

impl ImageFormat {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Bmp => "image/bmp",
        }
    }

    /// File extension for exports, derived from the detected format.
    #[cfg_attr(not(test), allow(dead_code))] // used by the export in the next slice
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Bmp => "bmp",
        }
    }

    pub fn from_mime(mime: &str) -> Option<Self> {
        Some(match mime {
            "image/png" => Self::Png,
            "image/jpeg" => Self::Jpeg,
            "image/gif" => Self::Gif,
            "image/webp" => Self::Webp,
            "image/avif" => Self::Avif,
            "image/bmp" => Self::Bmp,
            _ => return None,
        })
    }
}

fn u16_le(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?).into())
}
fn u16_be(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?).into())
}
fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
fn u32_be(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
fn u24_le(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 3)?;
    Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
}

fn looks_like_svg_or_markup(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('<') && (trimmed.contains("<svg") || trimmed.starts_with("<?xml"))
}

/// Identifies the format from magic bytes only (never from HTTP headers or file names).
pub fn sniff(bytes: &[u8]) -> Result<ImageFormat, ImageError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(ImageFormat::Png);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok(ImageFormat::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok(ImageFormat::Gif);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok(ImageFormat::Webp);
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let box_end = (u32_be(bytes, 0).unwrap_or(0) as usize).clamp(16, bytes.len().min(256));
        let (brands, _) = bytes[8..box_end].as_chunks::<4>();
        if brands
            .iter()
            .any(|brand| brand == b"avif" || brand == b"avis")
        {
            return Ok(ImageFormat::Avif);
        }
    }
    if bytes.len() >= 26
        && bytes.starts_with(b"BM")
        && matches!(u32_le(bytes, 14), Some(12 | 40 | 52 | 56 | 108 | 124))
    {
        return Ok(ImageFormat::Bmp);
    }
    if looks_like_svg_or_markup(bytes) {
        return Err(ImageError::SvgNotSupported);
    }
    Err(ImageError::UnsupportedFormat)
}

/// Dimensions read from the header, where the format makes that cheap and reliable.
fn header_dimensions(format: ImageFormat, bytes: &[u8]) -> Option<(u32, u32)> {
    match format {
        ImageFormat::Png if bytes.get(12..16) == Some(b"IHDR") => {
            Some((u32_be(bytes, 16)?, u32_be(bytes, 20)?))
        }
        ImageFormat::Gif => Some((u16_le(bytes, 6)?, u16_le(bytes, 8)?)),
        ImageFormat::Bmp => {
            let width = i32::from_le_bytes(bytes.get(18..22)?.try_into().ok()?);
            let height = i32::from_le_bytes(bytes.get(22..26)?.try_into().ok()?);
            Some((width.unsigned_abs(), height.unsigned_abs()))
        }
        ImageFormat::Webp => match bytes.get(12..16)? {
            b"VP8 " if bytes.get(23..26) == Some(&[0x9d, 0x01, 0x2a]) => {
                Some((u16_le(bytes, 26)? & 0x3fff, u16_le(bytes, 28)? & 0x3fff))
            }
            b"VP8L" if bytes.get(20) == Some(&0x2f) => {
                let bits = u32_le(bytes, 21)?;
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => Some((u24_le(bytes, 24)? + 1, u24_le(bytes, 27)? + 1)),
            _ => None,
        },
        ImageFormat::Jpeg => {
            let mut at = 2;
            while at + 9 < bytes.len() {
                if bytes[at] != 0xFF {
                    return None;
                }
                let marker = bytes[at + 1];
                if marker == 0xFF {
                    at += 1; // fill byte
                    continue;
                }
                if matches!(marker, 0xD0..=0xD9 | 0x01) {
                    at += 2;
                    continue;
                }
                let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
                if is_sof {
                    return Some((u16_be(bytes, at + 7)?, u16_be(bytes, at + 5)?));
                }
                at += 2 + u16_be(bytes, at + 2)? as usize;
            }
            None
        }
        _ => None,
    }
}

/// Rejects files that are visibly cut short, for formats with a reliable end marker or length.
fn looks_complete(format: ImageFormat, bytes: &[u8]) -> bool {
    match format {
        ImageFormat::Png => {
            bytes.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82])
        }
        ImageFormat::Gif => bytes.last() == Some(&0x3B),
        ImageFormat::Webp => u32_le(bytes, 4).is_some_and(|size| size as usize + 8 == bytes.len()),
        ImageFormat::Bmp => {
            u32_le(bytes, 2).is_some_and(|size| size == 0 || size as usize == bytes.len())
        }
        // JPEG files often carry trailing data after EOI and AVIF has no single end marker; the
        // extension's decode check (createImageBitmap) covers them.
        ImageFormat::Jpeg | ImageFormat::Avif => true,
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// --- Requests ----------------------------------------------------------------------------------

/// Metadata of one save action, as declared by the extension.
#[derive(Debug, Clone)]
pub struct SaveRequest {
    pub save_id: String,
    pub sha256: String,
    pub byte_length: u64,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub image_url: String,
    pub page_url: Option<String>,
    pub frame_url: Option<String>,
    pub incognito: bool,
}

#[derive(Debug, Clone)]
pub struct ThumbnailInput {
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub generator: u32,
    pub data: Vec<u8>,
}

fn valid_web_url(raw: &str) -> bool {
    raw.chars().count() <= MAX_URL_CHARS
        && url::Url::parse(raw).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

/// `image_url` may also be a `blob:` URL or a bare `data:<mime>` marker (the payload is never stored).
fn valid_image_url(raw: &str) -> bool {
    if let Some(mime) = raw.strip_prefix("data:") {
        let (kind, subtype) = mime.split_once('/').unwrap_or(("", ""));
        return kind == "image"
            && !subtype.is_empty()
            && subtype
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'));
    }
    if let Some(inner) = raw.strip_prefix("blob:") {
        return raw.chars().count() <= MAX_URL_CHARS && valid_web_url(inner);
    }
    valid_web_url(raw)
}

fn valid_dimensions(width: u32, height: u32, max: u32) -> bool {
    (1..=max).contains(&width) && (1..=max).contains(&height)
}

/// Checks everything that does not need the image bytes.
pub fn validate_request(request: &SaveRequest) -> Result<ImageFormat, ImageError> {
    let id = &request.save_id;
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(ImageError::InvalidSaveId);
    }
    if request.sha256.len() != 64
        || !request
            .sha256
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err(ImageError::InvalidHash);
    }
    if request.byte_length == 0 {
        return Err(ImageError::Empty);
    }
    if request.byte_length > MAX_IMAGE_BYTES as u64 {
        return Err(ImageError::TooLarge);
    }
    if request.mime_type == "image/svg+xml" {
        return Err(ImageError::SvgNotSupported);
    }
    let format = ImageFormat::from_mime(&request.mime_type).ok_or(ImageError::UnsupportedFormat)?;
    if !valid_dimensions(request.width, request.height, MAX_IMAGE_EDGE) {
        return Err(ImageError::InvalidDimensions);
    }
    let urls_ok = valid_image_url(&request.image_url)
        && request.page_url.as_deref().is_none_or(valid_web_url)
        && request.frame_url.as_deref().is_none_or(valid_web_url);
    if !urls_ok {
        return Err(ImageError::InvalidUrl);
    }
    Ok(format)
}

/// Checks the received original against the declared metadata: size, SHA-256, real format,
/// header dimensions and visible truncation.
pub fn validate_original(request: &SaveRequest, bytes: &[u8]) -> Result<ImageFormat, ImageError> {
    let declared = validate_request(request)?;
    if bytes.is_empty() {
        return Err(ImageError::Empty);
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ImageError::TooLarge);
    }
    if bytes.len() as u64 != request.byte_length {
        return Err(ImageError::LengthMismatch);
    }
    if sha256_hex(bytes) != request.sha256 {
        return Err(ImageError::HashMismatch);
    }
    let format = sniff(bytes)?;
    if format != declared {
        return Err(ImageError::MimeMismatch);
    }
    if let Some(dimensions) = header_dimensions(format, bytes) {
        if dimensions != (request.width, request.height) {
            return Err(ImageError::DimensionMismatch);
        }
    }
    if !looks_complete(format, bytes) {
        return Err(ImageError::Truncated);
    }
    Ok(format)
}

pub fn validate_thumbnail(thumbnail: &ThumbnailInput) -> Result<(), ImageError> {
    let format = sniff(&thumbnail.data).map_err(|_| ImageError::InvalidThumbnail)?;
    let allowed = matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Webp
    );
    if !allowed
        || ImageFormat::from_mime(&thumbnail.mime_type) != Some(format)
        || thumbnail.data.len() > MAX_THUMBNAIL_BYTES
        || thumbnail.generator == 0
        || !valid_dimensions(thumbnail.width, thumbnail.height, MAX_THUMBNAIL_EDGE)
        // The declared size must be the one in the file's own header.
        || header_dimensions(format, &thumbnail.data) != Some((thumbnail.width, thumbnail.height))
    {
        return Err(ImageError::InvalidThumbnail);
    }
    Ok(())
}

// --- Schema ------------------------------------------------------------------------------------

fn user_version(db: &Connection) -> Result<i64, ImageError> {
    Ok(db.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn tables_present(db: &Connection) -> Result<bool, ImageError> {
    let count: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('image_blobs', 'saved_images', 'image_thumbnails')",
        [],
        |row| row.get(0),
    )?;
    Ok(count == 3)
}

/// Checks the image tables' columns. With `require_all`, a missing table is also a mismatch;
/// without it, only tables that already exist are checked (before creating the others).
fn check_image_tables(db: &Connection, require_all: bool) -> Result<(), ImageError> {
    for (table, columns) in IMAGE_TABLES {
        let mut statement = db.prepare("SELECT name FROM pragma_table_info(?1)")?;
        let actual = statement
            .query_map([table], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if actual.is_empty() && !require_all {
            continue;
        }
        if !columns
            .iter()
            .all(|column| actual.iter().any(|name| name == column))
        {
            return Err(ImageError::SchemaMismatch);
        }
    }
    let incognito: Option<(bool, Option<String>)> = db
        .query_row(
            "SELECT \"notnull\", dflt_value FROM pragma_table_info('saved_images') WHERE name = 'incognito'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if matches!(incognito, Some((not_null, default)) if !not_null || default.is_some()) {
        return Err(ImageError::SchemaMismatch);
    }
    Ok(())
}

fn validate_image_schema(db: &Connection) -> Result<(), ImageError> {
    check_image_tables(db, true)
}

/// Adds the image tables to a vault without any. A vault whose image tables have another shape (a
/// schema 2 vault, say) is refused, not converted. Must run inside a write transaction, so the new
/// tables and the version bump commit together with the first image write (or not at all).
/// Existing tables and rows are not touched.
pub fn ensure_schema(db: &Connection) -> Result<(), ImageError> {
    let version = user_version(db)?;
    if version > IMAGE_SCHEMA_VERSION {
        return Err(ImageError::VaultNewer);
    }
    // A foreign table that merely shares a name is reported, not built upon.
    check_image_tables(db, false)?;
    db.execute_batch(IMAGE_SCHEMA_SQL)?;
    validate_image_schema(db)?;
    if version < IMAGE_SCHEMA_VERSION {
        db.pragma_update(None, "user_version", IMAGE_SCHEMA_VERSION)?;
    }
    Ok(())
}

/// Read-only operations never migrate: a vault without image tables simply has no images, and
/// image tables of another shape are reported.
fn readable(db: &Connection) -> Result<bool, ImageError> {
    if user_version(db)? > IMAGE_SCHEMA_VERSION {
        return Err(ImageError::VaultNewer);
    }
    let present = tables_present(db)?;
    if present {
        validate_image_schema(db)?;
    }
    Ok(present)
}

/// Row counts of the image tables, or None for a vault that has none yet.
pub fn table_counts(db: &Connection) -> rusqlite::Result<Option<(i64, i64, i64)>> {
    let present: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('image_blobs', 'saved_images', 'image_thumbnails')",
        [],
        |row| row.get(0),
    )?;
    if present != 3 {
        return Ok(None);
    }
    db.query_row(
        "SELECT (SELECT count(*) FROM image_blobs), (SELECT count(*) FROM saved_images), (SELECT count(*) FROM image_thumbnails)",
        [],
        |row| Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?))),
    )
}

// --- Saving ------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOutcome {
    pub blob_id: i64,
    pub saved_image_id: i64,
    /// The bytes were already in the vault; only a history row was added.
    pub deduplicated: bool,
    /// This exact save action had already been committed; nothing was added.
    pub replayed: bool,
}

/// What the host can tell the extension before any bytes are transferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precheck {
    /// This save action was committed earlier (a retry after a lost answer).
    AlreadySaved(SaveOutcome),
    /// The bytes are already stored; the save can be recorded without uploading them.
    BlobExists {
        blob_id: i64,
    },
    NeedUpload,
}

fn find_save(
    db: &Connection,
    save_id: &str,
) -> Result<Option<(i64, i64, String, bool)>, ImageError> {
    Ok(db
        .query_row(
            "SELECT s.id, s.blob_id, b.sha256, s.incognito FROM saved_images s JOIN image_blobs b ON b.id = s.blob_id WHERE s.save_id = ?1",
            [save_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?)
}

fn find_blob(db: &Connection, sha256: &str) -> Result<Option<i64>, ImageError> {
    Ok(db
        .query_row(
            "SELECT id FROM image_blobs WHERE sha256 = ?1",
            [sha256],
            |row| row.get(0),
        )
        .optional()?)
}

/// Read-only look-up used before transferring bytes. Save-action idempotency (`save_id`) and blob
/// deduplication (`sha256`) are answered separately.
pub fn precheck(db: &Connection, request: &SaveRequest) -> Result<Precheck, ImageError> {
    validate_request(request)?;
    if !readable(db)? {
        return Ok(Precheck::NeedUpload);
    }
    if let Some((saved_image_id, blob_id, sha256, incognito)) = find_save(db, &request.save_id)? {
        if sha256 != request.sha256 || incognito != request.incognito {
            return Err(ImageError::SaveIdConflict);
        }
        return Ok(Precheck::AlreadySaved(SaveOutcome {
            blob_id,
            saved_image_id,
            deduplicated: true,
            replayed: true,
        }));
    }
    Ok(match find_blob(db, &request.sha256)? {
        Some(blob_id) => Precheck::BlobExists { blob_id },
        None => Precheck::NeedUpload,
    })
}

/// Writes one save action. Runs inside the caller's write transaction; any error aborts all of it.
/// `original` (already checked by `validate_original`) may be None only when the blob is stored.
fn commit_save(
    db: &Connection,
    request: &SaveRequest,
    original: Option<(&[u8], ImageFormat)>,
    thumbnail: Option<&ThumbnailInput>,
    now_ms: i64,
) -> Result<SaveOutcome, ImageError> {
    ensure_schema(db)?;
    if let Some((saved_image_id, blob_id, sha256, incognito)) = find_save(db, &request.save_id)? {
        if sha256 != request.sha256 || incognito != request.incognito {
            return Err(ImageError::SaveIdConflict);
        }
        return Ok(SaveOutcome {
            blob_id,
            saved_image_id,
            deduplicated: true,
            replayed: true,
        });
    }
    let (blob_id, deduplicated) = match find_blob(db, &request.sha256)? {
        Some(blob_id) => {
            db.execute(
                "UPDATE image_blobs SET last_saved_at = max(last_saved_at, ?2), save_count = save_count + 1 WHERE id = ?1",
                params![blob_id, now_ms],
            )?;
            (blob_id, true)
        }
        None => {
            let (bytes, format) = original.ok_or(ImageError::BlobMissing)?;
            db.execute(
                "INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, save_count, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1, ?7)",
                params![request.sha256, format.mime(), bytes.len() as i64, request.width, request.height, now_ms, bytes],
            )?;
            (db.last_insert_rowid(), false)
        }
    };
    db.execute(
        "INSERT INTO saved_images (save_id, blob_id, image_url, page_url, frame_url, saved_at, incognito) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![request.save_id, blob_id, request.image_url, request.page_url, request.frame_url, now_ms, request.incognito],
    )?;
    let saved_image_id = db.last_insert_rowid();
    if let Some(thumbnail) = thumbnail {
        // A thumbnail is derived data: keep an existing one unless this recipe is newer.
        db.execute(
            "INSERT INTO image_thumbnails (blob_id, mime_type, width, height, generator, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (blob_id) DO UPDATE SET mime_type = excluded.mime_type, width = excluded.width, height = excluded.height, generator = excluded.generator, data = excluded.data
             WHERE excluded.generator > image_thumbnails.generator",
            params![blob_id, thumbnail.mime_type, thumbnail.width, thumbnail.height, thumbnail.generator, thumbnail.data],
        )?;
    }
    Ok(SaveOutcome {
        blob_id,
        saved_image_id,
        deduplicated,
        replayed: false,
    })
}

/// Validates outside the write lock, then commits only into the vault named by `ticket`.
pub fn save_image(
    session: &mut Session,
    ticket: &VaultTicket,
    request: &SaveRequest,
    original: Option<&[u8]>,
    thumbnail: Option<&ThumbnailInput>,
    now_ms: i64,
) -> Result<SaveOutcome, ImageError> {
    // Hashing and format checks run before the write lock is taken.
    let original = match original {
        Some(bytes) => Some((bytes, validate_original(request, bytes)?)),
        None => {
            validate_request(request)?;
            None
        }
    };
    if let Some(thumbnail) = thumbnail {
        validate_thumbnail(thumbnail)?;
    }
    session.write_for(ticket, |db| {
        commit_save(db, request, original, thumbnail, now_ms)
    })
}

// --- Reading -----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredThumbnail {
    pub mime_type: String,
    pub width: i64,
    pub height: i64,
    pub data: Vec<u8>,
}

/// The stored thumbnail of one image, re-checked before it leaves the host: only PNG, JPEG or WebP
/// whose bytes match the stored type are returned. Anything else (or none) is reported as missing.
pub fn thumbnail(db: &Connection, blob_id: i64) -> Result<Option<StoredThumbnail>, ImageError> {
    if !readable(db)? {
        return Ok(None);
    }
    let row = db
        .query_row(
            "SELECT mime_type, width, height, data FROM image_thumbnails WHERE blob_id = ?1",
            [blob_id],
            |row| {
                Ok(StoredThumbnail {
                    mime_type: row.get(0)?,
                    width: row.get(1)?,
                    height: row.get(2)?,
                    data: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row.filter(|t| {
        t.data.len() <= MAX_THUMBNAIL_BYTES
            && matches!(sniff(&t.data), Ok(f @ (ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Webp)) if f.mime() == t.mime_type)
    }))
}

// --- Listing -----------------------------------------------------------------------------------

/// Keyset position: the last item of the previous page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCursor {
    pub last_saved_at: i64,
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSummary {
    pub blob_id: i64,
    pub sha256: String,
    pub mime_type: String,
    pub byte_length: i64,
    pub width: i64,
    pub height: i64,
    pub created_at: i64,
    pub last_saved_at: i64,
    pub save_count: i64,
    pub has_thumbnail: bool,
    pub latest_image_url: String,
    pub latest_page_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<ImageCursor>,
}

// Never selects `data`; `image_blobs_recent` serves the order and the keyset condition.
const LIST_SQL: &str = "
SELECT b.id, b.sha256, b.mime_type, b.byte_length, b.width, b.height, b.created_at, b.last_saved_at,
       b.save_count, t.blob_id IS NOT NULL, s.image_url, s.page_url
FROM image_blobs b
LEFT JOIN image_thumbnails t ON t.blob_id = b.id
LEFT JOIN saved_images s ON s.id = (
  SELECT id FROM saved_images WHERE blob_id = b.id ORDER BY saved_at DESC, id DESC LIMIT 1
)
WHERE ?1 IS NULL OR (b.last_saved_at, b.id) < (?1, ?2)
ORDER BY b.last_saved_at DESC, b.id DESC
LIMIT ?3
";

fn page_limit(limit: u32) -> u32 {
    limit.clamp(1, MAX_PAGE_SIZE)
}

/// Distinct images, most recently saved first. Keyset paging keeps pages stable while new images
/// arrive and costs the same at any depth.
pub fn list_images(
    db: &Connection,
    after: Option<ImageCursor>,
    limit: u32,
) -> Result<Page<ImageSummary>, ImageError> {
    if !readable(db)? {
        return Ok(Page {
            items: vec![],
            next: None,
        });
    }
    let limit = page_limit(limit);
    let mut statement = db.prepare_cached(LIST_SQL)?;
    let rows = statement.query_map(
        params![
            after.map(|c| c.last_saved_at),
            after.map(|c| c.id),
            limit + 1
        ],
        |row| {
            Ok(ImageSummary {
                blob_id: row.get(0)?,
                sha256: row.get(1)?,
                mime_type: row.get(2)?,
                byte_length: row.get(3)?,
                width: row.get(4)?,
                height: row.get(5)?,
                created_at: row.get(6)?,
                last_saved_at: row.get(7)?,
                save_count: row.get(8)?,
                has_thumbnail: row.get(9)?,
                latest_image_url: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
                latest_page_url: row.get(11)?,
            })
        },
    )?;
    let mut items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let next = if items.len() > limit as usize {
        items.truncate(limit as usize);
        items.last().map(|last| ImageCursor {
            last_saved_at: last.last_saved_at,
            id: last.blob_id,
        })
    } else {
        None
    };
    Ok(Page { items, next })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSource {
    pub saved_image_id: i64,
    pub image_url: String,
    pub page_url: Option<String>,
    pub frame_url: Option<String>,
    pub saved_at: i64,
    pub incognito: bool,
}

/// Save history of one image, newest first, keyset paged on (saved_at, id).
pub fn image_sources(
    db: &Connection,
    blob_id: i64,
    after: Option<ImageCursor>,
    limit: u32,
) -> Result<Page<ImageSource>, ImageError> {
    if !readable(db)? {
        return Ok(Page {
            items: vec![],
            next: None,
        });
    }
    let limit = page_limit(limit);
    let mut statement = db.prepare_cached(
        "SELECT id, image_url, page_url, frame_url, saved_at, incognito FROM saved_images
         WHERE blob_id = ?1 AND (?2 IS NULL OR (saved_at, id) < (?2, ?3))
         ORDER BY saved_at DESC, id DESC LIMIT ?4",
    )?;
    let rows = statement.query_map(
        params![
            blob_id,
            after.map(|c| c.last_saved_at),
            after.map(|c| c.id),
            limit + 1
        ],
        |row| {
            Ok(ImageSource {
                saved_image_id: row.get(0)?,
                image_url: row.get(1)?,
                page_url: row.get(2)?,
                frame_url: row.get(3)?,
                saved_at: row.get(4)?,
                incognito: row.get(5)?,
            })
        },
    )?;
    let mut items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let next = if items.len() > limit as usize {
        items.truncate(limit as usize);
        items.last().map(|last| ImageCursor {
            last_saved_at: last.saved_at,
            id: last.saved_image_id,
        })
    } else {
        None
    };
    Ok(Page { items, next })
}

// --- Originals (read-only) and regenerated thumbnails ---------------------------------------

/// What is known about a stored original without reading its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginalInfo {
    pub sha256: String,
    pub mime_type: String,
    pub byte_length: i64,
    pub last_saved_at: i64,
}

/// The original `blob_id` refers to, provided it still has the expected SHA-256. A blob id from
/// another vault (or a stale list) that now names different bytes is reported as missing.
pub fn original_info(
    db: &Connection,
    blob_id: i64,
    sha256: &str,
) -> Result<OriginalInfo, ImageError> {
    if !readable(db)? {
        return Err(ImageError::BlobMissing);
    }
    let info = db
        .query_row(
            "SELECT sha256, mime_type, byte_length, last_saved_at FROM image_blobs WHERE id = ?1",
            [blob_id],
            |row| {
                Ok(OriginalInfo {
                    sha256: row.get(0)?,
                    mime_type: row.get(1)?,
                    byte_length: row.get(2)?,
                    last_saved_at: row.get(3)?,
                })
            },
        )
        .optional()?
        .filter(|info| info.sha256 == sha256)
        .ok_or(ImageError::BlobMissing)?;
    if info.byte_length <= 0 || info.byte_length > MAX_IMAGE_BYTES as i64 {
        return Err(ImageError::Db);
    }
    Ok(info)
}

/// Reads `length` bytes of an original from `offset`, through SQLite's incremental BLOB I/O
/// (opened read-only), so a chunk never loads the whole image into memory.
pub fn read_original_range(
    db: &Connection,
    blob_id: i64,
    offset: usize,
    length: usize,
) -> Result<Vec<u8>, ImageError> {
    let blob = db.blob_open(
        rusqlite::DatabaseName::Main,
        "image_blobs",
        "data",
        blob_id,
        true,
    )?;
    if offset
        .checked_add(length)
        .is_none_or(|end| end > blob.len())
    {
        return Err(ImageError::LengthMismatch);
    }
    let mut buffer = vec![0u8; length];
    blob.read_at_exact(&mut buffer, offset)?;
    Ok(buffer)
}

/// The whole original, checked against its recorded size and SHA-256 before it is used.
pub fn read_original(
    db: &Connection,
    blob_id: i64,
    info: &OriginalInfo,
) -> Result<Vec<u8>, ImageError> {
    let bytes = read_original_range(db, blob_id, 0, info.byte_length as usize)?;
    if sha256_hex(&bytes) != info.sha256 {
        return Err(ImageError::HashMismatch);
    }
    Ok(bytes)
}

/// Adds a thumbnail rebuilt from the original, only if the image has none. Nothing else is
/// written: the original, its save history, count and dates stay as they are. Returns whether the
/// thumbnail was stored (false: one already exists, e.g. rebuilt concurrently).
pub fn add_thumbnail(
    session: &mut Session,
    ticket: &VaultTicket,
    blob_id: i64,
    sha256: &str,
    thumbnail: &ThumbnailInput,
) -> Result<bool, ImageError> {
    validate_thumbnail(thumbnail)?;
    session.write_for(ticket, |db| {
        original_info(db, blob_id, sha256)?;
        let added = db.execute(
            "INSERT INTO image_thumbnails (blob_id, mime_type, width, height, generator, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (blob_id) DO NOTHING",
            params![blob_id, thumbnail.mime_type, thumbnail.width, thumbnail.height, thumbnail.generator, thumbnail.data],
        )?;
        Ok(added == 1)
    })
}

#[cfg(test)]
pub(crate) mod tests;
