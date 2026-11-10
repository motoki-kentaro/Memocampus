//! Encrypted vaults in the side panel (Issue #51, Slice B): creation with a Recovery Spell
//! (ふっかつのじゅもん), opening an existing one, and recovery with the spell.
//!
//! # Creation (`vault_create_encrypted` → `vault_create_encrypted_commit` | `_cancel`)
//!
//! Chrome starts a host process per message, so a creation spans processes. Its state is the
//! manifest `<app dir>\vault-create.json` and three pending files, none of which can be taken for
//! a usable vault:
//!
//! | Phase (manifest) | Files | Active vault |
//! |---|---|---|
//! | none | — | unchanged |
//! | `creating` | `<target>.pending` (keyed DB), `keys\<id>.json.pending` (DPAPI), `<target>.mckey.pending` | unchanged |
//! | `awaiting_confirmation` | all three, verified; the spell was shown once | unchanged |
//! | `promoted` | `<target>`, `keys\<id>.json`, `<target>.mckey` | unchanged |
//! | none (committed) | the same | `<target>`, publication pending |
//!
//! - Begin generates VK, the Recovery Spell (256 CSPRNG bits) and the vault id, writes the files
//!   (each never replacing anything), checks them, and returns the spell (43 base64url characters)
//!   once. The spell and VK are never written anywhere in the clear: VK only DPAPI-protected and
//!   spell-wrapped, the spell nowhere. No message shows it again.
//! - Commit needs the whole spell as the user re-entered (typed or pasted) it in the panel, all 43
//!   characters: the host checks that it opens the envelope written at begin, i.e. that the spell
//!   the user kept is the vault's. A mistyped spell is refused (`recovery_spell_rejected` or
//!   `recovery_spell_invalid`) and the creation stays pending, so the user can try again. It then verifies the DB, promotes the files (no replacement) and switches the active
//!   vault, which is the commit point. A retried commit continues from where a crash stopped.
//! - Cancel removes only files that name this creation's vault id and salt, never an active vault.
//! - While a manifest exists no other creation starts (`vault_creation_pending`), so an interrupted
//!   creation can never be mixed up with a new key. The spell cannot be shown again: an
//!   unconfirmed creation is cancelled and started over.
//!
//! # Opening and recovery
//!
//! `vault_open` of a file that is not plain SQLite finds its vault id (sidecar, else this user's
//! key store by salt) and unlocks it with DPAPI. If this Windows user has no usable key
//! (`key_unavailable`), nothing changes and the panel offers recovery: `vault_recover` takes the
//! spell, opens the sidecar's envelope, proves the key on the vault itself, and only then stores a
//! new DPAPI entry for this user and makes the vault active, publication pending. A wrong spell
//! or damaged metadata changes nothing. A sidecar of the 12-word phrase (version 1) is
//! `recovery_metadata_unsupported`: such a vault still opens with this user's DPAPI key, but
//! cannot be recovered or backed up as B/D by this build.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{
    approved, check_location, display, normalize_existing, normalize_new, requested_path,
    require_management, status, Failure, DEFAULT_VAULT_NAME,
};
use crate::dialog::Dialogs;
use crate::location;
use crate::vault::{
    self, Active, CipherSalt, ConfigLock, KeyError, KeyRequest, KeySource, KeyStore, NewActive,
    Session, VaultError, VaultId, VaultKey,
};
use memocampus_native_host::active_vault::VaultMode;
use memocampus_native_host::secure_vault::{
    cipher, fsutil, key_store, recovery, secret::RecoverySpell, sidecar, spell,
};
use zeroize::Zeroizing;

pub const MANIFEST: &str = "vault-create.json";
const MANIFEST_VERSION: u32 = 1;
const PENDING_DB: &str = ".pending";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Creating,
    AwaitingConfirmation,
    Promoted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Manifest {
    version: u32,
    vault_id: VaultId,
    target: String,
    phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cipher_salt: Option<CipherSalt>,
    location_approved: bool,
}

impl Manifest {
    fn target(&self) -> PathBuf {
        PathBuf::from(&self.target)
    }
}

fn manifest_path(session: &Session) -> Result<PathBuf, VaultError> {
    Ok(session
        .env
        .app_dir
        .as_deref()
        .ok_or(VaultError::AppDirUnavailable)?
        .join(MANIFEST))
}

