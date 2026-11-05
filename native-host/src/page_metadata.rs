//! Light page metadata (Issue #22): what the extension read from a page's DOM once it loaded, sent
//! with that page's visit. Best effort: anything wrong with it leaves the visit as it is.
//!
//! The extension already shapes the values with the same rules (`extension/page-metadata.js`); the
//! host applies them again, since what arrives is page text and never trusted:
//! - Text: every White_Space character becomes a space, every other control character (Cc) is
//!   removed, runs of spaces become one, the ends are trimmed, and the result is cut to its limit in
//!   Unicode scalar values (no ellipsis). Nothing left means NULL.
//! - URLs: kept exactly as sent (query included) when they are absolute `http`/`https` URLs without
//!   credentials, spaces or control characters, at most `MAX_URL` bytes; any other value is NULL.
//!   Never fetched.
//! - `capturedAt`: exactly `YYYY-MM-DDTHH:MM:SS.sssZ`, so it orders as text.
//!
//! A `page` that is not an object, has a field of the wrong type or a bad `capturedAt` is refused as a
//! whole and no row changes; a missing `page` means the DOM could not be read, so no row changes
//! either. Values never appear in errors or logs.

use serde_json::Value;

pub const MAX_TITLE: usize = 500;
pub const MAX_SITE_NAME: usize = 200;
pub const MAX_DESCRIPTION: usize = 1000;
pub const MAX_URL: usize = 2048;

/// One page's metadata, as stored in `page_metadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_site_name: Option<String>,
    pub og_image_url: Option<String>,
    pub favicon_url: Option<String>,
    pub captured_at: String,
}

/// The shaped metadata of `page`, or None when it must not change anything.
pub fn parse(page: &Value) -> Option<PageMetadata> {
    let page = page.as_object()?;
    let text = |key: &str, limit: usize| -> Option<Option<String>> {
        match page.get(key) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(raw)) => Some(clean_text(raw, limit)),
            Some(_) => None,
        }
    };
    let url = |key: &str| -> Option<Option<String>> {
        match page.get(key) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(raw)) => Some(clean_url(raw)),
            Some(_) => None,
        }
    };
    let captured_at = page.get("capturedAt")?.as_str()?;
    if !is_timestamp(captured_at) {
        return None;
    }
    Some(PageMetadata {
        title: text("title", MAX_TITLE)?,
        description: text("description", MAX_DESCRIPTION)?,
        og_title: text("ogTitle", MAX_TITLE)?,
        og_description: text("ogDescription", MAX_DESCRIPTION)?,
        og_site_name: text("ogSiteName", MAX_SITE_NAME)?,
        og_image_url: url("ogImageUrl")?,
        favicon_url: url("faviconUrl")?,
        captured_at: captured_at.to_owned(),
    })
}

/// Whitespace to single spaces, other control characters out, trimmed, cut to `limit` characters.
pub fn clean_text(raw: &str, limit: usize) -> Option<String> {
    let mut out = String::new();
    let mut count = 0;
    let mut pending_space = false;
    for c in raw.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if c.is_control() {
            continue;
        }
        if pending_space {
            if count + 1 >= limit {
                break;
            }
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        if count >= limit {
            break;
        }
        out.push(c);
        count += 1;
    }
    (!out.is_empty()).then_some(out)
}

/// The URL as sent, if it may be kept; never changed, so a query stays exactly as it was.
pub fn clean_url(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw.len() > MAX_URL
        || raw.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let parsed = url::Url::parse(raw).ok()?;
    let allowed = matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some_and(|host| !host.is_empty())
        && parsed.username().is_empty()
        && parsed.password().is_none();
    allowed.then(|| raw.to_owned())
}

/// `YYYY-MM-DDTHH:MM:SS.sssZ`, as `Date.prototype.toISOString` writes it for years 0000–9999.
pub fn is_timestamp(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 24 {
        return false;
    }
    let shape = b"dddd-dd-ddTdd:dd:dd.dddZ";
    if !bytes.iter().zip(shape).all(|(b, s)| match s {
        b'd' => b.is_ascii_digit(),
        other => b == other,
    }) {
        return false;
    }
    let number = |range: std::ops::Range<usize>| raw[range].parse::<u32>().unwrap_or(u32::MAX);
    (1..=12).contains(&number(5..7))
        && (1..=31).contains(&number(8..10))
        && number(11..13) <= 23
        && number(14..16) <= 59
        && number(17..19) <= 59
}

#[cfg(test)]
mod tests;
