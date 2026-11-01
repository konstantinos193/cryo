//! The `InBody` insertion mode, and the adoption agency algorithm.
//!
//! `InBody` is where almost every document spends its time, and almost
//! every rule in it is a recovery rule. `<p>` cannot nest, so a second
//! one closes the first. `<li>` closes the previous `<li>`. An end tag
//! for an element that is not open is either ignored or closes several —
//! and which it does depends on whether something "special" sits between.
//!
//! The adoption agency is the reason this mode is not a short function.
//! Formatting elements and block elements can be *misnested*:
//!
//! ```text
//!   <p>1<b>2<i>3</b>4</i>5</p>
//! ```
//!
//! The `</b>` closes an element whose children overlap an `<i>` that is
//! still open. There is no tree that preserves the source nesting, so the
//! algorithm reparents and *clones* elements until there is one — here,
//! producing an `<i>` inside the `<b>` and a second `<i>` after it, so
//! that "3" is both bold and italic while "4" is only italic. Every
//! browser agrees on the result because they all run this algorithm, and
//! an engine that does anything else renders real pages visibly wrong.

use super::stack::{Formatting, Scope};
use super::{Mode, Next, TreeBuilder, HEAD_ELEMENTS, VOID_ELEMENTS};
use crate::engine::cryo_impl::dom::{NodeId, QuirksMode};
use crate::engine::cryo_impl::html::tokenizer::{State, Tag, Token};

/// Block-level containers that close an open `<p>` and are closed by
/// their own end tag in default scope.
const BLOCK_ELEMENTS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "main",
    "menu",
    "nav",
    "ol",
    // `p` belongs here: a `<p>` start tag closes an open one, which is
    // why `<p>one<p>two` is two siblings and not a nest.
    "p",
    "section",
    "summary",
    "ul",
];

const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

/// The formatting elements: inline elements the parser will clone across
/// a block boundary to preserve their effect.
const FORMATTING_ELEMENTS: &[&str] = &[
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];

/// Elements whose end tag is implied by almost anything else starting.
const IMPLIED_END_TAGS: &[&str] = &[
    "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc",
];

/// The "special" elements.
///
/// Their only job is to act as a barrier: an end tag searching inward for
/// its element stops at the first special one, and the adoption agency
/// looks for the innermost special element above a formatting element.
/// That is what makes `</b>` close a `<b>` through a `<span>` but not
/// through a `<div>`.
const SPECIAL_ELEMENTS: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "keygen",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];

/// How many times the adoption agency's outer loop may run.
///
/// The spec's bound, and it is load-bearing rather than defensive:
/// pathologically misnested markup can otherwise reparent forever, and
/// stopping at eight is what every browser does, so the resulting tree
/// matches theirs.
const ADOPTION_AGENCY_ROUNDS: usize = 8;

impl TreeBuilder {
    pub(super) fn in_body(&mut self, token: Token) -> Next {
        match token {
            // A literal NUL in body content is dropped entirely.
            Token::Character('\0') => Next::Done,
            Token::Character(c) => {
                if self.ignore_next_newline {
                    self.ignore_next_newline = false;
                    if c == '\n' {
                        return Next::Done;
                    }
                }
                self.reconstruct_formatting();
                self.insert_text(c);
                Next::Done
            }
            Token::Comment(data) => {
                self.insert_comment(&data);
                Next::Done
            }
            Token::Doctype { .. } => Next::Done,
            Token::StartTag(tag) => self.in_body_start(tag),
            Token::EndTag(tag) => self.in_body_end(tag),
            Token::Eof => {
                self.stopped = true;
                Next::Done
            }
        }
    }

