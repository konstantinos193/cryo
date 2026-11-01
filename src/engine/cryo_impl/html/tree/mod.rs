//! Tree construction: the token stream becomes a [`Document`].
//!
//! This is the stage that makes a parser agree with other browsers. The
//! tokenizer only reports what the bytes say; everything about what
//! markup *means* — that `<p>` cannot contain `<p>`, that `<li>` closes
//! the previous one, that `<b>bold<p>still bold` keeps its boldness
//! across the block boundary — lives here, as a state machine over
//! insertion modes.
//!
//! The modes are the spec's, named the same way, because almost every
//! rule in them is a recovery rule for markup that is already wrong. They
//! cannot be derived from a tidier model: they *are* the model, agreed
//! between implementations by writing down what browsers already did.
//!
//! Modes implemented here: `Initial`, `BeforeHtml`, `BeforeHead`,
//! `InHead`, `InHeadNoscript`, `AfterHead`, `InBody`, `Text`, `AfterBody`
//! and `AfterAfterBody` — everything an ordinary document needs. The
//! table, select, template and frameset modes are the next increment;
//! until they land, their elements take the generic start-tag path, which
//! nests them where they appear rather than restructuring them.
//!
//! Layout:
//! - this file — the builder, insertion, and the modes around the head
//! - [`stack`] — open elements, scopes, active formatting elements
//! - [`body`] — `InBody` and the adoption agency

mod body;
mod foreign;
mod stack;
mod table;

use super::tokenizer::{is_whitespace, State, Tag, Token, Tokenizer};
use crate::engine::cryo_impl::dom::{
    DoctypeData, Document, ElementData, LocalName, NodeData, NodeId, QualName, QuirksMode,
};
use stack::{ActiveFormatting, OpenElements};

/// Parse a document from markup.
///
/// Infallible by design: HTML has no parse error that stops a load, so
/// every input produces a tree. There is no `Result` to forget to handle
/// and no way for a caller to receive a half-built document.
pub fn parse(input: &str) -> Document {
    TreeBuilder::new(input).run()
}

/// The insertion modes. See the module docs for which are implemented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    /// Inside a raw-text or RCDATA element, collecting its text.
    Text,
    InTable,
    /// Buffering character tokens seen inside a table, because whether
    /// they stay or get fostered out depends on whether any of them turns
    /// out to be non-whitespace.
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    AfterBody,
    AfterAfterBody,
}

/// Where a node is about to be inserted. `before` is set only when
/// foster parenting redirects an insertion out of a table.
#[derive(Clone, Copy, Debug)]
struct InsertionPoint {
    parent: NodeId,
    before: Option<NodeId>,
}

impl InsertionPoint {
    const fn inside(parent: NodeId) -> Self {
        Self {
            parent,
            before: None,
        }
    }
}

/// Whether a handler finished with the token or wants it reprocessed in
/// whatever mode was just switched to.
///
/// The spec's "reprocess the token" instruction, made explicit. A handler
/// that reprocesses must change the mode first, or the driver would spin.
enum Next {
    Done,
    Reprocess(Token),
}

/// Elements that are inserted and immediately closed — they can never
/// have children, so an end tag for one is always a parse error.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input",
    "keygen", "link", "meta", "param", "source", "track", "wbr",
];

/// Start tags that belong in the head wherever they appear.
const HEAD_ELEMENTS: &[&str] = &[
    "base", "basefont", "bgsound", "link", "meta", "noframes", "script", "style", "template",
    "title",
];

/// Elements whose direct children must be table structure. Anything else
/// inserted while one of these is current gets fostered out of the table.
const TABLE_CONTEXT: &[&str] = &["table", "tbody", "tfoot", "thead", "tr"];

/// Public identifier prefixes that force full quirks mode.
///
/// A short, representative set rather than the spec's full table: the
/// HTML 3.2 and 4.0-era doctypes that actually appear on pages old enough
/// to still need quirks layout.
const QUIRKS_PUBLIC_PREFIXES: &[&str] = &[
    "-//W3C//DTD HTML 3",
    "-//W3C//DTD HTML 4.0 Transitional//",
    "-//W3C//DTD HTML 4.0 Frameset//",
    "-//IETF//DTD HTML",
    "-//W3O//DTD W3 HTML",
    "-//Netscape Comm. Corp.//DTD HTML",
    "html",
];

