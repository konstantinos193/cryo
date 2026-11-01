//! Markup declarations: comments, doctypes and CDATA sections —
//! everything that starts `<!`.
//!
//! None of it renders. Comments are kept because scripts read them and
//! conditional-comment markup still exists in the wild; the doctype is
//! kept for one reason only, which is that its absence or its legacy
//! strings switch layout into quirks mode.
//!
//! The comment states are mostly about ending early. `<!-->` and
//! `<!--->` are both empty comments, `<!--a--!>` closes on the bang, and
//! an unterminated comment at EOF is emitted rather than dropped — every
//! one of those is a shape real pages contain, and a scanner that just
//! looked for `-->` would swallow the rest of the document on each.

use super::tag::is_whitespace;
use super::{DoctypeBuilder, State, Token, Tokenizer, REPLACEMENT};

impl Tokenizer {
    /// Emit the doctype under construction. `force_quirks` ORs into
    /// whatever the states already decided, since several ways of being
    /// malformed arrive here independently.
    fn emit_doctype(&mut self, force_quirks: bool) {
        let built = std::mem::take(&mut self.doctype);
        self.emit(Token::Doctype {
            name: built.name,
            public_id: built.public_id,
            system_id: built.system_id,
            force_quirks: built.force_quirks || force_quirks,
        });
    }

    fn emit_comment(&mut self) {
        let data = std::mem::take(&mut self.comment);
        self.emit(Token::Comment(data));
    }

    /// Decide what a `<!` opens.
    pub(super) fn markup_declaration_open_state(&mut self) {
        if self.consume_literal("--", false) {
            self.comment.clear();
            self.state = State::CommentStart;
        } else if self.consume_literal("DOCTYPE", true) {
            self.doctype = DoctypeBuilder::default();
            self.state = State::Doctype;
        } else if self.consume_literal("[CDATA[", false) {
            // Only meaningful inside SVG or MathML. In HTML content it is
            // a parse error that survives as a comment, text and all.
            self.comment = "[CDATA[".to_owned();
            self.state = State::BogusComment;
        } else {
            self.comment.clear();
            self.state = State::BogusComment;
        }
    }

    /// `<!bogus>`, `<?php ?>`, `</ >`: captured verbatim up to the `>`.
    pub(super) fn bogus_comment_state(&mut self) {
        match self.consume() {
            Some('>') => {
                self.emit_comment();
                self.state = State::Data;
            }
            Some('\0') => self.comment.push(REPLACEMENT),
            Some(c) => self.comment.push(c),
            None => {
                self.emit_comment();
                self.emit_eof();
            }
        }
    }

    // ---- comments --------------------------------------------------------

    pub(super) fn comment_start_state(&mut self) {
        match self.consume() {
            Some('-') => self.state = State::CommentStartDash,
            // `<!-->`: an empty comment, not an unterminated one.
            Some('>') => {
                self.emit_comment();
                self.state = State::Data;
            }
            _ => {
                self.reconsume();
                self.state = State::Comment;
            }
        }
    }

    pub(super) fn comment_start_dash_state(&mut self) {
        match self.consume() {
            Some('-') => self.state = State::CommentEnd,
            // `<!--->`: also empty.
            Some('>') => {
                self.emit_comment();
                self.state = State::Data;
            }
            None => {
                self.emit_comment();
                self.emit_eof();
            }
            Some(_) => {
                self.comment.push('-');
                self.reconsume();
                self.state = State::Comment;
            }
        }
    }

    pub(super) fn comment_state(&mut self) {
        match self.consume() {
            Some('-') => self.state = State::CommentEndDash,
            Some('\0') => self.comment.push(REPLACEMENT),
            // A nested `<!--` is ordinary comment data; the spec's
            // comment-less-than states only track it to report a parse
            // error, and produce the same data we do by appending.
            Some(c) => self.comment.push(c),
            None => {
                self.emit_comment();
                self.emit_eof();
            }
        }
    }

    pub(super) fn comment_end_dash_state(&mut self) {
        match self.consume() {
            Some('-') => self.state = State::CommentEnd,
            None => {
                self.emit_comment();
                self.emit_eof();
            }
            Some(_) => {
                self.comment.push('-');
                self.reconsume();
                self.state = State::Comment;
            }
        }
    }

