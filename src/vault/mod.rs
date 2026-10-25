//! The vault: passwords the user asked cryo to keep.
//!
//! `Credentials` is the aggregate — keyed by (origin, username), bounded,
//! and built in exactly one place (`Credential::hydrate`), so a stored
//! row and an imported CSV row arrive through the same validation
//! (AGENTS §6). `Vault` is the service the app drives.
//!
//! Two ports: `Store` is where the rows live, `Protector` is what makes
//! them unreadable at rest. The shipped pair is `SealedFile` + `Dpapi`
//! on Windows (the key tied to the signed-in account), `SealedFile` +
//! `Keyring` elsewhere (the key in the OS keychain) — a copied file is
//! inert on another account either way. Nothing here ever prints a
//! secret: `Secret`'s
//! `Debug` is redacted and there is no `Display`, because the one
//! reporting seam (`crate::report!`) formats with `Debug`.

use zeroize::Zeroize;

mod autofill;
mod dpapi;
pub mod fake;
mod file;
#[cfg(not(windows))]
mod keyring;

pub use autofill::SiteOrigin;
#[cfg(windows)]
pub use dpapi::Dpapi;
pub use file::SealedFile;
#[cfg(not(windows))]
pub use keyring::Keyring;

/// A password, kept apart from every other string in the program.
///
/// No `Display`, no derived `Debug`, no `Deref<Target = str>`: reading it
/// out takes `expose`, which is easy to grep for and rare on purpose.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// Hand over the plaintext. Call sites: the file store on save, the
    /// CSV exporter, and the explicit reveal the user clicks.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Characters, not bytes — the dots the UI shows stand for glyphs.
    pub fn len(&self) -> usize {
        self.0.chars().count()
    }
}

impl std::fmt::Debug for Secret {
    /// Redacted on purpose: a stray `{:?}` of a `Credential` in a log or
    /// an error must not be the line that leaks a password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(redacted, {} chars)", self.len())
    }
}

impl Drop for Secret {
    /// Plaintext doesn't linger in freed heap: every dropped copy wipes
    /// its bytes — best effort, same as the file adapter's buffer.
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// One saved sign-in. Immutable: a changed password replaces the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    /// Where it signs in. Usually `https://host`; Chromium exports also
    /// carry `android://…` rows, which are kept verbatim so a later
    /// export hands them back unchanged.
    origin: String,
    /// May be empty — some sites have a password and nothing else.
    username: String,
    secret: Secret,
    note: String,
    /// Unix seconds, stamped by the caller.
    updated_at: u64,
}

/// A credential as a foreign source proposes it: unvalidated strings.
/// Importers build these on a worker thread, so this stays plain data.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub origin: String,
    pub username: String,
    pub secret: String,
    pub note: String,
    pub updated_at: u64,
}

impl std::fmt::Debug for Draft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Draft")
            .field("origin", &self.origin)
            .field("username", &self.username)
            .field("secret", &"redacted")
            .field("note", &self.note)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl Drop for Draft {
    /// Carries plaintext like a `Secret` does — wipe it the same way.
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

/// What `add_all` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Sign-ins that were not in the vault before.
    pub added: usize,
    /// Same origin and username, different password — the newer wins.
    pub updated: usize,
    /// Byte-identical to a row already kept.
    pub duplicates: usize,
    /// Rows that failed validation (no origin, control characters).
    pub rejected: usize,
    /// Rows dropped because the vault is full.
    pub overflowed: usize,
}

/// Hard bound on kept credentials.
const MAX_CREDENTIALS: usize = 5_000;

const MAX_ORIGIN: usize = 2_048;
const MAX_USERNAME: usize = 300;
const MAX_SECRET: usize = 1_000;
const MAX_NOTE: usize = 1_000;

