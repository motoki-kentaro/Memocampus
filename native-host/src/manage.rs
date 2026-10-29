//! Vault management messages from the side panel: `vault_status`, `vault_create`,
//! `vault_open`, `vault_backup`. Every message gets exactly one `vault_result` response.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::dialog::Dialogs;
use crate::location::{self, Assessment, Verdict};
use crate::search::request_id;
use crate::vault::{self, Active, ConfigLock, Session, VaultError};

const MAX_PATH_CHARS: usize = 32_767;
const DEFAULT_VAULT_NAME: &str = "memocampus.sqlite3";

struct Failure {
    code: &'static str,
    extra: Map<String, Value>,
}

impl From<VaultError> for Failure {
    fn from(error: VaultError) -> Self {
        Self {
            code: error.code(),
            extra: Map::new(),
        }
    }
}

impl Failure {
    fn with(code: &'static str, extra: Value) -> Self {
        Self {
            code,
            extra: extra.as_object().cloned().unwrap_or_default(),
        }
    }
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn handle(session: &mut Session, dialogs: &dyn Dialogs, message: &Value) -> Value {
    let Some(id) = request_id(message) else {
        return json!({ "ok": false, "type": "vault_result", "requestId": null, "error": "invalid_request_id" });
    };
    let result = match message.get("type").and_then(Value::as_str) {
        Some("vault_status") => Ok(json!({ "status": status(session) })),
        Some("vault_create") => create(session, dialogs, message),
        Some("vault_open") => open(session, dialogs, message),
        Some("vault_backup") => backup(session, dialogs, message),
        _ => Err(Failure::with("unknown_type", json!({}))),
    };
    let mut response = match result {
        Ok(Value::Object(fields)) => fields,
        Ok(_) => Map::new(),
        Err(failure) => {
            let mut fields = failure.extra;
            fields.insert("error".into(), failure.code.into());
            fields
        }
    };
    let ok = !response.contains_key("error");
    response.insert("ok".into(), ok.into());
    response.insert("type".into(), "vault_result".into());
    response.insert("requestId".into(), id.into());
    Value::Object(response)
}

/// Snapshot of the vault configuration for the settings UI. Read-only: creates nothing.
pub fn status(session: &Session) -> Value {
    let env = &session.env;
    let default_path = env.default_vault_path().ok();
    let mut status = json!({
        "managementEnabled": env.management_enabled(),
        "overridden": env.db_override.is_some(),
        "defaultPath": default_path.as_deref().map(display),
        "defaultExists": default_path.as_deref().is_some_and(Path::exists),
        "vaultPath": null,
        "counts": null,
        "location": null,
        "locationApproved": false,
    });
    let path = match vault::resolve(env) {
        Err(error) => {
            status["state"] = error.code().into();
            return status;
        }
        Ok(Active::NotConfigured) => {
            status["state"] = "not_configured".into();
            return status;
        }
        Ok(Active::Override(path)) => path,
        Ok(Active::Configured(config)) => {
            status["locationApproved"] = config.location_approved.into();
            let path = PathBuf::from(config.active_vault);
            status["location"] = json!(location::probe(&path, env.exe_dir.as_deref()));
            path
        }
    };
    status["vaultPath"] = display(&path).into();
    match vault::open_vault(&path) {
        Ok(db) => {
            status["state"] = "ready".into();
            if let Ok(counts) = vault::counts(&db) {
                status["counts"] = json!(counts);
            }
        }
        Err(VaultError::Missing) => status["state"] = "missing".into(),
        Err(VaultError::Invalid) => status["state"] = "invalid".into(),
        Err(error) => status["state"] = error.code().into(),
    }
    status
}

fn require_management(session: &Session) -> Result<(), Failure> {
    if session.env.management_enabled() {
        Ok(())
    } else {
        Err(VaultError::ManagementDisabled.into())
    }
}

/// Optional absolute path from the message. Quotes from Explorer's "Copy as path" are removed.
fn requested_path(message: &Value, key: &str) -> Result<Option<PathBuf>, Failure> {
    match message.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => {
            let text = raw.trim().trim_matches('"').trim();
            if text.is_empty() {
                return Ok(None);
            }
            let path = PathBuf::from(text);
            if text.chars().count() > MAX_PATH_CHARS || !path.is_absolute() {
                return Err(VaultError::InvalidPath.into());
            }
            Ok(Some(path))
        }
        Some(_) => Err(VaultError::InvalidPath.into()),
    }
}

