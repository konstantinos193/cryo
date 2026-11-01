//! The raw-text states: element contents that are text rather than
//! markup, and the end tag that escapes back out.
//!
//! Three flavours, differing in exactly two ways — whether character
//! references decode, and whether the script comment dance applies:
//!
//! | state      | elements                     | `&amp;` | `<!--` dance |
//! |------------|------------------------------|---------|--------------|
//! | `Rcdata`   | `title`, `textarea`          | decodes | no           |
//! | `Rawtext`  | `style`, `xmp`, `iframe`     | literal | no           |
//! | `ScriptData` | `script`                   | literal | yes          |
//!
//! All three leave only on *their own* end tag, matched against the last
//! start tag's name. That is why `</div>` inside a `<script>` is script
//! text: the tokenizer is not looking for any end tag, only for
//! `</script>`.
//!
//! The script escape states exist because `<script><!-- … --></script>`
//! was once how scripts hid from ancient browsers, and the web kept the
//! behaviour. Inside such a comment a `<script>` string pushes the
//! tokenizer one level deeper still — the "double escaped" states — so
//! that an ad tag writing `document.write("<script>…</script>")` does not
//! truncate the element at the wrong place and swallow the rest of the
//! page.

use super::tag::is_whitespace;
use super::{State, Tag, Token, Tokenizer, REPLACEMENT};

/// Which raw-text flavour a shared state is running as.
///
/// The three flavours share their `<`, end-tag-open and end-tag-name
/// states verbatim, differing only in which state they fall back to. A
/// enum rather than passing the text state itself, so there is no way to
/// call a raw-text helper with a state that has no raw-text companions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RawKind {
    Rcdata,
    Rawtext,
    ScriptData,
}

impl RawKind {
    /// The state that reads this flavour's text.
    const fn text(self) -> State {
        match self {
            Self::Rcdata => State::Rcdata,
            Self::Rawtext => State::Rawtext,
            Self::ScriptData => State::ScriptData,
        }
    }

    /// The state entered after a `/` following a `<`.
    const fn end_tag_open(self) -> State {
        match self {
            Self::Rcdata => State::RcdataEndTagOpen,
            Self::Rawtext => State::RawtextEndTagOpen,
            Self::ScriptData => State::ScriptDataEndTagOpen,
        }
    }

    /// The state that accumulates the candidate end tag's name.
    const fn end_tag_name(self) -> State {
        match self {
            Self::Rcdata => State::RcdataEndTagName,
            Self::Rawtext => State::RawtextEndTagName,
            Self::ScriptData => State::ScriptDataEndTagName,
        }
    }
}

impl Tokenizer {
    /// Whether the end tag being built closes the element that opened
    /// this raw-text run. Everything else is text.
    fn is_appropriate_end_tag(&self) -> bool {
        self.tag_is_end && self.tag.name == self.last_start_tag
    }

    /// Abandon a candidate end tag: emit the `</` and the characters
    /// consumed while matching, then go back to reading text.
    fn flush_end_tag_attempt(&mut self, kind: RawKind) {
        self.emit_str("</");
        let temp = std::mem::take(&mut self.temp);
        self.emit_str(&temp);
        self.reconsume();
        self.state = kind.text();
    }

    // ---- the three text states -------------------------------------------

    pub(super) fn rcdata_state(&mut self) {
        match self.consume() {
            Some('&') => {
                let text = self.consume_char_ref(false);
                self.emit_str(&text);
            }
            Some('<') => self.state = State::RcdataLessThan,
            Some('\0') => self.emit_char(REPLACEMENT),
            Some(c) => self.emit_char(c),
            None => self.emit_eof(),
        }
    }

    pub(super) fn rawtext_state(&mut self) {
        match self.consume() {
            Some('<') => self.state = State::RawtextLessThan,
            Some('\0') => self.emit_char(REPLACEMENT),
            Some(c) => self.emit_char(c),
            None => self.emit_eof(),
        }
    }

    pub(super) fn script_data_state(&mut self) {
        match self.consume() {
            Some('<') => self.state = State::ScriptDataLessThan,
            Some('\0') => self.emit_char(REPLACEMENT),
            Some(c) => self.emit_char(c),
            None => self.emit_eof(),
        }
    }

    // ---- escaping out through an end tag ---------------------------------

    pub(super) fn raw_less_than_state(&mut self, kind: RawKind) {
        match self.consume() {
            Some('/') => {
                self.temp.clear();
                self.state = kind.end_tag_open();
            }
            _ => {
                self.emit_char('<');
                self.reconsume();
                self.state = kind.text();
            }
        }
    }

