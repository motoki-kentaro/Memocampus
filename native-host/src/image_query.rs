//! Read-only image queries for the side panel's Image tab (one-shot messages, like search).
//!
//! Chrome rejects host messages over 1 MB, so every reply is assembled against RESPONSE_BUDGET:
//! a list page that would exceed it ends early (its `next` cursor resumes exactly there), and
//! thumbnails that do not fit are returned as `deferred` for another request. Originals are never
//! sent from here. Each reply names the vault it came from (`vaultToken`) so the panel can drop
//! answers from a vault that is no longer active.

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::images::{self, ImageCursor, ImageError, MAX_PAGE_SIZE};
use crate::search::request_id;
use crate::vault::Session;

/// Serialized size limit for one reply: well below Chrome's 1 MB.
pub const RESPONSE_BUDGET: usize = 700_000;
pub const MAX_THUMBNAIL_IDS: usize = 16;
pub const MAX_SOURCES_PAGE: u32 = 20;

fn result_type(kind: &str) -> &'static str {
    match kind {
        "image_list" => "image_list_result",
        "image_thumbnails" => "image_thumbnails_result",
        _ => "image_sources_result",
    }
}

fn cursor(message: &Value) -> Result<Option<ImageCursor>, &'static str> {
    match message.get("cursor") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let last_saved_at = value.get("lastSavedAt").and_then(Value::as_i64);
            let id = value.get("id").and_then(Value::as_i64);
            match (last_saved_at, id) {
                (Some(last_saved_at), Some(id)) => Ok(Some(ImageCursor { last_saved_at, id })),
                _ => Err("invalid_cursor"),
            }
        }
    }
}

fn limit(message: &Value, max: u32) -> Result<u32, &'static str> {
    match message.get("limit") {
        None | Some(Value::Null) => Ok(max),
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=u64::from(max)).contains(n))
            .map(|n| n as u32)
            .ok_or("invalid_limit"),
    }
}

fn blob_id(value: Option<&Value>) -> Result<i64, &'static str> {
    value
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .ok_or("invalid_blob_id")
}

fn size_of(value: &impl Serialize) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() + 1)
        .unwrap_or(usize::MAX)
}

/// Takes items in order until the budget is spent (always at least one). Returns them and
/// whether the list was cut short.
fn within_budget<T: Serialize>(items: Vec<T>, budget: usize) -> (Vec<T>, bool) {
    let mut used = 0;
    let mut kept = Vec::new();
    let total = items.len();
    for item in items {
        let size = size_of(&item);
        if !kept.is_empty() && used + size > budget {
            break;
        }
        used += size;
        kept.push(item);
    }
    let cut = kept.len() < total;
    (kept, cut)
}

pub fn handle(session: &mut Session, message: &Value) -> Value {
    let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
    let id = request_id(message);
    let mut reply = match id {
        None => Err("invalid_request_id"),
        Some(_) => answer(session, kind, message),
    }
    .unwrap_or_else(|code| {
        let mut fields = Map::new();
        fields.insert("error".into(), code.into());
        fields
    });
    let ok = !reply.contains_key("error");
    reply.insert("ok".into(), ok.into());
    reply.insert("type".into(), result_type(kind).into());
    reply.insert("requestId".into(), id.into());
    Value::Object(reply)
}

fn answer(
    session: &mut Session,
    kind: &str,
    message: &Value,
) -> Result<Map<String, Value>, &'static str> {
    let ticket = session.ticket().map_err(|error| error.code())?;
    let budget = RESPONSE_BUDGET - 4096; // room for the envelope
    let mut fields = Map::new();
    fields.insert("vaultToken".into(), ticket.token.into());
    match kind {
        "image_list" => {
            let after = cursor(message)?;
            let limit = limit(message, MAX_PAGE_SIZE)?;
            let page = session
                .read(|db| images::list_images(db, after, limit))
                .map_err(|error| error.code())?
                .map_err(ImageError::code)?;
            let (items, cut) = within_budget(page.items, budget);
            let next = if cut {
                items.last().map(|last| ImageCursor {
                    last_saved_at: last.last_saved_at,
                    id: last.blob_id,
                })
            } else {
                page.next
            };
            fields.insert("items".into(), json!(items));
            fields.insert("next".into(), json!(next));
        }
        "image_thumbnails" => {
            let ids = message
                .get("blobIds")
                .and_then(Value::as_array)
                .filter(|ids| !ids.is_empty() && ids.len() <= MAX_THUMBNAIL_IDS)
                .ok_or("invalid_blob_ids")?
                .iter()
                .map(|value| blob_id(Some(value)))
                .collect::<Result<Vec<_>, _>>()?;
            let mut items = Vec::new();
            let mut deferred = Vec::new();
            let mut used = 0;
            for id in ids {
                if !deferred.is_empty() {
                    deferred.push(id);
                    continue;
                }
                let thumbnail = session
                    .read(|db| images::thumbnail(db, id))
                    .map_err(|error| error.code())?
                    .map_err(ImageError::code)?;
                let item = match thumbnail {
                    Some(t) => {
                        json!({ "blobId": id, "mimeType": t.mime_type, "width": t.width, "height": t.height, "data": base64_encode(&t.data) })
                    }
                    None => json!({ "blobId": id, "missing": true }),
                };
                let size = size_of(&item);
                if !items.is_empty() && used + size > budget {
                    deferred.push(id);
                    continue;
                }
                used += size;
                items.push(item);
            }
            fields.insert("items".into(), Value::Array(items));
            fields.insert("deferred".into(), json!(deferred));
        }
        "image_sources" => {
            let id = blob_id(message.get("blobId"))?;
            let after = cursor(message)?;
            let limit = limit(message, MAX_SOURCES_PAGE)?;
            let page = session
                .read(|db| images::image_sources(db, id, after, limit))
                .map_err(|error| error.code())?
                .map_err(ImageError::code)?;
            let (items, cut) = within_budget(page.items, budget);
            let next = if cut {
                items.last().map(|last| ImageCursor {
                    last_saved_at: last.saved_at,
                    id: last.saved_image_id,
                })
            } else {
                page.next
            };
            fields.insert("blobId".into(), id.into());
            fields.insert("items".into(), json!(items));
            fields.insert("next".into(), json!(next));
        }
        _ => return Err("unknown_type"),
    }
    Ok(fields)
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests;
