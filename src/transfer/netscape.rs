//! The Netscape bookmark file: the one format every browser speaks.
//!
//! Chrome, Edge, Firefox and Safari all export this and all import it,
//! which makes it cryo's bookmark interchange in both directions. The
//! shape that matters:
//!
//! ```text
//! <DL><p>
//!   <DT><H3 PERSONAL_TOOLBAR_FOLDER="true">Bookmarks bar</H3>
//!   <DL><p>
//!     <DT><A HREF="https://a.test" ADD_DATE="1600000000">Title</A>
//!   </DL><p>
//! </DL><p>
//! ```
//!
//! The file is HTML in name only — nobody writes it with a full parser,
//! and the exporters are not consistent about closing tags. This reads
//! it as the tag soup it is: `<H3>` names a folder, the `<DL>` after it
//! opens that folder's scope, `</DL>` closes it, `<A HREF>` is a
//! bookmark in whatever folder is open.
//!
//! Folder convention: the toolbar folder maps to cryo's bar (the empty
//! folder path), so an imported bar lands in the strip the user already
//! looks at.

use crate::bookmarks::{Bookmark, Draft};

/// Folder names other browsers give their toolbar. Edge's export does
/// not always set `PERSONAL_TOOLBAR_FOLDER`, so the name is a fallback.
const BAR_NAMES: [&str; 5] = [
    "bookmarks bar",
    "bookmarks toolbar",
    "favorites bar",
    "bookmark bar",
    "toolbar",
];

/// Deepest nesting followed while reading. A file that opens ten
/// thousand `<DL>`s is malformed or hostile; the bookmarks inside still
/// arrive, filed at the deepest path kept.
const MAX_DEPTH: usize = 16;

/// Read a bookmark file into drafts, in document order.
///
/// Nothing is validated here beyond the format — `Bookmarks::add_all`
/// owns what counts as a keepable bookmark (AGENTS §6), so a file full
/// of `javascript:` bookmarklets is refused there, once, with the rest
/// of the file intact.
pub fn parse(text: &str) -> Vec<Draft> {
    let mut out = Vec::new();
    let mut folders: Vec<String> = Vec::new();
    // The `<H3>` just read, waiting for its `<DL>` to open the scope.
    let mut pending: Option<String> = None;
    let lower = text.to_lowercase();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let Some(rel) = lower[i..].find('<') else {
            break;
        };
        let start = i + rel;
        let Some(close) = lower[start..].find('>') else {
            break;
        };
        let tag_end = start + close + 1;
        let tag = &text[start..tag_end];
        let tag_lower = &lower[start..tag_end];

        if tag_lower.starts_with("<h3") {
            let (name, _) = inner_text(text, tag_end);
            let attrs = tag;
            pending = Some(if is_bar(&name, attrs) {
                String::new()
            } else {
                decode_entities(&name)
            });
        } else if tag_lower.starts_with("<dl") {
            // The outermost `<DL>` is the file's root, which is cryo's
            // bar too — a link sitting directly in it belongs there.
            let name = pending.take().unwrap_or_default();
            if folders.len() < MAX_DEPTH {
                folders.push(name);
            }
        } else if tag_lower.starts_with("</dl") {
            folders.pop();
            pending = None;
        } else if tag_lower.starts_with("<a ") || tag_lower.starts_with("<a\n") {
            if let Some(url) = attribute(tag, "href") {
                let (title, after) = inner_text(text, tag_end);
                out.push(Draft {
                    title: decode_entities(&title),
                    url: decode_entities(&url),
                    folder: path(&folders),
                    added_at: attribute(tag, "add_date")
                        .and_then(|d| d.trim().parse::<u64>().ok())
                        .unwrap_or(0),
                    // The file format carries no icon — the first visit
                    // after the import fills it in.
                    favicon: None,
                });
                i = after;
                continue;
            }
        }
        i = tag_end;
    }
    out
}

/// Is this `<H3>` the browser's toolbar folder?
fn is_bar(name: &str, attrs: &str) -> bool {
    if attribute(attrs, "personal_toolbar_folder").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        return true;
    }
    let name = decode_entities(name).trim().to_lowercase();
    BAR_NAMES.contains(&name.as_str())
}

