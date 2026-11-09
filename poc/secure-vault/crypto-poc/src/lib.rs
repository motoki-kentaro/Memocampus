//! Issue #51 Slice A — isolated proof of concept for Secure Vault key management and the backup
//! container. NOT production code: nothing here is linked into the product, and every test works in
//! a fresh temporary directory with synthetic data. See `docs/secure-vault/poc-results.md`.
//!
//! Primitives come from audited, widely used crates (RustCrypto `chacha20poly1305`, `hkdf`, `hmac`,
//! `sha2`, `argon2`; rust-bitcoin `bip39`) and Windows (`BCryptGenRandom`, DPAPI). No primitive is
//! invented here; only their composition (the file layout) is Memocampus-specific.

use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub type VaultKey = [u8; 32];

// ---------------------------------------------------------------------------------------------
// Randomness (A-2): the OS CSPRNG.

#[cfg(windows)]
pub fn os_random(buf: &mut [u8]) {
    use windows_sys::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    // SAFETY: valid buffer and length; a null algorithm handle with the system-preferred flag.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    assert_eq!(status, 0, "BCryptGenRandom failed: NTSTATUS {status:#x}");
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    os_random(&mut out);
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

fn unhex_n<const N: usize>(text: &str) -> Option<[u8; N]> {
    unhex(text)?.try_into().ok()
}

// ---------------------------------------------------------------------------------------------
// DPAPI (A-2): user scope, no UI, optional entropy as a purpose label.

#[cfg(windows)]
pub mod dpapi {
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: DPAPI returned a LocalAlloc'ed buffer of cbData bytes that we now own.
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(out.pbData as _) };
        bytes
    }

    /// User-scoped protection (no CRYPTPROTECT_LOCAL_MACHINE). Err carries GetLastError().
    pub fn protect(secret: &[u8], entropy: &[u8]) -> Result<Vec<u8>, u32> {
        let (input, entropy) = (blob(secret), blob(entropy));
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: all pointers are valid for the call; no prompt struct, UI forbidden.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(unsafe { GetLastError() });
        }
        Ok(take(out))
    }

    pub fn unprotect(protected: &[u8], entropy: &[u8]) -> Result<Vec<u8>, u32> {
        let (input, entropy) = (blob(protected), blob(entropy));
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as above.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(unsafe { GetLastError() });
        }
        Ok(take(out))
    }
}

// ---------------------------------------------------------------------------------------------
// Recovery phrase (A-3): BIP-39 English wordlist and entropy<->mnemonic encoding with its checksum,
// 128-bit entropy = 12 words. NOT used from BIP-39: the PBKDF2 mnemonic->seed step, the optional
// passphrase, and any BIP-32 derivation. The 16 entropy bytes are the input keying material.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseError {
    WordCount(usize),
    /// 1-based position of a word not in the list.
    UnknownWord(usize),
    Checksum,
}

/// Normalisation accepted from the user: ASCII case-folded, any whitespace between words.
pub fn parse_phrase(input: &str) -> Result<[u8; 16], PhraseError> {
    let words: Vec<String> = input.split_whitespace().map(str::to_ascii_lowercase).collect();
    if words.len() != 12 {
        return Err(PhraseError::WordCount(words.len()));
    }
    let list = bip39::Language::English.word_list();
    for (i, word) in words.iter().enumerate() {
        if !list.contains(&word.as_str()) {
            return Err(PhraseError::UnknownWord(i + 1));
        }
    }
    let mnemonic = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &words.join(" "))
        .map_err(|_| PhraseError::Checksum)?;
    let entropy = mnemonic.to_entropy();
    entropy.try_into().map_err(|_| PhraseError::WordCount(12))
}

pub fn phrase_of(entropy: &[u8; 16]) -> String {
    bip39::Mnemonic::from_entropy_in(bip39::Language::English, entropy)
        .expect("16 bytes is a valid BIP-39 entropy length")
        .to_string()
}

