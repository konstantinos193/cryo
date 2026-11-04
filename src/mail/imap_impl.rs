//! The shipped `Client` adapter: one `imap::Session` over schannel TLS.
//!
//! Every call is a synchronous IMAP exchange on the worker thread. The
//! envelope path deliberately fetches `BODY.PEEK[HEADER.FIELDS …]`
//! rather than `ENVELOPE`: imap-proto does not decode RFC2047 encoded
//! words, and mail-parser already on the tree decodes them properly —
//! one parse here means "=?UTF-8?B?…?=" subjects never reach the list.

use imap::types::Flag as ImapFlag;

use super::account::{Account, Tls};
use super::client::{Client, Flag, Selection};
use super::folder::{display_name, role_of, Folder};
use super::message::{header_text, show_list};
use super::{Envelope, MailError};
use crate::vault::Secret;

type Session = imap::Session<imap::Connection>;

/// The header block a listing needs — one field-set fetch per window,
/// parsed by mail-parser for correct encoding and charset handling.
const ENVELOPE_QUERY: &str = "(UID FLAGS RFC822.SIZE BODY.PEEK[HEADER.FIELDS (FROM TO CC SUBJECT DATE MESSAGE-ID IN-REPLY-TO REFERENCES)])";

pub(super) fn connect(account: &Account, secret: &Secret) -> Result<Box<dyn Client>, MailError> {
    let spec = account.imap();
    // The mode is the account's, never `Auto`: a guessed downgrade to
    // plaintext is exactly what a hostile network would want.
    let (mode, step) = match spec.tls() {
        Tls::Wrapper => (imap::ConnectionMode::Tls, "connect"),
        Tls::StartTls => (imap::ConnectionMode::StartTls, "starttls"),
    };
    let client = imap::ClientBuilder::new(spec.host(), spec.port())
        .mode(mode)
        .tls_kind(imap::TlsKind::Native)
        .connect()
        .map_err(|e| MailError::imap(step, e))?;
    let session = client
        .login(account.username(), secret.expose())
        .map_err(|(e, _)| MailError::imap("login", e))?;
    Ok(Box::new(ImapClient { session, exists: 0 }))
}

struct ImapClient {
    session: Session,
    /// `EXISTS` of the last `select` — the `envelopes` window's right
    /// edge. `Session` doesn't expose it between commands.
    exists: u32,
}

impl Drop for ImapClient {
    fn drop(&mut self) {
        // LOGOUT on teardown — a dropped socket is also a logout on
        // most servers, so a refusal here is not worth reporting.
        let _ = self.session.logout();
    }
}