    pub(super) fn raw_end_tag_open_state(&mut self, kind: RawKind) {
        match self.consume() {
            Some(c) if c.is_ascii_alphabetic() => {
                self.tag = Tag::default();
                self.tag_is_end = true;
                self.in_attr = false;
                self.reconsume();
                self.state = kind.end_tag_name();
            }
            _ => {
                self.emit_str("</");
                self.reconsume();
                self.state = kind.text();
            }
        }
    }

    /// Accumulate a candidate end tag name. It only becomes a tag if it
    /// matches the element that opened this run; otherwise every
    /// character consumed so far is emitted as text.
    pub(super) fn raw_end_tag_name_state(&mut self, kind: RawKind) {
        match self.consume() {
            Some(c) if is_whitespace(c) && self.is_appropriate_end_tag() => {
                self.state = State::BeforeAttributeName;
            }
            Some('/') if self.is_appropriate_end_tag() => {
                self.state = State::SelfClosingStartTag;
            }
            Some('>') if self.is_appropriate_end_tag() => self.emit_end_tag(),
            Some(c) if c.is_ascii_alphabetic() => {
                self.tag.name.push(c.to_ascii_lowercase());
                self.temp.push(c);
            }
            _ => self.flush_end_tag_attempt(kind),
        }
    }

    /// Emit the matched end tag and return to [`State::Data`]. Raw-text
    /// end tags never carry attributes, so there is nothing to commit.
    fn emit_end_tag(&mut self) {
        let tag = std::mem::take(&mut self.tag);
        self.emit(Token::EndTag(tag));
        self.state = State::Data;
    }

    // ---- script comment escaping -----------------------------------------

    pub(super) fn script_data_less_than_state(&mut self) {
        match self.consume() {
            Some('/') => {
                self.temp.clear();
                self.state = State::ScriptDataEndTagOpen;
            }
            Some('!') => {
                self.emit_str("<!");
                self.state = State::ScriptDataEscapeStart;
            }
            _ => {
                self.emit_char('<');
                self.reconsume();
                self.state = State::ScriptData;
            }
        }
    }

    pub(super) fn script_data_escape_start_state(&mut self) {
        match self.consume() {
            Some('-') => {
                self.emit_char('-');
                self.state = State::ScriptDataEscapeStartDash;
            }
            _ => {
                self.reconsume();
                self.state = State::ScriptData;
            }
        }
    }

    pub(super) fn script_data_escape_start_dash_state(&mut self) {
        match self.consume() {
            Some('-') => {
                self.emit_char('-');
                self.state = State::ScriptDataEscapedDashDash;
            }
            _ => {
                self.reconsume();
                self.state = State::ScriptData;
            }
        }
    }

    pub(super) fn script_data_escaped_state(&mut self) {
        match self.consume() {
            Some('-') => {
                self.emit_char('-');
                self.state = State::ScriptDataEscapedDash;
            }
            Some('<') => self.state = State::ScriptDataEscapedLessThan,
            Some('\0') => self.emit_char(REPLACEMENT),
            Some(c) => self.emit_char(c),
            None => self.emit_eof(),
        }
    }

    pub(super) fn script_data_escaped_dash_state(&mut self) {
        match self.consume() {
            Some('-') => {
                self.emit_char('-');
                self.state = State::ScriptDataEscapedDashDash;
            }
            Some('<') => self.state = State::ScriptDataEscapedLessThan,
            Some('\0') => {
                self.emit_char(REPLACEMENT);
                self.state = State::ScriptDataEscaped;
            }
            Some(c) => {
                self.emit_char(c);
                self.state = State::ScriptDataEscaped;
            }
            None => self.emit_eof(),
        }
    }

    pub(super) fn script_data_escaped_dash_dash_state(&mut self) {
        match self.consume() {
            Some('-') => self.emit_char('-'),
            Some('<') => self.state = State::ScriptDataEscapedLessThan,
            // `-->` closes the comment and the escape with it.
            Some('>') => {
                self.emit_char('>');
                self.state = State::ScriptData;
            }
            Some('\0') => {
                self.emit_char(REPLACEMENT);
                self.state = State::ScriptDataEscaped;
            }
            Some(c) => {
                self.emit_char(c);
                self.state = State::ScriptDataEscaped;
            }
            None => self.emit_eof(),
        }
    }

    pub(super) fn script_data_escaped_less_than_state(&mut self) {
        match self.consume() {
            Some('/') => {
                self.temp.clear();
                self.state = State::ScriptDataEscapedEndTagOpen;
            }
            // `<script` inside the comment: one level deeper.
            Some(c) if c.is_ascii_alphabetic() => {
                self.temp.clear();
                self.emit_char('<');
                self.reconsume();
                self.state = State::ScriptDataDoubleEscapeStart;
            }
            _ => {
                self.emit_char('<');
                self.reconsume();
                self.state = State::ScriptDataEscaped;
            }
        }
    }

