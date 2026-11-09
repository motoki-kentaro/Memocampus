//! The active vault as `config.json` names it, its mode, and whether it is published to MCP
//! (Issue #51, Slice B0).
//!
//! The Native Host is the only writer of `config.json` (see the host's `vault` module). This module
//! holds what both sides need: the config format, the identity of a vault file (`vault_token`), and
//! the gate a reader passes before it may read a vault for MCP (`admit`).
//!
//! Rules:
//! - The mode of a vault comes from the config, never from sniffing the file.
//! - A vault is published only while the config names it as active **and** records a confirmed
//!   publication for exactly its file identity. A replaced or copied file has a new identity and is
//!   therefore unpublished; so is every vault a version 1 config names (the host migrates those).
//! - Anything that cannot be read or understood refuses; nothing is guessed.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "config.json";
/// Written by this version.
pub const CONFIG_VERSION: u32 = 2;
/// Written by hosts before Issue #51: `{version, activeVault, locationApproved}`. Still read.
pub const LEGACY_CONFIG_VERSION: u32 = 1;
/// A config is a few hundred bytes; anything far larger is not one.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// How the vault file is stored. Only `Plain` can be opened by this version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VaultMode {
    Plain,
    Encrypted,
}

impl VaultMode {
    pub fn code(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Encrypted => "encrypted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublicationState {
    /// Not (or no longer) approved for MCP: MCP reads nothing from it.
    Pending,
    /// The user approved MCP access to this vault file.
    Confirmed,
}

/// The publication decision for one vault file, named by its `vault_token`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Publication {
    pub vault_token: String,
    pub state: PublicationState,
    /// The policy revision shown when the user confirmed (informational; the current policy is
    /// applied by every read regardless).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub active_vault: String,
    /// The user explicitly accepted location warnings (sync folder, removable drive, ...).
    #[serde(default)]
    pub location_approved: bool,
    pub mode: VaultMode,
    /// None: nothing was ever decided for the active vault, which is therefore unpublished.
    #[serde(default)]
    pub publication: Option<Publication>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyConfig {
    active_vault: String,
    #[serde(default)]
    location_approved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// Present but unreadable (permissions, sharing, I/O).
    Io,
    /// Not a config this version understands: malformed, truncated, unknown version, unknown
    /// field or mode, relative path, malformed token.
    Invalid,
}

impl Config {
    /// A version 2 config naming `active_vault`, with no publication decision yet.
    pub fn new(active_vault: String, location_approved: bool, mode: VaultMode) -> Self {
        Self {
            version: CONFIG_VERSION,
            active_vault,
            location_approved,
            mode,
            publication: None,
        }
    }

    /// Read from a version 1 file: the host has not migrated it yet.
    pub fn is_legacy(&self) -> bool {
        self.version == LEGACY_CONFIG_VERSION
    }

    /// True only for a confirmed publication of exactly this vault file.
    pub fn is_published(&self, token: &str) -> bool {
        !self.is_legacy()
            && matches!(&self.publication, Some(publication)
                if publication.state == PublicationState::Confirmed && publication.vault_token == token)
    }

    /// The publication state of the vault file `token`: anything not confirmed for it is pending.
    pub fn publication_of(&self, token: &str) -> PublicationState {
        if self.is_published(token) {
            PublicationState::Confirmed
        } else {
            PublicationState::Pending
        }
    }

    /// Parses `config.json`. Version 1 is read as a plain vault with no publication decision.
    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        let value: Value = serde_json::from_slice(bytes).map_err(|_| ConfigError::Invalid)?;
        let config = match value.get("version").and_then(Value::as_u64) {
            Some(version) if version == u64::from(LEGACY_CONFIG_VERSION) => {
                let legacy: LegacyConfig =
                    serde_json::from_value(value).map_err(|_| ConfigError::Invalid)?;
                Self {
                    version: LEGACY_CONFIG_VERSION,
                    active_vault: legacy.active_vault,
                    location_approved: legacy.location_approved,
                    mode: VaultMode::Plain,
                    publication: None,
                }
            }
            Some(version) if version == u64::from(CONFIG_VERSION) => {
                serde_json::from_value(value).map_err(|_| ConfigError::Invalid)?
            }
            _ => return Err(ConfigError::Invalid),
        };
        let token_ok = config
            .publication
            .as_ref()
            .is_none_or(|publication| is_token(&publication.vault_token));
        if !Path::new(&config.active_vault).is_absolute() || !token_ok {
            return Err(ConfigError::Invalid);
        }
        Ok(config)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("a config always serializes")
    }
}

