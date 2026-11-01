//! The box tree: the DOM plus styles become the boxes layout measures.
//!
//! Not a copy of the DOM. Three things change on the way across, and
//! each is a rule layout then never has to think about:
//!
//! 1. **`display: none` elements vanish**, subtree and all. No box, no
//!    geometry, nothing to skip over later.
//! 2. **Text becomes boxes.** A text node is a box with a style, taken
//!    from its parent — text cannot be targeted by a selector, so it has
//!    no style of its own to compute.
//! 3. **Anonymous boxes appear.** A block container's children must be
//!    *either* all block-level or all inline-level — never mixed. Given
//!    `<div>text<p>para</p></div>`, the stray text gets wrapped in an
//!    anonymous block:
//!
//!    ```text
//!      div (block)
//!        anonymous block
//!          "text"
//!        p (block)
//!    ```
//!
//!    Without that, every later stage would need a rule for "a block
//!    container with a text child among its blocks". With it, a block
//!    container has one formatting context and one way to lay out.
//!
//! Known gap: an inline box containing a block-level child
//! (`<span>a<div>b</div>c</span>`) should *split* the inline around the
//! block. That is not implemented — the block stays nested inside the
//! inline, which will lay out wrongly. It is called out here rather than
//! silently producing an odd tree.

use crate::engine::cryo_impl::css::computed::{ComputedStyle, Display};
use crate::engine::cryo_impl::css::style_tree::StyleTree;
use crate::engine::cryo_impl::dom::{Document, NodeData, NodeId};

/// Elements whose content comes from outside CSS — a form control's
/// widget, an image's pixels, a plugin's surface.
///
/// They are "replaced" in the spec's sense, and the practical
/// consequence is that `width` and `height` apply to them even when
/// they are inline, because they have an intrinsic size of their own.
const REPLACED_ELEMENTS: &[&str] = &[
    "img", "input", "select", "textarea", "button", "video", "audio", "canvas", "iframe", "embed",
    "object", "progress", "meter",
];

fn is_replaced(doc: &Document, element: NodeId) -> bool {
    REPLACED_ELEMENTS
        .iter()
        .any(|name| doc.is_html_element(element, name))
}

/// The text a form field shows, if this element is one.
///
/// A field's contents are not in the markup: `<input value="x">` and
/// `<input placeholder="y">` both draw text the document never
/// contains, which is why it is made here. An empty field shows its
/// placeholder — greyed, in a browser; the same colour here, which is
/// a stated approximation until `::placeholder` is matched.
///
/// A password field shows dots rather than its value, because the
/// whole point of the type is that the value is not on screen.
fn field_text(doc: &Document, element: NodeId) -> Option<String> {
    if !doc.is_html_element(element, "input") {
        return None;
    }
    let data = doc.element(element)?;
    let kind = data.attr("type").unwrap_or("text").to_ascii_lowercase();
    // A checkbox, radio or button draws no text of its own.
    if matches!(
        kind.as_str(),
        "checkbox" | "radio" | "hidden" | "submit" | "reset" | "button" | "image" | "file"
    ) {
        return None;
    }
    match data.attr("value").filter(|value| !value.is_empty()) {
        Some(value) if kind == "password" => Some("•".repeat(value.chars().count())),
        Some(value) => Some(value.to_owned()),
        None => data
            .attr("placeholder")
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
    }
}

/// A box's identity within one [`BoxTree`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoxId(usize);

impl BoxId {
    /// Where this box sits in the order the tree was built.
    ///
    /// Ids are handed out as the markup is walked, so among one
    /// parent's children a smaller index means earlier in the
    /// document. Paint order needs that, because by then the
    /// fragments have been reordered — out-of-flow boxes are appended
    /// to their containing block's children rather than left where
    /// they were written.
    pub fn index(self) -> usize {
        self.0
    }
}

