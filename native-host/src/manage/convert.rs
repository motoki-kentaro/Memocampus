//! Converting the encryption state of the active vault (Issue #51, Slice D): a plain vault becomes a
//! **new** encrypted vault (D1, `encrypt`), an encrypted vault a **new** plain one (D2, `decrypt`).
//! The source vault is never written, moved or removed, and the active vault does not change until
//! the user activates the converted one.
//!
//! # Messages
//!
//! - `vault_convert_start {direction, expectedVaultToken, path?, approveWarnings?,
//!   acknowledgePlaintext?}`: the source is the active vault the panel shows (`expectedVaultToken`);
//!   `direction` must be the other mode (`encrypt` for a plain vault, `decrypt` for an encrypted
//!   one). `decrypt` needs `acknowledgePlaintext: true`: the user confirmed that the new vault keeps
//!   everything unencrypted on disk. Picks the target (a new file; dialog unless `path`), copies,
//!   verifies. D1 answers the new Recovery Spell, once; D2 answers the converted vault.
//! - `vault_convert_confirm {recoverySpell}` (D1): the spell re-entered in full; it must open the new
//!   vault's envelope to its key. Then the files get their final names.
//! - `vault_convert_activate {expectedVaultToken}`: the converted vault (its `vaultToken`) becomes
//!   the active vault, publication pending. Under the switch's fence over the old vault the copy is
//!   refreshed from the source first, so no write to the source since the conversion is lost.
//! - `vault_convert_keep {}`: ends the conversion without activating: the converted vault stays, as
//!   a vault that can be opened later (a snapshot of the source at conversion time).
//! - `vault_convert_discard {}`: removes what this conversion created (never the source, never the
//!   active vault).
//!
//! # Transaction
//!
//! The manifest `<app dir>\vault-convert.json` records one conversion at a time; while it exists no
//! other conversion, encrypted creation or restore starts (and no other of those lets a conversion
//! start). The copy is first written to a work file next to the target with an unpredictable name
//! (`.<target name>.<128-bit random hex>.partial`), recorded in the manifest before it is created.
//!
//! | Phase | On disk | Next |
//! |---|---|---|
//! | `converting` | the work file (being written) | discard (then start again) |
//! | `verified` | the work file, verified; D1: `keys\<newId>.json.pending`, `<target>.mckey.pending` | D1: confirm (spell) or discard; D2: activate / keep (promotes first) or discard |
//! | `converted` | `<target>` (D1: + `<target>.mckey`, `keys\<newId>.json`) | activate, keep or discard |
//! | none | activated (the config names it, publication pending) or kept | — |
//!
//! - Every file is created new (`create_new`) and promoted with `rename_no_replace`.
//! - A request out of order is refused (`vault_convert_incomplete`, `vault_convert_spell_unconfirmed`,
//!   `vault_convert_not_pending`, `vault_convert_invalid_state`).
//! - The Recovery Spell of D1 exists only in memory and in the one response of `start`. A process
//!   that dies before the user confirmed it leaves a `verified` conversion that the user confirms
//!   (if the spell was written down) or discards; it is never shown again.
//! - Discarding removes only what is provably this conversion's: the recorded work file, a target
//!   with the recorded file identity, sidecars and key store entries of the new vault id. Never a
//!   file with the source's identity, never the active vault.
//! - The source stays in use meanwhile (the copy holds its read lock only while copying).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use super::backup::{checkpoint, crashed, output_failure, Step};
use super::encrypted::{self, key_store_failure, promote_file, requested_spell, GivenKey};
use super::{
    approved, check_location, display, normalize_new, requested_path, require_management, status,
    Failure,
};
use crate::dialog::Dialogs;
use crate::location;
use crate::vault::{
    self, Active, ConfigLock, KeyRequest, KeyStore, NewActive, Session, VaultError,
};
use memocampus_native_host::active_vault::VaultMode;
use memocampus_native_host::secure_vault::convert::{self as copy, CopyError, Ends};
use memocampus_native_host::secure_vault::secret::{CipherSalt, RecoverySpell, VaultId, VaultKey};
use memocampus_native_host::secure_vault::{cipher, fsutil, key_store, recovery, sidecar, spell};
use zeroize::Zeroizing;

