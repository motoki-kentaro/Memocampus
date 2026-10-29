//! `search_pages`: read-only search over `visits` and `tab_snapshots`, aggregated by URL.
//!
//! Source rows are never modified; aggregation happens entirely inside the query.

use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};

pub const DEFAULT_LIMIT: u64 = 50;
pub const MAX_LIMIT: u64 = 100;
pub const MAX_OFFSET: u64 = 100_000;
pub const MAX_QUERY_CHARS: usize = 200;
const MAX_REQUEST_ID_CHARS: usize = 128;

#[derive(Debug, PartialEq)]
pub struct SearchRequest {
    pub query: String,
    pub limit: u64,
    pub offset: u64,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHit {
    pub url: String,
    pub title: String,
    pub last_visited_at: Option<String>,
    pub last_captured_at: Option<String>,
    pub last_activity_at: String,
    pub source: &'static str,
}

#[derive(Debug, PartialEq)]
pub struct SearchPage {
    pub items: Vec<PageHit>,
    pub has_more: bool,
}

// Each page keeps the latest non-empty title; `last_activity_at` (sort key) is the later of
// the visit and capture timestamps. Both are `Date#toISOString()` strings, so text order is time
// order. The title is picked in the same single GROUP BY pass by maximizing "timestamp<TAB>title"
// (timestamps never contain a tab), which is about twice as fast as a window function here.
const SEARCH_SQL: &str = r"
WITH pages(url, title, visited_at, captured_at) AS (
  SELECT url, title, visited_at, NULL FROM visits
  UNION ALL
  SELECT url, title, NULL, recorded_at FROM tab_snapshots
),
grouped AS (
  SELECT
    url,
    MAX(visited_at) AS last_visited_at,
    MAX(captured_at) AS last_captured_at,
    MAX(CASE WHEN title <> '' THEN COALESCE(visited_at, captured_at) || char(9) || title END) AS stamped_title
  FROM pages
  GROUP BY url
  HAVING MAX(url LIKE ?1 ESCAPE '\' OR title LIKE ?1 ESCAPE '\')
)
SELECT
  url,
  COALESCE(substr(stamped_title, instr(stamped_title, char(9)) + 1), '') AS title,
  last_visited_at,
  last_captured_at,
  MAX(COALESCE(last_visited_at, ''), COALESCE(last_captured_at, '')) AS last_activity_at
FROM grouped
ORDER BY last_activity_at DESC, url ASC
LIMIT ?2 OFFSET ?3
";

/// Escapes LIKE metacharacters so user input only ever matches literally.
pub fn like_pattern(query: &str) -> String {
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

fn optional_bounded(
    value: &Value,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, &'static str> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(raw) => raw
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .ok_or(if key == "limit" {
                "invalid_limit"
            } else {
                "invalid_offset"
            }),
    }
}

pub fn parse_request(value: &Value) -> Result<SearchRequest, &'static str> {
    let query = match value.get("query") {
        None | Some(Value::Null) => "",
        Some(Value::String(query)) => query.trim(),
        Some(_) => return Err("invalid_query"),
    };
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err("query_too_long");
    }
    Ok(SearchRequest {
        query: query.to_owned(),
        limit: optional_bounded(value, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?,
        offset: optional_bounded(value, "offset", 0, 0, MAX_OFFSET)?,
    })
}

pub fn search_pages(db: &Connection, request: &SearchRequest) -> rusqlite::Result<SearchPage> {
    let mut statement = db.prepare_cached(SEARCH_SQL)?;
    // Fetch one extra row to learn whether another page exists.
    let rows = statement.query_map(
        params![
            like_pattern(&request.query),
            request.limit + 1,
            request.offset
        ],
        |row| {
            let last_visited_at: Option<String> = row.get(2)?;
            let last_captured_at: Option<String> = row.get(3)?;
            let source = match (&last_visited_at, &last_captured_at) {
                (Some(_), Some(_)) => "both",
                (Some(_), None) => "visit",
                _ => "snapshot",
            };
            Ok(PageHit {
                url: row.get(0)?,
                title: row.get(1)?,
                last_visited_at,
                last_captured_at,
                last_activity_at: row.get(4)?,
                source,
            })
        },
    )?;
    let mut items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = items.len() as u64 > request.limit;
    items.truncate(request.limit as usize);
    Ok(SearchPage { items, has_more })
}

pub fn request_id(message: &Value) -> Option<&str> {
    message
        .get("requestId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.chars().count() <= MAX_REQUEST_ID_CHARS)
}

/// Error response for a `search_pages` message, also used when no vault is available.
pub fn failure(message: &Value, code: &str) -> Value {
    json!({ "ok": false, "type": "search_pages_result", "requestId": request_id(message), "error": code })
}

/// Builds the response for one `search_pages` message. Always returns a response so the
/// extension never waits on a silent host.
pub fn handle(db: &Connection, message: &Value) -> Value {
    let request_id = request_id(message);
    let failure = |code: &str| failure(message, code);
    let Some(request_id) = request_id else {
        return failure("invalid_request_id");
    };
    let request = match parse_request(message) {
        Ok(request) => request,
        Err(code) => return failure(code),
    };
    match search_pages(db, &request) {
        Ok(page) => json!({
            "ok": true,
            "type": "search_pages_result",
            "requestId": request_id,
            "items": page.items,
            "hasMore": page.has_more,
        }),
        // The SQLite error text may echo data, so only a fixed code leaves the host.
        Err(_) => failure("db_error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SCHEMA_SQL;

    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA_SQL).unwrap();
        db
    }

    fn visit(db: &Connection, id: &str, url: &str, title: &str, at: &str) {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, ?3, ?4)",
            params![id, url, title, at],
        )
        .unwrap();
    }

