//! The tag states: everything between a `<` that opens a tag and the `>`
//! that closes it, attributes included.
//!
//! The awkward cases here are all real markup. `<a href=/x>` has an
//! unquoted value; `<input disabled>` has a value-less attribute;
//! `<img src="a" / >` has a stray solidus; `<p id=a id=b>` has a
//! duplicate. Each is a distinct state transition rather than a special
//! case bolted onto a scanner, which is why they compose — `<a href=/x
//! disabled>` needs no rule of its own.

use super::{State, Tag, Token, Tokenizer, REPLACEMENT};

/// The spec's definition of whitespace in markup. Notably *not* Unicode
/// whitespace and not `\r` (already folded to `\n`), so a no-break space
/// inside a tag is a character, not a separator.
pub(crate) fn is_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{c}' | ' ')
}

impl Tokenizer {
    // ---- the tag being built --------------------------------------------

    /// Begin a fresh tag, discarding anything half-built.
    fn start_tag(&mut self, is_end: bool) {
        self.tag = Tag::default();
        self.tag_is_end = is_end;
        self.attr_name.clear();
        self.attr_value.clear();
        self.in_attr = false;
    }

    /// Begin a new attribute, committing the previous one first.
    fn start_attr(&mut self) {
        self.commit_attr();
        self.attr_name.clear();
        self.attr_value.clear();
        self.in_attr = true;
    }

    /// Move the attribute under construction onto the tag. A no-op when
    /// there is none — `<div >` must not gain an empty attribute.
    fn commit_attr(&mut self) {
        if self.in_attr {
            let name = std::mem::take(&mut self.attr_name);
            let value = std::mem::take(&mut self.attr_value);
            self.tag.attrs.push((name, value));
            self.in_attr = false;
        }
    }

    /// Emit the finished tag and return to [`State::Data`].
    ///
    /// Start tags record their name, because the raw-text states match
    /// end tags against it: only `</script>` closes a `<script>`, and
    /// `</div>` inside one is script text.
    fn emit_current_tag(&mut self) {
        self.commit_attr();
        let tag = std::mem::take(&mut self.tag);
        let token = if self.tag_is_end {
            Token::EndTag(tag)
        } else {
            self.last_start_tag = tag.name.clone();
            Token::StartTag(tag)
        };
        self.emit(token);
        self.state = State::Data;
    }

    // ---- opening a tag ---------------------------------------------------

    pub(super) fn tag_open_state(&mut self) {
        match self.consume() {
            Some('!') => self.state = State::MarkupDeclarationOpen,
            Some('/') => self.state = State::EndTagOpen,
            Some(c) if c.is_ascii_alphabetic() => {
                self.start_tag(false);
                self.reconsume();
                self.state = State::TagName;
            }
            // `<?php` and friends: not markup, captured as a comment.
            Some('?') => {
                self.comment.clear();
                self.reconsume();
                self.state = State::BogusComment;
            }
            // `a < b`: the `<` was just text.
            Some(_) => {
                self.emit_char('<');
                self.reconsume();
                self.state = State::Data;
            }
            None => {
                self.emit_char('<');
                self.emit_eof();
            }
        }
    }

    pub(super) fn end_tag_open_state(&mut self) {
        match self.consume() {
            Some(c) if c.is_ascii_alphabetic() => {
                self.start_tag(true);
                self.reconsume();
                self.state = State::TagName;
            }
            // `</>` is discarded entirely.
            Some('>') => self.state = State::Data,
            Some(_) => {
                self.comment.clear();
                self.reconsume();
                self.state = State::BogusComment;
            }
            None => {
                self.emit_str("</");
                self.emit_eof();
            }
        }
    }

