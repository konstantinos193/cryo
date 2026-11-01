//! What a document node *is*: its handle, its name, its attributes and its
//! payload. Tree shape — who is whose parent — belongs to
//! [`super::Document`]; a node here never knows its neighbours, so no node
//! can be moved into a tree position that contradicts the arena.

/// A node's identity inside one [`super::Document`].
///
/// Opaque on purpose: it is an arena index, and an index from one document
/// means nothing in another. Keeping the field crate-private is what stops
/// a caller inventing `NodeId(7)` and reading a node that was never theirs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub(super) usize);

impl NodeId {
    pub(super) const fn index(self) -> usize {
        self.0
    }
}

/// The three element namespaces an HTML parser can produce. Foreign
/// content is not a curiosity — `<svg>` appears in real pages, and its
/// elements match selectors case-sensitively where HTML's do not, so the
/// namespace has to travel with the name rather than being inferred later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Namespace {
    #[default]
    Html,
    Svg,
    MathMl,
}

/// An element or attribute name, already case-folded for its namespace.
///
/// HTML tag and attribute names are ASCII-case-insensitive, and the
/// tokenizer lowercases them as it reads. Folding at construction means
/// every later comparison is a plain `==`: no call site has to remember to
/// compare case-insensitively, which is the kind of thing exactly one call
/// site always forgets.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalName(Box<str>);

impl LocalName {
    /// Fold an HTML name: ASCII-lowercased, which is what the HTML
    /// tokenizer does to every tag and attribute name it reads.
    pub fn html(name: &str) -> Self {
        Self(name.to_ascii_lowercase().into_boxed_str())
    }

    /// Keep a name exactly as written — for SVG and MathML, where names
    /// like `viewBox` and `clipPathUnits` are case-sensitive.
    pub fn verbatim(name: &str) -> Self {
        Self(name.to_owned().into_boxed_str())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for LocalName {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

/// A namespaced element name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QualName {
    pub ns: Namespace,
    pub local: LocalName,
}

impl QualName {
    pub fn html(local: &str) -> Self {
        Self {
            ns: Namespace::Html,
            local: LocalName::html(local),
        }
    }

    /// True for an HTML-namespaced element with this local name — the test
    /// nearly every parser and layout decision actually wants, since
    /// `<svg><title>` is not HTML's `<title>`.
    pub fn is_html(&self, local: &str) -> bool {
        self.ns == Namespace::Html && self.local == *local
    }
}

/// One attribute as written. Name/value only: an attribute has no identity
/// beyond its name within its element, so this is a value object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub name: LocalName,
    pub value: String,
}

/// An element's own data. `attrs` keeps source order because serialising
/// back out has to reproduce it, and because the first of a duplicated
/// name is the one that counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementData {
    pub name: QualName,
    attrs: Vec<Attribute>,
}

impl ElementData {
    pub fn new(name: QualName) -> Self {
        Self {
            name,
            attrs: Vec::new(),
        }
    }

    /// Add an attribute unless the element already has one by that name.
    ///
    /// First-wins is the HTML spec's rule for duplicates
    /// (`<p id=a id=b>` has id `a`), and it is the reason this is the only
    /// way to add an attribute during parsing — a plain `push` would let
    /// the last one win and silently disagree with every other browser.
    pub fn set_if_absent(&mut self, name: LocalName, value: String) {
        if !self.attrs.iter().any(|a| a.name == name) {
            self.attrs.push(Attribute { name, value });
        }
    }

    /// Set an attribute, replacing any existing value. For script and
    /// internal mutation, where last write wins — unlike parsing.
    pub fn set(&mut self, name: LocalName, value: String) {
        match self.attrs.iter_mut().find(|a| a.name == name) {
            Some(existing) => existing.value = value,
            None => self.attrs.push(Attribute { name, value }),
        }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.name == *name)
            .map(|a| a.value.as_str())
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|a| a.name == *name)
    }

    pub fn attrs(&self) -> &[Attribute] {
        &self.attrs
    }

    /// The `id` attribute, if any — used by `#foo` selector matching.
    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }

    /// The `class` attribute split on ASCII whitespace. Returns an
    /// iterator rather than a `Vec` so selector matching, which runs per
    /// element per rule, allocates nothing.
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class")
            .unwrap_or_default()
            .split_ascii_whitespace()
    }

    pub fn has_class(&self, name: &str) -> bool {
        self.classes().any(|c| c == name)
    }
}