fn app_dir(session: &Session) -> Result<PathBuf, VaultError> {
    session
        .env
        .app_dir
        .clone()
        .ok_or(VaultError::AppDirUnavailable)
}

fn read_manifest(session: &Session) -> Result<Option<Manifest>, Failure> {
    match fsutil::read_bounded(&manifest_path(session)?, 16 * 1024) {
        Ok(bytes) => match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(manifest)
                if manifest.version == MANIFEST_VERSION
                    && Path::new(&manifest.target).is_absolute() =>
            {
                Ok(Some(manifest))
            }
            _ => Err(Failure::with("vault_creation_damaged", json!({}))),
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
        io::ErrorKind::AlreadyExists => Failure::with("vault_creation_pending", json!({})),
        _ => VaultError::Io.into(),
    })
}

fn pending_db(target: &Path) -> PathBuf {
    fsutil::with_suffix(target, PENDING_DB)
}

/// The creation the manifest records, for the status shown in the panel: its target path, or
/// None. A creation that already became the active vault (a crash before the manifest was
/// removed) is not pending.
pub fn pending_creation(session: &Session) -> Option<String> {
    match read_manifest(session) {
        Ok(Some(manifest)) if !completed(session, &manifest) => Some(manifest.target),
        Ok(_) => None,
        Err(_) => Some(String::new()),
    }
}

fn completed(session: &Session, manifest: &Manifest) -> bool {
    matches!(vault::resolve(&session.env), Ok(Active::Configured(config))
        if config.vault_id == Some(manifest.vault_id) && config.active_vault == manifest.target)
}

fn random_failure() -> Failure {
    Failure::with("random_unavailable", json!({}))
}

pub(super) fn key_store_failure(error: key_store::StoreError) -> Failure {
    let code = match error {
        key_store::StoreError::Unsupported => "key_protector_unavailable",
        key_store::StoreError::ProtectorFailed => "key_protector_failed",
        key_store::StoreError::Exists => "file_exists",
        key_store::StoreError::Io => "io_error",
    };
    Failure::with(code, json!({}))
}

