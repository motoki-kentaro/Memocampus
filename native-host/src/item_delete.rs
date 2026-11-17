//! Issue #7: deleting stored visits, URLs and images from the active vault, with the guarantees of
//! the Page deletion (Issue #6): a preview fixes the exact set, one unguessable review id confirms
//! it once, and the delete runs only if that very set is still what it was.
//!
//! One `connectNative` port is one Host process; everything below lives and dies with it. A restart
//! or another port cannot confirm an old preview, and nothing is resent by the Host.
//!
//! ```text
//! → item_state            { requestId }                                  ← vaultToken, generation
//! → visit_history         { url, query?, visitedFrom?, visitedTo?, after?: {visitedAt, id}, limit? }
//!                         ← { items: [{ id, title, visitedAt }], next, counts: { matched, total, snapshots } }
//! → item_delete_preview   { target }                                     ← reviewId, target, counts, detail?
//! → item_delete           { reviewId }                                   ← operationId, deleted
//! → item_delete_status    { operationId }                                ← confirmed
//! ```
//!
//! Every message after `item_state` carries the `vaultToken` and the `generation` (the vault
//! selection, `selection_generation`) it was made for. Targets:
//!
//! | `scope` | Deletes |
//! |---|---|
//! | `visit` `{id}` | one `visits` row (`event_id`) |
//! | `visits` `{query, visitedFrom?, visitedTo?}` | every visit `search_pages` would match (see `VISIT_MATCH`); never tab snapshots |
//! | `url` `{url}` | everything of one URL text, as `vault_items::delete_url` |
//! | `image` `{blobId, sha256}` | one image (distinct bytes), its saves, thumbnail, tag links |
//! | `images` `{tag?}` | every image, or every image with that tag (as `image_tag_search`) |
//!
//! A deleted visit takes its navigation row and, if it carried the URL's page metadata, that row
//! (`ON DELETE CASCADE`); navigations that came from it keep their row with no source. Tab snapshots
//! are a separate kind of record: only `url` deletes them.
//!
//! No `page_blobs.data` or `image_blobs.data` is read here. No VACUUM.
use crate::{
    image_tags::normalize_tag,
    images::ImageError,
    page_protocol,
    search::{like_pattern, parse_request, request_id},
    vault::{self, ConfigLock, Session},
    vault_items::{self, ImageCounts, UrlCounts},
    visit_days::VALID_VISITED_AT,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const REVIEW_TTL: Duration = Duration::from_secs(120);
const BUDGET: usize = 700_000;
const HISTORY_LIMIT: u64 = 20;
const HISTORY_MAX: u64 = 50;

/// The visits a search with this query and date range finds, one by one. It is `search_pages`'s
/// own row test (`url` or `title` LIKE the escaped query, and for a date range the same
/// `visited_at` window with the same validity test), applied to a `visits` row; tab snapshots are
/// not visits and are never part of it. `?4` is a single event id or NULL.
const VISIT_MATCH: &str = "(url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\') AND (?2 IS NULL OR (visited_at >= ?2 AND visited_at < ?3 AND VALID)) AND (?4 IS NULL OR event_id = ?4)";

fn visit_match() -> String {
    VISIT_MATCH.replace("VALID", VALID_VISITED_AT)
}

#[derive(Debug)]
struct Error(&'static str);
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self("db_error")
    }
}
impl From<vault::VaultError> for Error {
    fn from(e: vault::VaultError) -> Self {
        Self(e.code())
    }
}
impl From<ImageError> for Error {
    fn from(e: ImageError) -> Self {
        Self(e.code())
    }
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding {
    vault_token: String,
    generation: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Visit {
        id: String,
    },
    Visits {
        query: String,
        visited: Option<(String, String)>,
    },
    Url {
        url: String,
    },
    Image {
        blob_id: i64,
        sha256: String,
    },
    Images {
        tag: Option<String>,
    },
}

impl Target {
    fn checked(value: &Value) -> Result<Self> {
        let bad = || Error("invalid_target");
        let text = |key: &str| value.get(key).and_then(Value::as_str);
        match value.get("scope").and_then(Value::as_str) {
            Some("visit") => {
                let id = text("id")
                    .filter(|id| !id.is_empty() && id.len() <= 256 && !id.contains('\0'))
                    .ok_or_else(bad)?;
                Ok(Self::Visit { id: id.into() })
            }
            Some("visits") => {
                let request = parse_request(&json!({
                    "query": value.get("query"),
                    "visitedFrom": value.get("visitedFrom"),
                    "visitedTo": value.get("visitedTo"),
                }))
                .map_err(|_| bad())?;
                Ok(Self::Visits {
                    query: request.query,
                    visited: request.visited,
                })
            }
            Some("url") => {
                let url = text("url")
                    .filter(|url| {
                        !url.is_empty() && url.encode_utf16().count() <= vault_items::MAX_URL_CHARS
                    })
                    .ok_or_else(bad)?;
                Ok(Self::Url { url: url.into() })
            }
            Some("image") => {
                let blob_id = value
                    .get("blobId")
                    .and_then(Value::as_i64)
                    .filter(|id| *id > 0)
                    .ok_or_else(bad)?;
                let sha256 = text("sha256")
                    .filter(|sha| {
                        sha.len() == 64
                            && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                    })
                    .ok_or_else(bad)?;
                Ok(Self::Image {
                    blob_id,
                    sha256: sha256.into(),
                })
            }
            Some("images") => {
                let tag = match value.get("tag") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(raw)) => Some(normalize_tag(raw).map_err(Error::from)?),
                    _ => return Err(bad()),
                };
                Ok(Self::Images { tag })
            }
            _ => Err(bad()),
        }
    }
    /// What the Chrome side shows and sends back for confirmation: the normalized target.
    fn shown(&self) -> Value {
        match self {
            Self::Visits { query, visited } => json!({
                "scope": "visits", "query": query,
                "visitedFrom": visited.as_ref().map(|v| &v.0), "visitedTo": visited.as_ref().map(|v| &v.1),
            }),
            Self::Visit { id } => json!({"scope": "visit", "id": id}),
            Self::Url { url } => json!({"scope": "url", "url": url}),
            Self::Image { blob_id, sha256 } => {
                json!({"scope": "image", "blobId": blob_id, "sha256": sha256})
            }
            Self::Images { tag } => json!({"scope": "images", "tag": tag}),
        }
    }
}