    pub(super) fn script_data_escaped_end_tag_open_state(&mut self) {
        match self.consume() {
            Some(c) if c.is_ascii_alphabetic() => {
                self.tag = Tag::default();
                self.tag_is_end = true;
                self.in_attr = false;
                self.reconsume();
                self.state = State::ScriptDataEscapedEndTagName;
            }
            _ => {
                self.emit_str("</");
                self.reconsume();
                self.state = State::ScriptDataEscaped;
            }
        }
    }

    pub(super) fn script_data_escaped_end_tag_name_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) && self.is_appropriate_end_tag() => {
                self.state = State::BeforeAttributeName;
            }
            Some('/') if self.is_appropriate_end_tag() => {
                self.state = State::SelfClosingStartTag;
            }
            Some('>') if self.is_appropriate_end_tag() => self.emit_end_tag(),
            Some(c) if c.is_ascii_alphabetic() => {
                self.tag.name.push(c.to_ascii_lowercase());
                self.temp.push(c);
            }
            _ => {
                self.emit_str("</");
                let temp = std::mem::take(&mut self.temp);
                self.emit_str(&temp);
                self.reconsume();
                self.state = State::ScriptDataEscaped;
            }
        }
    }

    /// Decide whether the word just read was `script`, which is what
    /// pushes the tokenizer into (or out of) double escaping.
    pub(super) fn script_data_double_escape_start_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) || c == '/' || c == '>' => {
                self.state = if self.temp == "script" {
                    State::ScriptDataDoubleEscaped
                } else {
                    State::ScriptDataEscaped
                };
                self.emit_char(c);
            }
            Some(c) if c.is_ascii_alphabetic() => {
                self.temp.push(c.to_ascii_lowercase());
                self.emit_char(c);
            }
            _ => {
                self.reconsume();
                self.state = State::ScriptDataEscaped;
            }
        }
    }

    pub(super) fn script_data_double_escaped_state(&mut self) {
        match self.consume() {
            Some('-') => {
                self.emit_char('-');
                self.state = State::ScriptDataDoubleEscapedDash;
            }
            Some('<') => {
                self.emit_char('<');
                self.state = State::ScriptDataDoubleEscapedLessThan;
            }
            Some('\0') => self.emit_char(REPLACEMENT),
            Some(c) => self.emit_char(c),
            None => self.emit_eof(),
        }
    }

    pub(super) fn script_data_double_escaped_dash_state(&mut self) {
        match self.consume() {
            Some('-') => {
                self.emit_char('-');
                self.state = State::ScriptDataDoubleEscapedDashDash;
            }
            Some('<') => {
                self.emit_char('<');
                self.state = State::ScriptDataDoubleEscapedLessThan;
            }
            Some('\0') => {
                self.emit_char(REPLACEMENT);
                self.state = State::ScriptDataDoubleEscaped;
            }
            Some(c) => {
                self.emit_char(c);
                self.state = State::ScriptDataDoubleEscaped;
            }
            None => self.emit_eof(),
        }
    }

    pub(super) fn script_data_double_escaped_dash_dash_state(&mut self) {
        match self.consume() {
            Some('-') => self.emit_char('-'),
            Some('<') => {
                self.emit_char('<');
                self.state = State::ScriptDataDoubleEscapedLessThan;
            }
            Some('>') => {
                self.emit_char('>');
                self.state = State::ScriptData;
            }
            Some('\0') => {
                self.emit_char(REPLACEMENT);
                self.state = State::ScriptDataDoubleEscaped;
            }
            Some(c) => {
                self.emit_char(c);
                self.state = State::ScriptDataDoubleEscaped;
            }
            None => self.emit_eof(),
        }
    }

    pub(super) fn script_data_double_escaped_less_than_state(&mut self) {
        match self.consume() {
            Some('/') => {
                self.temp.clear();
                self.emit_char('/');
                self.state = State::ScriptDataDoubleEscapeEnd;
            }
            _ => {
                self.reconsume();
                self.state = State::ScriptDataDoubleEscaped;
            }
        }
    }

    pub(super) fn script_data_double_escape_end_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) || c == '/' || c == '>' => {
                self.state = if self.temp == "script" {
                    State::ScriptDataEscaped
                } else {
                    State::ScriptDataDoubleEscaped
                };
                self.emit_char(c);
            }
            Some(c) if c.is_ascii_alphabetic() => {
                self.temp.push(c.to_ascii_lowercase());
                self.emit_char(c);
            }
            _ => {
                self.reconsume();
                self.state = State::ScriptDataDoubleEscaped;
            }
        }
    }
}
