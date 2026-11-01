//! The table insertion modes, and the structure the parser invents to
//! make a table well-formed.
//!
//! Tables are the one place HTML's tree does not follow its markup. Three
//! things happen here that happen nowhere else:
//!
//! 1. **Implied structure.** `<table><tr><td>x` has no `<tbody>`, but the
//!    tree gets one anyway, because that is what the table layout
//!    algorithm — and every `querySelector` on the page — expects.
//! 2. **Foster parenting.** Content inside a table but outside a cell is
//!    moved to *before* the table:
//!    `<table><div>x</div><tr><td>y` renders the `x` above the table.
//!    Authors never write that on purpose; it happens constantly by
//!    accident, and browsers all agree on the recovery.
//! 3. **Buffered text.** Whitespace inside a table stays put, but real
//!    text gets fostered out — and the tokenizer emits one character at a
//!    time, so the decision cannot be made until the run ends. Hence
//!    [`super::Mode::InTableText`], which holds the characters until it
//!    knows.
//!
//! Cells also isolate formatting: entering one pushes a marker onto the
//! active formatting list, so a `<b>` left open outside the table does
//! not bleed into every cell.

use super::stack::Scope;
use super::{Mode, Next, TreeBuilder};
use crate::engine::cryo_impl::dom::NodeId;
use crate::engine::cryo_impl::html::tokenizer::{is_whitespace, Tag, Token};

/// The table-structure start tags that close an enclosing caption or
/// cell when they appear inside one.
const TABLE_SECTIONS: &[&str] = &[
    "caption", "col", "colgroup", "tbody", "td", "tfoot", "th", "thead", "tr",
];

/// The row-group elements.
const ROW_GROUPS: &[&str] = &["tbody", "tfoot", "thead"];

/// The cell elements.
const CELLS: &[&str] = &["td", "th"];

/// What "clear the stack back to a table context" stops at. `template`
/// is listed for forward compatibility with the template mode; `html`
/// guarantees the walk terminates.
const TABLE_CONTEXT_FLOOR: &[&str] = &["table", "template", "html"];
const TABLE_BODY_CONTEXT_FLOOR: &[&str] = &["tbody", "tfoot", "thead", "template", "html"];
const TABLE_ROW_CONTEXT_FLOOR: &[&str] = &["tr", "template", "html"];

impl TreeBuilder {
    // ---- InTable ---------------------------------------------------------

    pub(super) fn in_table(&mut self, token: Token) -> Next {
        match token {
            // Character data in a table context has to be buffered: its
            // destination depends on whether the whole run is whitespace.
            Token::Character(c)
                if self
                    .open
                    .current()
                    .is_some_and(|id| self.is_table_context(id)) =>
            {
                self.pending_table_text.clear();
                self.original_mode = Mode::InTable;
                self.mode = Mode::InTableText;
                Next::Reprocess(Token::Character(c))
            }
            Token::Comment(data) => {
                self.insert_comment(&data);
                Next::Done
            }
            Token::Doctype { .. } => Next::Done,
            Token::StartTag(tag) => self.in_table_start(tag),
            Token::EndTag(tag) => self.in_table_end(tag),
            Token::Eof => {
                self.stopped = true;
                Next::Done
            }
            other => self.foster_into_body(other),
        }
    }