/// What kind of box this is, which decides how it is laid out and how it
/// participates in its parent.
#[derive(Clone, Debug, PartialEq)]
pub enum BoxKind {
    /// Block-level, and a block container: `display: block` and friends.
    Block,
    /// Inline-level and not a container: `display: inline`.
    Inline,
    /// Inline-level outside, block container inside: `inline-block`.
    InlineBlock,
    /// A run of text, inline-level by nature.
    Text(String),
    /// A forced line break: `<br>`.
    ///
    /// Its own kind rather than an inline box that happens to be empty,
    /// because line breaking has to recognise it — and asking "is this
    /// element a `br`?" during layout would mean layout reaching back
    /// into the DOM for a tag name.
    LineBreak,
    /// Generated to hold an inline run among block siblings.
    AnonymousBlock,
}

impl BoxKind {
    /// Whether this box sits in its parent's inline flow.
    pub const fn is_inline_level(&self) -> bool {
        matches!(
            self,
            Self::Inline | Self::InlineBlock | Self::Text(_) | Self::LineBreak
        )
    }

    /// Whether this box lays its own children out as a block container.
    pub const fn is_block_container(&self) -> bool {
        matches!(self, Self::Block | Self::InlineBlock | Self::AnonymousBlock)
    }

    /// Whether this box was generated rather than coming from an element.
    pub const fn is_anonymous(&self) -> bool {
        matches!(self, Self::AnonymousBlock)
    }

    /// The kind an element with this `display` generates, or `None` for
    /// `display: none`.
    ///
    /// Flex, grid and table displays map to [`Self::Block`] for now:
    /// their boxes really are block-level, and their *inner* layout is
    /// what is missing — so this is the right answer already, and the
    /// gap is in the layout algorithms rather than here.
    fn for_display(display: Display) -> Option<Self> {
        if !display.generates_box() {
            return None;
        }
        Some(match display {
            Display::Inline => Self::Inline,
            Display::InlineBlock
            | Display::InlineFlex
            | Display::InlineGrid
            | Display::InlineTable => Self::InlineBlock,
            _ => Self::Block,
        })
    }
}

/// One box: its kind, its style, and its children.
#[derive(Clone, Debug)]
pub struct LayoutBox {
    pub kind: BoxKind,
    pub style: ComputedStyle,
    /// The element or text node this came from; `None` when anonymous.
    pub node: Option<NodeId>,
    pub children: Vec<BoxId>,
}

impl LayoutBox {
    /// The text this box holds, if it is a text box.
    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            BoxKind::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// The boxes of one document.
#[derive(Clone, Debug, Default)]
pub struct BoxTree {
    boxes: Vec<LayoutBox>,
    /// `None` for a document with nothing to render — an empty document,
    /// or a root that is `display: none`.
    root: Option<BoxId>,
}

impl BoxTree {
    /// Build the box tree for a document whose styles are resolved.
    pub fn build(doc: &Document, styles: &StyleTree) -> Self {
        let mut tree = Self::default();
        let root = doc
            .children(doc.root())
            .find(|&id| doc.element(id).is_some())
            .and_then(|root| tree.build_element(doc, styles, root));
        tree.root = root;
        tree
    }

    pub const fn root(&self) -> Option<BoxId> {
        self.root
    }

    pub fn get(&self, id: BoxId) -> &LayoutBox {
        &self.boxes[id.0]
    }

    pub fn children(&self, id: BoxId) -> &[BoxId] {
        &self.boxes[id.0].children
    }