    pub(super) fn tag_name_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => self.state = State::BeforeAttributeName,
            Some('/') => self.state = State::SelfClosingStartTag,
            Some('>') => self.emit_current_tag(),
            Some('\0') => self.tag.name.push(REPLACEMENT),
            Some(c) => self.tag.name.push(c.to_ascii_lowercase()),
            // An unfinished tag at EOF is dropped, not guessed at.
            None => self.emit_eof(),
        }
    }

    // ---- attributes ------------------------------------------------------

    pub(super) fn before_attribute_name_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {}
            Some('/' | '>') | None => {
                self.reconsume();
                self.state = State::AfterAttributeName;
            }
            // `<div =x>`: a leading `=` becomes part of the name.
            Some('=') => {
                self.start_attr();
                self.attr_name.push('=');
                self.state = State::AttributeName;
            }
            Some(_) => {
                self.start_attr();
                self.reconsume();
                self.state = State::AttributeName;
            }
        }
    }

    pub(super) fn attribute_name_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {
                self.reconsume();
                self.state = State::AfterAttributeName;
            }
            Some('/' | '>') | None => {
                self.reconsume();
                self.state = State::AfterAttributeName;
            }
            Some('=') => self.state = State::BeforeAttributeValue,
            Some('\0') => self.attr_name.push(REPLACEMENT),
            Some(c) => self.attr_name.push(c.to_ascii_lowercase()),
        }
    }

    pub(super) fn after_attribute_name_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {}
            Some('/') => self.state = State::SelfClosingStartTag,
            Some('=') => self.state = State::BeforeAttributeValue,
            Some('>') => self.emit_current_tag(),
            Some(_) => {
                self.start_attr();
                self.reconsume();
                self.state = State::AttributeName;
            }
            None => self.emit_eof(),
        }
    }

    pub(super) fn before_attribute_value_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {}
            Some('"') => self.state = State::AttributeValueDouble,
            Some('\'') => self.state = State::AttributeValueSingle,
            // `<a href=>`: the attribute keeps its empty value.
            Some('>') => self.emit_current_tag(),
            _ => {
                self.reconsume();
                self.state = State::AttributeValueUnquoted;
            }
        }
    }

    /// Both quoted value states; `quote` is the one that ends it, so the
    /// other kind is an ordinary character inside the value.
    pub(super) fn attribute_value_quoted_state(&mut self, quote: char) {
        match self.consume() {
            Some(c) if c == quote => self.state = State::AfterAttributeValueQuoted,
            Some('&') => {
                let text = self.consume_char_ref(true);
                self.attr_value.push_str(&text);
            }
            Some('\0') => self.attr_value.push(REPLACEMENT),
            Some(c) => self.attr_value.push(c),
            None => self.emit_eof(),
        }
    }

    pub(super) fn attribute_value_unquoted_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => self.state = State::BeforeAttributeName,
            Some('&') => {
                let text = self.consume_char_ref(true);
                self.attr_value.push_str(&text);
            }
            Some('>') => self.emit_current_tag(),
            Some('\0') => self.attr_value.push(REPLACEMENT),
            // `"`, `'`, `<`, `=` and `` ` `` here are parse errors that
            // still belong to the value.
            Some(c) => self.attr_value.push(c),
            None => self.emit_eof(),
        }
    }

    pub(super) fn after_attribute_value_quoted_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => self.state = State::BeforeAttributeName,
            Some('/') => self.state = State::SelfClosingStartTag,
            Some('>') => self.emit_current_tag(),
            // `<a href="x"y>`: missing separator, recovered by starting
            // the next attribute anyway.
            Some(_) => {
                self.reconsume();
                self.state = State::BeforeAttributeName;
            }
            None => self.emit_eof(),
        }
    }

    pub(super) fn self_closing_start_tag_state(&mut self) {
        match self.consume() {
            Some('>') => {
                self.tag.self_closing = true;
                self.emit_current_tag();
            }
            // `<img / >`: a solidus that closes nothing.
            Some(_) => {
                self.reconsume();
                self.state = State::BeforeAttributeName;
            }
            None => self.emit_eof(),
        }
    }
}
