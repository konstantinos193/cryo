//! The CSS tokenizer: stylesheet text becomes a flat token stream.
//!
//! Simpler than HTML's — no feedback from the parser, no element-specific
//! states — but with its own set of things that only look simple:
//!
//! - **Escapes everywhere.** `\65 m` is `em`, and a class may be written
//!   `.\31 23` to start with a digit. Handled once, in
//!   [`Tokenizer::consume_escape`], so no later stage ever sees one.
//! - **Numbers carry their type.** `1` and `1.0` are both the number one,
//!   but `line-height: 1` inherits differently from `line-height: 1.0px`,
//!   and `nth-child` only accepts integers — so the integer flag travels
//!   with the value rather than being re-derived by `fract() == 0.0`.
//! - **`url()` is not a function.** `url(foo.png)` takes an unquoted
//!   argument that no other function does, so it is lexed as one token.
//! - **Bad tokens are tokens.** An unterminated string produces
//!   [`CssToken::BadString`] rather than an error, because the parser's
//!   job is to discard the declaration containing it and keep going.
//!
//! What this stage deliberately does *not* do is interpret anything: it
//! does not know that `red` is a colour or that `px` is a length. It only
//! says what shape each piece of text has.

/// A CSS token.
///
/// `Delim` is the catch-all for a single character with no meaning of its
/// own yet — `>`, `+`, `~`, `*`, `.` — which the selector parser gives
/// meaning to by position. Keeping them as delimiters here is what lets
/// one tokenizer serve selectors, declarations and at-rules.
#[derive(Clone, Debug, PartialEq)]
pub enum CssToken {
    Ident(String),
    /// A name immediately followed by `(`, which has been consumed.
    Function(String),
    /// `@media`, `@import` — the name without the `@`.
    AtKeyword(String),
    /// `#fff` or `#main`. `id` marks the ones that could name an element:
    /// `#fff` could not, so a selector may reject it while a colour
    /// accepts it.
    Hash {
        value: String,
        id: bool,
    },
    Str(String),
    /// An unterminated string. The parser drops whatever contains it.
    BadString,
    Url(String),
    BadUrl,
    Delim(char),
    Number {
        value: f64,
        integer: bool,
    },
    Percentage(f64),
    Dimension {
        value: f64,
        unit: String,
    },
    /// One run of whitespace, however long. Length never matters in CSS,
    /// but presence does — it is the descendant combinator.
    Whitespace,
    /// `<!--` and `-->`, legal at the top level so that a stylesheet
    /// could once be inlined in HTML without breaking old parsers.
    Cdo,
    Cdc,
    Colon,
    Semicolon,
    Comma,
    LeftSquare,
    RightSquare,
    LeftParen,
    RightParen,
    LeftCurly,
    RightCurly,
}

/// The replacement character, which every U+0000 becomes.
const REPLACEMENT: char = '\u{fffd}';

/// The most hex digits an escape may carry: `\10FFFF` is the largest
/// code point there is.
const MAX_ESCAPE_DIGITS: usize = 6;

/// Tokenize a whole stylesheet.
///
/// Infallible, like HTML parsing: malformed input produces `Bad*` tokens
/// the parser discards, never an error that abandons the sheet.
pub fn tokenize(input: &str) -> Vec<CssToken> {
    let mut tokenizer = Tokenizer::new(input);
    let mut out = Vec::new();
    while let Some(token) = tokenizer.next_token() {
        out.push(token);
    }
    out
}

/// The tokenizer. Holds the preprocessed input and a cursor.
pub struct Tokenizer {
    input: Vec<char>,
    pos: usize,
}

impl Tokenizer {
    /// Build a tokenizer over `input`, applying the spec's preprocessing:
    /// newlines folded to LF and NULs replaced, so no later step has to
    /// consider a CR or a NUL.
    pub fn new(input: &str) -> Self {
        Self {
            input: preprocess(input),
            pos: 0,
        }
    }

