//! The CSS parser: a token stream becomes rules and declarations.
//!
//! This stage is structural only. It finds where each rule starts and
//! ends, splits blocks into `name: value` pairs, and nests functions and
//! brackets — but it does not know that `color` is a property or that
//! `red` is a colour. A declaration whose property nobody implements
//! parses perfectly well here and is dropped later, by the cascade.
//!
//! That separation is what makes CSS forward-compatible, and it rests on
//! one rule: **recovery discards exactly one construct.** A broken
//! declaration costs you that declaration, not the rule; a broken rule
//! costs you the rule, not the stylesheet. Getting the boundary wrong in
//! either direction is catastrophic in a way that is hard to notice —
//! too greedy and one typo blanks a page, too timid and garbage becomes
//! style.
//!
//! The output keeps selectors as unparsed [`ComponentValue`]s. Selector
//! parsing is a separate grammar with its own recovery rules, so it gets
//! its own stage rather than being tangled into block-finding.

use super::tokenizer::{tokenize, CssToken};

/// A parsed stylesheet: its rules in source order.
///
/// Order is load-bearing — when two declarations have equal specificity,
/// the later one wins — so this is a `Vec` and never a map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Rule {
    Style(StyleRule),
    At(AtRule),
}

/// A rule with a selector and declarations: the ordinary kind.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleRule {
    /// The selector text, still as component values. Whitespace is kept,
    /// because in a selector it is the descendant combinator.
    pub prelude: Vec<ComponentValue>,
    pub declarations: Vec<Declaration>,
}

/// An at-rule: `@import url(x);` or `@media screen { ... }`.
#[derive(Clone, Debug, PartialEq)]
pub struct AtRule {
    pub name: String,
    pub prelude: Vec<ComponentValue>,
    pub block: Option<AtBlock>,
}

/// What an at-rule's block holds.
///
/// The split exists because it depends on the at-rule, and only the
/// name can say which: a *conditional group rule* nests more rules,
/// everything else holds declarations or something stranger.
#[derive(Clone, Debug, PartialEq)]
pub enum AtBlock {
    /// A group rule's contents, parsed. The cascade decides whether
    /// the condition holds and the rules inside apply.
    Rules(Vec<Rule>),
    /// An at-rule whose block is a list of declarations, the way a
    /// style rule's is. `@font-face` is the one that matters: it
    /// says where a page's own typeface comes from.
    Declarations(Vec<Declaration>),
    /// Anything else, kept unparsed — `@keyframes` holds keyframe
    /// blocks, which nothing animates yet.
    Raw(Vec<ComponentValue>),
}

/// At-rules whose block holds rules rather than declarations.
///
/// Leaving these unparsed is not a small loss: a Tailwind stylesheet
/// puts nearly everything it has inside `@layer`, so a parser that
/// stops at the brace sees a few dozen rules out of some fifteen
/// hundred.
const GROUP_AT_RULES: [&str; 5] = ["media", "supports", "layer", "container", "scope"];

/// At-rules whose block is a declaration list rather than rules.
///
/// `@font-face` is read; the others are listed because their blocks
/// have the same shape, and parsing them costs nothing over keeping
/// the tokens.
const DECLARATION_AT_RULES: [&str; 3] = ["font-face", "page", "counter-style"];

/// One `property: value` pair.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    /// Lowercased: CSS property names are case-insensitive, and folding
    /// here means no later lookup has to remember that.
    pub name: String,
    /// The value, whitespace-trimmed at the ends but kept inside, since
    /// `margin: 0 auto` needs its gap.
    pub value: Vec<ComponentValue>,
    pub important: bool,
}

