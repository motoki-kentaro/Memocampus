//! The OS key protector: Windows DPAPI in user scope (`CryptProtectData` without
//! `CRYPTPROTECT_LOCAL_MACHINE`, UI forbidden). DPAPI derives its keys from the user's logon
//! credentials and master keys; Memocampus never uses the user's SID or name as key material.
//!
//! The entropy binds a blob to its purpose and vault: `"memocampus/vault-key/v1\0" || vaultId`.
//! DPAPI does not authenticate every byte of its blob (Slice A, PoC A-2.2: some altered blobs
//! still decrypt, always to the same data), so callers verify the result with the key check value.
//!
//! Other platforms have no protector in this version (residual risk R10): every call reports
//! `Unsupported`, and no key is ever generated or stored in the clear in its place.

use super::secret::VaultId;
use zeroize::Zeroizing;

const ENTROPY_LABEL: &[u8] = b"memocampus/vault-key/v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectorError {
    /// No protector on this platform.
    Unsupported,
    /// The OS refused (Win32 error code): another user's blob, damaged blob or profile.
    Refused(u32),
}

/// The identifier stored with each protected blob.
pub const NAME: &str = "dpapi-user";

fn entropy(vault_id: &VaultId) -> Vec<u8> {
    [ENTROPY_LABEL, vault_id.as_bytes()].concat()
}

#[cfg(windows)]
mod dpapi {
    use super::ProtectorError;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    use zeroize::{Zeroize, Zeroizing};

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    /// Copies DPAPI's output, zeroes DPAPI's buffer and frees it.
    fn take(out: CRYPT_INTEGER_BLOB) -> Zeroizing<Vec<u8>> {
        // SAFETY: DPAPI returned a LocalAlloc'ed buffer of `cbData` bytes, now owned by us.
        let slice = unsafe { std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize) };
        let copy = Zeroizing::new(slice.to_vec());
        slice.zeroize();
        // SAFETY: allocated by DPAPI with LocalAlloc, freed exactly once.
        unsafe { LocalFree(out.pbData as _) };
        copy
    }

    pub fn protect(secret: &[u8], entropy: &[u8]) -> Result<Vec<u8>, ProtectorError> {
        let (input, entropy) = (blob(secret), blob(entropy));
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: every pointer is valid for the call; no description, no prompt, UI forbidden.
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
            return Err(ProtectorError::Refused(unsafe { GetLastError() }));
        }
        Ok(take(out).to_vec())
    }

    pub fn unprotect(
        blob_bytes: &[u8],
        entropy: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, ProtectorError> {
        let (input, entropy) = (blob(blob_bytes), blob(entropy));
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as above; the description pointer is null (not requested).
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
            return Err(ProtectorError::Refused(unsafe { GetLastError() }));
        }
        Ok(take(out))
    }
}

/// Protects `secret` for the current Windows user, bound to `vault_id`.
pub fn protect(secret: &[u8], vault_id: &VaultId) -> Result<Vec<u8>, ProtectorError> {
    #[cfg(windows)]
    {
        dpapi::protect(secret, &entropy(vault_id))
    }
    #[cfg(not(windows))]
    {
        let _ = (secret, entropy(vault_id));
        Err(ProtectorError::Unsupported)
    }
}

pub fn unprotect(blob: &[u8], vault_id: &VaultId) -> Result<Zeroizing<Vec<u8>>, ProtectorError> {
    #[cfg(windows)]
    {
        dpapi::unprotect(blob, &entropy(vault_id))
    }
    #[cfg(not(windows))]
    {
        let _ = (blob, entropy(vault_id));
        Err(ProtectorError::Unsupported)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_blob_opens_only_for_its_vault_and_never_holds_the_secret_in_clear() {
        let id = VaultId::generate().unwrap();
        let secret = [0x42u8; 32];
        let blob = protect(&secret, &id).unwrap();
        assert!(!blob.windows(32).any(|window| window == secret));
        assert_eq!(unprotect(&blob, &id).unwrap().as_slice(), &secret);
        // Another vault id is other entropy: refused (ERROR_INVALID_DATA).
        assert!(matches!(
            unprotect(&blob, &VaultId::generate().unwrap()),
            Err(ProtectorError::Refused(_))
        ));
        assert!(unprotect(&blob[..blob.len() - 1], &id).is_err());
        assert!(unprotect(&[], &id).is_err());
        // Two protections of one secret differ.
        assert_ne!(protect(&secret, &id).unwrap(), blob);
    }
}
