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

mod dpapi;
pub mod fake;
mod file;
#[cfg(not(windows))]
mod keyring;

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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub origin: String,
    pub username: String,
    pub secret: String,
    pub note: String,
    pub updated_at: u64,
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

/// The saved sign-ins, in insertion order. One `Vec`, no parallel maps.
#[derive(Default)]
pub struct Credentials {
    rows: Vec<Credential>,
}

impl Credentials {
    /// Hydration door for stored state: the caller has already migrated
    /// the wire format. Duplicate keys collapse, overflow is dropped.
    pub fn hydrate(rows: Vec<Credential>) -> Self {
        let mut out = Self {
            rows: Vec::with_capacity(rows.len().min(MAX_CREDENTIALS)),
        };
        for row in rows.into_iter().take(MAX_CREDENTIALS) {
            if out.find(&row.origin, &row.username).is_none() {
                out.rows.push(row);
            }
        }
        out
    }

    pub fn rows(&self) -> &[Credential] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn find(&self, origin: &str, username: &str) -> Option<&Credential> {
        self.rows.iter().find(|c| c.is(origin, username))
    }

    /// Save one sign-in. An existing row with the same key is replaced —
    /// the vault holds one password per account, the current one.
    pub fn add(&mut self, draft: &Draft) -> Result<Saved, VaultError> {
        let fresh = Credential::hydrate(
            &draft.origin,
            &draft.username,
            Secret::new(draft.secret.clone()),
            &draft.note,
            draft.updated_at,
        )?;
        if let Some(slot) = self
            .rows
            .iter_mut()
            .find(|c| c.is(&fresh.origin, &fresh.username))
        {
            if slot.secret == fresh.secret && slot.note == fresh.note {
                return Ok(Saved::Unchanged);
            }
            *slot = fresh;
            return Ok(Saved::Updated);
        }
        if self.rows.len() >= MAX_CREDENTIALS {
            return Err(VaultError::Full);
        }
        self.rows.push(fresh);
        Ok(Saved::Created)
    }

    /// Bulk import through the same door `add` uses.
    pub fn add_all(&mut self, drafts: &[Draft]) -> MergeReport {
        let mut report = MergeReport::default();
        for draft in drafts {
            match self.add(draft) {
                Ok(Saved::Created) => report.added += 1,
                Ok(Saved::Updated) => report.updated += 1,
                Ok(Saved::Unchanged) => report.duplicates += 1,
                Err(VaultError::Full) => report.overflowed += 1,
                Err(_) => report.rejected += 1,
            }
        }
        report
    }

    /// Forget one sign-in. Reports whether anything was there.
    pub fn remove(&mut self, origin: &str, username: &str) -> bool {
        let before = self.rows.len();
        self.rows.retain(|c| !c.is(origin, username));
        self.rows.len() != before
    }

    /// Case-insensitive match over origin and username. Secrets are
    /// never searched — a match would confirm a guess.
    pub fn matches(&self, needle: &str, limit: usize) -> Vec<&Credential> {
        let needle = needle.trim().to_lowercase();
        self.rows
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.origin.to_lowercase().contains(&needle)
                    || c.username.to_lowercase().contains(&needle)
            })
            .take(limit)
            .collect()
    }
}

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
pub trait Store {
    fn load(&self) -> Result<Vec<Credential>, VaultError>;
    fn save(&self, rows: &[Credential]) -> Result<(), VaultError>;
}

/// What makes the rows unreadable at rest. `Dpapi` is the shipped
/// adapter; `fake::Unsealed` is the test one.
pub trait Protector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, VaultError>;
    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, VaultError>;
}

/// The service the app drives: the credentials stay authoritative in
/// memory, flushed through the `Store` after every mutation.
pub struct Vault {
    rows: Credentials,
    store: Box<dyn Store>,
    /// The load failed and the rows in memory are empty for that reason,
    /// not because the vault is new. Saving would overwrite a file that
    /// may still be good on another account, so writes are refused and
    /// the UI says why.
    sealed_off: Option<String>,
}

