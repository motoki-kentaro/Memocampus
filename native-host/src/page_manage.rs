//! Local Page metadata only, never MHTML/VisibleStore/MCP. Cursors and confirmations live on one
//! connectNative port, expire, and cannot be forged/reused after a Host restart. No schema change.
use crate::{
    page_protocol,
    search::request_id,
    vault::{self, ConfigLock, Session},
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const PAGE_LIMIT: u64 = 20;
const MAX_LIMIT: u64 = 50;
const BUDGET: usize = 700_000;
const CURSOR_TTL: Duration = Duration::from_secs(300);
const REVIEW_TTL: Duration = Duration::from_secs(120);
const MAX_CURSORS: usize = 128;
const COLUMNS: &str = "p.id,p.save_id,p.blob_id,p.url,p.title,p.captured_at,p.saved_at,p.incognito,b.sha256,b.byte_length";
// Literal substring search: SQLite LIKE folds ASCII only; all other Unicode is case-sensitive.
const MATCH: &str = "(?1 = '' OR p.url LIKE ?1 ESCAPE '!' OR p.title LIKE ?1 ESCAPE '!') AND (?2 IS NULL OR p.id = ?2) AND (?3 IS NULL OR p.url = ?3)";

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
type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    vault_token: String,
    generation: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "scope", rename_all = "camelCase", deny_unknown_fields)]
enum Target {
    Event {
        #[serde(serialize_with = "serialize_id")]
        id: i64,
    },
    Url {
        url: String,
    },
    Filter {
        query: String,
    },
}
impl Target {
    fn checked(value: &Value) -> Result<Self> {
        let mut value = value.clone();
        if value.get("scope").and_then(Value::as_str) == Some("event") {
            if let Some(Value::String(id)) = value.get("id") {
                let number = id
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n > 0 && n.to_string() == *id)
                    .ok_or(Error("invalid_target"))?;
                value["id"] = json!(number);
            } else if value
                .get("id")
                .and_then(Value::as_i64)
                .is_some_and(|n| n > 9_007_199_254_740_991)
            {
                return Err(Error("invalid_target"));
            }
        }
        let target: Self = serde_json::from_value(value).map_err(|_| Error("invalid_target"))?;
        match &target {
            Self::Event { id } if *id > 0 => {}
            Self::Url { url } if !url.is_empty() && url.len() <= crate::pages::MAX_URL_BYTES => {}
            Self::Filter { query } if query.chars().count() <= 200 && !query.contains('\0') => {}
            _ => return Err(Error("invalid_target")),
        }
        Ok(target)
    }
    fn bindings(&self) -> (String, Option<i64>, Option<&str>) {
        match self {
            Self::Event { id } => (String::new(), Some(*id), None),
            Self::Url { url } => (String::new(), None, Some(url)),
            Self::Filter { query } => (like(query), None, None),
        }
    }
}
fn like(query: &str) -> String {
    if query.is_empty() {
        String::new()
    } else {
        format!(
            "%{}%",
            query
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_")
        )
    }
}
fn query(message: &Value) -> Result<String> {
    let value = match message.get("query") {
        None => "",
        Some(Value::String(s)) => s,
        _ => return Err(Error("invalid_query")),
    };
    if value.chars().count() > 200 || value.contains('\0') {
        return Err(Error("invalid_query"));
    }
    Ok(value.into())
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Event {
    #[serde(serialize_with = "serialize_id")]
    id: i64,
    save_id: String,
    #[serde(serialize_with = "serialize_id")]
    blob_id: i64,
    url: String,
    title: String,
    captured_at: i64,
    saved_at: i64,
    incognito: bool,
    sha256: String,
    byte_length: i64,
}
fn serialize_id<S: serde::Serializer>(
    id: &i64,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&id.to_string())
}
fn event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get(0)?,
        save_id: row.get(1)?,
        blob_id: row.get(2)?,
        url: row.get(3)?,
        title: row.get(4)?,
        captured_at: row.get(5)?,
        saved_at: row.get(6)?,
        incognito: row.get(7)?,
        sha256: row.get(8)?,
        byte_length: row.get(9)?,
    })
}

