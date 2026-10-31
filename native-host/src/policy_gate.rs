//! Connects the privacy policy to recording: `policy_status` and `policy_initialize` messages, and
//! the decision for each automatically recorded event. A decision exists only if a valid policy
//! was loaded; a failure to load is reported as an error and never as a refusal of the URL.

use memocampus_native_host::policy::Decision;
use memocampus_native_host::policy_store::{PolicyStore, Snapshot, StoreError};
use serde_json::{json, Value};
use std::time::Duration;

use crate::search::request_id;

pub const NOT_INITIALIZED: &str = "policy_not_initialized";
pub const INVALID: &str = "policy_invalid";
pub const BUSY: &str = "policy_busy";
pub const UNAVAILABLE: &str = "policy_unavailable";
const ALREADY_INITIALIZED: &str = "policy_already_initialized";

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

fn state_of(code: &str) -> &'static str {
    match code {
        NOT_INITIALIZED => "uninitialized",
        BUSY => "busy",
        INVALID => "invalid",
        _ => "unavailable",
    }
}
