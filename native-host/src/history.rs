//! Read-only API of the full-page History (Slice 1): one day's visits as numbered pages, the days of a
//! month that have visits, and one visit with its navigation. Schema 9 is used as it is: no column, no
//! index, no table is added, and nothing here writes (see `docs/history-slice-1.md`).
//!
//! ```text
//! → history_list   { requestId, visitedFrom, visitedTo, domain?, mode?, page?, size? }
//! ← history_list_result   { ok, requestId, vaultToken, items, total, page, size, pages }
//! → history_days   { requestId, bounds: [b0 … bn], domain?, mode? }
//! ← history_days_result   { ok, requestId, vaultToken, counts: [c0 … c(n-1)], total }
//! → history_visit  { requestId, id }
//! ← history_visit_result  { ok, requestId, vaultToken, visit, navigation, source, targets, targetCount,
//!                           targetsTruncated, pageMetadata }
//! ← *_result { ok: false, requestId, error }
//! ```
//!
//! One visit is one item (`visits` row): the same URL visited twice is two items; tab snapshots are not
//! visits and never appear. Order: `visited_at DESC, event_id DESC`. Only a `visited_at` that is an exact
//! UTC instant (`visit_days::VALID_VISITED_AT`) is listed, counted or found; the same test serves the list,
//! the month and the detail, so the counts always agree.
//!
//! **Range.** The host knows no time zone: `visitedFrom <= visited_at < visitedTo`, exact
//! `Date#toISOString()` texts checked as `visit_days` checks them, and for a list at most 26 hours apart (a
//! local day is 23 to 25 hours). `history_days` takes the 29 to 32 bounds `visit_days` takes.
//!
//! **Domain.** The host name of the URL, never the whole URL. Blank: every visit. `exact` (the default): the
//! whole host name, as the `url` crate writes it (lower case, IDN as punycode) without trailing dots, which
//! is how `policy.rs` reads a host; a name with a scheme, path, port or user is `invalid_domain`. `regex`: the
//! `regex` crate (linear time, no look-around, no back-reference), anywhere in that host name, case
//! insensitive, at most 512 bytes, 1 MiB compiled and 256 KiB of lazy DFA; anything else is an error
//! (`invalid_regex`, `unsupported_regex`, `regex_limit`, `regex_too_long`), never "no filter". A visit whose
//! URL has no host name matches no non-blank filter. The filter is made before the vault is opened and is
//! applied before paging; rows are streamed, only one page of them is kept.
//!
//! **Paging.** `page` from 1, `size` 25, 50 (default) or 100. `total` is the number of matching visits,
//! `pages = ceil(total / size)` (0 when there is none); a page past the end is an empty list, not an error.
//!
//! **Size.** A list item carries the URL up to 2048 and the title up to 512 characters (cut on a character
//! boundary, flagged by `urlTruncated` / `titleTruncated`); the sum of the items as serialized (JSON escapes
//! included) may not pass `BUDGET`, else `response_too_large` and no item at all: the contract of `total` and
//! `pages` is never kept by silently returning fewer items. A detail holds the full URL and title of the
//! visit, and `response_too_large` when the whole answer would pass `BUDGET`.
//!
//! **Detail.** `source.state` is `present` (the visit, as it is stored), `censored` (the person deleted it on
//! purpose: `source_censored`, nothing else of it is known) or `unknown` (nothing was recorded, or it cannot be
//! told; a bare NULL is never "censored", and never "typed directly"). `targets` are the navigations whose
//! source is this visit. `pageMetadata` only when its row belongs to this very visit.

use memocampus_native_host::navigation::{self, SourceState};
use regex::{Regex, RegexBuilder};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use url::Url;

use crate::{
    search::request_id,
    vault::Session,
    visit_days::{self, INVALID_BOUNDS, VALID_VISITED_AT},
};