#[derive(Clone)]
struct Cursor {
    query: String,
    url: Option<String>,
    after: (i64, i64),
    fingerprint: [u8; 32],
    issued: Instant,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Counts {
    events: i64,
    urls: i64,
    blobs_removed: i64,
    blobs_kept: i64,
    bytes_removed: i64,
}
struct Review {
    id: String,
    target: Target,
    fingerprint: [u8; 32],
    counts: Counts,
    issued: Instant,
}
struct Receipt {
    id: String,
    counts: Counts,
    issued: Instant,
}
#[derive(Default)]
pub struct Manager {
    binding: Option<Binding>,
    cursors: HashMap<String, Cursor>,
    review: Option<Review>,
    receipt: Option<Receipt>,
}

/// Streaming metadata digest, never reads `page_blobs.data`. Includes original metadata and
/// reference counts so rowid reuse, same-count replacement and shared-BLOB changes are detected.
fn fingerprint(db: &Connection, target: &Target) -> Result<[u8; 32]> {
    let (pattern, id, url) = target.bindings();
    let sql=format!("SELECT {COLUMNS},refs.n FROM saved_pages p JOIN page_blobs b ON b.id=p.blob_id JOIN (SELECT blob_id,count(*) AS n FROM saved_pages GROUP BY blob_id) refs ON refs.blob_id=p.blob_id WHERE {MATCH} ORDER BY p.id");
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(params![pattern, id, url])?;
    let mut digest = Sha256::new();
    while let Some(row) = rows.next()? {
        let record = event(row)?;
        let refs: i64 = row.get(10)?;
        let bytes = serde_json::to_vec(&(record, refs)).map_err(|_| Error("db_error"))?;
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    Ok(digest.finalize().into())
}
fn counts(db: &Connection, target: &Target) -> Result<Counts> {
    let (pattern, id, url) = target.bindings();
    let sql=format!("WITH targets AS (SELECT p.id,p.blob_id,p.url FROM saved_pages p WHERE {MATCH}), blobs AS (SELECT b.id,b.byte_length,NOT EXISTS (SELECT 1 FROM saved_pages r WHERE r.blob_id=b.id AND r.id NOT IN (SELECT id FROM targets)) AS gone FROM page_blobs b WHERE b.id IN (SELECT blob_id FROM targets)) SELECT (SELECT count(*) FROM targets),(SELECT count(DISTINCT url) FROM targets),coalesce(sum(gone),0),coalesce(sum(NOT gone),0),coalesce(sum(CASE WHEN gone THEN byte_length ELSE 0 END),0) FROM blobs");
    Ok(db.query_row(&sql, params![pattern, id, url], |r| {
        Ok(Counts {
            events: r.get(0)?,
            urls: r.get(1)?,
            blobs_removed: r.get(2)?,
            blobs_kept: r.get(3)?,
            bytes_removed: r.get(4)?,
        })
    })?)
}
fn delete(db: &Connection, review: &Review) -> Result<Counts> {
    if fingerprint(db, &review.target)? != review.fingerprint
        || counts(db, &review.target)? != review.counts
    {
        return Err(Error("page_targets_changed"));
    }
    let (pattern, id, url) = review.target.bindings();
    // Temporary ID set, populated under the same IMMEDIATE transaction as the locked recheck.
    // Only blobs referenced by this operation are candidates; unrelated orphan blobs stay alone.
    db.execute_batch(
        "CREATE TEMP TABLE page_delete_targets (id INTEGER PRIMARY KEY,blob_id INTEGER NOT NULL)",
    )?;
    db.execute(&format!("INSERT INTO page_delete_targets SELECT p.id,p.blob_id FROM saved_pages p WHERE {MATCH}"),params![pattern,id,url])?;
    let removed = db.execute(
        "DELETE FROM saved_pages WHERE id IN (SELECT id FROM page_delete_targets)",
        [],
    )? as i64;
    let blobs=db.execute("DELETE FROM page_blobs WHERE id IN (SELECT blob_id FROM page_delete_targets) AND NOT EXISTS (SELECT 1 FROM saved_pages p WHERE p.blob_id=page_blobs.id)",[])? as i64;
    if removed != review.counts.events
        || blobs != review.counts.blobs_removed
        || counts(db, &review.target)?.events != 0
    {
        return Err(Error("page_targets_changed"));
    }
    db.execute_batch("DROP TABLE page_delete_targets")?;
    Ok(review.counts.clone())
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
        self.reply(kind, id, result)
    }
    fn reply(&mut self, kind: &str, id: Option<&str>, result: Result<Value>) -> Value {
        let mut reply = match result {
            Ok(v) => v,
            Err(Error(code)) => {
                if code.starts_with("vault_")
                    || matches!(code, "key_unavailable" | "config_invalid" | "io_error")
                {
                    self.binding = None;
                    self.cursors.clear();
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
    /// Caller holds the `ConfigLock`. Fresh vault/key/schema, then the request's token and
    /// selection generation must be the current ones (`page_state` alone may omit them).
    fn bind(
        &mut self,
        session: &mut Session,
        message: &Value,
        kind: &str,
    ) -> Result<(vault::VaultTicket, Binding)> {
        session.reset();
        let ticket = session.ticket()?;
        let binding = Binding {
            vault_token: ticket.token.clone(),
            generation: page_protocol::generation(session).map_err(Error)?,
        };
        if self.binding.as_ref() != Some(&binding) {
            self.cursors.clear();
            self.review = None;
            self.receipt = None;
            self.binding = Some(binding.clone());
        }
        if kind != "page_state"
            && (message.get("vaultToken").and_then(Value::as_str) != Some(&binding.vault_token)
                || message.get("generation").and_then(Value::as_str) != Some(&binding.generation))
        {
            return Err(Error("vault_switched"));
        }
        Ok((ticket, binding))
    }
    fn answer(&mut self, session: &mut Session, message: &Value, kind: &str) -> Result<Value> {
        let _lock = ConfigLock::acquire(&session.env)?;
        let (ticket, binding) = self.bind(session, message, kind)?;
        let mut value = match kind {
            "page_state" => json!({}),
            "page_list" | "page_history" => session.read_for(&ticket, |db| {
                let tx = db.unchecked_transaction()?;
                let result = self.list(&tx, message, kind)?;
                tx.commit()?;
                Ok::<_, Error>(result)
            })?,
            "page_delete_preview" => {
                self.review = None;
                let target =
                    Target::checked(message.get("target").ok_or(Error("invalid_target"))?)?;
                let (counts,fingerprint,detail)=session.read_for(&ticket,|db|{
                    let tx=db.unchecked_transaction()?;let summary=counts(&tx,&target)?;
                    if summary.events==0{return Err(Error("page_not_found"));}
                    let hash=fingerprint(&tx,&target)?;
                    let detail=if let Target::Event{id}=target { Some(tx.query_row(&format!("SELECT {COLUMNS} FROM saved_pages p JOIN page_blobs b ON b.id=p.blob_id WHERE p.id=?1"),[id],event)?) } else {None};
                    tx.commit()?;Ok::<_,Error>((summary,hash,detail))
                })?;
                let id = uuid::Uuid::new_v4().to_string();
                let value = json!({"reviewId":id,"target":target,"counts":counts,"event":detail,"expiresInSeconds":REVIEW_TTL.as_secs()});
                self.review = Some(Review {
                    id,
                    target,
                    fingerprint,
                    counts,
                    issued: Instant::now(),
                });
                value
            }
            "page_delete" => {
                let id = message
                    .get("reviewId")
                    .and_then(Value::as_str)
                    .ok_or(Error("invalid_review"))?;
                let review = self
                    .review
                    .take()
                    .filter(|review| review.id == id && review.issued.elapsed() <= REVIEW_TTL)
                    .ok_or(Error("invalid_review"))?;
                let deleted = session.write_for(&ticket, |db| delete(db, &review))?;
                let value = json!({"operationId":review.id,"deleted":deleted});
                self.receipt = Some(Receipt {
                    id: review.id,
                    counts: deleted,
                    issued: Instant::now(),
                });
                self.cursors.clear();
                value
            }
            "page_delete_status" => {
                let id = message
                    .get("operationId")
                    .and_then(Value::as_str)
                    .ok_or(Error("invalid_review"))?;
                match &self.receipt {
                    Some(r) if r.id == id && r.issued.elapsed() <= REVIEW_TTL => {
                        json!({"operationId":id,"deleted":r.counts,"confirmed":true})
                    }
                    _ => json!({"operationId":id,"confirmed":false}),
                }
            }
            _ => return Err(Error("unknown_type")),
        };
        value["ok"] = json!(true);
        value["vaultToken"] = json!(binding.vault_token);
        value["generation"] = json!(binding.generation);
        Ok(value)
    }
    fn list(&mut self, db: &Connection, message: &Value, kind: &str) -> Result<Value> {
        let query = query(message)?;
        let pattern = like(&query);
        let url = if kind == "page_history" {
            match Target::checked(&json!({"scope":"url","url":message.get("url")}))? {
                Target::Url { url } => Some(url),
                _ => unreachable!(),
            }
        } else {
            None
        };
        let limit = match message.get("limit") {
            None => PAGE_LIMIT,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=MAX_LIMIT).contains(n))
                .ok_or(Error("invalid_limit"))?,
        };
        let hash = fingerprint(
            db,
            &Target::Filter {
                query: String::new(),
            },
        )?;
        let after = match message.get("cursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) => {
                let cursor = self.cursors.get(id).ok_or(Error("invalid_cursor"))?;
                if cursor.issued.elapsed() > CURSOR_TTL
                    || cursor.query != query
                    || cursor.url != url
                {
                    return Err(Error("invalid_cursor"));
                }
                if cursor.fingerprint != hash {
                    return Err(Error("page_list_changed"));
                }
                Some(cursor.after)
            }
            _ => return Err(Error("invalid_cursor")),
        };
        let (at, id) = after.map_or((None, None), |(a, b)| (Some(a), Some(b)));
        let tail="(?4 IS NULL OR p.saved_at < ?4 OR (p.saved_at = ?4 AND p.id < ?5)) ORDER BY p.saved_at DESC,p.id DESC LIMIT ?6";
        let sql = if kind == "page_history" {
            format!("SELECT {COLUMNS},(SELECT count(*) FROM saved_pages t WHERE t.url=p.url),(SELECT count(*) FROM saved_pages t WHERE t.url=p.url AND (?1='' OR t.url LIKE ?1 ESCAPE '!' OR t.title LIKE ?1 ESCAPE '!')) FROM saved_pages p JOIN page_blobs b ON b.id=p.blob_id WHERE p.url=?2 AND (?1='' OR p.url LIKE ?1 ESCAPE '!' OR p.title LIKE ?1 ESCAPE '!') AND (?3 IS NULL) AND {tail}")
        } else {
            format!("SELECT {COLUMNS},(SELECT count(*) FROM saved_pages t WHERE t.url=p.url),(SELECT count(*) FROM saved_pages t WHERE t.url=p.url AND (?1='' OR t.url LIKE ?1 ESCAPE '!' OR t.title LIKE ?1 ESCAPE '!')) FROM saved_pages p JOIN page_blobs b ON b.id=p.blob_id WHERE (?2 IS NULL) AND (?3 IS NULL) AND NOT EXISTS (SELECT 1 FROM saved_pages t WHERE t.url=p.url AND (t.saved_at>p.saved_at OR (t.saved_at=p.saved_at AND t.id>p.id))) AND EXISTS (SELECT 1 FROM saved_pages t WHERE t.url=p.url AND (?1='' OR t.url LIKE ?1 ESCAPE '!' OR t.title LIKE ?1 ESCAPE '!')) AND {tail}")
        };
        let mut statement = db.prepare(&sql)?;
        let mut rows = statement.query(params![
            pattern,
            url,
            Option::<i64>::None,
            at,
            id,
            limit + 1
        ])?;
        let mut items = Vec::new();
        let mut bytes = 4096;
        let mut more = false;
        let mut last = None;
        while let Some(row) = rows.next()? {
            let record = event(row)?;
            let position = (record.saved_at, record.id);
            let mut value = serde_json::to_value(record).map_err(|_| Error("db_error"))?;
            value["eventCount"] = json!(row.get::<_, i64>(10)?);
            value["matchedCount"] = json!(row.get::<_, i64>(11)?);
            let size = serde_json::to_vec(&value)
                .map_err(|_| Error("db_error"))?
                .len()
                + 1;
            if size + 4096 > BUDGET {
                return Err(Error("page_response_too_large"));
            }
            if items.len() >= limit as usize || bytes + size > BUDGET {
                more = true;
                break;
            }
            bytes += size;
            items.push(value);
            last = Some(position);
        }
        self.cursors.retain(|_, c| c.issued.elapsed() <= CURSOR_TTL);
        let next = if more {
            if self.cursors.len() >= MAX_CURSORS {
                self.cursors.clear();
            }
            let id = uuid::Uuid::new_v4().to_string();
            self.cursors.insert(
                id.clone(),
                Cursor {
                    query,
                    url,
                    after: last.ok_or(Error("page_response_too_large"))?,
                    fingerprint: hash,
                    issued: Instant::now(),
                },
            );
            Some(id)
        } else {
            None
        };
        Ok(json!({"items":items,"next":next}))
    }
}

mod export;
#[cfg(test)]
mod tests;
