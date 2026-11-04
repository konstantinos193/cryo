//! The disk cache: the only place mailbox state lives.
//!
//! Per account and folder an `index.json` holds the newest
//! [`MAX_KEPT`] envelopes plus the `uidvalidity` they were fetched
//! under — a changed validity drops the file, since uids from another
//! generation name different messages. Opened messages land raw (the
//! RFC822 bytes) under `raw/` — parsing and sanitizing are cheap and
//! redone per open, so the cache serves replies and attachment saves
//! without a second copy to keep in step.
//!
//! The file's row is a DTO (AGENTS §6): `EnvelopeRow` serializes, the
//! domain `Envelope` does not, and the only translation lives here.
//! Everything is called from the worker thread — the event loop never
//! touches these paths.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Envelope, MailError};

/// How many envelopes one folder keeps on disk — and therefore the
/// largest resident listing the app ever holds.
pub const MAX_KEPT: usize = 500;

/// The index version `IndexFile` writes and reads.
pub(crate) const CURRENT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeRow {
    uid: u32,
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    in_reply_to: String,
    #[serde(default)]
    references: String,
    from: String,
    to: String,
    #[serde(default)]
    cc: String,
    subject: String,
    date: i64,
    size: u32,
    #[serde(default)]
    seen: bool,
    #[serde(default)]
    flagged: bool,
    #[serde(default)]
    answered: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    deleted: bool,
}

impl From<&Envelope> for EnvelopeRow {
    fn from(e: &Envelope) -> Self {
        Self {
            uid: e.uid,
            message_id: e.message_id.clone(),
            in_reply_to: e.in_reply_to.clone(),
            references: e.references.clone(),
            from: e.from.clone(),
            to: e.to.clone(),
            cc: e.cc.clone(),
            subject: e.subject.clone(),
            date: e.date,
            size: e.size,
            seen: e.seen,
            flagged: e.flagged,
            answered: e.answered,
            draft: e.draft,
            deleted: e.deleted,
        }
    }
}