pub const MANIFEST: &str = "vault-convert.json";
const MANIFEST_VERSION: u32 = 1;
const WORK_SUFFIX: &str = ".partial";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Encrypt,
    Decrypt,
}

impl Direction {
    fn code(self) -> &'static str {
        match self {
            Self::Encrypt => "encrypt",
            Self::Decrypt => "decrypt",
        }
    }

    fn source_mode(self) -> VaultMode {
        match self {
            Self::Encrypt => VaultMode::Plain,
            Self::Decrypt => VaultMode::Encrypted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Converting,
    Verified,
    Converted,
}

impl Phase {
    fn code(self) -> &'static str {
        match self {
            Self::Converting => "converting",
            Self::Verified => "verified",
            Self::Converted => "converted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Manifest {
    version: u32,
    direction: Direction,
    /// The source vault, as the config named it, and its file identity when the conversion began.
    source: String,
    source_token: String,
    /// D2: the source's vault id (its key is this user's key store entry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_vault_id: Option<VaultId>,
    target: String,
    work: String,
    phase: Phase,
    /// D1: the new vault's id (generated before anything is written) and, once known, its salt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vault_id: Option<VaultId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cipher_salt: Option<CipherSalt>,
    /// The converted file's identity, from verification on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    location_approved: bool,
}

impl Manifest {
    fn target(&self) -> PathBuf {
        PathBuf::from(&self.target)
    }

    fn work(&self) -> PathBuf {
        PathBuf::from(&self.work)
    }

    fn mode(&self) -> VaultMode {
        match self.direction {
            Direction::Encrypt => VaultMode::Encrypted,
            Direction::Decrypt => VaultMode::Plain,
        }
    }

    /// What every reader requires: absolute paths, the work file next to the target under a name
    /// of the reserved form, and the ids each direction and phase must (not) have.
    fn consistent(&self) -> bool {
        let target = self.target();
        let work = self.work();
        let work_named = match (target.file_name(), work.file_name()) {
            (Some(target_name), Some(work_name)) => work_name
                .to_string_lossy()
                .strip_prefix(&format!(".{}.", target_name.to_string_lossy()))
                .and_then(|rest| rest.strip_suffix(WORK_SUFFIX))
                .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())),
            _ => false,
        };
        let ids = match self.direction {
            Direction::Encrypt => {
                self.vault_id.is_some()
                    && self.source_vault_id.is_none()
                    && (self.phase == Phase::Converting || self.cipher_salt.is_some())
            }
            Direction::Decrypt => {
                self.vault_id.is_none()
                    && self.cipher_salt.is_none()
                    && self.source_vault_id.is_some()
            }
        };
        self.version == MANIFEST_VERSION
            && target.is_absolute()
            && Path::new(&self.source).is_absolute()
            && work.parent() == target.parent()
            && work_named
            && ids
            && (self.phase == Phase::Converting || self.token.is_some())
            && memocampus_native_host::active_vault::is_token(&self.source_token)
    }
}

fn app_dir(session: &Session) -> Result<PathBuf, VaultError> {
    session
        .env
        .app_dir
        .clone()
        .ok_or(VaultError::AppDirUnavailable)
}

fn manifest_path(session: &Session) -> Result<PathBuf, VaultError> {
    Ok(app_dir(session)?.join(MANIFEST))
}

fn read_manifest(session: &Session) -> Result<Option<Manifest>, Failure> {
    match fsutil::read_bounded(&manifest_path(session)?, 16 * 1024) {
        Ok(bytes) => match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(manifest) if manifest.consistent() => Ok(Some(manifest)),
            _ => Err(Failure::with("vault_convert_damaged", json!({}))),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(VaultError::Io.into()),
    }
}

fn write_manifest(session: &Session, manifest: &Manifest, new: bool) -> Result<(), Failure> {
    let bytes = serde_json::to_vec_pretty(manifest).expect("a manifest serializes");
    let path = manifest_path(session)?;
    let written = if new {
        fsutil::publish_new(&path, &bytes)
    } else {
        fsutil::publish_replacing(&path, &bytes)
    };
    written.map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => Failure::with("vault_convert_pending", json!({})),
        _ => VaultError::Io.into(),
    })
}

/// The conversion already became the active vault (a crash before its manifest was removed).
fn completed(session: &Session, manifest: &Manifest) -> bool {
    matches!(vault::resolve(&session.env), Ok(Active::Configured(config))
        if config.active_vault == manifest.target
            && config.mode == manifest.mode()
            && config.vault_id == manifest.vault_id)
}