    /// The next token, or `None` at the end of the input.
    pub fn next_token(&mut self) -> Option<CssToken> {
        self.consume_comments();
        let c = self.peek()?;
        Some(match c {
            c if is_whitespace(c) => {
                self.consume_while(is_whitespace);
                CssToken::Whitespace
            }
            '"' | '\'' => {
                self.pos += 1;
                self.consume_string(c)
            }
            '#' => self.consume_hash(),
            '(' => self.single(CssToken::LeftParen),
            ')' => self.single(CssToken::RightParen),
            '[' => self.single(CssToken::LeftSquare),
            ']' => self.single(CssToken::RightSquare),
            '{' => self.single(CssToken::LeftCurly),
            '}' => self.single(CssToken::RightCurly),
            ',' => self.single(CssToken::Comma),
            ':' => self.single(CssToken::Colon),
            ';' => self.single(CssToken::Semicolon),
            '+' | '.' => {
                if self.starts_number() {
                    self.consume_numeric()
                } else {
                    self.single(CssToken::Delim(c))
                }
            }
            '-' => self.consume_hyphen(),
            '@' => self.consume_at_keyword(),
            '<' => {
                if self.starts_with("<!--") {
                    self.pos += 4;
                    CssToken::Cdo
                } else {
                    self.single(CssToken::Delim('<'))
                }
            }
            '\\' => {
                if self.valid_escape_at(self.pos) {
                    self.consume_ident_like()
                } else {
                    // A backslash before a newline is not an escape.
                    self.single(CssToken::Delim('\\'))
                }
            }
            c if c.is_ascii_digit() => self.consume_numeric(),
            c if is_ident_start(c) => self.consume_ident_like(),
            _ => self.single(CssToken::Delim(c)),
        })
    }

    // ---- input -----------------------------------------------------------

    fn peek(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.input.get(self.pos + offset).copied()
    }

    /// Emit `token` and step over the one character it came from.
    fn single(&mut self, token: CssToken) -> CssToken {
        self.pos += 1;
        token
    }

    fn consume_while(&mut self, accept: impl Fn(char) -> bool) {
        while self.peek().is_some_and(&accept) {
            self.pos += 1;
        }
    }

    fn starts_with(&self, literal: &str) -> bool {
        literal
            .chars()
            .enumerate()
            .all(|(offset, want)| self.peek_at(offset) == Some(want))
    }

    /// Comments are not tokens; a run of them between tokens vanishes.
    fn consume_comments(&mut self) {
        while self.starts_with("/*") {
            self.pos += 2;
            while self.pos < self.input.len() && !self.starts_with("*/") {
                self.pos += 1;
            }
            // An unterminated comment runs to the end of the sheet.
            self.pos = (self.pos + 2).min(self.input.len());
        }
    }

    // ---- escapes and lookahead -------------------------------------------

    /// Whether `index` begins a valid escape: a backslash followed by
    /// anything except a newline.
    fn valid_escape_at(&self, index: usize) -> bool {
        self.input.get(index).copied() == Some('\\')
            && self
                .input
                .get(index + 1)
                .copied()
                .is_some_and(|c| c != '\n')
    }

    /// Consume an escape, the backslash already read, and return the
    /// character it denotes.
    ///
    /// `\41` is `A`, and one trailing whitespace character after the hex
    /// digits is part of the escape rather than content — which is why
    /// `\65 m` is the two letters `em` and not `e m`.
    fn consume_escape(&mut self) -> char {
        let Some(first) = self.peek() else {
            // A trailing backslash denotes the replacement character.
            return REPLACEMENT;
        };
        if !first.is_ascii_hexdigit() {
            self.pos += 1;
            return first;
        }

        let mut digits = String::new();
        while let Some(c) = self.peek() {
            if digits.len() == MAX_ESCAPE_DIGITS || !c.is_ascii_hexdigit() {
                break;
            }
            digits.push(c);
            self.pos += 1;
        }
        if self.peek().is_some_and(is_whitespace) {
            self.pos += 1;
        }
        let code = u32::from_str_radix(&digits, 16).unwrap_or(0);
        match code {
            // Null, surrogates and out-of-range all become U+FFFD.
            0 | 0xd800..=0xdfff | 0x110000.. => REPLACEMENT,
            _ => char::from_u32(code).unwrap_or(REPLACEMENT),
        }
    }