/// `vault_create_encrypted`: everything but the activation; answers with the spell, once.
pub fn begin(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    message: &Value,
) -> Result<Value, Failure> {
    require_management(session)?;
    if let Some(manifest) = read_manifest(session)? {
        if !completed(session, &manifest) {
            return Err(Failure::with(
                "vault_creation_pending",
                json!({ "path": manifest.target }),
            ));
        }
        let _ = fs::remove_file(manifest_path(session)?);
    }
    // Slice D: never beside an unfinished conversion (two spells at once could be confused).
    super::convert::refuse_while_pending(session)?;
    let default_path = session.env.default_vault_path()?;
    let target = match requested_path(message, "path")? {
        Some(path) => path,
        None if message.get("useDefault").and_then(Value::as_bool) == Some(true) => {
            if let Some(dir) = default_path.parent() {
                fs::create_dir_all(dir).map_err(|_| VaultError::Io)?;
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
    let reserved = [
        target.clone(),
        pending_db(&target),
        sidecar::path_for(&target),
        sidecar::pending_path_for(&target),
    ];
    if let Some(taken) = reserved.iter().find(|path| path.exists()) {
        return Err(Failure::with(
            "file_exists",
            json!({ "path": display(taken) }),
        ));
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        for base in [&target, &pending_db(&target)] {
            if fsutil::with_suffix(base, suffix).exists() {
                return Err(VaultError::SidecarExists.into());
            }
        }
    }
    let assessment = location::probe(&target, session.env.exe_dir.as_deref());
    let location_approved = check_location(&assessment, &target, approved(message))?;
    let app = app_dir(session)?;

    let _lock = ConfigLock::acquire(&session.env)?;
    let key = VaultKey::generate().map_err(|_| random_failure())?;
    let secret = RecoverySpell::generate().map_err(|_| random_failure())?;
    let vault_id = VaultId::generate().map_err(|_| random_failure())?;
    let mut manifest = Manifest {
        version: MANIFEST_VERSION,
        vault_id,
        target: display(&target),
        phase: Phase::Creating,
        cipher_salt: None,
        location_approved,
    };
    write_manifest(session, &manifest, true)?;
    let result = (|| {
        let salt = cipher::create(&pending_db(&target), &key).map_err(|error| match error {
            cipher::CreateError::Exists => Failure::from(VaultError::FileExists),
            cipher::CreateError::ParentMissing => VaultError::ParentMissing.into(),
            cipher::CreateError::Unavailable => VaultError::EncryptionUnsupported.into(),
            cipher::CreateError::Io => VaultError::Io.into(),
            cipher::CreateError::Db => VaultError::Db.into(),
        })?;
        manifest.cipher_salt = Some(salt);
        write_manifest(session, &manifest, false)?;
        key_store::store(&app, &vault_id, &salt, &key, key_store::Write::Pending)
            .map_err(key_store_failure)?;
        let envelope =
            recovery::wrap(&key, &secret, &vault_id, &salt).map_err(|_| random_failure())?;
        sidecar::write_new(
            &sidecar::pending_path_for(&target),
            &sidecar::Sidecar::new(vault_id, salt, envelope),
        )
        .map_err(|_| Failure::from(VaultError::Io))?;
        verify_pending(session, &manifest, &secret)?;
        manifest.phase = Phase::AwaitingConfirmation;
        write_manifest(session, &manifest, false)
    })();
    if let Err(failure) = result {
        remove_created(session, &manifest);
        return Err(failure);
    }
    // The only time the spell leaves the host. The response goes to the panel over native
    // messaging and nowhere else: no log, no file, no other message carries it.
    let shown: Zeroizing<String> = spell::encode(&secret);
    drop(secret);
    Ok(json!({
        "path": display(&target),
        "recoverySpell": shown.as_str(),
    }))
}

/// Every pending file opens to the same key: the DPAPI entry, the envelope with this spell, and
/// the vault itself.
fn verify_pending(
    session: &Session,
    manifest: &Manifest,
    secret: &RecoverySpell,
) -> Result<(), Failure> {
    let damaged = || Failure::with("vault_creation_damaged", json!({}));
    let salt = manifest.cipher_salt.ok_or_else(damaged)?;
    let target = manifest.target();
    let app = app_dir(session)?;
    // Each file under its pending name, or already under its final name if a commit was
    // interrupted between promotions (`promote_file` accepts both the same way).
    let pick = |pending: PathBuf, promoted: PathBuf| {
        if pending.exists() {
            pending
        } else {
            promoted
        }
    };
    let stored = key_store::unlock_pending(&app, &manifest.vault_id, &salt)
        .or_else(|_| key_store::unlock(&app, &manifest.vault_id, &salt))
        .map_err(|_| damaged())?;
    let sidecar_path = pick(
        sidecar::pending_path_for(&target),
        sidecar::path_for(&target),
    );
    let found = sidecar::read(&sidecar_path).map_err(|_| damaged())?;
    if found.vault_id != manifest.vault_id || found.cipher_salt != salt {
        return Err(damaged());
    }
    let unwrapped =
        recovery::unwrap(&found.recovery, secret, &manifest.vault_id, &salt).map_err(|error| {
            match error {
                recovery::UnwrapError::SpellRejected => {
                    Failure::with("recovery_spell_rejected", json!({}))
                }
                _ => damaged(),
            }
        })?;
    if !unwrapped.same_as(&stored) {
        return Err(damaged());
    }
    let db_path = pick(pending_db(&target), target);
    if cipher::read_head(&db_path).ok() != Some(cipher::Head::Salt(salt)) {
        return Err(damaged());
    }
    vault::open_checked_in(
        &db_path,
        VaultMode::Encrypted,
        Some(manifest.vault_id),
        &GivenKey(stored),
    )
    .map_err(|_| damaged())?;
    Ok(())
}

/// `vault_create_encrypted_commit`: the spell re-entered in full in the panel, then promotion and
/// the switch. `recoverySpell` must be the spell begin returned, all 43 characters.
pub fn commit(session: &mut Session, message: &Value) -> Result<Value, Failure> {
    require_management(session)?;
    let typed = Zeroizing::new(
        message
            .get("recoverySpell")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    );
    let secret = spell::parse(&typed).map_err(spell_failure)?;
    drop(typed);
    let lock = ConfigLock::acquire(&session.env)?;
    let mut manifest = match read_manifest(session)? {
        Some(manifest) if manifest.phase != Phase::Creating => manifest,
        _ => return Err(Failure::with("vault_creation_not_pending", json!({}))),
    };
    if completed(session, &manifest) {
        let _ = fs::remove_file(manifest_path(session)?);
        session.reset();
        return Ok(json!({ "path": manifest.target, "status": status(session) }));
    }
    verify_pending(session, &manifest, &secret)?;
    drop(secret);
    let salt = manifest.cipher_salt.expect("verified");
    let target = manifest.target();
    let app = app_dir(session)?;
    if manifest.phase == Phase::AwaitingConfirmation {
        let promote = || -> io::Result<()> {
            key_store::promote_pending(&app, &manifest.vault_id, &salt)?;
            promote_file(
                &sidecar::pending_path_for(&target),
                &sidecar::path_for(&target),
                |path| {
                    sidecar::read(path).is_ok_and(|found| {
                        found.vault_id == manifest.vault_id && found.cipher_salt == salt
                    })
                },
            )?;
            promote_file(&pending_db(&target), &target, |path| {
                cipher::read_head(path).ok() == Some(cipher::Head::Salt(salt))
            })
        };
        promote().map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => {
                Failure::with("file_exists", json!({ "path": display(&target) }))
            }
            _ => VaultError::Io.into(),
        })?;
        manifest.phase = Phase::Promoted;
        write_manifest(session, &manifest, false)?;
    }
    let keys = KeyStore::of(&session.env);
    vault::switch_active_to(
        &session.env,
        &lock,
        &NewActive::encrypted(&target, manifest.vault_id),
        manifest.location_approved,
        &keys,
        true,
    )
    .map_err(|error| {
        // The vault is complete but not active; a retried commit switches again.
        Failure::with(error.code(), json!({ "path": display(&target) }))
    })?;
    let _ = fs::remove_file(manifest_path(session)?);
    drop(lock);
    session.reset();
    Ok(json!({ "path": display(&target), "status": status(session) }))
}

/// Renames `from` to `to` without replacing; if `from` is gone and `to` is already this
/// creation's file (`ours`), that step was done before a crash.
pub(super) fn promote_file(from: &Path, to: &Path, ours: impl Fn(&Path) -> bool) -> io::Result<()> {
    match fsutil::rename_no_replace(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound && ours(to) => Ok(()),
        Err(error) => Err(error),
    }
}

/// `vault_create_encrypted_cancel`: removes what the recorded creation made.
pub fn cancel(session: &mut Session) -> Result<Value, Failure> {
    require_management(session)?;
    let _lock = ConfigLock::acquire(&session.env)?;
    match read_manifest(session) {
        Ok(Some(manifest)) => {
            if !completed(session, &manifest) {
                remove_created(session, &manifest);
            }
        }
        Ok(None) => {}
        // A manifest this version cannot read names nothing that could safely be removed: only the
        // manifest goes, so the panel is no longer stuck. Its `.pending` files stay; they can never
        // become active (their names and the missing manifest refuse every commit).
        Err(_) => {}
    }
    let _ = fs::remove_file(manifest_path(session)?);
    session.reset();
    Ok(json!({ "status": status(session) }))
}

/// Removes the files of an abandoned creation that provably belong to it, and the manifest. Never
/// the active vault.
fn remove_created(session: &Session, manifest: &Manifest) {
    let target = manifest.target();
    let active = |path: &Path| {
        matches!(vault::resolve(&session.env), Ok(Active::Configured(config))
            if Path::new(&config.active_vault) == path)
    };
    let salt = manifest.cipher_salt;
    let belongs = |path: &Path| match (cipher::read_head(path), salt) {
        (Ok(cipher::Head::Salt(found)), Some(salt)) => found == salt,
        // Created with `create_new` under a name this creation reserved, before its salt was known.
        (Ok(_), None) => path == pending_db(&target),
        _ => false,
    };
    for db in [pending_db(&target), target.clone()] {
        if !active(&db) && db.exists() && belongs(&db) {
            let _ = fs::remove_file(&db);
            let _ = fs::remove_file(fsutil::with_suffix(&db, "-journal"));
        }
    }
    for path in [
        sidecar::pending_path_for(&target),
        sidecar::path_for(&target),
    ] {
        // Any version: a creation begun before the Recovery Spell left a version 1 sidecar.
        if sidecar::identity(&path).is_some_and(|(vault_id, cipher_salt)| {
            vault_id == manifest.vault_id && salt.is_none_or(|salt| cipher_salt == salt)
        }) {
            let _ = fs::remove_file(&path);
        }
    }
    if let Some(app) = session.env.app_dir.as_deref() {
        key_store::remove_created(app, &manifest.vault_id, salt.as_ref());
    }
    if let Ok(path) = manifest_path(session) {
        let _ = fs::remove_file(path);
    }
}

/// A key already in hand (just unwrapped or just read), for one open.
pub(super) struct GivenKey(pub(super) VaultKey);

impl KeySource for GivenKey {
    fn key_for(&self, _: &KeyRequest) -> Result<VaultKey, KeyError> {
        Ok(self.0.duplicate())
    }
}

/// `recovery_spell_invalid`, with what is wrong (never the input itself).
pub(super) fn spell_failure(error: spell::SpellError) -> Failure {
    let details = match error {
        spell::SpellError::Length(count) => json!({ "reason": "length", "count": count }),
        spell::SpellError::Character(position) => {
            json!({ "reason": "character", "position": position })
        }
        spell::SpellError::NonCanonical => json!({ "reason": "non_canonical" }),
    };
    Failure::with("recovery_spell_invalid", details)
}

/// The spell of a request (`recoverySpell`), parsed strictly. Missing counts as length 0.
pub(super) fn requested_spell(message: &Value) -> Result<RecoverySpell, Failure> {
    let typed = Zeroizing::new(
        message
            .get("recoverySpell")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    );
    spell::parse(&typed).map_err(spell_failure)
}

/// The vault id of an encrypted file: its sidecar, else this user's key store (by salt). `None`:
/// nothing identifies the file as an encrypted Memocampus vault. The bool says whether a sidecar
/// offers recovery.
fn identify(
    path: &Path,
    salt: &CipherSalt,
    app: &Path,
) -> Result<Option<(VaultId, bool)>, Failure> {
    let with_path = |code: &'static str, recoverable: bool| {
        Failure::with(
            code,
            json!({ "path": display(path), "recoveryAvailable": recoverable }),
        )
    };
    match sidecar::read(&sidecar::path_for(path)) {
        Ok(found) if found.cipher_salt == *salt => Ok(Some((found.vault_id, true))),
        Ok(_) => Err(with_path("recovery_metadata_mismatch", false)),
        Err(sidecar::SidecarError::Missing) => {
            Ok(key_store::find_by_salt(app, salt).map(|id| (id, false)))
        }
        Err(sidecar::SidecarError::Invalid) => key_store::find_by_salt(app, salt)
            .map(|id| Some((id, false)))
            .ok_or_else(|| with_path("recovery_metadata_damaged", false)),
        // The 12-word sidecar: this user's DPAPI key still opens the vault; nothing else can.
        Err(sidecar::SidecarError::Unsupported) => key_store::find_by_salt(app, salt)
            .map(|id| Some((id, false)))
            .ok_or_else(|| with_path("recovery_metadata_unsupported", false)),
        Err(sidecar::SidecarError::Io) => Err(VaultError::Io.into()),
    }
}