/// For the status: the conversion the manifest records (not yet activated), or null. An unreadable
/// manifest is reported so that it can be discarded.
pub fn pending_conversion(session: &Session) -> Value {
    match read_manifest(session) {
        Ok(Some(manifest)) if !completed(session, &manifest) => json!({
            "direction": manifest.direction.code(),
            "phase": manifest.phase.code(),
            "path": manifest.target,
            "sourcePath": manifest.source,
            "vaultToken": manifest.token,
        }),
        Ok(_) => Value::Null,
        Err(_) => json!({ "path": null, "phase": "damaged" }),
    }
}

/// Refuses a vault operation that must not run beside an unfinished conversion.
pub fn refuse_while_pending(session: &Session) -> Result<(), Failure> {
    match read_manifest(session) {
        Ok(Some(manifest)) if completed(session, &manifest) => Ok(()),
        Ok(None) => Ok(()),
        Ok(Some(manifest)) => Err(Failure::with(
            "vault_convert_pending",
            json!({ "path": manifest.target }),
        )),
        Err(_) => Err(Failure::with(
            "vault_convert_pending",
            json!({ "path": null }),
        )),
    }
}

/// `vault_open` / `vault_recover` of the pending conversion's target or work file: refused, so
/// that the copy is never used (and written to) before it is activated or kept.
pub fn refuse_converting_file(session: &Session, path: &Path) -> Result<(), Failure> {
    let Ok(Some(manifest)) = read_manifest(session) else {
        return Ok(());
    };
    if completed(session, &manifest) {
        return Ok(());
    }
    let token = vault::vault_token(path).ok();
    let same = |candidate: &Path| {
        candidate == path || (token.is_some() && token == vault::vault_token(candidate).ok())
    };
    if same(&manifest.target()) || same(&manifest.work()) {
        return Err(Failure::with(
            "vault_convert_pending",
            json!({ "path": manifest.target }),
        ));
    }
    Ok(())
}

/// The failure a copy error is reported as. Fixed codes; nothing about the vault's content.
fn copy_failure(error: CopyError) -> Failure {
    match error {
        CopyError::SourceChanged => Failure::with("vault_convert_source_changed", json!({})),
        CopyError::Busy => VaultError::Busy.into(),
        CopyError::DiskFull => Failure::with("disk_full", json!({})),
        CopyError::Keying => VaultError::DecryptFailed.into(),
        CopyError::Mismatch => Failure::with("vault_convert_verify_failed", json!({})),
        CopyError::Io => VaultError::Io.into(),
        CopyError::Db => Failure::with("vault_convert_failed", json!({})),
    }
}

fn random_failure() -> Failure {
    Failure::with("random_unavailable", json!({}))
}

fn step(step: Step) -> Result<(), Failure> {
    checkpoint(step).map_err(|error| output_failure(&error, "vault_convert_failed"))
}

/// The key of the encrypted source (D2): this Windows user's key store entry for its id and salt.
fn source_key(session: &Session, path: &Path, vault_id: VaultId) -> Result<VaultKey, Failure> {
    let salt = match cipher::read_head(path) {
        Ok(cipher::Head::Salt(salt)) => salt,
        Ok(_) => return Err(VaultError::ModeMismatch.into()),
        Err(_) => return Err(VaultError::Io.into()),
    };
    session
        .keys()
        .key_for(&KeyRequest {
            vault_id: Some(vault_id),
            salt,
        })
        .map_err(|error| vault::key_kind(error).into())
}

fn default_name(source: &Path, direction: Direction) -> String {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "memocampus".into());
    match direction {
        Direction::Encrypt => format!("{stem}-encrypted.sqlite3"),
        Direction::Decrypt => format!("{stem}-plain.sqlite3"),
    }
}

