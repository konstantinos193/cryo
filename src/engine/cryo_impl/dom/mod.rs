//! The document tree: an arena of nodes plus the only code allowed to
//! change their shape.
//!
//! An arena rather than `Rc<RefCell<Node>>`: a DOM is a graph with parent
//! pointers, so reference counting it either leaks cycles or needs weak
//! pointers everywhere, and every traversal pays a borrow check it cannot
//! fail usefully. Indices make the tree a plain `Vec`, which also means a
//! `NodeId` is `Copy` and layout can hold ids across stages without
//! borrowing the document.
//!
//! The six links a node has (parent, first/last child, prev/next sibling)
//! are consistent only as a set. Every mutation here maintains all of
//! them, and [`node::Node`] keeps the fields crate-private, so there is no
//! way for outside code to leave the tree half-relinked.

pub mod node;

pub use node::{
    Attribute, DoctypeData, ElementData, LocalName, Namespace, Node, NodeData, NodeId, QualName,
};

/// A parsed document: the node arena and its root.
///
/// The root is always the node at index 0 and is always
/// [`NodeData::Document`] — created with the arena, never removed, so
/// "the tree has a root" is a structural fact rather than an invariant
/// someone has to check.
#[derive(Clone, Debug)]
pub struct Document {
    nodes: Vec<Node>,
    /// The document's address, for resolving relative URLs in markup.
    /// Empty for a document parsed from bytes with no known origin.
    base: String,
    /// Set when the parser saw no doctype, or a legacy one. Quirks mode
    /// changes real layout rules (notably how `height: 100%` and table
    /// font sizing behave), so it is recorded at parse time and read by
    /// the cascade — never re-derived later from the doctype node.
    quirks: QuirksMode,
}

