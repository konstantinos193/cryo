//! Folders: the mailboxes an account lists, and which one is the
//! inbox. `Role` is how the UI groups specials without trusting their
//! display names — a server may call trash "Deleted Items".
//!
//! `Folder::listed` is the one door a wire row comes through. IMAP names
//! are modified UTF-7 and use the server's own separator; the raw name
//! goes back on the wire untouched, everything a person reads is decoded
//! here.

use imap_proto::NameAttribute;

/// What a folder is for. Detection prefers the server's own
/// special-use attributes; name matching is the fallback for servers
/// that don't send any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Inbox,
    Drafts,
    Sent,
    Junk,
    Archive,
    Trash,
    /// Anything else — custom folders and specials we don't model.
    Other,
}

/// One mailbox as the list reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    /// The IMAP mailbox name — "INBOX", "[Gmail]/Sent Mail", "Notes".
    /// Sent on the wire verbatim; the UI never rewrites it.
    pub name: String,
    /// The last path segment for display ("Sent Mail").
    pub display: String,
    /// The decoded hierarchy, "/"-joined, without the `INBOX.` prefix
    /// some servers put on every folder — the rail's sort key.
    pub path: String,
    /// How deep `path` nests — the rail's indent.
    pub depth: u8,
    /// `false` for a `\Noselect` parent: it groups its children and
    /// holds no messages, so `SELECT` would be refused.
    pub selectable: bool,
    pub role: Role,
    /// Unread and total counts from the last sync — `None` before the
    /// folder has ever been selected.
    pub unseen: Option<u32>,
    pub exists: Option<u32>,
}

