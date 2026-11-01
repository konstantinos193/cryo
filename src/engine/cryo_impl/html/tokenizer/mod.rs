//! The HTML tokenizer: a character-level state machine turning markup
//! text into tags, comments, doctypes and characters.
//!
//! It is written as the spec's explicit state machine rather than as
//! hand-rolled scanning, because the states are where all the strange
//! real-world behaviour lives. `<a href=/x b=c>` (unquoted values),
//! `<!-->` (a comment that ends early), `a < b` (a `<` that is just text)
//! and `</p >` all work because a named state handles each, and none of
//! them is derivable from a tidier design. Diverging from the state names
//! would mean losing the ability to check behaviour against the spec
//! text, which is the only practical way to get this right.
//!
//! Structure: this file owns the token types, the tokenizer itself, the
//! driver and the states that read ordinary text. The rest sit beside it
//! by phase — [`tag`] for tags and attributes, [`text`] for the raw-text
//! and script states, [`markup`] for comments, doctypes and CDATA — all
//! as `impl Tokenizer` blocks, so the machine is one type with its states
//! grouped by what they read.
//!
//! The tree builder drives it one token at a time and can push it into a
//! raw-text state through [`Tokenizer::set_state`], which is how
//! `<script>` and `<title>` stop their contents being parsed as markup.
//! That feedback from parser to tokenizer is in the spec too: HTML cannot
//! be tokenized without knowing the tree.

mod markup;
mod tag;
mod text;

use std::collections::VecDeque;

use super::entities;
use text::RawKind;

/// Markup whitespace, re-exported for tree construction: the insertion
/// modes classify character tokens with the same predicate the tokenizer
/// uses, rather than each spelling out the set.
pub(crate) use tag::is_whitespace;

/// A tag's name and attributes, as the tokenizer saw them.
///
/// Attributes are `(name, value)` pairs in source order with duplicates
/// still present — dropping them is the tree builder's job, since the
/// first-wins rule is about element construction, not tokenization.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Tag {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    /// The tag ended with `/>`. Meaningful only for void and foreign
    /// elements; on an HTML element like `<div/>` it is a parse error the
    /// tree builder ignores.
    pub self_closing: bool,
}

/// What the tokenizer produces.
///
/// Characters come out one at a time, as the spec emits them. Batching
/// them into runs would mean the tokenizer deciding where text nodes
/// begin and end, which depends on the tree — so the document coalesces
/// instead, in [`crate::engine::cryo_impl::dom::Document::append_text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Doctype {
        name: String,
        public_id: Option<String>,
        system_id: Option<String>,
        /// The doctype was malformed enough to force quirks mode.
        force_quirks: bool,
    },
    StartTag(Tag),
    EndTag(Tag),
    Comment(String),
    Character(char),
    Eof,
}

/// The tokenizer's states, named as the spec names them.
///
/// Only the variants the tree builder sets are public; the rest are
/// reachable only from inside the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Data,
    /// Everything to EOF is text. Only `<plaintext>` reaches it.
    Plaintext,
    /// Text plus character references, closed by its own end tag:
    /// `<title>`, `<textarea>`.
    Rcdata,
    /// Text with no references: `<style>`, `<xmp>`, `<iframe>`.
    Rawtext,
    /// Raw text with the script comment dance: `<script>`.
    ScriptData,

    TagOpen,
    EndTagOpen,
    TagName,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    AttributeValueDouble,
    AttributeValueSingle,
    AttributeValueUnquoted,
    AfterAttributeValueQuoted,
    SelfClosingStartTag,

    RcdataLessThan,
    RcdataEndTagOpen,
    RcdataEndTagName,
    RawtextLessThan,
    RawtextEndTagOpen,
    RawtextEndTagName,
    ScriptDataLessThan,
    ScriptDataEndTagOpen,
    ScriptDataEndTagName,
    ScriptDataEscapeStart,
    ScriptDataEscapeStartDash,
    ScriptDataEscaped,
    ScriptDataEscapedDash,
    ScriptDataEscapedDashDash,
    ScriptDataEscapedLessThan,
    ScriptDataEscapedEndTagOpen,
    ScriptDataEscapedEndTagName,
    ScriptDataDoubleEscapeStart,
    ScriptDataDoubleEscaped,
    ScriptDataDoubleEscapedDash,
    ScriptDataDoubleEscapedDashDash,
    ScriptDataDoubleEscapedLessThan,
    ScriptDataDoubleEscapeEnd,

    MarkupDeclarationOpen,
    BogusComment,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,

    Doctype,
    BeforeDoctypeName,
    DoctypeName,
    AfterDoctypeName,
    AfterDoctypePublicKeyword,
    BeforeDoctypePublicId,
    DoctypePublicIdDouble,
    DoctypePublicIdSingle,
    AfterDoctypePublicId,
    BetweenDoctypePublicAndSystemIds,
    AfterDoctypeSystemKeyword,
    BeforeDoctypeSystemId,
    DoctypeSystemIdDouble,
    DoctypeSystemIdSingle,
    AfterDoctypeSystemId,
    BogusDoctype,

    CdataSection,
    CdataSectionBracket,
    CdataSectionEnd,
}

