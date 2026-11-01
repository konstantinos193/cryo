//! The two stacks tree construction runs on: the open elements, and the
//! list of active formatting elements.
//!
//! **Open elements** is the nesting an insertion mode reads to decide
//! what a tag means. The subtlety is not the stack, it is *scope*: an end
//! tag like `</p>` closes a `<p>` only if one is open "in button scope",
//! and a `</td>` searches only "in table scope". Each scope is the same
//! walk down the stack with a different set of elements that stop it —
//! which is why they are one function here parameterised by that set, and
//! not five hand-written loops that drift apart.
//!
//! **Active formatting elements** is what makes `<b>bold<p>still bold`
//! work. When a `<p>` implicitly closes inside a `<b>`, the `<b>` is
//! still logically open, so the parser remembers it and recreates it in
//! the new block. The list also holds markers, which stop that
//! reconstruction leaking out of a cell or a caption.

use crate::engine::cryo_impl::dom::{Document, Namespace, NodeId};

/// Elements that stop a default-scope search.
///
/// Spec-exact and order-insensitive. `html` is here because the search
/// must terminate, and `template`/`table`/`td`/`th` because content
/// inside them is isolated from the block structure outside.
const SCOPE_TERMINATORS: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "template",
];

/// Foreign elements that also terminate a default-scope search: MathML
/// text containers and the SVG elements that embed HTML.
const FOREIGN_SCOPE_TERMINATORS: &[(Namespace, &str)] = &[
    (Namespace::MathMl, "mi"),
    (Namespace::MathMl, "mo"),
    (Namespace::MathMl, "mn"),
    (Namespace::MathMl, "ms"),
    (Namespace::MathMl, "mtext"),
    (Namespace::MathMl, "annotation-xml"),
    (Namespace::Svg, "foreignObject"),
    (Namespace::Svg, "desc"),
    (Namespace::Svg, "title"),
];

/// Which scope an "is this element open?" question is asked in.
///
/// Each variant is a different stopping set, and picking the wrong one is
/// a real bug with quiet symptoms: `</p>` searched in plain scope instead
/// of button scope would close a `<p>` through a `<button>`, silently
/// reshaping the tree rather than failing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    /// The default: block containers, cells and foreign text stop it.
    Default,
    /// Default plus `button` — used by `<p>` handling.
    Button,
    /// Default plus `ol` and `ul` — used by `<li>` handling.
    ListItem,
    /// Only `html`, `table` and `template` stop it. Table end tags search
    /// here, so a `</tr>` is not blocked by a `<div>` open inside a cell.
    Table,
}

impl Scope {
    /// Whether `(ns, local)` stops a search in this scope.
    fn terminates(self, ns: Namespace, local: &str) -> bool {
        // Table scope has its own, far smaller set rather than being the
        // default set plus extras.
        if self == Self::Table {
            return ns == Namespace::Html && matches!(local, "html" | "table" | "template");
        }
        if ns != Namespace::Html {
            return FOREIGN_SCOPE_TERMINATORS
                .iter()
                .any(|&(foreign, name)| foreign == ns && name == local);
        }
        let extra = match self {
            Self::Button => matches!(local, "button"),
            Self::ListItem => matches!(local, "ol" | "ul"),
            Self::Default | Self::Table => false,
        };
        extra || SCOPE_TERMINATORS.contains(&local)
    }
}

/// The stack of open elements, innermost last.
#[derive(Debug, Default)]
pub(super) struct OpenElements {
    stack: Vec<NodeId>,
}

impl OpenElements {
    /// The innermost open element — the spec's "current node". `None`
    /// only before `<html>` is inserted.
    pub(super) fn current(&self) -> Option<NodeId> {
        self.stack.last().copied()
    }

    pub(super) fn push(&mut self, id: NodeId) {
        self.stack.push(id);
    }

    pub(super) fn pop(&mut self) -> Option<NodeId> {
        self.stack.pop()
    }

    pub(super) fn len(&self) -> usize {
        self.stack.len()
    }