// ---------------------------------------------------------------------------------------------
// Recovery envelope (A-2/A-3): VK wrapped under a KEK derived from the phrase entropy.
// KEK = HKDF-SHA256(ikm = entropy, salt = 32 random bytes, info = "memocampus/recovery/v1/kek").
// A fast KDF suffices: the input has 128 bits of entropy, so there is nothing to stretch (unlike a
// password). Wrap = XChaCha20-Poly1305, random 24-byte nonce, AAD binds the vault id and purpose.

const RECOVERY_INFO: &[u8] = b"memocampus/recovery/v1/kek";
const RECOVERY_AAD: &[u8] = b"memocampus/recovery/v1/wrap\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecoveryEnvelope {
    pub scheme: String,
    pub salt: String,
    pub nonce: String,
    pub wrapped: String,
}

pub const RECOVERY_SCHEME: &str = "bip39-128/hkdf-sha256/xchacha20poly1305/v1";

fn kek(entropy: &[u8; 16], salt: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt), entropy)
        .expand(RECOVERY_INFO, &mut out)
        .unwrap();
    out
}

fn recovery_aad(vault_id: &[u8; 16]) -> Vec<u8> {
    [RECOVERY_AAD, vault_id].concat()
}

pub fn wrap_for_recovery(vk: &VaultKey, entropy: &[u8; 16], vault_id: &[u8; 16]) -> RecoveryEnvelope {
    let salt: [u8; 32] = random();
    let nonce: [u8; 24] = random();
    let aead = XChaCha20Poly1305::new(&kek(entropy, &salt).into());
    let wrapped = aead
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: vk,
                aad: &recovery_aad(vault_id),
            },
        )
        .unwrap();
    RecoveryEnvelope {
        scheme: RECOVERY_SCHEME.into(),
        salt: hex(&salt),
        nonce: hex(&nonce),
        wrapped: hex(&wrapped),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwrapError {
    Unsupported,
    Malformed,
    /// Authentication failed: another phrase, another vault id, or altered metadata.
    Rejected,
}

pub fn unwrap_recovery(
    env: &RecoveryEnvelope,
    entropy: &[u8; 16],
    vault_id: &[u8; 16],
) -> Result<VaultKey, UnwrapError> {
    if env.scheme != RECOVERY_SCHEME {
        return Err(UnwrapError::Unsupported);
    }
    let salt: [u8; 32] = unhex_n(&env.salt).ok_or(UnwrapError::Malformed)?;
    let nonce: [u8; 24] = unhex_n(&env.nonce).ok_or(UnwrapError::Malformed)?;
    let wrapped = unhex(&env.wrapped).filter(|w| w.len() == 48).ok_or(UnwrapError::Malformed)?;
    let aead = XChaCha20Poly1305::new(&kek(entropy, &salt).into());
    let vk = aead
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &wrapped,
                aad: &recovery_aad(vault_id),
            },
        )
        .map_err(|_| UnwrapError::Rejected)?;
    vk.try_into().map_err(|_| UnwrapError::Malformed)
}

// ---------------------------------------------------------------------------------------------
// Backup container (A-5). Format A is a raw SQLite file and has no container.
//
//   magic     8  89 'M' 'C' 'V' 'B' 0D 0A 1A   (never "SQLite format 3\0"; detects text-mode damage)
//   version   2  u16 LE = 1
//   kind      1  'B' (encrypted-vault package) | 'C' (password envelope)
//   flags     1  0
//   hdr_len   4  u32 LE, 1..=MAX_HEADER
//   header       UTF-8 JSON, strict fields, non-secret only
//   B: payload (payloadLen bytes, SQLCipher file) || HMAC-SHA256(K_mac, prefix || payload)
//   C: STREAM(XChaCha20-Poly1305, BE32) chunks of the inner plaintext, AAD = SHA-256(prefix)