/// The wire version `SealedFile` writes and reads.
pub(crate) const CURRENT_VERSION: u32 = 1;

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum VaultError {
    #[error("a credential needs an origin")]
    NoOrigin,
    #[error("{0} is not a usable origin")]
    Origin(String),
    #[error("a password that long is not a password ({0} characters)")]
    SecretLength(usize),
    #[error("the vault is full ({MAX_CREDENTIALS} kept)")]
    Full,
    #[error("reading {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing the vault: {0}")]
    Parse(#[source] serde_json::Error),
    #[error("the vault file is not a cryo vault")]
    NotAVault,
    #[error("vault file has version {0}, this build reads {CURRENT_VERSION}")]
    Version(u32),
    /// Sealing failed — the OS refused to protect the bytes.
    #[error("sealing the vault failed: {0}")]
    Seal(String),
    /// Unsealing failed: another Windows account, a restored profile, or
    /// a damaged file. The message never carries ciphertext.
    #[error("unsealing the vault failed: {0}")]
    Unseal(String),
}

impl Credential {
    /// The one door (AGENTS §6). Stored rows, imported CSV rows and a
    /// password the user typed all land here.
    fn hydrate(
        origin: &str,
        username: &str,
        secret: Secret,
        note: &str,
        updated_at: u64,
    ) -> Result<Self, VaultError> {
        let origin = origin.trim();
        if origin.is_empty() {
            return Err(VaultError::NoOrigin);
        }
        if origin.len() > MAX_ORIGIN || has_controls(origin) {
            return Err(VaultError::Origin(truncate(origin, 40)));
        }
        if secret.len() > MAX_SECRET {
            return Err(VaultError::SecretLength(secret.len()));
        }
        Ok(Self {
            origin: origin.to_string(),
            username: truncate(username.trim(), MAX_USERNAME),
            secret,
            note: truncate(note.trim(), MAX_NOTE),
            updated_at,
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    /// The secret itself. Callers that only show a row want `secret_len`.
    pub fn secret(&self) -> &Secret {
        &self.secret
    }

    pub fn note(&self) -> &str {
        &self.note
    }

    pub fn updated_at(&self) -> u64 {
        self.updated_at
    }

    /// Same sign-in? Origins compare case-insensitively (hosts are),
    /// usernames exactly (`Bob` and `bob` are two accounts).
    pub fn is(&self, origin: &str, username: &str) -> bool {
        self.origin.eq_ignore_ascii_case(origin.trim()) && self.username == username.trim()
    }
}

/// The account identity `is()` expresses, as a hashable value — the
/// lowercased origin plus the exact username — for `add_all`'s index.
fn cred_key(c: &Credential) -> (String, String) {
    (c.origin.to_ascii_lowercase(), c.username.clone())
}

/// Newlines and NULs in an origin mean a mangled CSV, not a site.
fn has_controls(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => s[..cut].to_string(),
        None => s.to_string(),
    }
}

mod credentials;
pub use credentials::Credentials;

/// What `add` did to the vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    Created,
    Updated,
    /// The same password was already there — nothing to write.
    Unchanged,
}

/// Where the rows live. `SealedFile` is the shipped adapter;
/// `fake::Memory` is the test one.
pub trait Store: Send {
    fn load(&self) -> Result<Vec<Credential>, VaultError>;
    fn save(&self, rows: &[Credential]) -> Result<(), VaultError>;
}

/// What makes the rows unreadable at rest. `Dpapi` is the shipped
/// adapter; `fake::Unsealed` is the test one.
pub trait Protector: Send {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, VaultError>;
    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, VaultError>;
}

mod storage;
pub use storage::Vault;

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(origin: &str, user: &str, secret: &str) -> Draft {
        Draft {
            origin: origin.to_string(),
            username: user.to_string(),
            secret: secret.to_string(),
            note: String::new(),
            updated_at: 10,
        }
    }

    #[test]
    fn secret_never_prints_itself() {
        let s = Secret::new("hunter2");
        let shown = format!("{s:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert_eq!(shown, "Secret(redacted, 7 chars)");
        // A whole credential formats through the same guard.
        let c = Credential::hydrate("https://a.test", "bob", s, "", 1).unwrap();
        assert!(!format!("{c:?}").contains("hunter2"));
        let proposed = draft("https://a.test", "bob", "hunter2");
        assert!(!format!("{proposed:?}").contains("hunter2"));
    }
}