struct Review {
    id: String,
    target: Target,
    fingerprint: [u8; 32],
    counts: Value,
    issued: Instant,
}
struct Receipt {
    id: String,
    counts: Value,
    issued: Instant,
}
#[derive(Default)]
pub struct Manager {
    binding: Option<Binding>,
    review: Option<Review>,
    receipt: Option<Receipt>,
}

fn feed(digest: &mut Sha256, parts: &[&dyn std::fmt::Debug]) {
    let text = format!("{parts:?}");
    digest.update((text.len() as u64).to_le_bytes());
    digest.update(text.as_bytes());
}

fn visit_params(target: &Target) -> (String, Option<&str>, Option<&str>, Option<&str>) {
    match target {
        Target::Visit { id } => ("%".into(), None, None, Some(id)),
        Target::Visits { query, visited } => (
            like_pattern(query),
            visited.as_ref().map(|v| v.0.as_str()),
            visited.as_ref().map(|v| v.1.as_str()),
            None,
        ),
        _ => unreachable!("not a visit target"),
    }
}

fn visit_counts(db: &Connection, target: &Target) -> Result<Value> {
    let (pattern, from, to, id) = visit_params(target);
    let sql = format!(
        "WITH gone(event_id, url) AS (SELECT event_id, url FROM visits WHERE {})
         SELECT (SELECT count(*) FROM gone),
                (SELECT count(DISTINCT url) FROM gone),
                (SELECT count(*) FROM page_metadata WHERE event_id IN (SELECT event_id FROM gone)),
                (SELECT count(*) FROM navigation_events WHERE target_visit_id IN (SELECT event_id FROM gone)),
                (SELECT count(*) FROM navigation_events WHERE source_visit_id IN (SELECT event_id FROM gone)
                    AND target_visit_id NOT IN (SELECT event_id FROM gone)),
                (SELECT count(*) FROM tab_snapshots WHERE url IN (SELECT url FROM gone))",
        visit_match()
    );
    Ok(db.query_row(&sql, params![pattern, from, to, id], |r| {
        Ok(json!({
            "kind": "visits", "visits": r.get::<_, i64>(0)?, "urls": r.get::<_, i64>(1)?,
            "pageMetadata": r.get::<_, i64>(2)?, "navigations": r.get::<_, i64>(3)?,
            "navigationSourcesCleared": r.get::<_, i64>(4)?, "snapshotsKept": r.get::<_, i64>(5)?,
        }))
    })?)
}