    /// The element at `index` from the bottom.
    pub(super) fn at(&self, index: usize) -> Option<NodeId> {
        self.stack.get(index).copied()
    }

    /// Ids from innermost to outermost.
    pub(super) fn iter_inward(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.stack.iter().rev().copied()
    }

    /// Where `id` sits in the stack, counting from the bottom.
    pub(super) fn position(&self, id: NodeId) -> Option<usize> {
        self.stack.iter().position(|&n| n == id)
    }

    /// Remove `id` wherever it is. The adoption agency needs this: it
    /// takes elements out of the middle, not just off the top.
    pub(super) fn remove(&mut self, id: NodeId) {
        self.stack.retain(|&n| n != id);
    }

    /// Insert `id` immediately *inside* `above` — one position nearer the
    /// current node. The adoption agency's final step puts its fresh
    /// formatting element here, inside the block it just repaired.
    pub(super) fn insert_above(&mut self, above: NodeId, id: NodeId) {
        match self.position(above) {
            Some(index) => self.stack.insert(index + 1, id),
            None => self.stack.push(id),
        }
    }

    /// Swap `old` for `new` in place, keeping the nesting order. Used
    /// when an element is cloned and the clone takes its position.
    pub(super) fn replace(&mut self, old: NodeId, new: NodeId) {
        if let Some(index) = self.position(old) {
            self.stack[index] = new;
        }
    }

    /// Whether any HTML element with this local name is open at all,
    /// ignoring scope.
    pub(super) fn contains_html(&self, doc: &Document, local: &str) -> bool {
        self.stack.iter().any(|&id| doc.is_html_element(id, local))
    }

    /// Whether an HTML element with this local name is open *in* `scope`.
    pub(super) fn has_in_scope(&self, doc: &Document, local: &str, scope: Scope) -> bool {
        self.scan_scope(doc, scope, |d, id| d.is_html_element(id, local))
    }

    /// Whether any of `locals` is open in `scope` — the form the table
    /// and heading rules ask in (`h1`..`h6`, `td` or `th`).
    pub(super) fn has_any_in_scope(&self, doc: &Document, locals: &[&str], scope: Scope) -> bool {
        self.scan_scope(doc, scope, |d, id| {
            locals.iter().any(|&l| d.is_html_element(id, l))
        })
    }

    /// The one scope walk: inward from the current node, answering true
    /// at a match and false at the first element that terminates `scope`.
    fn scan_scope(
        &self,
        doc: &Document,
        scope: Scope,
        matches: impl Fn(&Document, NodeId) -> bool,
    ) -> bool {
        for id in self.iter_inward() {
            if matches(doc, id) {
                return true;
            }
            let Some(element) = doc.element(id) else {
                continue;
            };
            if scope.terminates(element.name.ns, element.name.local.as_str()) {
                return false;
            }
        }
        false
    }

    /// Pop elements until one with this local name has been popped.
    /// Leaves the stack untouched if there is none.
    pub(super) fn pop_until_html(&mut self, doc: &Document, local: &str) {
        if !self.contains_html(doc, local) {
            return;
        }
        while let Some(id) = self.pop() {
            if doc.is_html_element(id, local) {
                return;
            }
        }
    }

    /// Pop elements until one of `locals` has been popped.
    pub(super) fn pop_until_any_html(&mut self, doc: &Document, locals: &[&str]) {
        if !locals.iter().any(|&l| self.contains_html(doc, l)) {
            return;
        }
        while let Some(id) = self.pop() {
            if locals.iter().any(|&l| doc.is_html_element(id, l)) {
                return;
            }
        }
    }

    /// Pop until the current node *is* one of `locals`, or the stack
    /// empties. The inverse of [`Self::pop_while_html`], and the shape
    /// the table modes need: "clear the stack back to a table context"
    /// discards whatever stray elements are open inside the table.
    pub(super) fn pop_until_current_is(&mut self, doc: &Document, locals: &[&str]) {
        while let Some(id) = self.current() {
            if locals.iter().any(|&local| doc.is_html_element(id, local)) {
                return;
            }
            self.pop();
        }
    }