    fn in_body_start(&mut self, tag: Tag) -> Next {
        match tag.name.as_str() {
            // A second `<html>` or `<body>` merges its attributes rather
            // than nesting a second one.
            "html" => {
                if let Some(root) = self.open.at(0) {
                    self.merge_attributes(root, &tag);
                }
                Next::Done
            }
            "body" => {
                if let Some(body) = self.open.at(1) {
                    self.merge_attributes(body, &tag);
                }
                Next::Done
            }
            // Head content anywhere in the body still belongs to the head
            // rules — notably `<script>` and `<style>`, which must switch
            // the tokenizer into a raw-text state wherever they appear.
            name if HEAD_ELEMENTS.contains(&name) => self.in_head(Token::StartTag(tag)),

            name if BLOCK_ELEMENTS.contains(&name) => {
                self.close_p_if_open();
                self.insert_element(&tag);
                Next::Done
            }
            name if HEADINGS.contains(&name) => {
                self.close_p_if_open();
                // Headings do not nest: `<h1><h2>` closes the h1.
                if self
                    .open
                    .current()
                    .is_some_and(|id| HEADINGS.iter().any(|&h| self.doc.is_html_element(id, h)))
                {
                    self.open.pop();
                }
                self.insert_element(&tag);
                Next::Done
            }
            "pre" | "listing" => {
                self.close_p_if_open();
                self.insert_element(&tag);
                self.ignore_next_newline = true;
                Next::Done
            }
            // Nested forms are dropped; the pointer is how that is known.
            "form" => {
                if self.form.is_some() {
                    return Next::Done;
                }
                self.close_p_if_open();
                let id = self.insert_element(&tag);
                self.form = Some(id);
                Next::Done
            }
            "li" => {
                self.generate_implied_end_tags();
                if self.open.has_in_scope(&self.doc, "li", Scope::ListItem) {
                    self.open.pop_until_html(&self.doc, "li");
                }
                self.close_p_if_open();
                self.insert_element(&tag);
                Next::Done
            }
            "dd" | "dt" => {
                self.generate_implied_end_tags();
                for name in ["dd", "dt"] {
                    if self.open.has_in_scope(&self.doc, name, Scope::Default) {
                        self.open.pop_until_html(&self.doc, name);
                        break;
                    }
                }
                self.close_p_if_open();
                self.insert_element(&tag);
                Next::Done
            }
            "plaintext" => {
                self.close_p_if_open();
                self.insert_element(&tag);
                self.tokenizer.set_state(State::Plaintext);
                Next::Done
            }
            "button" => {
                if self.open.has_in_scope(&self.doc, "button", Scope::Default) {
                    self.generate_implied_end_tags();
                    self.open.pop_until_html(&self.doc, "button");
                }
                self.reconstruct_formatting();
                self.insert_element(&tag);
                Next::Done
            }

            // An `<a>` inside an open `<a>` closes the first one: links
            // never nest, however the markup is written.
            "a" => {
                if let Some(open_anchor) = self.formatting.last_between_markers(&self.doc, "a") {
                    self.adoption_agency("a");
                    self.formatting.remove(open_anchor);
                    self.open.remove(open_anchor);
                }
                self.insert_formatting(&tag);
                Next::Done
            }
            // `<nobr>` is the other element that cannot contain itself.
            "nobr" => {
                self.reconstruct_formatting();
                if self.open.has_in_scope(&self.doc, "nobr", Scope::Default) {
                    self.adoption_agency("nobr");
                }
                self.insert_formatting(&tag);
                Next::Done
            }
            name if FORMATTING_ELEMENTS.contains(&name) => {
                self.insert_formatting(&tag);
                Next::Done
            }
            // These isolate formatting: a marker stops reconstruction
            // leaking inside them.
            "applet" | "marquee" | "object" => {
                self.reconstruct_formatting();
                self.insert_element(&tag);
                self.formatting.push_marker();
                Next::Done
            }

            "hr" => {
                self.close_p_if_open();
                self.insert_void(&tag);
                Next::Done
            }
            "textarea" => {
                self.insert_element(&tag);
                self.ignore_next_newline = true;
                self.tokenizer.set_state(State::Rcdata);
                self.original_mode = self.mode;
                self.mode = Mode::Text;
                Next::Done
            }
            "xmp" => {
                self.close_p_if_open();
                self.reconstruct_formatting();
                self.parse_raw_text(&tag, State::Rawtext);
                Next::Done
            }
            "iframe" | "noembed" => {
                self.parse_raw_text(&tag, State::Rawtext);
                Next::Done
            }
            // An `<option>` closes the previous one.
            "option" | "optgroup" => {
                if self
                    .open
                    .current()
                    .is_some_and(|id| self.doc.is_html_element(id, "option"))
                {
                    self.open.pop();
                }
                self.reconstruct_formatting();
                self.insert_element(&tag);
                Next::Done
            }
            "table" => {
                // In quirks mode a table does *not* close an open
                // paragraph — one of the few places the mode changes the
                // tree rather than only the layout.
                if self.doc.quirks() != QuirksMode::Quirks {
                    self.close_p_if_open();
                }
                self.insert_element(&tag);
                self.mode = Mode::InTable;
                Next::Done
            }
            // Table structure outside a table is dropped: without the
            // table there is nothing for these to be structure *of*.
            "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" => {
                Next::Done
            }
            // Framesets are not supported; the tag is dropped rather than
            // producing a tree no later stage knows how to lay out.
            "frameset" => Next::Done,

            name => {
                self.reconstruct_formatting();
                // A trailing slash means nothing in HTML and
                // everything in SVG — see `in_foreign_content`.
                if VOID_ELEMENTS.contains(&name) || (tag.self_closing && self.in_foreign_content())
                {
                    self.insert_void(&tag);
                } else {
                    self.insert_element(&tag);
                }
                Next::Done
            }
        }
    }

