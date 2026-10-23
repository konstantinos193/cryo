//! Windows DPAPI adapter: the shipped `Protector`.
//!
//! `CryptProtectData` wraps the bytes under a key derived from the
//! signed-in Windows account, so the vault file is inert if it is copied
//! to another machine or opened under another user. The secondary
//! entropy below means a blob from another program's DPAPI call cannot
//! be swapped in and vice versa.
//!
//! This is the one file in the context that names Windows types — the
//! domain declares `Protector` and never learns who implements it
//! (AGENTS §3).

#![cfg(windows)]

use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

use super::{Protector, VaultError};

/// Secondary entropy: ties a blob to this program and this format.
/// Changing it makes every existing vault unreadable, so it is part of
/// the file format — bump `CURRENT_VERSION` with it.
const ENTROPY: &[u8] = b"cryo.vault.v1";

/// The user-bound protector. Holds nothing: the key lives with Windows.
pub struct Dpapi;

impl Protector for Dpapi {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, VaultError> {
        // SAFETY: both blobs point at live slices for the duration of
        // the call, and `out` is read only after a success — the only
        // case in which DPAPI wrote a buffer for us to free.
        unsafe {
            let input = blob(plain);
            let entropy = blob(ENTROPY);
            let mut out = CRYPT_INTEGER_BLOB::default();
            CryptProtectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(|e| VaultError::Seal(e.message()))?;
            Ok(take(out))
        }
    }

    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, VaultError> {
        // SAFETY: as above. A wrong user, a tampered blob or mismatched
        // entropy all surface as an `Err` here, never as a bad read.
        unsafe {
            let input = blob(sealed);
            let entropy = blob(ENTROPY);
            let mut out = CRYPT_INTEGER_BLOB::default();
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(|e| VaultError::Unseal(e.message()))?;
            Ok(take(out))
        }
    }
}

/// Point a DPAPI blob at a slice we own for the length of the call.
fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    }
}

/// Copy a blob DPAPI allocated and release it. The copy is the only
/// thing that outlives this function; leaking the `LocalAlloc` buffer
/// would leak plaintext on the unprotect path.
///
/// SAFETY: only called on a blob a successful DPAPI call filled in.
unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let bytes = if out.pbData.is_null() {
        Vec::new()
    } else {
        std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec()
    };
    if !out.pbData.is_null() {
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_os_keyring() {
        let sealed = Dpapi.protect(b"hunter2").unwrap();
        assert_ne!(sealed, b"hunter2");
        // DPAPI output carries no plaintext run of the input.
        assert!(!sealed.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(Dpapi.unprotect(&sealed).unwrap(), b"hunter2");
    }

    #[test]
    fn empty_input_round_trips() {
        let sealed = Dpapi.protect(b"").unwrap();
        assert_eq!(Dpapi.unprotect(&sealed).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn a_tampered_blob_fails_instead_of_decoding() {
        let mut sealed = Dpapi.protect(b"hunter2").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert!(matches!(
            Dpapi.unprotect(&sealed),
            Err(VaultError::Unseal(_))
        ));
    }

    #[test]
    fn garbage_is_rejected_not_guessed() {
        assert!(matches!(
            Dpapi.unprotect(b"not a dpapi blob"),
            Err(VaultError::Unseal(_))
        ));
    }
}