pub const MAGIC: [u8; 8] = [0x89, b'M', b'C', b'V', b'B', 0x0D, 0x0A, 0x1A];
pub const VERSION: u16 = 1;
pub const MAX_HEADER: u32 = 64 * 1024;
pub const CHUNK: usize = 64 * 1024;
const MAC_INFO: &[u8] = b"memocampus/backup-b/v1/mac";
const POLICY_INFO: &[u8] = b"memocampus/backup-b/v1/policy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detected {
    /// "SQLite format 3\0": format A (still to be fully validated).
    PlainSqlite,
    Container { kind: u8 },
    /// Neither: e.g. a bare SQLCipher file (random-looking first page) or anything else.
    Unknown,
}

pub fn detect(first16: &[u8]) -> Detected {
    if first16.starts_with(b"SQLite format 3\0") {
        Detected::PlainSqlite
    } else if first16.len() >= 12 && first16[..8] == MAGIC {
        Detected::Container { kind: first16[10] }
    } else {
        Detected::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreError {
    UnsupportedFormat,
    UnsupportedVersion,
    /// Lengths, header or parameters out of bounds: refused before any expensive work.
    Malformed,
    /// Truncated or extended file (length does not add up).
    Incomplete,
    PhraseRejected,
    PasswordRejected,
    /// Authentication of header/payload failed after the key was accepted.
    Tampered,
    Io,
}

fn prefix(kind: u8, header: &[u8]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(kind);
    out.push(0);
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(header);
    out
}

/// Reads and bounds-checks the prefix; returns (kind, header bytes, prefix bytes).
fn read_prefix(r: &mut impl Read) -> Result<(u8, Vec<u8>, Vec<u8>), RestoreError> {
    let mut fixed = [0u8; 16];
    r.read_exact(&mut fixed).map_err(|_| RestoreError::Incomplete)?;
    if fixed[..8] != MAGIC {
        return Err(RestoreError::UnsupportedFormat);
    }
    if u16::from_le_bytes([fixed[8], fixed[9]]) != VERSION {
        return Err(RestoreError::UnsupportedVersion);
    }
    if fixed[11] != 0 {
        return Err(RestoreError::Malformed);
    }
    let len = u32::from_le_bytes(fixed[12..16].try_into().unwrap());
    if len == 0 || len > MAX_HEADER {
        return Err(RestoreError::Malformed);
    }
    let mut header = vec![0u8; len as usize];
    r.read_exact(&mut header).map_err(|_| RestoreError::Incomplete)?;
    let mut all = fixed.to_vec();
    all.extend_from_slice(&header);
    Ok((fixed[10], header, all))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HeaderB {
    pub payload: String,
    pub vault_id: String,
    pub payload_len: u64,
    pub recovery: RecoveryEnvelope,
    /// Optional copy of the global privacy policy, encrypted under a key derived from VK.
    pub policy: Option<SealedBlob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SealedBlob {
    pub nonce: String,
    pub ct: String,
}

fn derive(vk: &VaultKey, info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(None, vk).expand(info, &mut out).unwrap();
    out
}

pub fn write_b(
    out: &mut impl Write,
    vk: &VaultKey,
    vault_id: &[u8; 16],
    recovery: RecoveryEnvelope,
    payload: &[u8],
    policy: Option<&[u8]>,
) -> std::io::Result<()> {
    let policy = policy.map(|plain| {
        let nonce: [u8; 24] = random();
        let ct = XChaCha20Poly1305::new(&derive(vk, POLICY_INFO).into())
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: vault_id })
            .unwrap();
        SealedBlob {
            nonce: hex(&nonce),
            ct: hex(&ct),
        }
    });
    let header = serde_json::to_vec(&HeaderB {
        payload: "sqlcipher4".into(),
        vault_id: hex(vault_id),
        payload_len: payload.len() as u64,
        recovery,
        policy,
    })
    .unwrap();
    let prefix = prefix(b'B', &header);
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&derive(vk, MAC_INFO)).unwrap();
    mac.update(&prefix);
    mac.update(payload);
    out.write_all(&prefix)?;
    out.write_all(payload)?;
    out.write_all(&mac.finalize().into_bytes())
}

pub struct RestoredB {
    pub vk: VaultKey,
    pub vault_id: [u8; 16],
    pub policy: Option<Vec<u8>>,
}

/// Restores a format B package from `r` (total length `total`), streaming the payload into `sink`.
/// Nothing in `sink` may be trusted (or published) unless this returns Ok: the MAC is checked last.
pub fn read_b(
    r: &mut impl Read,
    total: u64,
    entropy: &[u8; 16],
    sink: &mut impl Write,
) -> Result<RestoredB, RestoreError> {
    let (kind, header, prefix) = read_prefix(r)?;
    if kind != b'B' {
        return Err(RestoreError::UnsupportedFormat);
    }
    let header: HeaderB = serde_json::from_slice(&header).map_err(|_| RestoreError::Malformed)?;
    if header.payload != "sqlcipher4" {
        return Err(RestoreError::UnsupportedFormat);
    }
    let vault_id: [u8; 16] = unhex_n(&header.vault_id).ok_or(RestoreError::Malformed)?;
    // Exact length before any crypto: detects truncation and appended bytes.
    if Some(total) != (prefix.len() as u64).checked_add(header.payload_len).and_then(|n| n.checked_add(32)) {
        return Err(RestoreError::Incomplete);
    }
    let vk = unwrap_recovery(&header.recovery, entropy, &vault_id).map_err(|e| match e {
        UnwrapError::Rejected => RestoreError::PhraseRejected,
        UnwrapError::Unsupported => RestoreError::UnsupportedVersion,
        UnwrapError::Malformed => RestoreError::Malformed,
    })?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&derive(&vk, MAC_INFO)).unwrap();
    mac.update(&prefix);
    let mut left = header.payload_len;
    let mut buf = vec![0u8; CHUNK];
    while left > 0 {
        let n = (left as usize).min(CHUNK);
        r.read_exact(&mut buf[..n]).map_err(|_| RestoreError::Incomplete)?;
        mac.update(&buf[..n]);
        sink.write_all(&buf[..n]).map_err(|_| RestoreError::Io)?;
        left -= n as u64;
    }
    let mut tag = [0u8; 32];
    r.read_exact(&mut tag).map_err(|_| RestoreError::Incomplete)?;
    mac.verify_slice(&tag).map_err(|_| RestoreError::Tampered)?;
    let policy = match &header.policy {
        None => None,
        Some(sealed) => {
            let nonce: [u8; 24] = unhex_n(&sealed.nonce).ok_or(RestoreError::Malformed)?;
            let ct = unhex(&sealed.ct).ok_or(RestoreError::Malformed)?;
            Some(
                XChaCha20Poly1305::new(&derive(&vk, POLICY_INFO).into())
                    .decrypt(XNonce::from_slice(&nonce), Payload { msg: &ct, aad: &vault_id })
                    .map_err(|_| RestoreError::Tampered)?,
            )
        }
    };
    Ok(RestoredB { vk, vault_id, policy })
}

