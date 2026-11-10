//! The key store: `<app dir>\keys\<vaultId>.json`, one entry per encrypted vault this Windows user
//! can open without the spell. A cache for daily use (Slice E): never what a recovery depends on,
//! since the spell and the vault file give the key back (`recovery::vault_key`).
//!
//! ```json
//! {"version":2,"vaultId":"<32 hex>","cipherSalt":"<32 hex>","protector":"dpapi-user",
//!  "protected":"<base64 DPAPI blob>","kcv":"<64 hex>"}
//! ```
//!
//! Version 2 (Slice E): the vault id is `recovery::vault_id(cipherSalt)`, checked on every read.
//! Version 1 entries hold the random key of a vault of an earlier scheme: refused as `Legacy`, never
//! used, never removed or replaced by this build.
//!
//! An entry is keyed by the vault id, not by a path: a vault file that is moved or copied keeps its
//! key. It is accepted only for the vault id the caller expects and the salt the vault file
//! actually starts with, and the key DPAPI returns must match the key check value. Anything else is
//! refused; nothing here ever creates a key in place of a missing or unusable one.
//!
//! `<vaultId>.json.pending` is an entry of a vault still being created: never used to unlock.

use super::fsutil::{self, hex, unhex};
use super::protector::{self, ProtectorError};
use super::recovery;
use super::secret::{CipherSalt, VaultId, VaultKey};
use super::KeyError;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub const KEYS_DIR: &str = "keys";
pub const PENDING_SUFFIX: &str = ".pending";
const VERSION: u32 = 2;
const LEGACY_VERSION: u32 = 1;
const MAX_ENTRY_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Entry {
    version: u32,
    vault_id: VaultId,
    cipher_salt: CipherSalt,
    protector: String,
    protected: String,
    kcv: String,
}

pub fn keys_dir(app_dir: &Path) -> PathBuf {
    app_dir.join(KEYS_DIR)
}

pub fn entry_path(app_dir: &Path, vault_id: &VaultId) -> PathBuf {
    keys_dir(app_dir).join(format!("{}.json", vault_id.to_hex()))
}

pub fn pending_path(app_dir: &Path, vault_id: &VaultId) -> PathBuf {
    fsutil::with_suffix(&entry_path(app_dir, vault_id), PENDING_SUFFIX)
}

fn read_entry(path: &Path) -> Result<Entry, KeyError> {
    let bytes =
        fsutil::read_bounded(path, MAX_ENTRY_BYTES).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => KeyError::Missing,
            io::ErrorKind::InvalidData => KeyError::Mismatch,
            _ => KeyError::Io,
        })?;
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    match serde_json::from_slice::<Versioned>(&bytes) {
        Ok(found) if found.version == LEGACY_VERSION => return Err(KeyError::Legacy),
        Ok(found) if found.version == VERSION => {}
        _ => return Err(KeyError::Mismatch),
    }
    let entry: Entry = serde_json::from_slice(&bytes).map_err(|_| KeyError::Mismatch)?;
    if entry.protector != protector::NAME {
        return Err(KeyError::Mismatch);
    }
    Ok(entry)
}

fn open_entry(entry: &Entry, vault_id: &VaultId, salt: &CipherSalt) -> Result<VaultKey, KeyError> {
    if entry.vault_id != *vault_id
        || entry.cipher_salt != *salt
        || recovery::vault_id(salt) != *vault_id
    {
        return Err(KeyError::Mismatch);
    }
    let blob = base64::engine::general_purpose::STANDARD
        .decode(&entry.protected)
        .map_err(|_| KeyError::Mismatch)?;
    let kcv = unhex(&entry.kcv).ok_or(KeyError::Mismatch)?;
    let plain = protector::unprotect(&blob, vault_id).map_err(|error| match error {
        ProtectorError::Unsupported => KeyError::NotSupported,
        ProtectorError::Refused(_) => KeyError::ProtectorFailed,
    })?;
    let mut bytes: [u8; 32] = plain
        .as_slice()
        .try_into()
        .map_err(|_| KeyError::Mismatch)?;
    let key = VaultKey::from_bytes(&mut bytes);
    // DPAPI accepted the blob; only the check value proves it is this vault's key.
    if !recovery::key_check_matches(&key, vault_id, salt, &kcv) {
        return Err(KeyError::Mismatch);
    }
    Ok(key)
}

