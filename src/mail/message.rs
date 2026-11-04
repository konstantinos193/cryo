//! Bodies: what one open message hydrates into — display text plus the
//! attachment table. Never serialized, never cached in memory: the raw
//! RFC822 bytes sit on disk and this is rebuilt per open (parse once,
//! push, drop — the resident cost of reading mail is the page's DOM,
//! which a freeze can reclaim wholesale).

use mail_parser::{HeaderValue, Message, MimeHeaders};

/// Inline (`cid:`) images are re-encoded into the markup as data URLs;
/// past this many bytes of them the rest stay as their alt text — a
/// message is a page, not a photo album in one push.
const MAX_INLINE_BYTES: usize = 3 * 1024 * 1024;

/// One attachment as the listing shows it. `index` is its position in
/// the parsed message's attachment list — the only handle the page
/// needs; part ids stay on this side of the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub index: usize,
    pub name: String,
    pub mime: String,
    pub size: usize,
}

/// A message ready for the reading pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Body {
    /// Sanitized display HTML — `None` for a plain-text message. This
    /// is already stripped of scripts, layout weapons and — unless
    /// `remote_loaded` — remote content; the document may inject it
    /// verbatim.
    pub html: Option<String>,
    /// The plain-text body, for messages without HTML and as the
    /// reply-quote source either way.
    pub text: String,
    /// How many remote references the sanitizer suppressed — the
    /// "remote content blocked" badge's number.
    pub blocked_remote: usize,
    /// Whether this hydration let remote images through — the reader
    /// asked for them, for this message.
    pub remote_loaded: bool,
    pub attachments: Vec<Attachment>,
    /// `Reply-To`, display-ready — where a reply goes when the sender
    /// says so; empty when it is just `From`.
    pub reply_to: String,
    /// The `List-Unsubscribe` header's web link, when the sender offers
    /// one; empty otherwise.
    pub unsubscribe: String,
}

/// The hydration door (AGENTS §6): raw RFC822 bytes in, a `Body` out.
/// HTML goes through `sanitize` here and nowhere else, so there is no
/// second path hostile markup could take to the page. `allow_remote`
/// is the reader's per-message decision to load remote images.
pub(crate) fn hydrate(raw: &[u8], allow_remote: bool) -> Body {
    let Some(m) = mail_parser::MessageParser::default().parse(raw) else {
        return Body {
            html: None,
            text: String::new(),
            blocked_remote: 0,
            remote_loaded: allow_remote,
            attachments: Vec::new(),
            reply_to: String::new(),
            unsubscribe: String::new(),
        };
    };
    let text = m.body_text(0).map(|t| t.to_string()).unwrap_or_default();
    // Prefer the HTML part when one exists — the text copy stays as the
    // plain view and the reply-quote source. `body_html` reports a
    // text/plain body too, so check the part really is html first.
    let has_html = m.parts.iter().any(|p| p.is_content_type("text", "html"));
    let (html, blocked_remote) = match (has_html, m.body_html(0)) {
        (true, Some(raw_html)) => {
            let inlined = inline_cids(&m, &raw_html);
            let cleaned = super::sanitize::clean(&inlined, allow_remote);
            (
                (!cleaned.html.is_empty()).then_some(cleaned.html),
                cleaned.blocked_remote,
            )
        }
        _ => (None, 0),
    };
    let attachments = m
        .attachments()
        .enumerate()
        .map(|(index, part)| Attachment {
            index,
            name: part.attachment_name().unwrap_or("attachment").to_string(),
            mime: mime_of(part.content_type()),
            size: part.contents().len(),
        })
        .collect();
    Body {
        html,
        text,
        blocked_remote,
        remote_loaded: allow_remote,
        attachments,
        reply_to: show_list(m.reply_to()),
        unsubscribe: unsubscribe_link(m.list_unsubscribe()),
    }
}

/// `<img src="cid:…">` → the part's bytes as a data URL, so the image
/// the sender embedded shows without the page fetching anything.
fn inline_cids(m: &Message<'_>, html: &str) -> String {
    use base64::Engine;
    let mut out = html.to_string();
    let mut spent = 0usize;
    for part in &m.parts {
        let Some(cid) = part.content_id() else {
            continue;
        };
        let cid = cid.trim().trim_start_matches('<').trim_end_matches('>');
        if cid.is_empty() {
            continue;
        }
        let needle = format!("cid:{cid}");
        if !out.contains(&needle) {
            continue;
        }
        let mime = mime_of(part.content_type());
        if !mime.starts_with("image/") || spent + part.contents().len() > MAX_INLINE_BYTES {
            continue;
        }
        spent += part.contents().len();
        let data = format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(part.contents())
        );
        out = out.replace(&needle, &data);
    }
    out
}

/// The first `http(s)` link a `List-Unsubscribe` header offers — the
/// `mailto:` alternative is skipped, a reader clicks, they don't send.
/// mail-parser reads the header as an address list (its `<…>` items
/// look like one), so both shapes are checked.
fn unsubscribe_link(value: &HeaderValue<'_>) -> String {
    let is_web = |s: &str| s.starts_with("https://") || s.starts_with("http://");
    if let HeaderValue::Address(list) = value {
        return list
            .iter()
            .filter_map(|a| a.address())
            .map(str::trim)
            .find(|s| is_web(s))
            .unwrap_or_default()
            .to_string();
    }
    header_text(value)
        .split(',')
        .map(|s| {
            s.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .trim()
        })
        .find(|s| is_web(s))
        .unwrap_or_default()
        .to_string()
}