// Format C ---------------------------------------------------------------------------------------

/// Bounds a restore accepts, checked before Argon2 allocates anything (A-5 resource limits).
pub const ARGON_M_KIB_MIN: u32 = 19 * 1024;
pub const ARGON_M_KIB_MAX: u32 = 1024 * 1024;
pub const ARGON_T_MAX: u32 = 10;
pub const ARGON_P_MAX: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Kdf {
    pub alg: String,
    pub version: u32,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    pub salt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Section {
    /// "policy" | "vault-a" (raw SQLite) | "package-b" (a whole format B file)
    pub kind: String,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HeaderC {
    pub kdf: Kdf,
    pub aead: String,
    pub chunk: u32,
    pub nonce_prefix: String,
    /// HMAC-SHA256(K_check, CHECK_MSG): tells a wrong password from a damaged body. Guessing via it
    /// costs one Argon2id run per guess, the same as via the first chunk, so it adds no shortcut.
    pub check: String,
    pub sections: Vec<Section>,
}

const CHECK_MSG: &[u8] = b"memocampus/backup-c/v1/password-check";

/// (K_enc, K_check) from the Argon2id output.
fn split_password_key(master: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::from_prk(master).expect("32-byte PRK");
    let (mut enc, mut check) = ([0u8; 32], [0u8; 32]);
    hk.expand(b"memocampus/backup-c/v1/enc", &mut enc).unwrap();
    hk.expand(b"memocampus/backup-c/v1/check", &mut check).unwrap();
    (enc, check)
}

fn check_value(k_check: &[u8; 32]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(k_check).unwrap();
    mac.update(CHECK_MSG);
    mac.finalize().into_bytes().into()
}

pub const AEAD_C: &str = "xchacha20poly1305-stream-be32";

pub fn password_key(password: &str, kdf: &Kdf) -> Result<[u8; 32], RestoreError> {
    if kdf.alg != "argon2id" || kdf.version != 0x13 {
        return Err(RestoreError::UnsupportedVersion);
    }
    if !(ARGON_M_KIB_MIN..=ARGON_M_KIB_MAX).contains(&kdf.m_kib)
        || !(1..=ARGON_T_MAX).contains(&kdf.t)
        || !(1..=ARGON_P_MAX).contains(&kdf.p)
    {
        return Err(RestoreError::Malformed);
    }
    let salt: [u8; 32] = unhex_n(&kdf.salt).ok_or(RestoreError::Malformed)?;
    let params = argon2::Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|_| RestoreError::Malformed)?;
    let mut key = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(password.as_bytes(), &salt, &mut key)
        .map_err(|_| RestoreError::Malformed)?;
    Ok(key)
}

pub fn default_kdf() -> Kdf {
    Kdf {
        alg: "argon2id".into(),
        version: 0x13,
        m_kib: 64 * 1024,
        t: 3,
        p: 1,
        salt: hex(&random::<32>()),
    }
}

fn body_len(plain: u64) -> u64 {
    let chunks = plain.div_ceil(CHUNK as u64).max(1);
    plain + 16 * chunks
}

pub fn write_c(out: &mut impl Write, password: &str, kdf: Kdf, sections: &[(&str, &[u8])]) -> std::io::Result<()> {
    let nonce_prefix: [u8; 19] = random();
    let (key, k_check) = split_password_key(&password_key(password, &kdf).expect("own parameters are in bounds"));
    let header = serde_json::to_vec(&HeaderC {
        kdf: kdf.clone(),
        aead: AEAD_C.into(),
        chunk: CHUNK as u32,
        nonce_prefix: hex(&nonce_prefix),
        check: hex(&check_value(&k_check)),
        sections: sections
            .iter()
            .map(|(kind, data)| Section {
                kind: (*kind).into(),
                len: data.len() as u64,
            })
            .collect(),
    })
    .unwrap();
    let prefix = prefix(b'C', &header);
    let aad = Sha256::digest(&prefix);
    let mut enc = EncryptorBE32::from_aead(XChaCha20Poly1305::new(&key.into()), nonce_prefix.as_ref().into());
    out.write_all(&prefix)?;
    let plain: Vec<u8> = sections.iter().flat_map(|(_, d)| d.iter().copied()).collect();
    let mut chunks = plain.chunks(CHUNK).peekable();
    if plain.is_empty() {
        out.write_all(&enc.encrypt_last(Payload { msg: &[], aad: &aad }).unwrap())?;
        return Ok(());
    }
    while let Some(chunk) = chunks.next() {
        if chunks.peek().is_some() {
            out.write_all(&enc.encrypt_next(Payload { msg: chunk, aad: &aad }).unwrap())?;
        } else {
            out.write_all(&enc.encrypt_last(Payload { msg: chunk, aad: &aad }).unwrap())?;
            break;
        }
    }
    Ok(())
}

/// Decrypts a format C file, handing each section's plaintext to `sink(kind, bytes)` as it is
/// authenticated chunk by chunk. A failure at any chunk stops; the caller discards what it got.
pub fn read_c(
    r: &mut impl Read,
    total: u64,
    password: &str,
    sink: &mut dyn FnMut(&str, &[u8]) -> Result<(), RestoreError>,
) -> Result<Vec<Section>, RestoreError> {
    let (kind, header, prefix) = read_prefix(r)?;
    if kind != b'C' {
        return Err(RestoreError::UnsupportedFormat);
    }
    let header: HeaderC = serde_json::from_slice(&header).map_err(|_| RestoreError::Malformed)?;
    if header.aead != AEAD_C || header.chunk as usize != CHUNK {
        return Err(RestoreError::UnsupportedVersion);
    }
    let allowed = ["policy", "vault-a", "package-b"];
    if header.sections.is_empty()
        || header.sections.len() > 4
        || header.sections.iter().any(|s| !allowed.contains(&s.kind.as_str()))
    {
        return Err(RestoreError::Malformed);
    }
    let plain_len = header
        .sections
        .iter()
        .try_fold(0u64, |acc, s| acc.checked_add(s.len))
        .ok_or(RestoreError::Malformed)?;
    if Some(total) != (prefix.len() as u64).checked_add(body_len(plain_len)) {
        return Err(RestoreError::Incomplete);
    }
    let nonce_prefix: [u8; 19] = unhex_n(&header.nonce_prefix).ok_or(RestoreError::Malformed)?;
    // Bounds are checked inside before Argon2 runs.
    let check: [u8; 32] = unhex_n(&header.check).ok_or(RestoreError::Malformed)?;
    let (key, k_check) = split_password_key(&password_key(password, &header.kdf)?);
    let mut verifier = <Hmac<Sha256> as Mac>::new_from_slice(&k_check).unwrap();
    verifier.update(CHECK_MSG);
    verifier.verify_slice(&check).map_err(|_| RestoreError::PasswordRejected)?;
    let aad = Sha256::digest(&prefix);
    let mut dec = DecryptorBE32::from_aead(XChaCha20Poly1305::new(&key.into()), nonce_prefix.as_ref().into());
    let mut left = plain_len;
    let mut section = 0usize;
    let mut in_section = 0u64;
    let mut buf = vec![0u8; CHUNK + 16];
    loop {
        let n = (left as usize).min(CHUNK);
        let last = left <= CHUNK as u64;
        r.read_exact(&mut buf[..n + 16]).map_err(|_| RestoreError::Incomplete)?;
        let payload = Payload { msg: &buf[..n + 16], aad: &aad };
        let plain = if last { dec.decrypt_last_in_place_hack(payload) } else { dec.decrypt_next(payload) };
        // The password was verified above, so any chunk failure is damage or tampering.
        let plain = plain.map_err(|_| RestoreError::Tampered)?;
        let mut rest = &plain[..];
        while !rest.is_empty() {
            while header.sections[section].len == in_section {
                section += 1;
                in_section = 0;
            }
            let take = ((header.sections[section].len - in_section) as usize).min(rest.len());
            sink(&header.sections[section].kind, &rest[..take])?;
            in_section += take as u64;
            rest = &rest[take..];
        }
        left -= n as u64;
        if last {
            break;
        }
    }
    Ok(header.sections)
}

/// `DecryptorBE32::decrypt_last` consumes the decryptor; this keeps the loop above simple.
trait LastHack {
    fn decrypt_last_in_place_hack(&mut self, payload: Payload<'_, '_>) -> Result<Vec<u8>, chacha20poly1305::aead::Error>;
}

impl LastHack for DecryptorBE32<XChaCha20Poly1305> {
    fn decrypt_last_in_place_hack(&mut self, payload: Payload<'_, '_>) -> Result<Vec<u8>, chacha20poly1305::aead::Error> {
        // The STREAM "last" flag lives in the nonce; `decrypt_last` is the only way to set it, and it
        // takes `self` by value. Swap in a dummy decryptor to take ownership.
        let dummy = DecryptorBE32::from_aead(XChaCha20Poly1305::new(&[0u8; 32].into()), [0u8; 19].as_ref().into());
        std::mem::replace(self, dummy).decrypt_last(payload)
    }
}