/// `/`-joined folder path, skipping the root and any unnamed level.
fn path(folders: &[String]) -> String {
    folders
        .iter()
        .filter(|f| !f.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("/")
}

/// Text between a tag and its closing tag, plus where to carry on.
/// Stops at the next `<` either way, so a missing `</A>` — which real
/// exports do contain — costs nothing.
fn inner_text(text: &str, from: usize) -> (String, usize) {
    let rest = &text[from..];
    match rest.find('<') {
        Some(end) => (rest[..end].trim().to_string(), from + end),
        None => (rest.trim().to_string(), text.len()),
    }
}

/// Read one attribute out of a tag. Quoted values win; a bare value is
/// accepted because old exporters wrote `ADD_DATE=1600000000`.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(name) {
        let at = from + rel;
        from = at + name.len();
        // Must be a whole attribute name, not a tail of another one.
        let before_ok = tag[..at]
            .chars()
            .last()
            .is_some_and(|c| c.is_whitespace() || c == '<');
        let after = lower[from..].trim_start();
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let value = after[1..].trim_start();
        let quote = value.chars().next()?;
        return Some(if quote == '"' || quote == '\'' {
            value[1..].split(quote).next().unwrap_or("").to_string()
        } else {
            value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or("")
                .to_string()
        });
    }
    None
}

