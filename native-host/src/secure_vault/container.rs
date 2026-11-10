//! Portable backup containers (Issue #51, Slice C): formats B, C and D. Format A is a plain SQLite
//! file and has no container.
//!
//! # Common prefix
//!
//! ```text
//! magic 8 = 89 'M' 'C' 'V' 'B' 0D 0A 1A | version u16 LE = 2 | kind u8 ('B'|'C'|'D') | flags u8 = 0
//! | header_len u32 LE (1..=65536) | header (strict JSON, deny_unknown_fields, non-secret values only)
//! ```
//!
//! Version 2 (Recovery Spell) is what this build writes. Version 1 was written by Slice C with the
//! 12-word recovery phrase: a version 1 B or D is refused as `LegacyRecoveryPhrase` (never opened
//! as a spell envelope); a version 1 C, whose layout and keys do not involve the phrase at all and
//! are unchanged in version 2, is still read. Any other version is `UnsupportedVersion`.
//!
//! Every length is checked before anything is allocated or derived, and the file length must be
//! exactly what the header announces: a truncated file and a file with bytes appended are refused
//! before any key is derived. Nothing in a container names a path.
//!
//! # B — encrypted vault, normal backup
//!
//! ```text
//! prefix | payload (payloadLen: a SQLCipher 4 database, ciphertext) | policy copy (sealed, optional)
//!        | HMAC-SHA256(K_mac, prefix || payload || policy copy)
//! K_mac    = HKDF-SHA256(VK, info = "memocampus/backup-b/v2/mac")
//! K_policy = HKDF-SHA256(VK, info = "memocampus/backup-b/v2/policy"),
//!            XChaCha20-Poly1305, AAD = "memocampus/backup-b/v2/policy\0" || vaultId
//! ```
//!
//! The header carries the vault id, the payload's SQLCipher salt and the recovery envelope (the
//! vault key wrapped under keys derived from the Recovery Spell, `recovery`, scheme
//! `recovery::SCHEME`), bound to the salt `recoverySalt` it was made for. Restoring needs the spell
//! only: no key store, no DPAPI. The spell opens the envelope; the MAC and policy keys come from the
//! VK it yields, each through its own HKDF label, so the spell's bytes are never a key themselves.
//!
//! # C and D — password-protected backups
//!
//! ```text
//! prefix | STREAM-BE32(XChaCha20-Poly1305, K_enc, nonce prefix 19 bytes, AAD = SHA-256(prefix))
//!          over the concatenated sections, in 64 KiB chunks, the last one flagged
//! (K_enc, K_check) = HKDF-SHA256(prk = Argon2id(password, salt 32, m, t, p))
//! check  = HMAC-SHA256(K_check, "memocampus/backup-p/v1/password-check\0" || kind)
//! ```
//!
//! - C (plain vault): sections `meta` (JSON), `policy` (optional), `vault-sqlite3` (the SQLite file).
//! - D (encrypted vault): one section `package-b`, a whole format B file. The password yields only
//!   that package, which still needs the spell; the spell cannot open the outer layer.
//!
//! The chunk counter and the last-chunk flag are in the nonce, and the total plaintext length is
//! fixed by the (authenticated) header: a removed, reordered, duplicated or truncated chunk fails.
//! The check value tells a wrong password from damage; it gives no shortcut, because every guess
//! still costs one Argon2id run, exactly as trying to decrypt the first chunk would.

use super::fsutil::{hex, unhex};
use super::recovery::{self, Envelope, UnwrapError};
use super::secret::{random_bytes, CipherSalt, RandomError, RecoverySpell, VaultId, VaultKey};
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use zeroize::{Zeroize, Zeroizing};

pub const MAGIC: [u8; 8] = [0x89, b'M', b'C', b'V', b'B', 0x0D, 0x0A, 0x1A];
pub const VERSION: u16 = 2;
/// Slice C's containers (12-word recovery phrase). See the module documentation.
pub const LEGACY_VERSION: u16 = 1;
pub const FIXED_LEN: usize = 16;
pub const MAX_HEADER: u32 = 64 * 1024;
pub const CHUNK: usize = 64 * 1024;
const TAG: usize = 16;
const MAC_LEN: u64 = 32;
/// The largest policy copy a backup carries (the policy file itself is at most 1 MiB).
pub const MAX_POLICY_COPY: u64 = 4 * 1024 * 1024;
/// The largest `meta` section.
pub const MAX_META: u64 = 64 * 1024;

