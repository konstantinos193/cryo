//! The mail page: a real shell document at [`MAIL_URL`], one folder
//! holding its markup, style and script.
//!
//! Like every content surface it has no `/cmd`: it talks to the app
//! over `window.ipc.postMessage`, which lands as a `PageMessage`
//! carrying this origin — `app/mail.rs` accepts commands only from a
//! tab whose address is this URL, so a web page that guessed the wire
//! cannot drive the mailbox. Everything it shows arrives by eval
//! (`window.__cryo_mail`); nothing mail-shaped is fetchable over the
//! internal protocol.

/// The mail document, served at [`MAIL_URL`].
pub(crate) const HTML: &str = include_str!("mail/mail.html");

/// Its stylesheet, served at `/mail/mail.css`.
pub(crate) const STYLE: &str = include_str!("mail/mail.css");

/// Its script, served at `/mail/mail.js`.
pub(crate) const SCRIPT: &str = include_str!("mail/mail.js");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_drives_the_app_through_page_messages_only() {
        // The one channel: window.ipc, never a fetchable /cmd.
        assert!(SCRIPT.contains("window.ipc.postMessage"));
        assert!(!SCRIPT.contains("/cmd"));
        assert!(!HTML.contains("/cmd"));
    }

    #[test]
    fn the_page_defines_the_push_entry_point_the_app_evals() {
        assert!(SCRIPT.contains("window.__cryo_mail"));
    }

    #[test]
    fn the_page_and_the_wire_agree_on_command_names() {
        for kind in [
            "hello",
            "select_folder",
            "open",
            "sync",
            "set_flag",
            "move_to",
            "delete",
            "add_account",
            "remove_account",
            "send",
            "save_draft",
            "save_attachment",
            "attach_pick",
            "attach_drop",
            "attach_clear",
            "guess",
        ] {
            assert!(
                SCRIPT.contains(&format!("kind: '{kind}'")),
                "page never sends {kind}"
            );
        }
    }

    #[test]
    fn the_page_and_the_wire_agree_on_push_names() {
        for kind in [
            "state",
            "list",
            "body",
            "flag",
            "status",
            "account_reply",
            "send_reply",
            "guess",
            "attachment_saved",
            "attachments",
            "error",
        ] {
            assert!(
                SCRIPT.contains(&format!("'{kind}'")),
                "page never handles a {kind} push"
            );
        }
    }

    /// Remote content stays a Rust-side decision — the document must
    /// not go around the sanitizer and fetch anything itself.
    #[test]
    fn the_document_fetches_nothing() {
        assert!(!SCRIPT.contains("fetch("));
        assert!(!SCRIPT.contains("XMLHttpRequest"));
        assert!(!SCRIPT.contains("<img"));
    }
}
