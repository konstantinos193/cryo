//! Test doubles for the vault's two ports (AGENTS §10).
//!
//! `Memory` is also what `Vault::session_only` and incognito windows get:
//! a password saved there lives as long as the window does.
//!
//! `Unsealed` is a `Protector` that protects nothing. It exists so the
//! file format can be tested without the OS keyring; production wiring
//! passes `Dpapi`, and `lib.rs` is the only place that chooses.

use std::cell::RefCell;

use super::{Credential, Protector, Store, VaultError};

/// A store that keeps rows for this process only.
#[derive(Default)]
pub struct Memory {
    /// `RefCell` stays inside this module — it never appears in a public
    /// signature (AGENTS §5).
    rows: RefCell<Vec<Credential>>,
}

impl Memory {
    /// What the last save wrote — the assertion seam for tests.
    pub fn saved(&self) -> Vec<Credential> {
        self.rows.borrow().clone()
    }
}

impl Store for Memory {
    fn load(&self) -> Result<Vec<Credential>, VaultError> {
        Ok(self.rows.borrow().clone())
    }

    fn save(&self, rows: &[Credential]) -> Result<(), VaultError> {
        *self.rows.borrow_mut() = rows.to_vec();
        Ok(())
    }
}

/// A protector that hands the bytes straight back. Tests only: it makes
/// the vault file readable by anyone who opens it.
pub struct Unsealed;

impl Protector for Unsealed {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, VaultError> {
        Ok(plain.to_vec())
    }

    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, VaultError> {
        Ok(sealed.to_vec())
    }
}

/// A protector that always fails — the seam for "this file was written
/// by another Windows account".
pub struct Wrongkey;

impl Protector for Wrongkey {
    fn protect(&self, _plain: &[u8]) -> Result<Vec<u8>, VaultError> {
        Err(VaultError::Seal("no key".into()))
    }

    fn unprotect(&self, _sealed: &[u8]) -> Result<Vec<u8>, VaultError> {
        Err(VaultError::Unseal("no key".into()))
    }
}