    /// Pop while the current node is one of `locals`, without popping
    /// anything else. The spec's "generate implied end tags".
    pub(super) fn pop_while_html(&mut self, doc: &Document, locals: &[&str]) {
        while let Some(id) = self.current() {
            if locals.iter().any(|&l| doc.is_html_element(id, l)) {
                self.pop();
            } else {
                return;
            }
        }
    }
}

/// An entry in the list of active formatting elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Formatting {
    /// A barrier. Reconstruction stops here, so formatting from outside a
    /// cell, caption or `<object>` does not leak into it.
    Marker,
    Element(NodeId),
}

/// The list of active formatting elements, oldest first.
#[derive(Debug, Default)]
pub(super) struct ActiveFormatting {
    entries: Vec<Formatting>,
}

impl ActiveFormatting {
    pub(super) fn push_marker(&mut self) {
        self.entries.push(Formatting::Marker);
    }

    /// Drop everything back to and including the last marker.
    pub(super) fn clear_to_last_marker(&mut self) {
        while let Some(entry) = self.entries.pop() {
            if entry == Formatting::Marker {
                return;
            }
        }
    }

    pub(super) fn entries(&self) -> &[Formatting] {
        &self.entries
    }

    pub(super) fn position(&self, id: NodeId) -> Option<usize> {
        self.entries
            .iter()
            .position(|&e| e == Formatting::Element(id))
    }

    pub(super) fn remove(&mut self, id: NodeId) {
        self.entries.retain(|&e| e != Formatting::Element(id));
    }

    /// Replace the entry at `index`, used when the adoption agency clones
    /// an element and the list must point at the clone.
    pub(super) fn replace_at(&mut self, index: usize, id: NodeId) {
        if let Some(slot) = self.entries.get_mut(index) {
            *slot = Formatting::Element(id);
        }
    }

    pub(super) fn insert_at(&mut self, index: usize, id: NodeId) {
        let index = index.min(self.entries.len());
        self.entries.insert(index, Formatting::Element(id));
    }

    /// The innermost entry with this local name, stopping at the last
    /// marker. This is what `</b>` looks for.
    pub(super) fn last_between_markers(&self, doc: &Document, local: &str) -> Option<NodeId> {
        for entry in self.entries.iter().rev() {
            match *entry {
                Formatting::Marker => return None,
                Formatting::Element(id) if doc.is_html_element(id, local) => return Some(id),
                Formatting::Element(_) => {}
            }
        }
        None
    }

    /// Add a formatting element, applying the Noah's Ark clause.
    ///
    /// Three identical formatting elements — same name and same
    /// attributes — may be active at once; adding a fourth drops the
    /// oldest. Without it, `<b>` repeated in a loop makes reconstruction
    /// quadratic and nests pathologically deep on pages that do exactly
    /// that by accident.
    pub(super) fn push_element(&mut self, doc: &Document, id: NodeId) {
        let mut matching = Vec::new();
        for entry in self.entries.iter().rev() {
            match *entry {
                Formatting::Marker => break,
                Formatting::Element(other) if same_element(doc, other, id) => matching.push(other),
                Formatting::Element(_) => {}
            }
        }
        if matching.len() >= 3 {
            if let Some(&oldest) = matching.last() {
                self.remove(oldest);
            }
        }
        self.entries.push(Formatting::Element(id));
    }
}