pub const SIZES: [u64; 3] = [25, 50, 100];
pub const DEFAULT_SIZE: u64 = 50;
/// A local day is 23 to 25 hours; the list takes a little more.
pub const MAX_LIST_SPAN_MS: i64 = 26 * 3_600_000;
pub const MAX_DOMAIN_BYTES: usize = 512;
pub const REGEX_SIZE_LIMIT: usize = 1024 * 1024;
pub const REGEX_DFA_SIZE_LIMIT: usize = 256 * 1024;
pub const LIST_URL_CHARS: usize = 2048;
pub const LIST_TITLE_CHARS: usize = 512;
/// Of the serialized answer; Chrome refuses a native message of more than 1 MiB.
pub const BUDGET: usize = 700_000;
const TARGET_LIMIT: usize = 200;

const DB_ERROR: &str = "db_error";
pub const TOO_LARGE: &str = "response_too_large";

// ---------------------------------------------------------------------------------------------- filter

/// The domain condition of a list and of the days of a month: one implementation for both.
pub enum Filter {
    All,
    Exact(String),
    Pattern(Regex),
}

/// The host name of a stored URL, as `policy.rs` reads one: the `url` crate's, trailing dots removed.
pub fn host_of(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

fn normalized_exact(input: &str) -> Result<String, &'static str> {
    let text = input.trim();
    if text.len() > MAX_DOMAIN_BYTES {
        return Err("invalid_domain");
    }
    let parsed = Url::parse(&format!("http://{text}/")).map_err(|_| "invalid_domain")?;
    if parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("invalid_domain");
    }
    host_of(parsed.as_str()).ok_or("invalid_domain")
}

fn compiled(pattern: &str) -> Result<Regex, &'static str> {
    if pattern.len() > MAX_DOMAIN_BYTES {
        return Err("regex_too_long");
    }
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build()
        .map_err(|error| match error {
            regex::Error::CompiledTooBig(_) => "regex_limit",
            _ => {
                use regex_syntax::ast::ErrorKind;
                match regex_syntax::ast::parse::Parser::new().parse(pattern) {
                    Err(error) => match error.kind() {
                        ErrorKind::UnsupportedLookAround | ErrorKind::UnsupportedBackreference => {
                            "unsupported_regex"
                        }
                        ErrorKind::NestLimitExceeded(_) => "regex_limit",
                        _ => "invalid_regex",
                    },
                    Ok(_) => "invalid_regex",
                }
            }
        })
}

impl Filter {
    /// `domain` and `mode` of a request. Blank is every domain; a `mode` that is neither is refused, blank or not.
    pub fn parse(message: &Value) -> Result<Filter, &'static str> {
        let regex = match message.get("mode") {
            None | Some(Value::Null) => false,
            Some(Value::String(mode)) if mode == "exact" => false,
            Some(Value::String(mode)) if mode == "regex" => true,
            _ => return Err("invalid_mode"),
        };
        let domain = match message.get("domain") {
            None | Some(Value::Null) => return Ok(Filter::All),
            Some(Value::String(text)) => text,
            _ => return Err("invalid_domain"),
        };
        if domain.trim().is_empty() {
            return Ok(Filter::All);
        }
        if regex {
            compiled(domain).map(Filter::Pattern)
        } else {
            normalized_exact(domain).map(Filter::Exact)
        }
    }

    pub fn is_all(&self) -> bool {
        matches!(self, Filter::All)
    }

    pub fn matches(&self, url: &str) -> bool {
        match self {
            Filter::All => true,
            Filter::Exact(wanted) => host_of(url).is_some_and(|host| &host == wanted),
            Filter::Pattern(pattern) => host_of(url).is_some_and(|host| pattern.is_match(&host)),
        }
    }
}

// ----------------------------------------------------------------------------------------------- helpers