/// `vault_convert_start`.
pub fn start(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    message: &Value,
) -> Result<Value, Failure> {
    require_management(session)?;
    let direction = match message.get("direction").and_then(Value::as_str) {
        Some("encrypt") => Direction::Encrypt,
        Some("decrypt") => Direction::Decrypt,
        _ => return Err(Failure::with("invalid_request", json!({}))),
    };
    let expected = message
        .get("expectedVaultToken")
        .and_then(Value::as_str)
        .filter(|token| crate::image_original::is_hex64(token))
        .ok_or_else(|| Failure::with("invalid_vault_token", json!({})))?
        .to_owned();
    if direction == Direction::Decrypt
        && message.get("acknowledgePlaintext").and_then(Value::as_bool) != Some(true)
    {
        return Err(Failure::with(
            "vault_convert_plaintext_unacknowledged",
            json!({}),
        ));
    }
    // One vault operation of this kind at a time.
    if let Some(manifest) = read_manifest(session)? {
        if !completed(session, &manifest) {
            return Err(Failure::with(
                "vault_convert_pending",
                json!({ "path": manifest.target }),
            ));
        }
        let _ = fs::remove_file(manifest_path(session)?);
    }
    if let Some(path) = encrypted::pending_creation(session) {
        return Err(Failure::with(
            "vault_creation_pending",
            json!({ "path": path }),
        ));
    }
    if !super::restore::pending_restore(session).is_null() {
        return Err(Failure::with("vault_restore_pending", json!({})));
    }
    // The source: the active vault the panel shows, opened (a hot journal is rolled back here).
    let ticket = session.ticket()?;
    let active = session.active_target()?;
    if ticket.token != expected || active.path != ticket.path {
        return Err(VaultError::Switched.into());
    }
    if active.mode != direction.source_mode() {
        return Err(Failure::with(
            "vault_convert_mode_mismatch",
            json!({ "mode": active.mode.code() }),
        ));
    }
    let source = PathBuf::from(&ticket.path);
    let source_vault_id = match direction {
        Direction::Encrypt => None,
        Direction::Decrypt => Some(active.vault_id.ok_or(VaultError::ConfigInvalid)?),
    };
    // Neither a connection nor a lock is held while the dialog is open.
    session.reset();
    let target = match requested_path(message, "path")? {
        Some(path) => path,
        None => dialogs
            .choose_convert_target(
                source.parent().unwrap_or(Path::new("")),
                &default_name(&source, direction),
                direction == Direction::Encrypt,
            )
            .ok_or(VaultError::Cancelled)?,
    };
    let target = normalize_new(&target)?;
    if target.exists() {
        // However its path is spelled (an 8.3 alias, another case), the source itself.
        if vault::vault_token(&target).ok().as_deref() == Some(ticket.token.as_str()) {
            return Err(Failure::with(
                "vault_convert_same_as_source",
                json!({ "path": display(&target) }),
            ));
        }
        return Err(Failure::with(
            "file_exists",
            json!({ "path": display(&target) }),
        ));
    }
    let mut reserved = vec![
        sidecar::path_for(&target),
        sidecar::pending_path_for(&target),
    ];
    for suffix in ["-journal", "-wal", "-shm"] {
        reserved.push(fsutil::with_suffix(&target, suffix));
    }
    if let Some(taken) = reserved.iter().find(|path| path.exists()) {
        return Err(Failure::with(
            "file_exists",
            json!({ "path": display(taken) }),
        ));
    }
    let assessment = location::probe(&target, session.env.exe_dir.as_deref());
    let location_approved = check_location(&assessment, &target, approved(message))?;
    let name = target
        .file_name()
        .ok_or(VaultError::InvalidPath)?
        .to_string_lossy()
        .into_owned();
    let work = target.with_file_name(format!(
        ".{name}.{}{WORK_SUFFIX}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut manifest = Manifest {
        version: MANIFEST_VERSION,
        direction,
        source: ticket.path.clone(),
        source_token: ticket.token.clone(),
        source_vault_id,
        target: display(&target),
        work: display(&work),
        phase: Phase::Converting,
        vault_id: match direction {
            Direction::Encrypt => Some(VaultId::generate().map_err(|_| random_failure())?),
            Direction::Decrypt => None,
        },
        cipher_salt: None,
        token: None,
        location_approved,
    };
    fs::create_dir_all(app_dir(session)?).map_err(|_| VaultError::Io)?;
    write_manifest(session, &manifest, true)?;
    let outcome = match direction {
        Direction::Encrypt => encrypt(session, &mut manifest),
        Direction::Decrypt => decrypt(session, &mut manifest),
    };
    match outcome {
        Ok(mut answer) => {
            session.reset();
            answer["path"] = display(&target).into();
            answer["sourcePath"] = ticket.path.into();
            answer["direction"] = direction.code().into();
            answer["phase"] = manifest.phase.code().into();
            answer["vaultToken"] = json!(manifest.token);
            answer["status"] = status(session);
            Ok(answer)
        }
        Err(mut failure) => {
            if !crashed() {
                remove_converted(session, &manifest);
            }
            failure.extra.insert("path".into(), display(&target).into());
            Err(failure)
        }
    }
}

/// The empty work file, created new under the name the manifest recorded.
fn create_work(manifest: &Manifest) -> Result<(), Failure> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(manifest.work())
        .map(drop)
        .map_err(|error| output_failure(&error, "vault_convert_failed"))
}