/// Header bytes → an `Envelope` row. Parsing headers with mail-parser
/// is what makes encoded words and odd charsets display correctly.
fn envelope_from_header(uid: u32, flags: &[ImapFlag<'_>], size: u32, header: &[u8]) -> Envelope {
    let mut env = Envelope::placeholder(uid);
    env.size = size;
    env.seen = flags.iter().any(|f| f == &ImapFlag::Seen);
    env.flagged = flags.iter().any(|f| f == &ImapFlag::Flagged);
    env.answered = flags.iter().any(|f| f == &ImapFlag::Answered);
    env.draft = flags.iter().any(|f| f == &ImapFlag::Draft);
    env.deleted = flags.iter().any(|f| f == &ImapFlag::Deleted);
    let Some(m) = mail_parser::MessageParser::default().parse(header) else {
        return env;
    };
    env.from = show_list(m.from());
    env.to = show_list(m.to());
    env.cc = show_list(m.cc());
    if let Some(subject) = m.subject() {
        env.subject = subject.to_string();
    }
    if let Some(date) = m.date() {
        env.date = date.to_timestamp();
    }
    if let Some(id) = m.message_id() {
        env.message_id = id.to_string();
    }
    env.in_reply_to = header_text(m.in_reply_to());
    env.references = header_text(m.references());
    env
}

impl Client for ImapClient {
    fn verify(&mut self) -> Result<(), MailError> {
        // Login already ran in `connect` — NOOP confirms the session
        // itself is alive.
        self.session.noop().map_err(|e| MailError::imap("noop", e))
    }

    fn folders(&mut self) -> Result<Vec<Folder>, MailError> {
        let names = self
            .session
            .list(Some(""), Some("*"))
            .map_err(|e| MailError::imap("list", e))?;
        Ok(names
            .iter()
            .map(|n| Folder {
                name: n.name().to_string(),
                display: display_name(n.name()),
                role: role_of(n.attributes(), n.name()),
                unseen: None,
                exists: None,
            })
            .collect())
    }

    fn select(&mut self, folder: &str) -> Result<Selection, MailError> {
        let mailbox = self
            .session
            .select(folder)
            .map_err(|e| MailError::imap("select", e))?;
        self.exists = mailbox.exists;
        Ok(Selection {
            uid_validity: mailbox.uid_validity.unwrap_or(0),
            exists: mailbox.exists,
            unseen: mailbox.unseen,
        })
    }

    fn envelopes(&mut self, limit: u32) -> Result<Vec<Envelope>, MailError> {
        if self.exists == 0 {
            return Ok(Vec::new());
        }
        let first = self.exists.saturating_sub(limit.saturating_sub(1)).max(1);
        let fetches = self
            .session
            .fetch(format!("{first}:{}", self.exists), ENVELOPE_QUERY)
            .map_err(|e| MailError::imap("fetch", e))?;
        let mut out = Vec::with_capacity(fetches.len());
        for fetch in fetches.iter() {
            let Some(uid) = fetch.uid else { continue };
            let env = match fetch.header() {
                Some(header) => {
                    envelope_from_header(uid, fetch.flags(), fetch.size.unwrap_or(0), header)
                }
                None => {
                    let mut e = Envelope::placeholder(uid);
                    e.seen = fetch.flags().iter().any(|f| f == &ImapFlag::Seen);
                    e.size = fetch.size.unwrap_or(0);
                    e
                }
            };
            out.push(env);
        }
        // Seq order puts newest last — the listing wants it first.
        out.reverse();
        Ok(out)
    }

    fn raw(&mut self, uid: u32) -> Result<Vec<u8>, MailError> {
        let fetches = self
            .session
            .uid_fetch(uid.to_string(), "BODY.PEEK[]")
            .map_err(|e| MailError::imap("fetch-body", e))?;
        fetches
            .iter()
            .find_map(|f| f.body().map(|b| b.to_vec()))
            .ok_or_else(|| MailError::Invalid(format!("uid {uid} returned no body")))
    }

    fn set_flag(&mut self, uid: u32, flag: Flag, on: bool) -> Result<(), MailError> {
        let op = if on { "+FLAGS.SILENT" } else { "-FLAGS.SILENT" };
        self.session
            .uid_store(uid.to_string(), format!("{op} ({})", flag.atom()))
            .map_err(|e| MailError::imap("store", e))?;
        Ok(())
    }

    fn moveto(&mut self, uid: u32, target: &str) -> Result<(), MailError> {
        if self
            .session
            .capabilities()
            .map_err(|e| MailError::imap("capability", e))?
            .has_str("MOVE")
        {
            self.session
                .uid_mv(uid.to_string(), target)
                .map_err(|e| MailError::imap("move", e))?;
            return Ok(());
        }
        self.session
            .uid_copy(uid.to_string(), target)
            .map_err(|e| MailError::imap("copy", e))?;
        self.delete(uid)
    }

    fn delete(&mut self, uid: u32) -> Result<(), MailError> {
        self.set_flag(uid, Flag::Deleted, true)?;
        self.session
            .expunge()
            .map_err(|e| MailError::imap("expunge", e))?;
        Ok(())
    }

    fn append(&mut self, folder: &str, rfc822: &[u8]) -> Result<(), MailError> {
        self.session
            .append(folder, rfc822)
            .flags([ImapFlag::Seen])
            .finish()
            .map_err(|e| MailError::imap("append", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_subjects_decode_for_the_list() {
        // "=?UTF-8?B?…?=" base64 of "Réunion ✕".
        let header = b"From: Ann <ann@a.test>\r\nSubject: =?UTF-8?B?UsOpdW5pb24g4pyV?=\r\nDate: Wed, 01 Nov 2023 10:00:00 +0000\r\nMessage-Id: <m2@a.test>\r\nIn-Reply-To: <m1@a.test>\r\nReferences: <m0@a.test>\r\n <m1@a.test>\r\n\r\n";
        let env = envelope_from_header(7, &[], 100, header);
        assert_eq!(env.uid, 7);
        assert_eq!(env.from, "Ann <ann@a.test>");
        assert_eq!(env.subject, "Réunion ✕");
        assert!(env.date > 0);
        assert!(!env.seen);
        assert_eq!(env.message_id, "m2@a.test");
        assert_eq!(env.in_reply_to, "m1@a.test");
        assert_eq!(env.references, "m0@a.test m1@a.test");
    }

    #[test]
    fn missing_headers_still_make_a_row() {
        let env = envelope_from_header(3, &[ImapFlag::Seen], 0, b"\r\n");
        assert_eq!(env.uid, 3);
        assert!(env.seen);
    }

    #[test]
    fn a_plausible_limit_never_fetches_below_one() {
        for (exists, limit, first) in [(3u32, 500u32, 1u32), (800, 500, 301), (1, 1, 1)] {
            let f = exists.saturating_sub(limit.saturating_sub(1)).max(1);
            assert_eq!(f, first);
        }
    }
}