/// A value fragment: a token, a function call, or a bracketed block.
///
/// Nesting is resolved here so that no later stage has to count
/// brackets. `calc(1px + (2px * 3))` arrives as one function containing
/// one nested block.
#[derive(Clone, Debug, PartialEq)]
pub enum ComponentValue {
    Token(CssToken),
    Function {
        name: String,
        arguments: Vec<ComponentValue>,
    },
    Block {
        kind: BlockKind,
        values: Vec<ComponentValue>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Curly,
    Square,
    Paren,
}

impl BlockKind {
    /// The token that closes this kind of block.
    fn closer(self) -> CssToken {
        match self {
            Self::Curly => CssToken::RightCurly,
            Self::Square => CssToken::RightSquare,
            Self::Paren => CssToken::RightParen,
        }
    }
}

/// Parse a stylesheet. Infallible: malformed constructs are discarded.
pub fn parse(input: &str) -> Stylesheet {
    let mut parser = Parser {
        tokens: tokenize(input),
        pos: 0,
    };
    Stylesheet {
        rules: parser.consume_rules(false),
    }
}

/// Parse the contents of a `style` attribute — declarations with no
/// surrounding rule.
/// Parse a bare value — no property name, no semicolon.
///
/// An SVG presentation attribute carries one: `fill="rgb(1 2 3)"` is
/// a CSS value written where an attribute goes, and it has to reach
/// the same parsers a declaration's value would.
pub fn parse_value(input: &str) -> Vec<ComponentValue> {
    let mut parser = Parser {
        tokens: tokenize(input),
        pos: 0,
    };
    let mut values = Vec::new();
    while parser.peek().is_some() {
        values.push(parser.consume_component_value());
    }
    trim_whitespace(values)
}

pub fn parse_declarations(input: &str) -> Vec<Declaration> {
    let mut parser = Parser {
        tokens: tokenize(input),
        pos: 0,
    };
    parser.consume_declaration_block(false)
}

struct Parser {
    tokens: Vec<CssToken>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&CssToken> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<CssToken> {
        let token = self.tokens.get(self.pos).cloned();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(CssToken::Whitespace)) {
            self.pos += 1;
        }
    }

    // ---- rules -----------------------------------------------------------

    /// A list of rules.
    ///
    /// `bounded` is true inside a group at-rule's block, where a `}`
    /// ends the list; at the top level it is a stray to be skipped.
    fn consume_rules(&mut self, bounded: bool) -> Vec<Rule> {
        let mut rules = Vec::new();
        loop {
            match self.peek() {
                None => return rules,
                Some(CssToken::Whitespace) => self.pos += 1,
                // Legal at the top level only, and meaningless: they let
                // a stylesheet sit inside an HTML comment.
                Some(CssToken::Cdo | CssToken::Cdc) => self.pos += 1,
                Some(CssToken::AtKeyword(_)) => {
                    let rule = self.consume_at_rule();
                    rules.push(Rule::At(rule));
                }
                Some(CssToken::RightCurly) => {
                    self.pos += 1;
                    if bounded {
                        return rules;
                    }
                    // A stray `}` at the top level: skipped rather than
                    // treated as the start of a selector.
                }
                Some(_) => {
                    if let Some(rule) = self.consume_qualified_rule() {
                        rules.push(Rule::Style(rule));
                    }
                }
            }
        }
    }

    /// A selector plus its block. Returns `None` for a rule whose block
    /// never arrives — at end of input the whole rule is discarded,
    /// because a selector with no declarations styles nothing.
    fn consume_qualified_rule(&mut self) -> Option<StyleRule> {
        let mut prelude = Vec::new();
        loop {
            match self.peek() {
                None => return None,
                Some(CssToken::LeftCurly) => {
                    self.pos += 1;
                    return Some(StyleRule {
                        prelude: trim_whitespace(prelude),
                        declarations: self.consume_declaration_block(true),
                    });
                }
                Some(_) => prelude.push(self.consume_component_value()),
            }
        }
    }

    fn consume_at_rule(&mut self) -> AtRule {
        let name = match self.advance() {
            Some(CssToken::AtKeyword(name)) => name.to_ascii_lowercase(),
            // Only called when an at-keyword is next.
            _ => String::new(),
        };
        let mut prelude = Vec::new();
        let mut block = None;
        loop {
            match self.peek() {
                // An unterminated at-rule still yields what it had.
                None => break,
                Some(CssToken::Semicolon) => {
                    self.pos += 1;
                    break;
                }
                Some(CssToken::LeftCurly) => {
                    self.pos += 1;
                    block = Some(if GROUP_AT_RULES.contains(&name.as_str()) {
                        AtBlock::Rules(self.consume_rules(true))
                    } else if DECLARATION_AT_RULES.contains(&name.as_str()) {
                        AtBlock::Declarations(self.consume_declaration_block(true))
                    } else {
                        AtBlock::Raw(self.consume_until_close(&CssToken::RightCurly))
                    });
                    break;
                }
                Some(_) => prelude.push(self.consume_component_value()),
            }
        }
        AtRule {
            name,
            prelude: trim_whitespace(prelude),
            block,
        }
    }

