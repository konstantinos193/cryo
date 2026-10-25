//! Credentials aggregate: bounded collection, queries and mutations.
use super::*;

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

    /// Saved sign-ins whose canonical web origin equals `page`.
    pub fn for_page(&self, page: &SiteOrigin) -> Vec<&Credential> {
        autofill::for_page(self, page)
    }

    /// Draft → `Credential`: the validation half of `add`, shared with
    /// `add_all`.
    fn draft_credential(&self, draft: &Draft) -> Result<Credential, VaultError> {
        Credential::hydrate(
            &draft.origin,
            &draft.username,
            Secret::new(draft.secret.clone()),
            &draft.note,
            draft.updated_at,
        )
    }

    /// Store a validated row: replace in place when `pos` names the
    /// same account (reporting whether anything actually changed), else
    /// append under the bound. `add` and `add_all` share this once the
    /// key lookup is done.
    fn store_valid(&mut self, fresh: Credential, pos: Option<usize>) -> Result<Saved, VaultError> {
        match pos {
            Some(i) => {
                let slot = &mut self.rows[i];
                if slot.secret == fresh.secret && slot.note == fresh.note {
                    return Ok(Saved::Unchanged);
                }
                *slot = fresh;
                Ok(Saved::Updated)
            }
            None => {
                if self.rows.len() >= MAX_CREDENTIALS {
                    return Err(VaultError::Full);
                }
                self.rows.push(fresh);
                Ok(Saved::Created)
            }
        }
    }

    /// Save one sign-in. An existing row with the same key is replaced —
    /// the vault holds one password per account, the current one.
    pub fn add(&mut self, draft: &Draft) -> Result<Saved, VaultError> {
        let fresh = self.draft_credential(draft)?;
        let pos = self
            .rows
            .iter()
            .position(|c| c.is(&fresh.origin, &fresh.username));
        self.store_valid(fresh, pos)
    }

    /// Bulk import through the same doors `add` uses — `draft_credential`
    /// validates, `store_valid` stores — but the account lookup runs
    /// against a position index built once, so the import costs its own
    /// size, not its size squared.
    pub fn add_all(&mut self, drafts: &[Draft]) -> MergeReport {
        let mut report = MergeReport::default();
        let mut pos: std::collections::HashMap<(String, String), usize> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, c)| (cred_key(c), i))
            .collect();
        for draft in drafts {
            let fresh = match self.draft_credential(draft) {
                Ok(f) => f,
                Err(_) => {
                    report.rejected += 1;
                    continue;
                }
            };
            let key = cred_key(&fresh);
            match self.store_valid(fresh, pos.get(&key).copied()) {
                Ok(Saved::Created) => {
                    pos.insert(key, self.rows.len() - 1);
                    report.added += 1;
                }
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
}
