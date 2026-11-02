//! Connects the privacy policy to recording: `policy_status` and `policy_initialize` messages, and
//! the decision for each automatically recorded event. A decision exists only if a valid policy
//! was loaded; a failure to load is reported as an error and never as a refusal of the URL.
//!
//! `policy_evaluate_batch` is what the extension asks before it keeps a URL anywhere:
//!
//! ```text
//! → { type: "policy_evaluate_batch", requestId, items: [{ id, url }] }
//! ← { ok: true, type: "policy_evaluate_batch_result", requestId, etag, revision, items: [{ id, decision }] }
//! ← { ok: false, type: "policy_evaluate_batch_result", requestId, error }
//! ```
//!
//! A batch is answered whole or not at all, and the answer holds no URL or rule. `decision` is
//! `allow`, `save_but_no_mcp` or `never_save`; a malformed URL is `never_save`. Nothing here is a
//! permit: each event is judged again against the file when it is stored.

use memocampus_native_host::policy::{CompiledPolicy, Decision};
use memocampus_native_host::policy_store::{PolicyStore, Snapshot, StoreError};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;

use crate::search::request_id;

pub const NOT_INITIALIZED: &str = "policy_not_initialized";
pub const INVALID: &str = "policy_invalid";
pub const BUSY: &str = "policy_busy";
pub const UNAVAILABLE: &str = "policy_unavailable";
const ALREADY_INITIALIZED: &str = "policy_already_initialized";
const INVALID_BATCH: &str = "invalid_policy_batch";
const BATCH_TOO_LARGE: &str = "policy_batch_too_large";
const BATCH_RESULT: &str = "policy_evaluate_batch_result";

/// An answer must stay far below Chrome's limit of 1 MB from a host: at about 80 bytes per item
/// this is under 100 kB. The extension splits longer lists.
pub const MAX_BATCH_ITEMS: usize = 1000;
const MAX_BATCH_ID_CHARS: usize = 32;

/// A writer holds the lock only for one small file replacement.
const BUSY_RETRIES: u32 = 5;
const BUSY_WAIT: Duration = Duration::from_millis(20);

pub struct PolicyGate {
    store: Result<PolicyStore, StoreError>,
}

impl PolicyGate {
    pub fn from_process() -> Self {
        Self {
            store: PolicyStore::from_process(),
        }
    }

    fn load(&self) -> Result<Snapshot, &'static str> {
        let store = self.store.as_ref().map_err(|_| UNAVAILABLE)?;
        let mut attempt = 0;
        loop {
            match store.load() {
                Err(StoreError::Busy) if attempt < BUSY_RETRIES => {
                    attempt += 1;
                    std::thread::sleep(BUSY_WAIT);
                }
                Ok(snapshot) => return Ok(snapshot),
                Err(StoreError::Missing) if store.never_initialized() => {
                    return Err(NOT_INITIALIZED)
                }
                Err(StoreError::Busy) => return Err(BUSY),
                Err(StoreError::Missing | StoreError::Invalid) => return Err(INVALID),
                Err(_) => return Err(UNAVAILABLE),
            }
        }
    }

    /// The latest policy's decision. `Err` means no decision exists: nothing may be stored, and
    /// the caller must not acknowledge the event as dropped.
    pub fn decide(&self, url: &str) -> Result<Decision, &'static str> {
        self.load().map(|snapshot| snapshot.policy.evaluate(url))
    }

    pub fn evaluate_batch(&self, message: &Value) -> Value {
        let Some(id) = request_id(message) else {
            return json!({ "ok": false, "type": BATCH_RESULT, "requestId": null, "error": "invalid_request_id" });
        };
        match self.judge_batch(message) {
            Ok((etag, revision, items)) => json!({
                "ok": true, "type": BATCH_RESULT, "requestId": id, "etag": etag,
                "revision": revision, "items": items,
            }),
            Err(code) => {
                json!({ "ok": false, "type": BATCH_RESULT, "requestId": id, "error": code })
            }
        }
    }

    /// The shape first, so a bad request never reads the policy; one policy for the whole batch.
    fn judge_batch(&self, message: &Value) -> Result<(String, u64, Vec<Value>), &'static str> {
        let items = batch_items(message)?;
        let snapshot = self.load()?;
        let compiled = CompiledPolicy::compile(&snapshot.policy).map_err(|_| INVALID)?;
        let answers = items
            .into_iter()
            .map(|(id, url)| json!({ "id": id, "decision": decision_code(compiled.evaluate(url)) }))
            .collect();
        Ok((snapshot.etag, snapshot.policy.revision, answers))
    }

    pub fn handle(&self, message: &Value) -> Value {
        let Some(id) = request_id(message) else {
            return json!({ "ok": false, "type": "policy_result", "requestId": null, "error": "invalid_request_id" });
        };
        let initialize = message.get("type").and_then(Value::as_str) == Some("policy_initialize");
        let loaded = if initialize {
            self.initialize()
        } else {
            self.load()
        };
        match loaded {
            Ok(snapshot) => json!({
                "ok": true, "type": "policy_result", "requestId": id, "state": "ready",
                "policy": snapshot.policy, "etag": snapshot.etag,
            }),
            Err(code) if initialize => {
                json!({ "ok": false, "type": "policy_result", "requestId": id, "error": code })
            }
            Err(code) => {
                json!({ "ok": true, "type": "policy_result", "requestId": id, "state": state_of(code) })
            }
        }
    }

    /// Never replaces anything: a damaged or lost policy is refused, not reset.
    fn initialize(&self) -> Result<Snapshot, &'static str> {
        let store = self.store.as_ref().map_err(|_| UNAVAILABLE)?;
        store.initialize().map_err(|error| match error {
            StoreError::AlreadyInitialized => ALREADY_INITIALIZED,
            StoreError::Busy => BUSY,
            StoreError::Invalid => INVALID,
            _ => UNAVAILABLE,
        })
    }
}

fn batch_items(message: &Value) -> Result<Vec<(&str, &str)>, &'static str> {
    let items = message
        .get("items")
        .and_then(Value::as_array)
        .ok_or(INVALID_BATCH)?;
    if items.len() > MAX_BATCH_ITEMS {
        return Err(BATCH_TOO_LARGE);
    }
    let mut seen = HashSet::with_capacity(items.len());
    items
        .iter()
        .map(|item| {
            let text = |key| item.get(key).and_then(Value::as_str).ok_or(INVALID_BATCH);
            let (id, url) = (text("id")?, text("url")?);
            let usable = !id.is_empty()
                && id.len() <= MAX_BATCH_ID_CHARS
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
            if usable && seen.insert(id) {
                Ok((id, url))
            } else {
                Err(INVALID_BATCH)
            }
        })
        .collect()
}

fn decision_code(decision: Decision) -> &'static str {
    match decision {
        Decision::NeverSave => "never_save",
        Decision::SaveButNoMcp => "save_but_no_mcp",
        Decision::None => "allow",
    }
}

fn state_of(code: &str) -> &'static str {
    match code {
        NOT_INITIALIZED => "uninitialized",
        BUSY => "busy",
        INVALID => "invalid",
        _ => "unavailable",
    }
}
