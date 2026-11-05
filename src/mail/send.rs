//! Sending: the compose window's draft becomes an RFC822 message,
//! lettre drives the SMTP conversation, and the raw bytes come back so
//! the caller can `APPEND` the copy the Sent folder needs.
//!
//! The composer writes plain text. A signed message also goes out as
//! `multipart/alternative`: the HTML half is the escaped text around the
//! account's already-sanitized signature, so nothing the composer typed
//! can become markup. Files ride as a `multipart/mixed` beside it, read from disk here on
//! the worker at send time — the app holds paths, never bytes.

use std::path::PathBuf;

use lettre::message::header::{ContentType, Header, HeaderName, HeaderValue};
use lettre::message::{Attachment, MultiPart, MultiPartBuilder, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};

use super::account::{Account, Tls};
use super::MailError;
use crate::vault::Secret;

/// Past this much attached, the send is refused before SMTP is asked —
/// most relays cap around here, and a refusal names the reason.
const MAX_ATTACHED_BYTES: u64 = 25 * 1024 * 1024;

/// What the compose overlay submitted — validated before the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    /// Comma- or semicolon-separated recipients, "Name <addr>" allowed.
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// What the writer wrote, plain text.
    pub text: String,
    /// The quoted message a reply carries — the signature goes between
    /// the two, where a reader expects it.
    pub quote: String,
    /// Add the account's signature, if it has one.
    pub sign: bool,
    /// RFC2822 Message-Id of the message answered, for threading.
    pub in_reply_to: String,
    /// The thread's Message-Id chain a reply appends to.
    pub references: String,
    /// Files the compose drawer picked — read when the message is
    /// built, so a file that vanished in between fails the send
    /// loudly rather than going out empty.
    pub attachments: Vec<PathBuf>,
}

/// A header lettre doesn't name — threading fields with a static name.
macro_rules! text_header {
    ($name:ident, $wire:literal) => {
        #[derive(Clone)]
        struct $name(String);
        impl Header for $name {
            fn name() -> HeaderName {
                HeaderName::new_from_ascii_str($wire)
            }
            fn parse(s: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                Ok(Self(s.to_string()))
            }
            fn display(&self) -> HeaderValue {
                HeaderValue::new(Self::name(), self.0.clone())
            }
        }
    };
}
text_header!(InReplyTo, "In-Reply-To");
text_header!(References, "References");