    /// The children that take part in `id`'s flow.
    ///
    /// An out-of-flow box is still in the tree — `absolute` finds it
    /// there and places it against its containing block — but every
    /// in-flow sizing pass has to step over it, or an overlay would
    /// push its siblings down the page it is supposed to sit on top
    /// of.
    ///
    /// Floats are *not* excluded. They are not implemented, and until
    /// they are, laying one out in flow puts its content on the page;
    /// skipping it here would make it vanish.
    pub fn in_flow_children(&self, id: BoxId) -> Vec<BoxId> {
        self.boxes[id.0]
            .children
            .iter()
            .copied()
            .filter(|&child| !self.get(child).style.position.is_out_of_flow())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    /// Every box under `id` in tree order, `id` included.
    pub fn descendants(&self, id: BoxId) -> Vec<BoxId> {
        let mut out = Vec::new();
        let mut pending = vec![id];
        while let Some(current) = pending.pop() {
            out.push(current);
            // Reversed, so the stack yields children left to right.
            pending.extend(self.children(current).iter().rev().copied());
        }
        out
    }

    fn push(&mut self, layout_box: LayoutBox) -> BoxId {
        self.boxes.push(layout_box);
        BoxId(self.boxes.len() - 1)
    }

    /// Build the box for one element, or `None` if it generates none.
    fn build_element(
        &mut self,
        doc: &Document,
        styles: &StyleTree,
        element: NodeId,
    ) -> Option<BoxId> {
        let style = styles.get(element)?;
        let mut kind = BoxKind::for_display(style.display)?;
        // A replaced element is *atomic*: it has a size of its own and
        // `width`/`height` apply to it, which they do not to an
        // ordinary inline box. Without this an `<input>` is an empty
        // inline box, so a form's fields occupy nothing and vanish.
        if kind == BoxKind::Inline && is_replaced(doc, element) {
            kind = BoxKind::InlineBlock;
        }
        // A `<br>` is a break wherever it appears, and has no contents.
        if doc.is_html_element(element, "br") {
            return Some(self.push(LayoutBox {
                kind: BoxKind::LineBreak,
                style: style.clone(),
                node: Some(element),
                children: Vec::new(),
            }));
        }
        // An `<input>` has no children in the markup, but it is not
        // empty on the page: it shows its value, or its placeholder
        // when it has none. The browser supplies that text, which is
        // why it is made here rather than coming from the document.
        if let Some(text) = field_text(doc, element) {
            let label = self.push(LayoutBox {
                kind: BoxKind::Text(text),
                style: ComputedStyle::text_from(style),
                node: Some(element),
                children: Vec::new(),
            });
            return Some(self.push(LayoutBox {
                kind,
                style: style.clone(),
                node: Some(element),
                children: vec![label],
            }));
        }

        let mut children = self.build_children(doc, styles, element, style);
        // A flex or grid container takes each *element* child as an
        // item whatever its display, so it skips the block
        // container's "make them all one level" step — but it still
        // has to give bare text a box. See `wrap_text_runs`.
        let own_context = style.display.is_flex_container() || style.display.is_grid_container();
        if own_context {
            children = self.wrap_text_runs(&children, style);
        } else if kind.is_block_container() {
            children = self.enforce_one_formatting_context(&children, style);
        }
        Some(self.push(LayoutBox {
            kind,
            style: style.clone(),
            node: Some(element),
            children,
        }))
    }

    /// Boxes for an element's child nodes, in order.
    fn build_children(
        &mut self,
        doc: &Document,
        styles: &StyleTree,
        element: NodeId,
        style: &ComputedStyle,
    ) -> Vec<BoxId> {
        let mut children = Vec::new();
        for child in doc.children(element) {
            match &doc.node(child).data {
                NodeData::Element(_) => {
                    children.extend(self.build_element(doc, styles, child));
                }
                NodeData::Text(text) => {
                    // Text has no element of its own: it inherits from
                    // its parent and takes initial values for the rest.
                    children.push(self.push(LayoutBox {
                        kind: BoxKind::Text(text.clone()),
                        style: ComputedStyle::text_from(style),
                        node: Some(child),
                        children: Vec::new(),
                    }));
                }
                // Comments and the doctype generate nothing.
                _ => {}
            }
        }
        children
    }

    /// Make a block container's children uniformly block-level or
    /// inline-level, wrapping inline runs in anonymous blocks.
    ///
    /// Returns the children unchanged when they are already all inline —
    /// the common case, and one where wrapping would add a pointless box
    /// around every paragraph's contents.
    fn enforce_one_formatting_context(
        &mut self,
        children: &[BoxId],
        style: &ComputedStyle,
    ) -> Vec<BoxId> {
        let has_block = children
            .iter()
            .any(|&child| !self.get(child).kind.is_inline_level());
        if !has_block {
            return children.to_vec();
        }

        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        for &child in children {
            if self.get(child).kind.is_inline_level() {
                run.push(child);
            } else {
                self.flush_inline_run(&mut run, &mut out, style);
                out.push(child);
            }
        }
        self.flush_inline_run(&mut run, &mut out, style);
        out
    }

    /// Wrap bare text in anonymous items, for a flex or grid container.
    ///
    /// A flex container takes each *element* child as an item whatever
    /// its display — there is no "make them all one level" step, and
    /// doing it anyway makes a form field shrink to nothing inside a
    /// wrapper nobody wrote.
    ///
    /// Text is the exception, and it is not optional: a bare run of
    /// text has no box of its own, so it needs an anonymous one to be
    /// an item at all. Without it `<button>Sign In<svg/></button>`
    /// lays the label out as a block with no inline formatting
    /// context — which gives it a width and no height, and the label
    /// vanishes.
    fn wrap_text_runs(&mut self, children: &[BoxId], style: &ComputedStyle) -> Vec<BoxId> {
        let has_text = children
            .iter()
            .any(|&child| matches!(self.get(child).kind, BoxKind::Text(_)));
        if !has_text {
            return children.to_vec();
        }

        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        for &child in children {
            if matches!(self.get(child).kind, BoxKind::Text(_) | BoxKind::LineBreak) {
                run.push(child);
            } else {
                self.flush_inline_run(&mut run, &mut out, style);
                out.push(child);
            }
        }
        self.flush_inline_run(&mut run, &mut out, style);
        out
    }

    /// Wrap a pending inline run in an anonymous block and append it.
    ///
    /// A run that is nothing but collapsible whitespace is dropped
    /// instead: the newline and indentation between two `<p>` tags must
    /// not become a visible empty line, which is exactly what an
    /// anonymous block around it would be.
    fn flush_inline_run(
        &mut self,
        run: &mut Vec<BoxId>,
        out: &mut Vec<BoxId>,
        style: &ComputedStyle,
    ) {
        if run.is_empty() {
            return;
        }
        let children = std::mem::take(run);
        if children
            .iter()
            .all(|&child| self.is_collapsible_whitespace(child))
        {
            return;
        }
        let anonymous = self.push(LayoutBox {
            kind: BoxKind::AnonymousBlock,
            style: ComputedStyle::anonymous_from(style),
            node: None,
            children,
        });
        out.push(anonymous);
    }

    /// Whether this box is text that would disappear when collapsed.
    fn is_collapsible_whitespace(&self, id: BoxId) -> bool {
        let layout_box = self.get(id);
        match layout_box.text() {
            Some(text) => {
                layout_box.style.white_space.collapses_spaces()
                    && text.chars().all(char::is_whitespace)
            }
            // An empty inline element still generates a box.
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::cascade::{Origin, RuleSet};
    use crate::engine::cryo_impl::css::computed::ComputeContext;
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::html::parse as parse_html;

    /// Stands in for the user-agent stylesheet until there is one: the
    /// structural elements are block-level, and the head generates no
    /// boxes at all — which is why a document's boxes start at `<body>`.
    const BLOCKS: &str = concat!(
        "html, body, div, p, section, h1, ul, li { display: block }",
        "head, title, style, script, meta, link { display: none }",
    );

    fn build(markup: &str, css: &str) -> BoxTree {
        let doc = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, BLOCKS);
        rules.insert_css(Origin::Author, css);
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        BoxTree::build(&doc, &styles)
    }

    /// The tree as indented lines, which makes a wrong shape obvious.
    fn sketch(markup: &str, css: &str) -> String {
        let tree = build(markup, css);
        let mut out = String::new();
        if let Some(root) = tree.root() {
            write_box(&tree, root, 0, &mut out);
        }
        out
    }

    fn write_box(tree: &BoxTree, id: BoxId, depth: usize, out: &mut String) {
        let layout_box = tree.get(id);
        for _ in 0..depth {
            out.push_str("  ");
        }
        let label = match &layout_box.kind {
            BoxKind::Text(text) => format!("{text:?}"),
            BoxKind::AnonymousBlock => "anon-block".to_owned(),
            BoxKind::Block => "block".to_owned(),
            BoxKind::Inline => "inline".to_owned(),
            BoxKind::InlineBlock => "inline-block".to_owned(),
            BoxKind::LineBreak => "br".to_owned(),
        };
        out.push_str(&label);
        out.push('\n');
        for &child in tree.children(id) {
            write_box(tree, child, depth + 1, out);
        }
    }

    /// The tree as `tag:kind` lines, which is what most assertions want.
    fn tagged(markup: &str, css: &str) -> String {
        let doc = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, BLOCKS);
        rules.insert_css(Origin::Author, css);
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = BoxTree::build(&doc, &styles);
        let mut out = String::new();
        if let Some(root) = tree.root() {
            write_tagged(&doc, &tree, root, 0, &mut out);
        }
        out
    }

    fn write_tagged(doc: &Document, tree: &BoxTree, id: BoxId, depth: usize, out: &mut String) {
        let layout_box = tree.get(id);
        for _ in 0..depth {
            out.push_str("  ");
        }
        let label = match &layout_box.kind {
            BoxKind::Text(text) => format!("{text:?}"),
            BoxKind::AnonymousBlock => "anon".to_owned(),
            kind => {
                let name = layout_box
                    .node
                    .and_then(|node| doc.element(node))
                    .map_or("?", |data| data.name.local.as_str());
                let suffix = match kind {
                    BoxKind::Block => "block",
                    BoxKind::Inline => "inline",
                    BoxKind::InlineBlock => "inline-block",
                    BoxKind::LineBreak => "br",
                    _ => "?",
                };
                format!("{name}:{suffix}")
            }
        };
        out.push_str(&label);
        out.push('\n');
        for &child in tree.children(id) {
            write_tagged(doc, tree, child, depth + 1, out);
        }
    }

    #[test]
    fn a_simple_document_builds_a_box_per_element() {
        assert_eq!(
            tagged("<p>hi</p>", ""),
            concat!(
                "html:block\n",
                "  body:block\n",
                "    p:block\n",
                "      \"hi\"\n",
            )
        );
    }

    #[test]
    fn display_none_removes_the_element_and_its_subtree() {
        let tree = tagged(
            "<p>kept</p><div>gone<span>also gone</span></div>",
            "div { display: none }",
        );
        assert!(tree.contains("kept"));
        assert!(!tree.contains("gone"), "got:\n{tree}");
        assert!(!tree.contains("div"));
    }

    #[test]
    fn an_inline_element_makes_an_inline_box() {
        assert_eq!(
            tagged("<p>a<span>b</span></p>", ""),
            concat!(
                "html:block\n",
                "  body:block\n",
                "    p:block\n",
                "      \"a\"\n",
                "      span:inline\n",
                "        \"b\"\n",
            )
        );
    }

    #[test]
    fn an_all_inline_block_container_gets_no_anonymous_box() {
        // The common case: wrapping here would add a box per paragraph
        // for nothing.
        let tree = tagged("<p>a<span>b</span>c</p>", "");
        assert!(!tree.contains("anon"), "got:\n{tree}");
    }

    #[test]
    fn mixed_children_get_anonymous_blocks_around_the_inline_runs() {
        assert_eq!(
            tagged("<div>text<p>para</p>more</div>", ""),
            concat!(
                "html:block\n",
                "  body:block\n",
                "    div:block\n",
                "      anon\n",
                "        \"text\"\n",
                "      p:block\n",
                "        \"para\"\n",
                "      anon\n",
                "        \"more\"\n",
            )
        );
    }

    #[test]
    fn an_inline_run_of_several_boxes_shares_one_anonymous_block() {
        let tree = tagged("<div>a<span>b</span>c<p>block</p></div>", "");
        assert_eq!(tree.matches("anon").count(), 1, "got:\n{tree}");
    }

    #[test]
    fn whitespace_between_blocks_does_not_become_an_anonymous_box() {
        // The newline and indentation an author writes between tags must
        // not render as an empty line.
        let tree = tagged("<div>\n  <p>a</p>\n  <p>b</p>\n</div>", "");
        assert!(!tree.contains("anon"), "got:\n{tree}");
        assert_eq!(tree.matches("p:block").count(), 2);
    }

    #[test]
    fn whitespace_is_kept_when_it_separates_inline_content() {
        // Here it is a word separator, not filler between blocks.
        let tree = tagged("<p><span>a</span> <span>b</span></p>", "");
        assert!(tree.contains("\" \""), "got:\n{tree}");
    }

    #[test]
    fn preformatted_whitespace_is_never_dropped() {
        let tree = tagged("<div style='white-space: pre'>\n<p>a</p></div>", "");
        assert!(tree.contains("anon"), "pre whitespace needs a box:\n{tree}");
    }

    #[test]
    fn an_inline_block_is_inline_level_but_a_container() {
        let tree = build("<p>a<span>b</span></p>", "span { display: inline-block }");
        let box_ids = tree.descendants(tree.root().expect("a root"));
        let inline_block = box_ids
            .iter()
            .map(|&id| tree.get(id))
            .find(|layout_box| layout_box.kind == BoxKind::InlineBlock)
            .expect("the inline-block exists");
        assert!(inline_block.kind.is_inline_level());
        assert!(inline_block.kind.is_block_container());
    }

    #[test]
    fn comments_and_doctypes_generate_nothing() {
        let tree = tagged("<!DOCTYPE html><p><!--note-->a</p>", "");
        assert!(!tree.contains("note"), "got:\n{tree}");
        assert_eq!(tree.matches('"').count(), 2, "only the text node");
    }

    #[test]
    fn an_anonymous_box_inherits_but_takes_initial_values_elsewhere() {
        let tree = build(
            "<div>text<p>para</p></div>",
            "div { color: red; padding: 5px }",
        );
        let anonymous = tree
            .descendants(tree.root().expect("a root"))
            .into_iter()
            .map(|id| tree.get(id))
            .find(|layout_box| layout_box.kind.is_anonymous())
            .expect("an anonymous box was generated");
        assert_eq!(
            anonymous.style.color,
            crate::engine::cryo_impl::css::value::Color::rgb(255, 0, 0),
            "colour inherits"
        );
        assert_eq!(
            anonymous.style.padding,
            crate::engine::cryo_impl::css::computed::Sides::all(
                crate::engine::cryo_impl::css::value::LengthPercentage::ZERO
            ),
            "padding does not"
        );
        assert_eq!(anonymous.style.display, Display::Block);
        assert!(anonymous.node.is_none(), "an anonymous box has no node");
    }

    #[test]
    fn text_takes_its_parents_style() {
        let tree = build("<p>text</p>", "p { color: red }");
        let text = tree
            .descendants(tree.root().expect("a root"))
            .into_iter()
            .map(|id| tree.get(id))
            .find(|layout_box| layout_box.text() == Some("text"))
            .expect("the text box exists");
        assert_eq!(
            text.style.color,
            crate::engine::cryo_impl::css::value::Color::rgb(255, 0, 0)
        );
    }

    #[test]
    fn an_empty_document_builds_no_boxes() {
        let doc = Document::new();
        let styles = StyleTree::default();
        let tree = BoxTree::build(&doc, &styles);
        assert!(tree.is_empty());
        assert_eq!(tree.root(), None);
    }

    #[test]
    fn a_root_set_to_display_none_builds_nothing() {
        let tree = build("<p>x</p>", "html { display: none }");
        assert_eq!(tree.root(), None);
    }

    #[test]
    fn descendants_are_returned_in_tree_order() {
        let tree = build("<div><p>a</p><p>b</p></div>", "");
        let root = tree.root().expect("a root");
        let ids = tree.descendants(root);
        assert_eq!(ids.first(), Some(&root));
        // Every id appears once, and parents precede their children.
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    #[test]
    fn a_nested_list_keeps_its_structure() {
        assert_eq!(
            tagged("<ul><li>a</li><li>b</li></ul>", ""),
            concat!(
                "html:block\n",
                "  body:block\n",
                "    ul:block\n",
                "      li:block\n",
                "        \"a\"\n",
                "      li:block\n",
                "        \"b\"\n",
            )
        );
    }

    /// How many anonymous boxes the tree holds.
    fn anonymous_count(markup: &str, css: &str) -> usize {
        let tree = build(markup, css);
        let Some(root) = tree.root() else {
            return 0;
        };
        tree.descendants(root)
            .into_iter()
            .filter(|&id| tree.get(id).kind.is_anonymous())
            .count()
    }

    /// Mixed children: an inline beside a block.
    const MIXED: &str = "<div id=row><span>a</span><p>b</p></div>";

    #[test]
    fn a_flex_container_wraps_no_element_in_an_anonymous_block() {
        // The real case: a form field beside an icon. Wrapping the
        // field in an anonymous block makes *that* the flex item, and
        // the field inside it shrinks to nothing.
        assert_eq!(anonymous_count(MIXED, "#row { display: flex }"), 0);
    }

    #[test]
    fn a_grid_container_wraps_no_element_either() {
        assert_eq!(anonymous_count(MIXED, "#row { display: grid }"), 0);
    }

    #[test]
    fn a_flex_container_does_wrap_bare_text() {
        // `<button>Sign In<svg/></button>`: the label has no box of
        // its own, so it needs an anonymous one to be an item at all.
        // Without it the text gets a width and no height and the
        // label vanishes.
        assert_eq!(
            anonymous_count(
                "<div id=row>Sign In<p>x</p></div>",
                "#row { display: flex }"
            ),
            1
        );
    }

    #[test]
    fn a_flex_container_with_only_elements_wraps_nothing() {
        assert_eq!(
            anonymous_count(
                "<div id=row><p>a</p><p>b</p></div>",
                "#row { display: flex }"
            ),
            0
        );
    }

    #[test]
    fn a_block_container_still_wraps_its_mixed_children() {
        // The counterpart: a block container *does* need the wrapper,
        // or it would have two formatting contexts at once.
        assert_eq!(anonymous_count(MIXED, ""), 1);
    }

    #[test]
    fn the_sketch_helper_renders_without_tags() {
        // Exercises the kind-only renderer, so it cannot rot unnoticed.
        let drawn = sketch("<p>x</p>", "");
        assert!(drawn.starts_with("block\n"));
        assert!(drawn.contains("\"x\""));
    }
    #[test]
    fn a_text_field_shows_its_placeholder_when_it_is_empty() {
        let tree = build(r#"<input placeholder="you@example.com">"#, "");
        let root = tree.root().expect("a root");
        let text: Vec<&str> = tree
            .descendants(root)
            .into_iter()
            .filter_map(|id| tree.get(id).text())
            .collect();
        assert_eq!(text, ["you@example.com"]);
    }

    #[test]
    fn a_value_wins_over_the_placeholder() {
        let tree = build(r#"<input value="typed" placeholder="hint">"#, "");
        let root = tree.root().expect("a root");
        let text: Vec<&str> = tree
            .descendants(root)
            .into_iter()
            .filter_map(|id| tree.get(id).text())
            .collect();
        assert_eq!(text, ["typed"]);
    }

    #[test]
    fn a_password_field_shows_dots_rather_than_its_value() {
        let tree = build(r#"<input type=password value="secret">"#, "");
        let root = tree.root().expect("a root");
        let text: Vec<&str> = tree
            .descendants(root)
            .into_iter()
            .filter_map(|id| tree.get(id).text())
            .collect();
        assert_eq!(text, ["••••••"]);
    }

    #[test]
    fn a_checkbox_draws_no_text_of_its_own() {
        let tree = build(r#"<input type=checkbox value="on">"#, "");
        let root = tree.root().expect("a root");
        assert!(tree
            .descendants(root)
            .into_iter()
            .all(|id| tree.get(id).text().is_none()));
    }
}