/// `vault_open` of a file that is not plain SQLite: an encrypted vault, unlocked with this
/// user's key store. Returns `Ok(None)` for a file that is not encrypted-looking (the caller's
/// plain path then reports it).
pub fn open_existing(
    session: &mut Session,
    target: &Path,
    location_approved: bool,
) -> Result<Option<Value>, Failure> {
    let salt = match cipher::read_head(target) {
        Ok(cipher::Head::Salt(salt)) => salt,
        Ok(_) => return Ok(None),
        Err(_) => return Err(VaultError::Io.into()),
    };
    let app = app_dir(session)?;
    let Some((vault_id, recoverable)) = identify(target, &salt, &app)? else {
        return Ok(None);
    };
    let keys = KeyStore::of(&session.env);
    vault::open_checked_in(target, VaultMode::Encrypted, Some(vault_id), &keys).map_err(
        |failure| {
            Failure::with(
                failure.kind.code(),
                json!({
                    "path": display(target),
                    "recoveryAvailable": recoverable && failure.needs_handle_fence(),
                }),
            )
        },
    )?;
    let lock = ConfigLock::acquire(&session.env)?;
    vault::switch_active_to(
        &session.env,
        &lock,
        &NewActive::encrypted(target, vault_id),
        location_approved,
        &keys,
        false,
    )?;
    drop(lock);
    session.reset();
    Ok(Some(
        json!({ "path": display(target), "status": status(session) }),
    ))
}

