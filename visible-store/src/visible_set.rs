//! The visible set: a private `:memory:` database that holds public rows and nothing else, so that
//! counting, sorting and limiting cannot reach a private row whatever a query does.

use crate::snapshot::{RawPage, RawVisit};
use crate::{
    time, DomainCount, DomainStatsPage, VisibleError, VisiblePage, VisibleVisit, VisitPage,
};
use memocampus_native_host::page_metadata::{
    clean_text, clean_url, is_timestamp, MAX_DESCRIPTION, MAX_SITE_NAME, MAX_TITLE,
};
use memocampus_native_host::policy::{normalize_url, CompiledPolicy, Decision};
use rusqlite::limits::Limit;
use rusqlite::{params, Connection, Row};
use sha2::{Digest, Sha256};

pub(crate) type IdKey = [u8; 32];

const HMAC_BLOCK: usize = 64;
const SCHEMA: &str = "CREATE TABLE visible_visits (
    public_id TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    host TEXT NOT NULL,
    title TEXT NOT NULL,
    visited_at TEXT NOT NULL,
    visited_ms INTEGER NOT NULL,
    page_title TEXT,
    description TEXT,
    og_title TEXT,
    og_description TEXT,
    og_site_name TEXT,
    og_image_url TEXT,
    favicon_url TEXT,
    page_captured_at TEXT
) WITHOUT ROWID;";
/// The text a search looks in. The two metadata URLs are not searched: they name other resources.
const FILTER: &str = "(url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\'
    OR page_title LIKE ?1 ESCAPE '\\' OR description LIKE ?1 ESCAPE '\\'
    OR og_title LIKE ?1 ESCAPE '\\' OR og_description LIKE ?1 ESCAPE '\\'
    OR og_site_name LIKE ?1 ESCAPE '\\')
    AND visited_ms >= ?2 AND visited_ms < ?3";
/// Rows after a position, in the order of a page: time descending, then ID.
const AFTER: &str = "(visited_ms < ?4 OR (visited_ms = ?4 AND public_id > ?5))";
const COLUMNS: &str = "public_id, url, title, visited_at, page_title, description, og_title,
    og_description, og_site_name, og_image_url, favicon_url, page_captured_at";

/// A position in the page order: the time and ID of the last row returned.
pub(crate) type Position = (i64, String);

pub(crate) struct VisibleSet {
    pub(crate) db: Connection,
}

pub(crate) fn failed<E>(_: E) -> VisibleError {
    VisibleError::VisibleSetFailed
}

/// HMAC-SHA-256 truncated to 128 bits. The key is random per store, so an ID reveals nothing about
/// `identity`, the row's position or its neighbours, and cannot be derived outside this process.
/// `kind` keeps the IDs of different things apart.
pub(crate) fn public_id(key: &IdKey, kind: &str, identity: &str) -> String {
    let mut inner = Sha256::new();
    let mut outer = Sha256::new();
    let mut pad = [0u8; HMAC_BLOCK];
    pad[..key.len()].copy_from_slice(key);
    inner.update(pad.map(|byte| byte ^ 0x36));
    outer.update(pad.map(|byte| byte ^ 0x5c));
    inner.update(kind.as_bytes());
    inner.update(b"\0");
    inner.update(identity.as_bytes());
    outer.update(inner.finalize());
    let digest = outer.finalize();
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{kind}_{hex}")
}

/// Escapes LIKE metacharacters so user input only ever matches literally.
pub(crate) fn like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for ch in query.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