/// How strictly to lay the document out, decided by the doctype.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QuirksMode {
    /// A modern `<!DOCTYPE html>`, or close enough.
    #[default]
    NoQuirks,
    /// A known-transitional doctype: almost standards.
    LimitedQuirks,
    /// No doctype at all, or one of the legacy strings.
    Quirks,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// An empty document: one [`NodeData::Document`] root, no children.
    pub fn new() -> Self {
        Self {
            nodes: vec![Node::new(NodeData::Document)],
            base: String::new(),
            quirks: QuirksMode::NoQuirks,
        }
    }

    /// The root node's id. Always valid.
    pub const fn root(&self) -> NodeId {
        NodeId(0)
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn set_base(&mut self, base: impl Into<String>) {
        self.base = base.into();
    }

    pub const fn quirks(&self) -> QuirksMode {
        self.quirks
    }

    pub fn set_quirks(&mut self, mode: QuirksMode) {
        self.quirks = mode;
    }

    /// How many nodes the arena holds, root included. Detached nodes still
    /// count — nothing is ever freed, since a live `NodeId` to a freed
    /// slot is exactly the bug an arena is supposed to make impossible.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Always false: a document always has its root.
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        self.node(id).data.as_element()
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        self.node_mut(id).data.as_element_mut()
    }

    /// True when this node is an HTML element with the given local name.
    /// The tree builder asks this constantly ("is the current node a
    /// `<p>`?"), and spelling it out at each site invites namespace bugs.
    pub fn is_html_element(&self, id: NodeId, local: &str) -> bool {
        self.element(id).is_some_and(|e| e.name.is_html(local))
    }

    /// Create a free-floating node. It belongs to no parent until
    /// [`Self::append`] or [`Self::insert_before`] places it.
    pub fn create(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node::new(data));
        NodeId(self.nodes.len() - 1)
    }

    /// Create an HTML element node, unattached.
    pub fn create_html_element(&mut self, local: &str) -> NodeId {
        self.create(NodeData::Element(ElementData::new(QualName::html(local))))
    }

    /// Make `child` the last child of `parent`, detaching it first if it
    /// was somewhere else. Re-appending a node already in the tree is a
    /// move, which is what the HTML parser's adoption agency needs.
    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        let previous_last = self.node(parent).last_child;
        match previous_last {
            Some(last) => {
                self.node_mut(last).next_sibling = Some(child);
                self.node_mut(child).prev_sibling = Some(last);
            }
            None => self.node_mut(parent).first_child = Some(child),
        }
        self.node_mut(parent).last_child = Some(child);
        self.node_mut(child).parent = Some(parent);
    }

    /// Insert `child` immediately before `reference`, in `reference`'s
    /// parent. Does nothing if `reference` is detached — the caller has no
    /// insertion position to mean, and silently appending to the root
    /// would scatter nodes into the document.
    pub fn insert_before(&mut self, reference: NodeId, child: NodeId) {
        let Some(parent) = self.node(reference).parent else {
            return;
        };
        self.detach(child);
        let before = self.node(reference).prev_sibling;
        match before {
            Some(prev) => {
                self.node_mut(prev).next_sibling = Some(child);
                self.node_mut(child).prev_sibling = Some(prev);
            }
            None => self.node_mut(parent).first_child = Some(child),
        }
        self.node_mut(reference).prev_sibling = Some(child);
        self.node_mut(child).next_sibling = Some(reference);
        self.node_mut(child).parent = Some(parent);
    }

    /// Unlink `child` from its parent and siblings. The node and its own
    /// subtree survive, detached; its id stays valid.
    pub fn detach(&mut self, child: NodeId) {
        let Some(parent) = self.node_mut(child).parent.take() else {
            return;
        };
        let prev = self.node_mut(child).prev_sibling.take();
        let next = self.node_mut(child).next_sibling.take();

        match prev {
            Some(prev) => self.node_mut(prev).next_sibling = next,
            None => self.node_mut(parent).first_child = next,
        }
        match next {
            Some(next) => self.node_mut(next).prev_sibling = prev,
            None => self.node_mut(parent).last_child = prev,
        }
    }

    /// Append character data at the end of `parent`, merging into the
    /// trailing text node when there is one.
    ///
    /// Coalescing is not an optimisation: the tokenizer emits one
    /// character token at a time, so without it every text run becomes
    /// hundreds of single-character nodes and both selector matching
    /// (`:first-child`) and inline layout read a tree no other browser
    /// has.
    pub fn append_text(&mut self, parent: NodeId, text: &str) {
        if let Some(last) = self.node(parent).last_child {
            if let NodeData::Text(existing) = &mut self.node_mut(last).data {
                existing.push_str(text);
                return;
            }
        }
        let node = self.create(NodeData::Text(text.to_owned()));
        self.append(parent, node);
    }

    /// Append character data immediately before `reference`, merging into
    /// the text node already there when there is one. The foster-parenting
    /// counterpart of [`Self::append_text`].
    pub fn insert_text_before(&mut self, reference: NodeId, text: &str) {
        if let Some(prev) = self.node(reference).prev_sibling {
            if let NodeData::Text(existing) = &mut self.node_mut(prev).data {
                existing.push_str(text);
                return;
            }
        }
        let node = self.create(NodeData::Text(text.to_owned()));
        self.insert_before(reference, node);
    }

    /// This node's children in order.
    pub fn children(&self, parent: NodeId) -> Children<'_> {
        Children {
            doc: self,
            next: self.node(parent).first_child,
        }
    }

    /// Every node under `parent` in document order, `parent` excluded.
    pub fn descendants(&self, parent: NodeId) -> Descendants<'_> {
        Descendants {
            doc: self,
            root: parent,
            next: self.node(parent).first_child,
        }
    }

    /// This node's ancestors, nearest first, up to and including the root.
    pub fn ancestors(&self, from: NodeId) -> Ancestors<'_> {
        Ancestors {
            doc: self,
            next: self.node(from).parent,
        }
    }

    /// The first HTML element with this local name, in document order.
    /// Used for the handful of singular elements — `<head>`, `<body>`,
    /// `<title>`, `<base>` — that the parser and loader look up by name.
    pub fn first_html_element(&self, local: &str) -> Option<NodeId> {
        self.descendants(self.root())
            .find(|&id| self.is_html_element(id, local))
    }

    /// All character data under `parent`, concatenated in document order.
    /// This is `textContent`: what `<title>` means and what a text-only
    /// comparison in a test wants.
    pub fn text_content(&self, parent: NodeId) -> String {
        let mut out = String::new();
        for id in self.descendants(parent) {
            if let Some(text) = self.node(id).data.as_text() {
                out.push_str(text);
            }
        }
        out
    }

    /// The node after `id` in document order, without leaving the subtree
    /// rooted at `root`. The one piece of traversal arithmetic in the
    /// module; both iterators and layout's tree walks go through it rather
    /// than each re-deriving the climb-and-step rule.
    fn following_within(&self, id: NodeId, root: NodeId) -> Option<NodeId> {
        if let Some(child) = self.node(id).first_child {
            return Some(child);
        }
        let mut at = id;
        loop {
            if at == root {
                return None;
            }
            if let Some(sibling) = self.node(at).next_sibling {
                return Some(sibling);
            }
            at = self.node(at).parent?;
        }
    }
}