fn approved(message: &Value) -> bool {
    message.get("approveWarnings").and_then(Value::as_bool) == Some(true)
}

/// Absolute path of a file that may not exist yet: canonical parent + file name.
fn normalize_new(target: &Path) -> Result<PathBuf, Failure> {
    if location::is_unc(&target.to_string_lossy()) {
        return Ok(target.to_path_buf()); // rejected by the location check without network I/O
    }
    let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(VaultError::InvalidPath.into());
    };
    let parent = std::fs::canonicalize(parent).map_err(|_| VaultError::ParentMissing)?;
    Ok(location::simplify(&parent).join(name))
}

fn normalize_existing(target: &Path) -> Result<PathBuf, Failure> {
    if location::is_unc(&target.to_string_lossy()) {
        return Ok(target.to_path_buf());
    }
    match std::fs::canonicalize(target) {
        Ok(path) => Ok(location::simplify(&path)),
        Err(_) => Err(VaultError::Missing.into()),
    }
}

/// Returns whether the user accepted warnings; refuses rejected locations.
fn check_location(assessment: &Assessment, target: &Path, approve: bool) -> Result<bool, Failure> {
    let details = || json!({ "path": display(target), "location": assessment });
    match assessment.verdict {
        Verdict::NoIssueDetected => Ok(false),
        Verdict::Rejected => Err(Failure::with("location_rejected", details())),
        Verdict::NeedsApproval if approve => Ok(true),
        Verdict::NeedsApproval => Err(Failure::with("location_needs_approval", details())),
    }
}

fn activate(
    session: &mut Session,
    lock: &ConfigLock,
    target: &Path,
    location_approved: bool,
) -> Result<Value, Failure> {
    vault::switch_active(&session.env, lock, target, location_approved)?;
    session.reset();
    Ok(json!({ "path": display(target), "status": status(session) }))
}

fn create(session: &mut Session, dialogs: &dyn Dialogs, message: &Value) -> Result<Value, Failure> {
    require_management(session)?;
    let default_path = session.env.default_vault_path()?;
    let target = match requested_path(message, "path")? {
        Some(path) => path,
        None if message.get("useDefault").and_then(Value::as_bool) == Some(true) => {
            if let Some(dir) = default_path.parent() {
                std::fs::create_dir_all(dir).map_err(|_| VaultError::Io)?;
            }
            default_path
        }
        None => {
            let dir = default_path.parent().unwrap_or(Path::new(""));
            dialogs
                .choose_new_vault(dir, DEFAULT_VAULT_NAME)
                .ok_or(VaultError::Cancelled)?
        }
    };
    let target = normalize_new(&target)?;
    if target.exists() {
        return Err(Failure::with(
            "file_exists",
            json!({ "path": display(&target) }),
        ));
    }
    let assessment = location::probe(&target, session.env.exe_dir.as_deref());
    let location_approved = check_location(&assessment, &target, approved(message))?;
    let lock = ConfigLock::acquire(&session.env)?;
    vault::create_vault(&target)
        .map_err(|error| Failure::with(error.code(), json!({ "path": display(&target) })))?;
    activate(session, &lock, &target, location_approved).map_err(|mut failure| {
        // The new vault exists but is not active; it can be opened later.
        failure.extra.insert("path".into(), display(&target).into());
        failure.extra.insert("created".into(), true.into());
        failure
    })
}

fn open(session: &mut Session, dialogs: &dyn Dialogs, message: &Value) -> Result<Value, Failure> {
    require_management(session)?;
    let target = match requested_path(message, "path")? {
        Some(path) => path,
        None => {
            let start = session.env.app_dir.clone();
            dialogs
                .choose_existing_vault(start.as_deref())
                .ok_or(VaultError::Cancelled)?
        }
    };
    let target = normalize_existing(&target)?;
    let assessment = location::probe(&target, session.env.exe_dir.as_deref());
    // Location first: a rejected (e.g. network) file is never opened by SQLite.
    let location_approved = check_location(&assessment, &target, approved(message))?;
    vault::open_vault_checked(&target)
        .map_err(|error| Failure::with(error.code(), json!({ "path": display(&target) })))?;
    let lock = ConfigLock::acquire(&session.env)?;
    activate(session, &lock, &target, location_approved)
}