/// Counts of a verified copy, for the user to compare with the source.
fn counts_of(path: &Path, mode: VaultMode, key: Option<&VaultKey>) -> Value {
    let keys: Box<dyn vault::KeySource> = match key {
        Some(key) => Box::new(GivenKey(key.duplicate())),
        None => Box::new(vault::NoKeys),
    };
    let vault_id = key.map(|_| VaultId::from_array([0; 16]));
    vault::open_vault_in(path, mode, vault_id, keys.as_ref())
        .ok()
        .and_then(|db| vault::content_counts(&db).ok())
        .map_or(Value::Null, |(counts, images, _)| {
            json!({ "visits": counts.visits, "snapshots": counts.snapshots, "images": images.1 })
        })
}

/// D1: copy into the keyed work file, verify, protect the new key with DPAPI and wrap it with a new
/// Recovery Spell; answers the spell, once.
fn encrypt(session: &Session, manifest: &mut Manifest) -> Result<Value, Failure> {
    let vault_id = manifest.vault_id.ok_or(VaultError::ConfigInvalid)?;
    let key = VaultKey::generate().map_err(|_| random_failure())?;
    let secret = RecoverySpell::generate().map_err(|_| random_failure())?;
    let work = manifest.work();
    create_work(manifest)?;
    step(Step::ConvertCopy)?;
    copy::export(&Ends {
        source: Path::new(&manifest.source),
        source_key: None,
        source_token: &manifest.source_token,
        target: &work,
        target_key: Some(&key),
    })
    .map_err(copy_failure)?;
    step(Step::ConvertVerify)?;
    copy::verify(&work, Some(&key)).map_err(copy_failure)?;
    let salt = match cipher::read_head(&work) {
        Ok(cipher::Head::Salt(salt)) => salt,
        _ => return Err(copy_failure(CopyError::Mismatch)),
    };
    manifest.cipher_salt = Some(salt);
    write_manifest(session, manifest, false)?;
    step(Step::ConvertKeys)?;
    let app = app_dir(session)?;
    key_store::store(&app, &vault_id, &salt, &key, key_store::Write::Pending)
        .map_err(key_store_failure)?;
    let envelope = recovery::wrap(&key, &secret, &vault_id, &salt).map_err(|_| random_failure())?;
    sidecar::write_new(
        &sidecar::pending_path_for(&manifest.target()),
        &sidecar::Sidecar::new(vault_id, salt, envelope),
    )
    .map_err(|error| output_failure(&error, "vault_convert_failed"))?;
    verify_keys(session, manifest, &secret)?;
    let counts = counts_of(&work, VaultMode::Encrypted, Some(&key));
    drop(key);
    manifest.token = Some(vault::vault_token(&work)?);
    manifest.phase = Phase::Verified;
    write_manifest(session, manifest, false)?;
    // The only time this spell leaves the host: in this response to the panel, nowhere else.
    let shown: Zeroizing<String> = spell::encode(&secret);
    drop(secret);
    Ok(json!({ "recoverySpell": shown.as_str(), "counts": counts }))
}