    fn in_body_end(&mut self, tag: Tag) -> Next {
        match tag.name.as_str() {
            "body" => {
                if self.open.has_in_scope(&self.doc, "body", Scope::Default) {
                    self.mode = Mode::AfterBody;
                }
                Next::Done
            }
            "html" => {
                if self.open.has_in_scope(&self.doc, "body", Scope::Default) {
                    self.mode = Mode::AfterBody;
                    return Next::Reprocess(Token::EndTag(tag));
                }
                Next::Done
            }
            "p" => {
                // `</p>` with no open `<p>` still produces one, empty.
                if !self.open.has_in_scope(&self.doc, "p", Scope::Button) {
                    self.insert_implied("p");
                }
                self.close_p();
                Next::Done
            }
            "li" => {
                if self.open.has_in_scope(&self.doc, "li", Scope::ListItem) {
                    self.generate_implied_end_tags_except("li");
                    self.open.pop_until_html(&self.doc, "li");
                }
                Next::Done
            }
            name @ ("dd" | "dt") => {
                if self.open.has_in_scope(&self.doc, name, Scope::Default) {
                    self.generate_implied_end_tags_except(name);
                    self.open.pop_until_html(&self.doc, name);
                }
                Next::Done
            }
            name if HEADINGS.contains(&name) => {
                // Any open heading closes, not only one of the same level.
                if self
                    .open
                    .has_any_in_scope(&self.doc, HEADINGS, Scope::Default)
                {
                    self.generate_implied_end_tags();
                    self.open.pop_until_any_html(&self.doc, HEADINGS);
                }
                Next::Done
            }
            "form" => {
                if let Some(form) = self.form.take() {
                    self.generate_implied_end_tags();
                    self.open.remove(form);
                }
                Next::Done
            }
            name if BLOCK_ELEMENTS.contains(&name) => {
                if self.open.has_in_scope(&self.doc, name, Scope::Default) {
                    self.generate_implied_end_tags();
                    self.open.pop_until_html(&self.doc, name);
                }
                Next::Done
            }
            name if FORMATTING_ELEMENTS.contains(&name) => {
                if !self.adoption_agency(name) {
                    self.close_any_other(name);
                }
                Next::Done
            }
            name @ ("applet" | "marquee" | "object") => {
                if self.open.has_in_scope(&self.doc, name, Scope::Default) {
                    self.generate_implied_end_tags();
                    self.open.pop_until_html(&self.doc, name);
                    self.formatting.clear_to_last_marker();
                }
                Next::Done
            }
            // `</br>` is treated as `<br>`: authors write it, and a line
            // break is what they meant.
            "br" => {
                self.reconstruct_formatting();
                self.insert_void(&Tag {
                    name: "br".to_owned(),
                    ..Tag::default()
                });
                Next::Done
            }
            name => {
                self.close_any_other(name);
                Next::Done
            }
        }
    }

    // ---- shared steps ----------------------------------------------------

    /// Whether this element acts as a barrier to an inward search.
    fn is_special(&self, id: NodeId) -> bool {
        self.doc.element(id).is_some_and(|element| {
            element.name.ns == crate::engine::cryo_impl::dom::Namespace::Html
                && SPECIAL_ELEMENTS.contains(&element.name.local.as_str())
        })
    }