/// Public identifier prefixes that mean "almost standards".
const LIMITED_QUIRKS_PUBLIC_PREFIXES: &[&str] = &[
    "-//W3C//DTD XHTML 1.0 Transitional//",
    "-//W3C//DTD XHTML 1.0 Frameset//",
];

/// The parser: a tokenizer, the tree being built, and the state the
/// insertion modes share.
struct TreeBuilder {
    tokenizer: Tokenizer,
    doc: Document,
    open: OpenElements,
    formatting: ActiveFormatting,
    mode: Mode,
    /// Where [`Mode::Text`] returns to once the raw-text element closes.
    original_mode: Mode,
    /// The `<head>` element, so `AfterHead` can put stray head content
    /// back inside it without searching the tree.
    head: Option<NodeId>,
    /// The spec's form element pointer: a nested `<form>` is ignored
    /// rather than nested, which is why this is a pointer and not a
    /// stack.
    form: Option<NodeId>,
    /// A `<pre>` or `<textarea>` swallows one leading newline, because
    /// authors write the content on the line after the tag.
    ignore_next_newline: bool,
    /// When set, insertions inside a table are redirected to before it.
    /// See [`TreeBuilder::insertion_point`].
    foster_parenting: bool,
    /// Characters seen in [`Mode::InTableText`], held until something
    /// else arrives and their fate is known.
    pending_table_text: Vec<char>,
    /// Set once EOF has been handled; stops the driver.
    stopped: bool,
}

impl TreeBuilder {
    fn new(input: &str) -> Self {
        Self {
            tokenizer: Tokenizer::new(input),
            doc: Document::new(),
            open: OpenElements::default(),
            formatting: ActiveFormatting::default(),
            mode: Mode::Initial,
            original_mode: Mode::InBody,
            head: None,
            form: None,
            ignore_next_newline: false,
            foster_parenting: false,
            pending_table_text: Vec::new(),
            stopped: false,
        }
    }

    /// Pump tokens until EOF is handled, then hand back the document.
    fn run(mut self) -> Document {
        while !self.stopped {
            let token = self.tokenizer.next_token();
            self.process(token);
        }
        self.doc
    }

    /// Run a token through the current mode, repeating while a mode asks
    /// for it to be reprocessed.
    fn process(&mut self, token: Token) {
        let mut token = token;
        loop {
            match self.dispatch(token) {
                Next::Done => return,
                Next::Reprocess(again) => token = again,
            }
        }
    }

    fn dispatch(&mut self, token: Token) -> Next {
        match self.mode {
            Mode::Initial => self.initial(token),
            Mode::BeforeHtml => self.before_html(token),
            Mode::BeforeHead => self.before_head(token),
            Mode::InHead => self.in_head(token),
            Mode::InHeadNoscript => self.in_head_noscript(token),
            Mode::AfterHead => self.after_head(token),
            Mode::InBody => self.in_body(token),
            Mode::Text => self.text(token),
            Mode::InTable => self.in_table(token),
            Mode::InTableText => self.in_table_text(token),
            Mode::InCaption => self.in_caption(token),
            Mode::InColumnGroup => self.in_column_group(token),
            Mode::InTableBody => self.in_table_body(token),
            Mode::InRow => self.in_row(token),
            Mode::InCell => self.in_cell(token),
            Mode::AfterBody => self.after_body(token),
            Mode::AfterAfterBody => self.after_after_body(token),
        }
    }

    // ---- insertion -------------------------------------------------------