/// D2: copy the encrypted source (keyed with this user's key) into the plain work file, verify,
/// promote it to the target name.
fn decrypt(session: &Session, manifest: &mut Manifest) -> Result<Value, Failure> {
    let vault_id = manifest.source_vault_id.ok_or(VaultError::ConfigInvalid)?;
    let source = PathBuf::from(&manifest.source);
    let key = source_key(session, &source, vault_id)?;
    let work = manifest.work();
    create_work(manifest)?;
    step(Step::ConvertCopy)?;
    copy::export(&Ends {
        source: &source,
        source_key: Some(&key),
        source_token: &manifest.source_token,
        target: &work,
        target_key: None,
    })
    .map_err(copy_failure)?;
    drop(key);
    step(Step::ConvertVerify)?;
    copy::verify(&work, None).map_err(copy_failure)?;
    let counts = counts_of(&work, VaultMode::Plain, None);
    manifest.token = Some(vault::vault_token(&work)?);
    manifest.phase = Phase::Verified;
    write_manifest(session, manifest, false)?;
    promote(session, manifest)?;
    Ok(json!({ "counts": counts }))
}

/// D1: every pending file opens to the same key: the DPAPI entry, the envelope with `secret`, and
/// the converted database. Each file under its pending name, or under its final name once promoted.
fn verify_keys(
    session: &Session,
    manifest: &Manifest,
    secret: &RecoverySpell,
) -> Result<VaultKey, Failure> {
    let damaged = || Failure::with("vault_convert_damaged", json!({}));
    let (Some(vault_id), Some(salt)) = (manifest.vault_id, manifest.cipher_salt) else {
        return Err(damaged());
    };
    let target = manifest.target();
    let app = app_dir(session)?;
    let pick = |pending: PathBuf, promoted: PathBuf| {
        if pending.exists() {
            pending
        } else {
            promoted
        }
    };
    let stored = key_store::unlock_pending(&app, &vault_id, &salt)
        .or_else(|_| key_store::unlock(&app, &vault_id, &salt))
        .map_err(|_| damaged())?;
    let found = sidecar::read(&pick(
        sidecar::pending_path_for(&target),
        sidecar::path_for(&target),
    ))
    .map_err(|_| damaged())?;
    if found.vault_id != vault_id || found.cipher_salt != salt {
        return Err(damaged());
    }
    let unwrapped = recovery::unwrap(&found.recovery, secret, &vault_id, &salt).map_err(
        |error| match error {
            recovery::UnwrapError::SpellRejected => {
                Failure::with("recovery_spell_rejected", json!({}))
            }
            _ => damaged(),
        },
    )?;
    if !unwrapped.same_as(&stored) {
        return Err(damaged());
    }
    let db_path = pick(manifest.work(), target);
    if cipher::read_head(&db_path).ok() != Some(cipher::Head::Salt(salt)) {
        return Err(damaged());
    }
    vault::open_checked_in(
        &db_path,
        VaultMode::Encrypted,
        Some(vault_id),
        &GivenKey(stored.duplicate()),
    )
    .map_err(|_| damaged())?;
    Ok(stored)
}

/// Moves the verified files to their final names (no replacement), idempotently.
fn promote(session: &Session, manifest: &mut Manifest) -> Result<(), Failure> {
    step(Step::ConvertPromote)?;
    let target = manifest.target();
    let token = manifest.token.clone().ok_or(VaultError::Io)?;
    let promoted = (|| -> io::Result<()> {
        if let (Some(vault_id), Some(salt)) = (manifest.vault_id, manifest.cipher_salt) {
            let app = app_dir(session).map_err(|_| io::Error::other("app dir"))?;
            key_store::promote_pending(&app, &vault_id, &salt)?;
            promote_file(
                &sidecar::pending_path_for(&target),
                &sidecar::path_for(&target),
                |path| {
                    sidecar::read(path)
                        .is_ok_and(|found| found.vault_id == vault_id && found.cipher_salt == salt)
                },
            )?;
        }
        promote_file(&manifest.work(), &target, |path| {
            vault::vault_token(path).ok().as_deref() == Some(token.as_str())
        })
    })();
    promoted.map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => {
            Failure::with("file_exists", json!({ "path": display(&target) }))
        }
        _ => VaultError::Io.into(),
    })?;
    manifest.phase = Phase::Converted;
    write_manifest(session, manifest, false)
}