/// Whether two elements count as the same for the Noah's Ark clause:
/// same name, and the same attributes regardless of source order.
fn same_element(doc: &Document, a: NodeId, b: NodeId) -> bool {
    let (Some(left), Some(right)) = (doc.element(a), doc.element(b)) else {
        return false;
    };
    if left.name != right.name || left.attrs().len() != right.attrs().len() {
        return false;
    }
    left.attrs().iter().all(|attr| {
        right
            .attrs()
            .iter()
            .any(|other| other.name == attr.name && other.value == attr.value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::dom::LocalName;

    /// Build a chain of nested elements and push them all, returning the
    /// document and the ids in order.
    fn nest(names: &[&str]) -> (Document, OpenElements, Vec<NodeId>) {
        let mut doc = Document::new();
        let mut stack = OpenElements::default();
        let mut ids = Vec::new();
        let mut parent = doc.root();
        for name in names {
            let id = doc.create_html_element(name);
            doc.append(parent, id);
            stack.push(id);
            ids.push(id);
            parent = id;
        }
        (doc, stack, ids)
    }

    #[test]
    fn the_current_node_is_the_innermost_element() {
        let (_, stack, ids) = nest(&["html", "body", "div"]);
        assert_eq!(stack.current(), ids.last().copied());
        assert_eq!(stack.len(), 3);
    }

    #[test]
    fn an_open_element_is_in_scope_through_ordinary_containers() {
        let (doc, stack, _) = nest(&["html", "body", "div", "span"]);
        assert!(stack.has_in_scope(&doc, "div", Scope::Default));
        assert!(stack.has_in_scope(&doc, "body", Scope::Default));
    }

    #[test]
    fn a_table_cell_blocks_a_default_scope_search() {
        // A `<div>` outside the cell is not in scope from inside it.
        let (doc, stack, _) = nest(&["html", "body", "div", "table", "td", "span"]);
        assert!(!stack.has_in_scope(&doc, "div", Scope::Default));
        assert!(
            stack.contains_html(&doc, "div"),
            "still open, just not in scope"
        );
    }

    #[test]
    fn button_scope_differs_from_default_scope_for_p() {
        // This is the rule behind `<p><button>x</button>` not closing the p
        // from inside the button.
        let (doc, stack, _) = nest(&["html", "body", "p", "button", "span"]);
        assert!(stack.has_in_scope(&doc, "p", Scope::Default));
        assert!(!stack.has_in_scope(&doc, "p", Scope::Button));
    }

    #[test]
    fn list_item_scope_stops_at_a_list_container() {
        let (doc, stack, _) = nest(&["html", "body", "li", "ul", "li"]);
        let (doc2, stack2, _) = nest(&["html", "body", "li", "div"]);
        // The outer li is hidden behind the ul...
        assert_eq!(
            stack
                .iter_inward()
                .filter(|&id| doc.is_html_element(id, "li"))
                .count(),
            2
        );
        // ...but a plain div does not hide it.
        assert!(stack2.has_in_scope(&doc2, "li", Scope::ListItem));
    }

    #[test]
    fn has_any_in_scope_answers_for_a_set_of_names() {
        let (doc, stack, _) = nest(&["html", "body", "h3", "span"]);
        let headings = ["h1", "h2", "h3", "h4", "h5", "h6"];
        assert!(stack.has_any_in_scope(&doc, &headings, Scope::Default));
        assert!(!stack.has_any_in_scope(&doc, &["td", "th"], Scope::Default));
    }

    #[test]
    fn popping_until_a_name_removes_that_element_too() {
        let (doc, mut stack, ids) = nest(&["html", "body", "div", "span", "b"]);
        stack.pop_until_html(&doc, "div");
        assert_eq!(stack.len(), 2);
        assert_eq!(stack.current(), Some(ids[1]));
    }

    #[test]
    fn popping_until_an_absent_name_changes_nothing() {
        let (doc, mut stack, _) = nest(&["html", "body", "div"]);
        stack.pop_until_html(&doc, "table");
        assert_eq!(stack.len(), 3);
    }

    #[test]
    fn popping_while_stops_at_the_first_element_not_listed() {
        let (doc, mut stack, ids) = nest(&["html", "body", "div", "li", "p"]);
        stack.pop_while_html(&doc, &["p", "li"]);
        assert_eq!(stack.current(), Some(ids[2]), "stopped at the div");
    }

    #[test]
    fn remove_takes_an_element_out_of_the_middle() {
        let (_, mut stack, ids) = nest(&["html", "body", "b", "div"]);
        stack.remove(ids[2]);
        assert_eq!(stack.len(), 3);
        assert_eq!(stack.current(), ids.last().copied());
        assert_eq!(stack.position(ids[2]), None);
    }

    #[test]
    fn insert_above_places_an_element_inside_its_neighbour() {
        // The adoption agency's last step: the fresh formatting element
        // lands one position nearer the current node than the block.
        let (_, mut stack, ids) = nest(&["html", "body", "div"]);
        let moved = ids[0];
        stack.remove(moved);
        stack.insert_above(ids[1], moved);
        assert_eq!(stack.position(ids[1]), Some(0));
        assert_eq!(stack.position(moved), Some(1));
        assert_eq!(stack.position(ids[2]), Some(2));
    }

    #[test]
    fn a_marker_hides_formatting_from_reconstruction() {
        let (doc, _, ids) = nest(&["html", "b", "i"]);
        let mut list = ActiveFormatting::default();
        list.push_element(&doc, ids[1]);
        list.push_marker();
        list.push_element(&doc, ids[2]);

        assert_eq!(list.last_between_markers(&doc, "i"), Some(ids[2]));
        assert_eq!(
            list.last_between_markers(&doc, "b"),
            None,
            "the marker hides the outer b"
        );
    }

    #[test]
    fn clearing_to_the_last_marker_drops_only_the_recent_entries() {
        let (doc, _, ids) = nest(&["html", "b", "i", "u"]);
        let mut list = ActiveFormatting::default();
        list.push_element(&doc, ids[1]);
        list.push_marker();
        list.push_element(&doc, ids[2]);
        list.push_element(&doc, ids[3]);

        list.clear_to_last_marker();
        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.last_between_markers(&doc, "b"), Some(ids[1]));
    }

    #[test]
    fn noahs_ark_keeps_only_three_identical_formatting_elements() {
        let mut doc = Document::new();
        let mut list = ActiveFormatting::default();
        let mut ids = Vec::new();
        for _ in 0..4 {
            let id = doc.create_html_element("b");
            doc.append(doc.root(), id);
            ids.push(id);
            list.push_element(&doc, id);
        }
        assert_eq!(list.entries().len(), 3);
        assert_eq!(list.position(ids[0]), None, "the oldest was dropped");
        assert_eq!(list.position(ids[3]), Some(2));
    }

    #[test]
    fn differing_attributes_make_formatting_elements_distinct() {
        let mut doc = Document::new();
        let mut list = ActiveFormatting::default();
        for i in 0..4 {
            let id = doc.create_html_element("b");
            doc.append(doc.root(), id);
            if let Some(element) = doc.element_mut(id) {
                element.set(LocalName::html("class"), format!("c{i}"));
            }
            list.push_element(&doc, id);
        }
        assert_eq!(list.entries().len(), 4, "none of them match each other");
    }

    #[test]
    fn attribute_order_does_not_make_elements_distinct() {
        let mut doc = Document::new();
        let first = doc.create_html_element("b");
        let second = doc.create_html_element("b");
        doc.append(doc.root(), first);
        doc.append(doc.root(), second);
        if let Some(element) = doc.element_mut(first) {
            element.set(LocalName::html("a"), "1".into());
            element.set(LocalName::html("b"), "2".into());
        }
        if let Some(element) = doc.element_mut(second) {
            element.set(LocalName::html("b"), "2".into());
            element.set(LocalName::html("a"), "1".into());
        }
        assert!(same_element(&doc, first, second));
    }

    #[test]
    fn removing_a_formatting_entry_leaves_the_rest_in_order() {
        let (doc, _, ids) = nest(&["html", "b", "i", "u"]);
        let mut list = ActiveFormatting::default();
        for &id in &ids[1..] {
            list.push_element(&doc, id);
        }
        list.remove(ids[2]);
        assert_eq!(list.position(ids[1]), Some(0));
        assert_eq!(list.position(ids[3]), Some(1));
        assert_eq!(list.entries().len(), 2);
    }
}
