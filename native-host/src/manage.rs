//! Vault management messages from the side panel: `vault_status`, `vault_create`,
//! `vault_open`, `vault_backup`, and (Issue #51) `vault_publication_confirm` /
//! `vault_publication_revoke`, the encrypted vault messages (`encrypted.rs`), backup restore
//! (`restore.rs`) and the conversion between a plain and an encrypted vault (`convert.rs`). Every
//! message gets exactly one `vault_result` response.
//!
//! Publication decisions exist only here, in the Native Host, which only the extension can reach;
//! the MCP server has no way to make or undo one.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::dialog::Dialogs;
use crate::location::{self, Assessment, Verdict};
use crate::policy_gate;
use crate::search::request_id;
use crate::vault::{
    self, Active, ConfigLock, PolicyReadiness, PublicationChange, Session, VaultError,
};
use memocampus_native_host::active_vault::VaultMode;
use memocampus_native_host::policy_store::{PolicyStore, StoreError};

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
        Some("vault_publication_confirm") => publication(session, message, true),
        Some("vault_publication_revoke") => publication(session, message, false),
        Some("vault_create_encrypted") => encrypted::begin(session, dialogs, message),
        Some("vault_create_encrypted_commit") => encrypted::commit(session, message),
        Some("vault_create_encrypted_cancel") => encrypted::cancel(session),
        Some("vault_recover") => encrypted::recover(session, dialogs, message),
        Some("vault_restore_inspect") => restore::inspect(session, dialogs, message),
        Some("vault_restore") => restore::restore(session, dialogs, message),
        Some("vault_restore_activate") => restore::activate(session, message),
        Some("vault_restore_discard") => restore::discard(session),
        Some("vault_convert_start") => convert::start(session, dialogs, message),
        Some("vault_convert_confirm") => convert::confirm(session, message),
        Some("vault_convert_activate") => convert::activate(session, message),
        Some("vault_convert_keep") => convert::keep(session),
        Some("vault_convert_discard") => convert::discard(session),
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
        "vaultToken": null,
        "counts": null,
        "location": null,
        "locationApproved": false,
        "mode": null,
        // MCP access to the vault shown (Issue #51): "confirmed", "pending", or null when MCP
        // cannot read it at all (no vault, or the MEMOCAMPUS_DB override, which MCP never reads).
        "publication": null,
        "publicationPolicyRevision": null,
        // Issue #51 Slice B: whether new vaults can be encrypted here (DPAPI: Windows only), and
        // the target of a creation interrupted before its spell was confirmed (to be cancelled).
        "encryptionAvailable": cfg!(windows),
        "creationPending": encrypted::pending_creation(session),
        // Issue #51 Slice C: a restored vault not yet activated (or a restore interrupted).
        "restorePending": restore::pending_restore(session),
        // Issue #51 Slice D: a conversion of the encryption state not yet activated, kept or
        // discarded.
        "conversionPending": convert::pending_conversion(session),
    });
    let (path, mode, vault_id, config) = match vault::resolve(env) {
        Err(error) => {
            status["state"] = error.code().into();
            return status;
        }
        Ok(Active::NotConfigured) => {
            status["state"] = "not_configured".into();
            return status;
        }
        Ok(Active::Override(path)) => (path, VaultMode::Plain, None, None),
        Ok(Active::Configured(config)) => {
            status["locationApproved"] = config.location_approved.into();
            let path = PathBuf::from(&config.active_vault);
            status["location"] = json!(location::probe(&path, env.exe_dir.as_deref()));
            (path, config.mode, config.vault_id, Some(config))
        }
    };
    status["vaultPath"] = display(&path).into();
    status["mode"] = mode.code().into();
    match vault::open_vault_in(&path, mode, vault_id, session.keys()).map_err(VaultError::from) {
        Ok(db) => {
            status["state"] = "ready".into();
            // The vault shown, as `vault_backup` must name it (taken while `db` holds the file).
            let token = vault::vault_token(&path).ok();
            status["vaultToken"] = json!(token);
            if let (Some(config), Some(token)) = (&config, &token) {
                let published = config.is_published(token);
                status["publication"] = if published { "confirmed" } else { "pending" }.into();
                if published {
                    status["publicationPolicyRevision"] =
                        json!(config.publication.as_ref().and_then(|p| p.policy_revision));
                }
            }
            if let Ok(counts) = vault::counts(&db) {
                status["counts"] = json!(counts);
            }
        }
        Err(VaultError::Missing) => status["state"] = "missing".into(),
        Err(VaultError::Invalid) => status["state"] = "invalid".into(),
        Err(error) => status["state"] = error.code().into(),
    }
    if config.is_some() && status["publication"].is_null() {
        // Whatever cannot be shown as ready is not readable by MCP either.
        status["publication"] = "pending".into();
    }
    // Issue #51 Slice B: an encrypted vault this user cannot unlock may be recovered with its
    // spell; a creation interrupted before its confirmation is shown so it can be cancelled.
    if mode == VaultMode::Encrypted && status["state"] != "ready" {
        status["recoveryAvailable"] = encrypted::recovery_available(&path).into();
    }
    status
}