    /// Where a new node goes.
    ///
    /// Normally inside the innermost open element. The exception is
    /// *foster parenting*: content that lands inside a `<table>` but
    /// outside any cell is moved to just **before** the table instead.
    /// That is why an insertion is a position rather than a parent —
    /// `<table><div>x</div></table>` puts the div before the table, which
    /// is what every browser does and what layout then has to agree with.
    fn insertion_point(&self) -> InsertionPoint {
        let fallback = self.doc.root();
        let target = self.open.current().unwrap_or(fallback);
        if !self.foster_parenting || !self.is_table_context(target) {
            return InsertionPoint::inside(target);
        }

        // Inside a table: aim before the innermost open table.
        let table = self
            .open
            .iter_inward()
            .find(|&id| self.doc.is_html_element(id, "table"))
            .and_then(|id| self.open.position(id).map(|index| (id, index)));
        let Some((table, index)) = table else {
            // A table context with no table: the html element takes it.
            return InsertionPoint::inside(self.open.at(0).unwrap_or(fallback));
        };
        match self.doc.node(table).parent() {
            Some(parent) => InsertionPoint {
                parent,
                before: Some(table),
            },
            // A detached table — only reachable mid-reparenting; the
            // element below it on the stack is the next best home.
            None => InsertionPoint::inside(
                index
                    .checked_sub(1)
                    .and_then(|below| self.open.at(below))
                    .unwrap_or(fallback),
            ),
        }
    }

    /// Whether this element is one whose direct children must be table
    /// structure, so anything else gets fostered out.
    fn is_table_context(&self, id: NodeId) -> bool {
        TABLE_CONTEXT
            .iter()
            .any(|&local| self.doc.is_html_element(id, local))
    }

    /// Build an element from a start tag, applying the first-wins rule
    /// for duplicate attributes.
    fn create_element(&mut self, tag: &Tag) -> NodeId {
        let id = self
            .doc
            .create(NodeData::Element(ElementData::new(QualName::html(
                &tag.name,
            ))));
        if let Some(element) = self.doc.element_mut(id) {
            for (name, value) in &tag.attrs {
                element.set_if_absent(
                    LocalName::verbatim(foreign::adjust_attribute(name)),
                    value.clone(),
                );
            }
        }
        id
    }

    /// Put `id` at `point`, which is either the end of a parent or just
    /// before a sibling.
    fn insert_node(&mut self, point: InsertionPoint, id: NodeId) {
        match point.before {
            Some(reference) => self.doc.insert_before(reference, id),
            None => self.doc.append(point.parent, id),
        }
    }

    /// Insert an element and leave it open.
    fn insert_element(&mut self, tag: &Tag) -> NodeId {
        let id = self.create_element(tag);
        let point = self.insertion_point();
        self.insert_node(point, id);
        self.open.push(id);
        id
    }

    /// Whether the insertion point is inside an `<svg>` or `<math>`
    /// subtree.
    ///
    /// The one rule that depends on it: HTML ignores a trailing slash,
    /// so `<div/>` opens a div — but **foreign content honours it**,
    /// and an icon is written `<path …/><path …/>`. Ignoring the flag
    /// there nests every shape inside the one before it, and since
    /// [`svg::render`](crate::engine::cryo_impl::svg) reads an svg's
    /// own children, only the first one is ever drawn.
    fn in_foreign_content(&self) -> bool {
        self.open
            .iter_inward()
            .any(|id| self.doc.is_html_element(id, "svg") || self.doc.is_html_element(id, "math"))
    }

    /// Insert an element that cannot have children.
    fn insert_void(&mut self, tag: &Tag) -> NodeId {
        let id = self.create_element(tag);
        let point = self.insertion_point();
        self.insert_node(point, id);
        id
    }

    /// Insert an element by name only, for the ones the parser invents —
    /// an implied `<html>`, `<head>`, `<body>` or `<tbody>`.
    fn insert_implied(&mut self, local: &str) -> NodeId {
        let id = self.doc.create_html_element(local);
        let point = self.insertion_point();
        self.insert_node(point, id);
        self.open.push(id);
        id
    }

    fn insert_text(&mut self, c: char) {
        let point = self.insertion_point();
        let text = c.to_string();
        match point.before {
            Some(reference) => self.doc.insert_text_before(reference, &text),
            None => self.doc.append_text(point.parent, &text),
        }
    }

