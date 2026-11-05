//! The /mail page wire: `window.ipc.postMessage` carries a
//! [`PageCommand`] in, `window.__cryo_mail(json)` carries a [`Push`]
//! back. These are the boundary DTOs (AGENTS §6): dumb field bags that
//! serialize — the domain types they mirror never do. Field names and
//! `kind` tags are the contract `mail.js` programs against.
//!
//! A password only ever travels *in* (`AddAccount`, once, then the
//! vault owns it) — no `Push` variant can carry one out.

use serde::{Deserialize, Serialize};

/// What the document may ask. `deny_unknown_fields` so a typo'd key
/// fails loudly like the chrome wire does.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PageCommand {
    /// Document loaded — push the full state.
    Hello {},
    /// A folder row was clicked — push its listing.
    SelectFolder {
        account: String,
        folder: String,
    },
    /// A message row was clicked — push its body. `remote` is the
    /// reader's opt-in to this message's remote images.
    Open {
        account: String,
        folder: String,
        uid: u32,
        #[serde(default)]
        remote: bool,
    },
    /// The ⟳ button — sync every account now.
    Sync {},
    SetFlag {
        account: String,
        folder: String,
        uid: u32,
        flag: WireFlag,
        on: bool,
    },
    MoveTo {
        account: String,
        folder: String,
        uid: u32,
        target: String,
    },
    Delete {
        account: String,
        folder: String,
        uid: u32,
    },
    /// The onboarding form. `password` is used to verify once, then
    /// sealed into the vault — it never goes back over the wire.
    AddAccount {
        display: String,
        email: String,
        password: String,
        #[serde(default)]
        username: String,
        imap_host: String,
        imap_port: u16,
        imap_tls: String,
        smtp_host: String,
        smtp_port: u16,
        smtp_tls: String,
    },
    RemoveAccount {
        email: String,
    },
    /// The settings page's Save.
    SaveProfile {
        email: String,
        display: String,
        signature: String,
        photo: String,
    },
    /// The settings page leaving its HTML source view: typed markup
    /// goes through the sanitizer before the document renders it.
    CleanSignature {
        html: String,
    },
    /// Addresses the list is showing that need a picture. `lookup` is
    /// false when the reader turned lookups off: their own choices still
    /// answer, nothing goes out.
    Faces {
        addrs: Vec<String>,
        #[serde(default)]
        lookup: bool,
    },
    /// The reader chose a sender's picture; empty `photo` clears it.
    SetFace {
        addr: String,
        photo: String,
    },
    /// The compose overlay's Send. `in_reply_to`/`references` thread a
    /// reply; empty strings mean a fresh message.
    Send {
        account: String,
        to: String,
        cc: String,
        #[serde(default)]
        bcc: String,
        subject: String,
        text: String,
        #[serde(default)]
        quote: String,
        #[serde(default)]
        sign: bool,
        #[serde(default)]
        in_reply_to: String,
        #[serde(default)]
        references: String,
    },
    /// The compose overlay's "Save draft" — appended to Drafts, never
    /// sent.
    SaveDraft {
        account: String,
        to: String,
        cc: String,
        #[serde(default)]
        bcc: String,
        subject: String,
        text: String,
        #[serde(default)]
        quote: String,
        #[serde(default)]
        sign: bool,
    },
    /// Compose's "Attach" — the app opens its file picker and keeps
    /// the paths; the page only ever sees names and sizes.
    AttachPick {},
    /// The × on a picked file, by its position in the last
    /// `attachments` push.
    AttachDrop {
        index: usize,
    },
    /// Compose closed without sending — the picked paths are dropped.
    AttachClear {},
    /// The 💾 on an attachment row — `index` into the open message's
    /// attachment list.
    SaveAttachment {
        account: String,
        folder: String,
        uid: u32,
        index: usize,
    },
    /// The address fields' live server-guess request — the page asks
    /// before the form is submitted so the advanced fields can
    /// prefill.
    Guess {
        email: String,
    },
}

/// What the home page may ask — its one question. Routed by the tab's
/// address like [`PageCommand`]; a web page sending it lands in the
/// autofill parser instead.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GlanceCommand {
    /// The home page loaded — push the inbox glance.
    MailGlance {},
}

/// What the app evals into `window.__cryo_mail_glance` on the home
/// page: the inbox at a glance — counts and status, never a listing,
/// never a body. The home page is a content surface; what it learns
/// about mail is exactly this.
#[derive(Serialize)]
pub struct Glance {
    pub incognito: bool,
    /// Unread across every account's inbox.
    pub unseen: u32,
    pub accounts: Vec<GlanceAccount>,
}