/// At most `max` characters of `text`, cut on a character boundary; whether it was cut.
fn clip(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((end, _)) => (text[..end].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

fn failure(result: &str, message: &Value, code: &str) -> Value {
    json!({ "ok": false, "type": result, "requestId": request_id(message), "error": code })
}

fn success(result: &str, message: &Value, token: String, mut body: Value) -> Value {
    body["ok"] = json!(true);
    body["type"] = json!(result);
    body["requestId"] = json!(request_id(message));
    body["vaultToken"] = json!(token);
    body
}

fn db_error<E>(_: E) -> &'static str {
    DB_ERROR
}

/// `visitedFrom`/`visitedTo` of a list: exact instants, increasing, at most `MAX_LIST_SPAN_MS` apart.
fn list_range(message: &Value) -> Result<(String, String), &'static str> {
    let text = |key: &str| {
        message
            .get(key)
            .and_then(Value::as_str)
            .ok_or(INVALID_BOUNDS)
    };
    let (from, to) = (text("visitedFrom")?, text("visitedTo")?);
    visit_days::checked_bounds(&[from, to])?;
    let span =
        visit_days::timestamp_ms(to).unwrap_or(0) - visit_days::timestamp_ms(from).unwrap_or(0);
    if span > MAX_LIST_SPAN_MS {
        return Err(INVALID_BOUNDS);
    }
    Ok((from.to_owned(), to.to_owned()))
}

struct Visit {
    id: String,
    url: String,
    title: String,
    at: String,
}

// ------------------------------------------------------------------------------------------ history_list

struct ListRequest {
    from: String,
    to: String,
    filter: Filter,
    page: u64,
    size: u64,
}

fn parse_list(message: &Value) -> Result<ListRequest, &'static str> {
    let (from, to) = list_range(message)?;
    let filter = Filter::parse(message)?;
    let page = match message.get("page") {
        None => 1,
        Some(value) => value
            .as_u64()
            .filter(|page| *page >= 1)
            .ok_or("invalid_page")?,
    };
    let size = match message.get("size") {
        None => DEFAULT_SIZE,
        Some(value) => value
            .as_u64()
            .filter(|size| SIZES.contains(size))
            .ok_or("invalid_size")?,
    };
    Ok(ListRequest {
        from,
        to,
        filter,
        page,
        size,
    })
}

fn item(visit: &Visit) -> Value {
    let (url, url_cut) = clip(&visit.url, LIST_URL_CHARS);
    let (title, title_cut) = clip(&visit.title, LIST_TITLE_CHARS);
    json!({
        "id": visit.id, "visitedAt": visit.at, "url": url, "title": title,
        "host": host_of(&visit.url).unwrap_or_default(),
        "urlTruncated": url_cut, "titleTruncated": title_cut,
    })
}

fn run_list(db: &Connection, request: &ListRequest) -> Result<Value, &'static str> {
    // One snapshot for the count and the page.
    let tx = db.unchecked_transaction().map_err(db_error)?;
    let offset = (request.page - 1).saturating_mul(request.size);
    let read = |row: &rusqlite::Row| -> rusqlite::Result<Visit> {
        Ok(Visit {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
            at: row.get(3)?,
        })
    };
    let ordered = format!(
        "SELECT event_id, url, title, visited_at FROM visits WHERE visited_at >= ?1 AND visited_at < ?2 AND {VALID_VISITED_AT} ORDER BY visited_at DESC, event_id DESC"
    );
    // No host column: the day's rows are streamed in order and judged one by one, counted in the same pass,
    // and only the page is kept (an unfiltered list takes the same path: one scan, not two).
    let (total, page): (u64, Vec<Visit>) = {
        let mut statement = tx.prepare(&ordered).map_err(db_error)?;
        let mut rows = statement
            .query(params![request.from, request.to])
            .map_err(db_error)?;
        let (mut total, mut page) = (0u64, Vec::new());
        while let Some(row) = rows.next().map_err(db_error)? {
            let visit = read(row).map_err(db_error)?;
            if !request.filter.matches(&visit.url) {
                continue;
            }
            if total >= offset && (page.len() as u64) < request.size {
                page.push(visit);
            }
            total += 1;
        }
        (total, page)
    };
    let items: Vec<Value> = page.iter().map(item).collect();
    let mut bytes = 4096usize;
    for value in &items {
        bytes += serde_json::to_vec(value).map_err(db_error)?.len() + 1;
        if bytes > BUDGET {
            return Err(TOO_LARGE);
        }
    }
    Ok(json!({
        "items": items, "total": total, "page": request.page, "size": request.size,
        "pages": total.div_ceil(request.size),
    }))
}