impl From<EnvelopeRow> for Envelope {
    fn from(r: EnvelopeRow) -> Self {
        Self {
            uid: r.uid,
            message_id: r.message_id,
            in_reply_to: r.in_reply_to,
            references: r.references,
            from: r.from,
            to: r.to,
            cc: r.cc,
            subject: r.subject,
            date: r.date,
            size: r.size,
            seen: r.seen,
            flagged: r.flagged,
            answered: r.answered,
            draft: r.draft,
            deleted: r.deleted,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexFile {
    version: u32,
    uid_validity: u32,
    envelopes: Vec<EnvelopeRow>,
}

/// What one folder remembers between syncs.
pub struct Index {
    pub uid_validity: u32,
    /// Newest first — uid order, which is arrival order.
    pub envelopes: Vec<Envelope>,
}

/// A folder-name as a path segment — IMAP names carry `/` separators
/// and the occasional `*`; the cache file needs neither.
fn safe(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Dots and blanks are kept for readability, which means ".", ".."
    // and friends survive the map — those become a flat "_".
    if mapped.trim_matches(['.', ' ']).is_empty() {
        "_".into()
    } else {
        mapped
    }
}

/// The whole cache: one directory under the profile root.
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    /// `root` is `data_dir()/mail` — the composition root names it.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn account_dir(&self, account: &str) -> PathBuf {
        self.root.join(safe(account))
    }

    fn index_path(&self, account: &str, folder: &str) -> PathBuf {
        self.account_dir(account)
            .join(format!("{}.json", safe(folder)))
    }

    fn raw_path(&self, account: &str, folder: &str, uid: u32) -> PathBuf {
        self.account_dir(account)
            .join("raw")
            .join(safe(folder))
            .join(format!("{uid}.eml"))
    }

    /// The cached listing for a folder — `None` on a miss, a stale
    /// generation or a damaged file (the next sync refills all three).
    pub fn load_index(&self, account: &str, folder: &str, uid_validity: u32) -> Option<Index> {
        let path = self.index_path(account, folder);
        let bytes = crate::read_file(&path).ok()??;
        let file: IndexFile = serde_json::from_slice(&bytes).ok()?;
        if file.version != CURRENT_VERSION || file.uid_validity != uid_validity {
            return None;
        }
        Some(Index {
            uid_validity,
            envelopes: file.envelopes.into_iter().map(Into::into).collect(),
        })
    }

    /// Read the index without knowing the current validity — the
    /// folder-open path shows this while the sync is still asking the
    /// server what the generation is.
    pub fn peek_index(&self, account: &str, folder: &str) -> Option<Index> {
        let path = self.index_path(account, folder);
        let bytes = crate::read_file(&path).ok()??;
        let file: IndexFile = serde_json::from_slice(&bytes).ok()?;
        if file.version != CURRENT_VERSION {
            return None;
        }
        Some(Index {
            uid_validity: file.uid_validity,
            envelopes: file.envelopes.into_iter().map(Into::into).collect(),
        })
    }

    /// Replace a folder's listing — the sync fetched the newest window
    /// wholesale, so this is a write, not a merge. Capped here so a
    /// caller can't grow the file past what the app may hold.
    pub fn save_index(
        &self,
        account: &str,
        folder: &str,
        uid_validity: u32,
        mut envelopes: Vec<Envelope>,
    ) -> Result<(), MailError> {
        envelopes.truncate(MAX_KEPT);
        let file = IndexFile {
            version: CURRENT_VERSION,
            uid_validity,
            envelopes: envelopes.iter().map(Into::into).collect(),
        };
        let bytes = serde_json::to_vec(&file)
            .map_err(|e| MailError::io("encode", &self.index_path(account, folder), e))?;
        crate::write_file_atomic(&self.index_path(account, folder), &bytes)
            .map_err(|e| MailError::io("write", &self.index_path(account, folder), e))
    }

    /// The raw message as fetched — `None` when it was never opened.
    pub fn load_raw(&self, account: &str, folder: &str, uid: u32) -> Option<Vec<u8>> {
        crate::read_file(&self.raw_path(account, folder, uid)).ok()?
    }

    pub fn save_raw(
        &self,
        account: &str,
        folder: &str,
        uid: u32,
        bytes: &[u8],
    ) -> Result<(), MailError> {
        crate::write_file_atomic(&self.raw_path(account, folder, uid), bytes)
            .map_err(|e| MailError::io("write", &self.raw_path(account, folder, uid), e))
    }

    /// An account went away — its cache goes with it.
    pub fn drop_account(&self, account: &str) -> Result<(), MailError> {
        let dir = self.account_dir(account);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(MailError::io("remove", &dir, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(uid: u32) -> Envelope {
        Envelope {
            uid,
            from: "ann@a.test".into(),
            subject: format!("s{uid}"),
            date: 1_700_000_000 + u64::from(uid) as i64,
            size: 10,
            ..Envelope::default()
        }
    }

    #[test]
    fn index_round_trips_and_validity_is_a_generation() {
        let dir = crate::test_dir("mail-store");
        let store = FileStore::new(dir.clone());
        store
            .save_index("a@x", "INBOX", 7, vec![env(2), env(1)])
            .unwrap();
        assert_eq!(
            store.load_index("a@x", "INBOX", 7).unwrap().envelopes[0].uid,
            2
        );
        assert!(store.load_index("a@x", "INBOX", 8).is_none());
        assert!(store.load_index("a@x", "missing", 7).is_none());
        // peek ignores the generation — the folder-open path needs it.
        assert_eq!(store.peek_index("a@x", "INBOX").unwrap().uid_validity, 7);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn names_from_the_wire_cannot_escape_the_root() {
        for hostile in ["../x", "a/b\\c", "..", "*", "[Gmail]/All Mail"] {
            let s = safe(hostile);
            assert!(!s.contains(['/', '\\']), "{hostile} -> {s}");
            assert_ne!(s, "..", "{hostile}");
        }
        let dir = crate::test_dir("mail-store-escape");
        let store = FileStore::new(dir.clone());
        let path = store.index_path("a@x", "../evil");
        assert!(path.starts_with(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn raw_round_trips_and_caps_apply_to_indexes() {
        let dir = crate::test_dir("mail-raw");
        let store = FileStore::new(dir.clone());
        store.save_raw("a@x", "INBOX", 5, b"raw bytes").unwrap();
        assert_eq!(store.load_raw("a@x", "INBOX", 5).unwrap(), b"raw bytes");
        assert!(store.load_raw("a@x", "INBOX", 6).is_none());
        let many: Vec<Envelope> = (0..MAX_KEPT as u32 + 50).map(env).collect();
        store.save_index("a@x", "INBOX", 1, many).unwrap();
        assert_eq!(
            store.load_index("a@x", "INBOX", 1).unwrap().envelopes.len(),
            MAX_KEPT
        );
        store.drop_account("a@x").unwrap();
        assert!(store.load_index("a@x", "INBOX", 1).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