    fn insert_comment(&mut self, data: &str) {
        let id = self.doc.create(NodeData::Comment(data.to_owned()));
        let point = self.insertion_point();
        self.insert_node(point, id);
    }

    /// Insert a comment as a child of the document itself, which is where
    /// a comment before `<html>` or after `</html>` belongs.
    fn insert_comment_at_root(&mut self, data: &str) {
        let id = self.doc.create(NodeData::Comment(data.to_owned()));
        let root = self.doc.root();
        self.doc.append(root, id);
    }

    /// Add any attributes the token has that the element lacks. A second
    /// `<html>` or `<body>` tag merges rather than nesting.
    fn merge_attributes(&mut self, into: NodeId, tag: &Tag) {
        if let Some(element) = self.doc.element_mut(into) {
            for (name, value) in &tag.attrs {
                element.set_if_absent(
                    LocalName::verbatim(foreign::adjust_attribute(name)),
                    value.clone(),
                );
            }
        }
    }

    /// Parse an element's contents as raw text: `<style>`, `<script>`,
    /// `<title>`, `<textarea>`.
    ///
    /// The tokenizer cannot know to do this on its own — which state a
    /// `<title>` switches to depends on the tree — so the parser pushes
    /// it there and remembers where to come back to.
    fn parse_raw_text(&mut self, tag: &Tag, state: State) {
        self.insert_element(tag);
        self.tokenizer.set_state(state);
        self.original_mode = self.mode;
        self.mode = Mode::Text;
    }

    // ---- the modes around the head ---------------------------------------

    fn initial(&mut self, token: Token) -> Next {
        match token {
            Token::Character(c) if is_whitespace(c) => Next::Done,
            Token::Comment(data) => {
                self.insert_comment_at_root(&data);
                Next::Done
            }
            Token::Doctype {
                name,
                public_id,
                system_id,
                force_quirks,
            } => {
                let quirks = quirks_for(
                    &name,
                    public_id.as_deref(),
                    system_id.as_deref(),
                    force_quirks,
                );
                self.doc.set_quirks(quirks);
                let node = self.doc.create(NodeData::Doctype(DoctypeData {
                    name,
                    public_id: public_id.unwrap_or_default(),
                    system_id: system_id.unwrap_or_default(),
                }));
                let root = self.doc.root();
                self.doc.append(root, node);
                self.mode = Mode::BeforeHtml;
                Next::Done
            }
            // No doctype at all: quirks mode, and the token belongs to the
            // document proper.
            other => {
                self.doc.set_quirks(QuirksMode::Quirks);
                self.mode = Mode::BeforeHtml;
                Next::Reprocess(other)
            }
        }
    }

    fn before_html(&mut self, token: Token) -> Next {
        match token {
            Token::Doctype { .. } => Next::Done,
            Token::Comment(data) => {
                self.insert_comment_at_root(&data);
                Next::Done
            }
            Token::Character(c) if is_whitespace(c) => Next::Done,
            Token::StartTag(tag) if tag.name == "html" => {
                self.insert_element(&tag);
                self.mode = Mode::BeforeHead;
                Next::Done
            }
            Token::EndTag(ref tag)
                if !matches!(tag.name.as_str(), "head" | "body" | "html" | "br") =>
            {
                Next::Done
            }
            other => {
                self.insert_implied("html");
                self.mode = Mode::BeforeHead;
                Next::Reprocess(other)
            }
        }
    }

    fn before_head(&mut self, token: Token) -> Next {
        match token {
            Token::Character(c) if is_whitespace(c) => Next::Done,
            Token::Comment(data) => {
                self.insert_comment(&data);
                Next::Done
            }
            Token::Doctype { .. } => Next::Done,
            Token::StartTag(tag) if tag.name == "html" => self.in_body(Token::StartTag(tag)),
            Token::StartTag(tag) if tag.name == "head" => {
                let id = self.insert_element(&tag);
                self.head = Some(id);
                self.mode = Mode::InHead;
                Next::Done
            }
            Token::EndTag(ref tag)
                if !matches!(tag.name.as_str(), "head" | "body" | "html" | "br") =>
            {
                Next::Done
            }
            other => {
                let id = self.insert_implied("head");
                self.head = Some(id);
                self.mode = Mode::InHead;
                Next::Reprocess(other)
            }
        }
    }

