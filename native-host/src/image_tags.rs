//! Tags the user gives stored images afterwards (Issue #44, Slice 1: storage and host messages).
//!
//! ```text
//! → image_tags_get    { blobId }
//! ← image_tags_get_result    { vaultToken, blobId, tags }
//! → image_tag_add     { blobId, sha256, vaultToken, tag }
//! ← image_tag_add_result     { vaultToken, blobId, tag, added, tags }      added=false: it had it
//! → image_tag_remove  { blobId, sha256, vaultToken, tag }
//! ← image_tag_remove_result  { vaultToken, blobId, tag, removed, tags }    removed=false: it had not
//! → image_tag_list    { cursor?: string | null, limit?: 1..=200 }
//! ← image_tag_list_result    { vaultToken, items: [{ name, imageCount }], next: string | null }
//! → image_tag_search  { tag, cursor?: { lastSavedAt, id } | null, limit?: 1..=60 }
//! ← image_tag_search_result  { vaultToken, tag, items: [image_list item], next }
//! ```
//!
//! Every message also carries `requestId`, and every reply `ok`, `type` and `requestId`; a failure
//! is `{ ok: false, error }` and has changed nothing. `tag` in a reply is the normalized name
//! (`normalize_tag`), and `tags` is the image's tags after the request, ordered by name.
//!
//! A tag belongs to the image (`image_blobs`, the `blobId` of `image_list`), not to one save action.
//! Writes name the vault (`vaultToken`) and the bytes (`sha256`) they mean, like `image_read`: a
//! request for another vault fails with `vault_switched`, and a blob id that is unknown or now names
//! other bytes with `blob_missing`. An image has at most `MAX_TAGS_PER_IMAGE` tags.
//!
//! Tags are local: they hold no URL, nothing here reads the privacy policy, and MCP reads no tag. A
//! tag no image has any more is removed by the vault itself (`image_tags_drop_unused`), so the tag
//! list only ever shows tags in use.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::image_original::{target, ticket_for};
use crate::image_query::{cursor, limit, within_budget, RESPONSE_BUDGET};
use crate::images::{self, ImageCursor, ImageError, MAX_PAGE_SIZE};
use crate::search::request_id;
use crate::vault::Session;

/// Characters of a normalized tag (Unicode scalar values, as SQLite's `length`).
pub const MAX_TAG_CHARS: usize = 64;
/// A request's `tag` before normalization, in UTF-8 bytes: room for spacing, never unbounded work.
pub const MAX_TAG_INPUT_BYTES: usize = 1024;
pub const MAX_TAGS_PER_IMAGE: usize = 64;
pub const MAX_TAG_PAGE: u32 = 200;

/// Invisible characters that would make two tags look the same: soft hyphen, zero-width and
/// direction marks, word joiners, bidi embeddings and isolates, the BOM, interlinear annotations.
fn is_invisible_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}')
}

/// The stored form of a tag, or `InvalidTag`:
///
/// 1. Whitespace (Unicode `White_Space`: spaces, tabs, line breaks, the ideographic space U+3000)
///    is removed at both ends, and every run of it inside becomes one U+0020.
/// 2. Any other control character, or an invisible format character (`is_invisible_format`), is
///    refused rather than removed, so what the user typed is never silently changed.
/// 3. Letters are lowercased (Unicode, `str::to_lowercase`): "Cat", "CAT" and "cat" are one tag.
/// 4. The result has 1 to `MAX_TAG_CHARS` characters.
///
/// Nothing else is changed: no Unicode normalization (NFC/NFKC), so full-width and half-width
/// forms, or kana and kanji, stay different tags. Every printable character is allowed.
pub fn normalize_tag(raw: &str) -> Result<String, ImageError> {
    if raw.len() > MAX_TAG_INPUT_BYTES {
        return Err(ImageError::InvalidTag);
    }
    let spaced = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if spaced
        .chars()
        .any(|c| c.is_control() || is_invisible_format(c))
    {
        return Err(ImageError::InvalidTag);
    }
    let name = spaced.to_lowercase();
    let chars = name.chars().count();
    if chars == 0 || chars > MAX_TAG_CHARS {
        return Err(ImageError::InvalidTag);
    }
    Ok(name)
}

