//! Removing one stored URL, or one stored image, from the active vault (Issue #43, Slice 1).
//!
//! ```text
//! → url_delete_preview   { url }
//! ← url_delete_preview_result   { vaultToken, url, counts, irreversible: true }
//! → url_delete           { url, vaultToken }
//! ← url_delete_result           { vaultToken, url, deleted: counts, irreversible: true }
//! → image_delete_preview { blobId, sha256 }
//! ← image_delete_preview_result { vaultToken, blobId, sha256, byteLength, counts, irreversible: true }
//! → image_delete         { blobId, sha256, vaultToken }
//! ← image_delete_result         { vaultToken, blobId, sha256, deleted: counts, irreversible: true }
//! ```
//!
//! Every message also carries `requestId`, and every reply `ok`, `type` and `requestId`; a failure
//! is `{ ok: false, error }` and has changed nothing. The two deletions are one transaction each,
//! committed before the reply is written: there is no success without the commit, and any error
//! rolls the whole of it back. Nothing goes to a trash: the rows are gone (`irreversible`). Backups
//! are separate files and are not touched. The file does not shrink (no VACUUM).
//!
//! **A URL** is a row of `search_pages`: exactly that `url` text, in `visits` and `tab_snapshots`.
//! Deleting it removes every visit and tab snapshot of it, its page metadata, and the navigation of
//! each of its visits (`ON DELETE CASCADE`). A navigation of another URL that came from one of its
//! visits keeps its row with no source (`ON DELETE SET NULL`); no other visit is joined to it. No
//! other URL and no image is deleted: images saved from the page stay (`imageSavesKept`). The policy
//! is not read or changed, so the URL is recorded again on the next visit if the policy allows it.
//!
//! `counts` (preview) and `deleted` are `{ visits, tabSnapshots, pageMetadata, navigations,
//! navigationSourcesCleared, imageSavesKept }`. The preview and the deletion name the same thing
//! (the URL text and the vault); a deletion of a URL with nothing stored is a no-op of zero counts.
//!
//! **An image** is an item of `image_list`: the distinct bytes (`blobId`, `sha256`). Deleting it
//! removes every save of it (`saved_images`: its save history), its bytes (`image_blobs`), its
//! thumbnail and its tag links (`ON DELETE CASCADE`), and each of its tags no other image has (the
//! vault's trigger `image_tags_drop_unused`). No visit, URL or other image is touched.
//! `counts` and `deleted` are `{ saves, thumbnails, tagLinks, tagsRemoved }`.
//!
//! Writes name the vault (`vaultToken`, as given by the preview or `image_list`): a request for
//! another vault fails with `vault_switched`, checked again under the write lock. An image whose id
//! is unknown or now names other bytes fails with `blob_missing`, as for tags.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::image_original::{target, ticket_for};
use crate::images::ImageError;
use crate::search::request_id;
use crate::vault::Session;

