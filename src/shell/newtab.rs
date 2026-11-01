//! The home page: what the window opens on, and what every new tab shows.
//!
//! Document, stylesheet and script live in `newtab/`, one file each, served
//! as three requests — the same shape as the chrome document. The `<title>`
//! matters, because it is what the tab strip shows.
//!
//! The address box navigates the view itself rather than talking to the
//! shell. Content views deliberately have no IPC handler, because the
//! internal protocol answers any origin: wiring `/cmd` into a content view
//! would let any website drive the browser. So the page reads only what
//! that protocol already serves every origin, `/dials` and `/engines`,
//! and keeps the reader's own edits to the tile row in this origin's
//! localStorage beside the search-engine pick.

/// The new-tab document, served at [`crate::shell::pages::NEW_TAB_URL`].
pub(crate) const HTML: &str = include_str!("newtab/newtab.html");

/// Its stylesheet, served at `/newtab/newtab.css`.
pub(crate) const STYLE: &str = include_str!("newtab/newtab.css");

/// Its script, served at `/newtab/newtab.js`.
pub(crate) const SCRIPT: &str = include_str!("newtab/newtab.js");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigates_itself_and_keeps_no_command_channel() {
        assert!(SCRIPT.contains("location.href"));
        assert!(!SCRIPT.contains("/cmd"));
        assert!(!HTML.contains("/cmd"));
    }

    #[test]
    fn refuses_javascript_urls_from_its_own_origin() {
        assert!(SCRIPT.contains("/^javascript:/i.test(s)"));
    }

    /// A stored tile is read back from localStorage and turned into a
    /// link, so the scheme check has to sit in front of the store, not
    /// only in front of the input box.
    #[test]
    fn stored_tiles_are_held_to_http_schemes() {
        assert!(SCRIPT.contains("parsed.protocol !== \"https:\""));
        assert!(SCRIPT.contains("parsed.protocol !== \"http:\""));
    }

    /// `[hidden] { display: none }` is one attribute selector, so an id
    /// rule with a `display` of its own outranks it and the element
    /// shows anyway. The dialog did exactly that: it opened on load and
    /// never shut.
    #[test]
    fn an_id_styled_element_the_document_hides_stays_hidden() {
        for tag in HTML.split('<') {
            let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
            if !tag.contains(" hidden") {
                continue;
            }
            let Some((_, rest)) = tag.split_once("id=\"") else {
                continue;
            };
            let id = &rest[..rest.find('"').expect("an id attribute is quoted")];
            let Some((_, body)) = STYLE.split_once(&format!("#{id} {{")) else {
                continue;
            };
            let rule = &body[..body.find('}').unwrap_or(body.len())];
            if rule.contains("display") {
                assert!(
                    STYLE.contains(&format!("#{id}[hidden]")),
                    "#{id} sets its own display and is hidden in the \
                     document, so it needs its own [hidden] rule"
                );
            }
        }
    }

    #[test]
    fn dial_icons_leave_the_origin_out_of_the_request() {
        assert!(SCRIPT.contains("img.referrerPolicy = \"no-referrer\""));
    }

    #[test]
    fn reads_only_the_paths_a_content_view_may_fetch() {
        // The comment above each call names the same path, so dedupe.
        let mut fetched: Vec<&str> = SCRIPT
            .match_indices("fetch(\"")
            .map(|(at, _)| {
                let rest = &SCRIPT[at + 7..];
                &rest[..rest.find('"').expect("fetch argument is a literal")]
            })
            .collect();
        fetched.sort_unstable();
        fetched.dedup();
        assert_eq!(fetched, ["/dials", "/engines"]);
    }
}