/// The image `blob_id`, provided it still has the expected SHA-256.
fn require_image(db: &Connection, blob_id: i64, sha256: Option<&str>) -> Result<(), ImageError> {
    let stored = db
        .query_row(
            "SELECT sha256 FROM image_blobs WHERE id = ?1",
            [blob_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match stored {
        Some(stored) if sha256.is_none_or(|expected| expected == stored) => Ok(()),
        _ => Err(ImageError::BlobMissing),
    }
}

/// The tags of one image, ordered by name. An unknown image is `BlobMissing`.
pub fn tags_of(db: &Connection, blob_id: i64) -> Result<Vec<String>, ImageError> {
    require_image(db, blob_id, None)?;
    let mut statement = db.prepare_cached(
        "SELECT t.name FROM image_tag_links l JOIN image_tags t ON t.id = l.tag_id
         WHERE l.blob_id = ?1 ORDER BY t.name",
    )?;
    let names = statement
        .query_map([blob_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(names)
}

/// Gives the image the tag `name` (already normalized); false if it had it. In one transaction of
/// the caller: the tag row is created only together with its first link.
pub fn add_tag(
    db: &Connection,
    blob_id: i64,
    sha256: &str,
    name: &str,
) -> Result<bool, ImageError> {
    require_image(db, blob_id, Some(sha256))?;
    let (has, count): (bool, i64) = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM image_tag_links l JOIN image_tags t ON t.id = l.tag_id
                        WHERE l.blob_id = ?1 AND t.name = ?2),
                (SELECT count(*) FROM image_tag_links WHERE blob_id = ?1)",
        params![blob_id, name],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if has {
        return Ok(false);
    }
    if count >= MAX_TAGS_PER_IMAGE as i64 {
        return Err(ImageError::TooManyTags);
    }
    db.execute(
        "INSERT INTO image_tags (name) VALUES (?1) ON CONFLICT (name) DO NOTHING",
        [name],
    )?;
    db.execute(
        "INSERT INTO image_tag_links (blob_id, tag_id) SELECT ?1, id FROM image_tags WHERE name = ?2",
        params![blob_id, name],
    )?;
    Ok(true)
}

/// Takes the tag `name` (already normalized) from the image; false if it did not have it. A tag
/// that no image has afterwards is removed by the vault's trigger in the same transaction.
pub fn remove_tag(
    db: &Connection,
    blob_id: i64,
    sha256: &str,
    name: &str,
) -> Result<bool, ImageError> {
    require_image(db, blob_id, Some(sha256))?;
    let removed = db.execute(
        "DELETE FROM image_tag_links WHERE blob_id = ?1 AND tag_id = (SELECT id FROM image_tags WHERE name = ?2)",
        params![blob_id, name],
    )?;
    Ok(removed == 1)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagCount {
    pub name: String,
    pub image_count: i64,
}

/// Every tag in use with the number of images that have it, ordered by name (SQLite's binary
/// order), keyset paged on the name.
pub fn list_tags(
    db: &Connection,
    after: Option<&str>,
    limit: u32,
) -> Result<(Vec<TagCount>, Option<String>), ImageError> {
    let limit = limit.clamp(1, MAX_TAG_PAGE);
    let mut statement = db.prepare_cached(
        "SELECT t.name, (SELECT count(*) FROM image_tag_links l WHERE l.tag_id = t.id)
         FROM image_tags t WHERE ?1 IS NULL OR t.name > ?1 ORDER BY t.name LIMIT ?2",
    )?;
    let mut items = statement
        .query_map(params![after, limit + 1], |row| {
            Ok(TagCount {
                name: row.get(0)?,
                image_count: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next = if items.len() > limit as usize {
        items.truncate(limit as usize);
        items.last().map(|last| last.name.clone())
    } else {
        None
    };
    Ok((items, next))
}

// The columns and order of `images::LIST_SQL`, for the images that have one tag.
const SEARCH_SQL: &str = "
SELECT b.id, b.sha256, b.mime_type, b.byte_length, b.width, b.height, b.created_at, b.last_saved_at,
       b.save_count, t.blob_id IS NOT NULL, s.image_url, s.page_url
FROM image_tag_links l
JOIN image_blobs b ON b.id = l.blob_id
LEFT JOIN image_thumbnails t ON t.blob_id = b.id
LEFT JOIN saved_images s ON s.id = (
  SELECT id FROM saved_images WHERE blob_id = b.id ORDER BY saved_at DESC, id DESC LIMIT 1
)
WHERE l.tag_id = (SELECT id FROM image_tags WHERE name = ?4)
  AND (?1 IS NULL OR (b.last_saved_at, b.id) < (?1, ?2))
ORDER BY b.last_saved_at DESC, b.id DESC
LIMIT ?3
";

/// The images with the tag `name` (already normalized), as `image_list` gives them: most recently
/// saved first, keyset paged the same way. An unknown tag has no images.
pub fn search_by_tag(
    db: &Connection,
    name: &str,
    after: Option<ImageCursor>,
    limit: u32,
) -> Result<images::Page<images::ImageSummary>, ImageError> {
    let limit = images::page_limit(limit);
    let mut statement = db.prepare_cached(SEARCH_SQL)?;
    let items = statement
        .query_map(
            params![
                after.map(|c| c.last_saved_at),
                after.map(|c| c.id),
                limit + 1,
                name
            ],
            images::summary_of,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(images::summary_page(items, limit))
}

// --- Messages ----------------------------------------------------------------------------------

fn result_type(kind: &str) -> &'static str {
    match kind {
        "image_tags_get" => "image_tags_get_result",
        "image_tag_add" => "image_tag_add_result",
        "image_tag_remove" => "image_tag_remove_result",
        "image_tag_list" => "image_tag_list_result",
        _ => "image_tag_search_result",
    }
}

pub fn handle(session: &mut Session, message: &Value) -> Value {
    let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
    let id = request_id(message);
    let mut reply = match id {
        None => Err("invalid_request_id"),
        Some(_) => answer(session, kind, message),
    }
    .unwrap_or_else(|code| {
        let mut fields = Map::new();
        fields.insert("error".into(), code.into());
        fields
    });
    let ok = !reply.contains_key("error");
    reply.insert("ok".into(), ok.into());
    reply.insert("type".into(), result_type(kind).into());
    reply.insert("requestId".into(), id.into());
    Value::Object(reply)
}

fn tag_of(message: &Value) -> Result<String, &'static str> {
    let raw = message
        .get("tag")
        .and_then(Value::as_str)
        .ok_or("invalid_tag")?;
    normalize_tag(raw).map_err(ImageError::code)
}

fn tag_cursor(message: &Value) -> Result<Option<String>, &'static str> {
    match message.get("cursor") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(name)) if normalize_tag(name).as_deref() == Ok(name.as_str()) => {
            Ok(Some(name.clone()))
        }
        Some(_) => Err("invalid_cursor"),
    }
}

fn answer(
    session: &mut Session,
    kind: &str,
    message: &Value,
) -> Result<Map<String, Value>, &'static str> {
    let mut fields = Map::new();
    match kind {
        "image_tag_add" | "image_tag_remove" => {
            let target = target(message)?;
            let name = tag_of(message)?;
            let ticket = ticket_for(session, message)?;
            let adding = kind == "image_tag_add";
            let (changed, tags) = session
                .write_for(&ticket, |db| {
                    let changed = if adding {
                        add_tag(db, target.blob_id, &target.sha256, &name)?
                    } else {
                        remove_tag(db, target.blob_id, &target.sha256, &name)?
                    };
                    Ok::<_, ImageError>((changed, tags_of(db, target.blob_id)?))
                })
                .map_err(ImageError::code)?;
            fields.insert("vaultToken".into(), ticket.token.into());
            fields.insert("blobId".into(), target.blob_id.into());
            fields.insert("tag".into(), name.into());
            fields.insert(
                if adding { "added" } else { "removed" }.into(),
                changed.into(),
            );
            fields.insert("tags".into(), json!(tags));
        }
        "image_tags_get" => {
            let blob_id = message
                .get("blobId")
                .and_then(Value::as_i64)
                .filter(|id| *id > 0)
                .ok_or("invalid_blob_id")?;
            let (token, tags) = session
                .read_with_token(|db| tags_of(db, blob_id))
                .map_err(|error| error.code())?;
            let tags = tags.map_err(ImageError::code)?;
            fields.insert("vaultToken".into(), token.into());
            fields.insert("blobId".into(), blob_id.into());
            fields.insert("tags".into(), json!(tags));
        }
        "image_tag_list" => {
            let after = tag_cursor(message)?;
            let limit = limit(message, MAX_TAG_PAGE)?;
            let (token, page) = session
                .read_with_token(|db| list_tags(db, after.as_deref(), limit))
                .map_err(|error| error.code())?;
            let (items, next) = page.map_err(ImageError::code)?;
            fields.insert("vaultToken".into(), token.into());
            fields.insert("items".into(), json!(items));
            fields.insert("next".into(), json!(next));
        }
        "image_tag_search" => {
            let name = tag_of(message)?;
            let after = cursor(message)?;
            let limit = limit(message, MAX_PAGE_SIZE)?;
            let (token, page) = session
                .read_with_token(|db| search_by_tag(db, &name, after, limit))
                .map_err(|error| error.code())?;
            let page = page.map_err(ImageError::code)?;
            let (items, cut) = within_budget(page.items, RESPONSE_BUDGET - 4096);
            let next = if cut {
                items.last().map(|last| ImageCursor {
                    last_saved_at: last.last_saved_at,
                    id: last.blob_id,
                })
            } else {
                page.next
            };
            fields.insert("vaultToken".into(), token.into());
            fields.insert("tag".into(), name.into());
            fields.insert("items".into(), json!(items));
            fields.insert("next".into(), json!(next));
        }
        _ => return Err("unknown_type"),
    }
    Ok(fields)
}

#[cfg(test)]
mod tests;