/// The replacement character, which every U+0000 in the input becomes.
pub(super) const REPLACEMENT: char = '\u{fffd}';

/// A doctype under construction. Its ids are `Option` because "absent"
/// and "present but empty" are different: `<!DOCTYPE html PUBLIC "">`
/// has an empty public id, and quirks detection distinguishes them.
#[derive(Default)]
struct DoctypeBuilder {
    name: String,
    public_id: Option<String>,
    system_id: Option<String>,
    force_quirks: bool,
}

/// The tokenizer. Holds the input, the current state, the token being
/// built, and a queue for the states that emit more than one token.
pub struct Tokenizer {
    /// The input as characters. Indexed, not iterated, because the spec
    /// reconsumes the current character in dozens of states and an
    /// iterator cannot step back.
    input: Vec<char>,
    pos: usize,
    state: State,
    /// Tokens produced but not yet handed out.
    pending: VecDeque<Token>,
    /// Set once the input is exhausted and [`Token::Eof`] has been queued.
    finished: bool,

    /// The tag being built, and the attribute being built within it.
    tag: Tag,
    /// Whether `tag` is an end tag.
    tag_is_end: bool,
    attr_name: String,
    attr_value: String,
    /// Whether an attribute is under construction at all — `<div >` has
    /// none, and committing an empty one would invent an attribute.
    in_attr: bool,

    comment: String,
    doctype: DoctypeBuilder,

    /// The last start tag's name. An end tag in a raw-text state only
    /// closes it if it matches this, which is why `</div>` inside
    /// `<script>` is script text rather than a tag.
    last_start_tag: String,
    /// Characters consumed while trying to match an end tag or a script
    /// escape keyword. Flushed as text when the match fails.
    temp: String,
}

impl Tokenizer {
    /// Build a tokenizer over `input`.
    ///
    /// Newlines are normalised here: the spec folds CRLF and lone CR to
    /// LF before tokenizing, so no later state has to consider CR, and
    /// text nodes never carry a stray `\r` that would widen a line box.
    pub fn new(input: &str) -> Self {
        Self {
            input: normalise_newlines(input),
            pos: 0,
            state: State::Data,
            pending: VecDeque::new(),
            finished: false,
            tag: Tag::default(),
            tag_is_end: false,
            attr_name: String::new(),
            attr_value: String::new(),
            in_attr: false,
            comment: String::new(),
            doctype: DoctypeBuilder::default(),
            last_start_tag: String::new(),
            temp: String::new(),
        }
    }

    /// Push the tokenizer into a different state. The tree builder calls
    /// this on `<script>`, `<style>`, `<title>` and friends, whose
    /// contents are not markup.
    pub fn set_state(&mut self, state: State) {
        self.state = state;
    }

    /// The next token, running the machine until one is available.
    /// Returns [`Token::Eof`] forever once the input is spent.
    pub fn next_token(&mut self) -> Token {
        loop {
            if let Some(token) = self.pending.pop_front() {
                return token;
            }
            if self.finished {
                return Token::Eof;
            }
            self.step();
        }
    }