/// A doctype's three parts. Kept because quirks-mode detection reads them,
/// not because anything renders them.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DoctypeData {
    pub name: String,
    pub public_id: String,
    pub system_id: String,
}

/// What a node carries. An enum rather than a node struct with optional
/// fields: a text node has no attributes and an element has no character
/// data, and making those states unrepresentable is cheaper than
/// asserting them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeData {
    /// The root. Exactly one per document, created with it.
    Document,
    Doctype(DoctypeData),
    Element(ElementData),
    Text(String),
    Comment(String),
}

impl NodeData {
    pub fn as_element(&self) -> Option<&ElementData> {
        match self {
            Self::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn as_element_mut(&mut self) -> Option<&mut ElementData> {
        match self {
            Self::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// A node plus its place in the tree. The links are crate-private: they
/// are only ever consistent as a set, so [`super::Document`] maintains
/// them together and no outside code can half-update them.
#[derive(Clone, Debug)]
pub struct Node {
    pub data: NodeData,
    pub(super) parent: Option<NodeId>,
    pub(super) first_child: Option<NodeId>,
    pub(super) last_child: Option<NodeId>,
    pub(super) prev_sibling: Option<NodeId>,
    pub(super) next_sibling: Option<NodeId>,
}

impl Node {
    pub(super) fn new(data: NodeData) -> Self {
        Self {
            data,
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
        }
    }

    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    pub fn first_child(&self) -> Option<NodeId> {
        self.first_child
    }

    pub fn next_sibling(&self) -> Option<NodeId> {
        self.next_sibling
    }

    pub fn prev_sibling(&self) -> Option<NodeId> {
        self.prev_sibling
    }

    pub fn element(&self) -> Option<&ElementData> {
        self.data.as_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_names_fold_case_but_foreign_names_do_not() {
        assert_eq!(LocalName::html("DiV").as_str(), "div");
        assert_eq!(LocalName::verbatim("viewBox").as_str(), "viewBox");
    }

    #[test]
    fn is_html_distinguishes_namespace_from_name() {
        let svg_title = QualName {
            ns: Namespace::Svg,
            local: LocalName::verbatim("title"),
        };
        assert!(QualName::html("TITLE").is_html("title"));
        assert!(!svg_title.is_html("title"));
    }

    #[test]
    fn duplicate_attributes_keep_the_first_value() {
        let mut el = ElementData::new(QualName::html("p"));
        el.set_if_absent(LocalName::html("id"), "a".into());
        el.set_if_absent(LocalName::html("ID"), "b".into());
        assert_eq!(el.id(), Some("a"));
        assert_eq!(el.attrs().len(), 1);
    }

    #[test]
    fn set_replaces_where_parsing_would_have_kept_the_first() {
        let mut el = ElementData::new(QualName::html("p"));
        el.set_if_absent(LocalName::html("id"), "a".into());
        el.set(LocalName::html("id"), "b".into());
        assert_eq!(el.id(), Some("b"));
        assert_eq!(el.attrs().len(), 1);
    }

    #[test]
    fn classes_split_on_any_ascii_whitespace() {
        let mut el = ElementData::new(QualName::html("p"));
        el.set_if_absent(LocalName::html("class"), "  a\tb\nc ".into());
        assert_eq!(el.classes().collect::<Vec<_>>(), ["a", "b", "c"]);
        assert!(el.has_class("b"));
        assert!(!el.has_class("d"));
    }

    #[test]
    fn classes_on_an_element_without_the_attribute_are_empty() {
        let el = ElementData::new(QualName::html("p"));
        assert_eq!(el.classes().count(), 0);
    }
}