const B_PAYLOAD: &str = "sqlcipher4";
const MAC_INFO: &[u8] = b"memocampus/backup-b/v2/mac";
const POLICY_INFO: &[u8] = b"memocampus/backup-b/v2/policy";
const POLICY_AAD: &[u8] = b"memocampus/backup-b/v2/policy\0";
const ENC_INFO: &[u8] = b"memocampus/backup-p/v1/enc";
const CHECK_INFO: &[u8] = b"memocampus/backup-p/v1/check";
const CHECK_MSG: &[u8] = b"memocampus/backup-p/v1/password-check\0";
pub const AEAD: &str = "xchacha20poly1305-stream-be32";

pub const SECTION_META: &str = "meta";
pub const SECTION_POLICY: &str = "policy";
pub const SECTION_VAULT: &str = "vault-sqlite3";
pub const SECTION_PACKAGE: &str = "package-b";

/// Argon2id bounds a restore accepts, checked before Argon2 allocates anything. A crafted backup
/// can make a restore cost at most 256 MiB and 10 passes.
pub const ARGON_M_KIB_MIN: u32 = 19 * 1024;
pub const ARGON_M_KIB_MAX: u32 = 256 * 1024;
pub const ARGON_T_MAX: u32 = 10;
pub const ARGON_P_MAX: u32 = 4;
/// What new backups use (OWASP's first Argon2id recommendation is 19 MiB, t = 2; this is more).
pub const ARGON_M_KIB_DEFAULT: u32 = 64 * 1024;
pub const ARGON_T_DEFAULT: u32 = 3;
pub const ARGON_P_DEFAULT: u32 = 1;
/// Password length accepted for a new backup, in characters (bytes are bounded too).
pub const PASSWORD_MIN_CHARS: usize = 8;
pub const PASSWORD_MAX_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Encrypted vault, normal backup.
    B,
    /// Plain vault, password-protected.
    C,
    /// Encrypted vault, password-protected (outer layer around a format B package).
    D,
}

impl Kind {
    fn byte(self) -> u8 {
        match self {
            Self::B => b'B',
            Self::C => b'C',
            Self::D => b'D',
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'B' => Some(Self::B),
            b'C' => Some(Self::C),
            b'D' => Some(Self::D),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
        }
    }

    pub fn needs_password(self) -> bool {
        matches!(self, Self::C | Self::D)
    }

    pub fn needs_spell(self) -> bool {
        matches!(self, Self::B | Self::D)
    }
}

/// Why a container was refused. No value carries key material, a password, a spell or a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// Not a Memocampus backup container (or an unknown kind).
    UnsupportedFormat,
    /// A container of another version, scheme or KDF.
    UnsupportedVersion,
    /// A B or D container of Slice C, protected by the 12-word recovery phrase this build no
    /// longer reads.
    LegacyRecoveryPhrase,
    /// A length, count, parameter or field out of bounds or of the wrong shape.
    Malformed,
    /// The file is shorter or longer than its header says.
    Incomplete,
    /// The vault inside has another schema version than this build uses.
    SchemaMismatch,
    /// The spell is not the one of this backup's envelope.
    SpellRejected,
    /// The spell matches but the envelope does not authenticate: damaged metadata.
    RecoveryDamaged,
    /// The password check value does not match.
    PasswordRejected,
    /// Authentication failed after the key was accepted: altered or damaged content.
    Tampered,
    /// The backup file could not be read.
    Io,
    /// Writing the restored data failed (the kind tells e.g. a full disk).
    Output(io::ErrorKind),
}

/// Marks a STREAM authentication failure inside an `io::Error` (see `PasswordReader`).
#[derive(Debug)]
struct Corrupt;

impl std::fmt::Display for Corrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("authentication failed")
    }
}

impl std::error::Error for Corrupt {}

fn corrupt() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, Corrupt)
}

/// A read error from a container: authentication failure, end of file, or I/O.
fn classify(error: &io::Error) -> FormatError {
    if error.get_ref().is_some_and(|inner| inner.is::<Corrupt>()) {
        FormatError::Tampered
    } else if error.kind() == io::ErrorKind::UnexpectedEof {
        FormatError::Incomplete
    } else {
        FormatError::Io
    }
}