/// The key of vault `vault_id`, whose file starts with `salt`, for this Windows user.
pub fn unlock(app_dir: &Path, vault_id: &VaultId, salt: &CipherSalt) -> Result<VaultKey, KeyError> {
    open_entry(&read_entry(&entry_path(app_dir, vault_id))?, vault_id, salt)
}

/// The pending entry of a vault being created (see `unlock`).
pub fn unlock_pending(
    app_dir: &Path,
    vault_id: &VaultId,
    salt: &CipherSalt,
) -> Result<VaultKey, KeyError> {
    open_entry(
        &read_entry(&pending_path(app_dir, vault_id))?,
        vault_id,
        salt,
    )
}

/// True when an entry of an earlier scheme (version 1) names `salt`: the vault file with that salt
/// is a vault of an earlier, unsupported scheme. Reads only the version and the salt.
pub fn legacy_entry_for(app_dir: &Path, salt: &CipherSalt) -> bool {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Named {
        version: u32,
        cipher_salt: CipherSalt,
    }
    let Ok(items) = std::fs::read_dir(keys_dir(app_dir)) else {
        return false;
    };
    items.flatten().any(|item| {
        fsutil::read_bounded(&item.path(), MAX_ENTRY_BYTES)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Named>(&bytes).ok())
            .is_some_and(|named| named.version == LEGACY_VERSION && named.cipher_salt == *salt)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// No protector on this platform.
    Unsupported,
    /// DPAPI refused to protect, or the blob did not open again to the same key.
    ProtectorFailed,
    /// An entry already exists where a new one was to be created.
    Exists,
    Io,
}

/// How an entry is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// A pending entry for a vault being created (`<id>.json.pending`); never replaces one.
    Pending,
    /// The entry itself; never replaces one.
    New,
    /// The entry itself, replacing an existing one in one rename (after a recovery proved the key).
    Replace,
}

/// Protects `key` with DPAPI for this Windows user and stores it. The blob is opened again and
/// checked before anything is written.
pub fn store(
    app_dir: &Path,
    vault_id: &VaultId,
    salt: &CipherSalt,
    key: &VaultKey,
    write: Write,
) -> Result<PathBuf, StoreError> {
    let blob = protector::protect(key.expose(), vault_id).map_err(|error| match error {
        ProtectorError::Unsupported => StoreError::Unsupported,
        ProtectorError::Refused(_) => StoreError::ProtectorFailed,
    })?;
    let entry = Entry {
        version: VERSION,
        vault_id: *vault_id,
        cipher_salt: *salt,
        protector: protector::NAME.into(),
        protected: base64::engine::general_purpose::STANDARD.encode(&blob),
        kcv: hex(&recovery::key_check_value(key, vault_id, salt)),
    };
    match open_entry(&entry, vault_id, salt) {
        Ok(again) if again.same_as(key) => {}
        _ => return Err(StoreError::ProtectorFailed),
    }
    let bytes = Zeroizing::new(serde_json::to_vec_pretty(&entry).expect("an entry serializes"));
    std::fs::create_dir_all(keys_dir(app_dir)).map_err(|_| StoreError::Io)?;
    let path = match write {
        Write::Pending => pending_path(app_dir, vault_id),
        Write::New | Write::Replace => entry_path(app_dir, vault_id),
    };
    let written = match write {
        Write::Replace => fsutil::publish_replacing(&path, &bytes),
        _ => fsutil::publish_new(&path, &bytes),
    };
    written.map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => StoreError::Exists,
        _ => StoreError::Io,
    })?;
    Ok(path)
}

