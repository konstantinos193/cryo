//! Sanitizing message HTML for the reading pane.
//!
//! Mail HTML is adversarial input rendered inside the shell's own
//! origin — a script, an injected form or a `javascript:` link here
//! would run beside the document that owns `window.ipc`. So the policy
//! is strict and all on this side of the wire:
//!
//! - an allowlist of tags and attributes — no `class`/`id` (a class
//!   name colliding with the page's own could restyle the UI), no
//!   script/iframe/form/meta/style;
//! - `href` limited to `http`, `https` and `mailto`, every anchor
//!   forced `target="_blank"` so a click lands in a new tab instead of
//!   navigating the mail tab;
//! - `img` limited to self-contained `data:` sources — remote images
//!   are tracking pixels and are stripped, not loaded — unless the
//!   reader asked for this message's remote content, in which case
//!   `http(s)` sources pass with `referrerpolicy="no-referrer"` forced
//!   on, so even an opted-in load says nothing about where it came
//!   from;
//! - inline `style` is kept (emails lay out with it) declaration by
//!   declaration — `url(`, `expression(`, `position`, `z-index`,
//!   `behavior`/`binding` and `@import` never survive (`url(http…)`
//!   only when remote content was asked for);
//! - the count of suppressed remote references rides along for the
//!   "remote content blocked" badge.

use std::collections::HashSet;

/// Above this the message is treated as a text mail regardless of its
/// HTML part — a multi-megabyte "newsletter" is a RAM claim, not a
/// body.
const MAX_HTML_BYTES: usize = 4 * 1024 * 1024;

/// What [`clean`] found besides the markup — how many remote fetches it
/// refused.
pub struct Cleaned {
    pub html: String,
    pub blocked_remote: usize,
}

fn is_remote(value: &str) -> bool {
    let v = value.trim_start();
    v.starts_with("http://") || v.starts_with("https://") || v.starts_with("//")
}

/// A `url(…)` whose target is a plain web fetch — what an opted-in
/// load may keep. Anything else inside `url(` (`data:`, `javascript:`,
/// a relative path) stays banned.
fn is_web_url_decl(lower: &str) -> bool {
    let Some(at) = lower.find("url(") else {
        return false;
    };
    let inner = lower[at + 4..].trim_start().trim_start_matches(['\'', '"']);
    inner.starts_with("https://") || inner.starts_with("http://")
}

/// A declaration-list filter for `style="…"`: keeps the declaration
/// unless it can position over the chrome, run code, or fetch.
fn scrub_style(
    value: &str,
    allow_remote: bool,
    blocked: &std::sync::atomic::AtomicUsize,
) -> Option<String> {
    use std::sync::atomic::Ordering;
    const BANNED: &[&str] = &[
        "url(",
        "expression(",
        "position:",
        "z-index:",
        "behavior:",
        "binding:",
        "@import",
        "javascript:",
        "-moz-binding",
    ];
    let mut out = String::with_capacity(value.len());
    for decl in value.split(';') {
        let lower = decl.to_ascii_lowercase();
        let banned = BANNED.iter().any(|b| lower.contains(b));
        let fetch = lower.contains("url(");
        let kept_fetch = allow_remote
            && fetch
            && is_web_url_decl(&lower)
            && !BANNED[1..].iter().any(|b| lower.contains(b));
        if banned && !kept_fetch {
            // Only fetches count toward the badge — a dropped `position:
            // fixed` is layout policy, not blocked remote content.
            if fetch {
                blocked.fetch_add(1, Ordering::Relaxed);
            }
            continue;
        }
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(decl.trim());
    }
    (!out.is_empty()).then_some(out)
}

