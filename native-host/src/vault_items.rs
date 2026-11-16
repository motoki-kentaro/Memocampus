//! The deletion cores of one stored URL and one stored image (Issue #43), used by `item_delete`
//! (Issue #7), which is the only message path to them: a preview fixes the exact set, a review id
//! confirms it once, on one lasting port. The one-shot `url_delete*` / `image_delete*` messages of
//! Issue #43 no longer exist.
//!
//! **A URL** is a row of `search_pages`: exactly that `url` text, in `visits` and `tab_snapshots`.
//! Deleting it removes every visit and tab snapshot of it, its page metadata, and the navigation of
//! each of its visits (`ON DELETE CASCADE`). A navigation of another URL that came from one of its
//! visits keeps its row with no source (`ON DELETE SET NULL`); no other visit is joined to it. No
//! other URL and no image is deleted: images saved from the page stay (`imageSavesKept`). The policy
//! is not read or changed, so the URL is recorded again on the next visit if the policy allows it.
//!
//! **An image** is an item of `image_list`: the distinct bytes (`blobId`, `sha256`). Deleting it
//! removes every save of it (`saved_images`: its save history), its bytes (`image_blobs`), its
//! thumbnail and its tag links (`ON DELETE CASCADE`), and each of its tags no other image has (the
//! vault's trigger `image_tags_drop_unused`). No visit, URL or other image is touched.
//!
//! Nothing goes to a trash: the rows are gone. Backups are separate files and are not touched. The
//! file does not shrink (no VACUUM).

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::images::ImageError;

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
    // A navigation of another URL that came from one of these visits stays, and says its source was
    // deleted on purpose (`source_censored`, Issue #7) instead of looking like one that was never known.
    // The source is cleared here, in the same statement, so the table's CHECK holds (SET NULL would too).
    let censored = db.execute(
        "UPDATE navigation_events SET source_visit_id = NULL, source_censored = 1
          WHERE source_visit_id IN (SELECT event_id FROM visits WHERE url = ?1)
            AND target_visit_id NOT IN (SELECT event_id FROM visits WHERE url = ?1)",
        [url],
    )? as i64;
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
        || censored != counts.navigation_sources_cleared
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

#[cfg(test)]
mod tests;