    fn snapshot(db: &Connection, id: &str, url: &str, title: &str, at: &str) {
        db.execute(
            "INSERT INTO tab_snapshots VALUES (?1, 1, 0, ?2, ?3, 0, 0, ?4)",
            params![id, url, title, at],
        )
        .unwrap();
    }

    fn search(db: &Connection, query: &str, limit: u64, offset: u64) -> SearchPage {
        search_pages(
            db,
            &SearchRequest {
                query: query.to_owned(),
                limit,
                offset,
            },
        )
        .unwrap()
    }

    fn urls(page: &SearchPage) -> Vec<&str> {
        page.items.iter().map(|hit| hit.url.as_str()).collect()
    }

    #[test]
    fn matches_url_or_title_substring_case_insensitively() {
        let db = db();
        visit(
            &db,
            "1",
            "https://www.rust-lang.org/",
            "Rust Programming Language",
            "2026-10-01T00:00:00.000Z",
        );
        visit(
            &db,
            "2",
            "https://example.test/a",
            "Learning RUST",
            "2026-10-02T00:00:00.000Z",
        );
        visit(
            &db,
            "3",
            "https://example.test/b",
            "Go",
            "2026-10-03T00:00:00.000Z",
        );
        snapshot(
            &db,
            "4",
            "https://news.test/",
            "日本語のニュース",
            "2026-10-04T00:00:00.000Z",
        );

        assert_eq!(
            urls(&search(&db, "rust", 50, 0)),
            ["https://example.test/a", "https://www.rust-lang.org/"]
        );
        assert_eq!(
            urls(&search(&db, "example.test/b", 50, 0)),
            ["https://example.test/b"]
        );
        assert_eq!(
            urls(&search(&db, "ニュース", 50, 0)),
            ["https://news.test/"]
        );
    }

    #[test]
    fn empty_query_lists_everything_newest_first() {
        let db = db();
        visit(&db, "1", "https://a.test/", "A", "2026-10-01T00:00:00.000Z");
        snapshot(&db, "2", "https://b.test/", "B", "2026-10-03T00:00:00.000Z");
        visit(&db, "3", "https://c.test/", "C", "2026-10-02T00:00:00.000Z");
        // Ties fall back to URL order so pagination is deterministic.
        visit(&db, "4", "https://d.test/", "D", "2026-10-02T00:00:00.000Z");

        assert_eq!(
            urls(&search(&db, "", 50, 0)),
            [
                "https://b.test/",
                "https://c.test/",
                "https://d.test/",
                "https://a.test/"
            ]
        );
    }