    fn in_head(&mut self, token: Token) -> Next {
        match token {
            Token::Character(c) if is_whitespace(c) => {
                self.insert_text(c);
                Next::Done
            }
            Token::Comment(data) => {
                self.insert_comment(&data);
                Next::Done
            }
            Token::Doctype { .. } => Next::Done,
            Token::StartTag(tag) if tag.name == "html" => self.in_body(Token::StartTag(tag)),
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "base" | "basefont" | "bgsound" | "link" | "meta"
                ) =>
            {
                self.insert_void(&tag);
                Next::Done
            }
            Token::StartTag(tag) if tag.name == "title" => {
                self.parse_raw_text(&tag, State::Rcdata);
                Next::Done
            }
            Token::StartTag(tag) if matches!(tag.name.as_str(), "style" | "noframes") => {
                self.parse_raw_text(&tag, State::Rawtext);
                Next::Done
            }
            Token::StartTag(tag) if tag.name == "script" => {
                self.parse_raw_text(&tag, State::ScriptData);
                Next::Done
            }
            // Scripting is disabled until a VM is linked, so `<noscript>`
            // content is parsed as ordinary markup rather than as text.
            Token::StartTag(tag) if tag.name == "noscript" => {
                self.insert_element(&tag);
                self.mode = Mode::InHeadNoscript;
                Next::Done
            }
            Token::EndTag(ref tag) if tag.name == "head" => {
                self.open.pop();
                self.mode = Mode::AfterHead;
                Next::Done
            }
            Token::StartTag(ref tag) if tag.name == "head" => Next::Done,
            Token::EndTag(ref tag) if !matches!(tag.name.as_str(), "body" | "html" | "br") => {
                Next::Done
            }
            other => {
                self.open.pop();
                self.mode = Mode::AfterHead;
                Next::Reprocess(other)
            }
        }
    }

    fn in_head_noscript(&mut self, token: Token) -> Next {
        match token {
            Token::EndTag(ref tag) if tag.name == "noscript" => {
                self.open.pop();
                self.mode = Mode::InHead;
                Next::Done
            }
            Token::Doctype { .. } => Next::Done,
            Token::Character(c) if is_whitespace(c) => self.in_head(Token::Character(c)),
            Token::Comment(data) => self.in_head(Token::Comment(data)),
            Token::StartTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"
                ) =>
            {
                self.in_head(Token::StartTag(tag))
            }
            Token::EndTag(ref tag) if tag.name == "br" => {
                self.open.pop();
                self.mode = Mode::InHead;
                Next::Reprocess(token)
            }
            other => {
                self.open.pop();
                self.mode = Mode::InHead;
                Next::Reprocess(other)
            }
        }
    }

    fn after_head(&mut self, token: Token) -> Next {
        match token {
            Token::Character(c) if is_whitespace(c) => {
                self.insert_text(c);
                Next::Done
            }
            Token::Comment(data) => {
                self.insert_comment(&data);
                Next::Done
            }
            Token::Doctype { .. } => Next::Done,
            Token::StartTag(tag) if tag.name == "html" => self.in_body(Token::StartTag(tag)),
            Token::StartTag(tag) if tag.name == "body" => {
                self.insert_element(&tag);
                self.mode = Mode::InBody;
                Next::Done
            }
            // Head content that strayed past `</head>` goes back into the
            // head, which is still in the tree even though it is closed.
            Token::StartTag(tag) if HEAD_ELEMENTS.contains(&tag.name.as_str()) => {
                let head = self.head;
                if let Some(head) = head {
                    self.open.push(head);
                }
                let result = self.in_head(Token::StartTag(tag));
                if let Some(head) = head {
                    self.open.remove(head);
                }
                result
            }
            Token::StartTag(ref tag) if tag.name == "head" => Next::Done,
            Token::EndTag(ref tag) if !matches!(tag.name.as_str(), "body" | "html" | "br") => {
                Next::Done
            }
            other => {
                self.insert_implied("body");
                self.mode = Mode::InBody;
                Next::Reprocess(other)
            }
        }
    }

    /// Inside a raw-text element: everything is text until its end tag.
    fn text(&mut self, token: Token) -> Next {
        match token {
            Token::Character(c) => {
                // `<pre>`/`<textarea>` eat one leading newline.
                if self.ignore_next_newline {
                    self.ignore_next_newline = false;
                    if c == '\n' {
                        return Next::Done;
                    }
                }
                self.insert_text(c);
                Next::Done
            }
            Token::EndTag(_) => {
                self.open.pop();
                self.mode = self.original_mode;
                Next::Done
            }
            // Unterminated raw text: close the element and carry on, so
            // an unclosed `<script>` does not swallow the document.
            Token::Eof => {
                self.open.pop();
                self.mode = self.original_mode;
                Next::Reprocess(Token::Eof)
            }
            _ => Next::Done,
        }
    }

    fn after_body(&mut self, token: Token) -> Next {
        match token {
            Token::Character(c) if is_whitespace(c) => self.in_body(Token::Character(c)),
            Token::Comment(data) => {
                // Belongs after the body, inside `<html>`.
                let parent = self.open.at(0).unwrap_or_else(|| self.doc.root());
                let id = self.doc.create(NodeData::Comment(data));
                self.doc.append(parent, id);
                Next::Done
            }
            Token::Doctype { .. } => Next::Done,
            Token::StartTag(tag) if tag.name == "html" => self.in_body(Token::StartTag(tag)),
            Token::EndTag(ref tag) if tag.name == "html" => {
                self.mode = Mode::AfterAfterBody;
                Next::Done
            }
            Token::Eof => {
                self.stopped = true;
                Next::Done
            }
            other => {
                self.mode = Mode::InBody;
                Next::Reprocess(other)
            }
        }
    }

    fn after_after_body(&mut self, token: Token) -> Next {
        match token {
            Token::Comment(data) => {
                self.insert_comment_at_root(&data);
                Next::Done
            }
            Token::Doctype { .. } => Next::Done,
            Token::Character(c) if is_whitespace(c) => self.in_body(Token::Character(c)),
            Token::StartTag(tag) if tag.name == "html" => self.in_body(Token::StartTag(tag)),
            Token::Eof => {
                self.stopped = true;
                Next::Done
            }
            other => {
                self.mode = Mode::InBody;
                Next::Reprocess(other)
            }
        }
    }
}

