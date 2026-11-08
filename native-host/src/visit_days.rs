//! Visit days for the URL tab's calendar (Issue #5): how many visits fall in each of a list of UTC
//! ranges, and the UTC instants such ranges are made of.
//!
//! ```text
//! → visit_days        { requestId, bounds: [b0, b1, …, bn] }
//! ← visit_days_result { ok: true, requestId, vaultToken, counts: [c0, …, c(n-1)] }
//! ← visit_days_result { ok: false, requestId, error }
//! ```
//!
//! `bounds` are the starts of consecutive local days and the start of the day after the last, as
//! the extension computes them in the person's time zone (`new Date(y, m, d).toISOString()`); the host
//! knows no time zone and corrects nothing. They must be 29 to 32 exact `Date#toISOString()` texts
//! (`timestamp_ms`), strictly increasing, spanning at most 32 days; anything else is `invalid_bounds`,
//! answered before any vault is opened. `counts[i]` is the number of visits (`visits` rows, each
//! visit of a URL counted) with `bounds[i] <= visited_at < bounds[i + 1]`. Tab snapshots are not
//! visits and are not counted. A stored `visited_at` that is not an exact, existing UTC instant
//! (`VALID_VISITED_AT`) is in no range: it is counted nowhere and changed nowhere.
//!
//! Only counts leave the host. The answer names the vault it was read from (`vaultToken`, as
//! `search_pages` and `image_list` do), so the extension can tell answers of different vaults apart.

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::search::request_id;

pub const MIN_BOUNDS: usize = 29;
pub const MAX_BOUNDS: usize = 32;
/// The longest span of `bounds`, and of a `search_pages` date range: a month of local days.
pub const MAX_SPAN_MS: i64 = 32 * 86_400_000;
pub const INVALID_BOUNDS: &str = "invalid_bounds";
const RESULT: &str = "visit_days_result";

/// Whether the stored `visited_at` is an exact, existing UTC instant in `Date#toISOString()` form:
/// the shape (`GLOB`), a date and time SQLite reads back unchanged (no February 30, no minute 60),
/// and no hour 24 (which SQLite would keep). Only such texts are compared, and between them text
/// order is time order. Put after the range test, so it costs only rows in range.
pub const VALID_VISITED_AT: &str = "visited_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    AND substr(visited_at, 12, 2) < '24'
    AND strftime('%Y-%m-%dT%H:%M:%fZ', visited_at) = visited_at";

fn digits(text: &[u8]) -> Option<i64> {
    text.iter().try_fold(0i64, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + i64::from(byte - b'0'))
    })
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's days-from-civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Milliseconds since the epoch of exactly `YYYY-MM-DDTHH:MM:SS.sssZ` naming an existing instant
/// (years 0000 to 9999, as `Date#toISOString()` writes them); None for anything else.
pub fn timestamp_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 24
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[23] != b'Z'
    {
        return None;
    }
    let year = digits(&bytes[..4])?;
    let month = digits(&bytes[5..7])?;
    let day = digits(&bytes[8..10])?;
    let hour = digits(&bytes[11..13])?;
    let minute = digits(&bytes[14..16])?;
    let second = digits(&bytes[17..19])?;
    let milli = digits(&bytes[20..23])?;
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    Some(
        ((days_from_civil(year, month, day) * 24 + hour) * 60 + minute) * 60_000
            + second * 1000
            + milli,
    )
}

/// Exact instants, strictly increasing, spanning at most `MAX_SPAN_MS`; their texts.
pub fn checked_bounds<'a>(texts: &[&'a str]) -> Result<Vec<&'a str>, &'static str> {
    let mut previous: Option<i64> = None;
    let mut first = None;
    for text in texts {
        let ms = timestamp_ms(text).ok_or(INVALID_BOUNDS)?;
        if previous.is_some_and(|previous| ms <= previous) {
            return Err(INVALID_BOUNDS);
        }
        first.get_or_insert(ms);
        previous = Some(ms);
    }
    match (first, previous) {
        (Some(first), Some(last)) if last - first <= MAX_SPAN_MS => Ok(texts.to_vec()),
        _ => Err(INVALID_BOUNDS),
    }
}

/// The `bounds` of a `visit_days` request, checked; before any vault is opened.
pub fn parse_request(message: &Value) -> Result<Vec<String>, &'static str> {
    let items = message
        .get("bounds")
        .and_then(Value::as_array)
        .filter(|items| (MIN_BOUNDS..=MAX_BOUNDS).contains(&items.len()))
        .ok_or(INVALID_BOUNDS)?;
    let texts = items
        .iter()
        .map(|item| item.as_str().ok_or(INVALID_BOUNDS))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(checked_bounds(&texts)?
        .into_iter()
        .map(str::to_owned)
        .collect())
}

/// Visits per range `[bounds[i], bounds[i + 1])`. One query over the whole span; each visit is put
/// in its range here, by text order, which is time order between valid texts.
pub fn count_visits(db: &Connection, bounds: &[String]) -> rusqlite::Result<Vec<u64>> {
    let (Some(first), Some(last)) = (bounds.first(), bounds.last()) else {
        return Ok(Vec::new());
    };
    let mut counts = vec![0u64; bounds.len().saturating_sub(1)];
    let sql = format!(
        "SELECT visited_at FROM visits WHERE visited_at >= ?1 AND visited_at < ?2 AND {VALID_VISITED_AT}"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query([first, last])?;
    while let Some(row) = rows.next()? {
        let at: String = row.get(0)?;
        // The last bound not after `at`: its range holds the visit.
        let index = bounds.partition_point(|bound| bound.as_str() <= at.as_str());
        if let Some(count) = index.checked_sub(1).and_then(|i| counts.get_mut(i)) {
            *count += 1;
        }
    }
    Ok(counts)
}

pub fn failure(message: &Value, code: &str) -> Value {
    json!({ "ok": false, "type": RESULT, "requestId": request_id(message), "error": code })
}

pub fn success(message: &Value, vault_token: String, counts: Vec<u64>) -> Value {
    json!({ "ok": true, "type": RESULT, "requestId": request_id(message), "vaultToken": vault_token, "counts": counts })
}

#[cfg(test)]
mod tests;