/// What the first bytes of a file say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detected {
    /// `SQLite format 3\0`: format A (still to be validated in full).
    PlainSqlite,
    Container(Kind),
    /// The container magic with another version.
    UnsupportedVersion,
    /// A version 1 B or D (12-word recovery phrase).
    LegacyRecoveryPhrase,
    /// Anything else, including a bare SQLCipher file and a container of an unknown kind.
    Unknown,
}

pub fn detect(head: &[u8]) -> Detected {
    if head.starts_with(b"SQLite format 3\0") {
        return Detected::PlainSqlite;
    }
    if head.len() < FIXED_LEN || head[..8] != MAGIC {
        return Detected::Unknown;
    }
    let version = u16::from_le_bytes([head[8], head[9]]);
    if version != VERSION && version != LEGACY_VERSION {
        return Detected::UnsupportedVersion;
    }
    match Kind::from_byte(head[10]) {
        Some(Kind::C) if head[11] == 0 => Detected::Container(Kind::C),
        Some(_) if head[11] == 0 && version == LEGACY_VERSION => Detected::LegacyRecoveryPhrase,
        Some(kind) if head[11] == 0 => Detected::Container(kind),
        _ => Detected::Unknown,
    }
}

#[cfg(test)]
thread_local! {
    /// Tests only: the version a container is written with, to produce a Slice C (version 1)
    /// format C file byte for byte.
    static WRITE_VERSION: std::cell::Cell<u16> = const { std::cell::Cell::new(VERSION) };
}

fn write_version() -> u16 {
    #[cfg(test)]
    return WRITE_VERSION.with(std::cell::Cell::get);
    #[cfg(not(test))]
    VERSION
}

fn make_prefix(kind: Kind, header: &[u8]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&write_version().to_le_bytes());
    out.push(kind.byte());
    out.push(0);
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(header);
    out
}

/// The prefix of a container: its kind, header bytes and all prefix bytes (for MAC / AAD).
pub struct Prefix {
    pub kind: Kind,
    pub header: Vec<u8>,
    pub bytes: Vec<u8>,
}

/// Reads and bounds-checks the prefix. The header length is checked before it is allocated.
pub fn read_prefix(r: &mut impl Read) -> Result<Prefix, FormatError> {
    let mut fixed = [0u8; FIXED_LEN];
    r.read_exact(&mut fixed).map_err(|e| classify(&e))?;
    let kind = match detect(&fixed) {
        Detected::Container(kind) => kind,
        Detected::UnsupportedVersion => return Err(FormatError::UnsupportedVersion),
        Detected::LegacyRecoveryPhrase => return Err(FormatError::LegacyRecoveryPhrase),
        _ => return Err(FormatError::UnsupportedFormat),
    };
    let len = u32::from_le_bytes(fixed[12..16].try_into().expect("4 bytes"));
    if len == 0 || len > MAX_HEADER {
        return Err(FormatError::Malformed);
    }
    let mut header = vec![0u8; len as usize];
    r.read_exact(&mut header).map_err(|e| classify(&e))?;
    let mut bytes = fixed.to_vec();
    bytes.extend_from_slice(&header);
    Ok(Prefix {
        kind,
        header,
        bytes,
    })
}

fn hkdf32(ikm: &[u8], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, ikm)
        .expand(info, out.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 length");
    out
}

fn fixed_hex<const N: usize>(text: &str) -> Result<[u8; N], FormatError> {
    unhex(text)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(FormatError::Malformed)
}

// --- Format B ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SealedPolicy {
    /// 24 random bytes, hex.
    pub nonce: String,
    /// Length of the sealed copy (plaintext + 16-byte tag) after the payload.
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HeaderB {
    pub payload: String,
    pub schema_version: i64,
    pub vault_id: VaultId,
    /// The salt the payload (a SQLCipher file) starts with.
    pub cipher_salt: CipherSalt,
    /// The salt the recovery envelope is bound to (the vault's when the envelope was made).
    pub recovery_salt: CipherSalt,
    pub recovery: Envelope,
    pub payload_len: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<SealedPolicy>,
}

impl HeaderB {
    fn policy_len(&self) -> u64 {
        self.policy.as_ref().map_or(0, |sealed| sealed.len)
    }