    // ---- declarations ----------------------------------------------------

    /// The declarations inside a `{ }` block, the `{` already consumed.
    ///
    /// `bounded` is false for a `style` attribute, which has no closing
    /// brace to stop at.
    fn consume_declaration_block(&mut self, bounded: bool) -> Vec<Declaration> {
        let mut declarations = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                None => return declarations,
                Some(CssToken::Semicolon) => self.pos += 1,
                Some(CssToken::RightCurly) => {
                    self.pos += 1;
                    // In a block this is the end; in a `style` attribute
                    // it is a stray brace, so carry on past it.
                    if bounded {
                        return declarations;
                    }
                }
                // Nested at-rules in a style block (`@media` inside a
                // rule) are consumed and dropped: nothing reads them yet,
                // and leaving them would make them look like properties.
                Some(CssToken::AtKeyword(_)) => {
                    self.consume_at_rule();
                }
                Some(CssToken::Ident(_)) => {
                    if let Some(declaration) = self.consume_declaration() {
                        declarations.push(declaration);
                    }
                }
                // Anything else cannot begin a property name.
                Some(_) => self.discard_declaration(),
            }
        }
    }

    /// One declaration, an ident known to be next.
    fn consume_declaration(&mut self) -> Option<Declaration> {
        let name = match self.advance() {
            Some(CssToken::Ident(name)) => name.to_ascii_lowercase(),
            _ => return None,
        };
        self.skip_whitespace();
        if !matches!(self.peek(), Some(CssToken::Colon)) {
            // `color red` — no colon, so this is not a declaration.
            self.discard_declaration();
            return None;
        }
        self.pos += 1;

        let mut value = Vec::new();
        loop {
            match self.peek() {
                None | Some(CssToken::Semicolon) | Some(CssToken::RightCurly) => break,
                Some(_) => value.push(self.consume_component_value()),
            }
        }

        let (value, important) = split_important(trim_whitespace(value));
        // An empty value is not a declaration: `color: ;` sets nothing.
        if value.is_empty() {
            return None;
        }
        Some(Declaration {
            name,
            value,
            important,
        })
    }

    /// Skip a malformed declaration, stopping before the `}` that would
    /// end the block. Nested blocks are skipped whole, so a `;` inside
    /// `calc(...)` does not end the recovery early.
    fn discard_declaration(&mut self) {
        loop {
            match self.peek() {
                None | Some(CssToken::RightCurly) => return,
                Some(CssToken::Semicolon) => {
                    self.pos += 1;
                    return;
                }
                Some(_) => {
                    self.consume_component_value();
                }
            }
        }
    }

    // ---- component values ------------------------------------------------

    fn consume_component_value(&mut self) -> ComponentValue {
        let Some(token) = self.advance() else {
            // Only reached when a caller checked `peek` first, so this is
            // unreachable in practice; whitespace is the harmless filler.
            return ComponentValue::Token(CssToken::Whitespace);
        };
        match token {
            CssToken::LeftCurly => self.consume_block(BlockKind::Curly),
            CssToken::LeftSquare => self.consume_block(BlockKind::Square),
            CssToken::LeftParen => self.consume_block(BlockKind::Paren),
            CssToken::Function(name) => ComponentValue::Function {
                arguments: self.consume_until_close(&CssToken::RightParen),
                name,
            },
            other => ComponentValue::Token(other),
        }
    }

    fn consume_block(&mut self, kind: BlockKind) -> ComponentValue {
        ComponentValue::Block {
            values: self.consume_until_close(&kind.closer()),
            kind,
        }
    }

    /// Component values up to `closer`, which is consumed. Unclosed
    /// blocks end at the end of input rather than failing.
    fn consume_until_close(&mut self, closer: &CssToken) -> Vec<ComponentValue> {
        let mut values = Vec::new();
        loop {
            match self.peek() {
                None => return values,
                Some(token) if token == closer => {
                    self.pos += 1;
                    return values;
                }
                Some(_) => values.push(self.consume_component_value()),
            }
        }
    }
}