    /// Whether the cursor begins a number: an optional sign, then either
    /// a digit, or a `.` followed by a digit.
    fn starts_number(&self) -> bool {
        let (first, second, third) = (self.peek(), self.peek_at(1), self.peek_at(2));
        match first {
            Some('+' | '-') => match second {
                Some(c) if c.is_ascii_digit() => true,
                Some('.') => third.is_some_and(|c| c.is_ascii_digit()),
                _ => false,
            },
            Some('.') => second.is_some_and(|c| c.is_ascii_digit()),
            Some(c) => c.is_ascii_digit(),
            None => false,
        }
    }

    /// Whether the cursor begins an identifier, counting escapes and the
    /// leading hyphens of custom properties.
    fn starts_ident(&self) -> bool {
        match self.peek() {
            Some('-') => match self.peek_at(1) {
                // `--custom-property`
                Some('-') => true,
                Some(c) if is_ident_start(c) => true,
                _ => self.valid_escape_at(self.pos + 1),
            },
            Some('\\') => self.valid_escape_at(self.pos),
            Some(c) => is_ident_start(c),
            None => false,
        }
    }

    // ---- token kinds ------------------------------------------------------

    /// A name, with escapes resolved.
    fn consume_name(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek() {
                Some(c) if is_ident_char(c) => {
                    out.push(c);
                    self.pos += 1;
                }
                Some('\\') if self.valid_escape_at(self.pos) => {
                    self.pos += 1;
                    out.push(self.consume_escape());
                }
                _ => return out,
            }
        }
    }

    /// An identifier, or a function, or `url(...)`.
    fn consume_ident_like(&mut self) -> CssToken {
        let name = self.consume_name();
        if self.peek() != Some('(') {
            return CssToken::Ident(name);
        }
        self.pos += 1;
        if !name.eq_ignore_ascii_case("url") {
            return CssToken::Function(name);
        }
        // `url(` may be followed by a quoted string, in which case it is
        // an ordinary function after all; only an unquoted argument makes
        // it a url token.
        let after_space = self.pos + self.count_while(is_whitespace);
        match self.input.get(after_space).copied() {
            Some('"' | '\'') => CssToken::Function(name),
            _ => self.consume_url(),
        }
    }

    /// How many characters from the cursor satisfy `accept`.
    fn count_while(&self, accept: impl Fn(char) -> bool) -> usize {
        self.input[self.pos.min(self.input.len())..]
            .iter()
            .take_while(|&&c| accept(c))
            .count()
    }

    /// An unquoted url, the `url(` already consumed.
    fn consume_url(&mut self) -> CssToken {
        self.consume_while(is_whitespace);
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return CssToken::Url(out),
                Some(')') => {
                    self.pos += 1;
                    return CssToken::Url(out);
                }
                Some(c) if is_whitespace(c) => {
                    self.consume_while(is_whitespace);
                    // Only `)` or the end may follow the whitespace.
                    return match self.peek() {
                        None => CssToken::Url(out),
                        Some(')') => {
                            self.pos += 1;
                            CssToken::Url(out)
                        }
                        Some(_) => self.consume_bad_url(),
                    };
                }
                // Characters that cannot appear in an unquoted url.
                Some('"' | '\'' | '(') => return self.consume_bad_url(),
                Some('\\') if self.valid_escape_at(self.pos) => {
                    self.pos += 1;
                    out.push(self.consume_escape());
                }
                Some('\\') => return self.consume_bad_url(),
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// Skip to the end of a malformed url so the parser resumes at a
    /// sane place rather than mid-argument.
    fn consume_bad_url(&mut self) -> CssToken {
        loop {
            match self.peek() {
                None => return CssToken::BadUrl,
                Some(')') => {
                    self.pos += 1;
                    return CssToken::BadUrl;
                }
                Some('\\') if self.valid_escape_at(self.pos) => {
                    self.pos += 1;
                    self.consume_escape();
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// A string, the opening quote already consumed.
    fn consume_string(&mut self, quote: char) -> CssToken {
        let mut out = String::new();
        loop {
            match self.peek() {
                // Unterminated at end of input: still a string.
                None => return CssToken::Str(out),
                Some(c) if c == quote => {
                    self.pos += 1;
                    return CssToken::Str(out);
                }
                // A raw newline ends the string badly — the usual cause
                // is a missing closing quote, and treating the rest of
                // the sheet as string content would lose all of it.
                Some('\n') => return CssToken::BadString,
                Some('\\') => {
                    // A backslash-newline is a line continuation.
                    if self.peek_at(1) == Some('\n') {
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                        out.push(self.consume_escape());
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn consume_hash(&mut self) -> CssToken {
        self.pos += 1;
        let next_is_name = self.peek().is_some_and(is_ident_char);
        if !next_is_name && !self.valid_escape_at(self.pos) {
            return CssToken::Delim('#');
        }
        // `id` is true only when the name could also be an identifier,
        // which is what separates `#main` from `#0a0`.
        let id = self.starts_ident();
        let value = self.consume_name();
        CssToken::Hash { value, id }
    }

    fn consume_at_keyword(&mut self) -> CssToken {
        self.pos += 1;
        if !self.starts_ident() {
            return CssToken::Delim('@');
        }
        CssToken::AtKeyword(self.consume_name())
    }

    /// A `-`, which may begin a number, an identifier, a `-->`, or be a
    /// delimiter on its own.
    fn consume_hyphen(&mut self) -> CssToken {
        if self.starts_number() {
            return self.consume_numeric();
        }
        if self.starts_with("-->") {
            self.pos += 3;
            return CssToken::Cdc;
        }
        if self.starts_ident() {
            return self.consume_ident_like();
        }
        self.single(CssToken::Delim('-'))
    }

    /// A number, percentage or dimension.
    fn consume_numeric(&mut self) -> CssToken {
        let (value, integer) = self.consume_number();
        if self.starts_ident() {
            let unit = self.consume_name();
            return CssToken::Dimension { value, unit };
        }
        if self.peek() == Some('%') {
            self.pos += 1;
            return CssToken::Percentage(value);
        }
        CssToken::Number { value, integer }
    }

    /// The numeric part, returning the value and whether it was written
    /// as an integer.
    fn consume_number(&mut self) -> (f64, bool) {
        let start = self.pos;
        let mut integer = true;
        if matches!(self.peek(), Some('+' | '-')) {
            self.pos += 1;
        }
        self.consume_while(|c| c.is_ascii_digit());
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            integer = false;
            self.pos += 1;
            self.consume_while(|c| c.is_ascii_digit());
        }
        if self.consume_exponent() {
            integer = false;
        }

        let text: String = self.input[start..self.pos].iter().collect();
        // Every path above accepts only characters `f64` can parse, so a
        // failure here would be a tokenizer bug, not bad input.
        (text.parse().unwrap_or(0.0), integer)
    }

    /// `e10`, `E-3`. Returns whether one was there.
    fn consume_exponent(&mut self) -> bool {
        if !matches!(self.peek(), Some('e' | 'E')) {
            return false;
        }
        let signed = matches!(self.peek_at(1), Some('+' | '-'));
        let digit_at = if signed { 2 } else { 1 };
        if !self.peek_at(digit_at).is_some_and(|c| c.is_ascii_digit()) {
            return false;
        }
        self.pos += digit_at;
        self.consume_while(|c| c.is_ascii_digit());
        true
    }
}

/// Fold newlines and replace NULs, per the spec's preprocessing.
fn preprocess(input: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\u{c}' => out.push('\n'),
            '\0' => out.push(REPLACEMENT),
            _ => out.push(c),
        }
    }
    out
}

/// CSS whitespace: newline, tab and space only.
fn is_whitespace(c: char) -> bool {
    matches!(c, '\n' | '\t' | ' ')
}

/// Whether `c` may start an identifier. Non-ASCII is included wholesale,
/// which is what makes `.café` and emoji class names work.
fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c >= '\u{80}'
}

/// Whether `c` may appear in an identifier after the first character.
fn is_ident_char(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit() || c == '-'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokens with whitespace dropped, which is what most assertions
    /// below care about.
    fn tokens(input: &str) -> Vec<CssToken> {
        tokenize(input)
            .into_iter()
            .filter(|t| *t != CssToken::Whitespace)
            .collect()
    }

    fn ident(name: &str) -> CssToken {
        CssToken::Ident(name.to_owned())
    }

    #[test]
    fn an_empty_sheet_has_no_tokens() {
        assert_eq!(tokenize(""), []);
    }

    #[test]
    fn a_simple_rule_tokenizes() {
        assert_eq!(
            tokens("p { color: red }"),
            [
                ident("p"),
                CssToken::LeftCurly,
                ident("color"),
                CssToken::Colon,
                ident("red"),
                CssToken::RightCurly,
            ]
        );
    }

    #[test]
    fn whitespace_runs_collapse_to_one_token() {
        assert_eq!(
            tokenize("a \n\t b"),
            [ident("a"), CssToken::Whitespace, ident("b")]
        );
    }

    #[test]
    fn comments_vanish_without_joining_tokens() {
        assert_eq!(tokens("a/*x*/b"), [ident("a"), ident("b")]);
        assert_eq!(tokens("/*a*//*b*/c"), [ident("c")]);
    }

    #[test]
    fn an_unterminated_comment_runs_to_the_end() {
        assert_eq!(tokens("a /* never closed"), [ident("a")]);
    }

    #[test]
    fn numbers_remember_whether_they_were_written_as_integers() {
        assert_eq!(
            tokens("1 1.0 -2 +3 .5 1e3"),
            [
                CssToken::Number {
                    value: 1.0,
                    integer: true
                },
                CssToken::Number {
                    value: 1.0,
                    integer: false
                },
                CssToken::Number {
                    value: -2.0,
                    integer: true
                },
                CssToken::Number {
                    value: 3.0,
                    integer: true
                },
                CssToken::Number {
                    value: 0.5,
                    integer: false
                },
                CssToken::Number {
                    value: 1000.0,
                    integer: false
                },
            ]
        );
    }

    #[test]
    fn dimensions_and_percentages_are_distinct_from_numbers() {
        assert_eq!(
            tokens("10px 50% 1.5em -3deg"),
            [
                CssToken::Dimension {
                    value: 10.0,
                    unit: "px".into()
                },
                CssToken::Percentage(50.0),
                CssToken::Dimension {
                    value: 1.5,
                    unit: "em".into()
                },
                CssToken::Dimension {
                    value: -3.0,
                    unit: "deg".into()
                },
            ]
        );
    }

    #[test]
    fn a_lone_dot_or_plus_is_a_delimiter_not_a_number() {
        assert_eq!(
            tokens(".foo + bar"),
            [
                CssToken::Delim('.'),
                ident("foo"),
                CssToken::Delim('+'),
                ident("bar"),
            ]
        );
    }

    #[test]
    fn a_hyphen_can_be_a_number_an_ident_or_a_delimiter() {
        assert_eq!(
            tokens("-5 -moz-thing --custom -"),
            [
                CssToken::Number {
                    value: -5.0,
                    integer: true
                },
                ident("-moz-thing"),
                ident("--custom"),
                CssToken::Delim('-'),
            ]
        );
    }

    #[test]
    fn strings_parse_in_both_quote_styles() {
        assert_eq!(
            tokens("\"double\" 'single'"),
            [
                CssToken::Str("double".into()),
                CssToken::Str("single".into()),
            ]
        );
    }

    #[test]
    fn a_newline_in_a_string_makes_it_bad() {
        // The usual cause is a missing quote; the parser drops the
        // declaration rather than eating the rest of the sheet.
        assert_eq!(
            tokens("a \"oops\nb"),
            [ident("a"), CssToken::BadString, ident("b")]
        );
    }

    #[test]
    fn a_backslash_newline_in_a_string_is_a_continuation() {
        assert_eq!(tokens("\"one\\\ntwo\""), [CssToken::Str("onetwo".into())]);
    }

    #[test]
    fn escapes_resolve_to_their_code_points() {
        assert_eq!(tokens("\\41"), [ident("A")]);
        assert_eq!(tokens("\"\\41\""), [CssToken::Str("A".into())]);
        // One space after the hex digits belongs to the escape.
        assert_eq!(tokens("\\65 m"), [ident("em")]);
        // A non-hex escape is the character itself.
        assert_eq!(tokens("\\-x"), [ident("-x")]);
    }

    #[test]
    fn an_escape_of_null_or_a_surrogate_becomes_the_replacement_char() {
        assert_eq!(tokens("\\0"), [ident("\u{fffd}")]);
        assert_eq!(tokens("\\D800"), [ident("\u{fffd}")]);
        assert_eq!(tokens("\\110000"), [ident("\u{fffd}")]);
    }

    #[test]
    fn hashes_distinguish_ids_from_colours() {
        assert_eq!(
            tokens("#main #0a0 #"),
            [
                CssToken::Hash {
                    value: "main".into(),
                    id: true
                },
                CssToken::Hash {
                    value: "0a0".into(),
                    id: false
                },
                CssToken::Delim('#'),
            ]
        );
    }

    #[test]
    fn a_function_keeps_its_name_and_consumes_its_paren() {
        assert_eq!(
            tokens("rgb(1,2,3)"),
            [
                CssToken::Function("rgb".into()),
                CssToken::Number {
                    value: 1.0,
                    integer: true
                },
                CssToken::Comma,
                CssToken::Number {
                    value: 2.0,
                    integer: true
                },
                CssToken::Comma,
                CssToken::Number {
                    value: 3.0,
                    integer: true
                },
                CssToken::RightParen,
            ]
        );
    }

    #[test]
    fn an_unquoted_url_is_one_token() {
        assert_eq!(tokens("url(a/b.png)"), [CssToken::Url("a/b.png".into())]);
        assert_eq!(
            tokens("url(  spaced.png  )"),
            [CssToken::Url("spaced.png".into())]
        );
    }

    #[test]
    fn a_quoted_url_is_an_ordinary_function() {
        assert_eq!(
            tokens("url('a.png')"),
            [
                CssToken::Function("url".into()),
                CssToken::Str("a.png".into()),
                CssToken::RightParen,
            ]
        );
    }

    #[test]
    fn url_is_recognised_case_insensitively() {
        assert_eq!(tokens("URL(a.png)"), [CssToken::Url("a.png".into())]);
    }

    #[test]
    fn a_malformed_url_is_skipped_to_its_closing_paren() {
        assert_eq!(
            tokens("url(a b) x"),
            [CssToken::BadUrl, ident("x")],
            "recovery must resume after the url, not inside it"
        );
    }

    #[test]
    fn at_keywords_carry_their_name() {
        assert_eq!(
            tokens("@media screen"),
            [CssToken::AtKeyword("media".into()), ident("screen")]
        );
        assert_eq!(tokens("@ x"), [CssToken::Delim('@'), ident("x")]);
    }

    #[test]
    fn html_comment_delimiters_are_tokens_at_the_top_level() {
        assert_eq!(
            tokens("<!-- a -->"),
            [CssToken::Cdo, ident("a"), CssToken::Cdc]
        );
    }

    #[test]
    fn selector_combinators_come_through_as_delimiters() {
        assert_eq!(
            tokens("a > b ~ c + d * e"),
            [
                ident("a"),
                CssToken::Delim('>'),
                ident("b"),
                CssToken::Delim('~'),
                ident("c"),
                CssToken::Delim('+'),
                ident("d"),
                CssToken::Delim('*'),
                ident("e"),
            ]
        );
    }

    #[test]
    fn attribute_selector_punctuation_tokenizes() {
        assert_eq!(
            tokens("[data-x=\"1\"]"),
            [
                CssToken::LeftSquare,
                ident("data-x"),
                CssToken::Delim('='),
                CssToken::Str("1".into()),
                CssToken::RightSquare,
            ]
        );
    }

    #[test]
    fn non_ascii_identifiers_are_allowed() {
        assert_eq!(tokens(".café"), [CssToken::Delim('.'), ident("café")]);
    }

    #[test]
    fn nulls_become_the_replacement_character() {
        assert_eq!(tokens("a\0b"), [ident("a\u{fffd}b")]);
    }

    #[test]
    fn newlines_are_normalised_before_tokenizing() {
        assert_eq!(tokenize("a\r\nb").len(), 3, "ident, whitespace, ident");
    }

    #[test]
    fn a_realistic_rule_block_round_trips() {
        let css = ".card:hover > p::first-line { margin: 0 auto !important; }";
        let got = tokens(css);
        assert_eq!(got.first(), Some(&CssToken::Delim('.')));
        assert_eq!(got.last(), Some(&CssToken::RightCurly));
        assert!(got.contains(&ident("hover")));
        assert!(got.contains(&ident("first-line")));
        assert!(got.contains(&CssToken::Delim('!')));
        assert!(got.contains(&ident("important")));
    }
}