    /// Checks shape and bounds; returns the total length of the package with `prefix_len`.
    fn total_len(&self, prefix_len: usize) -> Result<u64, FormatError> {
        if self.payload != B_PAYLOAD {
            return Err(FormatError::UnsupportedVersion);
        }
        if let Some(sealed) = &self.policy {
            fixed_hex::<24>(&sealed.nonce)?;
            if sealed.len < TAG as u64 || sealed.len > MAX_POLICY_COPY + TAG as u64 {
                return Err(FormatError::Malformed);
            }
        }
        // A SQLCipher file is at least one page.
        if self.payload_len < 512 {
            return Err(FormatError::Malformed);
        }
        (prefix_len as u64)
            .checked_add(self.payload_len)
            .and_then(|n| n.checked_add(self.policy_len()))
            .and_then(|n| n.checked_add(MAC_LEN))
            .ok_or(FormatError::Malformed)
    }
}

/// What a format B package states about its vault (all non-secret).
pub struct VaultFacts {
    pub schema_version: i64,
    pub vault_id: VaultId,
    pub cipher_salt: CipherSalt,
    pub recovery_salt: CipherSalt,
    pub recovery: Envelope,
}

/// A format B package ready to be written: prefix and sealed policy copy are computed, so its
/// total length is known before a byte is written (format D needs it for its section table).
pub struct PreparedB {
    prefix: Vec<u8>,
    payload_len: u64,
    policy_ct: Vec<u8>,
    mac_key: Zeroizing<[u8; 32]>,
}

impl PreparedB {
    pub fn new(
        key: &VaultKey,
        facts: VaultFacts,
        payload_len: u64,
        policy: Option<&[u8]>,
    ) -> Result<Self, RandomError> {
        let (sealed, policy_ct) = match policy {
            None => (None, Vec::new()),
            Some(plain) => {
                let mut nonce = [0u8; 24];
                random_bytes(&mut nonce)?;
                let aad = [POLICY_AAD, facts.vault_id.as_bytes()].concat();
                let ct = XChaCha20Poly1305::new(hkdf32(key.expose(), POLICY_INFO).as_ref().into())
                    .encrypt(
                        XNonce::from_slice(&nonce),
                        Payload {
                            msg: plain,
                            aad: &aad,
                        },
                    )
                    .expect("XChaCha20-Poly1305 encryption cannot fail");
                (
                    Some(SealedPolicy {
                        nonce: hex(&nonce),
                        len: ct.len() as u64,
                    }),
                    ct,
                )
            }
        };
        let header = serde_json::to_vec(&HeaderB {
            payload: B_PAYLOAD.into(),
            schema_version: facts.schema_version,
            vault_id: facts.vault_id,
            cipher_salt: facts.cipher_salt,
            recovery_salt: facts.recovery_salt,
            recovery: facts.recovery,
            payload_len,
            policy: sealed,
        })
        .expect("a header serializes");
        Ok(Self {
            prefix: make_prefix(Kind::B, &header),
            payload_len,
            policy_ct,
            mac_key: hkdf32(key.expose(), MAC_INFO),
        })
    }

    pub fn total_len(&self) -> u64 {
        self.prefix.len() as u64 + self.payload_len + self.policy_ct.len() as u64 + MAC_LEN
    }

    /// Writes the package; `payload` must yield exactly the announced length.
    pub fn write_to(&self, out: &mut impl Write, payload: &mut impl Read) -> io::Result<()> {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(self.mac_key.as_ref()).expect("any key length");
        mac.update(&self.prefix);
        out.write_all(&self.prefix)?;
        let mut left = self.payload_len;
        let mut buf = vec![0u8; CHUNK];
        while left > 0 {
            let n = (left as usize).min(CHUNK);
            payload.read_exact(&mut buf[..n])?;
            mac.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            left -= n as u64;
        }
        let mut probe = [0u8; 1];
        if payload.read(&mut probe)? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload longer than announced",
            ));
        }
        mac.update(&self.policy_ct);
        out.write_all(&self.policy_ct)?;
        out.write_all(&mac.finalize().into_bytes())
    }
}

/// A format B package read and authenticated.
pub struct RestoredB {
    pub key: VaultKey,
    pub header: HeaderB,
    /// The policy copy, decrypted (it holds the user's privacy rules).
    pub policy: Option<Zeroizing<Vec<u8>>>,
}

/// The header of a format B package of `total` bytes, checked against that length. Reads only the
/// prefix.
pub fn read_b_header(r: &mut impl Read, total: u64) -> Result<(Prefix, HeaderB), FormatError> {
    let prefix = read_prefix(r)?;
    if prefix.kind != Kind::B {
        return Err(FormatError::UnsupportedFormat);
    }
    let header: HeaderB =
        serde_json::from_slice(&prefix.header).map_err(|_| FormatError::Malformed)?;
    if header.total_len(prefix.bytes.len())? != total {
        return Err(FormatError::Incomplete);
    }
    if header.schema_version != crate::schema::SCHEMA_VERSION {
        return Err(FormatError::SchemaMismatch);
    }
    Ok((prefix, header))
}