/// `vault_recover`: unlocks an encrypted vault with its Recovery Spell, stores its key for this
/// Windows user (DPAPI) and makes it the active vault, publication pending.
pub fn recover(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    message: &Value,
) -> Result<Value, Failure> {
    require_management(session)?;
    let typed = Zeroizing::new(
        message
            .get("recoverySpell")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    );
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
    super::refuse_work_file(session, &target)?;
    let assessment = location::probe(&target, session.env.exe_dir.as_deref());
    let location_approved = check_location(&assessment, &target, approved(message))?;
    let with_path = |code: &'static str| Failure::with(code, json!({ "path": display(&target) }));
    let salt = match cipher::read_head(&target) {
        Ok(cipher::Head::Salt(salt)) => salt,
        Ok(cipher::Head::PlainSqlite) => return Err(with_path("vault_not_encrypted")),
        Ok(cipher::Head::TooShort) => return Err(with_path("vault_invalid")),
        Err(_) => return Err(VaultError::Io.into()),
    };
    let found = match sidecar::read(&sidecar::path_for(&target)) {
        Ok(found) if found.cipher_salt == salt => found,
        Ok(_) => return Err(with_path("recovery_metadata_mismatch")),
        Err(sidecar::SidecarError::Missing) => return Err(with_path("recovery_metadata_missing")),
        Err(sidecar::SidecarError::Invalid) => return Err(with_path("recovery_metadata_damaged")),
        Err(sidecar::SidecarError::Unsupported) => {
            return Err(with_path("recovery_metadata_unsupported"))
        }
        Err(sidecar::SidecarError::Io) => return Err(VaultError::Io.into()),
    };
    let secret = spell::parse(&typed).map_err(spell_failure)?;
    drop(typed);
    let key =
        recovery::unwrap(&found.recovery, &secret, &found.vault_id, &salt).map_err(|error| {
            match error {
                recovery::UnwrapError::SpellRejected => with_path("recovery_spell_rejected"),
                _ => with_path("recovery_metadata_damaged"),
            }
        })?;
    drop(secret);
    // Proven on the vault itself before anything is stored.
    vault::open_checked_in(
        &target,
        VaultMode::Encrypted,
        Some(found.vault_id),
        &GivenKey(key.duplicate()),
    )
    .map_err(|failure| with_path(failure.kind.code()))?;
    let app = app_dir(session)?;
    let write = match key_store::unlock(&app, &found.vault_id, &salt) {
        Ok(existing) if existing.same_as(&key) => None,
        Err(KeyError::Missing) => Some(key_store::Write::New),
        // An entry this user cannot use (another profile's, damaged, another key): replaced in one
        // rename, now that the spell proved the key.
        _ => Some(key_store::Write::Replace),
    };
    if let Some(write) = write {
        key_store::store(&app, &found.vault_id, &salt, &key, write).map_err(key_store_failure)?;
    }
    drop(key);
    let lock = ConfigLock::acquire(&session.env)?;
    vault::switch_active_to(
        &session.env,
        &lock,
        &NewActive::encrypted(&target, found.vault_id),
        location_approved,
        &KeyStore::of(&session.env),
        true,
    )?;
    drop(lock);
    session.reset();
    Ok(json!({ "path": display(&target), "status": status(session) }))
}

/// For the status: whether the encrypted vault at `path` has a sidecar matching its salt, so the
/// panel can offer recovery with the spell.
pub fn recovery_available(path: &Path) -> bool {
    matches!(
        (cipher::read_head(path), sidecar::read(&sidecar::path_for(path))),
        (Ok(cipher::Head::Salt(salt)), Ok(found)) if found.cipher_salt == salt
    )
}