/// Email display HTML → markup the reading pane may inject verbatim.
/// `allow_remote` is the reader's opt-in for this one message: remote
/// images pass, every other rule holds.
pub fn clean(html: &str, allow_remote: bool) -> Cleaned {
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let blocked = Arc::new(AtomicUsize::new(0));
    if html.len() > MAX_HTML_BYTES {
        return Cleaned {
            html: String::new(),
            blocked_remote: 0,
        };
    }
    let filter_blocked = blocked.clone();
    let tags: HashSet<&str> = [
        "a",
        "abbr",
        "address",
        "article",
        "aside",
        "b",
        "bdi",
        "bdo",
        "big",
        "blockquote",
        "br",
        "caption",
        "center",
        "cite",
        "code",
        "col",
        "colgroup",
        "dd",
        "del",
        "dfn",
        "div",
        "dl",
        "dt",
        "em",
        "figcaption",
        "figure",
        "font",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hr",
        "i",
        "img",
        "ins",
        "kbd",
        "li",
        "main",
        "mark",
        "ol",
        "p",
        "pre",
        "q",
        "rp",
        "rt",
        "ruby",
        "s",
        "samp",
        "section",
        "small",
        "span",
        "strike",
        "strong",
        "sub",
        "sup",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "time",
        "tr",
        "tt",
        "u",
        "ul",
        "var",
        "wbr",
    ]
    .into_iter()
    .collect();
    // The filter only sees attributes the allowlist lets through — so
    // the attrs that need *scrubbing* (style, href, src) are allowed
    // here and the filter decides their values. Everything else —
    // class, id, on*, data-* — falls away before the filter runs.
    let generic: HashSet<&str> = [
        "style", "title", "dir", "lang", "align", "valign", "bgcolor",
    ]
    .into_iter()
    .collect();
    let mut tag_attributes: std::collections::HashMap<&str, HashSet<&str>> =
        std::collections::HashMap::new();
    tag_attributes.insert("a", ["href", "name"].into_iter().collect());
    tag_attributes.insert(
        "img",
        ["src", "alt", "width", "height"].into_iter().collect(),
    );
    tag_attributes.insert(
        "table",
        ["width", "border", "cellpadding", "cellspacing"]
            .into_iter()
            .collect(),
    );
    tag_attributes.insert(
        "td",
        ["colspan", "rowspan", "width", "height"]
            .into_iter()
            .collect(),
    );
    tag_attributes.insert(
        "th",
        ["colspan", "rowspan", "width", "height"]
            .into_iter()
            .collect(),
    );
    tag_attributes.insert("col", ["span", "width"].into_iter().collect());
    tag_attributes.insert("font", ["color", "face", "size"].into_iter().collect());
    tag_attributes.insert("ol", ["start", "type"].into_iter().collect());
    tag_attributes.insert("ul", ["type"].into_iter().collect());
    tag_attributes.insert("li", ["value"].into_iter().collect());
    tag_attributes.insert("blockquote", ["cite"].into_iter().collect());
    tag_attributes.insert("q", ["cite"].into_iter().collect());
    let mut builder = ammonia::Builder::new();
    builder
        .tags(tags)
        .generic_attributes(generic)
        .tag_attributes(tag_attributes)
        // Links get their rel here; the target is forced by the
        // post-pass below.
        .link_rel(Some("noopener noreferrer"))
        .url_schemes(
            ["http", "https", "mailto", "data", "cid"]
                .into_iter()
                .collect(),
        )
        .attribute_filter(move |elem, attr, value| match (elem, attr) {
            ("img", "src") => {
                let v = value.trim();
                if v.starts_with("data:image/")
                    || (allow_remote && is_remote(v) && !v.starts_with("//"))
                {
                    Some(Cow::Borrowed(value))
                } else {
                    if is_remote(v) || !v.is_empty() {
                        filter_blocked.fetch_add(1, Ordering::Relaxed);
                    }
                    None
                }
            }
            ("img", "srcset" | "background") => {
                filter_blocked.fetch_add(1, Ordering::Relaxed);
                None
            }
            ("a", "href") => {
                let v = value.trim_start();
                if v.starts_with("http://") || v.starts_with("https://") || v.starts_with("mailto:")
                {
                    Some(Cow::Borrowed(value))
                } else {
                    None
                }
            }
            (_, "style") => scrub_style(value, allow_remote, &filter_blocked).map(Cow::Owned),
            _ => Some(Cow::Borrowed(value)),
        });
    let html = builder.clean(html).to_string();
    // `target` isn't an attribute ammonia will add — force it on the
    // normalized markup so a clicked link can never navigate this tab.
    // Likewise every image, loaded or inline, sends no referrer and
    // waits until it scrolls into view.
    let html = html
        .replace("<a ", "<a target=\"_blank\" rel=\"noopener noreferrer\" ")
        .replace(
            "<img ",
            "<img referrerpolicy=\"no-referrer\" loading=\"lazy\" ",
        );
    Cleaned {
        html,
        blocked_remote: blocked.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_and_active_markup_never_survive() {
        for payload in [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<iframe src=\"http://evil\"></iframe>",
            "<form action=\"http://evil\"><input name=p></form>",
            "<meta http-equiv=refresh content=\"0;url=http://evil\">",
            "<svg onload=alert(1)></svg>",
            "<a href=\"data:text/html,<script>x</script>\">x</a>",
        ] {
            let out = clean(payload, false).html;
            for needle in [
                "script",
                "iframe",
                "onerror",
                "onload",
                "javascript",
                "<form",
                "<meta",
                "<svg",
                "<input",
            ] {
                assert!(
                    !out.to_ascii_lowercase().contains(needle),
                    "{payload} -> {out}"
                );
            }
        }
    }

    #[test]
    fn remote_content_is_stripped_and_counted() {
        const MARKUP: &str =
            "<img src=\"http://tracker/x.gif\"><p style=\"background:url('http://t/y')\">hi</p>";
        let out = clean(MARKUP, false);
        assert_eq!(out.blocked_remote, 2);
        assert!(!out.html.contains("http://"));
        assert!(!out.html.contains("url("));
        // Self-contained images survive.
        let ok = clean("<img src=\"data:image/png;base64,zz\">", false).html;
        assert!(ok.contains("data:image/png;base64,zz"));
        // Opted in: web fetches pass, without a referrer, nothing else
        // changes.
        let loaded = clean(MARKUP, true);
        assert_eq!(loaded.blocked_remote, 0);
        assert!(loaded.html.contains("http://tracker/x.gif"));
        assert!(loaded.html.contains("referrerpolicy=\"no-referrer\""));
        assert!(loaded.html.contains("url('http://t/y')"), "{}", loaded.html);
        let still = clean(
            "<img src=\"//proto-relative/x\"><p style=\"background:url(javascript:1)\">x</p>",
            true,
        );
        assert_eq!(still.blocked_remote, 2);
        assert!(!still.html.contains("proto-relative"));
        assert!(!still.html.contains("url("));
    }

    #[test]
    fn layout_weapons_and_ui_collisions_are_removed() {
        let out = clean(
            "<div class=\"menu-pop\" id=\"tabs\" style=\"position:fixed;z-index:9;color:red\">x</div>",
            false,
        )
        .html;
        for needle in ["class", "id", "position", "z-index"] {
            assert!(!out.contains(needle), "{out}");
        }
        assert!(out.contains("color:red"));
    }

    #[test]
    fn links_open_elsewhere_and_only_safe_schemes() {
        let out = clean(
            "<a href=\"https://a.test\">a</a><a href=\"ftp://f\">f</a>",
            false,
        )
        .html;
        assert!(out.contains("target=\"_blank\""));
        assert!(!out.contains("ftp://"));
        // mailto is a link a mail client must keep.
        assert!(clean("<a href=\"mailto:a@b.test\">m</a>", false)
            .html
            .contains("mailto:"));
    }
}