/// The vault key of a package, from the Recovery Spell (restore).
pub fn unlock_with_spell(header: &HeaderB, spell: &RecoverySpell) -> Result<VaultKey, FormatError> {
    recovery::unwrap(
        &header.recovery,
        spell,
        &header.vault_id,
        &header.recovery_salt,
    )
    .map_err(|error| match error {
        UnwrapError::SpellRejected => FormatError::SpellRejected,
        UnwrapError::Damaged => FormatError::RecoveryDamaged,
        UnwrapError::Unsupported => FormatError::UnsupportedVersion,
        UnwrapError::Malformed => FormatError::Malformed,
    })
}

/// Reads a format B package of `total` bytes, streaming the payload into `sink`. `unlock` gives
/// the vault key from the header (the spell on restore; the vault key itself when a new backup is
/// verified). Nothing written to `sink` may be trusted unless this returns `Ok`: the MAC over the
/// whole package is checked last.
pub fn read_b(
    r: &mut impl Read,
    total: u64,
    unlock: impl FnOnce(&HeaderB) -> Result<VaultKey, FormatError>,
    sink: &mut impl Write,
) -> Result<RestoredB, FormatError> {
    let (prefix, header) = read_b_header(r, total)?;
    let key = unlock(&header)?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(hkdf32(key.expose(), MAC_INFO).as_ref())
        .expect("any key length");
    mac.update(&prefix.bytes);
    let mut left = header.payload_len;
    let mut buf = vec![0u8; CHUNK];
    while left > 0 {
        let n = (left as usize).min(CHUNK);
        r.read_exact(&mut buf[..n]).map_err(|e| classify(&e))?;
        mac.update(&buf[..n]);
        sink.write_all(&buf[..n])
            .map_err(|e| FormatError::Output(e.kind()))?;
        left -= n as u64;
    }
    let mut policy_ct = vec![0u8; header.policy_len() as usize];
    r.read_exact(&mut policy_ct).map_err(|e| classify(&e))?;
    mac.update(&policy_ct);
    let mut tag = [0u8; 32];
    r.read_exact(&mut tag).map_err(|e| classify(&e))?;
    mac.verify_slice(&tag).map_err(|_| FormatError::Tampered)?;
    let policy = match &header.policy {
        None => None,
        Some(sealed) => {
            let nonce: [u8; 24] = fixed_hex(&sealed.nonce)?;
            let aad = [POLICY_AAD, header.vault_id.as_bytes()].concat();
            let plain = XChaCha20Poly1305::new(hkdf32(key.expose(), POLICY_INFO).as_ref().into())
                .decrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &policy_ct,
                        aad: &aad,
                    },
                )
                .map_err(|_| FormatError::Tampered)?;
            Some(Zeroizing::new(plain))
        }
    };
    Ok(RestoredB {
        key,
        header,
        policy,
    })
}

// --- Formats C and D ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Kdf {
    pub alg: String,
    pub version: u32,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    /// 32 random bytes, hex; new for every backup.
    pub salt: String,
}

impl Kdf {
    /// The parameters of a new backup, with a fresh salt.
    pub fn new_default() -> Result<Self, RandomError> {
        Self::with_cost(ARGON_M_KIB_DEFAULT, ARGON_T_DEFAULT, ARGON_P_DEFAULT)
    }

    pub fn with_cost(m_kib: u32, t: u32, p: u32) -> Result<Self, RandomError> {
        let mut salt = [0u8; 32];
        random_bytes(&mut salt)?;
        Ok(Self {
            alg: "argon2id".into(),
            version: 0x13,
            m_kib,
            t,
            p,
            salt: hex(&salt),
        })
    }