/// Write component values back out as CSS text.
///
/// Not a faithful round-trip of the source — comments are gone, numbers
/// are reformatted — but a faithful rendering of the *value*, which is
/// what a `style` attribute read-back, a devtools pane and a test
/// assertion all want. Keeping it here rather than in each caller means
/// one place knows how a dimension or a function is spelled.
pub fn serialize(values: &[ComponentValue]) -> String {
    values.iter().map(serialize_one).collect()
}

fn serialize_one(value: &ComponentValue) -> String {
    match value {
        ComponentValue::Token(token) => serialize_token(token),
        ComponentValue::Function { name, arguments } => {
            format!("{name}({})", serialize(arguments))
        }
        ComponentValue::Block { kind, values } => {
            let inner = serialize(values);
            match kind {
                BlockKind::Curly => format!("{{{inner}}}"),
                BlockKind::Square => format!("[{inner}]"),
                BlockKind::Paren => format!("({inner})"),
            }
        }
    }
}

fn serialize_token(token: &CssToken) -> String {
    match token {
        CssToken::Ident(name) => name.clone(),
        CssToken::AtKeyword(name) => format!("@{name}"),
        CssToken::Hash { value, .. } => format!("#{value}"),
        CssToken::Str(text) => format!("\"{text}\""),
        CssToken::Url(url) => format!("url({url})"),
        CssToken::Delim(c) => c.to_string(),
        CssToken::Number { value, .. } => format!("{value}"),
        CssToken::Percentage(value) => format!("{value}%"),
        CssToken::Dimension { value, unit } => format!("{value}{unit}"),
        CssToken::Whitespace => " ".to_owned(),
        CssToken::Colon => ":".to_owned(),
        CssToken::Semicolon => ";".to_owned(),
        CssToken::Comma => ",".to_owned(),
        CssToken::LeftCurly => "{".to_owned(),
        CssToken::RightCurly => "}".to_owned(),
        CssToken::LeftSquare => "[".to_owned(),
        CssToken::RightSquare => "]".to_owned(),
        CssToken::LeftParen => "(".to_owned(),
        CssToken::RightParen => ")".to_owned(),
        CssToken::Function(name) => format!("{name}("),
        CssToken::Cdo => "<!--".to_owned(),
        CssToken::Cdc => "-->".to_owned(),
        // The bad tokens have no text: whatever produced them was
        // discarded, so rendering them as nothing is honest.
        CssToken::BadString | CssToken::BadUrl => String::new(),
    }
}

/// Drop leading and trailing whitespace tokens, keeping interior ones.
pub(super) fn trim_whitespace(mut values: Vec<ComponentValue>) -> Vec<ComponentValue> {
    let whitespace = ComponentValue::Token(CssToken::Whitespace);
    while values.first() == Some(&whitespace) {
        values.remove(0);
    }
    while values.last() == Some(&whitespace) {
        values.pop();
    }
    values
}

