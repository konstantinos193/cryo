//! The non-Windows `Protector`, in the DPAPI shape: a random 256-bit
//! key minted once, kept in the OS keychain (Secret Service on Linux,
//! Keychain on macOS), and the vault bytes sealed with
//! XChaCha20-Poly1305. A copied `vault.bin` is inert without this
//! account's keychain, matching what DPAPI gives Windows.
//!
//! Sealed layout: `nonce (24 bytes) || ciphertext+tag`. The nonce is
//! random per save, so there is no counter to persist.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

use super::{Protector, VaultError};

/// Keychain coordinates of the vault key. Not a secret — the pair
/// just names the entry, the way DPAPI's scope is the user account.
const SERVICE: &str = "cryo";
const ACCOUNT: &str = "vault-key";

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;

pub struct Keyring;

impl Keyring {
    /// Fetch the key, minting and storing it on first use. A stored
    /// blob of any other length isn't ours — refuse it rather than
    /// silently rotate under a readable vault.
    fn key(&self) -> Result<Key, VaultError> {
        let entry = ::keyring::Entry::new(SERVICE, ACCOUNT)
            .map_err(|e| VaultError::Seal(format!("keychain: {e}")))?;
        match entry.get_secret() {
            Ok(raw) => Key::try_from(raw.as_slice())
                .map_err(|_| VaultError::Unseal("keychain entry is not a vault key".into())),
            Err(::keyring::Error::NoEntry) => {
                let mut key = Key::default();
                getrandom::getrandom(&mut key)
                    .map_err(|e| VaultError::Seal(format!("entropy: {e}")))?;
                entry
                    .set_secret(&key)
                    .map_err(|e| VaultError::Seal(format!("keychain: {e}")))?;
                Ok(key)
            }
            Err(e) => Err(VaultError::Unseal(format!("keychain: {e}"))),
        }
    }
}

impl Protector for Keyring {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, VaultError> {
        let cipher = XChaCha20Poly1305::new(&self.key()?);
        let mut nonce = XNonce::default();
        getrandom::getrandom(&mut nonce).map_err(|e| VaultError::Seal(format!("entropy: {e}")))?;
        let sealed = cipher
            .encrypt(&nonce, plain)
            .map_err(|_| VaultError::Seal("encryption failed".into()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, VaultError> {
        let (nonce, body) = sealed
            .split_at_checked(NONCE_LEN)
            .ok_or_else(|| VaultError::Unseal("sealed vault is too short".into()))?;
        let cipher = XChaCha20Poly1305::new(&self.key()?);
        cipher
            .decrypt(XNonce::from_slice(nonce), body)
            .map_err(|_| VaultError::Unseal("decryption failed".into()))
    }
}