/// The global policy, as publication decisions need it (see `vault::migrate_legacy_config`).
pub fn policy_readiness(env: &vault::Environment) -> PolicyReadiness {
    let Some(store) = env
        .app_dir
        .clone()
        .and_then(|dir| PolicyStore::new(dir).ok())
    else {
        return PolicyReadiness::Unknown;
    };
    match store.load() {
        Ok(snapshot) => PolicyReadiness::Ready(snapshot.policy.revision),
        Err(StoreError::Missing | StoreError::Invalid | StoreError::Unsupported) => {
            PolicyReadiness::NotReady
        }
        Err(_) => PolicyReadiness::Unknown,
    }
}

fn policy_failure(error: StoreError) -> Failure {
    let code = match error {
        StoreError::Missing => policy_gate::NOT_INITIALIZED,
        StoreError::Invalid => policy_gate::INVALID,
        StoreError::Unsupported => policy_gate::UNSUPPORTED,
        StoreError::Busy => policy_gate::BUSY,
        _ => policy_gate::UNAVAILABLE,
    };
    Failure::with(code, json!({}))
}

/// Confirms (`confirm`) or revokes MCP access to the vault the panel shows: `expectedVaultToken`
/// is the `vaultToken` of that status. Confirming needs a usable global policy, whose revision is
/// recorded; the policy itself is never written. Decided under the config lock (as every switch),
/// so it can only apply to the vault shown, else `vault_switched` and nothing changes.
fn publication(session: &mut Session, message: &Value, confirm: bool) -> Result<Value, Failure> {
    require_management(session)?;
    let expected = message
        .get("expectedVaultToken")
        .and_then(Value::as_str)
        .filter(|token| crate::image_original::is_hex64(token))
        .ok_or_else(|| Failure::with("invalid_vault_token", json!({})))?;
    let change = if confirm {
        let store = PolicyStore::new(
            session
                .env
                .app_dir
                .clone()
                .ok_or(VaultError::AppDirUnavailable)?,
        )
        .map_err(|_| VaultError::AppDirUnavailable)?;
        let snapshot = store.load().map_err(policy_failure)?;
        PublicationChange::Confirm {
            policy_revision: snapshot.policy.revision,
        }
    } else {
        PublicationChange::Revoke
    };
    let lock = ConfigLock::acquire(&session.env)?;
    vault::set_publication(&session.env, &lock, expected, change)?;
    drop(lock);
    session.reset();
    Ok(json!({ "status": status(session) }))
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

/// Issue #51 Slice D: a file that is (or may be) unfinished work of a vault operation is never
/// opened as a vault: the work files of creations, restores and conversions (`.pending`,
/// `.partial`), and the target of a conversion that is not activated or kept yet.
fn refuse_work_file(session: &Session, target: &Path) -> Result<(), Failure> {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name.ends_with(".partial") || name.ends_with(".pending") {
        return Err(Failure::with(
            "vault_work_file",
            json!({ "path": display(target) }),
        ));
    }
    convert::refuse_converting_file(session, target)
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
    refuse_work_file(session, &target)?;
    let assessment = location::probe(&target, session.env.exe_dir.as_deref());
    // Location first: a rejected (e.g. network) file is never opened by SQLite.
    let location_approved = check_location(&assessment, &target, approved(message))?;
    // Issue #51 Slice B: a file that is not plain SQLite may be an encrypted vault.
    if let Some(opened) = encrypted::open_existing(session, &target, location_approved)? {
        return Ok(opened);
    }
    vault::open_vault_checked(&target)
        .map_err(|error| Failure::with(error.code(), json!({ "path": display(&target) })))?;
    let lock = ConfigLock::acquire(&session.env)?;
    activate(session, &lock, &target, location_approved)
}

/// Backs up the vault the user saw: `expectedVaultToken` is the `vaultToken` of the status the panel
/// shows. That vault must be the active one when the request arrives (else nothing is asked), and the
/// backup is read only from that same vault file (`read_for`), after the folder is chosen. A switch
/// at any point before (panel shown, dialog open, folder given) fails with `vault_switched`, before
/// any file is created; a switch away and back to the same file is that vault again.
///
/// Issue #51 Slice C: `protection` is `"normal"` (default) or `"password"` (with `password`). A plain
/// vault's normal backup is format A (a plain SQLite file, as before); the others are containers
/// (`backup.rs`): B (encrypted vault), C (plain, password) and D (encrypted, password). B and D also
/// need `recoverySpell`, which must recover this very vault.
fn backup(session: &mut Session, dialogs: &dyn Dialogs, message: &Value) -> Result<Value, Failure> {
    require_management(session)?;
    let expected = message
        .get("expectedVaultToken")
        .and_then(Value::as_str)
        .filter(|token| crate::image_original::is_hex64(token))
        .ok_or_else(|| Failure::with("invalid_vault_token", json!({})))?;
    let protection = backup::protection(message)?;
    // Fail before showing a dialog if there is nothing to back up, or not the vault shown.
    let ticket = session.ticket()?;
    let active = session.active_target()?;
    if ticket.token != expected || active.path != ticket.path {
        return Err(VaultError::Switched.into());
    }
    let kind = backup::kind_for(active.mode, &protection);
    // B and D (review finding C-03): the spell the user holds, checked for form before the
    // dialog and against this vault's envelope and key before anything is written. A and C never
    // take one.
    let spell = match kind {
        Some(kind) if kind.needs_spell() => Some(encrypted::requested_spell(message)?),
        _ => None,
    };
    // Neither a connection nor a lock is held while the dialog is open.
    session.reset();
    let dir = match requested_path(message, "directory")? {
        Some(path) => path,
        None => dialogs.choose_backup_dir().ok_or(VaultError::Cancelled)?,
    };
    let Some(kind) = kind else {
        let path = session.read_for(&ticket, |db| {
            vault::backup_vault(db, &dir, SystemTime::now())
        })?;
        return Ok(json!({
            "backupPath": display(&path),
            "vaultPath": ticket.path,
            "vaultToken": ticket.token,
            "format": "A",
            "policyCopy": false,
        }));
    };
    let source = backup::Source {
        path: PathBuf::from(&ticket.path),
        vault_id: active.vault_id,
        app: session
            .env
            .app_dir
            .clone()
            .ok_or(VaultError::AppDirUnavailable)?,
        token: ticket.token.clone(),
        spell,
    };
    let written = session.read_for(&ticket, |db| {
        backup::write(db, &source, &dir, kind, &protection, SystemTime::now())
    })?;
    Ok(backup::response(&written, &ticket.path, &ticket.token))
}

mod backup;
mod convert;
mod encrypted;
mod restore;

#[cfg(test)]
mod backup_race_tests;

#[cfg(test)]
mod backup_restore_tests;

#[cfg(test)]
mod convert_tests;

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
        fn choose_export_file(&self, _: &str, _: &str) -> Option<PathBuf> {
            unreachable!("vault management never exports images")
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
        let early = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_backup", "expectedVaultToken": "0".repeat(64) }),
        );
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
        let shown = send(&mut session, &dialogs, json!({ "type": "vault_status" }));
        let token = shown["status"]["vaultToken"].clone();
        assert!(token.is_string(), "{shown}");
        let done = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_backup", "expectedVaultToken": token }),
        );
        assert_eq!(done["ok"], true, "{done}");
        assert_eq!(done["vaultToken"], token);
        let path = PathBuf::from(done["backupPath"].as_str().unwrap());
        assert_eq!(path.parent().unwrap(), dest);
        assert!(vault::open_vault_checked(&path).is_ok());
    }

    // --- Issue #51 B0-2: publication ----------------------------------------------------------

    fn init_policy(session: &Session) {
        PolicyStore::new(session.env.app_dir.clone().unwrap())
            .unwrap()
            .initialize()
            .unwrap();
    }

    #[test]
    fn a_new_vault_is_pending_until_the_user_confirms_it_with_a_ready_policy() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let dialogs = FakeDialogs::default();
        let created = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_create", "path": tmp.0.join("v.sqlite3") }),
        );
        assert_eq!(created["status"]["publication"], "pending", "{created}");
        assert_eq!(created["status"]["mode"], "plain");
        let token = created["status"]["vaultToken"].clone();

        // No policy yet: nothing can be decided for MCP, and the config is not touched.
        let config = std::fs::read(session.env.config_path().unwrap()).unwrap();
        let refused = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_publication_confirm", "expectedVaultToken": token }),
        );
        assert_eq!(refused["error"], "policy_not_initialized");
        assert_eq!(
            std::fs::read(session.env.config_path().unwrap()).unwrap(),
            config
        );
        assert!(
            !session
                .env
                .app_dir
                .as_ref()
                .unwrap()
                .join("policy.json")
                .exists(),
            "confirming never initializes the policy"
        );

        init_policy(&session);
        let confirmed = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_publication_confirm", "expectedVaultToken": token }),
        );
        assert_eq!(confirmed["ok"], true, "{confirmed}");
        assert_eq!(confirmed["status"]["publication"], "confirmed");
        assert_eq!(confirmed["status"]["publicationPolicyRevision"], 1);

        let revoked = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_publication_revoke", "expectedVaultToken": token }),
        );
        assert_eq!(revoked["status"]["publication"], "pending", "{revoked}");
        assert_eq!(revoked["status"]["publicationPolicyRevision"], json!(null));
    }

    #[test]
    fn a_decision_needs_the_token_of_the_vault_shown() {
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let dialogs = FakeDialogs::default();
        init_policy(&session);
        let a = tmp.0.join("a.sqlite3");
        send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_create", "path": a }),
        );
        let shown = send(&mut session, &dialogs, json!({ "type": "vault_status" }));
        let token = shown["status"]["vaultToken"].clone();
        // Another vault becomes active before the user's decision arrives.
        let b = tmp.0.join("b.sqlite3");
        send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_create", "path": b }),
        );
        for kind in ["vault_publication_confirm", "vault_publication_revoke"] {
            let response = send(
                &mut session,
                &dialogs,
                json!({ "type": kind, "expectedVaultToken": token }),
            );
            assert_eq!(response["error"], "vault_switched", "{kind}");
        }
        let status = send(&mut session, &dialogs, json!({ "type": "vault_status" }));
        assert_eq!(status["status"]["publication"], "pending");
        for bad in [json!(null), json!("x"), json!("A".repeat(64)), json!(42)] {
            let response = send(
                &mut session,
                &dialogs,
                json!({ "type": "vault_publication_confirm", "expectedVaultToken": bad }),
            );
            assert_eq!(response["error"], "invalid_vault_token", "{bad}");
        }
    }

    #[test]
    fn the_override_and_unusable_vaults_offer_no_publication() {
        let tmp = TempDir::new();
        let dialogs = FakeDialogs::default();
        let env = Environment {
            db_override: Some(tmp.0.join("dev.sqlite3")),
            ..tmp.env()
        };
        let mut session = Session::new(env);
        let status = send(&mut session, &dialogs, json!({ "type": "vault_status" }));
        assert_eq!(status["status"]["publication"], json!(null));
        let refused = send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_publication_confirm", "expectedVaultToken": "0".repeat(64) }),
        );
        assert_eq!(refused["error"], "management_disabled");

        let mut session = Session::new(tmp.env());
        let target = tmp.0.join("v.sqlite3");
        send(
            &mut session,
            &dialogs,
            json!({ "type": "vault_create", "path": target }),
        );
        std::fs::remove_file(&target).unwrap();
        let status = send(&mut session, &dialogs, json!({ "type": "vault_status" }));
        assert_eq!(status["status"]["state"], "missing");
        assert_eq!(status["status"]["publication"], "pending");
    }

    #[test]
    fn an_encrypted_vault_is_reported_as_such_never_as_invalid() {
        use memocampus_native_host::active_vault::Config;
        let tmp = TempDir::new();
        let mut session = Session::new(tmp.env());
        let path = tmp.0.join("enc.sqlite3");
        std::fs::write(&path, vec![0x5a; 4096]).unwrap();
        vault::write_config(
            &session.env,
            &Config::encrypted(
                path.to_string_lossy().into_owned(),
                false,
                vault::VaultId::from_array([0x22; 16]),
            ),
        )
        .unwrap();
        let status = send(
            &mut session,
            &FakeDialogs::default(),
            json!({ "type": "vault_status" }),
        );
        // Slice B: no key for it in this user's key store, and no sidecar to recover from.
        assert_eq!(status["status"]["state"], "key_unavailable");
        assert_eq!(status["status"]["recoveryAvailable"], false);
        assert_eq!(status["status"]["mode"], "encrypted");
        assert_eq!(status["status"]["publication"], "pending");
        assert_eq!(status["status"]["vaultToken"], json!(null));
        // Creating over it is refused; the file is untouched.
        let again = send(
            &mut session,
            &FakeDialogs::default(),
            json!({ "type": "vault_create", "path": path }),
        );
        assert_eq!(again["error"], "file_exists");
        assert_eq!(std::fs::read(&path).unwrap(), vec![0x5a; 4096]);
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
