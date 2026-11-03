//! Connects the privacy policy to recording: `policy_status` and `policy_initialize` messages, and
//! the decision for each automatically recorded event. A decision exists only if a valid policy
//! was loaded; a failure to load is reported as an error and never as a refusal of the URL.
//!
//! `policy_status` says only whether a policy is usable, and which version: no rule. The document
//! itself comes only from `policy_get`, which nothing in recording asks for. `policy_migrate` is
//! the explicit step from version 1 to 2 and takes the ETag of the policy it converts.
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
//!
//! For a settings page (#23) and the first run (#24), see docs/issue-025-policy-engine.md, Slice 6:
//! `policy_validate`, `policy_test_rule` and `policy_test_url` judge a draft the caller sends and
//! change nothing; `policy_preset` returns a draft; `policy_apply` stores a draft in place of the
//! policy of a given ETag; `policy_start` stores the first policy of a new install, which is what
//! starts recording. Only the last two write, and only through `PolicyStore`.

use memocampus_native_host::policy::{self, CompiledPolicy, Decision, Policy, Problem, RuleSpec};
use memocampus_native_host::policy_store::{self as store, PolicyStore, Snapshot, StoreError};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;

use crate::search::request_id;

pub const NOT_INITIALIZED: &str = "policy_not_initialized";
pub const INVALID: &str = "policy_invalid";
pub const BUSY: &str = "policy_busy";
pub const UNAVAILABLE: &str = "policy_unavailable";
pub const UNSUPPORTED: &str = "policy_unsupported";
const CONFLICT: &str = "policy_conflict";
const NOT_MIGRATABLE: &str = "policy_not_migratable";
const INVALID_ETAG: &str = "invalid_etag";
const TOO_LARGE_TO_SEND: &str = "policy_too_large";
/// Chrome refuses a message from a host above 1 MB; the policy file itself may be up to 1 MiB.
const MAX_REPLY_BYTES: usize = 1_000_000;
const ALREADY_INITIALIZED: &str = "policy_already_initialized";
const INVALID_BATCH: &str = "invalid_policy_batch";
const BATCH_TOO_LARGE: &str = "policy_batch_too_large";
const BATCH_RESULT: &str = "policy_evaluate_batch_result";
const INVALID_DRAFT: &str = "invalid_policy_draft";
const DRAFT_TOO_LARGE: &str = "policy_draft_too_large";
const VERSION_MISMATCH: &str = "policy_version_mismatch";
const INVALID_RULE: &str = "invalid_rule";
const INVALID_URL: &str = "invalid_url";
const UNKNOWN_PRESET: &str = "unknown_preset";
/// A draft is the policy document; the file holds at most 1 MiB of it written out.
const MAX_DRAFT_BYTES: usize = 1_048_576;
/// As the evaluator's limit on URLs, which are never recorded beyond it.
const MAX_URL_CHARS: usize = 32_768;

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
                Err(StoreError::Unsupported) => return Err(UNSUPPORTED),
                Err(StoreError::Missing | StoreError::Invalid) => return Err(INVALID),
                Err(_) => return Err(UNAVAILABLE),
            }
        }
    }

    /// The latest policy's decision. `Err` means no decision exists: nothing may be stored, and
    /// the caller must not acknowledge the event as dropped.
    pub fn decide(&self, url: &str) -> Result<Decision, &'static str> {
        self.load().map(|snapshot| snapshot.compiled.evaluate(url))
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
        let answers = items
            .into_iter()
            .map(|(id, url)| {
                json!({ "id": id, "decision": decision_code(snapshot.compiled.evaluate(url)) })
            })
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
            Ok(snapshot) => {
                let mut reply = version_of(&snapshot);
                reply["ok"] = true.into();
                reply["type"] = "policy_result".into();
                reply["requestId"] = id.into();
                reply
            }
            Err(code) if initialize => {
                json!({ "ok": false, "type": "policy_result", "requestId": id, "error": code })
            }
            Err(code) => {
                json!({ "ok": true, "type": "policy_result", "requestId": id, "state": state_of(code) })
            }
        }
    }

    /// The active policy itself, for an explicit request only.
    pub fn get(&self, message: &Value) -> Value {
        const RESULT: &str = "policy_get_result";
        let Some(id) = request_id(message) else {
            return json!({ "ok": false, "type": RESULT, "requestId": null, "error": "invalid_request_id" });
        };
        let failure =
            |code: &str| json!({ "ok": false, "type": RESULT, "requestId": id, "error": code });
        match self.load() {
            Ok(snapshot) => {
                let mut reply = version_of(&snapshot);
                reply["ok"] = true.into();
                reply["type"] = RESULT.into();
                reply["requestId"] = id.into();
                reply["policy"] = json!(snapshot.policy);
                if reply.to_string().len() > MAX_REPLY_BYTES {
                    return failure(TOO_LARGE_TO_SEND);
                }
                reply
            }
            Err(code) => failure(code),
        }
    }

    /// Explicit, never retried, and only against the ETag the caller has seen.
    pub fn migrate(&self, message: &Value) -> Value {
        const RESULT: &str = "policy_migrate_result";
        let Some(id) = request_id(message) else {
            return json!({ "ok": false, "type": RESULT, "requestId": null, "error": "invalid_request_id" });
        };
        let outcome = (|| {
            let etag = message
                .get("etag")
                .and_then(Value::as_str)
                .filter(|etag| !etag.is_empty())
                .ok_or(INVALID_ETAG)?;
            let current = self.load()?;
            if current.etag != etag {
                return Err(CONFLICT);
            }
            let store = self.store.as_ref().map_err(|_| UNAVAILABLE)?;
            store.migrate(&current).map_err(|error| match error {
                StoreError::Conflict => CONFLICT,
                StoreError::NotMigratable => NOT_MIGRATABLE,
                StoreError::Busy => BUSY,
                StoreError::Invalid => INVALID,
                StoreError::Unsupported => UNSUPPORTED,
                _ => UNAVAILABLE,
            })
        })();
        match outcome {
            Ok(snapshot) => {
                let mut reply = version_of(&snapshot);
                reply["ok"] = true.into();
                reply["type"] = RESULT.into();
                reply["requestId"] = id.into();
                reply
            }
            Err(code) => json!({ "ok": false, "type": RESULT, "requestId": id, "error": code }),
        }
    }

    /// Whether a draft could be applied as it is. Changes nothing.
    pub fn validate(&self, message: &Value) -> Value {
        answer(message, "policy_validate_result", || {
            let (draft, _) = draft_of(message)?;
            Ok(
                json!({ "schemaVersion": draft.schema_version, "engineVersion": draft.engine_version }),
            )
        })
    }

    /// Whether one rule alone matches a URL. Not a decision; see `policy::rule_matches`.
    pub fn test_rule(&self, message: &Value) -> Value {
        answer(message, "policy_test_rule_result", || {
            let rule: RuleSpec = message
                .get("rule")
                .filter(|rule| rule.is_object())
                .and_then(|rule| serde_json::from_value(rule.clone()).ok())
                .ok_or(Refusal::Code(INVALID_RULE))?;
            let url = url_of(message)?;
            let pattern = matches!(rule, RuleSpec::Typed(policy::TypedRule::Pattern { .. }));
            let matched = policy::rule_matches(&rule, url).map_err(|error| {
                Refusal::Code(match error {
                    policy::Error::InvalidUrl => INVALID_URL,
                    policy::Error::UnsupportedRule => "unsupported_regex",
                    policy::Error::RegexLimit => "regex_limit",
                    _ if pattern => "invalid_pattern",
                    _ => "invalid_regex",
                })
            })?;
            Ok(json!({ "matched": matched }))
        })
    }

    /// The decision a draft would give a URL. Changes nothing, and permits nothing: recording
    /// judges with the stored policy only.
    pub fn test_url(&self, message: &Value) -> Value {
        answer(message, "policy_test_url_result", || {
            let url = url_of(message)?;
            let (_, compiled) = draft_of(message)?;
            Ok(json!({ "decision": decision_code(compiled.evaluate(url)) }))
        })
    }

    /// A preset as a draft. Changes nothing; applying one is `policy_apply` or `policy_start`.
    pub fn preset(&self, message: &Value) -> Value {
        answer(message, "policy_preset_result", || {
            match message.get("presetId").and_then(Value::as_str) {
                Some("default") => {
                    let preset = Policy::default();
                    Ok(json!({
                        "presetId": "default",
                        "schemaVersion": preset.schema_version,
                        "engineVersion": preset.engine_version,
                        "policy": preset,
                    }))
                }
                _ => Err(Refusal::Code(UNKNOWN_PRESET)),
            }
        })
    }

    /// Stores a draft in place of the policy of `expectedEtag`, all or nothing. The draft keeps
    /// the stored version and revision; the store writes the next revision. Changing the version is
    /// `policy_migrate` only.
    pub fn apply(&self, message: &Value) -> Value {
        answer(message, "policy_apply_result", || {
            let expected = etag_of(message, "expectedEtag")?;
            let current = self.load().map_err(Refusal::Code)?;
            if current.etag != expected {
                return Err(Refusal::Code(CONFLICT));
            }
            let (draft, _) = draft_of(message)?;
            if (draft.schema_version, draft.engine_version)
                != (current.policy.schema_version, current.policy.engine_version)
            {
                return Err(Refusal::Code(VERSION_MISMATCH));
            }
            let store = self
                .store
                .as_ref()
                .map_err(|_| Refusal::Code(UNAVAILABLE))?;
            let applied = store
                .update(&current, draft)
                .map_err(|error| Refusal::Code(store_code(error)))?;
            Ok(version_of(&applied))
        })
    }

    /// The first policy of a new install, which starts recording: only before any initialization,
    /// with a usable vault, for a valid version 2 draft. The caller cannot prove a person asked;
    /// the step is explicit, never automatic, and never replaces a policy.
    pub fn start(&self, message: &Value, vault: Result<(), &'static str>) -> Value {
        answer(message, "policy_start_result", || {
            let store = self
                .store
                .as_ref()
                .map_err(|_| Refusal::Code(UNAVAILABLE))?;
            if !store.never_initialized() {
                return Err(Refusal::Code(ALREADY_INITIALIZED));
            }
            vault.map_err(Refusal::Code)?;
            let (draft, _) = draft_of(message)?;
            let current = Policy::default();
            if (draft.schema_version, draft.engine_version)
                != (current.schema_version, current.engine_version)
            {
                return Err(Refusal::Code(VERSION_MISMATCH));
            }
            let started = store.initialize_with(&draft).map_err(|error| {
                Refusal::Code(match error {
                    StoreError::AlreadyInitialized => ALREADY_INITIALIZED,
                    other => store_code(other),
                })
            })?;
            Ok(version_of(&started))
        })
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

/// Why a request was not done: a code, and for a draft that cannot be used, where.
enum Refusal {
    Code(&'static str),
    Draft(&'static str, Vec<Problem>),
}

/// One reply of `result` for `message`: `{ ok: true, type, requestId, ...fields }`, or
/// `{ ok: false, type, requestId, error, errors? }`. Never the rules or a URL of the request.
fn answer(
    message: &Value,
    result: &'static str,
    work: impl FnOnce() -> Result<Value, Refusal>,
) -> Value {
    let Some(id) = request_id(message) else {
        return json!({ "ok": false, "type": result, "requestId": null, "error": "invalid_request_id" });
    };
    let mut reply = match work() {
        Ok(fields) => {
            let mut reply = fields;
            reply["ok"] = true.into();
            reply
        }
        Err(Refusal::Code(code)) => json!({ "ok": false, "error": code }),
        Err(Refusal::Draft(code, problems)) => {
            json!({ "ok": false, "error": code, "errors": problems })
        }
    };
    reply["type"] = result.into();
    reply["requestId"] = id.into();
    if reply.to_string().len() > MAX_REPLY_BYTES {
        return json!({ "ok": false, "type": result, "requestId": id, "error": TOO_LARGE_TO_SEND });
    }
    reply
}

/// The draft of a request, if it is a policy that compiles, of a size the store can write, and its
/// compiled form. Compiled once; only a draft that fails is gone through again for its problems.
fn draft_of(message: &Value) -> Result<(Policy, CompiledPolicy), Refusal> {
    let value = message
        .get("policy")
        .filter(|value| value.is_object())
        .ok_or(Refusal::Code(INVALID_DRAFT))?;
    if serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > MAX_DRAFT_BYTES) {
        return Err(Refusal::Code(DRAFT_TOO_LARGE));
    }
    let draft: Policy = serde_json::from_value(value.clone())
        .map_err(|_| Refusal::Draft(INVALID, document_problems(value)))?;
    let Ok(compiled) = CompiledPolicy::compile(&draft) else {
        let problems = draft.problems();
        let unsupported = problems
            .iter()
            .all(|problem| problem.code == "unsupported_regex");
        return Err(Refusal::Draft(
            if unsupported { UNSUPPORTED } else { INVALID },
            problems,
        ));
    };
    if !store::fits(&draft) {
        return Err(Refusal::Draft(
            INVALID,
            vec![Problem::of("policy_too_large")],
        ));
    }
    Ok((draft, compiled))
}

/// For a document that is no policy at all: the rules that are no rule, or no place.
fn document_problems(value: &Value) -> Vec<Problem> {
    let mut found = Vec::new();
    for layer in ["neverSave", "saveButNoMcp"] {
        let rules = value
            .pointer(&format!("/layers/{layer}/rules"))
            .and_then(Value::as_array);
        for (index, rule) in rules.into_iter().flatten().enumerate() {
            if serde_json::from_value::<RuleSpec>(rule.clone()).is_err() {
                found.push(Problem {
                    layer: Some(layer),
                    rule_index: Some(index),
                    code: "invalid_rule_form",
                });
            }
        }
    }
    if found.is_empty() {
        found.push(Problem::of("invalid_policy_document"));
    }
    found
}

fn url_of(message: &Value) -> Result<&str, Refusal> {
    message
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| url.encode_utf16().count() <= MAX_URL_CHARS)
        .ok_or(Refusal::Code(INVALID_URL))
}

fn etag_of<'a>(message: &'a Value, key: &str) -> Result<&'a str, Refusal> {
    message
        .get(key)
        .and_then(Value::as_str)
        .filter(|etag| !etag.is_empty())
        .ok_or(Refusal::Code(INVALID_ETAG))
}

fn store_code(error: StoreError) -> &'static str {
    match error {
        StoreError::Conflict => CONFLICT,
        StoreError::NotMigratable => NOT_MIGRATABLE,
        StoreError::Busy => BUSY,
        StoreError::Invalid => INVALID,
        StoreError::Unsupported => UNSUPPORTED,
        StoreError::Missing => INVALID,
        _ => UNAVAILABLE,
    }
}

/// What identifies a policy without showing it.
fn version_of(snapshot: &Snapshot) -> Value {
    json!({
        "state": "ready",
        "etag": snapshot.etag,
        "revision": snapshot.policy.revision,
        "schemaVersion": snapshot.policy.schema_version,
        "engineVersion": snapshot.policy.engine_version,
    })
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
        UNSUPPORTED => "unsupported",
        _ => "unavailable",
    }
}