    pub(super) fn comment_end_state(&mut self) {
        match self.consume() {
            Some('>') => {
                self.emit_comment();
                self.state = State::Data;
            }
            Some('!') => self.state = State::CommentEndBang,
            // `--->`: the extra dashes are data.
            Some('-') => self.comment.push('-'),
            None => {
                self.emit_comment();
                self.emit_eof();
            }
            Some(_) => {
                self.comment.push_str("--");
                self.reconsume();
                self.state = State::Comment;
            }
        }
    }

    pub(super) fn comment_end_bang_state(&mut self) {
        match self.consume() {
            Some('-') => {
                self.comment.push_str("--!");
                self.state = State::CommentEndDash;
            }
            // `<!--a--!>`: the bang closes it.
            Some('>') => {
                self.emit_comment();
                self.state = State::Data;
            }
            None => {
                self.emit_comment();
                self.emit_eof();
            }
            Some(_) => {
                self.comment.push_str("--!");
                self.reconsume();
                self.state = State::Comment;
            }
        }
    }

    // ---- doctype ---------------------------------------------------------

    pub(super) fn doctype_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => self.state = State::BeforeDoctypeName,
            None => {
                self.emit_doctype(true);
                self.emit_eof();
            }
            // `<!DOCTYPEhtml>`: missing space, recovered.
            Some(_) => {
                self.reconsume();
                self.state = State::BeforeDoctypeName;
            }
        }
    }

    pub(super) fn before_doctype_name_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {}
            Some('\0') => {
                self.doctype.name.push(REPLACEMENT);
                self.state = State::DoctypeName;
            }
            // `<!DOCTYPE>`: no name at all means quirks.
            Some('>') => {
                self.emit_doctype(true);
                self.state = State::Data;
            }
            None => {
                self.emit_doctype(true);
                self.emit_eof();
            }
            Some(c) => {
                self.doctype.name.push(c.to_ascii_lowercase());
                self.state = State::DoctypeName;
            }
        }
    }

    pub(super) fn doctype_name_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => self.state = State::AfterDoctypeName,
            Some('>') => {
                self.emit_doctype(false);
                self.state = State::Data;
            }
            Some('\0') => self.doctype.name.push(REPLACEMENT),
            Some(c) => self.doctype.name.push(c.to_ascii_lowercase()),
            None => {
                self.emit_doctype(true);
                self.emit_eof();
            }
        }
    }

    /// After the name, only `PUBLIC` or `SYSTEM` may follow. Anything
    /// else is a doctype we cannot read, which forces quirks.
    pub(super) fn after_doctype_name_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {}
            Some('>') => {
                self.emit_doctype(false);
                self.state = State::Data;
            }
            None => {
                self.emit_doctype(true);
                self.emit_eof();
            }
            Some(_) => {
                self.reconsume();
                if self.consume_literal("PUBLIC", true) {
                    self.state = State::AfterDoctypePublicKeyword;
                } else if self.consume_literal("SYSTEM", true) {
                    self.state = State::AfterDoctypeSystemKeyword;
                } else {
                    self.doctype.force_quirks = true;
                    self.state = State::BogusDoctype;
                }
            }
        }
    }

    pub(super) fn after_doctype_public_keyword_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => self.state = State::BeforeDoctypePublicId,
            Some('"') => self.open_public_id(State::DoctypePublicIdDouble),
            Some('\'') => self.open_public_id(State::DoctypePublicIdSingle),
            _ => self.bail_doctype(),
        }
    }

    pub(super) fn before_doctype_public_id_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {}
            Some('"') => self.open_public_id(State::DoctypePublicIdDouble),
            Some('\'') => self.open_public_id(State::DoctypePublicIdSingle),
            _ => self.bail_doctype(),
        }
    }

    /// Both quoted public-id states. An id is `Some("")` the moment its
    /// quote opens: "absent" and "present but empty" are different
    /// answers to quirks detection.
    pub(super) fn doctype_public_id_state(&mut self, quote: char) {
        match self.consume() {
            Some(c) if c == quote => self.state = State::AfterDoctypePublicId,
            Some('\0') => self.push_public_id(REPLACEMENT),
            // An unterminated id: `>` ends the doctype, badly.
            Some('>') => {
                self.emit_doctype(true);
                self.state = State::Data;
            }
            Some(c) => self.push_public_id(c),
            None => {
                self.emit_doctype(true);
                self.emit_eof();
            }
        }
    }

    pub(super) fn after_doctype_public_id_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => self.state = State::BetweenDoctypePublicAndSystemIds,
            Some('>') => {
                self.emit_doctype(false);
                self.state = State::Data;
            }
            Some('"') => self.open_system_id(State::DoctypeSystemIdDouble),
            Some('\'') => self.open_system_id(State::DoctypeSystemIdSingle),
            _ => self.bail_doctype(),
        }
    }

    pub(super) fn between_doctype_ids_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {}
            Some('>') => {
                self.emit_doctype(false);
                self.state = State::Data;
            }
            Some('"') => self.open_system_id(State::DoctypeSystemIdDouble),
            Some('\'') => self.open_system_id(State::DoctypeSystemIdSingle),
            _ => self.bail_doctype(),
        }
    }

    pub(super) fn after_doctype_system_keyword_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => self.state = State::BeforeDoctypeSystemId,
            Some('"') => self.open_system_id(State::DoctypeSystemIdDouble),
            Some('\'') => self.open_system_id(State::DoctypeSystemIdSingle),
            _ => self.bail_doctype(),
        }
    }

    pub(super) fn before_doctype_system_id_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {}
            Some('"') => self.open_system_id(State::DoctypeSystemIdDouble),
            Some('\'') => self.open_system_id(State::DoctypeSystemIdSingle),
            _ => self.bail_doctype(),
        }
    }

    pub(super) fn doctype_system_id_state(&mut self, quote: char) {
        match self.consume() {
            Some(c) if c == quote => self.state = State::AfterDoctypeSystemId,
            Some('\0') => self.push_system_id(REPLACEMENT),
            Some('>') => {
                self.emit_doctype(true);
                self.state = State::Data;
            }
            Some(c) => self.push_system_id(c),
            None => {
                self.emit_doctype(true);
                self.emit_eof();
            }
        }
    }

    pub(super) fn after_doctype_system_id_state(&mut self) {
        match self.consume() {
            Some(c) if is_whitespace(c) => {}
            Some('>') => {
                self.emit_doctype(false);
                self.state = State::Data;
            }
            None => {
                self.emit_doctype(true);
                self.emit_eof();
            }
            // Junk after a complete system id is ignored, and does *not*
            // force quirks — the doctype itself was readable.
            Some(_) => {
                self.reconsume();
                self.state = State::BogusDoctype;
            }
        }
    }

    pub(super) fn bogus_doctype_state(&mut self) {
        match self.consume() {
            Some('>') => {
                self.emit_doctype(false);
                self.state = State::Data;
            }
            Some(_) => {}
            None => {
                self.emit_doctype(false);
                self.emit_eof();
            }
        }
    }

    /// The shared "this doctype is unreadable" exit, reached from every
    /// state expecting a quoted id. Handles the `>` and EOF cases that
    /// end the token and the junk case that skips to the `>`.
    fn bail_doctype(&mut self) {
        self.doctype.force_quirks = true;
        match self.input.get(self.pos.saturating_sub(1)).copied() {
            Some('>') => {
                self.emit_doctype(true);
                self.state = State::Data;
            }
            None => {
                self.emit_doctype(true);
                self.emit_eof();
            }
            Some(_) => {
                self.reconsume();
                self.state = State::BogusDoctype;
            }
        }
    }

    fn open_public_id(&mut self, next: State) {
        self.doctype.public_id = Some(String::new());
        self.state = next;
    }

    fn open_system_id(&mut self, next: State) {
        self.doctype.system_id = Some(String::new());
        self.state = next;
    }

    fn push_public_id(&mut self, c: char) {
        self.doctype
            .public_id
            .get_or_insert_with(String::new)
            .push(c);
    }

    fn push_system_id(&mut self, c: char) {
        self.doctype
            .system_id
            .get_or_insert_with(String::new)
            .push(c);
    }

    // ---- CDATA -----------------------------------------------------------

    /// Reached only from foreign content, where the tree builder sets
    /// this state itself; in HTML content `<![CDATA[` is a comment.
    pub(super) fn cdata_section_state(&mut self) {
        match self.consume() {
            Some(']') => self.state = State::CdataSectionBracket,
            Some(c) => self.emit_char(c),
            None => self.emit_eof(),
        }
    }

    pub(super) fn cdata_section_bracket_state(&mut self) {
        match self.consume() {
            Some(']') => self.state = State::CdataSectionEnd,
            _ => {
                self.emit_char(']');
                self.reconsume();
                self.state = State::CdataSection;
            }
        }
    }

    pub(super) fn cdata_section_end_state(&mut self) {
        match self.consume() {
            Some(']') => self.emit_char(']'),
            Some('>') => self.state = State::Data,
            _ => {
                self.emit_str("]]");
                self.reconsume();
                self.state = State::CdataSection;
            }
        }
    }
}