/// The entities exporters actually emit, plus numeric references.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.find(';').filter(|s| *s <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity.to_lowercase().as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            other => other
                .strip_prefix('#')
                .and_then(|num| match num.strip_prefix('x') {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse::<u32>().ok(),
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Render kept bookmarks as a file Chrome, Edge and Firefox will import.
///
/// `stamped_at` is the file's own `ADD_DATE` for folders — the caller
/// stamps, so no clock is read here.
pub fn render(rows: &[&Bookmark], stamped_at: u64) -> String {
    let mut out = String::with_capacity(rows.len() * 120 + 400);
    out.push_str(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n\
         <!-- This is an automatically generated file.\n\
         \x20    It will be read and overwritten.\n\
         \x20    DO NOT EDIT! -->\n\
         <META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n\
         <TITLE>Bookmarks</TITLE>\n\
         <H1>Bookmarks</H1>\n\
         <DL><p>\n",
    );
    // The bar becomes the toolbar folder, so another browser imports it
    // into its own bar instead of a folder called "cryo".
    let bar: Vec<&&Bookmark> = rows.iter().filter(|b| b.in_bar()).collect();
    out.push_str(&format!(
        "    <DT><H3 ADD_DATE=\"{stamped_at}\" LAST_MODIFIED=\"{stamped_at}\" \
         PERSONAL_TOOLBAR_FOLDER=\"true\">Bookmarks bar</H3>\n    <DL><p>\n"
    ));
    for mark in &bar {
        out.push_str(&link(mark, 2));
    }
    out.push_str("    </DL><p>\n");

    let tree = Tree::build(rows);
    for child in &tree.children {
        child.render_into(&mut out, 1, stamped_at);
    }
    out.push_str("</DL><p>\n");
    out
}

/// One `<DT><A>` line at `depth` levels of indent.
fn link(mark: &Bookmark, depth: usize) -> String {
    format!(
        "{indent}<DT><A HREF=\"{url}\" ADD_DATE=\"{added}\">{title}</A>\n",
        indent = "    ".repeat(depth + 1),
        url = escape(mark.url()),
        added = mark.added_at(),
        title = escape(mark.title()),
    )
}

/// Folder paths are stored flat (`News/Rust`); the file wants them
/// nested, so the writer rebuilds the tree it needs.
#[derive(Default)]
struct Tree<'a> {
    name: String,
    children: Vec<Tree<'a>>,
    marks: Vec<&'a Bookmark>,
}

impl<'a> Tree<'a> {
    fn build(rows: &[&'a Bookmark]) -> Self {
        let mut root = Tree::default();
        for mark in rows.iter().filter(|b| !b.in_bar()) {
            let mut node = &mut root;
            for segment in mark.folder().split('/').filter(|s| !s.is_empty()) {
                let at = match node.children.iter().position(|c| c.name == segment) {
                    Some(at) => at,
                    None => {
                        node.children.push(Tree {
                            name: segment.to_string(),
                            ..Tree::default()
                        });
                        node.children.len() - 1
                    }
                };
                node = &mut node.children[at];
            }
            node.marks.push(mark);
        }
        root
    }

    fn render_into(&self, out: &mut String, depth: usize, stamped_at: u64) {
        let indent = "    ".repeat(depth);
        out.push_str(&format!(
            "{indent}<DT><H3 ADD_DATE=\"{stamped_at}\" LAST_MODIFIED=\"{stamped_at}\">{}</H3>\n\
             {indent}<DL><p>\n",
            escape(&self.name)
        ));
        for mark in &self.marks {
            out.push_str(&link(mark, depth));
        }
        for child in &self.children {
            child.render_into(out, depth + 1, stamped_at);
        }
        out.push_str(&format!("{indent}</DL><p>\n"));
    }
}

/// Escape what HTML cannot carry raw. Titles come from web pages, so
/// this is the line between a bookmark title and markup in the file.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Does this text look like a bookmark file? Used to sniff a picked
/// file, so it answers on the first few hundred bytes.
pub fn looks_like(text: &str) -> bool {
    let head = text
        .get(..text.len().min(2048))
        .unwrap_or(text)
        .to_lowercase();
    head.contains("netscape-bookmark-file")
        || (head.contains("<dl") && head.contains("<a href"))
        || (head.contains("<dt") && head.contains("<a href"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bookmarks::Bookmarks;

    const CHROME_EXPORT: &str = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<!-- This is an automatically generated file. -->
<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=UTF-8">
<TITLE>Bookmarks</TITLE>
<H1>Bookmarks</H1>
<DL><p>
    <DT><H3 ADD_DATE="1600000000" LAST_MODIFIED="1600000001" PERSONAL_TOOLBAR_FOLDER="true">Bookmarks bar</H3>
    <DL><p>
        <DT><A HREF="https://bar.test/" ADD_DATE="1600000002">On the bar</A>
        <DT><H3 ADD_DATE="1600000003">News</H3>
        <DL><p>
            <DT><A HREF="https://news.test/" ADD_DATE="1600000004">News &amp; Views</A>
            <DT><H3 ADD_DATE="1600000005">Rust</H3>
            <DL><p>
                <DT><A HREF="https://blog.rust-lang.org/" ADD_DATE="1600000006">Rust Blog</A>
            </DL><p>
        </DL><p>
    </DL><p>
    <DT><H3 ADD_DATE="1600000007">Other bookmarks</H3>
    <DL><p>
        <DT><A HREF="https://other.test/" ADD_DATE="1600000008">Elsewhere</A>
    </DL><p>
</DL><p>
"#;

    #[test]
    fn reads_a_chrome_export_with_its_folders() {
        let drafts = parse(CHROME_EXPORT);
        assert_eq!(drafts.len(), 4);
        assert_eq!(drafts[0].url, "https://bar.test/");
        assert_eq!(drafts[0].folder, "", "toolbar items belong on the bar");
        assert_eq!(drafts[0].added_at, 1_600_000_002);
        assert_eq!(drafts[1].folder, "News");
        assert_eq!(drafts[1].title, "News & Views");
        assert_eq!(drafts[2].folder, "News/Rust");
        assert_eq!(drafts[3].folder, "Other bookmarks");
    }

    #[test]
    fn a_firefox_toolbar_folder_also_lands_on_the_bar() {
        let text = r#"<DL><p>
            <DT><H3>Bookmarks Toolbar</H3>
            <DL><p><DT><A HREF="https://a.test">A</A></DL><p>
        </DL><p>"#;
        assert_eq!(parse(text)[0].folder, "");
    }

    #[test]
    fn links_at_the_root_belong_to_the_bar() {
        let text = r#"<DL><p><DT><A HREF="https://a.test">A</A></DL><p>"#;
        assert_eq!(parse(text)[0].folder, "");
    }

    #[test]
    fn missing_closing_tags_do_not_lose_bookmarks() {
        // Real exports leave `</A>` and `</DT>` off.
        let text =
            "<DL><p><DT><A HREF=\"https://a.test\">A\n<DT><A HREF=\"https://b.test\">B\n</DL>";
        let drafts = parse(text);
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts[0].title, "A");
        assert_eq!(drafts[1].url, "https://b.test");
    }

    #[test]
    fn unquoted_and_single_quoted_attributes_both_read() {
        let text = "<DT><A HREF='https://a.test' ADD_DATE=1600000000>A</A>";
        let drafts = parse(text);
        assert_eq!(drafts[0].url, "https://a.test");
        assert_eq!(drafts[0].added_at, 1_600_000_000);
    }

    #[test]
    fn entities_decode_including_numeric_ones() {
        assert_eq!(decode_entities("a &amp; b"), "a & b");
        assert_eq!(decode_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_entities("&quot;q&quot;"), "\"q\"");
        assert_eq!(decode_entities("&#39;s"), "'s");
        assert_eq!(decode_entities("&#x27;s"), "'s");
        assert_eq!(decode_entities("&#9731;"), "☃");
        // Not an entity: left alone rather than eaten.
        assert_eq!(decode_entities("a & b"), "a & b");
        assert_eq!(decode_entities("&notanentity;"), "&notanentity;");
    }

    #[test]
    fn a_url_holding_an_entity_decodes_back() {
        let text = r#"<DT><A HREF="https://a.test/?x=1&amp;y=2">A</A>"#;
        assert_eq!(parse(text)[0].url, "https://a.test/?x=1&y=2");
    }

    #[test]
    fn an_anchor_without_an_href_is_skipped() {
        let text = r#"<DL><p><DT><A NAME="anchor">x</A><DT><A HREF="https://a.test">A</A></DL>"#;
        let drafts = parse(text);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].url, "https://a.test");
    }

    #[test]
    fn runaway_nesting_is_bounded_but_keeps_the_bookmarks() {
        let mut text = String::new();
        for i in 0..MAX_DEPTH + 20 {
            text.push_str(&format!("<DT><H3>d{i}</H3><DL><p>"));
        }
        text.push_str(r#"<DT><A HREF="https://deep.test">Deep</A>"#);
        let drafts = parse(&text);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].folder.split('/').count(), MAX_DEPTH);
    }

    #[test]
    fn parsing_an_empty_or_unrelated_file_yields_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("<html><body>hello</body></html>").is_empty());
    }

    #[test]
    fn looks_like_recognizes_exports_and_refuses_other_files() {
        assert!(looks_like(CHROME_EXPORT));
        assert!(looks_like("<DL><DT><A HREF=\"https://a\">a</A>"));
        assert!(!looks_like("url,username,password\n"));
        assert!(!looks_like("{\"roots\":{}}"));
        assert!(!looks_like(""));
    }

    /// The round trip that matters: export what we keep, read it back,
    /// and land on the same bookmarks in the same folders.
    #[test]
    fn export_then_import_is_lossless() {
        let mut marks = Bookmarks::default();
        for (title, url, folder) in [
            ("On the bar", "https://bar.test/", ""),
            (
                "Quotes \"and\" <angles> & amps",
                "https://odd.test/?a=1&b=2",
                "",
            ),
            ("News", "https://news.test/", "News"),
            ("Rust Blog", "https://blog.rust-lang.org/", "News/Rust"),
            ("Elsewhere", "https://other.test/", "Other bookmarks"),
        ] {
            marks
                .add(&Draft {
                    title: title.to_string(),
                    url: url.to_string(),
                    folder: folder.to_string(),
                    added_at: 1_700_000_000,
                    favicon: None,
                })
                .unwrap();
        }
        let rows: Vec<&Bookmark> = marks.rows().iter().collect();
        let html = render(&rows, 1_700_000_999);

        let mut back = Bookmarks::default();
        let report = back.add_all(&parse(&html));
        assert_eq!(report.added, 5);
        assert_eq!(report.rejected, 0);
        let mut before: Vec<(String, String, String, u64)> = marks
            .rows()
            .iter()
            .map(|b| {
                (
                    b.title().into(),
                    b.url().into(),
                    b.folder().into(),
                    b.added_at(),
                )
            })
            .collect();
        let mut after: Vec<(String, String, String, u64)> = back
            .rows()
            .iter()
            .map(|b| {
                (
                    b.title().into(),
                    b.url().into(),
                    b.folder().into(),
                    b.added_at(),
                )
            })
            .collect();
        before.sort();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn the_rendered_file_declares_the_format_and_the_toolbar() {
        let mut marks = Bookmarks::default();
        marks
            .add(&Draft {
                title: "A".into(),
                url: "https://a.test".into(),
                folder: String::new(),
                added_at: 5,
                favicon: None,
            })
            .unwrap();
        let rows: Vec<&Bookmark> = marks.rows().iter().collect();
        let html = render(&rows, 9);
        assert!(html.starts_with("<!DOCTYPE NETSCAPE-Bookmark-file-1>"));
        assert!(html.contains("PERSONAL_TOOLBAR_FOLDER=\"true\""));
        assert!(html.contains("<DT><A HREF=\"https://a.test\" ADD_DATE=\"5\">A</A>"));
        assert!(looks_like(&html));
    }

    #[test]
    fn an_empty_registry_still_renders_a_valid_file() {
        let html = render(&[], 1);
        assert!(looks_like(&html));
        assert!(parse(&html).is_empty());
    }
}