/// Iterator over one node's children. See [`Document::children`].
pub struct Children<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        self.next = self.doc.node(current).next_sibling;
        Some(current)
    }
}

/// Pre-order iterator over a subtree. See [`Document::descendants`].
pub struct Descendants<'a> {
    doc: &'a Document,
    root: NodeId,
    next: Option<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        self.next = self.doc.following_within(current, self.root);
        Some(current)
    }
}

/// Iterator from a node up to the root. See [`Document::ancestors`].
pub struct Ancestors<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Ancestors<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        self.next = self.doc.node(current).parent;
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<html><body><p>hi</p><span>there</span></body></html>`
    fn sample() -> (Document, NodeId, NodeId, NodeId) {
        let mut doc = Document::new();
        let root = doc.root();
        let html = doc.create_html_element("html");
        doc.append(root, html);
        let body = doc.create_html_element("body");
        doc.append(html, body);
        let p = doc.create_html_element("p");
        doc.append(body, p);
        doc.append_text(p, "hi");
        let span = doc.create_html_element("span");
        doc.append(body, span);
        doc.append_text(span, "there");
        (doc, body, p, span)
    }

    #[test]
    fn a_new_document_is_just_its_root() {
        let doc = Document::new();
        assert_eq!(doc.len(), 1);
        assert_eq!(doc.node(doc.root()).data, NodeData::Document);
        assert_eq!(doc.children(doc.root()).count(), 0);
    }

    #[test]
    fn append_links_siblings_both_ways() {
        let (doc, body, p, span) = sample();
        assert_eq!(doc.children(body).collect::<Vec<_>>(), [p, span]);
        assert_eq!(doc.node(p).next_sibling(), Some(span));
        assert_eq!(doc.node(span).prev_sibling(), Some(p));
        assert_eq!(doc.node(span).parent(), Some(body));
    }

    #[test]
    fn insert_before_the_first_child_updates_the_parents_head() {
        let (mut doc, body, p, span) = sample();
        let div = doc.create_html_element("div");
        doc.insert_before(p, div);
        assert_eq!(doc.children(body).collect::<Vec<_>>(), [div, p, span]);
        assert_eq!(doc.node(div).prev_sibling(), None);
    }

    #[test]
    fn insert_before_a_detached_reference_does_nothing() {
        let (mut doc, _, _, _) = sample();
        let orphan = doc.create_html_element("i");
        let div = doc.create_html_element("div");
        doc.insert_before(orphan, div);
        assert_eq!(doc.node(div).parent(), None);
        assert_eq!(doc.children(doc.root()).count(), 1);
    }

    #[test]
    fn detaching_the_last_child_updates_the_parents_tail() {
        let (mut doc, body, p, span) = sample();
        doc.detach(span);
        assert_eq!(doc.children(body).collect::<Vec<_>>(), [p]);
        assert_eq!(doc.node(p).next_sibling(), None);
        assert_eq!(doc.node(span).parent(), None);
    }

    #[test]
    fn detaching_a_middle_child_closes_the_sibling_chain() {
        let (mut doc, body, p, span) = sample();
        let div = doc.create_html_element("div");
        doc.insert_before(span, div);
        doc.detach(div);
        assert_eq!(doc.children(body).collect::<Vec<_>>(), [p, span]);
        assert_eq!(doc.node(p).next_sibling(), Some(span));
        assert_eq!(doc.node(span).prev_sibling(), Some(p));
    }

    #[test]
    fn appending_an_attached_node_moves_it() {
        let (mut doc, body, p, span) = sample();
        doc.append(p, span);
        // It leaves `body` entirely and lands after `p`'s existing text node.
        assert_eq!(doc.children(body).collect::<Vec<_>>(), [p]);
        assert_eq!(doc.node(span).parent(), Some(p));
        assert_eq!(doc.node(span).next_sibling(), None);
        let text = doc.node(p).first_child().expect("p holds its text node");
        assert_eq!(doc.node(span).prev_sibling(), Some(text));
        assert_eq!(doc.text_content(p), "hithere");
    }

    #[test]
    fn text_appended_twice_becomes_one_node() {
        let mut doc = Document::new();
        let p = doc.create_html_element("p");
        doc.append(doc.root(), p);
        doc.append_text(p, "ab");
        doc.append_text(p, "cd");
        assert_eq!(doc.children(p).count(), 1);
        assert_eq!(doc.text_content(p), "abcd");
    }

    #[test]
    fn text_does_not_merge_across_an_element() {
        let mut doc = Document::new();
        let p = doc.create_html_element("p");
        doc.append(doc.root(), p);
        doc.append_text(p, "ab");
        let br = doc.create_html_element("br");
        doc.append(p, br);
        doc.append_text(p, "cd");
        assert_eq!(doc.children(p).count(), 3);
        assert_eq!(doc.text_content(p), "abcd");
    }

    #[test]
    fn inserted_text_merges_into_the_preceding_text_node() {
        let mut doc = Document::new();
        let p = doc.create_html_element("p");
        doc.append(doc.root(), p);
        doc.append_text(p, "ab");
        let br = doc.create_html_element("br");
        doc.append(p, br);
        doc.insert_text_before(br, "cd");
        assert_eq!(doc.children(p).count(), 2);
        assert_eq!(doc.text_content(p), "abcd");
    }

    #[test]
    fn descendants_are_document_order_and_exclude_the_start_node() {
        let (doc, body, p, span) = sample();
        let names: Vec<String> = doc
            .descendants(body)
            .map(|id| match &doc.node(id).data {
                NodeData::Element(e) => e.name.local.as_str().to_owned(),
                NodeData::Text(t) => format!("#{t}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(names, ["p", "#hi", "span", "#there"]);
        assert!(!doc.descendants(body).any(|id| id == body));
        assert_eq!(doc.descendants(p).count(), 1);
        assert_eq!(doc.descendants(span).count(), 1);
    }

    #[test]
    fn descendants_never_escape_the_subtree() {
        let (doc, _, p, _) = sample();
        assert_eq!(doc.descendants(p).count(), 1);
    }

    #[test]
    fn ancestors_run_nearest_first_to_the_root() {
        let (doc, body, p, _) = sample();
        let html = doc.node(body).parent().expect("body was appended to html");
        assert_eq!(
            doc.ancestors(p).collect::<Vec<_>>(),
            [body, html, doc.root()]
        );
    }

    #[test]
    fn first_html_element_finds_by_name_in_document_order() {
        let (doc, body, _, _) = sample();
        assert_eq!(doc.first_html_element("body"), Some(body));
        assert_eq!(doc.first_html_element("table"), None);
    }

    #[test]
    fn text_content_of_the_root_is_the_whole_document() {
        let (doc, _, _, _) = sample();
        assert_eq!(doc.text_content(doc.root()), "hithere");
    }
}
