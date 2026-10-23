//! File adapter: the vault as one sealed document on disk.
//!
//! Layout is deliberately boring — a magic string, a version, then one
//! opaque blob from the `Protector`:
//!
//! ```text
//! "cryovault" | version: u32 LE | sealed(JSON of the rows)
//! ```
//!
//! The version sits *outside* the seal so a future format can be
//! recognised without a key, and the DTOs inside live here and only here
//! — `Credential` never derives serde (AGENTS §6). Nothing readable
//! reaches the disk: origins and usernames are inside the seal too,
//! because a list of the sites someone has accounts on is itself worth
//! protecting.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Credential, Protector, Secret, Store, VaultError, CURRENT_VERSION};

/// First bytes of a cryo vault. A file that does not start with these is
/// refused rather than handed to the protector.
const MAGIC: &[u8] = b"cryovault";

/// Length of the plaintext header: magic plus the version word.
const HEADER: usize = MAGIC.len() + 4;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultBody {
    rows: Vec<RowFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RowFile {
    origin: String,
    username: String,
    secret: String,
    note: String,
    updated_at: u64,
}

impl From<&Credential> for RowFile {
    fn from(c: &Credential) -> Self {
        Self {
            origin: c.origin().to_string(),
            username: c.username().to_string(),
            // Inside the seal. This is the one place a secret is written.
            secret: c.secret().expose().to_string(),
            note: c.note().to_string(),
            updated_at: c.updated_at(),
        }
    }
}

/// The vault as one sealed file — the store the real build uses.
pub struct SealedFile {
    path: PathBuf,
    protector: Box<dyn Protector>,
}

impl SealedFile {
    /// The protector is injected, never chosen here: `lib.rs` wires
    /// `Dpapi`, tests wire `fake::Unsealed`.
    pub fn new(path: PathBuf, protector: impl Protector + 'static) -> Self {
        Self {
            path,
            protector: Box::new(protector),
        }
    }

    /// `vault.bin` under the per-user data dir (`crate::data_dir`),
    /// beside the history and bookmark files.
    pub fn default_path() -> PathBuf {
        crate::data_dir().join("vault.bin")
    }

    fn read_err(&self, source: std::io::Error) -> VaultError {
        VaultError::Read {
            path: self.path.display().to_string(),
            source,
        }
    }

    fn write_err(&self, source: std::io::Error) -> VaultError {
        VaultError::Write {
            path: self.path.display().to_string(),
            source,
        }
    }
}

impl Store for SealedFile {
    fn load(&self) -> Result<Vec<Credential>, VaultError> {
        // A missing file is a fresh profile, not an error.
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(self.read_err(e)),
        };
        if bytes.len() < HEADER || !bytes.starts_with(MAGIC) {
            return Err(VaultError::NotAVault);
        }
        let version = u32::from_le_bytes([
            bytes[MAGIC.len()],
            bytes[MAGIC.len() + 1],
            bytes[MAGIC.len() + 2],
            bytes[MAGIC.len() + 3],
        ]);
        // The wire is versioned, not the domain: an older payload would
        // be migrated here, before anything is hydrated.
        if version != CURRENT_VERSION {
            return Err(VaultError::Version(version));
        }
        let mut plain = self.protector.unprotect(&bytes[HEADER..])?;
        let body: Result<VaultBody, _> = serde_json::from_slice(&plain);
        plain.fill(0);
        let body = body.map_err(VaultError::Parse)?;
        // Hydration goes through the domain's one door; a row this build
        // refuses is dropped rather than failing the whole vault.
        Ok(body
            .rows
            .into_iter()
            .filter_map(|r| {
                Credential::hydrate(
                    &r.origin,
                    &r.username,
                    Secret::new(r.secret),
                    &r.note,
                    r.updated_at,
                )
                .ok()
            })
            .collect())
    }

    fn save(&self, rows: &[Credential]) -> Result<(), VaultError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| self.write_err(e))?;
        }
        let body = VaultBody {
            rows: rows.iter().map(RowFile::from).collect(),
        };
        let mut plain = serde_json::to_vec(&body).map_err(VaultError::Parse)?;
        let sealed = self.protector.protect(&plain);
        // Best effort, not a guarantee: the allocator may already have
        // copied these bytes. It still keeps the plaintext from sitting
        // in a freed page for the rest of the session.
        plain.fill(0);
        let sealed = sealed?;

        let mut out = Vec::with_capacity(HEADER + sealed.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&CURRENT_VERSION.to_le_bytes());
        out.extend_from_slice(&sealed);
        // Write-then-rename: a crash mid-write leaves the old vault whole.
        let tmp = self.path.with_extension("bin.tmp");
        std::fs::write(&tmp, &out).map_err(|e| self.write_err(e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| self.write_err(e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::{Unsealed, Wrongkey};
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("cryo-vault-{tag}-{}", std::process::id()))
            .join("vault.bin")
    }

    fn row(origin: &str, user: &str, secret: &str) -> Credential {
        Credential::hydrate(origin, user, Secret::new(secret), "note", 7).unwrap()
    }

    #[test]
    fn missing_file_loads_empty() {
        let store = SealedFile::new(temp("missing"), Unsealed);
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn save_then_load_round_trips_every_field() {
        let path = temp("round");
        let store = SealedFile::new(path.clone(), Unsealed);
        let rows = vec![row("https://a.test", "bob", "pw")];
        store.save(&rows).unwrap();
        let back = store.load().unwrap();
        assert_eq!(back, rows);
        assert_eq!(back[0].secret().expose(), "pw");
        assert_eq!(back[0].note(), "note");
        assert_eq!(back[0].updated_at(), 7);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_header_is_plaintext_and_the_body_is_not() {
        let path = temp("header");
        let store = SealedFile::new(path.clone(), Unsealed);
        store.save(&[row("https://a.test", "bob", "pw")]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(MAGIC));
        assert_eq!(
            u32::from_le_bytes(bytes[MAGIC.len()..HEADER].try_into().unwrap()),
            CURRENT_VERSION
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_foreign_file_is_refused_before_the_protector_sees_it() {
        let path = temp("foreign");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"this is not a vault at all").unwrap();
        let store = SealedFile::new(path.clone(), Unsealed);
        assert!(matches!(store.load(), Err(VaultError::NotAVault)));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_truncated_file_is_refused() {
        let path = temp("short");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"cryo").unwrap();
        let store = SealedFile::new(path.clone(), Unsealed);
        assert!(matches!(store.load(), Err(VaultError::NotAVault)));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_future_version_is_an_error_not_a_guess() {
        let path = temp("ver");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&99u32.to_le_bytes());
        bytes.extend_from_slice(b"{}");
        std::fs::write(&path, &bytes).unwrap();
        let store = SealedFile::new(path.clone(), Unsealed);
        assert!(matches!(store.load(), Err(VaultError::Version(99))));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_vault_from_another_account_fails_to_unseal() {
        let path = temp("wrongkey");
        SealedFile::new(path.clone(), Unsealed)
            .save(&[row("https://a.test", "bob", "pw")])
            .unwrap();
        let theirs = SealedFile::new(path.clone(), Wrongkey);
        assert!(matches!(theirs.load(), Err(VaultError::Unseal(_))));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unknown_fields_in_the_body_are_rejected() {
        let path = temp("unknown");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&CURRENT_VERSION.to_le_bytes());
        bytes.extend_from_slice(br#"{"rows":[],"mystery":1}"#);
        std::fs::write(&path, &bytes).unwrap();
        let store = SealedFile::new(path.clone(), Unsealed);
        assert!(matches!(store.load(), Err(VaultError::Parse(_))));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_stored_row_the_domain_refuses_is_dropped_not_fatal() {
        let path = temp("refused");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&CURRENT_VERSION.to_le_bytes());
        bytes.extend_from_slice(
            br#"{"rows":[
                 {"origin":"https://a.test","username":"bob","secret":"pw","note":"","updated_at":1},
                 {"origin":"","username":"nobody","secret":"pw","note":"","updated_at":1}
               ]}"#,
        );
        std::fs::write(&path, &bytes).unwrap();
        let rows = SealedFile::new(path.clone(), Unsealed).load().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].origin(), "https://a.test");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn the_shipped_pair_keeps_the_secret_off_the_disk() {
        use super::super::Dpapi;
        let path = temp("dpapi");
        let store = SealedFile::new(path.clone(), Dpapi);
        store
            .save(&[row("https://bank.test", "bob", "swordfish")])
            .unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(9).any(|w| w == b"swordfish"));
        // The site list is inside the seal too, not just the password.
        assert!(!raw.windows(9).any(|w| w == b"bank.test"));
        let back = SealedFile::new(path.clone(), Dpapi).load().unwrap();
        assert_eq!(back[0].secret().expose(), "swordfish");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn default_path_lands_in_the_profile_folder() {
        let path = SealedFile::default_path();
        assert_eq!(path.file_name().unwrap(), "vault.bin");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "cryo");
    }
}