    /// The spec's "generate implied end tags": close the elements whose
    /// end tag almost anything implies. The table modes need it too,
    /// before closing a cell or a caption.
    pub(super) fn generate_implied_end_tags(&mut self) {
        self.open.pop_while_html(&self.doc, IMPLIED_END_TAGS);
    }

    fn generate_implied_end_tags_except(&mut self, except: &str) {
        let locals: Vec<&str> = IMPLIED_END_TAGS
            .iter()
            .copied()
            .filter(|&name| name != except)
            .collect();
        self.open.pop_while_html(&self.doc, &locals);
    }

    /// Close an open `<p>`, if one is open in button scope. The rule
    /// behind `<p>one<p>two` producing two siblings rather than nesting.
    fn close_p_if_open(&mut self) {
        if self.open.has_in_scope(&self.doc, "p", Scope::Button) {
            self.close_p();
        }
    }

    fn close_p(&mut self) {
        self.generate_implied_end_tags_except("p");
        self.open.pop_until_html(&self.doc, "p");
    }

    /// Insert a formatting element and remember it, so it can be
    /// recreated if a block closes it implicitly.
    fn insert_formatting(&mut self, tag: &Tag) {
        self.reconstruct_formatting();
        let id = self.insert_element(tag);
        self.formatting.push_element(&self.doc, id);
    }

    /// The "any other end tag" rule: search inward for a matching open
    /// element, and give up at the first special element.
    ///
    /// This is what makes `</span>` close a span through a `<b>`, while
    /// `</span>` inside a `<div>` whose span is outside does nothing at
    /// all — the `<div>` is special, so the search stops there.
    fn close_any_other(&mut self, local: &str) {
        let inward: Vec<NodeId> = self.open.iter_inward().collect();
        for id in inward {
            if self.doc.is_html_element(id, local) {
                self.generate_implied_end_tags_except(local);
                while let Some(popped) = self.open.pop() {
                    if popped == id {
                        break;
                    }
                }
                return;
            }
            if self.is_special(id) {
                return;
            }
        }
    }

    /// Copy an element's name and attributes into a new, unattached node.
    fn clone_element(&mut self, source: NodeId) -> Option<NodeId> {
        let data = self.doc.element(source).cloned()?;
        Some(
            self.doc
                .create(crate::engine::cryo_impl::dom::NodeData::Element(data)),
        )
    }

    /// Recreate any formatting elements that a block implicitly closed,
    /// so their effect continues into the new block.
    ///
    /// This is what makes `<b>bold<p>still bold` work: the `<p>` closed
    /// the `<b>` on the stack, but the `<b>` is still in the active list,
    /// so a fresh one opens inside the paragraph.
    fn reconstruct_formatting(&mut self) {
        let entries = self.formatting.entries().to_vec();
        let Some(last) = entries.len().checked_sub(1) else {
            return;
        };
        if !self.needs_recreating(&entries, last) {
            return;
        }

        // Rewind to the first entry that is still open (or a marker).
        let mut index = last;
        while index > 0 {
            index -= 1;
            if !self.needs_recreating(&entries, index) {
                index += 1;
                break;
            }
        }

        for (offset, entry) in entries[index..].iter().enumerate() {
            let position = index + offset;
            let Formatting::Element(old) = *entry else {
                continue;
            };
            let Some(fresh) = self.clone_element(old) else {
                continue;
            };
            let point = self.insertion_point();
            self.insert_node(point, fresh);
            self.open.push(fresh);
            self.formatting.replace_at(position, fresh);
        }
    }

    /// Whether the entry at `index` names an element that is no longer
    /// open and so has to be recreated. Markers never do.
    fn needs_recreating(&self, entries: &[Formatting], index: usize) -> bool {
        match entries.get(index) {
            Some(Formatting::Element(id)) => self.open.position(*id).is_none(),
            _ => false,
        }
    }