    fn in_table_start(&mut self, tag: Tag) -> Next {
        match tag.name.as_str() {
            "caption" => {
                self.clear_back_to(TABLE_CONTEXT_FLOOR);
                // A caption isolates formatting, like a cell.
                self.formatting.push_marker();
                self.insert_element(&tag);
                self.mode = Mode::InCaption;
                Next::Done
            }
            "colgroup" => {
                self.clear_back_to(TABLE_CONTEXT_FLOOR);
                self.insert_element(&tag);
                self.mode = Mode::InColumnGroup;
                Next::Done
            }
            // A bare `<col>` gets the `<colgroup>` it never had.
            "col" => {
                self.clear_back_to(TABLE_CONTEXT_FLOOR);
                self.insert_implied("colgroup");
                self.mode = Mode::InColumnGroup;
                Next::Reprocess(Token::StartTag(tag))
            }
            "tbody" | "tfoot" | "thead" => {
                self.clear_back_to(TABLE_CONTEXT_FLOOR);
                self.insert_element(&tag);
                self.mode = Mode::InTableBody;
                Next::Done
            }
            // A row or cell straight inside `<table>` implies a `<tbody>`.
            "td" | "th" | "tr" => {
                self.clear_back_to(TABLE_CONTEXT_FLOOR);
                self.insert_implied("tbody");
                self.mode = Mode::InTableBody;
                Next::Reprocess(Token::StartTag(tag))
            }
            // A `<table>` inside a table closes the outer one rather than
            // nesting: nested tables need an intervening cell.
            "table" => {
                if !self.open.has_in_scope(&self.doc, "table", Scope::Table) {
                    return Next::Done;
                }
                self.open.pop_until_html(&self.doc, "table");
                self.reset_insertion_mode();
                Next::Reprocess(Token::StartTag(tag))
            }
            "style" | "script" | "template" | "title" => self.in_head(Token::StartTag(tag)),
            // A hidden input is allowed to stay inside the table; any
            // other input is content and gets fostered out.
            "input" if is_hidden_input(&tag) => {
                self.insert_void(&tag);
                Next::Done
            }
            "form" => {
                if self.form.is_none() {
                    let id = self.insert_void(&tag);
                    self.form = Some(id);
                }
                Next::Done
            }
            _ => self.foster_into_body(Token::StartTag(tag)),
        }
    }

    fn in_table_end(&mut self, tag: Tag) -> Next {
        match tag.name.as_str() {
            "table" => {
                if self.open.has_in_scope(&self.doc, "table", Scope::Table) {
                    self.open.pop_until_html(&self.doc, "table");
                    self.reset_insertion_mode();
                }
                Next::Done
            }
            // Structure end tags with no matching open element: dropped,
            // because closing something that is not open would close the
            // table itself.
            "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot" | "th"
            | "thead" | "tr" => Next::Done,
            "template" => self.in_head(Token::EndTag(tag)),
            _ => self.foster_into_body(Token::EndTag(tag)),
        }
    }

    /// Run a token through `InBody` with foster parenting on, so whatever
    /// it inserts lands before the table instead of inside it.
    fn foster_into_body(&mut self, token: Token) -> Next {
        self.foster_parenting = true;
        let result = self.in_body(token);
        self.foster_parenting = false;
        result
    }

    // ---- InTableText -----------------------------------------------------

    /// Hold character tokens until the run ends, then place them all.
    ///
    /// Whitespace-only runs stay inside the table — they are the newlines
    /// and indentation between rows. A run with any real text in it is
    /// content that does not belong in table structure, so the whole run
    /// is fostered out.
    pub(super) fn in_table_text(&mut self, token: Token) -> Next {
        match token {
            Token::Character('\0') => Next::Done,
            Token::Character(c) => {
                self.pending_table_text.push(c);
                Next::Done
            }
            other => {
                let buffered = std::mem::take(&mut self.pending_table_text);
                self.mode = self.original_mode;
                if buffered.iter().any(|&c| !is_whitespace(c)) {
                    for c in buffered {
                        self.foster_into_body(Token::Character(c));
                    }
                } else {
                    for c in buffered {
                        self.insert_text(c);
                    }
                }
                Next::Reprocess(other)
            }
        }
    }

    // ---- InCaption -------------------------------------------------------

    pub(super) fn in_caption(&mut self, token: Token) -> Next {
        let closing = match &token {
            Token::EndTag(tag) if tag.name == "caption" => Some(false),
            Token::EndTag(tag) if tag.name == "table" => Some(true),
            Token::StartTag(tag) if TABLE_SECTIONS.contains(&tag.name.as_str()) => Some(true),
            Token::EndTag(tag)
                if matches!(
                    tag.name.as_str(),
                    "body"
                        | "col"
                        | "colgroup"
                        | "html"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                return Next::Done
            }
            _ => None,
        };

        let Some(reprocess) = closing else {
            return self.in_body(token);
        };
        if !self.open.has_in_scope(&self.doc, "caption", Scope::Table) {
            return Next::Done;
        }
        self.generate_implied_end_tags();
        self.open.pop_until_html(&self.doc, "caption");
        self.formatting.clear_to_last_marker();
        self.mode = Mode::InTable;
        if reprocess {
            Next::Reprocess(token)
        } else {
            Next::Done
        }
    }