    /// Refuses anything outside the bounds before any memory is allocated.
    fn check(&self) -> Result<[u8; 32], FormatError> {
        if self.alg != "argon2id" || self.version != 0x13 {
            return Err(FormatError::UnsupportedVersion);
        }
        if !(ARGON_M_KIB_MIN..=ARGON_M_KIB_MAX).contains(&self.m_kib)
            || !(1..=ARGON_T_MAX).contains(&self.t)
            || !(1..=ARGON_P_MAX).contains(&self.p)
            || self.m_kib < 8 * self.p
        {
            return Err(FormatError::Malformed);
        }
        fixed_hex(&self.salt)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Section {
    pub kind: String,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HeaderP {
    pub kdf: Kdf,
    pub aead: String,
    pub chunk: u32,
    /// 19 random bytes, hex (XChaCha20's 24-byte nonce minus STREAM's counter and flag).
    pub nonce_prefix: String,
    pub check: String,
    pub sections: Vec<Section>,
}

impl HeaderP {
    /// The section layout a kind allows: C `meta`, optional `policy`, `vault-sqlite3`; D one
    /// `package-b`. Each section is bounded; returns the plaintext total.
    fn plain_len(&self, kind: Kind) -> Result<u64, FormatError> {
        let kinds: Vec<&str> = self.sections.iter().map(|s| s.kind.as_str()).collect();
        let layout_ok = match kind {
            Kind::C => {
                kinds == [SECTION_META, SECTION_VAULT]
                    || kinds == [SECTION_META, SECTION_POLICY, SECTION_VAULT]
            }
            Kind::D => kinds == [SECTION_PACKAGE],
            Kind::B => false,
        };
        if !layout_ok {
            return Err(FormatError::Malformed);
        }
        for section in &self.sections {
            let bounded = match section.kind.as_str() {
                SECTION_META => (1..=MAX_META).contains(&section.len),
                SECTION_POLICY => (1..=MAX_POLICY_COPY).contains(&section.len),
                _ => section.len >= 512,
            };
            if !bounded {
                return Err(FormatError::Malformed);
            }
        }
        self.sections
            .iter()
            .try_fold(0u64, |total, section| total.checked_add(section.len))
            .ok_or(FormatError::Malformed)
    }
}

/// Ciphertext length of `plain` bytes: one tag per chunk, at least one (final) chunk.
pub fn body_len(plain: u64) -> Option<u64> {
    let chunks = plain.div_ceil(CHUNK as u64).max(1);
    plain.checked_add(chunks.checked_mul(TAG as u64)?)
}

/// Reads the prefix of a C or D container and checks everything that needs no password: kind,
/// version, section layout and bounds, KDF parameters and the exact file length. Returns the
/// prefix, header and plaintext length.
fn checked_password_header(
    r: &mut impl Read,
    total: u64,
    kind: Kind,
) -> Result<(Prefix, HeaderP, u64), FormatError> {
    let prefix = read_prefix(r)?;
    if prefix.kind != kind {
        return Err(FormatError::UnsupportedFormat);
    }
    let header: HeaderP =
        serde_json::from_slice(&prefix.header).map_err(|_| FormatError::Malformed)?;
    if header.aead != AEAD || header.chunk as usize != CHUNK {
        return Err(FormatError::UnsupportedVersion);
    }
    let plain = header.plain_len(kind)?;
    let expected = body_len(plain)
        .and_then(|body| body.checked_add(prefix.bytes.len() as u64))
        .ok_or(FormatError::Malformed)?;
    if expected != total {
        return Err(FormatError::Incomplete);
    }
    header.kdf.check()?;
    fixed_hex::<19>(&header.nonce_prefix)?;
    fixed_hex::<32>(&header.check)?;
    Ok((prefix, header, plain))
}

/// What a restore can tell about a C or D container before the password is entered.
pub fn inspect_password(r: &mut impl Read, total: u64, kind: Kind) -> Result<HeaderP, FormatError> {
    checked_password_header(r, total, kind).map(|(_, header, _)| header)
}

/// The keys derived from a password: encryption and check.
struct PasswordKeys {
    enc: Zeroizing<[u8; 32]>,
    check: Zeroizing<[u8; 32]>,
}

fn password_keys(password: &[u8], kdf: &Kdf) -> Result<PasswordKeys, FormatError> {
    let salt = kdf.check()?;
    if password.is_empty() || password.len() > PASSWORD_MAX_BYTES {
        return Err(FormatError::PasswordRejected);
    }
    let params = argon2::Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32))
        .map_err(|_| FormatError::Malformed)?;
    let mut master = Zeroizing::new([0u8; 32]);
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(password, &salt, master.as_mut())
        .map_err(|_| FormatError::Malformed)?;
    let hkdf = Hkdf::<Sha256>::from_prk(master.as_ref()).expect("a 32-byte PRK");
    let (mut enc, mut check) = (Zeroizing::new([0u8; 32]), Zeroizing::new([0u8; 32]));
    hkdf.expand(ENC_INFO, enc.as_mut()).expect("valid length");
    hkdf.expand(CHECK_INFO, check.as_mut())
        .expect("valid length");
    Ok(PasswordKeys { enc, check })
}

fn check_mac(keys: &PasswordKeys, kind: Kind) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(keys.check.as_ref()).expect("any length");
    mac.update(CHECK_MSG);
    mac.update(&[kind.byte()]);
    mac
}