    #[test]
    fn aggregates_by_url_keeping_visit_and_capture_times_distinct() {
        let db = db();
        let url = "https://both.test/";
        visit(&db, "v1", url, "Old title", "2026-10-01T00:00:00.000Z");
        visit(&db, "v2", url, "New title", "2026-10-05T00:00:00.000Z");
        snapshot(&db, "s1", url, "", "2026-10-09T00:00:00.000Z");
        visit(
            &db,
            "v3",
            "https://visit.test/",
            "V",
            "2026-10-02T00:00:00.000Z",
        );
        snapshot(
            &db,
            "s2",
            "https://snap.test/",
            "S",
            "2026-10-03T00:00:00.000Z",
        );

        let page = search(&db, "", 50, 0);
        assert_eq!(page.items.len(), 3);
        assert_eq!(
            page.items[0],
            PageHit {
                url: url.to_owned(),
                title: "New title".to_owned(), // latest non-empty title wins over the empty snapshot title
                last_visited_at: Some("2026-10-05T00:00:00.000Z".to_owned()),
                last_captured_at: Some("2026-10-09T00:00:00.000Z".to_owned()),
                last_activity_at: "2026-10-09T00:00:00.000Z".to_owned(),
                source: "both",
            }
        );
        assert_eq!(page.items[1].source, "snapshot");
        assert_eq!(page.items[1].last_visited_at, None);
        assert_eq!(page.items[2].source, "visit");
        assert_eq!(page.items[2].last_captured_at, None);
    }

    #[test]
    fn titles_with_tabs_survive_and_untitled_pages_get_an_empty_title() {
        let db = db();
        visit(
            &db,
            "1",
            "https://tab.test/",
            "a\tb",
            "2026-10-01T00:00:00.000Z",
        );
        visit(
            &db,
            "2",
            "https://untitled.test/",
            "",
            "2026-10-02T00:00:00.000Z",
        );
        snapshot(
            &db,
            "3",
            "https://untitled.test/",
            "",
            "2026-10-03T00:00:00.000Z",
        );
        let page = search(&db, "", 50, 0);
        assert_eq!(page.items[0].url, "https://untitled.test/");
        assert_eq!(page.items[0].title, "");
        assert_eq!(page.items[1].title, "a\tb");
    }

    #[test]
    fn a_page_matches_through_any_of_its_historical_titles() {
        let db = db();
        visit(
            &db,
            "1",
            "https://x.test/",
            "Rust notes",
            "2026-10-01T00:00:00.000Z",
        );
        visit(
            &db,
            "2",
            "https://x.test/",
            "Renamed",
            "2026-10-02T00:00:00.000Z",
        );
        let page = search(&db, "rust", 50, 0);
        assert_eq!(urls(&page), ["https://x.test/"]);
        assert_eq!(page.items[0].title, "Renamed");
    }

    #[test]
    fn paginates_with_has_more_and_stable_boundaries() {
        let db = db();
        for i in 0..5 {
            visit(
                &db,
                &i.to_string(),
                &format!("https://p{i}.test/"),
                "P",
                &format!("2026-10-0{}T00:00:00.000Z", i + 1),
            );
        }
        let first = search(&db, "", 2, 0);
        assert_eq!(urls(&first), ["https://p4.test/", "https://p3.test/"]);
        assert!(first.has_more);
        let last = search(&db, "", 2, 4);
        assert_eq!(urls(&last), ["https://p0.test/"]);
        assert!(!last.has_more);
        let exact = search(&db, "", 5, 0);
        assert_eq!(exact.items.len(), 5);
        assert!(!exact.has_more);
        let beyond = search(&db, "", 2, 10);
        assert!(beyond.items.is_empty());
        assert!(!beyond.has_more);
    }

    #[test]
    fn empty_database_and_no_match_return_empty_results() {
        let db = db();
        assert_eq!(
            search(&db, "", 50, 0),
            SearchPage {
                items: vec![],
                has_more: false
            }
        );
        visit(&db, "1", "https://a.test/", "A", "2026-10-01T00:00:00.000Z");
        assert_eq!(
            search(&db, "zzz", 50, 0),
            SearchPage {
                items: vec![],
                has_more: false
            }
        );
    }

    #[test]
    fn like_metacharacters_match_literally() {
        let db = db();
        visit(
            &db,
            "1",
            "https://a.test/100%25",
            "100% done",
            "2026-10-01T00:00:00.000Z",
        );
        visit(
            &db,
            "2",
            "https://b.test/plain",
            "1000 done",
            "2026-10-02T00:00:00.000Z",
        );
        visit(
            &db,
            "3",
            "https://c.test/snake_case",
            "x",
            "2026-10-03T00:00:00.000Z",
        );
        visit(
            &db,
            "4",
            "https://d.test/snakeXcase",
            "x",
            "2026-10-04T00:00:00.000Z",
        );
        visit(
            &db,
            "5",
            "https://e.test/",
            r"C:\path",
            "2026-10-05T00:00:00.000Z",
        );

        assert_eq!(urls(&search(&db, "100%", 50, 0)), ["https://a.test/100%25"]);
        assert_eq!(
            urls(&search(&db, "snake_case", 50, 0)),
            ["https://c.test/snake_case"]
        );
        assert_eq!(urls(&search(&db, r"c:\p", 50, 0)), ["https://e.test/"]);
        assert_eq!(like_pattern(r"a%b_c\d"), r"%a\%b\_c\\d%");
    }