    // ---- InColumnGroup ---------------------------------------------------

    pub(super) fn in_column_group(&mut self, token: Token) -> Next {
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
            Token::StartTag(tag) if tag.name == "col" => {
                self.insert_void(&tag);
                Next::Done
            }
            Token::EndTag(tag) if tag.name == "colgroup" => {
                if self.current_is("colgroup") {
                    self.open.pop();
                    self.mode = Mode::InTable;
                }
                Next::Done
            }
            Token::EndTag(tag) if tag.name == "col" => Next::Done,
            other => {
                if !self.current_is("colgroup") {
                    return Next::Done;
                }
                self.open.pop();
                self.mode = Mode::InTable;
                Next::Reprocess(other)
            }
        }
    }

    // ---- InTableBody -----------------------------------------------------

    pub(super) fn in_table_body(&mut self, token: Token) -> Next {
        match token {
            Token::StartTag(tag) if tag.name == "tr" => {
                self.clear_back_to(TABLE_BODY_CONTEXT_FLOOR);
                self.insert_element(&tag);
                self.mode = Mode::InRow;
                Next::Done
            }
            // A cell with no row gets one.
            Token::StartTag(tag) if CELLS.contains(&tag.name.as_str()) => {
                self.clear_back_to(TABLE_BODY_CONTEXT_FLOOR);
                self.insert_implied("tr");
                self.mode = Mode::InRow;
                Next::Reprocess(Token::StartTag(tag))
            }
            Token::EndTag(ref tag) if ROW_GROUPS.contains(&tag.name.as_str()) => {
                if self.open.has_in_scope(&self.doc, &tag.name, Scope::Table) {
                    self.clear_back_to(TABLE_BODY_CONTEXT_FLOOR);
                    self.open.pop();
                    self.mode = Mode::InTable;
                }
                Next::Done
            }
            Token::EndTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" | "tr"
                ) =>
            {
                Next::Done
            }
            other => {
                // A new section, or the table ending, closes this one.
                let closes = match &other {
                    Token::StartTag(tag) => matches!(
                        tag.name.as_str(),
                        "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead"
                    ),
                    Token::EndTag(tag) => tag.name == "table",
                    _ => false,
                };
                if !closes {
                    return self.in_table(other);
                }
                if !self
                    .open
                    .has_any_in_scope(&self.doc, ROW_GROUPS, Scope::Table)
                {
                    return Next::Done;
                }
                self.clear_back_to(TABLE_BODY_CONTEXT_FLOOR);
                self.open.pop();
                self.mode = Mode::InTable;
                Next::Reprocess(other)
            }
        }
    }

    // ---- InRow -----------------------------------------------------------

    pub(super) fn in_row(&mut self, token: Token) -> Next {
        match token {
            Token::StartTag(tag) if CELLS.contains(&tag.name.as_str()) => {
                self.clear_back_to(TABLE_ROW_CONTEXT_FLOOR);
                self.insert_element(&tag);
                self.mode = Mode::InCell;
                // A cell isolates formatting from outside the table.
                self.formatting.push_marker();
                Next::Done
            }
            Token::EndTag(ref tag) if tag.name == "tr" => {
                if self.open.has_in_scope(&self.doc, "tr", Scope::Table) {
                    self.clear_back_to(TABLE_ROW_CONTEXT_FLOOR);
                    self.open.pop();
                    self.mode = Mode::InTableBody;
                }
                Next::Done
            }
            Token::EndTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th"
                ) =>
            {
                Next::Done
            }
            other => {
                let closes = match &other {
                    Token::StartTag(tag) => matches!(
                        tag.name.as_str(),
                        "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" | "tr"
                    ),
                    Token::EndTag(tag) => {
                        tag.name == "table" || ROW_GROUPS.contains(&tag.name.as_str())
                    }
                    _ => false,
                };
                if !closes {
                    return self.in_table(other);
                }
                if !self.open.has_in_scope(&self.doc, "tr", Scope::Table) {
                    return Next::Done;
                }
                self.clear_back_to(TABLE_ROW_CONTEXT_FLOOR);
                self.open.pop();
                self.mode = Mode::InTableBody;
                Next::Reprocess(other)
            }
        }
    }

    // ---- InCell ----------------------------------------------------------

    pub(super) fn in_cell(&mut self, token: Token) -> Next {
        match token {
            Token::EndTag(ref tag) if CELLS.contains(&tag.name.as_str()) => {
                if self.open.has_in_scope(&self.doc, &tag.name, Scope::Table) {
                    self.generate_implied_end_tags();
                    self.open.pop_until_html(&self.doc, &tag.name);
                    self.formatting.clear_to_last_marker();
                    self.mode = Mode::InRow;
                }
                Next::Done
            }
            Token::EndTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html"
                ) =>
            {
                Next::Done
            }
            other => {
                // Anything that implies the cell is over closes it, then
                // gets handled by the row or section above.
                let closes = match &other {
                    Token::StartTag(tag) => TABLE_SECTIONS.contains(&tag.name.as_str()),
                    Token::EndTag(tag) => {
                        matches!(tag.name.as_str(), "table" | "tr")
                            || ROW_GROUPS.contains(&tag.name.as_str())
                    }
                    _ => false,
                };
                if !closes {
                    return self.in_body(other);
                }
                if !self.open.has_any_in_scope(&self.doc, CELLS, Scope::Table) {
                    return Next::Done;
                }
                self.close_cell();
                Next::Reprocess(other)
            }
        }
    }

    /// The spec's "close the cell": finish the open `<td>` or `<th>` and
    /// return to the row.
    fn close_cell(&mut self) {
        self.generate_implied_end_tags();
        self.open.pop_until_any_html(&self.doc, CELLS);
        self.formatting.clear_to_last_marker();
        self.mode = Mode::InRow;
    }

    // ---- shared ----------------------------------------------------------

    fn current_is(&self, local: &str) -> bool {
        self.open
            .current()
            .is_some_and(|id| self.doc.is_html_element(id, local))
    }

    /// Discard whatever stray elements are open inside the table until
    /// the current node is one of `floor`.
    ///
    /// Stray elements get here by being fostered: the content moved out
    /// of the table, but its element stayed on the stack, and the next
    /// piece of table structure has to start from a clean context.
    fn clear_back_to(&mut self, floor: &[&str]) {
        self.open.pop_until_current_is(&self.doc, floor);
    }

    /// The spec's "reset the insertion mode appropriately": work out
    /// which mode the current nesting implies.
    ///
    /// Needed after popping a table, because what follows depends on
    /// where the table was — a table inside a cell returns to `InCell`,
    /// one in the body returns to `InBody`.
    pub(super) fn reset_insertion_mode(&mut self) {
        let nesting: Vec<NodeId> = self.open.iter_inward().collect();
        let depth = nesting.len();
        for (index, id) in nesting.into_iter().enumerate() {
            let outermost = index + 1 == depth;
            let Some(element) = self.doc.element(id) else {
                continue;
            };
            let mode = match element.name.local.as_str() {
                "td" | "th" if !outermost => Mode::InCell,
                "tr" => Mode::InRow,
                "tbody" | "thead" | "tfoot" => Mode::InTableBody,
                "caption" => Mode::InCaption,
                "colgroup" => Mode::InColumnGroup,
                "table" => Mode::InTable,
                "body" => Mode::InBody,
                "head" if !outermost => Mode::InBody,
                "html" if self.head.is_none() => Mode::BeforeHead,
                "html" => Mode::AfterHead,
                _ if outermost => Mode::InBody,
                _ => continue,
            };
            self.mode = mode;
            return;
        }
        self.mode = Mode::InBody;
    }
}