/// Streams the sections of a format C or D container. The sections are written in order, as the
/// header announced them, and `finish` writes the final chunk.
pub struct PasswordWriter<W: Write> {
    out: W,
    enc: Option<EncryptorBE32<XChaCha20Poly1305>>,
    aad: [u8; 32],
    buf: Zeroizing<Vec<u8>>,
    remaining: u64,
}

impl<W: Write> PasswordWriter<W> {
    /// Derives the key (one Argon2id run), writes the prefix and returns the writer.
    pub fn start(
        mut out: W,
        password: &[u8],
        kind: Kind,
        kdf: Kdf,
        sections: Vec<Section>,
    ) -> io::Result<Self> {
        let other = |_| io::Error::other("random or parameters");
        let keys = password_keys(password, &kdf).map_err(|_| io::Error::other("kdf"))?;
        let mut nonce_prefix = [0u8; 19];
        random_bytes(&mut nonce_prefix).map_err(other)?;
        let header = HeaderP {
            kdf,
            aead: AEAD.into(),
            chunk: CHUNK as u32,
            nonce_prefix: hex(&nonce_prefix),
            check: hex(&check_mac(&keys, kind).finalize().into_bytes()),
            sections,
        };
        let remaining = header
            .plain_len(kind)
            .map_err(|_| io::Error::other("section layout"))?;
        let prefix = make_prefix(
            kind,
            &serde_json::to_vec(&header).expect("a header serializes"),
        );
        out.write_all(&prefix)?;
        Ok(Self {
            out,
            enc: Some(EncryptorBE32::from_aead(
                XChaCha20Poly1305::new(keys.enc.as_ref().into()),
                nonce_prefix.as_ref().into(),
            )),
            aad: Sha256::digest(&prefix).into(),
            buf: Zeroizing::new(Vec::with_capacity(CHUNK)),
            remaining,
        })
    }

    /// Writes the final chunk; every announced byte must have been written.
    pub fn finish(mut self) -> io::Result<W> {
        if self.remaining != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sections shorter than announced",
            ));
        }
        let enc = self.enc.take().expect("not finished twice");
        let ct = enc
            .encrypt_last(Payload {
                msg: &self.buf,
                aad: &self.aad,
            })
            .map_err(|_| io::Error::other("stream"))?;
        self.buf.zeroize();
        self.out.write_all(&ct)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

impl<W: Write> Write for PasswordWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        let take = data.len().min(CHUNK - self.buf.len());
        if take as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sections longer than announced",
            ));
        }
        self.buf.extend_from_slice(&data[..take]);
        self.remaining -= take as u64;
        // A full chunk is final only if nothing follows it; `finish` writes the final one.
        if self.buf.len() == CHUNK && self.remaining > 0 {
            let enc = self.enc.as_mut().expect("not finished");
            let ct = enc
                .encrypt_next(Payload {
                    msg: &self.buf,
                    aad: &self.aad,
                })
                .map_err(|_| io::Error::other("stream"))?;
            self.buf.zeroize();
            self.out.write_all(&ct)?;
        }
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Decrypts a format C or D body chunk by chunk. Every byte it returns has been authenticated; an
/// authentication failure is an `io::Error` that `classify` reports as `Tampered`.
pub struct PasswordReader<R: Read> {
    inner: R,
    dec: Option<DecryptorBE32<XChaCha20Poly1305>>,
    aad: [u8; 32],
    remaining: u64,
    plain: Zeroizing<Vec<u8>>,
    pos: usize,
    cipher: Vec<u8>,
}