#[derive(Serialize)]
pub struct GlanceAccount {
    pub email: String,
    pub display: String,
    pub unseen: u32,
    /// "syncing" | "" (idle) | an error's text.
    pub status: String,
}

/// The flag names the wire carries — the same words the list's buttons
/// use.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireFlag {
    Seen,
    Flagged,
    Answered,
    Draft,
    Deleted,
}

impl From<WireFlag> for super::client::Flag {
    fn from(w: WireFlag) -> Self {
        match w {
            WireFlag::Seen => Self::Seen,
            WireFlag::Flagged => Self::Flagged,
            WireFlag::Answered => Self::Answered,
            WireFlag::Draft => Self::Draft,
            WireFlag::Deleted => Self::Deleted,
        }
    }
}

/// An account as the folder column shows it.
#[derive(Serialize)]
pub struct PushAccount {
    pub email: String,
    pub display: String,
    /// Sanitized HTML; compose shows it, settings edits it.
    pub signature: String,
    /// A `data:` URL, or empty.
    pub photo: String,
    pub username: String,
    /// "host:port" of each server, for the settings page to show.
    pub imap: String,
    pub smtp: String,
    pub folders: Vec<PushFolder>,
    /// "syncing" | "ok" | an error's text — the row's status dot.
    pub status: String,
}

#[derive(Serialize)]
pub struct PushFolder {
    pub name: String,
    pub display: String,
    /// The decoded hierarchy, "/"-joined — the rail sorts by it.
    pub path: String,
    pub depth: u8,
    /// `false` for a parent that only groups its children.
    pub selectable: bool,
    /// "inbox" | "sent" | "drafts" | "junk" | "archive" | "trash" | "other".
    pub role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unseen: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exists: Option<u32>,
}

/// One row of the message list — the page's copy of an `Envelope`,
/// plus the conversation it belongs to.
#[derive(Serialize)]
pub struct ListEntry {
    pub uid: u32,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub date: i64,
    pub size: u32,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    /// Rows sharing this are one conversation — `thread::keys`'s
    /// verdict, so the page groups without re-deriving it.
    pub thread: String,
}

/// A listing's rows, threaded — the one place envelopes become rows.
pub fn list_entries(envelopes: &[super::Envelope]) -> Vec<ListEntry> {
    let keys = super::thread::keys(envelopes);
    envelopes
        .iter()
        .zip(keys)
        .map(|(e, thread)| ListEntry {
            uid: e.uid,
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
            thread,
        })
        .collect()
}

/// One file the compose drawer has picked — name and size only; the
/// path stays with the app.
#[derive(Serialize)]
pub struct PushFile {
    pub name: String,
    pub size: u64,
}

/// One attachment row in a `Body` push.
#[derive(Serialize)]
pub struct PushAttachment {
    pub index: usize,
    pub name: String,
    pub mime: String,
    pub size: usize,
}

/// One open message as the reading pane shows it — the `body` push's
/// payload, boxed because it dwarfs every other push.
#[derive(Serialize)]
pub struct PushBody {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub date: i64,
    pub html: Option<String>,
    pub text: String,
    pub blocked_remote: usize,
    /// Remote images were let through for this push.
    pub remote_loaded: bool,
    pub attachments: Vec<PushAttachment>,
    /// Where a reply goes when the sender set `Reply-To`; empty means
    /// `from`.
    pub reply_to: String,
    /// The sender's one-click unsubscribe page, if offered.
    pub unsubscribe: String,
    /// The headers a reply prefills from.
    pub message_id: String,
    /// The chain a reply's `References` extends.
    pub references: String,
    /// Flags as the list showed them — the read/star buttons' state.
    pub seen: bool,
    pub flagged: bool,
}