/// As the policy engine's limit on URLs; a stored URL is never longer.
pub const MAX_URL_CHARS: usize = 32_768;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlCounts {
    pub visits: i64,
    pub tab_snapshots: i64,
    pub page_metadata: i64,
    /// Navigation rows of the URL's visits, removed with them.
    pub navigations: i64,
    /// Navigation rows of other visits whose source was one of the URL's visits: kept, source NULL.
    pub navigation_sources_cleared: i64,
    /// Saves of images whose image, page or frame URL is this URL: never deleted here.
    pub image_saves_kept: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCounts {
    pub saves: i64,
    pub thumbnails: i64,
    pub tag_links: i64,
    /// Tags of this image that no other image has: the vault removes them with it.
    pub tags_removed: i64,
}

const URL_COUNTS: &str = "
WITH gone(event_id) AS (SELECT event_id FROM visits WHERE url = ?1)
SELECT
  (SELECT count(*) FROM gone),
  (SELECT count(*) FROM tab_snapshots WHERE url = ?1),
  (SELECT count(*) FROM page_metadata WHERE url = ?1 OR event_id IN gone),
  (SELECT count(*) FROM navigation_events WHERE target_visit_id IN gone),
  (SELECT count(*) FROM navigation_events
    WHERE source_visit_id IN gone AND target_visit_id NOT IN gone),
  (SELECT count(*) FROM saved_images WHERE image_url = ?1 OR page_url = ?1 OR frame_url = ?1)";

pub fn url_counts(db: &Connection, url: &str) -> rusqlite::Result<UrlCounts> {
    db.query_row(URL_COUNTS, [url], |row| {
        Ok(UrlCounts {
            visits: row.get(0)?,
            tab_snapshots: row.get(1)?,
            page_metadata: row.get(2)?,
            navigations: row.get(3)?,
            navigation_sources_cleared: row.get(4)?,
            image_saves_kept: row.get(5)?,
        })
    })
}

/// Deletes everything stored of `url`, in the caller's transaction; what was deleted. Every
/// statement names the URL as a parameter. The counts are taken in the same transaction, and the
/// result is checked: if anything of the URL is left, the whole deletion fails (and rolls back).
pub fn delete_url(db: &Connection, url: &str) -> Result<UrlCounts, ImageError> {
    let counts = url_counts(db, url)?;
    // The metadata row goes with its visit (CASCADE); a row of this URL is removed in any case.
    db.execute("DELETE FROM page_metadata WHERE url = ?1", [url])?;
    let visits = db.execute("DELETE FROM visits WHERE url = ?1", [url])?;
    let snapshots = db.execute("DELETE FROM tab_snapshots WHERE url = ?1", [url])?;
    let left = url_counts(db, url)?;
    let expected_left = UrlCounts {
        image_saves_kept: counts.image_saves_kept,
        ..UrlCounts::default()
    };
    if visits as i64 != counts.visits
        || snapshots as i64 != counts.tab_snapshots
        || left != expected_left
    {
        return Err(ImageError::Db);
    }
    Ok(counts)
}

/// The image `blob_id` with the bytes `sha256`, and what deleting it would delete.
pub fn image_counts(
    db: &Connection,
    blob_id: i64,
    sha256: &str,
) -> Result<(i64, ImageCounts), ImageError> {
    db.query_row(
        "SELECT byte_length,
           (SELECT count(*) FROM saved_images WHERE blob_id = b.id),
           (SELECT count(*) FROM image_thumbnails WHERE blob_id = b.id),
           (SELECT count(*) FROM image_tag_links WHERE blob_id = b.id),
           (SELECT count(*) FROM image_tag_links l WHERE l.blob_id = b.id
              AND NOT EXISTS (SELECT 1 FROM image_tag_links o WHERE o.tag_id = l.tag_id AND o.blob_id <> b.id))
         FROM image_blobs b WHERE b.id = ?1 AND b.sha256 = ?2",
        params![blob_id, sha256],
        |row| {
            Ok((
                row.get(0)?,
                ImageCounts {
                    saves: row.get(1)?,
                    thumbnails: row.get(2)?,
                    tag_links: row.get(3)?,
                    tags_removed: row.get(4)?,
                },
            ))
        },
    )
    .optional()?
    .ok_or(ImageError::BlobMissing)
}

/// Deletes the image `blob_id` if it still has the bytes `sha256`, in the caller's transaction.
pub fn delete_image(
    db: &Connection,
    blob_id: i64,
    sha256: &str,
) -> Result<ImageCounts, ImageError> {
    let (_, counts) = image_counts(db, blob_id, sha256)?;
    let tags_before: i64 = db.query_row("SELECT count(*) FROM image_tags", [], |row| row.get(0))?;
    let saves = db.execute("DELETE FROM saved_images WHERE blob_id = ?1", [blob_id])?;
    // The thumbnail and the tag links go with the bytes (CASCADE), unused tags by the trigger.
    let blobs = db.execute(
        "DELETE FROM image_blobs WHERE id = ?1 AND sha256 = ?2",
        params![blob_id, sha256],
    )?;
    let (left, tags_after): (i64, i64) = db.query_row(
        "SELECT (SELECT count(*) FROM saved_images WHERE blob_id = ?1)
              + (SELECT count(*) FROM image_thumbnails WHERE blob_id = ?1)
              + (SELECT count(*) FROM image_tag_links WHERE blob_id = ?1),
                (SELECT count(*) FROM image_tags)",
        [blob_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if saves as i64 != counts.saves
        || blobs != 1
        || left != 0
        || tags_before - tags_after != counts.tags_removed
    {
        return Err(ImageError::Db);
    }
    Ok(counts)
}

fn url_of(message: &Value) -> Result<&str, &'static str> {
    message
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty() && url.encode_utf16().count() <= MAX_URL_CHARS)
        .ok_or("invalid_url")
}

fn result_type(kind: &str) -> &'static str {
    match kind {
        "url_delete_preview" => "url_delete_preview_result",
        "url_delete" => "url_delete_result",
        "image_delete_preview" => "image_delete_preview_result",
        _ => "image_delete_result",
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

fn answer(
    session: &mut Session,
    kind: &str,
    message: &Value,
) -> Result<Map<String, Value>, &'static str> {
    let mut fields = Map::new();
    match kind {
        "url_delete_preview" => {
            let url = url_of(message)?;
            // The counts and the token that a delete must name come from one vault (one resolution).
            let (token, counts) = session
                .read_with_token(|db| url_counts(db, url))
                .map_err(|error| error.code())?;
            let counts = counts.map_err(|_| "db_error")?;
            fields.insert("vaultToken".into(), token.into());
            fields.insert("url".into(), url.into());
            fields.insert("counts".into(), json!(counts));
        }
        "url_delete" => {
            let url = url_of(message)?;
            let ticket = ticket_for(session, message)?;
            let deleted = session
                .write_for(&ticket, |db| delete_url(db, url))
                .map_err(ImageError::code)?;
            fields.insert("vaultToken".into(), ticket.token.into());
            fields.insert("url".into(), url.into());
            fields.insert("deleted".into(), json!(deleted));
        }
        "image_delete_preview" => {
            let target = target(message)?;
            let (token, preview) = session
                .read_with_token(|db| image_counts(db, target.blob_id, &target.sha256))
                .map_err(|error| error.code())?;
            let (byte_length, counts) = preview.map_err(ImageError::code)?;
            fields.insert("vaultToken".into(), token.into());
            fields.insert("blobId".into(), target.blob_id.into());
            fields.insert("sha256".into(), target.sha256.into());
            fields.insert("byteLength".into(), byte_length.into());
            fields.insert("counts".into(), json!(counts));
        }
        "image_delete" => {
            let target = target(message)?;
            let ticket = ticket_for(session, message)?;
            let deleted = session
                .write_for(&ticket, |db| {
                    delete_image(db, target.blob_id, &target.sha256)
                })
                .map_err(ImageError::code)?;
            fields.insert("vaultToken".into(), ticket.token.into());
            fields.insert("blobId".into(), target.blob_id.into());
            fields.insert("sha256".into(), target.sha256.into());
            fields.insert("deleted".into(), json!(deleted));
        }
        _ => return Err("unknown_type"),
    }
    fields.insert("irreversible".into(), true.into());
    Ok(fields)
}

#[cfg(test)]
mod tests;