/// A text or text-list header as one whitespace-joined string — what
/// `In-Reply-To`, `References` and `List-Unsubscribe` carry.
pub(super) fn header_text(value: &HeaderValue<'_>) -> String {
    match value {
        HeaderValue::Text(t) => t.trim().to_string(),
        HeaderValue::TextList(list) => list
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// An address header's display string — "A <a@x>, b@y" as one joined
/// list; nothing for a missing or unparseable header.
pub(super) fn show_list(addrs: Option<&mail_parser::Address<'_>>) -> String {
    addrs
        .map(|a| a.iter().map(show_mail_addr).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

/// One parsed `Addr` as display text — "Name <addr>" or the bare addr.
fn show_mail_addr(a: &mail_parser::Addr<'_>) -> String {
    let addr = a.address().unwrap_or_default();
    match a.name().filter(|n| !n.is_empty()) {
        Some(name) => format!("{name} <{addr}>"),
        None => addr.to_string(),
    }
}

/// "image/png" for the attachment row; the catch-all when the part
/// carries no Content-Type.
fn mime_of(ct: Option<&mail_parser::ContentType<'_>>) -> String {
    ct.map(|c| {
        let sub = c.c_subtype.as_deref().unwrap_or("*");
        format!("{}/{sub}", c.c_type)
    })
    .unwrap_or_else(|| "application/octet-stream".into())
}

/// One attachment's decoded bytes for a save — the raw cached message
/// is re-parsed rather than keeping a second store of decoded parts.
pub(crate) fn attachment_bytes(raw: &[u8], index: usize) -> Option<(String, String, Vec<u8>)> {
    let m = mail_parser::MessageParser::default().parse(raw)?;
    let part = m.attachments().nth(index)?;
    Some((
        part.attachment_name().unwrap_or("attachment").to_string(),
        mime_of(part.content_type()),
        part.contents().to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &[u8] = b"From: a@b\r\nSubject: x\r\nContent-Type: text/plain\r\n\r\nhello body";
    const WITH_HTML: &[u8] = b"From: a@b\r\nContent-Type: multipart/alternative; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nplain\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>rich</p><script>x</script>\r\n--b--\r\n";

    #[test]
    fn plain_text_hydrates_without_html() {
        let body = hydrate(PLAIN, false);
        assert!(body.html.is_none());
        assert_eq!(body.text.trim(), "hello body");
        assert!(body.attachments.is_empty());
        assert!(!body.remote_loaded);
    }

    #[test]
    fn html_bodies_arrive_sanitized() {
        let body = hydrate(WITH_HTML, false);
        let html = body.html.expect("an html part exists");
        assert!(html.contains("<p>rich</p>"));
        assert!(!html.contains("script"));
        assert_eq!(body.text.trim(), "plain");
    }

    #[test]
    fn remote_images_load_only_when_asked() {
        let raw = b"From: a@b\r\nContent-Type: text/html\r\n\r\n<img src=\"https://t.test/x.png\">";
        let blocked = hydrate(raw, false);
        assert_eq!(blocked.blocked_remote, 1);
        assert!(!blocked.html.unwrap_or_default().contains("t.test"));
        let loaded = hydrate(raw, true);
        assert_eq!(loaded.blocked_remote, 0);
        assert!(loaded.remote_loaded);
        let html = loaded.html.unwrap();
        assert!(html.contains("https://t.test/x.png"));
        assert!(html.contains("referrerpolicy=\"no-referrer\""));
    }

    #[test]
    fn inline_cid_images_become_data_urls() {
        let raw = b"From: a@b\r\nContent-Type: multipart/related; boundary=b\r\n\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>pic</p><img src=\"cid:pic1@x\">\r\n--b\r\nContent-Type: image/png\r\nContent-ID: <pic1@x>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b--\r\n";
        let body = hydrate(raw, false);
        let html = body.html.expect("html part");
        assert!(
            html.contains("data:image/png;base64,iVBORw0KGgo="),
            "{html}"
        );
        assert!(!html.contains("cid:"));
        assert_eq!(body.blocked_remote, 0);
    }

    #[test]
    fn reply_to_and_unsubscribe_ride_along() {
        let raw = b"From: a@b\r\nReply-To: Desk <desk@b>\r\nList-Unsubscribe: <mailto:u@b>, <https://b/unsub?x=1>\r\nContent-Type: text/plain\r\n\r\nhi";
        let body = hydrate(raw, false);
        assert_eq!(body.reply_to, "Desk <desk@b>");
        assert_eq!(body.unsubscribe, "https://b/unsub?x=1");
    }

    #[test]
    fn attachments_list_and_come_back_decoded() {
        let raw = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nhi\r\n--b\r\nContent-Type: application/pdf; name=\"doc.pdf\"\r\nContent-Disposition: attachment\r\n\r\nPDFBYTES\r\n--b--\r\n";
        let body = hydrate(raw, false);
        assert_eq!(body.attachments.len(), 1);
        assert_eq!(body.attachments[0].name, "doc.pdf");
        let (name, _mime, bytes) = attachment_bytes(raw, 0).unwrap();
        assert_eq!(name, "doc.pdf");
        assert!(bytes.starts_with(b"PDFBYTES"));
        assert!(attachment_bytes(raw, 9).is_none());
    }
}
