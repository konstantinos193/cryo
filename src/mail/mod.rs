//! The mail context: an IMAP+SMTP client, eM Client-shaped.
//!
//! `Account` is the entity (keyed by its address), `Folder` and
//! `Envelope` are value objects, `Body` is what one open message
//! hydrates into. `Client` and `Store` are the ports the domain
//! declares: `imap_impl` is the shipped client adapter (lettre covers
//! send in `send`), `FileStore` the disk cache — envelopes as one JSON
//! index per folder, raw RFC822 bytes per opened message.
//!
//! RAM is the product, so the context is built to own almost none:
//! bodies are parsed on open, pushed to the page and dropped; the
//! worker (`worker`) is one thread that connects per request batch and
//! drops every session after an idle spell — nothing keeps a socket or
//! a mailbox resident. The app adapter lives in `app/mail.rs`; the page
//! is `shell/mail`.

mod account;
mod autoconfig;
mod client;
mod envelope;
mod folder;
mod guess;
mod imap_impl;
mod message;
mod sanitize;
mod send;
mod store;
mod thread;
mod wire;
mod worker;

pub use account::{load_accounts, save_accounts, Account, ServerSpec, Tls};
pub use client::{Client, Flag};
pub use envelope::Envelope;
pub use folder::{Folder, Role};
pub use guess::defaults_for;
pub use message::{Attachment, Body};
pub use send::Draft;
pub use store::FileStore;
pub use wire::{
    list_entries, Glance, GlanceAccount, GlanceCommand, ListEntry, PageCommand, Push, PushAccount,
    PushAttachment, PushBody, PushFile, PushFolder,
};
pub use worker::{spawn, Opened, Outcome, Request, Worker};

/// The one error type the context reports (AGENTS §7): variants name
/// the operation that failed, and no dependency's error leaks out.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum MailError {
    /// An address or server field failed validation — surfaced to the
    /// onboarding form verbatim.
    #[error("{0}")]
    Invalid(String),
    /// The IMAP server refused or could not be reached.
    #[error("imap {op}: {reason}")]
    Imap { op: &'static str, reason: String },
    /// The SMTP server refused or could not be reached.
    #[error("smtp {op}: {reason}")]
    Smtp { op: &'static str, reason: String },
    /// A stored file could not be read, written or parsed.
    #[error("{op} {path}: {reason}")]
    Io {
        op: &'static str,
        path: String,
        reason: String,
    },
    /// The stored index is from another build — the cache is dropped
    /// and refilled rather than migrated (it is a cache, not data).
    #[error("stored index has version {0}, this build reads 1")]
    Version(u32),
}

impl MailError {
    pub(crate) fn imap(op: &'static str, e: impl std::fmt::Display) -> Self {
        Self::Imap {
            op,
            reason: e.to_string(),
        }
    }

    pub(crate) fn smtp(op: &'static str, e: impl std::fmt::Display) -> Self {
        Self::Smtp {
            op,
            reason: e.to_string(),
        }
    }

    pub(crate) fn io(op: &'static str, path: &std::path::Path, e: impl std::fmt::Display) -> Self {
        Self::Io {
            op,
            path: path.display().to_string(),
            reason: e.to_string(),
        }
    }
}