// ------------------------------------------------------------------------------------------ history_days

struct DaysRequest {
    bounds: Vec<String>,
    filter: Filter,
}

fn parse_days(message: &Value) -> Result<DaysRequest, &'static str> {
    let bounds = visit_days::parse_request(message)?;
    Ok(DaysRequest {
        bounds,
        filter: Filter::parse(message)?,
    })
}

fn run_days(db: &Connection, request: &DaysRequest) -> Result<Value, &'static str> {
    let tx = db.unchecked_transaction().map_err(db_error)?;
    let counts = if request.filter.is_all() {
        visit_days::count_visits(&tx, &request.bounds).map_err(db_error)?
    } else {
        let (Some(first), Some(last)) = (request.bounds.first(), request.bounds.last()) else {
            return Err(INVALID_BOUNDS);
        };
        let mut counts = vec![0u64; request.bounds.len() - 1];
        let mut statement = tx
            .prepare(&format!("SELECT url, visited_at FROM visits WHERE visited_at >= ?1 AND visited_at < ?2 AND {VALID_VISITED_AT}"))
            .map_err(db_error)?;
        let mut rows = statement.query([first, last]).map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            let (url, at): (String, String) =
                (row.get(0).map_err(db_error)?, row.get(1).map_err(db_error)?);
            let index = request
                .bounds
                .partition_point(|bound| bound.as_str() <= at.as_str());
            if let Some(count) = index.checked_sub(1).and_then(|i| counts.get_mut(i)) {
                if request.filter.matches(&url) {
                    *count += 1;
                }
            }
        }
        counts
    };
    Ok(json!({ "counts": counts, "total": counts.iter().sum::<u64>() }))
}

// ----------------------------------------------------------------------------------------- history_visit

fn parse_visit(message: &Value) -> Result<String, &'static str> {
    message
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| navigation::is_visit_id(id))
        .map(str::to_owned)
        .ok_or("invalid_id")
}