    /// The adoption agency algorithm: reparent misnested formatting and
    /// block elements into a tree that can actually exist.
    ///
    /// Returns `false` when the subject is not an active formatting
    /// element at all, which means the caller should fall back to the
    /// "any other end tag" rule. Every other outcome is handled here.
    fn adoption_agency(&mut self, subject: &str) -> bool {
        // The easy case: the element is current and not being tracked, so
        // it just closes.
        if let Some(current) = self.open.current() {
            if self.doc.is_html_element(current, subject)
                && self.formatting.position(current).is_none()
            {
                self.open.pop();
                return true;
            }
        }

        for _ in 0..ADOPTION_AGENCY_ROUNDS {
            let Some(formatting_element) = self.formatting.last_between_markers(&self.doc, subject)
            else {
                return false;
            };
            let Some(element_position) = self.open.position(formatting_element) else {
                // Tracked but already closed: forget it.
                self.formatting.remove(formatting_element);
                return true;
            };
            if !self.open.has_in_scope(&self.doc, subject, Scope::Default) {
                return true;
            }

            // The innermost special element inside the formatting one.
            // Without one there is no misnesting to repair.
            let furthest_block = (element_position + 1..self.open.len())
                .filter_map(|index| self.open.at(index))
                .find(|&id| self.is_special(id));
            let Some(furthest_block) = furthest_block else {
                while let Some(popped) = self.open.pop() {
                    if popped == formatting_element {
                        break;
                    }
                }
                self.formatting.remove(formatting_element);
                return true;
            };

            let common_ancestor = self.open.at(element_position.saturating_sub(1));
            let bookmark = self.formatting.position(formatting_element).unwrap_or(0);
            let last_node = self.reparent_inner(formatting_element, furthest_block, bookmark);
            self.finish_adoption(Adoption {
                formatting_element,
                furthest_block,
                common_ancestor,
                last_node: last_node.node,
                bookmark: last_node.bookmark,
            });
        }
        true
    }

    /// The agency's inner loop: walk outward from the furthest block to
    /// the formatting element, cloning every tracked element on the way
    /// and nesting the previous one inside it.
    fn reparent_inner(
        &mut self,
        formatting_element: NodeId,
        furthest_block: NodeId,
        mut bookmark: usize,
    ) -> InnerResult {
        let mut node = furthest_block;
        let mut last_node = furthest_block;
        let mut rounds = 0usize;

        loop {
            rounds += 1;
            let Some(position) = self.open.position(node) else {
                break;
            };
            let Some(above) = position.checked_sub(1).and_then(|next| self.open.at(next)) else {
                break;
            };
            node = above;
            if node == formatting_element {
                break;
            }

            // After three rounds, stop tracking elements we pass: the
            // spec's guard against pathological nesting growing the list.
            if rounds > 3 && self.formatting.position(node).is_some() {
                self.formatting.remove(node);
            }
            let Some(list_position) = self.formatting.position(node) else {
                self.open.remove(node);
                continue;
            };

            let Some(clone) = self.clone_element(node) else {
                break;
            };
            self.formatting.replace_at(list_position, clone);
            self.open.replace(node, clone);
            node = clone;

            if last_node == furthest_block {
                bookmark = list_position + 1;
            }
            self.doc.append(node, last_node);
            last_node = node;
        }

        InnerResult {
            node: last_node,
            bookmark,
        }
    }

    /// The agency's closing steps: move the furthest block's children
    /// into a fresh copy of the formatting element, and put that copy
    /// inside the block.
    fn finish_adoption(&mut self, step: Adoption) {
        if let Some(ancestor) = step.common_ancestor {
            self.doc.append(ancestor, step.last_node);
        }

        let Some(fresh) = self.clone_element(step.formatting_element) else {
            return;
        };
        let children: Vec<NodeId> = self.doc.children(step.furthest_block).collect();
        for child in children {
            self.doc.append(fresh, child);
        }
        self.doc.append(step.furthest_block, fresh);

        let removed_at = self.formatting.position(step.formatting_element);
        self.formatting.remove(step.formatting_element);
        let bookmark = match removed_at {
            Some(index) if index < step.bookmark => step.bookmark - 1,
            _ => step.bookmark,
        };
        self.formatting.insert_at(bookmark, fresh);

        self.open.remove(step.formatting_element);
        self.open.insert_above(step.furthest_block, fresh);
    }
}

/// What [`TreeBuilder::reparent_inner`] leaves for the closing steps.
struct InnerResult {
    node: NodeId,
    bookmark: usize,
}

