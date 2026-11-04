//! Folders: the mailboxes an account lists, and which one is the
//! inbox. `Role` is how the UI groups specials without trusting their
//! display names — a server may call trash "Deleted Items".

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
    pub role: Role,
    /// Unread and total counts from the last sync — `None` before the
    /// folder has ever been selected.
    pub unseen: Option<u32>,
    pub exists: Option<u32>,
}

impl Folder {
    /// IMAP reports INBOX's name case-sensitively; its role is certain
    /// regardless of attributes or spelling.
    pub fn is_inbox(&self) -> bool {
        self.role == Role::Inbox
    }
}

/// The flags `LIST` answers carry for specials — RFC 6154 plus the
/// names Gmail actually sends.
pub(super) fn role_of(attrs: &[imap::types::NameAttribute<'_>], name: &str) -> Role {
    for attr in attrs {
        // Special-use flags arrive as Custom atoms — the enum's own
        // variants are structural (\Noselect and friends).
        let imap::types::NameAttribute::Custom(c) = attr else {
            continue;
        };
        let role = match c.as_ref() {
            "\\Inbox" => Some(Role::Inbox),
            "\\Drafts" => Some(Role::Drafts),
            "\\Sent" => Some(Role::Sent),
            "\\Junk" | "\\Spam" => Some(Role::Junk),
            "\\All" | "\\Archive" => Some(Role::Archive),
            "\\Trash" => Some(Role::Trash),
            _ => None,
        };
        if let Some(role) = role {
            return role;
        }
    }
    if name.eq_ignore_ascii_case("inbox") {
        return Role::Inbox;
    }
    // Display-name fallbacks for servers that send no attributes.
    let last = name.rsplit(['/', '.']).next().unwrap_or(name);
    match last.to_ascii_lowercase().as_str() {
        "drafts" | "draft" => Role::Drafts,
        "sent" | "sent mail" | "sent items" => Role::Sent,
        "junk" | "spam" | "bulk mail" => Role::Junk,
        "archive" | "all mail" => Role::Archive,
        "trash" | "deleted" | "deleted items" | "bin" => Role::Trash,
        _ => Role::Other,
    }
}

/// The leaf the UI shows: "INBOX" or the part after the last
/// separator. `[Gmail]/Sent Mail` displays as "Sent Mail".
pub(super) fn display_name(name: &str) -> String {
    if name.eq_ignore_ascii_case("inbox") {
        return "Inbox".into();
    }
    name.rsplit(['/', '.'])
        .next()
        .unwrap_or(name)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_give_every_special_a_display_leaf() {
        assert_eq!(display_name("INBOX"), "Inbox");
        assert_eq!(display_name("[Gmail]/Sent Mail"), "Sent Mail");
        assert_eq!(display_name("Personal.Receipts"), "Receipts");
        assert_eq!(display_name("Notes"), "Notes");
    }

    #[test]
    fn bare_inbox_is_inbox_without_attributes() {
        assert_eq!(role_of(&[], "INBOX"), Role::Inbox);
        assert_eq!(role_of(&[], "inbox"), Role::Inbox);
        assert_eq!(role_of(&[], "Trash"), Role::Trash);
        assert_eq!(role_of(&[], "All Mail"), Role::Archive);
        assert_eq!(role_of(&[], "Receipts"), Role::Other);
    }
}