/// `vault_convert_confirm` (D1): the new spell, re-entered in full.
pub fn confirm(session: &mut Session, message: &Value) -> Result<Value, Failure> {
    require_management(session)?;
    let secret = requested_spell(message)?;
    let _lock = ConfigLock::acquire(&session.env)?;
    let mut manifest = match read_manifest(session)? {
        Some(manifest) if !completed(session, &manifest) => manifest,
        _ => return Err(Failure::with("vault_convert_not_pending", json!({}))),
    };
    if manifest.direction != Direction::Encrypt {
        return Err(Failure::with("vault_convert_invalid_state", json!({})));
    }
    if manifest.phase == Phase::Converting {
        return Err(Failure::with(
            "vault_convert_incomplete",
            json!({ "path": manifest.target }),
        ));
    }
    // A wrong spell leaves everything as it was: the user may try again.
    verify_keys(session, &manifest, &secret)?;
    drop(secret);
    if manifest.phase == Phase::Verified {
        promote(session, &mut manifest)?;
    }
    session.reset();
    Ok(json!({
        "path": manifest.target,
        "vaultToken": manifest.token,
        "phase": manifest.phase.code(),
        "status": status(session),
    }))
}

/// The manifest of a conversion that may be activated or kept: converted (D2 verified: promoted
/// now). Out-of-order requests are refused.
fn ready(session: &Session, manifest: &mut Manifest) -> Result<(), Failure> {
    match (manifest.direction, manifest.phase) {
        (_, Phase::Converting) => Err(Failure::with(
            "vault_convert_incomplete",
            json!({ "path": manifest.target }),
        )),
        (Direction::Encrypt, Phase::Verified) => Err(Failure::with(
            "vault_convert_spell_unconfirmed",
            json!({ "path": manifest.target }),
        )),
        (Direction::Decrypt, Phase::Verified) => promote(session, manifest),
        (_, Phase::Converted) => Ok(()),
    }
}

/// `vault_convert_activate`: the converted vault becomes the active vault, publication pending.
pub fn activate(session: &mut Session, message: &Value) -> Result<Value, Failure> {
    require_management(session)?;
    let expected = message
        .get("expectedVaultToken")
        .and_then(Value::as_str)
        .filter(|token| crate::image_original::is_hex64(token))
        .ok_or_else(|| Failure::with("invalid_vault_token", json!({})))?
        .to_owned();
    let lock = ConfigLock::acquire(&session.env)?;
    let mut manifest = match read_manifest(session)? {
        Some(manifest) => manifest,
        None => return Err(Failure::with("vault_convert_not_pending", json!({}))),
    };
    let target = manifest.target();
    if completed(session, &manifest) {
        let _ = fs::remove_file(manifest_path(session)?);
        session.reset();
        return Ok(json!({ "path": display(&target), "status": status(session) }));
    }
    if manifest.token.as_deref() != Some(expected.as_str()) && manifest.phase != Phase::Converting {
        return Err(VaultError::Switched.into());
    }
    ready(session, &mut manifest)?;
    if vault::vault_token(&target).ok() != manifest.token {
        return Err(Failure::with(
            "vault_convert_damaged",
            json!({ "path": display(&target) }),
        ));
    }
    let source = PathBuf::from(&manifest.source);
    let app = app_dir(session)?;
    // The keys of both ends: the new vault's (D1, its promoted key store entry) or the source's
    // (D2, this user's entry).
    let (source_key, target_key) = match manifest.direction {
        Direction::Encrypt => {
            let (Some(vault_id), Some(salt)) = (manifest.vault_id, manifest.cipher_salt) else {
                return Err(Failure::with("vault_convert_damaged", json!({})));
            };
            let key = key_store::unlock(&app, &vault_id, &salt)
                .map_err(|_| Failure::from(VaultError::KeyUnavailable))?;
            (None, Some(key))
        }
        Direction::Decrypt => {
            let vault_id = manifest.source_vault_id.ok_or(VaultError::ConfigInvalid)?;
            (Some(source_key(session, &source, vault_id)?), None)
        }
    };
    let new = match (manifest.direction, manifest.vault_id) {
        (Direction::Encrypt, Some(vault_id)) => NewActive::encrypted(&target, vault_id),
        (Direction::Decrypt, None) => NewActive::plain(&target),
        _ => return Err(Failure::with("vault_convert_damaged", json!({}))),
    };
    let keys = KeyStore::of(&session.env);
    vault::open_checked_in(&target, new.mode, new.vault_id, &keys).map_err(|failure| {
        Failure::with(failure.kind.code(), json!({ "path": display(&target) }))
    })?;
    let mut refreshed: Option<Failure> = None;
    let switched = vault::switch_active_fenced(
        &session.env,
        &lock,
        &new,
        manifest.location_approved,
        &keys,
        true,
        &mut || {
            // Writers of the old active vault (normally the source) are fenced off: the copy is
            // brought up to date with every write the source took since the conversion.
            let result = step(Step::ConvertRefresh)
                .and_then(|()| {
                    copy::refresh(&Ends {
                        source: &source,
                        source_key: source_key.as_ref(),
                        source_token: &manifest.source_token,
                        target: &target,
                        target_key: target_key.as_ref(),
                    })
                    .map_err(copy_failure)
                })
                .and_then(|()| copy::verify(&target, target_key.as_ref()).map_err(copy_failure));
            result.map_err(|failure| {
                refreshed = Some(failure);
                VaultError::Io
            })
        },
    );
    if let Err(error) = switched {
        let mut failure = refreshed.unwrap_or_else(|| error.into());
        failure.extra.insert("path".into(), display(&target).into());
        return Err(failure);
    }
    let _ = fs::remove_file(manifest_path(session)?);
    drop(lock);
    session.reset();
    Ok(json!({ "path": display(&target), "status": status(session) }))
}

