//! Envelopes: the header row a folder listing is built from. A value
//! object — replaced wholesale, never mutated through a reference.
//! Deliberately small: this is the whole resident set of a mailbox —
//! bodies live on disk. It derives nothing serde (AGENTS §6): the
//! index file's row lives in `store`, the page's row in `wire`.

/// One message's row in a folder listing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Envelope {
    pub uid: u32,
    /// RFC2822 Message-Id — replies thread through it.
    pub message_id: String,
    /// The `In-Reply-To` header, verbatim — one id, usually.
    pub in_reply_to: String,
    /// The `References` chain, whitespace-separated ids — `thread`
    /// joins conversations through it.
    pub references: String,
    /// Display-ready sender: "Ann <ann@a.test>" or the bare address.
    pub from: String,
    /// Display-ready recipients, comma-joined.
    pub to: String,
    pub cc: String,
    pub subject: String,
    /// Unix seconds; `0` when the server sent none. `i64` because a
    /// pre-1970 Date: is a real (if mangled) thing.
    pub date: i64,
    /// RFC822.SIZE — shown as "12 KB" in the list.
    pub size: u32,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub deleted: bool,
}

impl Envelope {
    /// The row for a message the server listed with no usable headers
    /// at all — it still occupies a uid.
    pub fn placeholder(uid: u32) -> Self {
        Self {
            uid,
            subject: "(no subject)".into(),
            ..Self::default()
        }
    }
}