/// Whether this is `<input type=hidden>`, the one input allowed to sit
/// inside table structure rather than being fostered out.
fn is_hidden_input(tag: &Tag) -> bool {
    tag.attrs
        .iter()
        .any(|(name, value)| name == "type" && value.eq_ignore_ascii_case("hidden"))
}

#[cfg(test)]
mod tests {
    use crate::engine::cryo_impl::dom::{Document, NodeData, NodeId};
    use crate::engine::cryo_impl::html::tree::parse;

    /// The body's subtree as indented names, which is where every table
    /// case below ends up.
    fn body_sketch(markup: &str) -> String {
        let doc = parse(markup);
        let body = doc
            .first_html_element("body")
            .expect("a body is always built");
        let mut out = String::new();
        write_children(&doc, body, 0, &mut out);
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
                other => out.push_str(&format!("{other:?}")),
            }
            out.push('\n');
            write_children(doc, id, depth + 1, out);
        }
    }

    #[test]
    fn a_table_gains_the_tbody_the_markup_omitted() {
        assert_eq!(
            body_sketch("<table><tr><td>x</table>"),
            concat!(
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"x\"\n",
            )
        );
    }

    #[test]
    fn an_explicit_tbody_is_not_duplicated() {
        assert_eq!(
            body_sketch("<table><tbody><tr><td>x</td></tr></tbody></table>"),
            "table\n  tbody\n    tr\n      td\n        \"x\"\n"
        );
    }

    #[test]
    fn a_cell_with_no_row_gets_one() {
        assert_eq!(
            body_sketch("<table><td>x</table>"),
            "table\n  tbody\n    tr\n      td\n        \"x\"\n"
        );
    }

    #[test]
    fn a_second_cell_closes_the_first() {
        assert_eq!(
            body_sketch("<table><tr><td>a<td>b</table>"),
            concat!(
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"a\"\n",
                "      td\n",
                "        \"b\"\n",
            )
        );
    }

    #[test]
    fn a_second_row_closes_the_first() {
        assert_eq!(
            body_sketch("<table><tr><td>a<tr><td>b</table>"),
            concat!(
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"a\"\n",
                "    tr\n",
                "      td\n",
                "        \"b\"\n",
            )
        );
    }

    #[test]
    fn head_and_foot_sections_keep_their_source_order() {
        assert_eq!(
            body_sketch("<table><thead><tr><th>h<tbody><tr><td>b<tfoot><tr><td>f</table>"),
            concat!(
                "table\n",
                "  thead\n",
                "    tr\n",
                "      th\n",
                "        \"h\"\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"b\"\n",
                "  tfoot\n",
                "    tr\n",
                "      td\n",
                "        \"f\"\n",
            )
        );
    }

    #[test]
    fn a_caption_sits_inside_the_table() {
        assert_eq!(
            body_sketch("<table><caption>C</caption><tr><td>x</table>"),
            concat!(
                "table\n",
                "  caption\n",
                "    \"C\"\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"x\"\n",
            )
        );
    }

    #[test]
    fn an_unclosed_caption_is_closed_by_the_first_row() {
        assert_eq!(
            body_sketch("<table><caption>C<tr><td>x</table>"),
            concat!(
                "table\n",
                "  caption\n",
                "    \"C\"\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"x\"\n",
            )
        );
    }

    #[test]
    fn a_bare_col_gains_a_colgroup() {
        assert_eq!(
            body_sketch("<table><col><col><tr><td>x</table>"),
            concat!(
                "table\n",
                "  colgroup\n",
                "    col\n",
                "    col\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"x\"\n",
            )
        );
    }

    #[test]
    fn content_inside_a_table_is_fostered_out_before_it() {
        // The headline recovery rule: the div renders *above* the table.
        assert_eq!(
            body_sketch("<table><div>stray</div><tr><td>x</table>"),
            concat!(
                "div\n",
                "  \"stray\"\n",
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"x\"\n",
            )
        );
    }

    #[test]
    fn stray_text_inside_a_table_is_fostered_out_too() {
        assert_eq!(
            body_sketch("<table>stray<tr><td>x</table>"),
            concat!(
                "\"stray\"\n",
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"x\"\n",
            )
        );
    }

    #[test]
    fn whitespace_inside_a_table_stays_inside_it() {
        // Indentation between rows must not be fostered out, or every
        // pretty-printed table would push blank text above itself.
        let sketch = body_sketch("<table>\n  <tr><td>x</table>");
        assert!(
            sketch.starts_with("table\n"),
            "nothing should precede the table, got:\n{sketch}"
        );
    }

    #[test]
    fn text_in_a_cell_is_not_fostered() {
        assert_eq!(
            body_sketch("<table><tr><td>  kept  </table>"),
            "table\n  tbody\n    tr\n      td\n        \"  kept  \"\n"
        );
    }

    #[test]
    fn a_hidden_input_stays_in_the_table_but_a_text_input_is_fostered() {
        let hidden = body_sketch("<table><input type=hidden><tr><td>x</table>");
        assert!(
            hidden.starts_with("table\n"),
            "hidden input should not foster, got:\n{hidden}"
        );
        let visible = body_sketch("<table><input type=text><tr><td>x</table>");
        assert!(
            visible.starts_with("input\n"),
            "a text input should foster out, got:\n{visible}"
        );
    }

    #[test]
    fn a_table_inside_a_table_closes_the_outer_one() {
        // Nested tables need an intervening cell; without one the second
        // `<table>` ends the first.
        assert_eq!(
            body_sketch("<table><tr><td>a</table><table><tr><td>b</table>"),
            concat!(
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"a\"\n",
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"b\"\n",
            )
        );
    }

    #[test]
    fn a_table_nested_in_a_cell_really_nests() {
        assert_eq!(
            body_sketch("<table><tr><td><table><tr><td>inner</table></table>"),
            concat!(
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        table\n",
                "          tbody\n",
                "            tr\n",
                "              td\n",
                "                \"inner\"\n",
            )
        );
    }

    #[test]
    fn content_after_a_table_returns_to_the_body() {
        // `reset_insertion_mode` picking InBody again.
        assert_eq!(
            body_sketch("<table><tr><td>x</table><p>after"),
            concat!(
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        \"x\"\n",
                "p\n",
                "  \"after\"\n",
            )
        );
    }

    #[test]
    fn content_after_an_inner_table_returns_to_the_cell() {
        // `reset_insertion_mode` picking InCell, not InBody — the "more"
        // has to stay in the outer cell.
        assert_eq!(
            body_sketch("<table><tr><td><table><tr><td>in</table>more</table>"),
            concat!(
                "table\n",
                "  tbody\n",
                "    tr\n",
                "      td\n",
                "        table\n",
                "          tbody\n",
                "            tr\n",
                "              td\n",
                "                \"in\"\n",
                "        \"more\"\n",
            )
        );
    }

    #[test]
    fn a_cell_isolates_formatting_from_outside_the_table() {
        // The `<b>` must not be recreated inside the cell: the marker
        // pushed on entering it stops reconstruction.
        let sketch = body_sketch("<b>bold<table><tr><td>plain</table>");
        assert_eq!(
            sketch,
            concat!(
                "b\n",
                "  \"bold\"\n",
                "  table\n",
                "    tbody\n",
                "      tr\n",
                "        td\n",
                "          \"plain\"\n",
            )
        );
    }

    #[test]
    fn a_style_element_inside_a_table_is_not_fostered() {
        let sketch = body_sketch("<table><style>td{color:red}</style><tr><td>x</table>");
        assert!(
            sketch.starts_with("table\n  style\n"),
            "style belongs in the table, got:\n{sketch}"
        );
    }

    #[test]
    fn an_unclosed_table_at_eof_still_produces_its_structure() {
        assert_eq!(
            body_sketch("<table><tr><td>x"),
            "table\n  tbody\n    tr\n      td\n        \"x\"\n"
        );
    }

    #[test]
    fn stray_table_structure_end_tags_are_ignored() {
        assert_eq!(
            body_sketch("<table><tr><td>x</th></tr></tbody></table>"),
            "table\n  tbody\n    tr\n      td\n        \"x\"\n"
        );
    }

    #[test]
    fn a_comment_inside_a_table_stays_there() {
        let sketch = body_sketch("<table><!--c--><tr><td>x</table>");
        assert!(sketch.starts_with("table\n  <!--c-->\n"), "got:\n{sketch}");
    }
}