/// One comma/semicolon-separated recipient list → lettre mailboxes.
fn recipients(s: &str, field: &'static str) -> Result<Vec<lettre::message::Mailbox>, MailError> {
    let mut out = Vec::new();
    for part in s.split([',', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mailbox = part
            .parse::<lettre::message::Mailbox>()
            .map_err(|_| MailError::Invalid(format!("{field}: \"{part}\" is not an address")))?;
        out.push(mailbox);
    }
    Ok(out)
}

/// Build the message the draft describes — shared by `send`, the
/// Drafts `APPEND` and the test, so the headers a message carries are
/// the same everywhere.
pub(crate) fn build(account: &Account, draft: &Draft) -> Result<Message, MailError> {
    let to = recipients(&draft.to, "to")?;
    if to.is_empty() {
        return Err(MailError::Invalid("a message needs a recipient".into()));
    }
    // Header injection: CRLF in a header field is a second message the
    // sender didn't write — lettre escapes values it knows, the custom
    // threading headers are cleaned here at the boundary.
    let clean = |v: &str| v.replace(['\r', '\n'], " ").trim().to_string();
    let mut builder = Message::builder()
        .from(account.mailbox()?)
        .subject(clean(&draft.subject));
    for m in to {
        builder = builder.to(m);
    }
    for m in recipients(&draft.cc, "cc")? {
        builder = builder.cc(m);
    }
    for m in recipients(&draft.bcc, "bcc")? {
        builder = builder.bcc(m);
    }
    let in_reply_to = clean(&draft.in_reply_to);
    if !in_reply_to.is_empty() {
        builder = builder.header(InReplyTo(in_reply_to.clone()));
    }
    let references = clean(&draft.references);
    if !references.is_empty() {
        builder = builder.header(References(references));
    }
    let signature = Some(account.signature()).filter(|s| draft.sign && !s.is_empty());
    let plain = plain_body(draft, signature);
    let html = signature.map(|s| html_body(draft, s));
    if draft.attachments.is_empty() {
        return match html {
            Some(html) => builder.multipart(MultiPart::alternative_plain_html(plain, html)),
            None => builder.body(plain),
        }
        .map_err(|e| MailError::Invalid(e.to_string()));
    }
    let start: MultiPartBuilder = MultiPart::mixed();
    let mut mixed = match html {
        Some(html) => start.multipart(MultiPart::alternative_plain_html(plain, html)),
        None => start.singlepart(SinglePart::plain(plain)),
    };
    let mut total = 0u64;
    for path in &draft.attachments {
        let bytes = std::fs::read(path).map_err(|e| MailError::io("read", path, e))?;
        total += bytes.len() as u64;
        if total > MAX_ATTACHED_BYTES {
            return Err(MailError::Invalid(format!(
                "attachments exceed {} MB",
                MAX_ATTACHED_BYTES / 1024 / 1024
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let mime = mime_for(&name);
        let content_type =
            ContentType::parse(mime).map_err(|e| MailError::Invalid(format!("{name}: {e}")))?;
        mixed = mixed.singlepart(Attachment::new(name).body(bytes, content_type));
    }
    builder
        .multipart(mixed)
        .map_err(|e| MailError::Invalid(e.to_string()))
}

/// The text a plain reader gets: writing, the RFC 3676 "-- " signature
/// marker with the signature as text, then the quote.
fn plain_body(draft: &Draft, signature: Option<&str>) -> String {
    let Some(signature) = signature else {
        return format!("{}{}", draft.text, draft.quote);
    };
    let text = mail_parser::decoders::html::html_to_text(signature);
    format!(
        "{}\n\n-- \n{}{}",
        draft.text.trim_end(),
        text.trim(),
        draft.quote
    )
}

/// The same three parts as HTML. Only the signature is markup.
fn html_body(draft: &Draft, signature: &str) -> String {
    const PRE: &str = "<div style=\"white-space:pre-wrap\">";
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut out = format!(
        "{PRE}{}</div><br><div>{signature}</div>",
        escape(draft.text.trim_end())
    );
    if !draft.quote.is_empty() {
        out.push_str(&format!(
            "<br>{PRE}{}</div>",
            escape(draft.quote.trim_start())
        ));
    }
    out
}

/// A Content-Type from the file's extension — the common ones named,
/// the rest sent as bytes the receiver sniffs.
fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Connect, authenticate, send — and hand back the formatted RFC822
/// for the Sent-folder `APPEND` the caller performs.
pub fn send(account: &Account, secret: &Secret, draft: &Draft) -> Result<Vec<u8>, MailError> {
    let message = build(account, draft)?;
    let spec = account.smtp();
    let creds = Credentials::new(account.username().to_string(), secret.expose().to_string());
    let builder = match spec.tls() {
        Tls::Wrapper => SmtpTransport::relay(spec.host()),
        Tls::StartTls => SmtpTransport::starttls_relay(spec.host()),
    }
    .map_err(|e| MailError::smtp("relay", e))?;
    let mailer = builder.credentials(creds).port(spec.port()).build();
    mailer
        .send(&message)
        .map_err(|e| MailError::smtp("send", e))?;
    Ok(message.formatted())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::account::ServerSpec;

    fn account() -> Account {
        let spec = |h: &str| ServerSpec::new(h, 1, Tls::Wrapper).unwrap();
        Account::new(
            "Ann R",
            "ann@a.test",
            "",
            spec("i.a.test"),
            spec("s.a.test"),
        )
        .unwrap()
    }

    #[test]
    fn a_built_message_carries_sender_recipients_and_threading() {
        let draft = Draft {
            to: "Bob <bob@b.test>, c@d.test".into(),
            cc: "d@e.test".into(),
            subject: "Re: thing\r\nBcc: nobody@evil.test".into(),
            text: "body".into(),
            in_reply_to: "<m1@b.test>".into(),
            references: "<m0@b.test>".into(),
            ..Draft::default()
        };
        let raw = String::from_utf8(build(&account(), &draft).unwrap().formatted()).unwrap();
        // lettre writes display names quoted-encoded — check the parts
        // that must be present rather than one rendering.
        assert!(raw.contains("Ann R"), "{raw}");
        assert!(raw.contains("ann@a.test"), "{raw}");
        assert!(raw.contains("bob@b.test"), "{raw}");
        assert!(raw.contains("c@d.test"), "{raw}");
        assert!(raw.contains("d@e.test"), "{raw}");
        assert!(raw.contains("In-Reply-To: <m1@b.test>"), "{raw}");
        assert!(raw.contains("References: <m0@b.test>"), "{raw}");
        // The smuggled header never became a header line.
        assert!(!raw.lines().any(|l| l.starts_with("Bcc:")));
        assert!(raw.ends_with("body"));
    }

    #[test]
    fn files_ride_as_mixed_parts_and_bcc_is_a_header_lettre_strips_on_the_wire() {
        let dir = crate::test_dir("mail-send-attach");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"hello file").unwrap();
        let draft = Draft {
            to: "bob@b.test".into(),
            bcc: "quiet@q.test".into(),
            subject: "with file".into(),
            text: "see attached".into(),
            attachments: vec![file.clone()],
            ..Draft::default()
        };
        let message = build(&account(), &draft).unwrap();
        let raw = String::from_utf8(message.formatted()).unwrap();
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("filename=\"notes.txt\""), "{raw}");
        assert!(raw.contains("text/plain"), "{raw}");
        assert!(raw.contains("see attached"), "{raw}");
        // Bcc reaches the envelope, never the headers the recipient sees.
        assert!(message
            .envelope()
            .to()
            .iter()
            .any(|a| a.to_string() == "quiet@q.test"));
        assert!(!raw.contains("quiet@q.test"), "{raw}");
        // A file that vanished is a refused send, not an empty part.
        std::fs::remove_dir_all(&dir).ok();
        assert!(build(&account(), &draft).is_err());
    }

    fn signed() -> Account {
        account()
            .with_profile("Ann R", "<p>Ann <b>Rivers</b></p>", "")
            .unwrap()
    }

    fn reply() -> Draft {
        Draft {
            to: "bob@b.test".into(),
            subject: "s".into(),
            text: "my <reply>\n".into(),
            quote: "\n\nOn Monday, Bob wrote:\n> hello".into(),
            sign: true,
            ..Draft::default()
        }
    }

    /// A built message back through the parser: the text and HTML a
    /// recipient's client would show.
    fn parts(message: &Message) -> (String, String) {
        let raw = message.formatted();
        let m = mail_parser::MessageParser::default().parse(&raw).unwrap();
        (
            m.body_text(0).unwrap().to_string(),
            m.body_html(0).unwrap().to_string(),
        )
    }

    #[test]
    fn a_signature_goes_between_the_writing_and_the_quote() {
        let (text, html) = parts(&build(&signed(), &reply()).unwrap());
        let order = |body: &str, sig: &str| {
            let (a, b, c) = (
                body.find("reply").unwrap(),
                body.find(sig).unwrap(),
                body.find("Bob wrote").unwrap(),
            );
            assert!(a < b && b < c, "{body}");
        };
        order(&text, "Rivers");
        assert!(text.lines().any(|l| l == "-- "), "{text}");
        order(&html, "<b>Rivers</b>");
        assert!(
            html.contains("my &lt;reply&gt;"),
            "typed text stays text: {html}"
        );
    }

    #[test]
    fn a_signed_message_keeps_its_attachments() {
        let dir = crate::test_dir("mail-send-signed");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"hello file").unwrap();
        let draft = Draft {
            attachments: vec![file],
            ..reply()
        };
        let raw = String::from_utf8(build(&signed(), &draft).unwrap().formatted()).unwrap();
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(raw.contains("filename=\"notes.txt\""), "{raw}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_signature_means_the_plain_message_it_always_was() {
        let raw = String::from_utf8(build(&account(), &reply()).unwrap().formatted()).unwrap();
        assert!(!raw.contains("multipart"), "no signature to carry: {raw}");
        let unsigned = Draft {
            sign: false,
            ..reply()
        };
        let raw = String::from_utf8(build(&signed(), &unsigned).unwrap().formatted()).unwrap();
        assert!(!raw.contains("Rivers"), "taken off this message: {raw}");
        assert!(raw.contains("Bob wrote"), "{raw}");
    }

    #[test]
    fn recipients_must_parse_and_exist() {
        assert!(recipients("a@b.test; c@d.test", "to").unwrap().len() == 2);
        assert!(recipients("", "to").unwrap().is_empty());
        assert!(recipients("not an address", "to").is_err());
        let draft = Draft {
            to: String::new(),
            ..Draft::default()
        };
        assert!(build(&account(), &draft).is_err());
    }
}
