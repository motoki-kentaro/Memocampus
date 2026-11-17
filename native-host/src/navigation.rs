//! Navigation facts sent with a visit (Issue #41, Slice 1): how the browser reported arriving at the
//! visit's document. The extension observes them (Slice 2); the host checks their shape here and
//! stores them with the visit, in its transaction (`main.rs`).
//!
//! ```json
//! "navigation": {
//!   "sourceVisitId": null | "<visit id>", "kind": "document" | "history_state",
//!   "transitionType": "link" | … | "unknown",
//!   "clientRedirect": false, "serverRedirect": false, "forwardBack": false,
//!   "fromAddressBar": false, "newTab": false, "occurredAt": "YYYY-MM-DDTHH:MM:SS.sssZ"
//! }
//! ```
//!
//! Every key is required and no other is accepted; booleans are JSON booleans; an unknown `kind` or
//! `transitionType` is refused, never mapped to `"unknown"` (the extension sends `"unknown"` itself
//! when Chrome reports nothing usable). `sourceVisitId` is a claim from the extension, not a proven
//! fact: the host only checks that it names a stored visit that the policy allows (`main.rs`). It
//! means "the document shown before", never "the page that was clicked".
//!
//! A navigation that fails any check is refused as a whole; the visit it came with is not affected.

use serde_json::Value;

use crate::page_metadata::is_timestamp;

/// Visit ids are the extension's `crypto.randomUUID()`; anything printable up to this length is
/// accepted as an id to look up, as for the visit's own id. Longer or other text names no visit.
pub const MAX_VISIT_ID: usize = 128;

pub const TRANSITION_TYPES: [&str; 12] = [
    "link",
    "typed",
    "auto_bookmark",
    "auto_subframe",
    "manual_subframe",
    "generated",
    "start_page",
    "form_submit",
    "reload",
    "keyword",
    "keyword_generated",
    "unknown",
];

pub const KINDS: [&str; 2] = ["document", "history_state"];

const KEYS: [&str; 9] = [
    "sourceVisitId",
    "kind",
    "transitionType",
    "clientRedirect",
    "serverRedirect",
    "forwardBack",
    "fromAddressBar",
    "newTab",
    "occurredAt",
];

/// What the source of a stored navigation is (Issue #7, `navigation_events.source_censored`). Three
/// states a reader must not confuse: a visit that is there, a visit the person deleted on purpose, and a
/// source that was never known, stored or allowed (a bare NULL). Only the deletion sets the marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState<'a> {
    Present(&'a str),
    Censored,
    Unknown,
}

/// What a reader shows where a deleted source stood. Display only: it is never stored in place of a URL.
pub const CENSORED_LABEL: &str = "[CENSORED]";

/// The state of a row's source. A censored row with a source is no row this host writes (the table's
/// CHECK refuses it): `None`, so a reader never shows a guess.
pub fn source_state(source: Option<&str>, censored: bool) -> Option<SourceState<'_>> {
    match (source, censored) {
        (Some(id), false) => Some(SourceState::Present(id)),
        (None, true) => Some(SourceState::Censored),
        (None, false) => Some(SourceState::Unknown),
        (Some(_), true) => None,
    }
}

impl SourceState<'_> {
    /// The text for a source: the visit as the caller names it, `[CENSORED]`, or the caller's "unknown".
    pub fn label(self, present: impl FnOnce(&str) -> String, unknown: &str) -> String {
        match self {
            Self::Present(id) => present(id),
            Self::Censored => CENSORED_LABEL.to_owned(),
            Self::Unknown => unknown.to_owned(),
        }
    }
}

/// One navigation, checked, as stored in `navigation_events` (the target is the visit itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Navigation {
    pub source_visit_id: Option<String>,
    pub kind: &'static str,
    pub transition_type: &'static str,
    pub client_redirect: bool,
    pub server_redirect: bool,
    pub forward_back: bool,
    pub from_address_bar: bool,
    pub new_tab: bool,
    pub occurred_at: String,
}

/// The navigation of the visit `target_visit_id`, or None when it is not exactly in the form above.
pub fn parse(navigation: &Value, target_visit_id: &str) -> Option<Navigation> {
    let object = navigation.as_object()?;
    if object.len() != KEYS.len() || !KEYS.iter().all(|key| object.contains_key(*key)) {
        return None;
    }
    let source_visit_id = match &object["sourceVisitId"] {
        Value::Null => None,
        Value::String(id) if is_visit_id(id) && id != target_visit_id => Some(id.clone()),
        _ => return None,
    };
    let one_of = |key: &str, allowed: &[&'static str]| -> Option<&'static str> {
        let raw = object[key].as_str()?;
        allowed.iter().copied().find(|value| *value == raw)
    };
    let flag = |key: &str| object[key].as_bool();
    let occurred_at = object["occurredAt"].as_str()?;
    if !is_calendar_timestamp(occurred_at) {
        return None;
    }
    Some(Navigation {
        source_visit_id,
        kind: one_of("kind", &KINDS)?,
        transition_type: one_of("transitionType", &TRANSITION_TYPES)?,
        client_redirect: flag("clientRedirect")?,
        server_redirect: flag("serverRedirect")?,
        forward_back: flag("forwardBack")?,
        from_address_bar: flag("fromAddressBar")?,
        new_tab: flag("newTab")?,
        occurred_at: occurred_at.to_owned(),
    })
}

/// 1 to `MAX_VISIT_ID` printable ASCII characters, no space.
pub fn is_visit_id(raw: &str) -> bool {
    (1..=MAX_VISIT_ID).contains(&raw.len()) && raw.bytes().all(|byte| byte.is_ascii_graphic())
}

/// `is_timestamp`, and a day that exists in that month (no 31 April, 29 February only in leap years).
pub fn is_calendar_timestamp(raw: &str) -> bool {
    if !is_timestamp(raw) {
        return false;
    }
    let number = |range: std::ops::Range<usize>| raw[range].parse::<u32>().unwrap_or(u32::MAX);
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    day <= days
}

#[cfg(test)]
mod tests;
