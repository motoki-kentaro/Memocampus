//! The recovery sidecar `<vault>.mckey` (PO decision P1): what an encrypted vault needs, besides
//! its file and the phrase, to be opened on another PC or by another Windows user.
//!
//! ```json
//! {"version":1,"format":"memocampus-vault-recovery","vaultId":"<32 hex>",
//!  "cipherSalt":"<32 hex>","recovery":{<recovery::Envelope>}}
//! ```
//!
//! Not secret: the key inside is wrapped under the phrase (128-bit level). It travels with the
//! vault file; one moved without the other is detected by the salt, and the envelope is bound to
//! the vault id and salt.

use super::fsutil;
use super::recovery::Envelope;
use super::secret::{CipherSalt, VaultId};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub const SUFFIX: &str = ".mckey";
pub const PENDING_SUFFIX: &str = ".mckey.pending";
const FORMAT: &str = "memocampus-vault-recovery";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Sidecar {
    pub version: u32,
    pub format: String,
    pub vault_id: VaultId,
    pub cipher_salt: CipherSalt,
    pub recovery: Envelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarError {
    Missing,
    /// Present but not a sidecar this version understands (damaged, truncated, other format).
    Invalid,
    Io,
}

impl Sidecar {
    pub fn new(vault_id: VaultId, cipher_salt: CipherSalt, recovery: Envelope) -> Self {
        Self {
            version: VERSION,
            format: FORMAT.into(),
            vault_id,
            cipher_salt,
            recovery,
        }
    }
}

pub fn path_for(vault: &Path) -> PathBuf {
    fsutil::with_suffix(vault, SUFFIX)
}

pub fn pending_path_for(vault: &Path) -> PathBuf {
    fsutil::with_suffix(vault, PENDING_SUFFIX)
}

pub fn read(path: &Path) -> Result<Sidecar, SidecarError> {
    let bytes = fsutil::read_bounded(path, MAX_BYTES).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => SidecarError::Missing,
        io::ErrorKind::InvalidData => SidecarError::Invalid,
        _ => SidecarError::Io,
    })?;
    let sidecar: Sidecar = serde_json::from_slice(&bytes).map_err(|_| SidecarError::Invalid)?;
    if sidecar.version != VERSION || sidecar.format != FORMAT {
        return Err(SidecarError::Invalid);
    }
    Ok(sidecar)
}

/// Writes a new sidecar at `path`, never replacing a file.
pub fn write_new(path: &Path, sidecar: &Sidecar) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(sidecar).expect("a sidecar serializes");
    fsutil::publish_new(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::super::recovery;
    use super::super::secret::{RecoverySeed, VaultKey};
    use super::*;

    #[test]
    fn a_sidecar_round_trips_and_anything_else_is_invalid() {
        let dir = std::env::temp_dir().join(format!("mc-sidecar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let vault = dir.join("v.sqlite3");
        let path = path_for(&vault);
        assert!(path.to_string_lossy().ends_with("v.sqlite3.mckey"));
        assert_eq!(read(&path).unwrap_err(), SidecarError::Missing);
        let (id, salt) = (
            VaultId::generate().unwrap(),
            CipherSalt::from_array([1; 16]),
        );
        let key = VaultKey::generate().unwrap();
        let envelope =
            recovery::wrap(&key, &RecoverySeed::generate().unwrap(), &id, &salt).unwrap();
        let sidecar = Sidecar::new(id, salt, envelope);
        write_new(&path, &sidecar).unwrap();
        assert_eq!(read(&path).unwrap(), sidecar);
        assert!(write_new(&path, &sidecar).is_err(), "never replaced");
        assert!(!key.appears_in(&std::fs::read(&path).unwrap()));
        let text = std::fs::read_to_string(&path).unwrap();
        for bad in [
            text.replace("memocampus-vault-recovery", "other"),
            text.replace("\"version\": 1", "\"version\": 2"),
            text[..text.len() / 2].to_string(),
            text.replacen('{', "{\"extra\":1,", 1),
        ] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(read(&path).unwrap_err(), SidecarError::Invalid);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