    /// Run one state.
    ///
    /// A dispatch table: one arm per state, deliberately flat so an arm
    /// can be read against the spec's state of the same name. Grouping it
    /// into sub-dispatchers would add a classification of states that is
    /// itself another table to keep in sync.
    fn step(&mut self) {
        match self.state {
            State::Data => self.data_state(),
            State::Plaintext => self.plaintext_state(),
            State::Rcdata => self.rcdata_state(),
            State::Rawtext => self.rawtext_state(),
            State::ScriptData => self.script_data_state(),

            State::TagOpen => self.tag_open_state(),
            State::EndTagOpen => self.end_tag_open_state(),
            State::TagName => self.tag_name_state(),
            State::BeforeAttributeName => self.before_attribute_name_state(),
            State::AttributeName => self.attribute_name_state(),
            State::AfterAttributeName => self.after_attribute_name_state(),
            State::BeforeAttributeValue => self.before_attribute_value_state(),
            State::AttributeValueDouble => self.attribute_value_quoted_state('"'),
            State::AttributeValueSingle => self.attribute_value_quoted_state('\''),
            State::AttributeValueUnquoted => self.attribute_value_unquoted_state(),
            State::AfterAttributeValueQuoted => self.after_attribute_value_quoted_state(),
            State::SelfClosingStartTag => self.self_closing_start_tag_state(),

            State::RcdataLessThan => self.raw_less_than_state(RawKind::Rcdata),
            State::RcdataEndTagOpen => self.raw_end_tag_open_state(RawKind::Rcdata),
            State::RcdataEndTagName => self.raw_end_tag_name_state(RawKind::Rcdata),
            State::RawtextLessThan => self.raw_less_than_state(RawKind::Rawtext),
            State::RawtextEndTagOpen => self.raw_end_tag_open_state(RawKind::Rawtext),
            State::RawtextEndTagName => self.raw_end_tag_name_state(RawKind::Rawtext),
            State::ScriptDataLessThan => self.script_data_less_than_state(),
            State::ScriptDataEndTagOpen => self.raw_end_tag_open_state(RawKind::ScriptData),
            State::ScriptDataEndTagName => self.raw_end_tag_name_state(RawKind::ScriptData),
            State::ScriptDataEscapeStart => self.script_data_escape_start_state(),
            State::ScriptDataEscapeStartDash => self.script_data_escape_start_dash_state(),
            State::ScriptDataEscaped => self.script_data_escaped_state(),
            State::ScriptDataEscapedDash => self.script_data_escaped_dash_state(),
            State::ScriptDataEscapedDashDash => self.script_data_escaped_dash_dash_state(),
            State::ScriptDataEscapedLessThan => self.script_data_escaped_less_than_state(),
            State::ScriptDataEscapedEndTagOpen => self.script_data_escaped_end_tag_open_state(),
            State::ScriptDataEscapedEndTagName => self.script_data_escaped_end_tag_name_state(),
            State::ScriptDataDoubleEscapeStart => self.script_data_double_escape_start_state(),
            State::ScriptDataDoubleEscaped => self.script_data_double_escaped_state(),
            State::ScriptDataDoubleEscapedDash => self.script_data_double_escaped_dash_state(),
            State::ScriptDataDoubleEscapedDashDash => {
                self.script_data_double_escaped_dash_dash_state()
            }
            State::ScriptDataDoubleEscapedLessThan => {
                self.script_data_double_escaped_less_than_state()
            }
            State::ScriptDataDoubleEscapeEnd => self.script_data_double_escape_end_state(),

            State::MarkupDeclarationOpen => self.markup_declaration_open_state(),
            State::BogusComment => self.bogus_comment_state(),
            State::CommentStart => self.comment_start_state(),
            State::CommentStartDash => self.comment_start_dash_state(),
            State::Comment => self.comment_state(),
            State::CommentEndDash => self.comment_end_dash_state(),
            State::CommentEnd => self.comment_end_state(),
            State::CommentEndBang => self.comment_end_bang_state(),

            State::Doctype => self.doctype_state(),
            State::BeforeDoctypeName => self.before_doctype_name_state(),
            State::DoctypeName => self.doctype_name_state(),
            State::AfterDoctypeName => self.after_doctype_name_state(),
            State::AfterDoctypePublicKeyword => self.after_doctype_public_keyword_state(),
            State::BeforeDoctypePublicId => self.before_doctype_public_id_state(),
            State::DoctypePublicIdDouble => self.doctype_public_id_state('"'),
            State::DoctypePublicIdSingle => self.doctype_public_id_state('\''),
            State::AfterDoctypePublicId => self.after_doctype_public_id_state(),
            State::BetweenDoctypePublicAndSystemIds => self.between_doctype_ids_state(),
            State::AfterDoctypeSystemKeyword => self.after_doctype_system_keyword_state(),
            State::BeforeDoctypeSystemId => self.before_doctype_system_id_state(),
            State::DoctypeSystemIdDouble => self.doctype_system_id_state('"'),
            State::DoctypeSystemIdSingle => self.doctype_system_id_state('\''),
            State::AfterDoctypeSystemId => self.after_doctype_system_id_state(),
            State::BogusDoctype => self.bogus_doctype_state(),

            State::CdataSection => self.cdata_section_state(),
            State::CdataSectionBracket => self.cdata_section_bracket_state(),
            State::CdataSectionEnd => self.cdata_section_end_state(),
        }
    }