/// The navigation records a deletion touches, as the fingerprint sees them: the ones that go with a
/// deleted visit ('k', the visit is their target) and the ones that stay with a censored source
/// ('m', the visit is their source, their target stays). `gone` selects the visits (its parameters follow).
fn feed_navigations(
    db: &Connection,
    digest: &mut Sha256,
    gone: &str,
    parameters: &[&dyn rusqlite::ToSql],
) -> Result<()> {
    let sql = format!(
        "WITH gone(event_id) AS ({gone})
         SELECT 'k', target_visit_id, coalesce(source_visit_id, ''), source_censored, kind, transition_type, occurred_at
           FROM navigation_events WHERE target_visit_id IN (SELECT event_id FROM gone)
         UNION ALL
         SELECT 'm', target_visit_id, coalesce(source_visit_id, ''), source_censored, kind, transition_type, occurred_at
           FROM navigation_events WHERE source_visit_id IN (SELECT event_id FROM gone)
            AND target_visit_id NOT IN (SELECT event_id FROM gone)
         ORDER BY 1, 2"
    );
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(parameters)?;
    while let Some(row) = rows.next()? {
        let line: (String, String, String, i64, String, String, String) = (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        );
        feed(digest, &[&"n", &line]);
    }
    Ok(())
}

fn visit_fingerprint(db: &Connection, target: &Target) -> Result<[u8; 32]> {
    let (pattern, from, to, id) = visit_params(target);
    let sql = format!(
        "SELECT event_id, url, title, visited_at FROM visits WHERE {} ORDER BY event_id",
        visit_match()
    );
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(params![pattern, from, to, id])?;
    let mut digest = Sha256::new();
    while let Some(row) = rows.next()? {
        let (a, b, c, d): (String, String, String, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        feed(&mut digest, &[&a, &b, &c, &d]);
    }
    drop(rows);
    drop(statement);
    feed_navigations(
        db,
        &mut digest,
        &format!("SELECT event_id FROM visits WHERE {}", visit_match()),
        params![pattern, from, to, id],
    )?;
    Ok(digest.finalize().into())
}

fn url_fingerprint(db: &Connection, url: &str) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut visits = db.prepare(
        "SELECT event_id, title, visited_at FROM visits WHERE url = ?1 ORDER BY event_id",
    )?;
    let mut rows = visits.query([url])?;
    while let Some(row) = rows.next()? {
        let (a, b, c): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        feed(&mut digest, &[&"v", &a, &b, &c]);
    }
    let mut snapshots = db.prepare(
        "SELECT event_id, window_id, tab_index, title, pinned, incognito, recorded_at FROM tab_snapshots WHERE url = ?1 ORDER BY event_id",
    )?;
    let mut rows = snapshots.query([url])?;
    while let Some(row) = rows.next()? {
        let a: String = row.get(0)?;
        let numbers = (
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        );
        let (b, c): (String, String) = (row.get(3)?, row.get(6)?);
        feed(&mut digest, &[&"s", &a, &numbers, &b, &c]);
    }
    drop(rows);
    drop(snapshots);
    feed_navigations(
        db,
        &mut digest,
        "SELECT event_id FROM visits WHERE url = ?1",
        params![url],
    )?;
    Ok(digest.finalize().into())
}