/// The adoption agency's state at the point its closing steps run.
/// Grouped into a struct because passing six positional ids would make
/// the call site unreadable and the order easy to get wrong.
struct Adoption {
    formatting_element: NodeId,
    furthest_block: NodeId,
    common_ancestor: Option<NodeId>,
    last_node: NodeId,
    bookmark: usize,
}

#[cfg(test)]
mod tests {
    use crate::engine::cryo_impl::dom::{Document, NodeData, NodeId, QuirksMode};
    use crate::engine::cryo_impl::html::tree::parse;

    /// Render the tree as indented element names and quoted text, which
    /// makes a wrong tree obvious in a failure message.
    fn sketch(doc: &Document) -> String {
        let mut out = String::new();
        write_children(doc, doc.root(), 0, &mut out);
        out
    }

    fn write_children(doc: &Document, parent: NodeId, depth: usize, out: &mut String) {
        for id in doc.children(parent) {
            for _ in 0..depth {
                out.push_str("  ");
            }
            match &doc.node(id).data {
                NodeData::Element(element) => out.push_str(element.name.local.as_str()),
                NodeData::Text(text) => out.push_str(&format!("{text:?}")),
                NodeData::Comment(text) => out.push_str(&format!("<!--{text}-->")),
                NodeData::Doctype(d) => out.push_str(&format!("<!DOCTYPE {}>", d.name)),
                NodeData::Document => out.push_str("#document"),
            }
            out.push('\n');
            write_children(doc, id, depth + 1, out);
        }
    }

    #[test]
    fn a_minimal_document_gains_html_head_and_body() {
        let doc = parse("<p>hi</p>");
        assert_eq!(sketch(&doc), "html\n  head\n  body\n    p\n      \"hi\"\n");
    }

    #[test]
    fn a_full_document_keeps_its_structure() {
        let doc =
            parse("<!DOCTYPE html><html><head><title>T</title></head><body><p>x</p></body></html>");
        assert_eq!(doc.quirks(), QuirksMode::NoQuirks);
        assert_eq!(
            sketch(&doc),
            "<!DOCTYPE html>\nhtml\n  head\n    title\n      \"T\"\n  body\n    p\n      \"x\"\n"
        );
    }

    #[test]
    fn a_missing_doctype_means_quirks_mode() {
        assert_eq!(parse("<p>x</p>").quirks(), QuirksMode::Quirks);
        assert_eq!(parse("<!DOCTYPE html><p>x").quirks(), QuirksMode::NoQuirks);
    }

    #[test]
    fn head_content_moves_into_the_head() {
        let doc = parse("<title>T</title><p>body text");
        assert_eq!(
            sketch(&doc),
            "html\n  head\n    title\n      \"T\"\n  body\n    p\n      \"body text\"\n"
        );
    }

    #[test]
    fn a_paragraph_cannot_contain_another_paragraph() {
        let doc = parse("<p>one<p>two");
        assert_eq!(
            sketch(&doc),
            "html\n  head\n  body\n    p\n      \"one\"\n    p\n      \"two\"\n"
        );
    }

    #[test]
    fn a_list_item_closes_the_previous_one() {
        let doc = parse("<ul><li>a<li>b</ul>");
        assert_eq!(
            sketch(&doc),
            "html\n  head\n  body\n    ul\n      li\n        \"a\"\n      li\n        \"b\"\n"
        );
    }

    #[test]
    fn a_heading_closes_an_open_heading_of_any_level() {
        let doc = parse("<h1>a<h2>b");
        assert_eq!(
            sketch(&doc),
            "html\n  head\n  body\n    h1\n      \"a\"\n    h2\n      \"b\"\n"
        );
    }

    #[test]
    fn an_end_tag_for_a_heading_closes_a_different_level() {
        // `<h1>x</h2>` closes the h1: browsers agree, markup be damned.
        let doc = parse("<h1>x</h2>y");
        assert_eq!(
            sketch(&doc),
            "html\n  head\n  body\n    h1\n      \"x\"\n    \"y\"\n"
        );
    }