/// Makes a pending entry the entry of its vault, never replacing one. Idempotent: an entry that is
/// already in place for this vault and salt (a commit retried after a crash) is accepted.
pub fn promote_pending(app_dir: &Path, vault_id: &VaultId, salt: &CipherSalt) -> io::Result<()> {
    let (pending, entry) = (
        pending_path(app_dir, vault_id),
        entry_path(app_dir, vault_id),
    );
    match fsutil::rename_no_replace(&pending, &entry) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match read_entry(&entry) {
            Ok(found) if found.vault_id == *vault_id && found.cipher_salt == *salt => Ok(()),
            _ => Err(error),
        },
        Err(error) => Err(error),
    }
}

/// Removes the pending or the promoted entry of an abandoned creation, only if it names this vault
/// id and salt.
pub fn remove_created(app_dir: &Path, vault_id: &VaultId, salt: Option<&CipherSalt>) {
    for path in [
        pending_path(app_dir, vault_id),
        entry_path(app_dir, vault_id),
    ] {
        match read_entry(&path) {
            Ok(entry)
                if entry.vault_id == *vault_id && salt.is_none_or(|s| entry.cipher_salt == *s) =>
            {
                let _ = std::fs::remove_file(&path);
            }
            _ => {}
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("mc-keys-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry_json(dir: &Path, id: &VaultId) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(entry_path(dir, id)).unwrap()).unwrap()
    }

    fn rewrite(dir: &Path, id: &VaultId, value: &serde_json::Value) {
        std::fs::write(entry_path(dir, id), value.to_string()).unwrap();
    }

    #[test]
    fn an_entry_unlocks_only_its_vault_with_its_salt() {
        let dir = Dir::new();
        let salt = CipherSalt::from_array([3; 16]);
        let id = recovery::vault_id(&salt);
        let key = VaultKey::generate().unwrap();
        assert_eq!(unlock(&dir.0, &id, &salt).unwrap_err(), KeyError::Missing);
        store(&dir.0, &id, &salt, &key, Write::New).unwrap();
        assert!(unlock(&dir.0, &id, &salt).unwrap().same_as(&key));
        let bytes = std::fs::read(entry_path(&dir.0, &id)).unwrap();
        assert!(!key.appears_in(&bytes), "never in the clear");
        assert_eq!(
            unlock(&dir.0, &id, &CipherSalt::from_array([4; 16])).unwrap_err(),
            KeyError::Mismatch
        );
        // A vault id that is not the salt's is never stored or accepted.
        let foreign = VaultId::generate().unwrap();
        assert_eq!(
            store(&dir.0, &foreign, &salt, &key, Write::New).unwrap_err(),
            StoreError::ProtectorFailed
        );
        assert!(!entry_path(&dir.0, &foreign).exists());
        assert!(!legacy_entry_for(&dir.0, &salt));
        // Never replaced by a second creation.
        assert_eq!(
            store(
                &dir.0,
                &id,
                &salt,
                &VaultKey::generate().unwrap(),
                Write::New
            )
            .unwrap_err(),
            StoreError::Exists
        );
        assert!(unlock(&dir.0, &id, &salt).unwrap().same_as(&key));
    }

    #[test]
    fn a_swapped_altered_or_foreign_entry_is_refused() {
        let dir = Dir::new();
        let (salt, salt_b) = (
            CipherSalt::from_array([5; 16]),
            CipherSalt::from_array([0x15; 16]),
        );
        let (a, b) = (recovery::vault_id(&salt), recovery::vault_id(&salt_b));
        store(
            &dir.0,
            &a,
            &salt,
            &VaultKey::generate().unwrap(),
            Write::New,
        )
        .unwrap();
        store(
            &dir.0,
            &b,
            &salt_b,
            &VaultKey::generate().unwrap(),
            Write::New,
        )
        .unwrap();
        let original = entry_json(&dir.0, &a);

        // B's entry copied under A's name: the vault id inside disagrees.
        std::fs::copy(entry_path(&dir.0, &b), entry_path(&dir.0, &a)).unwrap();
        assert_eq!(unlock(&dir.0, &a, &salt).unwrap_err(), KeyError::Mismatch);
        // B's blob in A's entry: DPAPI refuses (the vault id is its entropy).
        let mut value = original.clone();
        value["protected"] = entry_json(&dir.0, &b)["protected"].clone();
        rewrite(&dir.0, &a, &value);
        assert_eq!(
            unlock(&dir.0, &a, &salt).unwrap_err(),
            KeyError::ProtectorFailed
        );
        // A blob DPAPI opens, but of another key for the same vault id: the check value refuses.
        let mut value = original.clone();
        let other = VaultKey::generate().unwrap();
        value["protected"] = base64::engine::general_purpose::STANDARD
            .encode(protector::protect(other.expose(), &a).unwrap())
            .into();
        rewrite(&dir.0, &a, &value);
        assert_eq!(unlock(&dir.0, &a, &salt).unwrap_err(), KeyError::Mismatch);
        // Damaged or unknown content.
        for (field, bad) in [
            ("kcv", serde_json::json!("00")),
            ("protected", serde_json::json!("%%%")),
            ("version", serde_json::json!(3)),
            ("vaultId", serde_json::json!(b.to_hex())),
            ("protector", serde_json::json!("plain")),
            ("cipherSalt", serde_json::json!("zz")),
        ] {
            let mut value = original.clone();
            value[field] = bad;
            rewrite(&dir.0, &a, &value);
            assert_eq!(
                unlock(&dir.0, &a, &salt).unwrap_err(),
                KeyError::Mismatch,
                "{field}"
            );
        }
        let mut value = original.clone();
        value["extra"] = 1.into();
        rewrite(&dir.0, &a, &value);
        assert_eq!(unlock(&dir.0, &a, &salt).unwrap_err(), KeyError::Mismatch);
        std::fs::write(entry_path(&dir.0, &a), b"{").unwrap();
        assert_eq!(unlock(&dir.0, &a, &salt).unwrap_err(), KeyError::Mismatch);
        // An entry of an earlier scheme (version 1): refused as legacy, recognised by its salt.
        let mut value = original.clone();
        value["version"] = 1.into();
        rewrite(&dir.0, &a, &value);
        assert_eq!(unlock(&dir.0, &a, &salt).unwrap_err(), KeyError::Legacy);
        assert!(legacy_entry_for(&dir.0, &salt));
        assert!(!legacy_entry_for(&dir.0, &salt_b));
        remove_created(&dir.0, &a, Some(&salt));
        assert!(entry_path(&dir.0, &a).exists(), "never removed");
        rewrite(&dir.0, &a, &original);
        assert!(unlock(&dir.0, &a, &salt).is_ok());
    }

    #[test]
    fn a_pending_entry_never_unlocks_until_promoted_and_promotion_never_replaces() {
        let dir = Dir::new();
        let salt = CipherSalt::from_array([6; 16]);
        let id = recovery::vault_id(&salt);
        let key = VaultKey::generate().unwrap();
        store(&dir.0, &id, &salt, &key, Write::Pending).unwrap();
        assert_eq!(unlock(&dir.0, &id, &salt).unwrap_err(), KeyError::Missing);
        assert!(unlock_pending(&dir.0, &id, &salt).unwrap().same_as(&key));
        promote_pending(&dir.0, &id, &salt).unwrap();
        assert!(unlock(&dir.0, &id, &salt).unwrap().same_as(&key));
        promote_pending(&dir.0, &id, &salt).unwrap();
        // Another pending entry cannot replace the promoted one.
        store(
            &dir.0,
            &id,
            &salt,
            &VaultKey::generate().unwrap(),
            Write::Pending,
        )
        .unwrap();
        assert!(promote_pending(&dir.0, &id, &salt).is_err());
        assert!(unlock(&dir.0, &id, &salt).unwrap().same_as(&key));
        remove_created(&dir.0, &id, Some(&CipherSalt::from_array([7; 16])));
        assert!(entry_path(&dir.0, &id).exists(), "another salt: kept");
        remove_created(&dir.0, &id, Some(&salt));
        assert!(!entry_path(&dir.0, &id).exists() && !pending_path(&dir.0, &id).exists());
    }
}