fn run_visit(db: &Connection, id: &str) -> Result<Value, &'static str> {
    // The visit and everything around it from one snapshot: a deletion in between shows as not found.
    let tx = db.unchecked_transaction().map_err(db_error)?;
    let one = |sql: &str, id: &str| -> Result<Option<Visit>, &'static str> {
        tx.query_row(sql, [id], |row| {
            Ok(Visit {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                at: row.get(3)?,
            })
        })
        .optional()
        .map_err(db_error)
    };
    let Some(visit) = one(
        &format!("SELECT event_id, url, title, visited_at FROM visits WHERE event_id = ?1 AND {VALID_VISITED_AT}"),
        id,
    )?
    else {
        return Err("visit_not_found");
    };
    type Row = (
        Option<String>,
        bool,
        String,
        String,
        bool,
        bool,
        bool,
        bool,
        bool,
        String,
    );
    let row: Option<Row> = tx
        .query_row(
            "SELECT source_visit_id, source_censored, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at FROM navigation_events WHERE target_visit_id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?)),
        )
        .optional()
        .map_err(db_error)?;
    let (navigation_value, source) = match &row {
        None => (Value::Null, json!({ "state": "unknown" })),
        Some((source, censored, kind, transition, client, server, back, bar, new_tab, at)) => {
            let source = match navigation::source_state(source.as_deref(), *censored) {
                Some(SourceState::Present(source_id)) => {
                    match one(
                        "SELECT event_id, url, title, visited_at FROM visits WHERE event_id = ?1",
                        source_id,
                    )? {
                        Some(found) => json!({ "state": "present", "visit": item(&found) }),
                        None => json!({ "state": "unknown" }),
                    }
                }
                Some(SourceState::Censored) => json!({ "state": "censored" }),
                Some(SourceState::Unknown) | None => json!({ "state": "unknown" }),
            };
            (
                json!({
                    "kind": kind, "transitionType": transition, "clientRedirect": client, "serverRedirect": server,
                    "forwardBack": back, "fromAddressBar": bar, "newTab": new_tab, "occurredAt": at,
                }),
                source,
            )
        }
    };
    let target_count: i64 = tx
        .query_row(
            "SELECT count(*) FROM navigation_events WHERE source_visit_id = ?1",
            [id],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    let mut statement = tx
        .prepare(
            "SELECT v.event_id, v.url, v.title, v.visited_at, n.kind, n.transition_type, n.occurred_at
             FROM navigation_events n JOIN visits v ON v.event_id = n.target_visit_id
             WHERE n.source_visit_id = ?1 ORDER BY v.visited_at ASC, v.event_id ASC LIMIT ?2",
        )
        .map_err(db_error)?;
    let targets = statement
        .query_map(params![id, TARGET_LIMIT as i64], |row| {
            let found = Visit {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                at: row.get(3)?,
            };
            let mut value = item(&found);
            value["kind"] = json!(row.get::<_, String>(4)?);
            value["transitionType"] = json!(row.get::<_, String>(5)?);
            value["occurredAt"] = json!(row.get::<_, String>(6)?);
            Ok(value)
        })
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    drop(statement);
    let page_metadata = tx
        .query_row(
            "SELECT title, description, og_title, og_description, og_site_name, og_image_url, favicon_url, captured_at FROM page_metadata WHERE event_id = ?1",
            [id],
            |row| {
                Ok(json!({
                    "title": row.get::<_, Option<String>>(0)?, "description": row.get::<_, Option<String>>(1)?,
                    "ogTitle": row.get::<_, Option<String>>(2)?, "ogDescription": row.get::<_, Option<String>>(3)?,
                    "ogSiteName": row.get::<_, Option<String>>(4)?, "ogImageUrl": row.get::<_, Option<String>>(5)?,
                    "faviconUrl": row.get::<_, Option<String>>(6)?, "capturedAt": row.get::<_, String>(7)?,
                }))
            },
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(Value::Null);
    let body = json!({
        "visit": { "id": visit.id, "visitedAt": visit.at, "url": visit.url, "title": visit.title, "host": host_of(&visit.url).unwrap_or_default() },
        "navigation": navigation_value,
        "source": source,
        "targets": targets,
        "targetCount": target_count,
        "targetsTruncated": target_count as usize > TARGET_LIMIT,
        "pageMetadata": page_metadata,
    });
    if serde_json::to_vec(&body).map_err(db_error)?.len() + 4096 > BUDGET {
        return Err(TOO_LARGE);
    }
    Ok(body)
}

// --------------------------------------------------------------------------------------------- dispatch

fn respond<R>(
    session: &mut Session,
    message: &Value,
    result: &'static str,
    parsed: Result<R, &'static str>,
    run: impl FnOnce(&Connection, &R) -> Result<Value, &'static str>,
) -> Value {
    // Everything that can be refused without the vault is refused before it is opened.
    let request = match parsed {
        Ok(request) => request,
        Err(code) => return failure(result, message, code),
    };
    match session.read_with_token(|db| run(db, &request)) {
        Ok((token, Ok(body))) => success(result, message, token, body),
        Ok((_, Err(code))) => failure(result, message, code),
        Err(error) => failure(result, message, error.code()),
    }
}

/// `history_list`, `history_days` or `history_visit`.
pub fn handle(session: &mut Session, message: &Value) -> Value {
    let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
    let result = match kind {
        "history_list" => "history_list_result",
        "history_days" => "history_days_result",
        "history_visit" => "history_visit_result",
        _ => return failure("history_result", message, "unknown_type"),
    };
    if request_id(message).is_none() {
        return failure(result, message, "invalid_request_id");
    }
    match kind {
        "history_list" => respond(session, message, result, parse_list(message), run_list),
        "history_days" => respond(session, message, result, parse_days(message), run_days),
        _ => respond(session, message, result, parse_visit(message), |db, id| {
            run_visit(db, id)
        }),
    }
}

#[cfg(test)]
mod tests;