    #[test]
    fn a_paragraph_start_tag_does_not_close_a_formatting_element() {
        // `<p>` closes an open `<p>`, not an open `<b>` — so the
        // paragraph nests *inside* the bold, and no clone is needed.
        let doc = parse("<b>bold<p>still bold");
        assert_eq!(
            sketch(&doc),
            "html\n  head\n  body\n    b\n      \"bold\"\n      p\n        \"still bold\"\n"
        );
    }

    #[test]
    fn formatting_is_recreated_after_a_block_pops_it_off_the_stack() {
        // Here the `</p>` does pop the `<b>`, which stays on the active
        // formatting list — so a fresh `<b>` opens for the trailing text.
        let doc = parse("<p><b>bold</p>still bold");
        assert_eq!(
            sketch(&doc),
            concat!(
                "html\n",
                "  head\n",
                "  body\n",
                "    p\n",
                "      b\n",
                "        \"bold\"\n",
                "    b\n",
                "      \"still bold\"\n",
            )
        );
    }

    #[test]
    fn misnested_formatting_is_repaired_by_cloning() {
        // The textbook adoption agency case. "3" must end up both bold
        // and italic, "4" italic only.
        let doc = parse("<p>1<b>2<i>3</b>4</i>5</p>");
        assert_eq!(
            sketch(&doc),
            concat!(
                "html\n",
                "  head\n",
                "  body\n",
                "    p\n",
                "      \"1\"\n",
                "      b\n",
                "        \"2\"\n",
                "        i\n",
                "          \"3\"\n",
                "      i\n",
                "        \"4\"\n",
                "      \"5\"\n",
            )
        );
    }

    #[test]
    fn an_anchor_cannot_nest_inside_another_anchor() {
        let doc = parse("<a href=1>one<a href=2>two");
        assert_eq!(
            sketch(&doc),
            "html\n  head\n  body\n    a\n      \"one\"\n    a\n      \"two\"\n"
        );
    }

    #[test]
    fn an_end_tag_search_stops_at_a_special_element() {
        // The `</span>` cannot reach out of the div, so it does nothing.
        let doc = parse("<span><div></span>x</div>");
        assert_eq!(
            sketch(&doc),
            "html\n  head\n  body\n    span\n      div\n        \"x\"\n"
        );
    }

    #[test]
    fn an_unmatched_end_tag_is_ignored() {
        // None of the three closes anything, and the text either side of
        // them is one node because nothing came between.
        let doc = parse("<p>x</b></i></div>y");
        assert_eq!(sketch(&doc), "html\n  head\n  body\n    p\n      \"xy\"\n");
    }

    #[test]
    fn a_stray_p_end_tag_in_the_body_produces_an_empty_paragraph() {
        let doc = parse("<body></p>x");
        assert_eq!(sketch(&doc), "html\n  head\n  body\n    p\n    \"x\"\n");
    }

    #[test]
    fn a_stray_end_tag_before_the_body_is_dropped_entirely() {
        // `</p>` arrives in AfterHead, which ignores end tags outright —
        // so unlike the case above, no paragraph is invented.
        let doc = parse("</p>x");
        assert_eq!(sketch(&doc), "html\n  head\n  body\n    \"x\"\n");
    }

    #[test]
    fn void_elements_never_gain_children() {
        let doc = parse("<p>a<br>b<img src=x>c");
        assert_eq!(
            sketch(&doc),
            "html\n  head\n  body\n    p\n      \"a\"\n      br\n      \"b\"\n      img\n      \"c\"\n"
        );
    }

    #[test]
    fn script_contents_are_text_not_markup() {
        let doc = parse("<body><script>if (a<b) { x(); }</script><p>after");
        assert_eq!(
            sketch(&doc),
            concat!(
                "html\n",
                "  head\n",
                "  body\n",
                "    script\n",
                "      \"if (a<b) { x(); }\"\n",
                "    p\n",
                "      \"after\"\n",
            )
        );
    }

    #[test]
    fn style_contents_are_text_and_land_in_the_head() {
        let doc = parse("<style>p { color: red }</style><p>x");
        assert_eq!(
            sketch(&doc),
            concat!(
                "html\n",
                "  head\n",
                "    style\n",
                "      \"p { color: red }\"\n",
                "  body\n",
                "    p\n",
                "      \"x\"\n",
            )
        );
    }

