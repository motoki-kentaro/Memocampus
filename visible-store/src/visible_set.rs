//! The visible set: a private `:memory:` database that holds public rows and nothing else, so that
//! counting, sorting and limiting cannot reach a private row whatever a query does.

use crate::snapshot::RawVisit;
use crate::{time, VisibleError, VisibleVisit, VisitPage};
use memocampus_native_host::policy::{CompiledPolicy, Decision};
use rusqlite::limits::Limit;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

pub(crate) type IdKey = [u8; 32];

const HMAC_BLOCK: usize = 64;
const SCHEMA: &str = "CREATE TABLE visible_visits (
    public_id TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    title TEXT NOT NULL,
    visited_at TEXT NOT NULL,
    visited_ms INTEGER NOT NULL
) WITHOUT ROWID;";
const FILTER: &str =
    "(url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\') AND visited_ms >= ?2 AND visited_ms < ?3";

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
                .prepare("INSERT INTO visible_visits VALUES (?1, ?2, ?3, ?4, ?5)")
                .map_err(failed)?;
            for row in rows {
                // `Decision::None` is the only public outcome; an unparsable URL is `NeverSave`.
                if policy.evaluate(&row.url) != Decision::None {
                    continue;
                }
                let Some(visited_ms) = time::parse_timestamp(&row.visited_at) else {
                    continue;
                };
                insert
                    .execute(params![
                        public_id(key, "visit", &row.event_id),
                        row.url,
                        row.title,
                        row.visited_at,
                        visited_ms
                    ])
                    .map_err(failed)?;
            }
        }
        db.execute_batch("COMMIT; PRAGMA query_only = ON;")
            .map_err(failed)?;
        Ok(Self { db })
    }

    /// `total` counts the matching public rows; `items` are the newest `limit` of them.
    pub(crate) fn query(
        &self,
        pattern: &str,
        range: (i64, i64),
        limit: u32,
    ) -> Result<VisitPage, VisibleError> {
        let total: i64 = self
            .db
            .query_row(
                &format!("SELECT count(*) FROM visible_visits WHERE {FILTER}"),
                params![pattern, range.0, range.1],
                |row| row.get(0),
            )
            .map_err(failed)?;
        let mut statement = self
            .db
            .prepare(&format!(
                "SELECT public_id, url, title, visited_at FROM visible_visits WHERE {FILTER}
                 ORDER BY visited_ms DESC, public_id LIMIT ?4"
            ))
            .map_err(failed)?;
        let items = statement
            .query_map(params![pattern, range.0, range.1, limit], |row| {
                Ok(VisibleVisit {
                    id: row.get(0)?,
                    url: row.get(1)?,
                    title: row.get(2)?,
                    visited_at: row.get(3)?,
                })
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(failed)?;
        let total = u64::try_from(total).map_err(failed)?;
        Ok(VisitPage {
            has_more: total > items.len() as u64,
            items,
            total,
        })
    }
}