/// Ids of the images a target names, in id order; metadata only.
fn image_ids(db: &Connection, target: &Target) -> Result<Vec<i64>> {
    let ids = match target {
        Target::Image { blob_id, sha256 } => db
            .prepare("SELECT id FROM image_blobs WHERE id = ?1 AND sha256 = ?2")?
            .query_map(params![blob_id, sha256], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
        Target::Images { tag: None } => db
            .prepare("SELECT id FROM image_blobs ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
        Target::Images { tag: Some(tag) } => db
            .prepare("SELECT l.blob_id FROM image_tag_links l JOIN image_tags t ON t.id = l.tag_id WHERE t.name = ?1 ORDER BY l.blob_id")?
            .query_map([tag], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
        _ => unreachable!("not an image target"),
    };
    Ok(ids)
}

fn image_counts(db: &Connection, ids: &[i64]) -> Result<Value> {
    let set: std::collections::HashSet<i64> = ids.iter().copied().collect();
    let (mut bytes, mut saves, mut thumbnails, mut links) = (0i64, 0i64, 0i64, 0i64);
    let mut blob = db.prepare(
        "SELECT byte_length, (SELECT count(*) FROM saved_images WHERE blob_id = b.id),
                (SELECT count(*) FROM image_thumbnails WHERE blob_id = b.id),
                (SELECT count(*) FROM image_tag_links WHERE blob_id = b.id) FROM image_blobs b WHERE id = ?1",
    )?;
    for id in ids {
        let (b, s, t, l): (i64, i64, i64, i64) =
            blob.query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        bytes += b;
        saves += s;
        thumbnails += t;
        links += l;
    }
    // A tag disappears with the last image that has it (the vault's trigger): only when every one
    // of its links is inside the deleted set.
    let mut totals: std::collections::HashMap<i64, (i64, i64)> = Default::default();
    let mut statement = db.prepare("SELECT tag_id, blob_id FROM image_tag_links")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let entry = totals.entry(row.get(0)?).or_default();
        entry.0 += 1;
        entry.1 += i64::from(set.contains(&row.get::<_, i64>(1)?));
    }
    let tags_removed = totals
        .values()
        .filter(|(all, inside)| *inside > 0 && all == inside)
        .count();
    Ok(json!({
        "kind": "images", "images": ids.len(), "bytes": bytes, "saves": saves,
        "thumbnails": thumbnails, "tagLinks": links, "tagsRemoved": tags_removed,
    }))
}

/// Metadata only (never the `data` columns): the blobs, their saves, thumbnails and tag links.
fn image_fingerprint(db: &Connection, ids: &[i64]) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut blob = db.prepare("SELECT sha256, mime_type, byte_length, width, height, created_at, last_saved_at, save_count FROM image_blobs WHERE id = ?1")?;
    let mut saves = db.prepare("SELECT id, save_id, image_url, page_url, frame_url, saved_at, incognito, mcp_public FROM saved_images WHERE blob_id = ?1 ORDER BY id")?;
    let mut thumbnail = db.prepare("SELECT mime_type, width, height, generator, length(data) FROM image_thumbnails WHERE blob_id = ?1")?;
    let mut links =
        db.prepare("SELECT tag_id FROM image_tag_links WHERE blob_id = ?1 ORDER BY tag_id")?;
    for id in ids {
        let row: (String, String, i64, i64, i64, i64, i64, i64) = blob.query_row([id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?;
        feed(&mut digest, &[&"b", id, &row]);
        let mut rows = saves.query([id])?;
        while let Some(r) = rows.next()? {
            let a: (
                i64,
                String,
                String,
                Option<String>,
                Option<String>,
                i64,
                i64,
                i64,
            ) = (
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            );
            feed(&mut digest, &[&"s", &a]);
        }
        let mut rows = thumbnail.query([id])?;
        while let Some(r) = rows.next()? {
            let a: (String, i64, i64, i64, i64) =
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
            feed(&mut digest, &[&"t", &a]);
        }
        let mut rows = links.query([id])?;
        while let Some(r) = rows.next()? {
            feed(&mut digest, &[&"l", &r.get::<_, i64>(0)?]);
        }
    }
    Ok(digest.finalize().into())
}

/// Counts, fingerprint and (for one visit) the row, in the caller's read snapshot.
fn measure(db: &Connection, target: &Target) -> Result<(Value, [u8; 32], Value)> {
    match target {
        Target::Visit { .. } | Target::Visits { .. } => {
            let counts = visit_counts(db, target)?;
            let fingerprint = visit_fingerprint(db, target)?;
            let detail = match target {
                Target::Visit { id } => db
                    .query_row(
                        "SELECT url, title, visited_at FROM visits WHERE event_id = ?1",
                        [id],
                        |r| Ok(json!({"id": id, "url": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?, "visitedAt": r.get::<_, String>(2)?})),
                    )
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            };
            Ok((counts, fingerprint, detail))
        }
        Target::Url { url } => {
            let c: UrlCounts = vault_items::url_counts(db, url)?;
            let mut counts = serde_json::to_value(c).map_err(|_| Error("db_error"))?;
            counts["kind"] = json!("url");
            counts["urls"] = json!(i64::from(c.visits + c.tab_snapshots > 0));
            Ok((counts, url_fingerprint(db, url)?, Value::Null))
        }
        Target::Image { .. } | Target::Images { .. } => {
            let ids = image_ids(db, target)?;
            let counts = image_counts(db, &ids)?;
            let fingerprint = image_fingerprint(db, &ids)?;
            Ok((counts, fingerprint, Value::Null))
        }
    }
}

fn has_targets(counts: &Value) -> bool {
    let key = match counts["kind"].as_str() {
        Some("images") => "images",
        Some("url") => "urls",
        _ => "visits",
    };
    counts[key].as_i64().unwrap_or(0) > 0
}

fn delete(db: &Connection, review: &Review) -> Result<Value> {
    let (counts, fingerprint, _) = measure(db, &review.target)?;
    if counts != review.counts || fingerprint != review.fingerprint {
        return Err(Error("item_targets_changed"));
    }
    match &review.target {
        Target::Visit { .. } | Target::Visits { .. } => {
            let (pattern, from, to, id) = visit_params(&review.target);
            let expected = counts["visits"].as_i64().unwrap_or(-1);
            db.execute_batch("CREATE TEMP TABLE item_delete_visits (event_id TEXT PRIMARY KEY)")?;
            db.execute(
                &format!(
                    "INSERT INTO item_delete_visits SELECT event_id FROM visits WHERE {}",
                    visit_match()
                ),
                params![pattern, from, to, id],
            )?;
            // Navigations that stay but whose source is deleted now: the source is marked as deleted on
            // purpose (never inferred from NULL), in the same statement that clears it.
            let censored = db.execute(
                "UPDATE navigation_events SET source_visit_id = NULL, source_censored = 1
                  WHERE source_visit_id IN (SELECT event_id FROM item_delete_visits)
                    AND target_visit_id NOT IN (SELECT event_id FROM item_delete_visits)",
                [],
            )? as i64;
            let removed = db.execute(
                "DELETE FROM visits WHERE event_id IN (SELECT event_id FROM item_delete_visits)",
                [],
            )? as i64;
            let left: i64 = db.query_row(
                "SELECT count(*) FROM visits WHERE event_id IN (SELECT event_id FROM item_delete_visits)",
                [],
                |r| r.get(0),
            )?;
            let remaining = visit_counts(db, &review.target)?;
            if removed != expected
                || censored != counts["navigationSourcesCleared"].as_i64().unwrap_or(-1)
                || left != 0
                || remaining["visits"] != 0
            {
                return Err(Error("item_targets_changed"));
            }
            db.execute_batch("DROP TABLE item_delete_visits")?;
        }
        Target::Url { url } => {
            vault_items::delete_url(db, url)?;
        }
        Target::Image { .. } | Target::Images { .. } => {
            let ids = image_ids(db, &review.target)?;
            let mut total = ImageCounts::default();
            for id in &ids {
                let sha: String =
                    db.query_row("SELECT sha256 FROM image_blobs WHERE id = ?1", [id], |r| {
                        r.get(0)
                    })?;
                let done = vault_items::delete_image(db, *id, &sha)?;
                total.saves += done.saves;
                total.thumbnails += done.thumbnails;
                total.tag_links += done.tag_links;
                total.tags_removed += done.tags_removed;
            }
            if total.saves != counts["saves"].as_i64().unwrap_or(-1)
                || total.tags_removed != counts["tagsRemoved"].as_i64().unwrap_or(-1)
                || !image_ids(db, &review.target)?.is_empty()
            {
                return Err(Error("item_targets_changed"));
            }
        }
    }
    Ok(counts)
}

impl Manager {
    pub fn handle(&mut self, session: &mut Session, message: &Value) -> Value {
        let id = request_id(message);
        let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
        let result = if id.is_none() {
            Err(Error("invalid_request_id"))
        } else {
            self.answer(session, message, kind)
        };
        let mut reply = match result {
            Ok(v) => v,
            Err(Error(code)) => {
                if code.starts_with("vault_")
                    || matches!(code, "key_unavailable" | "config_invalid" | "io_error")
                {
                    self.binding = None;
                    self.review = None;
                    self.receipt = None;
                }
                json!({"ok":false,"error":code})
            }
        };
        reply["type"] = json!(format!("{kind}_result"));
        reply["requestId"] = json!(id);
        reply
    }

    fn answer(&mut self, session: &mut Session, message: &Value, kind: &str) -> Result<Value> {
        if !matches!(
            kind,
            "item_state"
                | "visit_history"
                | "item_delete_preview"
                | "item_delete"
                | "item_delete_status"
        ) {
            return Err(Error("unknown_type"));
        }
        // A malformed request is refused before any vault is opened.
        let wanted = if kind == "item_delete_preview" {
            Some(Target::checked(
                message.get("target").ok_or(Error("invalid_target"))?,
            )?)
        } else {
            None
        };
        let _lock = ConfigLock::acquire(&session.env)?;
        session.reset();
        let ticket = session.ticket()?;
        let binding = Binding {
            vault_token: ticket.token.clone(),
            generation: page_protocol::generation(session).map_err(Error)?,
        };
        if self.binding.as_ref() != Some(&binding) {
            self.review = None;
            self.receipt = None;
            self.binding = Some(binding.clone());
        }
        if kind != "item_state"
            && (message.get("vaultToken").and_then(Value::as_str) != Some(&binding.vault_token)
                || message.get("generation").and_then(Value::as_str) != Some(&binding.generation))
        {
            return Err(Error("vault_switched"));
        }
        let mut value = match kind {
            "item_state" => json!({}),
            "visit_history" => session.read_for(&ticket, |db| {
                let tx = db.unchecked_transaction()?;
                let result = history(&tx, message)?;
                tx.commit()?;
                Ok::<_, Error>(result)
            })?,
            "item_delete_preview" => {
                self.review = None;
                let target = wanted.ok_or(Error("invalid_target"))?;
                let (counts, fingerprint, detail) = session.read_for(&ticket, |db| {
                    let tx = db.unchecked_transaction()?;
                    let measured = measure(&tx, &target)?;
                    tx.commit()?;
                    Ok::<_, Error>(measured)
                })?;
                if !has_targets(&counts) {
                    return Err(Error("item_not_found"));
                }
                let id = uuid::Uuid::new_v4().to_string();
                let value = json!({"reviewId": id, "target": target.shown(), "counts": counts, "detail": detail, "expiresInSeconds": REVIEW_TTL.as_secs()});
                self.review = Some(Review {
                    id,
                    target,
                    fingerprint,
                    counts,
                    issued: Instant::now(),
                });
                value
            }
            "item_delete" => {
                let id = message
                    .get("reviewId")
                    .and_then(Value::as_str)
                    .ok_or(Error("invalid_review"))?;
                // Another id neither confirms nor uses up the pending review.
                if self.review.as_ref().is_none_or(|review| review.id != id) {
                    return Err(Error("invalid_review"));
                }
                let review = self
                    .review
                    .take()
                    .filter(|review| review.issued.elapsed() <= REVIEW_TTL)
                    .ok_or(Error("invalid_review"))?;
                let deleted = session.write_for(&ticket, |db| delete(db, &review))?;
                let value = json!({"operationId": review.id, "deleted": deleted});
                self.receipt = Some(Receipt {
                    id: review.id,
                    counts: deleted,
                    issued: Instant::now(),
                });
                value
            }
            _ => {
                let id = message
                    .get("operationId")
                    .and_then(Value::as_str)
                    .ok_or(Error("invalid_review"))?;
                match &self.receipt {
                    Some(r) if r.id == id && r.issued.elapsed() <= REVIEW_TTL => {
                        json!({"operationId": id, "deleted": r.counts, "confirmed": true})
                    }
                    _ => json!({"operationId": id, "confirmed": false}),
                }
            }
        };
        value["ok"] = json!(true);
        value["vaultToken"] = json!(binding.vault_token);
        value["generation"] = json!(binding.generation);
        Ok(value)
    }
}

/// One URL's visits, newest first (keyset paging: no server state, stable while visits come and
/// go). Only visits that match the query and date range of the list the person is looking at.
fn history(db: &Connection, message: &Value) -> Result<Value> {
    let url = match Target::checked(&json!({"scope": "url", "url": message.get("url")}))? {
        Target::Url { url } => url,
        _ => unreachable!(),
    };
    let request = parse_request(&json!({
        "query": message.get("query"),
        "visitedFrom": message.get("visitedFrom"),
        "visitedTo": message.get("visitedTo"),
    }))
    .map_err(Error)?;
    let limit = match message.get("limit") {
        None => HISTORY_LIMIT,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=HISTORY_MAX).contains(n))
            .ok_or(Error("invalid_limit"))?,
    };
    let after = match message.get("after") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let at = v.get("visitedAt").and_then(Value::as_str);
            let id = v.get("id").and_then(Value::as_str);
            Some((
                at.ok_or(Error("invalid_cursor"))?.to_owned(),
                id.ok_or(Error("invalid_cursor"))?.to_owned(),
            ))
        }
    };
    let (from, to) = match &request.visited {
        Some((a, b)) => (Some(a.as_str()), Some(b.as_str())),
        None => (None, None),
    };
    let pattern = like_pattern(&request.query);
    let filter = format!("url = ?5 AND ({})", visit_match());
    let sql = format!(
        "SELECT event_id, title, visited_at FROM visits WHERE {filter} AND (?6 IS NULL OR visited_at < ?6 OR (visited_at = ?6 AND event_id < ?7)) ORDER BY visited_at DESC, event_id DESC LIMIT ?8"
    );
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(params![
        pattern,
        from,
        to,
        Option::<String>::None,
        url,
        after.as_ref().map(|a| a.0.as_str()),
        after.as_ref().map(|a| a.1.as_str()),
        limit + 1
    ])?;
    let (mut items, mut bytes, mut more, mut last) = (Vec::new(), 4096usize, false, None);
    while let Some(row) = rows.next()? {
        let (id, title, at): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        let value = json!({"id": id, "title": title, "visitedAt": at});
        let size = serde_json::to_vec(&value)
            .map_err(|_| Error("db_error"))?
            .len()
            + 1;
        if size + 4096 > BUDGET {
            return Err(Error("item_response_too_large"));
        }
        if items.len() >= limit as usize || bytes + size > BUDGET {
            more = true;
            break;
        }
        bytes += size;
        last = Some((at, id));
        items.push(value);
    }
    drop(rows);
    drop(statement);
    let counts = db.query_row(
        "SELECT (SELECT count(*) FROM visits WHERE url = ?1 AND (url LIKE ?2 ESCAPE '\\' OR title LIKE ?2 ESCAPE '\\') AND (?3 IS NULL OR (visited_at >= ?3 AND visited_at < ?4 AND VALID)))
              ,(SELECT count(*) FROM visits WHERE url = ?1)
              ,(SELECT count(*) FROM tab_snapshots WHERE url = ?1)"
            .replace("VALID", VALID_VISITED_AT)
            .as_str(),
        params![url, pattern, from, to],
        |r| Ok(json!({"matched": r.get::<_, i64>(0)?, "total": r.get::<_, i64>(1)?, "snapshots": r.get::<_, i64>(2)?})),
    )?;
    let next = if more {
        last.map(|(visited_at, id)| json!({"visitedAt": visited_at, "id": id}))
    } else {
        None
    };
    Ok(json!({"items": items, "next": next, "counts": counts}))
}

#[cfg(test)]
mod tests;