/// `vault_convert_keep`: the conversion ends without activation; the converted vault stays.
pub fn keep(session: &mut Session) -> Result<Value, Failure> {
    require_management(session)?;
    let _lock = ConfigLock::acquire(&session.env)?;
    let mut manifest = match read_manifest(session)? {
        Some(manifest) if !completed(session, &manifest) => manifest,
        _ => return Err(Failure::with("vault_convert_not_pending", json!({}))),
    };
    ready(session, &mut manifest)?;
    let _ = fs::remove_file(manifest_path(session)?);
    session.reset();
    Ok(json!({ "path": manifest.target, "status": status(session) }))
}

/// Removes what the recorded conversion provably created, and the manifest. Never the source,
/// never the active vault.
fn remove_converted(session: &Session, manifest: &Manifest) {
    let target = manifest.target();
    let work = manifest.work();
    let active = |path: &Path| {
        matches!(vault::resolve(&session.env), Ok(Active::Configured(config))
            if Path::new(&config.active_vault) == path)
    };
    let source = |path: &Path| {
        path == Path::new(&manifest.source)
            || vault::vault_token(path).ok().as_deref() == Some(manifest.source_token.as_str())
    };
    let ours = |path: &Path| match &manifest.token {
        Some(token) => vault::vault_token(path).ok().as_deref() == Some(token.as_str()),
        // Before verification only the work file this conversion created (`create_new`, under the
        // unpredictable name it recorded first) can be its own.
        None => path == work,
    };
    for db in [work.clone(), target.clone()] {
        if active(&db) || source(&db) {
            continue;
        }
        let removable = db.exists() && ours(&db);
        if removable {
            let _ = fs::remove_file(&db);
        }
        if removable || (db == work && !db.exists()) {
            let _ = fs::remove_file(fsutil::with_suffix(&db, "-journal"));
        }
    }
    if let Some(vault_id) = manifest.vault_id {
        for path in [
            sidecar::pending_path_for(&target),
            sidecar::path_for(&target),
        ] {
            if sidecar::identity(&path).is_some_and(|(id, salt)| {
                id == vault_id && manifest.cipher_salt.is_none_or(|known| known == salt)
            }) {
                let _ = fs::remove_file(&path);
            }
        }
        if let Some(app) = session.env.app_dir.as_deref() {
            key_store::remove_created(app, &vault_id, manifest.cipher_salt.as_ref());
        }
    }
    if let Ok(path) = manifest_path(session) {
        let _ = fs::remove_file(path);
    }
}

/// `vault_convert_discard`.
pub fn discard(session: &mut Session) -> Result<Value, Failure> {
    require_management(session)?;
    let _lock = ConfigLock::acquire(&session.env)?;
    match read_manifest(session) {
        Ok(Some(manifest)) if !completed(session, &manifest) => {
            remove_converted(session, &manifest)
        }
        // Activated already, nothing recorded, or a manifest this version cannot read (it names
        // nothing that could safely be removed): only the manifest goes.
        _ => {
            let _ = fs::remove_file(manifest_path(session)?);
        }
    }
    session.reset();
    Ok(json!({ "status": status(session) }))
}