/// Split a trailing `!important` off a value.
///
/// It is not part of the value — `color: red !important` and
/// `color: red` have the same value and differ only in cascade priority
/// — so it is lifted out here rather than left for every property parser
/// to strip.
fn split_important(mut values: Vec<ComponentValue>) -> (Vec<ComponentValue>, bool) {
    let is_important_ident = |value: &ComponentValue| match value {
        ComponentValue::Token(CssToken::Ident(name)) => name.eq_ignore_ascii_case("important"),
        _ => false,
    };
    let is_bang =
        |value: &ComponentValue| matches!(value, ComponentValue::Token(CssToken::Delim('!')));

    let mut index = values.len();
    // ... important
    index = match index.checked_sub(1) {
        Some(at) if values.get(at).is_some_and(is_important_ident) => at,
        _ => return (values, false),
    };
    // Optional whitespace between `!` and `important`.
    if index
        .checked_sub(1)
        .and_then(|at| values.get(at))
        .is_some_and(|value| *value == ComponentValue::Token(CssToken::Whitespace))
    {
        index -= 1;
    }
    index = match index.checked_sub(1) {
        Some(at) if values.get(at).is_some_and(is_bang) => at,
        _ => return (values, false),
    };

    values.truncate(index);
    (trim_whitespace(values), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values back to text, so assertions read like CSS.
    fn render(values: &[ComponentValue]) -> String {
        serialize(values)
    }

    /// One declaration flattened for assertions.
    type DeclarationSketch = (String, String, bool);
    /// One style rule flattened: its selector text and declarations.
    type RuleSketch = (String, Vec<DeclarationSketch>);

    /// The style rules of a sheet, as `(selector, [(prop, value, important)])`.
    fn style_rules(input: &str) -> Vec<RuleSketch> {
        parse(input)
            .rules
            .into_iter()
            .filter_map(|rule| match rule {
                Rule::Style(style) => Some((
                    render(&style.prelude),
                    style
                        .declarations
                        .into_iter()
                        .map(|d| (d.name, render(&d.value), d.important))
                        .collect(),
                )),
                Rule::At(_) => None,
            })
            .collect()
    }

    #[test]
    fn an_empty_sheet_has_no_rules() {
        assert_eq!(parse("").rules, []);
        assert_eq!(parse("   \n\t ").rules, []);
        assert_eq!(parse("/* just a comment */").rules, []);
    }

    #[test]
    fn a_simple_rule_parses() {
        assert_eq!(
            style_rules("p { color: red }"),
            [("p".to_owned(), vec![("color".into(), "red".into(), false)])]
        );
    }

    #[test]
    fn several_declarations_keep_their_order() {
        assert_eq!(
            style_rules("p { margin: 0; padding: 1px; color: red }"),
            [(
                "p".to_owned(),
                vec![
                    ("margin".into(), "0".into(), false),
                    ("padding".into(), "1px".into(), false),
                    ("color".into(), "red".into(), false),
                ]
            )]
        );
    }

    #[test]
    fn property_names_fold_case_but_values_do_not() {
        assert_eq!(
            style_rules("p { COLOR: Red }"),
            [("p".to_owned(), vec![("color".into(), "Red".into(), false)])]
        );
    }

    #[test]
    fn a_multi_part_value_keeps_its_internal_spacing() {
        assert_eq!(
            style_rules("p { margin: 0 auto 1px }"),
            [(
                "p".to_owned(),
                vec![("margin".into(), "0 auto 1px".into(), false)]
            )]
        );
    }

    #[test]
    fn important_is_lifted_out_of_the_value() {
        assert_eq!(
            style_rules("p { color: red !important }"),
            [("p".to_owned(), vec![("color".into(), "red".into(), true)])]
        );
        // No space, and odd case, are both legal.
        assert_eq!(
            style_rules("p { color: red!IMPORTANT }"),
            [("p".to_owned(), vec![("color".into(), "red".into(), true)])]
        );
    }

    #[test]
    fn a_value_merely_containing_important_is_not_important() {
        assert_eq!(
            style_rules("p { font-family: important }"),
            [(
                "p".to_owned(),
                vec![("font-family".into(), "important".into(), false)]
            )]
        );
    }

    #[test]
    fn selectors_are_kept_whole_including_their_whitespace() {
        // The space between `div` and `p` is the descendant combinator,
        // so losing it would change what the rule matches.
        assert_eq!(
            style_rules("div p > a.link:hover { color: red }")[0].0,
            "div p > a.link:hover"
        );
    }

    #[test]
    fn a_selector_list_is_one_prelude() {
        assert_eq!(style_rules("h1, h2, h3 { margin: 0 }")[0].0, "h1, h2, h3");
    }

    #[test]
    fn functions_nest_without_the_caller_counting_brackets() {
        assert_eq!(
            style_rules("p { width: calc(100% - (2px * 3)) }"),
            [(
                "p".to_owned(),
                vec![("width".into(), "calc(100% - (2px * 3))".into(), false)]
            )]
        );
    }

    #[test]
    fn urls_survive_as_values() {
        assert_eq!(
            style_rules("p { background: url(a/b.png) no-repeat }"),
            [(
                "p".to_owned(),
                vec![("background".into(), "url(a/b.png) no-repeat".into(), false)]
            )]
        );
    }

    #[test]
    fn a_declaration_with_no_colon_is_dropped_and_the_rest_survives() {
        // The recovery boundary: one bad declaration, not the whole rule.
        assert_eq!(
            style_rules("p { color red; margin: 0 }"),
            [("p".to_owned(), vec![("margin".into(), "0".into(), false)])]
        );
    }

    #[test]
    fn an_empty_value_is_not_a_declaration() {
        assert_eq!(
            style_rules("p { color: ; margin: 0 }"),
            [("p".to_owned(), vec![("margin".into(), "0".into(), false)])]
        );
    }

    #[test]
    fn a_bad_declaration_with_a_semicolon_inside_a_function_recovers_correctly() {
        assert_eq!(
            style_rules("p { junk calc(1;2); margin: 0 }"),
            [("p".to_owned(), vec![("margin".into(), "0".into(), false)])],
            "the ; inside calc must not end the recovery early"
        );
    }

    #[test]
    fn a_trailing_semicolon_is_harmless_and_so_is_its_absence() {
        assert_eq!(
            style_rules("p { color: red; }")[0].1,
            [("color".to_owned(), "red".to_owned(), false)]
        );
        assert_eq!(
            style_rules("p { color: red }")[0].1,
            [("color".to_owned(), "red".to_owned(), false)]
        );
    }

    #[test]
    fn a_later_rule_does_not_lose_its_place_after_a_broken_one() {
        let rules = style_rules("p { color: } div { color: blue } span { color: green }");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[1].0, "div");
        assert_eq!(rules[2].0, "span");
    }

    #[test]
    fn an_unclosed_rule_at_end_of_input_keeps_what_it_had() {
        assert_eq!(
            style_rules("p { color: red"),
            [("p".to_owned(), vec![("color".into(), "red".into(), false)])]
        );
    }

    #[test]
    fn a_selector_with_no_block_is_discarded_entirely() {
        assert_eq!(style_rules("p, div"), []);
    }

    #[test]
    fn a_stray_closing_brace_does_not_derail_the_next_rule() {
        let rules = style_rules("} p { color: red }");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].0, "p");
    }

    #[test]
    fn an_at_rule_with_a_semicolon_has_no_block() {
        let sheet = parse("@import url(a.css);");
        let Some(Rule::At(rule)) = sheet.rules.first() else {
            panic!("expected an at-rule, got {:?}", sheet.rules);
        };
        assert_eq!(rule.name, "import");
        assert_eq!(render(&rule.prelude), "url(a.css)");
        assert_eq!(rule.block, None);
    }

    #[test]
    fn a_group_at_rule_holds_parsed_rules() {
        let sheet = parse("@media screen { p { color: red } }");
        let Some(Rule::At(rule)) = sheet.rules.first() else {
            panic!("expected an at-rule, got {:?}", sheet.rules);
        };
        assert_eq!(rule.name, "media");
        assert_eq!(render(&rule.prelude), "screen");
        let Some(AtBlock::Rules(inner)) = &rule.block else {
            panic!("expected nested rules, got {:?}", rule.block);
        };
        assert_eq!(inner.len(), 1);
        let Some(Rule::Style(style)) = inner.first() else {
            panic!("expected a style rule");
        };
        assert_eq!(render(&style.prelude), "p");
        assert_eq!(style.declarations[0].name, "color");
    }

    #[test]
    fn group_at_rules_nest() {
        // Tailwind's output is `@layer` wrapping `@media` wrapping the
        // rules, so one level of unwrapping is not enough.
        let sheet = parse("@layer utils { @media screen { p { color: red } } }");
        let Some(Rule::At(outer)) = sheet.rules.first() else {
            panic!("expected @layer");
        };
        let Some(AtBlock::Rules(middle)) = &outer.block else {
            panic!("expected nested rules in @layer");
        };
        let Some(Rule::At(inner)) = middle.first() else {
            panic!("expected @media inside @layer");
        };
        let Some(AtBlock::Rules(rules)) = &inner.block else {
            panic!("expected nested rules in @media");
        };
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn an_at_rule_that_holds_declarations_gets_them_parsed() {
        // `@font-face` holds declarations, not rules, and only the
        // at-rule's name can say which — so the name decides.
        let sheet = parse("@font-face { font-family: X; src: url(a.woff2) }");
        let Some(Rule::At(rule)) = sheet.rules.first() else {
            panic!("expected an at-rule");
        };
        let Some(AtBlock::Declarations(declarations)) = &rule.block else {
            panic!("expected declarations, got {:?}", rule.block);
        };
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[0].name, "font-family");
        assert_eq!(declarations[1].name, "src");
    }

    #[test]
    fn an_at_rule_holding_neither_rules_nor_declarations_stays_raw() {
        // `@keyframes` holds keyframe blocks, which are neither.
        let sheet = parse("@keyframes spin { from { opacity: 0 } }");
        let Some(Rule::At(rule)) = sheet.rules.first() else {
            panic!("expected an at-rule");
        };
        assert!(
            matches!(rule.block, Some(AtBlock::Raw(_))),
            "got {:?}",
            rule.block
        );
    }

    #[test]
    fn at_rule_names_fold_case() {
        let sheet = parse("@MEDIA screen {}");
        let Some(Rule::At(rule)) = sheet.rules.first() else {
            panic!("expected an at-rule");
        };
        assert_eq!(rule.name, "media");
    }

    #[test]
    fn rules_after_an_at_rule_still_parse() {
        let sheet = parse("@import url(a.css); p { color: red }");
        assert_eq!(sheet.rules.len(), 2);
        assert!(matches!(sheet.rules[1], Rule::Style(_)));
    }

    #[test]
    fn html_comment_delimiters_at_the_top_level_are_skipped() {
        assert_eq!(
            style_rules("<!-- p { color: red } -->"),
            [("p".to_owned(), vec![("color".into(), "red".into(), false)])]
        );
    }

    #[test]
    fn a_style_attribute_parses_without_a_surrounding_rule() {
        let declarations = parse_declarations("color: red; margin: 0 auto");
        let rendered: Vec<_> = declarations
            .iter()
            .map(|d| (d.name.as_str(), render(&d.value)))
            .collect();
        assert_eq!(
            rendered,
            [("color", "red".to_owned()), ("margin", "0 auto".to_owned())]
        );
    }

    #[test]
    fn a_style_attribute_recovers_from_a_broken_declaration() {
        let declarations = parse_declarations("color red; margin: 0");
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].name, "margin");
    }

    #[test]
    fn an_empty_style_attribute_yields_nothing() {
        assert_eq!(parse_declarations(""), []);
        assert_eq!(parse_declarations("  ;; "), []);
    }

    #[test]
    fn attribute_selectors_survive_in_a_prelude() {
        assert_eq!(
            style_rules("a[href^=\"https\"] { color: red }")[0].0,
            "a[href^=\"https\"]"
        );
    }

    #[test]
    fn a_realistic_sheet_parses_every_rule() {
        let css = "
            /* reset */
            * { margin: 0; padding: 0 }
            @media (min-width: 40em) {
                .card { padding: 1rem }
            }
            .card { border: 1px solid #ccc; background: url(bg.png) }
            .card:hover > .title { color: rgb(0, 0, 0) !important }
        ";
        let sheet = parse(css);
        assert_eq!(sheet.rules.len(), 4);
        let styles = style_rules(css);
        assert_eq!(styles.len(), 3, "the @media rule is not a style rule");
        assert_eq!(styles[2].0, ".card:hover > .title");
        assert_eq!(
            styles[2].1,
            [("color".to_owned(), "rgb(0, 0, 0)".to_owned(), true)]
        );
    }
}