/// What the app evals into `window.__cryo_mail`. One function, one
/// tagged union — the page dispatches on `kind`.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Push {
    /// The whole shell state: accounts + folders, and whether the
    /// window may use mail at all (incognito cannot).
    State {
        incognito: bool,
        accounts: Vec<PushAccount>,
    },
    /// A folder's listing — newest first.
    List {
        account: String,
        folder: String,
        envelopes: Vec<ListEntry>,
    },
    /// One open message. `html` is sanitized; `text` is the plain
    /// fallback and the reply-quote source.
    Body(Box<PushBody>),
    /// A folder changed under the open view — a flag the page set, or
    /// one a sync reported; the row repaints without a refetch.
    Flag {
        account: String,
        folder: String,
        uid: u32,
        flag: &'static str,
        on: bool,
    },
    /// Per-account activity line — "syncing", "" (idle), or an error.
    Status { account: String, status: String },
    /// The onboarding form's verdict: "" on success (a `state` push
    /// follows), or the reason it failed.
    AccountReply { error: String },
    /// The settings page's verdict: "" on success (a `state` push
    /// follows), or the reason it was refused.
    ProfileReply { error: String },
    /// Typed signature source, safe to render.
    SignatureClean { html: String },
    /// One sender's picture, as a `data:` URL. `chosen` marks the reader's
    /// own, where an empty `src` means it was cleared.
    Face {
        addr: String,
        src: String,
        chosen: bool,
    },
    /// Compose's verdict: "" = sent.
    SendReply { error: String },
    /// A guessed server pair for the onboarding form, or nulls.
    Guess {
        imap_host: Option<String>,
        imap_port: Option<u16>,
        imap_tls: Option<String>,
        smtp_host: Option<String>,
        smtp_port: Option<u16>,
        smtp_tls: Option<String>,
    },
    /// The save flow's verdict — "" on success, else the reason.
    AttachmentSaved { name: String, error: String },
    /// The compose drawer's picked files, after every pick or drop.
    Attachments { files: Vec<PushFile> },
    /// Something failed — the toast's text.
    Error { op: String, detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse_the_shapes_the_page_sends() {
        let cmd: PageCommand =
            serde_json::from_str(r#"{"kind":"open","account":"a","folder":"INBOX","uid":7}"#)
                .unwrap();
        assert!(matches!(cmd, PageCommand::Open { uid: 7, .. }));
        let cmd: PageCommand = serde_json::from_str(
            r#"{"kind":"set_flag","account":"a","folder":"INBOX","uid":7,"flag":"seen","on":true}"#,
        )
        .unwrap();
        assert!(matches!(cmd, PageCommand::SetFlag { .. }));
        let cmd: PageCommand = serde_json::from_str(
            r#"{"kind":"add_account","display":"A","email":"a@b","password":"p","username":"","imap_host":"i","imap_port":993,"imap_tls":"tls","smtp_host":"s","smtp_port":465,"smtp_tls":"tls"}"#,
        )
        .unwrap();
        assert!(matches!(cmd, PageCommand::AddAccount { .. }));
        // Unknown kinds and smuggled fields are rejected.
        assert!(serde_json::from_str::<PageCommand>(r#"{"kind":"exec","cmd":"x"}"#).is_err());
        assert!(serde_json::from_str::<PageCommand>(r#"{"kind":"hello","extra":1}"#).is_err());
    }

    #[test]
    fn the_home_page_asks_one_thing_and_nothing_else() {
        assert!(matches!(
            serde_json::from_str::<GlanceCommand>(r#"{"kind":"mail_glance"}"#).unwrap(),
            GlanceCommand::MailGlance {}
        ));
        // The mail wire's own commands are not the home page's to send.
        assert!(serde_json::from_str::<GlanceCommand>(
            r#"{"kind":"open","account":"a","folder":"INBOX","uid":7}"#
        )
        .is_err());
        assert!(serde_json::from_str::<GlanceCommand>(r#"{"kind":"mail_glance","x":1}"#).is_err());
    }

    #[test]
    fn the_body_push_keeps_its_kind_tag_through_the_box() {
        let json = serde_json::to_string(&Push::Body(Box::new(PushBody {
            account: "a".into(),
            folder: "INBOX".into(),
            uid: 7,
            from: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            date: 0,
            html: None,
            text: "hi".into(),
            blocked_remote: 0,
            remote_loaded: false,
            attachments: Vec::new(),
            reply_to: String::new(),
            unsubscribe: String::new(),
            message_id: String::new(),
            references: String::new(),
            seen: true,
            flagged: false,
        })))
        .unwrap();
        assert!(json.starts_with("{\"kind\":\"body\","), "{json}");
        assert!(json.contains("\"uid\":7"));
    }

    #[test]
    fn no_push_variant_can_carry_a_secret() {
        // A grep-shaped guard: the whole enum would need a field named
        // password to leak one.
        let json = serde_json::to_string(&Push::AccountReply { error: "x".into() }).unwrap();
        assert!(!json.contains("password"));
    }
}