/// Decide quirks mode from the doctype.
///
/// The common case is the only one that matters for new content: a bare
/// `<!DOCTYPE html>` with no ids is standards mode. Everything unreadable
/// or legacy falls back to quirks, which is the safe direction — quirks
/// renders old pages as their authors saw them.
fn quirks_for(
    name: &str,
    public_id: Option<&str>,
    system_id: Option<&str>,
    force_quirks: bool,
) -> QuirksMode {
    if force_quirks || name != "html" {
        return QuirksMode::Quirks;
    }
    let public = public_id.unwrap_or_default().to_ascii_lowercase();
    if QUIRKS_PUBLIC_PREFIXES
        .iter()
        .any(|prefix| public.starts_with(&prefix.to_ascii_lowercase()))
    {
        return QuirksMode::Quirks;
    }
    // An HTML 4.01 public id with no system id is quirks; with one it is
    // standards, which is the one place the system id changes the answer.
    if public.starts_with("-//w3c//dtd html 4.01 frameset//")
        || public.starts_with("-//w3c//dtd html 4.01 transitional//")
    {
        return if system_id.is_some() {
            QuirksMode::LimitedQuirks
        } else {
            QuirksMode::Quirks
        };
    }
    if LIMITED_QUIRKS_PUBLIC_PREFIXES
        .iter()
        .any(|prefix| public.starts_with(&prefix.to_ascii_lowercase()))
    {
        return QuirksMode::LimitedQuirks;
    }
    QuirksMode::NoQuirks
}