    // ---- input ----------------------------------------------------------

    /// Consume and return the next character, or `None` at EOF.
    pub(super) fn consume(&mut self) -> Option<char> {
        let c = self.input.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// Step back over the character just consumed, so the next state sees
    /// it again. The spec's "reconsume in" instruction.
    pub(super) fn reconsume(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    /// The remaining input as a string, for the few places that match a
    /// literal — `<!--`, `DOCTYPE`, `[CDATA[`.
    fn rest(&self) -> String {
        self.input[self.pos.min(self.input.len())..]
            .iter()
            .collect()
    }

    /// If the remaining input starts with `literal`, consume it.
    /// `fold_case` for the keywords HTML matches case-insensitively.
    fn consume_literal(&mut self, literal: &str, fold_case: bool) -> bool {
        let rest = self.rest();
        let matched = if fold_case {
            rest.len() >= literal.len()
                && rest
                    .get(..literal.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(literal))
        } else {
            rest.starts_with(literal)
        };
        if matched {
            self.pos += literal.chars().count();
        }
        matched
    }

    // ---- emitting -------------------------------------------------------

    pub(super) fn emit(&mut self, token: Token) {
        self.pending.push_back(token);
    }

    pub(super) fn emit_char(&mut self, c: char) {
        self.pending.push_back(Token::Character(c));
    }

    pub(super) fn emit_str(&mut self, text: &str) {
        for c in text.chars() {
            self.pending.push_back(Token::Character(c));
        }
    }

    /// Queue [`Token::Eof`] and stop the machine.
    pub(super) fn emit_eof(&mut self) {
        self.finished = true;
        self.pending.push_back(Token::Eof);
    }

    // ---- character references -------------------------------------------

    /// Consume a character reference, the `&` already read, and return the
    /// text it stands for.
    ///
    /// `in_attribute` selects the spec's extra rule for attribute values:
    /// a reference with no trailing `;` followed by `=` or an
    /// alphanumeric is *not* decoded, so `?a=1&copy=2` keeps its `&copy`
    /// as text instead of turning a query string into `©=2`. Getting this
    /// wrong silently corrupts links, which is why it lives here rather
    /// than at the two call sites.
    fn consume_char_ref(&mut self, in_attribute: bool) -> String {
        match self.input.get(self.pos).copied() {
            Some('#') => self.consume_numeric_ref(),
            Some(c) if c.is_ascii_alphanumeric() => self.consume_named_ref(in_attribute),
            // A bare `&` is text: `a & b` and `&;` are not references.
            _ => "&".to_owned(),
        }
    }

    fn consume_named_ref(&mut self, in_attribute: bool) -> String {
        let rest = self.rest();
        let Some(found) = entities::match_named(&rest) else {
            // Unknown name: the `&` and the name stay as written.
            return "&".to_owned();
        };
        let terminated = rest[..found.consumed.min(rest.len())].ends_with(';')
            || rest.chars().nth(found.consumed.saturating_sub(1)) == Some(';');
        if in_attribute && !terminated {
            let next = rest.chars().nth(found.consumed);
            if next == Some('=') || next.is_some_and(|c| c.is_ascii_alphanumeric()) {
                return "&".to_owned();
            }
        }
        self.pos += found.consumed;
        found.text
    }

    /// `&#1234;` and `&#x4d2;`. A reference with no digits is text.
    fn consume_numeric_ref(&mut self) -> String {
        let start = self.pos;
        self.pos += 1; // the '#'
        let hex = matches!(self.input.get(self.pos).copied(), Some('x' | 'X'));
        if hex {
            self.pos += 1;
        }
        let radix = if hex { 16 } else { 10 };

        let digits_from = self.pos;
        while self
            .input
            .get(self.pos)
            .copied()
            .is_some_and(|c| c.is_digit(radix))
        {
            self.pos += 1;
        }
        if self.pos == digits_from {
            // `&#` with nothing usable after it.
            self.pos = start;
            return "&".to_owned();
        }

        let digits: String = self.input[digits_from..self.pos].iter().collect();
        // A missing semicolon is a parse error, not a reason to reject.
        if self.input.get(self.pos).copied() == Some(';') {
            self.pos += 1;
        }
        // Overlong input cannot be a valid scalar value; `numeric_replacement`
        // maps anything out of range to U+FFFD, so saturate rather than wrap.
        let code = u32::from_str_radix(&digits, radix).unwrap_or(u32::MAX);
        entities::numeric_replacement(code).to_string()
    }

    // ---- text states ----------------------------------------------------

    /// Ordinary document text.
    fn data_state(&mut self) {
        match self.consume() {
            Some('&') => {
                let text = self.consume_char_ref(false);
                self.emit_str(&text);
            }
            Some('<') => self.state = State::TagOpen,
            // U+0000 in text is a parse error but survives as U+FFFD.
            Some('\0') => self.emit_char(REPLACEMENT),
            Some(c) => self.emit_char(c),
            None => self.emit_eof(),
        }
    }

    /// `<plaintext>`: no markup at all, to EOF.
    fn plaintext_state(&mut self) {
        match self.consume() {
            Some('\0') => self.emit_char(REPLACEMENT),
            Some(c) => self.emit_char(c),
            None => self.emit_eof(),
        }
    }
}

/// Fold CRLF and lone CR to LF, per the spec's input preprocessing.
fn normalise_newlines(input: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every token from `input`, the trailing `Eof` dropped.
    fn tokens(input: &str) -> Vec<Token> {
        let mut t = Tokenizer::new(input);
        let mut out = Vec::new();
        loop {
            match t.next_token() {
                Token::Eof => return out,
                token => out.push(token),
            }
        }
    }

    /// Just the character tokens, as a string.
    fn text(input: &str) -> String {
        tokens(input)
            .into_iter()
            .filter_map(|t| match t {
                Token::Character(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    fn start(name: &str, attrs: &[(&str, &str)]) -> Token {
        Token::StartTag(Tag {
            name: name.to_owned(),
            attrs: attrs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            self_closing: false,
        })
    }

    fn end(name: &str) -> Token {
        Token::EndTag(Tag {
            name: name.to_owned(),
            ..Tag::default()
        })
    }

    #[test]
    fn plain_text_comes_through_unchanged() {
        assert_eq!(text("hello world"), "hello world");
    }

    #[test]
    fn an_empty_input_is_just_eof() {
        assert_eq!(tokens(""), []);
    }

    #[test]
    fn newlines_are_normalised_before_tokenizing() {
        assert_eq!(text("a\r\nb\rc\nd"), "a\nb\nc\nd");
    }

    #[test]
    fn nulls_in_text_become_the_replacement_character() {
        assert_eq!(text("a\0b"), "a\u{fffd}b");
    }

    #[test]
    fn a_simple_element_tokenizes_to_a_tag_pair() {
        assert_eq!(
            tokens("<p>hi</p>"),
            [
                start("p", &[]),
                Token::Character('h'),
                Token::Character('i'),
                end("p"),
            ]
        );
    }

    #[test]
    fn tag_and_attribute_names_are_lowercased() {
        assert_eq!(tokens("<DIV CLASS=x>"), [start("div", &[("class", "x")])]);
    }

    #[test]
    fn attribute_values_parse_in_all_three_quoting_styles() {
        assert_eq!(
            tokens("<a href=\"/x\" rel='me' n=7>"),
            [start("a", &[("href", "/x"), ("rel", "me"), ("n", "7")])]
        );
    }

    #[test]
    fn a_valueless_attribute_gets_an_empty_value() {
        assert_eq!(
            tokens("<input disabled required>"),
            [start("input", &[("disabled", ""), ("required", "")])]
        );
    }

    #[test]
    fn a_self_closing_tag_is_flagged() {
        let Token::StartTag(tag) = &tokens("<br/>")[0] else {
            panic!("expected a start tag");
        };
        assert!(tag.self_closing);
    }

    #[test]
    fn whitespace_around_attributes_and_the_solidus_is_tolerated() {
        assert_eq!(
            tokens("<img  src = \"a.png\"   / >"),
            [start("img", &[("src", "a.png")])]
        );
    }

    #[test]
    fn an_end_tag_with_trailing_space_still_closes() {
        assert_eq!(tokens("</p >"), [end("p")]);
    }

    #[test]
    fn a_lone_less_than_is_text_not_a_tag() {
        assert_eq!(text("a < b"), "a < b");
    }

    #[test]
    fn references_decode_in_text_and_in_attribute_values() {
        assert_eq!(text("a&amp;b &#65; &#x42;"), "a&b A B");
        assert_eq!(tokens("<a t=\"&amp;\">"), [start("a", &[("t", "&")])]);
    }

    #[test]
    fn a_query_string_ampersand_survives_in_an_attribute() {
        // `&copy=2` must not become `©=2` — the rule that keeps links intact.
        assert_eq!(
            tokens("<a href=\"?a=1&copy=2\">"),
            [start("a", &[("href", "?a=1&copy=2")])]
        );
    }

    #[test]
    fn the_same_reference_does_decode_in_text() {
        assert_eq!(text("&copy 2"), "\u{a9} 2");
    }

    #[test]
    fn a_bare_ampersand_stays_text() {
        assert_eq!(text("a & b"), "a & b");
        assert_eq!(text("&#"), "&#");
        assert_eq!(text("&;"), "&;");
    }

    #[test]
    fn comments_tokenize_including_the_malformed_short_forms() {
        assert_eq!(tokens("<!-- hi -->"), [Token::Comment(" hi ".into())]);
        assert_eq!(tokens("<!-->"), [Token::Comment(String::new())]);
        assert_eq!(tokens("<!--->"), [Token::Comment(String::new())]);
        assert_eq!(tokens("<!--a--!>"), [Token::Comment("a".into())]);
    }

    #[test]
    fn a_bogus_comment_captures_what_follows_the_bang() {
        assert_eq!(tokens("<!bogus>"), [Token::Comment("bogus".into())]);
        assert_eq!(tokens("<?php ?>"), [Token::Comment("?php ?".into())]);
    }

    #[test]
    fn a_modern_doctype_parses_without_forcing_quirks() {
        assert_eq!(
            tokens("<!DOCTYPE html>"),
            [Token::Doctype {
                name: "html".into(),
                public_id: None,
                system_id: None,
                force_quirks: false,
            }]
        );
    }

    #[test]
    fn a_legacy_doctype_keeps_its_public_and_system_ids() {
        assert_eq!(
            tokens(
                "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"http://www.w3.org/TR/html4/strict.dtd\">"
            ),
            [Token::Doctype {
                name: "html".into(),
                public_id: Some("-//W3C//DTD HTML 4.01//EN".into()),
                system_id: Some("http://www.w3.org/TR/html4/strict.dtd".into()),
                force_quirks: false,
            }]
        );
    }

    #[test]
    fn an_unterminated_doctype_forces_quirks() {
        let Token::Doctype { force_quirks, .. } = &tokens("<!DOCTYPE")[0] else {
            panic!("expected a doctype");
        };
        assert!(force_quirks);
    }

    #[test]
    fn rawtext_contents_are_never_markup() {
        let mut t = Tokenizer::new("<style>a < b & c</style>");
        assert_eq!(t.next_token(), start("style", &[]));
        t.set_state(State::Rawtext);
        let mut body = String::new();
        loop {
            match t.next_token() {
                Token::Character(c) => body.push(c),
                other => {
                    assert_eq!(other, end("style"));
                    break;
                }
            }
        }
        assert_eq!(body, "a < b & c");
    }

    #[test]
    fn rcdata_decodes_references_but_not_tags() {
        let mut t = Tokenizer::new("<title>a&amp;b <i></title>");
        assert_eq!(t.next_token(), start("title", &[]));
        t.set_state(State::Rcdata);
        let mut body = String::new();
        loop {
            match t.next_token() {
                Token::Character(c) => body.push(c),
                other => {
                    assert_eq!(other, end("title"));
                    break;
                }
            }
        }
        assert_eq!(body, "a&b <i>");
    }

    #[test]
    fn a_non_matching_end_tag_in_rawtext_stays_text() {
        let mut t = Tokenizer::new("<script></div>x</script>");
        assert_eq!(t.next_token(), start("script", &[]));
        t.set_state(State::ScriptData);
        let mut body = String::new();
        loop {
            match t.next_token() {
                Token::Character(c) => body.push(c),
                other => {
                    assert_eq!(other, end("script"));
                    break;
                }
            }
        }
        assert_eq!(body, "</div>x");
    }

    #[test]
    fn a_script_containing_a_comment_opener_still_closes_properly() {
        let mut t = Tokenizer::new("<script><!-- a </script>");
        assert_eq!(t.next_token(), start("script", &[]));
        t.set_state(State::ScriptData);
        let mut body = String::new();
        loop {
            match t.next_token() {
                Token::Character(c) => body.push(c),
                other => {
                    assert_eq!(other, end("script"));
                    break;
                }
            }
        }
        assert_eq!(body, "<!-- a ");
    }

    #[test]
    fn a_nested_script_string_inside_a_script_comment_does_not_end_it() {
        // The double-escape states: `</script>` inside this run is text,
        // and only the one after `</script>`-in-text closes the element.
        let mut t = Tokenizer::new("<script><!--<script>x</script>--></script>");
        assert_eq!(t.next_token(), start("script", &[]));
        t.set_state(State::ScriptData);
        let mut body = String::new();
        let mut ended = false;
        while !ended {
            match t.next_token() {
                Token::Character(c) => body.push(c),
                Token::EndTag(tag) => {
                    assert_eq!(tag.name, "script");
                    ended = true;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(body, "<!--<script>x</script>-->");
    }

    #[test]
    fn eof_inside_a_tag_drops_the_unfinished_tag() {
        // No tag token: an unterminated tag at EOF is discarded, not guessed.
        assert_eq!(tokens("<div"), []);
        assert_eq!(tokens("<div class=\"x"), []);
    }

    #[test]
    fn eof_is_returned_repeatedly_once_reached() {
        let mut t = Tokenizer::new("a");
        assert_eq!(t.next_token(), Token::Character('a'));
        assert_eq!(t.next_token(), Token::Eof);
        assert_eq!(t.next_token(), Token::Eof);
        assert_eq!(t.next_token(), Token::Eof);
    }

    #[test]
    fn a_cdata_section_outside_foreign_content_becomes_a_comment() {
        assert_eq!(
            tokens("<![CDATA[x]]>"),
            [Token::Comment("[CDATA[x]]".into())]
        );
    }

    #[test]
    fn duplicate_attributes_survive_tokenization_for_the_tree_builder() {
        assert_eq!(
            tokens("<p id=a id=b>"),
            [start("p", &[("id", "a"), ("id", "b")])]
        );
    }
}