    #[test]
    fn hostile_query_is_bound_as_data_and_rows_stay_untouched() {
        let db = db();
        visit(&db, "1", "https://a.test/", "A", "2026-10-01T00:00:00.000Z");
        snapshot(&db, "2", "https://a.test/", "A", "2026-10-02T00:00:00.000Z");
        assert!(search(&db, "'); DROP TABLE visits; --", 50, 0)
            .items
            .is_empty());
        search(&db, "", 50, 0);
        let counts: (i64, i64) = db
            .query_row(
                "SELECT (SELECT count(*) FROM visits), (SELECT count(*) FROM tab_snapshots)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1));
    }

    #[test]
    fn parse_request_applies_defaults_and_trims() {
        assert_eq!(
            parse_request(&json!({ "query": "  rust  " })).unwrap(),
            SearchRequest {
                query: "rust".to_owned(),
                limit: DEFAULT_LIMIT,
                offset: 0
            }
        );
        assert_eq!(
            parse_request(&json!({ "query": null, "limit": null }))
                .unwrap()
                .query,
            ""
        );
        assert_eq!(
            parse_request(&json!({ "limit": MAX_LIMIT, "offset": MAX_OFFSET }))
                .unwrap()
                .limit,
            MAX_LIMIT
        );
        let at_limit = "あ".repeat(MAX_QUERY_CHARS);
        assert!(parse_request(&json!({ "query": at_limit })).is_ok());
    }

    #[test]
    fn parse_request_rejects_abnormal_values() {
        let cases = [
            (json!({ "query": 42 }), "invalid_query"),
            (
                json!({ "query": "x".repeat(MAX_QUERY_CHARS + 1) }),
                "query_too_long",
            ),
            (json!({ "limit": 0 }), "invalid_limit"),
            (json!({ "limit": MAX_LIMIT + 1 }), "invalid_limit"),
            (json!({ "limit": -1 }), "invalid_limit"),
            (json!({ "limit": 1.5 }), "invalid_limit"),
            (json!({ "limit": "10" }), "invalid_limit"),
            (json!({ "offset": -1 }), "invalid_offset"),
            (json!({ "offset": MAX_OFFSET + 1 }), "invalid_offset"),
            (json!({ "offset": 1e30 }), "invalid_offset"),
        ];
        for (message, expected) in cases {
            assert_eq!(parse_request(&message), Err(expected), "{message}");
        }
    }

    #[test]
    fn handle_returns_tagged_responses() {
        let db = db();
        visit(&db, "1", "https://a.test/", "A", "2026-10-01T00:00:00.000Z");
        let ok = handle(
            &db,
            &json!({ "type": "search_pages", "requestId": "r1", "query": "a.test" }),
        );
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["type"], "search_pages_result");
        assert_eq!(ok["requestId"], "r1");
        assert_eq!(ok["hasMore"], false);
        assert_eq!(ok["items"][0]["lastVisitedAt"], "2026-10-01T00:00:00.000Z");
        assert_eq!(ok["items"][0]["lastCapturedAt"], Value::Null);
        assert_eq!(ok["items"][0]["source"], "visit");

        let bad = handle(
            &db,
            &json!({ "type": "search_pages", "requestId": "r2", "limit": 0 }),
        );
        assert_eq!(
            bad,
            json!({ "ok": false, "type": "search_pages_result", "requestId": "r2", "error": "invalid_limit" })
        );

        let no_id = handle(&db, &json!({ "type": "search_pages" }));
        assert_eq!(no_id["error"], "invalid_request_id");
        assert_eq!(no_id["requestId"], Value::Null);
    }

    #[test]
    fn handle_reports_db_errors_without_details() {
        let db = Connection::open_in_memory().unwrap(); // no tables
        let response = handle(&db, &json!({ "requestId": "r1" }));
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"], "db_error");
    }
}
