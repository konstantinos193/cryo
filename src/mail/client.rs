//! The client port: everything the domain needs from an IMAP server,
//! in our own types. `imap_impl` is the shipped adapter; a second impl
//! is what this seam exists for (AGENTS §4 — the domain names what it
//! needs, plumbing fills it in). Every call is blocking and belongs on
//! the worker thread.

use super::{Account, Envelope, Folder, MailError};
use crate::vault::Secret;

/// One flag a `set_flag` call adds or removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    Seen,
    Flagged,
    Answered,
    Draft,
    Deleted,
}

impl Flag {
    /// The IMAP atom this maps to.
    pub fn atom(self) -> &'static str {
        match self {
            Self::Seen => "\\Seen",
            Self::Flagged => "\\Flagged",
            Self::Answered => "\\Answered",
            Self::Draft => "\\Draft",
            Self::Deleted => "\\Deleted",
        }
    }
}

/// The `uidvalidity` a `select` reports — an index cached under a
/// different generation is dropped, since uids from another
/// generation name different messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub uid_validity: u32,
    /// `EXISTS` — the row count, for the folder line in the UI.
    pub exists: u32,
    /// `UNSEEN`, when the server reported it.
    pub unseen: Option<u32>,
}

/// What the domain asks of a server. All blocking, all on the worker.
pub trait Client {
    /// Prove the credentials work — the onboarding flow's gate.
    fn verify(&mut self) -> Result<(), MailError>;

    /// `LIST` — the account's folders, roles resolved.
    fn folders(&mut self) -> Result<Vec<Folder>, MailError>;

    /// `SELECT` — the folder's generation and counts.
    fn select(&mut self, folder: &str) -> Result<Selection, MailError>;

    /// The newest `limit` envelopes of the selected folder, newest
    /// first — one `FETCH` of a seq range, flags included.
    fn envelopes(&mut self, limit: u32) -> Result<Vec<Envelope>, MailError>;

    /// The full RFC822 bytes of one message, without marking it seen
    /// (`BODY.PEEK[]`) — the app's own flag write is what marks it.
    fn raw(&mut self, uid: u32) -> Result<Vec<u8>, MailError>;

    /// Add or clear a flag on one message.
    fn set_flag(&mut self, uid: u32, flag: Flag, on: bool) -> Result<(), MailError>;

    /// Move a message between mailboxes — `MOVE` when the server
    /// offers it, `COPY` + `\Deleted` + `EXPUNGE` otherwise.
    fn moveto(&mut self, uid: u32, target: &str) -> Result<(), MailError>;

    /// `\Deleted` + `EXPUNGE`.
    fn delete(&mut self, uid: u32) -> Result<(), MailError>;

    /// `APPEND` — the Sent copy after SMTP accept, or a saved draft.
    fn append(&mut self, folder: &str, rfc822: &[u8]) -> Result<(), MailError>;
}

/// Connect and log in — the one construction site for the shipped
/// client, so TLS setup lives in the adapter.
pub fn connect(account: &Account, secret: &Secret) -> Result<Box<dyn Client>, MailError> {
    super::imap_impl::connect(account, secret)
}