impl<R: Read> PasswordReader<R> {
    /// Reads and checks the prefix (kind, bounds, exact `total` length), derives the key (one
    /// Argon2id run, after the bounds) and checks the password.
    pub fn open(
        mut inner: R,
        total: u64,
        password: &[u8],
        kind: Kind,
    ) -> Result<(HeaderP, Self), FormatError> {
        let (prefix, header, plain) = checked_password_header(&mut inner, total, kind)?;
        let nonce_prefix: [u8; 19] = fixed_hex(&header.nonce_prefix)?;
        let check: [u8; 32] = fixed_hex(&header.check)?;
        let keys = password_keys(password, &header.kdf)?;
        check_mac(&keys, kind)
            .verify_slice(&check)
            .map_err(|_| FormatError::PasswordRejected)?;
        let reader = Self {
            inner,
            dec: Some(DecryptorBE32::from_aead(
                XChaCha20Poly1305::new(keys.enc.as_ref().into()),
                nonce_prefix.as_ref().into(),
            )),
            aad: Sha256::digest(&prefix.bytes).into(),
            remaining: plain,
            plain: Zeroizing::new(Vec::new()),
            pos: 0,
            cipher: vec![0u8; CHUNK + TAG],
        };
        Ok((header, reader))
    }

    /// True once every chunk, the final one included, was read and authenticated.
    pub fn is_complete(&self) -> bool {
        self.remaining == 0 && self.pos == self.plain.len() && self.dec.is_none()
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        let n = (self.remaining as usize).min(CHUNK);
        let last = self.remaining <= CHUNK as u64;
        self.inner.read_exact(&mut self.cipher[..n + TAG])?;
        let payload = Payload {
            msg: &self.cipher[..n + TAG],
            aad: &self.aad,
        };
        let plain = if last {
            let dec = self.dec.take().ok_or_else(corrupt)?;
            dec.decrypt_last(payload)
        } else {
            self.dec.as_mut().ok_or_else(corrupt)?.decrypt_next(payload)
        }
        .map_err(|_| corrupt())?;
        self.plain.zeroize();
        *self.plain = plain;
        self.pos = 0;
        self.remaining -= n as u64;
        Ok(())
    }
}

impl<R: Read> Read for PasswordReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.plain.len() {
            if self.remaining == 0 && self.dec.is_none() {
                return Ok(0);
            }
            self.next_chunk()?;
        }
        let n = out.len().min(self.plain.len() - self.pos);
        out[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Copies exactly `len` bytes from a container reader to `sink`, telling read failures (damage,
/// truncation) from write failures (e.g. a full disk).
pub fn copy_exact(
    reader: &mut impl Read,
    len: u64,
    sink: &mut impl Write,
) -> Result<(), FormatError> {
    let mut left = len;
    let mut buf = Zeroizing::new(vec![0u8; CHUNK]);
    while left > 0 {
        let n = (left as usize).min(CHUNK);
        reader.read_exact(&mut buf[..n]).map_err(|e| classify(&e))?;
        sink.write_all(&buf[..n])
            .map_err(|e| FormatError::Output(e.kind()))?;
        left -= n as u64;
    }
    Ok(())
}

/// Reads a small section (meta, policy copy) into memory; its length is bounded by the header.
pub fn read_small(reader: &mut impl Read, len: u64) -> Result<Zeroizing<Vec<u8>>, FormatError> {
    let mut out = Zeroizing::new(vec![0u8; len as usize]);
    reader.read_exact(&mut out).map_err(|e| classify(&e))?;
    Ok(out)
}

/// The `meta` section of a format C backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Meta {
    pub format: String,
    pub version: u32,
    pub mode: String,
    pub schema_version: i64,
}

pub const META_FORMAT: &str = "memocampus-backup-meta";

impl Meta {
    pub fn plain() -> Self {
        Self {
            format: META_FORMAT.into(),
            version: 1,
            mode: "plain".into(),
            schema_version: crate::schema::SCHEMA_VERSION,
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let meta: Self = serde_json::from_slice(bytes).map_err(|_| FormatError::Malformed)?;
        if meta.format != META_FORMAT || meta.version != 1 || meta.mode != "plain" {
            return Err(FormatError::UnsupportedVersion);
        }
        if meta.schema_version != crate::schema::SCHEMA_VERSION {
            return Err(FormatError::SchemaMismatch);
        }
        Ok(meta)
    }
}

/// A writer that hashes what passes through it (to compare a backup's content with its source).
pub struct Hashing<W> {
    pub inner: W,
    hash: Sha256,
}

impl<W> Hashing<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hash: Sha256::new(),
        }
    }

    pub fn digest(self) -> [u8; 32] {
        self.hash.finalize().into()
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(data)?;
        self.hash.update(&data[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        self.hash.update(&out[..n]);
        Ok(n)
    }
}

#[cfg(test)]
mod tests;