fn backup(session: &mut Session, dialogs: &dyn Dialogs, message: &Value) -> Result<Value, Failure> {
    require_management(session)?;
    // Fail before showing a dialog if there is nothing to back up.
    session.read(|_| ())?;
    let dir = match requested_path(message, "directory")? {
        Some(path) => path,
        None => dialogs.choose_backup_dir().ok_or(VaultError::Cancelled)?,
    };
    let path = session.read(|db| vault::backup_vault(db, &dir, SystemTime::now()))??;
    Ok(json!({ "backupPath": display(&path) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::tests::TempDir;
    use crate::vault::Environment;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeDialogs {
        answer: Option<PathBuf>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl Dialogs for FakeDialogs {
        fn choose_new_vault(&self, _: &Path, name: &str) -> Option<PathBuf> {
            assert_eq!(name, DEFAULT_VAULT_NAME);
            self.calls.borrow_mut().push("new");
            self.answer.clone()
        }
        fn choose_existing_vault(&self, _: Option<&Path>) -> Option<PathBuf> {
            self.calls.borrow_mut().push("existing");
            self.answer.clone()
        }
        fn choose_backup_dir(&self) -> Option<PathBuf> {
            self.calls.borrow_mut().push("backup");
            self.answer.clone()
        }
    }

    fn send(session: &mut Session, dialogs: &FakeDialogs, message: Value) -> Value {
        let mut message = message;
        message["requestId"] = "r".into();
        let response = handle(session, dialogs, &message);
        assert_eq!(response["type"], "vault_result");
        assert_eq!(response["requestId"], "r");
        response
    }

    #[test]
    fn first_run_is_not_configured_and_creates_nothing() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let response = send(
            &mut session,
            &FakeDialogs::default(),
            json!({ "type": "vault_status" }),
        );
        assert_eq!(response["ok"], true);
        assert_eq!(response["status"]["state"], "not_configured");
        assert_eq!(response["status"]["managementEnabled"], true);
        assert_eq!(response["status"]["defaultExists"], false);
        assert!(
            !tmp.0.join("app").exists(),
            "status must not create the app dir"
        );
    }

    #[test]
    fn create_default_then_status_is_ready_and_a_second_create_is_refused() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let dialogs = FakeDialogs::default();
        let created = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_create", "useDefault": true }),
        );
        assert_eq!(created["ok"], true, "{created}");
        assert_eq!(created["status"]["state"], "ready");
        assert_eq!(
            created["status"]["counts"],
            json!({ "visits": 0, "snapshots": 0 })
        );
        assert!(dialogs.calls.borrow().is_empty());

        let again = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_create", "useDefault": true }),
        );
        assert_eq!(again["ok"], false);
        assert_eq!(again["error"], "file_exists");
    }

    #[test]
    fn create_via_dialog_and_cancellation() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let cancelled = send(
            &mut session,
            &FakeDialogs::default(),
            json!({ "type": "vault_create" }),
        );
        assert_eq!(cancelled["error"], "cancelled");

        let chosen = tmp.0.join("picked.sqlite3");
        let dialogs = FakeDialogs {
            answer: Some(chosen.clone()),
            ..FakeDialogs::default()
        };
        let created = send(&mut session, &dialogs, json!({ "type": "vault_create" }));
        assert_eq!(created["ok"], true, "{created}");
        assert_eq!(*dialogs.calls.borrow(), ["new"]);
        assert!(chosen.exists());
    }

    #[test]
    fn create_rejects_bad_paths_without_side_effects() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let dialogs = FakeDialogs::default();
        for (path, code) in [
            (json!("relative.sqlite3"), "invalid_path"),
            (json!(42), "invalid_path"),
            (
                json!(tmp.0.join("missing").join("v.sqlite3")),
                "parent_missing",
            ),
            (json!(r"\\nas.invalid\share\v.sqlite3"), "location_rejected"),
            (
                json!(tmp.0.join("exe").join("v.sqlite3")),
                "location_rejected",
            ),
        ] {
            std::fs::create_dir_all(tmp.0.join("exe")).unwrap();
            let response = send(
                &mut session,
                &dialogs,
                json!({ "type": "vault_create", "path": path }),
            );
            assert_eq!(response["error"], code, "{response}");
        }
        assert!(!tmp.0.join("exe").join("v.sqlite3").exists());
        assert_eq!(vault::resolve(&session.env), Ok(Active::NotConfigured));
    }

    #[test]
    fn quoted_paths_from_explorer_are_accepted() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let target = tmp.0.join("quoted.sqlite3");
        let response = send(
            &mut session,
            &FakeDialogs::default(),
            json!({ "type": "vault_create", "path": format!("\"{}\"", target.display()) }),
        );
        assert_eq!(response["ok"], true, "{response}");
    }

    #[test]
    fn open_validates_and_switches_only_to_real_vaults() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let dialogs = FakeDialogs::default();
        let junk = tmp.0.join("junk.sqlite3");
        std::fs::write(&junk, b"not a database").unwrap();
        let refused = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_open", "path": junk }),
        );
        assert_eq!(refused["error"], "vault_invalid");
        assert_eq!(std::fs::read(&junk).unwrap(), b"not a database");

        let missing = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_open", "path": tmp.0.join("none.sqlite3") }),
        );
        assert_eq!(missing["error"], "vault_missing");
        assert!(!tmp.0.join("none.sqlite3").exists());

        let real = tmp.0.join("real.sqlite3");
        vault::create_vault(&real).unwrap();
        let opened = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_open", "path": real }),
        );
        assert_eq!(opened["ok"], true, "{opened}");
        assert_eq!(opened["status"]["state"], "ready");
    }

    #[test]
    fn missing_vault_is_reported_and_not_recreated() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let dialogs = FakeDialogs::default();
        let target = tmp.0.join("v.sqlite3");
        send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_create", "path": target }),
        );
        std::fs::remove_file(&target).unwrap();
        let status = send(&mut session, &dialogs, json!({ "type": "vault_status" }));
        assert_eq!(status["status"]["state"], "missing");
        assert!(!target.exists());
    }

    #[test]
    fn override_disables_every_management_operation() {
        let tmp = TempDir::new();
        let env = Environment {
            db_override: Some(tmp.0.join("dev.sqlite3")),
            ..tmp.env()
        };
        let mut session = Session::new(env);
        let dialogs = FakeDialogs::default();
        for kind in ["vault_create", "vault_open", "vault_backup"] {
            let response = send(
                &mut session,
                &dialogs,
                json!({ "type": kind, "useDefault": true }),
            );
            assert_eq!(response["error"], "management_disabled", "{kind}");
        }
        let status = send(&mut session, &dialogs, json!({ "type": "vault_status" }));
        assert_eq!(status["status"]["overridden"], true);
        assert_eq!(status["status"]["state"], "missing");
        assert!(dialogs.calls.borrow().is_empty());
    }

    #[test]
    fn backup_requires_a_vault_and_writes_into_the_chosen_folder() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let dest = tmp.0.join("backups");
        std::fs::create_dir(&dest).unwrap();
        let dialogs = FakeDialogs {
            answer: Some(dest.clone()),
            ..FakeDialogs::default()
        };
        let early = send(&mut session, &dialogs, json!({ "type": "vault_backup" }));
        assert_eq!(early["error"], "vault_not_configured");
        assert!(
            dialogs.calls.borrow().is_empty(),
            "no dialog without a vault"
        );

        send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_create", "path": tmp.0.join("v.sqlite3") }),
        );
        let done = send(&mut session, &dialogs, json!({ "type": "vault_backup" }));
        assert_eq!(done["ok"], true, "{done}");
        let path = PathBuf::from(done["backupPath"].as_str().unwrap());
        assert_eq!(path.parent().unwrap(), dest);
        assert!(vault::open_vault_checked(&path).is_ok());
    }

    #[test]
    fn unknown_types_and_missing_ids_get_an_error_response() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let dialogs = FakeDialogs::default();
        assert_eq!(
            send(&mut session, &dialogs, json!({ "type": "vault_delete" }))["error"],
            "unknown_type"
        );
        let no_id = handle(&mut session, &dialogs, &json!({ "type": "vault_status" }));
        assert_eq!(no_id["error"], "invalid_request_id");
    }
}
