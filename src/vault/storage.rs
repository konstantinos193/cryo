//! Vault persistence service: loading and saving the in-memory aggregate.
use super::*;

/// The service the app drives: the credentials stay authoritative in
/// memory, flushed through the `Store` after every mutation.
pub struct Vault {
    rows: Credentials,
    store: Box<dyn Store>,
    writer: Option<crate::persist::worker::WriteQueue<Credential>>,
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
                writer: None,
                sealed_off: None,
            },
            Err(e) => {
                crate::report!("{e:?}");
                Self {
                    rows: Credentials::default(),
                    store: Box::new(store),
                    writer: None,
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
            writer: None,
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
        if let Some(writer) = &self.writer {
            writer.submit(self.rows.rows().to_vec());
            Ok(())
        } else {
            self.store.save(self.rows.rows())
        }
    }

    pub(crate) fn background(
        &mut self,
        failed: impl Fn(String) + Send + 'static,
    ) -> std::io::Result<crate::persist::worker::WriterShutdown> {
        let store = std::mem::replace(&mut self.store, Box::new(fake::Memory::default()));
        let (writer, shutdown) = crate::persist::worker::WriteQueue::spawn(
            move |rows| store.save(rows).map_err(|e| e.to_string()),
            failed,
        )?;
        self.writer = Some(writer);
        Ok(shutdown)
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