impl VisibleSet {
    /// A set that fails to build is dropped whole; the caller only ever sees a complete one.
    pub(crate) fn build(
        rows: Vec<RawVisit>,
        policy: &CompiledPolicy,
        key: &IdKey,
    ) -> Result<Self, VisibleError> {
        let db = Connection::open_in_memory().map_err(failed)?;
        db.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
        db.execute_batch(SCHEMA).map_err(failed)?;
        db.execute_batch("BEGIN").map_err(failed)?;
        {
            let mut insert = db
                .prepare(
                    "INSERT INTO visible_visits VALUES
                     (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                )
                .map_err(failed)?;
            for row in rows {
                // `Decision::None` is the only public outcome; an unparsable URL is `NeverSave`.
                if policy.evaluate(&row.url) != Decision::None {
                    continue;
                }
                let Some(visited_ms) = time::parse_timestamp(&row.visited_at) else {
                    continue;
                };
                // The host as the policy judged it: lower case, without trailing dots or port.
                let Ok(normalized) = normalize_url(&row.url) else {
                    continue;
                };
                let page = row
                    .page
                    .as_ref()
                    .and_then(|page| checked_page(page, policy));
                let field = |pick: fn(&VisiblePage) -> &Option<String>| {
                    page.as_ref().and_then(|page| pick(page).clone())
                };
                insert
                    .execute(params![
                        public_id(key, "visit", &row.event_id),
                        row.url,
                        normalized.host,
                        row.title,
                        row.visited_at,
                        visited_ms,
                        field(|page| &page.title),
                        field(|page| &page.description),
                        field(|page| &page.og_title),
                        field(|page| &page.og_description),
                        field(|page| &page.og_site_name),
                        field(|page| &page.og_image_url),
                        field(|page| &page.favicon_url),
                        page.as_ref().map(|page| page.captured_at.clone()),
                    ])
                    .map_err(failed)?;
            }
        }
        db.execute_batch("COMMIT; PRAGMA query_only = ON;")
            .map_err(failed)?;
        Ok(Self { db })
    }

    /// `total` counts the matching public rows; `items` are the newest `limit` of them after
    /// `after`, and `has_more` says whether more follow those.
    pub(crate) fn query(
        &self,
        pattern: &str,
        range: (i64, i64),
        after: Option<&Position>,
        limit: u32,
    ) -> Result<VisitPage, VisibleError> {
        // Before every row: no stored time reaches `i64::MAX` (`time::parse_timestamp`).
        let start: Position = (i64::MAX, String::new());
        let after = after.unwrap_or(&start);
        let count = |condition: &str| -> Result<u64, VisibleError> {
            let count: i64 = self
                .db
                .query_row(
                    &format!("SELECT count(*) FROM visible_visits WHERE {FILTER} AND {condition}"),
                    params![pattern, range.0, range.1, after.0, after.1],
                    |row| row.get(0),
                )
                .map_err(failed)?;
            u64::try_from(count).map_err(failed)
        };
        let total = count("?4 IS NOT NULL AND ?5 IS NOT NULL")?;
        let remaining = count(AFTER)?;
        let mut statement = self
            .db
            .prepare(&format!(
                "SELECT {COLUMNS} FROM visible_visits WHERE {FILTER} AND {AFTER}
                 ORDER BY visited_ms DESC, public_id LIMIT ?6"
            ))
            .map_err(failed)?;
        let items = statement
            .query_map(
                params![pattern, range.0, range.1, after.0, after.1, limit],
                visit_of,
            )
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(failed)?;
        Ok(VisitPage {
            has_more: remaining > items.len() as u64,
            items,
            total,
        })
    }

    /// Public visits per host in the range, most visited first. `total_visits` and
    /// `total_domains` cover the whole range, before the limit.
    pub(crate) fn domains(
        &self,
        range: (i64, i64),
        limit: u32,
    ) -> Result<DomainStatsPage, VisibleError> {
        let (total_visits, total_domains): (i64, i64) = self
            .db
            .query_row(
                "SELECT count(*), count(DISTINCT host) FROM visible_visits
                 WHERE visited_ms >= ?1 AND visited_ms < ?2",
                params![range.0, range.1],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(failed)?;
        let mut statement = self
            .db
            .prepare(
                "SELECT host, count(*) AS visits FROM visible_visits
                 WHERE visited_ms >= ?1 AND visited_ms < ?2
                 GROUP BY host ORDER BY visits DESC, host LIMIT ?3",
            )
            .map_err(failed)?;
        let items = statement
            .query_map(params![range.0, range.1, limit], |row| {
                Ok(DomainCount {
                    domain: row.get(0)?,
                    visit_count: row.get(1)?,
                })
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(failed)?;
        let total_domains = u64::try_from(total_domains).map_err(failed)?;
        Ok(DomainStatsPage {
            has_more: total_domains > items.len() as u64,
            items,
            total_visits: u64::try_from(total_visits).map_err(failed)?,
            total_domains,
        })
    }
}

fn visit_of(row: &Row) -> rusqlite::Result<VisibleVisit> {
    let captured_at: Option<String> = row.get(11)?;
    Ok(VisibleVisit {
        id: row.get(0)?,
        url: row.get(1)?,
        title: row.get(2)?,
        visited_at: row.get(3)?,
        page: match captured_at {
            None => None,
            Some(captured_at) => Some(VisiblePage {
                title: row.get(4)?,
                description: row.get(5)?,
                og_title: row.get(6)?,
                og_description: row.get(7)?,
                og_site_name: row.get(8)?,
                og_image_url: row.get(9)?,
                favicon_url: row.get(10)?,
                captured_at,
            }),
        },
    })
}

/// Stored text is kept only in the form the host writes (`page_metadata::clean_text`), so nothing
/// is repaired; any other value is absent. The two URLs must also be public themselves: they may
/// name another host, and a private host is never described, even as an image source.
fn checked_page(page: &RawPage, policy: &CompiledPolicy) -> Option<VisiblePage> {
    if !is_timestamp(&page.captured_at) {
        return None;
    }
    let text = |value: &Option<String>, limit: usize| {
        value
            .as_deref()
            .filter(|raw| clean_text(raw, limit).as_deref() == Some(*raw))
            .map(str::to_owned)
    };
    let url = |value: &Option<String>| {
        value
            .as_deref()
            .filter(|raw| clean_url(raw).as_deref() == Some(*raw))
            .filter(|raw| policy.evaluate(raw) == Decision::None)
            .map(str::to_owned)
    };
    Some(VisiblePage {
        title: text(&page.title, MAX_TITLE),
        description: text(&page.description, MAX_DESCRIPTION),
        og_title: text(&page.og_title, MAX_TITLE),
        og_description: text(&page.og_description, MAX_DESCRIPTION),
        og_site_name: text(&page.og_site_name, MAX_SITE_NAME),
        og_image_url: url(&page.og_image_url),
        favicon_url: url(&page.favicon_url),
        captured_at: page.captured_at.clone(),
    })
}