/// `config.json` of `app_dir`; `None` if there is none.
pub fn read_config(app_dir: &Path) -> Result<Option<Config>, ConfigError> {
    let file = match File::open(app_dir.join(CONFIG_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConfigError::Io),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigError::Io)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(ConfigError::Invalid);
    }
    Config::parse(&bytes).map(Some)
}

/// 64 lower-case hexadecimal digits, as `vault_token` writes them.
pub fn is_token(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Identifies a vault file independently of how its path is spelled (case, `\\?\` prefix,
/// junctions): SHA-256 over the volume serial number and file index on Windows. Only the hash
/// leaves the host, never the path. Replacing the file (e.g. restoring a backup) yields a new token.
pub fn vault_token(path: &Path) -> io::Result<String> {
    vault_token_of(&File::open(path)?)
}

/// `vault_token` of a file already open.
pub fn vault_token_of(file: &File) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    Ok(Sha256::digest(file_identity(file)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<Vec<u8>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    // SAFETY: the handle is valid while `file` lives and the struct is plain data.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut identity = b"memocampus-vault-identity-v1\0".to_vec();
    for part in [
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ] {
        identity.extend_from_slice(&part.to_le_bytes());
    }
    Ok(identity)
}

#[cfg(not(windows))]
fn file_identity(file: &File) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    let mut identity = b"memocampus-vault-identity-v1\0".to_vec();
    identity.extend_from_slice(&metadata.dev().to_le_bytes());
    identity.extend_from_slice(&metadata.ino().to_le_bytes());
    Ok(identity)
}

/// Why a reader may not read a vault for MCP. Fixed codes only, never a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    NotConfigured,
    ConfigInvalid,
    /// `config.json` or the vault file exists but cannot be read now.
    Unavailable,
    /// The active vault file does not exist.
    VaultMissing,
    /// A fixed-path reader's file is not the active vault.
    NotActive,
    /// The active vault has no confirmed publication for its current file identity.
    PublicationPending,
    /// The active vault, its mode or its publication changed while a read was under way.
    Switched,
}

impl GateError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "vault_not_configured",
            Self::ConfigInvalid => "config_invalid",
            Self::Unavailable | Self::VaultMissing => "vault_unavailable",
            Self::NotActive => "vault_not_active",
            Self::PublicationPending => "vault_publication_pending",
            Self::Switched => "vault_switched",
        }
    }
}

/// A vault the gate let through: the active vault, confirmed for this file identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    pub path: PathBuf,
    pub mode: VaultMode,
    pub token: String,
}

/// The publication gate for readers. `pinned` is the path a fixed-path reader was started with:
/// it is accepted only while it is the active vault file itself (same identity, however spelled).
///
/// Checked in this order, each failure refusing: a readable version 2 config, the active vault
/// file's identity, the pinned file being that file, and a confirmed publication for that
/// identity. A version 1 config is never published: the host migrates it first.
pub fn admit(app_dir: &Path, pinned: Option<&Path>) -> Result<Admitted, GateError> {
    let config = match read_config(app_dir) {
        Ok(Some(config)) => config,
        Ok(None) => return Err(GateError::NotConfigured),
        Err(ConfigError::Invalid) => return Err(GateError::ConfigInvalid),
        Err(ConfigError::Io) => return Err(GateError::Unavailable),
    };
    if config.is_legacy() || config.publication.is_none() {
        return Err(GateError::PublicationPending);
    }
    let path = PathBuf::from(&config.active_vault);
    let token = match vault_token(&path) {
        Ok(token) => token,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(GateError::VaultMissing)
        }
        Err(_) => return Err(GateError::Unavailable),
    };
    if let Some(pinned) = pinned {
        match vault_token(pinned) {
            Ok(pinned_token) if pinned_token == token => {}
            Ok(_) => return Err(GateError::NotActive),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(GateError::NotActive)
            }
            Err(_) => return Err(GateError::Unavailable),
        }
    }
    if !config.is_published(&token) {
        return Err(GateError::PublicationPending);
    }
    Ok(Admitted {
        path,
        mode: config.mode,
        token,
    })
}

/// After a read: the gate still admits exactly the same vault, mode and identity. Anything else
/// (switched, pending again, replaced, unreadable) discards the read as `Switched`.
pub fn still_admitted(
    app_dir: &Path,
    pinned: Option<&Path>,
    admitted: &Admitted,
) -> Result<(), GateError> {
    match admit(app_dir, pinned) {
        Ok(now) if now == *admitted => Ok(()),
        _ => Err(GateError::Switched),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("memocampus-active-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, text: &str) {
            fs::write(self.0.join(CONFIG_FILE), text).unwrap();
        }

        fn vault(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, name).unwrap();
            path
        }

        fn config(&self, active: &Path, publication: Option<(&str, &str)>) {
            let mut value = serde_json::json!({
                "version": 2, "activeVault": active, "mode": "plain",
            });
            if let Some((token, state)) = publication {
                value["publication"] = serde_json::json!({"vaultToken": token, "state": state});
            }
            self.write(&value.to_string());
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn absolute() -> &'static str {
        if cfg!(windows) {
            r"C:\\v\\vault.sqlite3"
        } else {
            "/v/vault.sqlite3"
        }
    }

    #[test]
    fn version_1_is_read_as_plain_and_unpublished() {
        let config = Config::parse(
            format!(
                r#"{{"version":1,"activeVault":"{}","locationApproved":true}}"#,
                absolute()
            )
            .as_bytes(),
        )
        .unwrap();
        assert!(config.is_legacy());
        assert_eq!(config.mode, VaultMode::Plain);
        assert!(config.location_approved);
        assert_eq!(config.publication, None);
        assert!(!config.is_published(&"0".repeat(64)));
    }

    #[test]
    fn anything_not_understood_is_invalid_never_guessed() {
        let path = absolute();
        let token = "a".repeat(64);
        for text in [
            String::new(),
            "{".into(),
            format!(r#"{{"version":2,"activeVault":"{path}","mode":"plain""#), // truncated
            format!(r#"{{"version":3,"activeVault":"{path}","mode":"plain"}}"#),
            format!(r#"{{"version":"2","activeVault":"{path}","mode":"plain"}}"#),
            format!(r#"{{"activeVault":"{path}","mode":"plain"}}"#),
            format!(r#"{{"version":2,"activeVault":"{path}"}}"#), // no mode
            format!(r#"{{"version":2,"activeVault":"{path}","mode":"sqlcipher"}}"#),
            format!(r#"{{"version":2,"activeVault":"{path}","mode":"Plain"}}"#),
            r#"{"version":2,"activeVault":"relative.sqlite3","mode":"plain"}"#.to_string(),
            format!(r#"{{"version":2,"activeVault":"{path}","mode":"plain","extra":1}}"#),
            format!(
                r#"{{"version":2,"activeVault":"{path}","mode":"plain","publication":{{"vaultToken":"{}","state":"confirmed"}}}}"#,
                "A".repeat(64)
            ),
            format!(
                r#"{{"version":2,"activeVault":"{path}","mode":"plain","publication":{{"vaultToken":"{token}","state":"approved"}}}}"#
            ),
            format!(
                r#"{{"version":2,"activeVault":"{path}","mode":"plain","publication":{{"vaultToken":"{token}"}}}}"#
            ),
            r#"{"version":1,"activeVault":"relative"}"#.to_string(),
        ] {
            assert_eq!(
                Config::parse(text.as_bytes()),
                Err(ConfigError::Invalid),
                "{text}"
            );
        }
    }

    #[test]
    fn a_publication_belongs_to_one_token_and_only_confirmed_publishes() {
        let token = "b".repeat(64);
        let mut config = Config::new(absolute().replace("\\\\", "\\"), false, VaultMode::Plain);
        assert_eq!(config.publication_of(&token), PublicationState::Pending);
        config.publication = Some(Publication {
            vault_token: token.clone(),
            state: PublicationState::Pending,
            policy_revision: None,
        });
        assert!(!config.is_published(&token));
        config.publication.as_mut().unwrap().state = PublicationState::Confirmed;
        assert!(config.is_published(&token));
        assert!(!config.is_published(&"c".repeat(64)), "another vault's");
        let round = Config::parse(&config.to_bytes()).unwrap();
        assert_eq!(round, config);
    }

    #[test]
    fn the_gate_admits_only_the_confirmed_active_vault_file() {
        let dir = Dir::new();
        let app = dir.0.clone();
        assert_eq!(admit(&app, None), Err(GateError::NotConfigured));
        dir.write("{not json");
        assert_eq!(admit(&app, None), Err(GateError::ConfigInvalid));

        let a = dir.vault("a.sqlite3");
        let b = dir.vault("b.sqlite3");
        let token_a = vault_token(&a).unwrap();
        let token_b = vault_token(&b).unwrap();
        assert_ne!(token_a, token_b);

        // Version 1: never published, whatever it names.
        dir.write(&serde_json::json!({"version": 1, "activeVault": a}).to_string());
        assert_eq!(admit(&app, None), Err(GateError::PublicationPending));

        dir.config(&a, None);
        assert_eq!(admit(&app, None), Err(GateError::PublicationPending));
        dir.config(&a, Some((&token_a, "pending")));
        assert_eq!(admit(&app, None), Err(GateError::PublicationPending));
        dir.config(&a, Some((&token_b, "confirmed")));
        assert_eq!(
            admit(&app, None),
            Err(GateError::PublicationPending),
            "B's approval never publishes A"
        );
        dir.config(&a, Some((&token_a, "confirmed")));
        let admitted = admit(&app, None).unwrap();
        assert_eq!(admitted.token, token_a);
        assert_eq!(admitted.mode, VaultMode::Plain);

        // Fixed path: only the active file itself, however spelled.
        assert_eq!(admit(&app, Some(&a)).unwrap(), admitted);
        assert_eq!(admit(&app, Some(&b)), Err(GateError::NotActive));
        assert_eq!(
            admit(&app, Some(&dir.0.join("none.sqlite3"))),
            Err(GateError::NotActive)
        );
        if cfg!(windows) {
            let upper = PathBuf::from(a.to_string_lossy().to_uppercase());
            assert_eq!(admit(&app, Some(&upper)).unwrap(), admitted);
        }

        // A replaced file is another vault: its approval is gone.
        fs::remove_file(&a).unwrap();
        assert_eq!(admit(&app, None), Err(GateError::VaultMissing));
        fs::write(&a, "a.sqlite3").unwrap();
        assert_eq!(admit(&app, None), Err(GateError::PublicationPending));
    }

    #[test]
    fn a_change_during_a_read_is_a_switch() {
        let dir = Dir::new();
        let app = dir.0.clone();
        let a = dir.vault("a.sqlite3");
        let b = dir.vault("b.sqlite3");
        let (token_a, token_b) = (vault_token(&a).unwrap(), vault_token(&b).unwrap());
        dir.config(&a, Some((&token_a, "confirmed")));
        let admitted = admit(&app, None).unwrap();
        assert_eq!(still_admitted(&app, None, &admitted), Ok(()));
        dir.config(&b, Some((&token_b, "confirmed")));
        assert_eq!(
            still_admitted(&app, None, &admitted),
            Err(GateError::Switched)
        );
        dir.config(&a, Some((&token_a, "pending")));
        assert_eq!(
            still_admitted(&app, None, &admitted),
            Err(GateError::Switched)
        );
        dir.write("");
        assert_eq!(
            still_admitted(&app, None, &admitted),
            Err(GateError::Switched)
        );
    }
}