impl Folder {
    /// A `LIST` row → a folder.
    pub(super) fn listed(name: &str, delimiter: Option<&str>, attrs: &[NameAttribute<'_>]) -> Self {
        let parts = segments(name, delimiter);
        let depth = parts.len().saturating_sub(1).min(usize::from(u8::MAX)) as u8;
        // A nested "Clients/Sent" is the client's folder, not the
        // account's Sent — only the server's own word can say otherwise.
        let role = attr_role(attrs).unwrap_or_else(|| {
            if depth == 0 {
                name_role(name)
            } else {
                Role::Other
            }
        });
        Self {
            name: name.to_string(),
            display: if role == Role::Inbox {
                "Inbox".into()
            } else {
                parts.last().cloned().unwrap_or_default()
            },
            path: parts.join("/"),
            depth,
            selectable: !attrs.iter().any(|a| matches!(a, NameAttribute::NoSelect)),
            role,
            unseen: None,
            exists: None,
        }
    }

    /// IMAP reports INBOX's name case-sensitively; its role is certain
    /// regardless of attributes or spelling.
    pub fn is_inbox(&self) -> bool {
        self.role == Role::Inbox
    }
}

/// The flags `LIST` answers carry for specials — RFC 6154 plus the
/// names Gmail actually sends.
fn attr_role(attrs: &[NameAttribute<'_>]) -> Option<Role> {
    for attr in attrs {
        // RFC 6154 specials are typed; Gmail's `\Inbox` and the older
        // `\Spam` arrive as extension atoms.
        let role = match attr {
            NameAttribute::Drafts => Some(Role::Drafts),
            NameAttribute::Sent => Some(Role::Sent),
            NameAttribute::Junk => Some(Role::Junk),
            NameAttribute::All | NameAttribute::Archive => Some(Role::Archive),
            NameAttribute::Trash => Some(Role::Trash),
            NameAttribute::Extension(c) => match c.as_ref() {
                "\\Inbox" => Some(Role::Inbox),
                "\\Spam" => Some(Role::Junk),
                _ => None,
            },
            _ => None,
        };
        if role.is_some() {
            return role;
        }
    }
    None
}

/// What a folder is for when the server sends no attributes.
fn name_role(name: &str) -> Role {
    if name.eq_ignore_ascii_case("inbox") {
        return Role::Inbox;
    }
    // Display-name fallbacks for servers that send no attributes.
    let last = name.rsplit(['/', '.']).next().unwrap_or(name);
    match last.to_ascii_lowercase().as_str() {
        "drafts" | "draft" => Role::Drafts,
        "sent" | "sent mail" | "sent items" | "sent messages" => Role::Sent,
        "junk" | "spam" | "bulk mail" => Role::Junk,
        "archive" | "all mail" => Role::Archive,
        "trash" | "deleted" | "deleted items" | "deleted messages" | "bin" => Role::Trash,
        _ => Role::Other,
    }
}

/// A mailbox name as a person's path: decoded, split on the server's
/// own separator, with a leading `INBOX` level dropped — on a server
/// that nests everything under it, "INBOX.Clients" is a top-level folder
/// to whoever reads the rail.
fn segments(name: &str, delimiter: Option<&str>) -> Vec<String> {
    let decoded = decode_mailbox(name);
    let mut parts: Vec<String> = match delimiter.filter(|d| !d.is_empty()) {
        Some(d) => decoded.split(d).map(|p| p.trim().to_string()).collect(),
        None => vec![decoded.trim().to_string()],
    };
    if parts.len() > 1 && parts[0].eq_ignore_ascii_case("inbox") {
        parts.remove(0);
    }
    parts
}

/// RFC 3501 §5.1.3: modified UTF-7, the mailbox-name encoding. A run the
/// decoder can't read stays as it arrived — a strange name still beats a
/// missing folder.
fn decode_mailbox(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[at..]);
            return out;
        };
        let run = &after[..end];
        if run.is_empty() {
            out.push('&');
        } else if let Some(text) = decode_run(run) {
            out.push_str(&text);
        } else {
            out.push_str(&rest[at..at + end + 2]);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// One `&…-` run: base64 with `,` for `/`, unpadded, of UTF-16BE.
fn decode_run(run: &str) -> Option<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(run.replace(',', "/"))
        .ok()?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role_of(attrs: &[NameAttribute<'_>], name: &str) -> Role {
        Folder::listed(name, Some("/"), attrs).role
    }

    #[test]
    fn names_give_every_special_a_display_leaf() {
        let leaf = |name, delim| Folder::listed(name, delim, &[]).display;
        assert_eq!(leaf("INBOX", Some(".")), "Inbox");
        assert_eq!(leaf("[Gmail]/Sent Mail", Some("/")), "Sent Mail");
        assert_eq!(leaf("Personal.Receipts", Some(".")), "Receipts");
        assert_eq!(leaf("Notes", None), "Notes");
    }

    #[test]
    fn the_servers_own_separator_decides_the_hierarchy() {
        // A dot in a "/"-separated server's name is just a dot.
        let f = Folder::listed("Clients/v1.2", Some("/"), &[]);
        assert_eq!((f.display.as_str(), f.depth), ("v1.2", 1));
        // Everything under INBOX. is top-level to a reader.
        let f = Folder::listed("INBOX.Clients", Some("."), &[]);
        assert_eq!((f.path.as_str(), f.depth), ("Clients", 0));
        let f = Folder::listed("INBOX.Clients.Hotels", Some("."), &[]);
        assert_eq!((f.path.as_str(), f.depth), ("Clients/Hotels", 1));
        assert_eq!(f.name, "INBOX.Clients.Hotels", "the wire name is untouched");
    }

    #[test]
    fn greek_and_other_utf7_names_decode() {
        assert_eq!(decode_mailbox("&A5UDuwO7A7cDvQO5A7oDrA-"), "Ελληνικά");
        assert_eq!(decode_mailbox("Entw&APw-rfe"), "Entwürfe");
        assert_eq!(decode_mailbox("Tom &- Jerry"), "Tom & Jerry");
        assert_eq!(decode_mailbox("&Jjo-!"), "☺!");
        // Unreadable runs stay, nothing is lost.
        assert_eq!(decode_mailbox("a&b"), "a&b");
        assert_eq!(decode_mailbox("a&!!!-b"), "a&!!!-b");
        let f = Folder::listed("INBOX.&A5UDuwO7A7cDvQO5A7oDrA-", Some("."), &[]);
        assert_eq!(f.display, "Ελληνικά");
        assert_eq!(f.name, "INBOX.&A5UDuwO7A7cDvQO5A7oDrA-");
    }

    #[test]
    fn a_noselect_parent_is_kept_but_not_selectable() {
        let f = Folder::listed("[Gmail]", Some("/"), &[NameAttribute::NoSelect]);
        assert!(!f.selectable);
        assert_eq!(f.role, Role::Other);
        assert!(Folder::listed("Notes", Some("/"), &[]).selectable);
    }

    #[test]
    fn a_nested_folder_named_like_a_special_is_not_the_special() {
        let role = |name| Folder::listed(name, Some("."), &[]).role;
        assert_eq!(role("Clients.Sent"), Role::Other);
        assert_eq!(role("INBOX.Sent"), Role::Sent);
        assert_eq!(role("INBOX.spam"), Role::Junk);
    }

    #[test]
    fn bare_inbox_is_inbox_without_attributes() {
        assert_eq!(role_of(&[], "INBOX"), Role::Inbox);
        assert_eq!(role_of(&[], "inbox"), Role::Inbox);
        assert_eq!(role_of(&[], "Trash"), Role::Trash);
        assert_eq!(role_of(&[], "All Mail"), Role::Archive);
        assert_eq!(role_of(&[], "Receipts"), Role::Other);
    }

    #[test]
    fn special_use_attributes_beat_names() {
        assert_eq!(
            role_of(&[NameAttribute::Trash], "Deleted Items"),
            Role::Trash
        );
        assert_eq!(
            role_of(&[NameAttribute::All], "[Gmail]/All Mail"),
            Role::Archive
        );
        assert_eq!(
            role_of(&[NameAttribute::Extension("\\Inbox".into())], "Whatever"),
            Role::Inbox
        );
        assert_eq!(
            role_of(&[NameAttribute::Extension("\\Spam".into())], "Bulk"),
            Role::Junk
        );
        assert_eq!(role_of(&[NameAttribute::NoSelect], "[Gmail]"), Role::Other);
    }
}