impl Vault {
    /// Load what the store has and keep writing back to it.
    ///
    /// An unreadable vault is not an empty vault: unlike history, losing
    /// a password is not recoverable by browsing again, so a failed load
    /// latches writes off instead of healing the file on the next save.
    pub fn open(store: impl Store + 'static) -> Self {
        match store.load() {
            Ok(rows) => Self {
                rows: Credentials::hydrate(rows),
                store: Box::new(store),
                sealed_off: None,
            },
            Err(e) => {
                crate::report!("{e:?}");
                Self {
                    rows: Credentials::default(),
                    store: Box::new(store),
                    sealed_off: Some(e.to_string()),
                }
            }
        }
    }

    /// In-memory only — saves and finds but never persists. The test
    /// seam, and what an incognito window gets.
    pub fn session_only() -> Self {
        Self {
            rows: Credentials::default(),
            store: Box::new(fake::Memory::default()),
            sealed_off: None,
        }
    }

    pub fn credentials(&self) -> &Credentials {
        &self.rows
    }

    /// Why the vault is refusing writes, if it is.
    pub fn locked_out(&self) -> Option<&str> {
        self.sealed_off.as_deref()
    }

    pub fn add(&mut self, draft: &Draft) -> Result<Saved, VaultError> {
        self.writable()?;
        let saved = self.rows.add(draft)?;
        if saved != Saved::Unchanged {
            self.flush()?;
        }
        Ok(saved)
    }

    pub fn add_all(&mut self, drafts: &[Draft]) -> Result<MergeReport, VaultError> {
        self.writable()?;
        let report = self.rows.add_all(drafts);
        if report.added > 0 || report.updated > 0 {
            self.flush()?;
        }
        Ok(report)
    }

    pub fn remove(&mut self, origin: &str, username: &str) -> Result<bool, VaultError> {
        self.writable()?;
        let removed = self.rows.remove(origin, username);
        if removed {
            self.flush()?;
        }
        Ok(removed)
    }

    fn writable(&self) -> Result<(), VaultError> {
        match &self.sealed_off {
            Some(why) => Err(VaultError::Unseal(why.clone())),
            None => Ok(()),
        }
    }

