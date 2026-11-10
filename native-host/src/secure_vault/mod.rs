//! Secure Vault (Issue #51, Slice B): what the host, VisibleStore and (through VisibleStore) the
//! MCP server need to use an encrypted vault, and nothing else.
//!
//! - `secret`: the vault key (VK), the Recovery Spell secret and the non-secret identifiers. Key
//!   material has no `Debug`/`Display`/`Serialize` output and is zeroed when dropped.
//! - `spell`: the Recovery Spell (ふっかつのじゅもん): 256 CSPRNG bits written as 43 base64url
//!   characters, parsed strictly (length, alphabet, canonical form).
//! - `recovery`: the spell envelope (HKDF-SHA256 + XChaCha20-Poly1305), with a spell check value
//!   that tells a wrong spell from damaged metadata.
//! - `protector`: Windows DPAPI, user scope. Isolated: other platforms report `Unsupported`.
//! - `key_store`: `<app dir>\keys\<vaultId>.json`, the DPAPI-protected VK of each vault this
//!   Windows user can open without the spell.
//! - `sidecar`: `<vault>.mckey`, the spell envelope that travels with the vault file.
//! - `cipher`: keyed SQLCipher connections. The key is handed to SQLCipher through its C API, never
//!   through SQL text.
//! - `container` (Slice C): the portable backup formats B, C and D (format A is plain SQLite).
//! - `convert` (Slice D): copying a vault between a plain and an encrypted database (the source
//!   attached read-only, one transaction, compared row by row).
//!
//! Nothing here logs, and no error value carries key material, a spell or a path.

pub mod cipher;
pub mod container;
pub mod convert;
pub mod fsutil;
pub mod key_store;
pub mod protector;
pub mod recovery;
pub mod secret;
pub mod sidecar;
pub mod spell;

/// Why the key of an encrypted vault is not available. Never carries key material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// This build cannot open encrypted vaults (no SQLCipher, or no key protector on this OS).
    NotSupported,
    /// No key store entry for the vault.
    Missing,
    /// The OS protector (DPAPI) refused the entry: another Windows user, a damaged profile or blob.
    ProtectorFailed,
    /// An entry exists but does not belong to this vault (vault id, salt or key check value differ),
    /// or is damaged.
    Mismatch,
    /// The key needs the user (the Recovery Spell) before it can be used.
    Locked,
    /// The key store could not be read now (sharing, permissions, I/O).
    Io,
}