    #[test]
    fn a_pre_element_swallows_one_leading_newline() {
        let doc = parse("<pre>\nkept\n</pre>");
        assert_eq!(
            doc.text_content(doc.root()),
            "kept\n",
            "the first newline is the one authors did not mean"
        );
    }

    #[test]
    fn a_textarea_keeps_its_markup_as_text() {
        let doc = parse("<textarea><b>not bold</b></textarea>");
        assert_eq!(doc.text_content(doc.root()), "<b>not bold</b>");
    }

    #[test]
    fn nested_forms_are_dropped() {
        let doc = parse("<form><p>a<form><p>b");
        assert_eq!(
            sketch(&doc),
            concat!(
                "html\n",
                "  head\n",
                "  body\n",
                "    form\n",
                "      p\n",
                "        \"a\"\n",
                "      p\n",
                "        \"b\"\n",
            )
        );
    }

    #[test]
    fn attributes_survive_onto_the_element() {
        let doc = parse("<p id=x class='a b'>hi");
        let p = doc.first_html_element("p").expect("the p was inserted");
        let element = doc.element(p).expect("p is an element");
        assert_eq!(element.id(), Some("x"));
        assert_eq!(element.classes().collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn a_second_body_tag_merges_its_attributes() {
        let doc = parse("<body class=one><p>x<body id=two>");
        let body = doc.first_html_element("body").expect("body exists");
        let element = doc.element(body).expect("body is an element");
        assert_eq!(element.attr("class"), Some("one"));
        assert_eq!(element.attr("id"), Some("two"));
        assert_eq!(
            doc.descendants(doc.root())
                .filter(|&id| doc.is_html_element(id, "body"))
                .count(),
            1
        );
    }

    #[test]
    fn comments_land_where_they_were_written() {
        let doc = parse("<!--before--><html><body><!--inside--><p>x");
        assert!(sketch(&doc).contains("<!--before-->"));
        assert!(sketch(&doc).contains("<!--inside-->"));
    }

    #[test]
    fn an_unclosed_script_at_eof_does_not_swallow_the_document() {
        let doc = parse("<p>a<script>var x = 1;");
        assert!(doc.first_html_element("p").is_some());
        assert!(doc.first_html_element("script").is_some());
    }

    #[test]
    fn deeply_misnested_formatting_terminates() {
        // The eight-round bound: this must finish, not hang.
        let doc = parse("<b><i><b><i><b><i><b><i><b><i>x</b></i></b></i></b>");
        assert!(doc.text_content(doc.root()).contains('x'));
    }

    #[test]
    fn whitespace_between_head_elements_is_kept_in_the_head() {
        let doc = parse("<!DOCTYPE html><html>\n<head>\n<title>T</title>\n</head>\n<body>x");
        assert!(sketch(&doc).contains("title"));
        assert!(doc.text_content(doc.root()).contains('x'));
    }

    #[test]
    fn self_closing_shapes_inside_an_svg_are_siblings() {
        // Every icon of more than one shape depends on this. Ignore
        // the slash and the second path becomes a child of the first,
        // where nothing that reads an svg's children will find it.
        let doc = parse(r#"<svg><path d="M0 0"/><path d="M1 1"/><circle r="2"/></svg>"#);
        let svg = doc
            .descendants(doc.root())
            .find(|&id| doc.is_html_element(id, "svg"))
            .expect("an svg");
        assert_eq!(doc.children(svg).count(), 3, "{}", sketch(&doc));
    }

    #[test]
    fn a_self_closing_tag_in_html_still_opens_an_element() {
        // The slash means nothing outside foreign content, so the
        // text belongs inside the div.
        let doc = parse("<div/>inside</div>");
        let div = doc
            .descendants(doc.root())
            .find(|&id| doc.is_html_element(id, "div"))
            .expect("a div");
        assert!(doc.text_content(div).contains("inside"), "{}", sketch(&doc));
    }

    #[test]
    fn a_group_inside_an_svg_still_nests_when_it_is_not_self_closing() {
        let doc = parse(r#"<svg><g><path d="M0 0"/></g></svg>"#);
        let group = doc
            .descendants(doc.root())
            .find(|&id| doc.is_html_element(id, "g"))
            .expect("a g");
        assert_eq!(doc.children(group).count(), 1, "{}", sketch(&doc));
    }
}