    fn flush(&self) -> Result<(), VaultError> {
        self.store.save(self.rows.rows())
    }
}

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
    }

    #[test]
    fn add_then_find_round_trips() {
        let mut vault = Credentials::default();
        assert_eq!(
            vault.add(&draft("https://a.test", "bob", "pw")).unwrap(),
            Saved::Created
        );
        let found = vault.find("https://a.test", "bob").unwrap();
        assert_eq!(found.secret().expose(), "pw");
    }

    #[test]
    fn origins_match_case_insensitively_usernames_do_not() {
        let mut vault = Credentials::default();
        vault.add(&draft("https://A.test", "bob", "pw")).unwrap();
        assert!(vault.find("https://a.test", "bob").is_some());
        assert!(vault.find("https://a.test", "Bob").is_none());
    }

    #[test]
    fn the_same_account_keeps_one_current_password() {
        let mut vault = Credentials::default();
        vault.add(&draft("https://a.test", "bob", "old")).unwrap();
        assert_eq!(
            vault.add(&draft("https://a.test", "bob", "new")).unwrap(),
            Saved::Updated
        );
        assert_eq!(vault.len(), 1);
        assert_eq!(
            vault
                .find("https://a.test", "bob")
                .unwrap()
                .secret()
                .expose(),
            "new"
        );
        assert_eq!(
            vault.add(&draft("https://a.test", "bob", "new")).unwrap(),
            Saved::Unchanged
        );
    }

    #[test]
    fn two_accounts_on_one_site_both_stay() {
        let mut vault = Credentials::default();
        vault.add(&draft("https://a.test", "bob", "pw")).unwrap();
        vault.add(&draft("https://a.test", "alice", "pw2")).unwrap();
        assert_eq!(vault.len(), 2);
    }

    #[test]
    fn mangled_rows_are_refused() {
        let mut vault = Credentials::default();
        assert!(matches!(
            vault.add(&draft("   ", "bob", "pw")),
            Err(VaultError::NoOrigin)
        ));
        assert!(matches!(
            vault.add(&draft("https://a.test\nhttps://b.test", "bob", "pw")),
            Err(VaultError::Origin(_))
        ));
        assert!(matches!(
            vault.add(&draft("https://a.test", "bob", &"x".repeat(MAX_SECRET + 1))),
            Err(VaultError::SecretLength(_))
        ));
        assert!(vault.is_empty());
    }

    #[test]
    fn an_empty_username_is_a_real_row() {
        let mut vault = Credentials::default();
        vault.add(&draft("https://a.test", "", "pw")).unwrap();
        assert_eq!(vault.len(), 1);
        assert!(vault.find("https://a.test", "").is_some());
    }

    #[test]
    fn android_origins_survive_unchanged() {
        let mut vault = Credentials::default();
        vault
            .add(&draft("android://hash@com.example.app", "bob", "pw"))
            .unwrap();
        assert_eq!(vault.rows()[0].origin(), "android://hash@com.example.app");
    }

    #[test]
    fn add_all_reports_every_outcome() {
        let mut vault = Credentials::default();
        vault.add(&draft("https://a.test", "bob", "pw")).unwrap();
        let report = vault.add_all(&[
            draft("https://a.test", "bob", "pw"),
            draft("https://a.test", "bob", "changed"),
            draft("https://b.test", "ann", "pw"),
            draft("", "nobody", "pw"),
        ]);
        assert_eq!(report.duplicates, 1);
        assert_eq!(report.updated, 1);
        assert_eq!(report.added, 1);
        assert_eq!(report.rejected, 1);
    }

    #[test]
    fn a_full_vault_overflows_instead_of_growing() {
        let mut vault = Credentials::default();
        let drafts: Vec<Draft> = (0..MAX_CREDENTIALS + 3)
            .map(|i| draft(&format!("https://s{i}.test"), "bob", "pw"))
            .collect();
        let report = vault.add_all(&drafts);
        assert_eq!(vault.len(), MAX_CREDENTIALS);
        assert_eq!(report.overflowed, 3);
    }

    #[test]
    fn remove_forgets_one_account_only() {
        let mut vault = Credentials::default();
        vault.add(&draft("https://a.test", "bob", "pw")).unwrap();
        vault.add(&draft("https://a.test", "ann", "pw")).unwrap();
        assert!(vault.remove("https://a.test", "bob"));
        assert!(!vault.remove("https://a.test", "bob"));
        assert_eq!(vault.len(), 1);
    }

    #[test]
    fn matches_never_searches_secrets() {
        let mut vault = Credentials::default();
        vault
            .add(&draft("https://bank.test", "bob", "swordfish"))
            .unwrap();
        assert_eq!(vault.matches("bank", 5).len(), 1);
        assert_eq!(vault.matches("bob", 5).len(), 1);
        assert_eq!(vault.matches("swordfish", 5).len(), 0);
        assert_eq!(vault.matches("", 5).len(), 1);
    }

    #[test]
    fn hydrate_collapses_stored_duplicate_keys() {
        let rows = vec![
            Credential::hydrate("https://a.test", "bob", Secret::new("a"), "", 1).unwrap(),
            Credential::hydrate("https://a.test", "bob", Secret::new("b"), "", 2).unwrap(),
        ];
        let vault = Credentials::hydrate(rows);
        assert_eq!(vault.len(), 1);
        assert_eq!(vault.rows()[0].secret().expose(), "a");
    }

    #[test]
    fn a_vault_that_failed_to_open_refuses_to_overwrite_itself() {
        /// A store that cannot be read but would happily be written.
        struct Broken;
        impl Store for Broken {
            fn load(&self) -> Result<Vec<Credential>, VaultError> {
                Err(VaultError::Unseal("wrong user".into()))
            }
            fn save(&self, _rows: &[Credential]) -> Result<(), VaultError> {
                panic!("a locked-out vault must not write");
            }
        }
        let mut vault = Vault::open(Broken);
        assert!(vault.locked_out().is_some());
        assert!(matches!(
            vault.add(&draft("https://a.test", "bob", "pw")),
            Err(VaultError::Unseal(_))
        ));
    }

    #[test]
    fn session_only_saves_but_writes_nowhere() {
        let mut vault = Vault::session_only();
        vault.add(&draft("https://a.test", "bob", "pw")).unwrap();
        assert_eq!(vault.credentials().len(), 1);
        assert!(vault.locked_out().is_none());
    }
}
