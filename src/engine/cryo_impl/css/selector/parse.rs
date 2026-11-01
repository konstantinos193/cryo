//! Parsing selectors out of a rule's prelude.
//!
//! The prelude arrives as [`ComponentValue`]s, where a selector's pieces
//! are already split by the tokenizer but carry no meaning: `.card` is a
//! delimiter and an ident, `#main` is a hash, `[href]` is a square
//! block. This stage gives them meaning by position.
//!
//! **An invalid selector invalidates the whole rule.** Not just the one
//! selector in the list — `h1, h2::bogus, h3 { … }` styles nothing at
//! all. That is deliberate in CSS: it means a rule using a selector the
//! engine does not know cannot half-apply, which is what lets authors
//! use a new selector as a feature test. So every function here returns
//! `Option` and a single `None` discards the rule.
//!
//! The one piece of real arithmetic is `an+b`, and it is harder than it
//! looks because the tokenizer splits it unhelpfully: `2n-1` arrives as
//! a single dimension with the unit `n-1`, while `n+1` arrives as an
//! ident and a signed number. [`nth_from_tokens`] handles the shapes.

use super::{
    AttrMatch, AttributeSelector, Combinator, Compound, Nth, PseudoClass, PseudoElement, Selector,
    SelectorList, SelectorPart,
};
use crate::engine::cryo_impl::css::parser::{BlockKind, ComponentValue};
use crate::engine::cryo_impl::css::tokenizer::CssToken;

/// Parse a comma-separated selector list. `None` discards the rule.
pub fn parse_selector_list(prelude: &[ComponentValue]) -> Option<SelectorList> {
    let mut selectors = Vec::new();
    for group in split_on_commas(prelude) {
        selectors.push(parse_complex(&group)?);
    }
    if selectors.is_empty() {
        return None;
    }
    Some(SelectorList { selectors })
}

/// Split a prelude on top-level commas. Commas inside a `:is(...)` are
/// already nested inside a function's arguments, so they are not seen.
fn split_on_commas(values: &[ComponentValue]) -> Vec<Vec<ComponentValue>> {
    let mut groups = vec![Vec::new()];
    for value in values {
        if *value == ComponentValue::Token(CssToken::Comma) {
            groups.push(Vec::new());
        } else {
            let last = groups.last_mut().expect("groups always has one entry");
            last.push(value.clone());
        }
    }
    groups
}

/// Parse one complex selector: compounds joined by combinators.
///
/// Stored innermost-first, because that is the order matching runs in.
fn parse_complex(values: &[ComponentValue]) -> Option<Selector> {
    let mut cursor = Cursor::new(values);
    let mut compounds = Vec::new();
    let mut combinators: Vec<Combinator> = Vec::new();
    let mut pseudo_element = None;

    loop {
        cursor.skip_whitespace();
        let (compound, found_pseudo) = parse_compound(&mut cursor)?;
        // A pseudo-element may only sit on the rightmost compound; a
        // combinator after one is nonsense (`::before > p`).
        if pseudo_element.is_some() {
            return None;
        }
        pseudo_element = found_pseudo;
        compounds.push(compound);

        match next_combinator(&mut cursor) {
            Link::End => break,
            Link::Invalid => return None,
            Link::Combinator(combinator) => combinators.push(combinator),
        }
    }

    let mut parts = Vec::with_capacity(compounds.len());
    for (index, compound) in compounds.into_iter().enumerate().rev() {
        parts.push(SelectorPart {
            compound,
            // The combinator to this compound's left, which is how to
            // reach the next part outward.
            combinator: index.checked_sub(1).map(|at| combinators[at]),
        });
    }
    Some(Selector {
        parts,
        pseudo_element,
    })
}

/// What follows a compound selector.
enum Link {
    Combinator(Combinator),
    End,
    Invalid,
}

/// Read the combinator after a compound, if any.
fn next_combinator(cursor: &mut Cursor<'_>) -> Link {
    let had_space = cursor.skip_whitespace();
    match cursor.peek().and_then(as_delim) {
        Some('>') => {
            cursor.advance();
            Link::Combinator(Combinator::Child)
        }
        Some('+') => {
            cursor.advance();
            Link::Combinator(Combinator::NextSibling)
        }
        Some('~') => {
            cursor.advance();
            Link::Combinator(Combinator::SubsequentSibling)
        }
        _ => {
            if cursor.peek().is_none() {
                Link::End
            } else if had_space {
                // Whitespace between compounds *is* the descendant
                // combinator — the one combinator you can write by
                // accident, and the one you cannot omit.
                Link::Combinator(Combinator::Descendant)
            } else {
                // Two compounds with nothing between them: unparseable.
                Link::Invalid
            }
        }
    }
}

/// Parse one compound selector, plus a trailing pseudo-element.
fn parse_compound(cursor: &mut Cursor<'_>) -> Option<(Compound, Option<PseudoElement>)> {
    let mut compound = Compound::default();
    let mut pseudo_element = None;
    let mut saw_anything = false;

    loop {
        // Nothing may follow a pseudo-element within a compound.
        if pseudo_element.is_some() {
            break;
        }
        let Some(value) = cursor.peek() else { break };
        match value {
            ComponentValue::Token(CssToken::Ident(name)) => {
                // A type selector is only legal first: `p.x` is fine,
                // `.x p` is two compounds, and `.xp` is one class.
                if saw_anything {
                    return None;
                }
                compound.tag = Some(name.to_ascii_lowercase());
                cursor.advance();
            }
            ComponentValue::Token(CssToken::Hash {
                value: id,
                id: true,
            }) => {
                compound.id = Some(id.clone());
                cursor.advance();
            }
            ComponentValue::Token(CssToken::Delim('*')) => {
                if saw_anything {
                    return None;
                }
                cursor.advance();
            }
            ComponentValue::Token(CssToken::Delim('.')) => {
                cursor.advance();
                let Some(ComponentValue::Token(CssToken::Ident(name))) = cursor.advance() else {
                    return None;
                };
                compound.classes.push(name.clone());
            }
            ComponentValue::Block {
                kind: BlockKind::Square,
                values,
            } => {
                compound.attributes.push(parse_attribute(values)?);
                cursor.advance();
            }
            ComponentValue::Token(CssToken::Colon) => match parse_pseudo(cursor)? {
                Pseudo::Class(class) => compound.pseudo_classes.push(class),
                Pseudo::Element(element) => pseudo_element = Some(element),
            },
            _ => break,
        }
        saw_anything = true;
    }

    if !saw_anything {
        return None;
    }
    Some((compound, pseudo_element))
}

enum Pseudo {
    Class(PseudoClass),
    Element(PseudoElement),
}

/// Parse a `:pseudo` or `::pseudo`, the first colon still unread.
fn parse_pseudo(cursor: &mut Cursor<'_>) -> Option<Pseudo> {
    cursor.advance();
    let double_colon = cursor.peek() == Some(&ComponentValue::Token(CssToken::Colon));
    if double_colon {
        cursor.advance();
    }

    match cursor.advance() {
        Some(ComponentValue::Token(CssToken::Ident(name))) => {
            let lowered = name.to_ascii_lowercase();
            if double_colon {
                return pseudo_element(&lowered).map(Pseudo::Element);
            }
            // `:before` and `:after` predate `::`, so both spellings
            // have to work — pages in the wild use each.
            if let Some(element) = legacy_pseudo_element(&lowered) {
                return Some(Pseudo::Element(element));
            }
            simple_pseudo_class(&lowered).map(Pseudo::Class)
        }
        Some(ComponentValue::Function { name, arguments }) => {
            if double_colon {
                return None;
            }
            functional_pseudo_class(&name.to_ascii_lowercase(), arguments).map(Pseudo::Class)
        }
        _ => None,
    }
}

fn pseudo_element(name: &str) -> Option<PseudoElement> {
    Some(match name {
        "before" => PseudoElement::Before,
        "after" => PseudoElement::After,
        "first-line" => PseudoElement::FirstLine,
        "first-letter" => PseudoElement::FirstLetter,
        "marker" => PseudoElement::Marker,
        "placeholder" => PseudoElement::Placeholder,
        "selection" => PseudoElement::Selection,
        _ => return None,
    })
}

/// The four pseudo-elements that may also be written with one colon.
fn legacy_pseudo_element(name: &str) -> Option<PseudoElement> {
    match name {
        "before" | "after" | "first-line" | "first-letter" => pseudo_element(name),
        _ => None,
    }
}

/// An unknown pseudo-class returns `None`, which discards the rule —
/// the behaviour that lets a selector act as a feature test.
fn simple_pseudo_class(name: &str) -> Option<PseudoClass> {
    Some(match name {
        "first-child" => PseudoClass::FirstChild,
        "last-child" => PseudoClass::LastChild,
        "only-child" => PseudoClass::OnlyChild,
        "first-of-type" => PseudoClass::FirstOfType,
        "last-of-type" => PseudoClass::LastOfType,
        "only-of-type" => PseudoClass::OnlyOfType,
        "empty" => PseudoClass::Empty,
        "root" => PseudoClass::Root,
        "host" => PseudoClass::Host,
        "link" | "any-link" => PseudoClass::Link,
        "hover" => PseudoClass::Hover,
        "focus" => PseudoClass::Focus,
        "active" => PseudoClass::Active,
        "checked" => PseudoClass::Checked,
        "disabled" => PseudoClass::Disabled,
        "enabled" => PseudoClass::Enabled,
        _ => return None,
    })
}

fn functional_pseudo_class(name: &str, arguments: &[ComponentValue]) -> Option<PseudoClass> {
    Some(match name {
        "nth-child" => PseudoClass::NthChild(parse_nth(arguments)?),
        "nth-last-child" => PseudoClass::NthLastChild(parse_nth(arguments)?),
        "nth-of-type" => PseudoClass::NthOfType(parse_nth(arguments)?),
        "nth-last-of-type" => PseudoClass::NthLastOfType(parse_nth(arguments)?),
        "not" => PseudoClass::Not(parse_selector_list(arguments)?),
        "is" | "matches" | "any" => PseudoClass::Is(parse_selector_list(arguments)?),
        "where" => PseudoClass::Where(parse_selector_list(arguments)?),
        _ => return None,
    })
}

/// Parse an attribute selector's contents, the brackets already stripped.
fn parse_attribute(values: &[ComponentValue]) -> Option<AttributeSelector> {
    let tokens = bare_tokens(values);
    let (name, rest) = match tokens.split_first() {
        Some((CssToken::Ident(name), rest)) => (name.to_ascii_lowercase(), rest),
        _ => return None,
    };

    if rest.is_empty() {
        return Some(AttributeSelector {
            name,
            match_kind: AttrMatch::Exists,
            case_insensitive: false,
        });
    }

    // The operators other than `=` are two tokens: `^` then `=`.
    let (operator, rest) = match rest.split_first() {
        Some((CssToken::Delim('='), rest)) => ('=', rest),
        Some((CssToken::Delim(c @ ('~' | '|' | '^' | '$' | '*')), rest)) => {
            match rest.split_first() {
                Some((CssToken::Delim('='), rest)) => (*c, rest),
                _ => return None,
            }
        }
        _ => return None,
    };

    let (value, rest) = match rest.split_first() {
        Some((CssToken::Str(text), rest)) => (text.clone(), rest),
        Some((CssToken::Ident(text), rest)) => (text.clone(), rest),
        _ => return None,
    };

    // A trailing `i` or `s` sets case sensitivity explicitly.
    let case_insensitive = match rest {
        [] => false,
        [CssToken::Ident(flag)] if flag.eq_ignore_ascii_case("i") => true,
        [CssToken::Ident(flag)] if flag.eq_ignore_ascii_case("s") => false,
        _ => return None,
    };

    let match_kind = match operator {
        '=' => AttrMatch::Equals(value),
        '~' => AttrMatch::Includes(value),
        '|' => AttrMatch::DashMatch(value),
        '^' => AttrMatch::Prefix(value),
        '$' => AttrMatch::Suffix(value),
        '*' => AttrMatch::Substring(value),
        _ => return None,
    };
    Some(AttributeSelector {
        name,
        match_kind,
        case_insensitive,
    })
}

/// Plain tokens with whitespace dropped and nested values discarded.
///
/// Safe for attribute selectors and `an+b`: neither may contain a
/// function or block, and in both the tokenizer has already split what
/// whitespace was separating.
fn bare_tokens(values: &[ComponentValue]) -> Vec<CssToken> {
    values
        .iter()
        .filter_map(|value| match value {
            ComponentValue::Token(CssToken::Whitespace) => None,
            ComponentValue::Token(token) => Some(token.clone()),
            _ => None,
        })
        .collect()
}

/// Parse an `an+b` argument.
fn parse_nth(arguments: &[ComponentValue]) -> Option<Nth> {
    nth_from_tokens(&bare_tokens(arguments))
}

/// Parse `an+b` from its tokens.
///
/// The offset reaches us in one of two shapes depending on spacing:
/// `2n+1` makes the `+` the number's sign, so it arrives as one signed
/// number, while `2n + 1` makes it a delimiter of its own. Both are
/// legal, so both are handled — and `2n-1` arrives as neither, because
/// the tokenizer folds it into the *unit*.
fn nth_from_tokens(tokens: &[CssToken]) -> Option<Nth> {
    // A leading `+` that was not absorbed as a sign.
    if let [CssToken::Delim('+'), rest @ ..] = tokens {
        return nth_from_tokens(rest);
    }
    match tokens {
        [CssToken::Ident(word)] if word.eq_ignore_ascii_case("odd") => {
            Some(Nth { step: 2, offset: 1 })
        }
        [CssToken::Ident(word)] if word.eq_ignore_ascii_case("even") => {
            Some(Nth { step: 2, offset: 0 })
        }
        // A bare integer: only that one index, no step.
        [CssToken::Number {
            value,
            integer: true,
        }] => Some(Nth {
            step: 0,
            offset: *value as i32,
        }),
        [first, rest @ ..] => {
            let (step, swallowed) = parse_step(first)?;
            let trailing = parse_offset(rest)?;
            let offset = match (swallowed, trailing) {
                (Some(offset), None) | (None, Some(offset)) => offset,
                (None, None) => 0,
                // `2n-1 +2`: the offset was given twice.
                (Some(_), Some(_)) => return None,
            };
            Some(Nth { step, offset })
        }
        [] => None,
    }
}

/// The `an` part: its step, plus an offset the token swallowed.
fn parse_step(token: &CssToken) -> Option<(i32, Option<i32>)> {
    let (step, tail) = match token {
        // `2n`, `2n-1` — the unit is `n` or `n-<digits>`.
        CssToken::Dimension { value, unit } => (*value as i32, strip_prefix_ci(unit, "n")?),
        // `n`, `-n`, `n-1`, `-n-1`.
        CssToken::Ident(word) => match strip_prefix_ci(word, "-n") {
            Some(tail) => (-1, tail),
            None => (1, strip_prefix_ci(word, "n")?),
        },
        _ => return None,
    };
    if tail.is_empty() {
        return Some((step, None));
    }
    Some((step, Some(negative_digits(tail)?)))
}

/// `-12` from a unit or ident tail. Anything else is not an offset, so
/// `2nx` is rejected rather than read as a step of two.
fn negative_digits(tail: &str) -> Option<i32> {
    let digits = tail.strip_prefix('-')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(-digits.parse::<i32>().ok()?)
}

/// The trailing `b`, written as a signed number or as a sign plus one.
fn parse_offset(tokens: &[CssToken]) -> Option<Option<i32>> {
    match tokens {
        [] => Some(None),
        [CssToken::Number {
            value,
            integer: true,
        }] => Some(Some(*value as i32)),
        [CssToken::Delim('+'), CssToken::Number {
            value,
            integer: true,
        }] => Some(Some(*value as i32)),
        [CssToken::Delim('-'), CssToken::Number {
            value,
            integer: true,
        }] => Some(Some(-(*value as i32))),
        _ => None,
    }
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

/// A cursor over component values.
struct Cursor<'a> {
    values: &'a [ComponentValue],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(values: &'a [ComponentValue]) -> Self {
        Self { values, pos: 0 }
    }

    fn peek(&self) -> Option<&'a ComponentValue> {
        self.values.get(self.pos)
    }

    fn advance(&mut self) -> Option<&'a ComponentValue> {
        let value = self.values.get(self.pos);
        if value.is_some() {
            self.pos += 1;
        }
        value
    }

    /// Skip whitespace, reporting whether there was any — the caller
    /// needs to know, because whitespace is a combinator.
    fn skip_whitespace(&mut self) -> bool {
        let start = self.pos;
        while self.peek() == Some(&ComponentValue::Token(CssToken::Whitespace)) {
            self.pos += 1;
        }
        self.pos != start
    }
}

fn as_delim(value: &ComponentValue) -> Option<char> {
    match value {
        ComponentValue::Token(CssToken::Delim(c)) => Some(*c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::{parse, Rule};
    use crate::engine::cryo_impl::css::selector::Specificity;

    /// Parse the selector list of `selector { }`.
    fn selectors(selector: &str) -> Option<SelectorList> {
        let sheet = parse(&format!("{selector} {{ color: red }}"));
        let Some(Rule::Style(rule)) = sheet.rules.first() else {
            return None;
        };
        parse_selector_list(&rule.prelude)
    }

    fn one(selector: &str) -> Selector {
        let list = selectors(selector).unwrap_or_else(|| panic!("{selector} should parse"));
        assert_eq!(list.selectors.len(), 1, "expected a single selector");
        list.selectors.into_iter().next().expect("just checked")
    }

    fn specificity(selector: &str) -> Specificity {
        one(selector).specificity()
    }

    #[test]
    fn a_type_selector_parses() {
        let selector = one("p");
        assert_eq!(selector.parts.len(), 1);
        assert_eq!(selector.parts[0].compound.tag.as_deref(), Some("p"));
    }

    #[test]
    fn type_selectors_fold_case_but_classes_and_ids_do_not() {
        assert_eq!(one("DIV").parts[0].compound.tag.as_deref(), Some("div"));
        assert_eq!(one(".Card").parts[0].compound.classes, ["Card"]);
        assert_eq!(one("#Main").parts[0].compound.id.as_deref(), Some("Main"));
    }

    #[test]
    fn a_compound_collects_every_piece() {
        let compound = one("a.link.big#main[href]:hover").parts[0].compound.clone();
        assert_eq!(compound.tag.as_deref(), Some("a"));
        assert_eq!(compound.id.as_deref(), Some("main"));
        assert_eq!(compound.classes, ["link", "big"]);
        assert_eq!(compound.attributes.len(), 1);
        assert_eq!(compound.pseudo_classes, [PseudoClass::Hover]);
    }

    #[test]
    fn the_universal_selector_has_no_tag() {
        assert_eq!(one("*").parts[0].compound.tag, None);
        assert_eq!(specificity("*"), Specificity::new(0, 0, 0));
    }

    #[test]
    fn parts_are_stored_innermost_first() {
        // `div p > a` matches from the `a` outward.
        let selector = one("div p > a");
        let tags: Vec<_> = selector
            .parts
            .iter()
            .map(|part| part.compound.tag.clone().unwrap_or_default())
            .collect();
        assert_eq!(tags, ["a", "p", "div"]);
        assert_eq!(selector.parts[0].combinator, Some(Combinator::Child));
        assert_eq!(selector.parts[1].combinator, Some(Combinator::Descendant));
        assert_eq!(selector.parts[2].combinator, None);
    }

    #[test]
    fn every_combinator_parses() {
        assert_eq!(one("a > b").parts[0].combinator, Some(Combinator::Child));
        assert_eq!(
            one("a + b").parts[0].combinator,
            Some(Combinator::NextSibling)
        );
        assert_eq!(
            one("a ~ b").parts[0].combinator,
            Some(Combinator::SubsequentSibling)
        );
        assert_eq!(one("a b").parts[0].combinator, Some(Combinator::Descendant));
    }

    #[test]
    fn combinators_need_no_surrounding_space() {
        assert_eq!(one("a>b").parts[0].combinator, Some(Combinator::Child));
        assert_eq!(one("a+b").parts.len(), 2);
    }

    #[test]
    fn a_selector_list_keeps_every_selector() {
        let list = selectors("h1, h2, h3").expect("parses");
        assert_eq!(list.selectors.len(), 3);
    }

    #[test]
    fn attribute_selectors_parse_every_operator() {
        let cases = [
            ("[href]", AttrMatch::Exists),
            ("[href=\"x\"]", AttrMatch::Equals("x".into())),
            ("[class~=\"x\"]", AttrMatch::Includes("x".into())),
            ("[lang|=\"en\"]", AttrMatch::DashMatch("en".into())),
            ("[href^=\"https\"]", AttrMatch::Prefix("https".into())),
            ("[src$=\".png\"]", AttrMatch::Suffix(".png".into())),
            ("[title*=\"mid\"]", AttrMatch::Substring("mid".into())),
        ];
        for (text, want) in cases {
            let compound = one(&format!("a{text}")).parts[0].compound.clone();
            assert_eq!(compound.attributes[0].match_kind, want, "{text}");
        }
    }

    #[test]
    fn an_unquoted_attribute_value_is_allowed() {
        let compound = one("[type=text]").parts[0].compound.clone();
        assert_eq!(
            compound.attributes[0].match_kind,
            AttrMatch::Equals("text".into())
        );
    }

    #[test]
    fn the_case_insensitive_flag_parses() {
        let sensitive = one("[href=\"X\"]").parts[0].compound.clone();
        assert!(!sensitive.attributes[0].case_insensitive);
        let insensitive = one("[href=\"X\" i]").parts[0].compound.clone();
        assert!(insensitive.attributes[0].case_insensitive);
    }

    #[test]
    fn attribute_names_fold_case() {
        let compound = one("[HREF]").parts[0].compound.clone();
        assert_eq!(compound.attributes[0].name, "href");
    }

    #[test]
    fn pseudo_elements_parse_with_one_colon_or_two() {
        assert_eq!(one("p::before").pseudo_element, Some(PseudoElement::Before));
        assert_eq!(one("p:before").pseudo_element, Some(PseudoElement::Before));
        assert_eq!(
            one("p::first-line").pseudo_element,
            Some(PseudoElement::FirstLine)
        );
        assert_eq!(
            one("p::selection").pseudo_element,
            Some(PseudoElement::Selection)
        );
    }

    #[test]
    fn a_double_colon_pseudo_class_is_invalid() {
        assert!(selectors("p::hover").is_none());
    }

    #[test]
    fn a_single_colon_only_works_for_the_legacy_four() {
        assert!(selectors("p:selection").is_none());
        assert_eq!(one("p:after").pseudo_element, Some(PseudoElement::After));
    }

    #[test]
    fn nth_child_parses_every_spelling() {
        let cases = [
            ("odd", Nth { step: 2, offset: 1 }),
            ("even", Nth { step: 2, offset: 0 }),
            ("3", Nth { step: 0, offset: 3 }),
            ("2n", Nth { step: 2, offset: 0 }),
            ("2n+1", Nth { step: 2, offset: 1 }),
            (
                "2n-1",
                Nth {
                    step: 2,
                    offset: -1,
                },
            ),
            ("n", Nth { step: 1, offset: 0 }),
            ("n+2", Nth { step: 1, offset: 2 }),
            (
                "n-2",
                Nth {
                    step: 1,
                    offset: -2,
                },
            ),
            (
                "-n+3",
                Nth {
                    step: -1,
                    offset: 3,
                },
            ),
            (
                "-n-3",
                Nth {
                    step: -1,
                    offset: -3,
                },
            ),
            ("+2n+1", Nth { step: 2, offset: 1 }),
        ];
        for (argument, want) in cases {
            let compound = one(&format!("p:nth-child({argument})")).parts[0]
                .compound
                .clone();
            assert_eq!(
                compound.pseudo_classes,
                [PseudoClass::NthChild(want)],
                "nth-child({argument})"
            );
        }
    }

    #[test]
    fn nth_accepts_whitespace_around_its_parts() {
        let compound = one("p:nth-child( 2n + 1 )").parts[0].compound.clone();
        assert_eq!(
            compound.pseudo_classes,
            [PseudoClass::NthChild(Nth { step: 2, offset: 1 })]
        );
    }

    #[test]
    fn a_malformed_nth_invalidates_the_rule() {
        assert!(selectors("p:nth-child(2x)").is_none());
        assert!(selectors("p:nth-child()").is_none());
        assert!(selectors("p:nth-child(1.5)").is_none());
    }

    #[test]
    fn the_other_nth_pseudo_classes_parse() {
        let three = Nth { step: 0, offset: 3 };
        assert_eq!(
            one("p:nth-last-child(3)").parts[0].compound.pseudo_classes,
            [PseudoClass::NthLastChild(three)]
        );
        assert_eq!(
            one("p:nth-of-type(3)").parts[0].compound.pseudo_classes,
            [PseudoClass::NthOfType(three)]
        );
        assert_eq!(
            one("p:nth-last-of-type(3)").parts[0]
                .compound
                .pseudo_classes,
            [PseudoClass::NthLastOfType(three)]
        );
    }

    #[test]
    fn not_and_is_take_selector_lists() {
        let compound = one("p:not(.a, .b)").parts[0].compound.clone();
        let [PseudoClass::Not(inner)] = compound.pseudo_classes.as_slice() else {
            panic!("expected :not, got {:?}", compound.pseudo_classes);
        };
        assert_eq!(inner.selectors.len(), 2);

        let compound = one("p:is(h1 > span)").parts[0].compound.clone();
        assert!(matches!(
            compound.pseudo_classes.as_slice(),
            [PseudoClass::Is(_)]
        ));
    }

    #[test]
    fn an_unknown_pseudo_class_invalidates_the_rule() {
        // The feature-test behaviour: the rule must not half-apply.
        assert!(selectors("p:focus-visible").is_none());
        assert!(selectors("p:bogus").is_none());
    }

    #[test]
    fn one_bad_selector_invalidates_the_whole_list() {
        assert!(
            selectors("h1, h2:bogus, h3").is_none(),
            "an invalid member must discard the entire rule"
        );
    }

    #[test]
    fn host_parses_so_the_rule_beside_it_survives() {
        // `:root, :host { … }` is how a design system declares its
        // custom properties. Rejecting `:host` takes `:root` down
        // with it — and with it every colour, spacing step and font
        // the page owns.
        assert!(selectors(":host").is_some());
        let list = selectors(":root, :host").expect("both members parse");
        assert_eq!(list.selectors.len(), 2);
    }

    #[test]
    fn a_type_selector_after_anything_else_is_invalid() {
        assert!(selectors(".a p").is_some(), "that is two compounds");
        assert!(selectors("[href]p").is_none(), "but this is not");
    }

    #[test]
    fn specificity_counts_each_kind() {
        assert_eq!(specificity("p"), Specificity::new(0, 0, 1));
        assert_eq!(specificity(".card"), Specificity::new(0, 1, 0));
        assert_eq!(specificity("#main"), Specificity::new(1, 0, 0));
        assert_eq!(specificity("[href]"), Specificity::new(0, 1, 0));
        assert_eq!(specificity("p:hover"), Specificity::new(0, 1, 1));
        assert_eq!(specificity("div p > a"), Specificity::new(0, 0, 3));
        assert_eq!(
            specificity("#main .card a[href]:hover"),
            Specificity::new(1, 3, 1)
        );
    }

    #[test]
    fn a_pseudo_element_counts_as_an_element() {
        assert_eq!(specificity("p::before"), Specificity::new(0, 0, 2));
    }

    #[test]
    fn not_takes_its_arguments_specificity_and_where_takes_none() {
        assert_eq!(specificity("p:not(#main)"), Specificity::new(1, 0, 1));
        assert_eq!(specificity("p:is(.a, #b)"), Specificity::new(1, 0, 1));
        assert_eq!(specificity("p:where(#main)"), Specificity::new(0, 0, 1));
    }

    #[test]
    fn an_empty_prelude_does_not_parse() {
        assert!(parse_selector_list(&[]).is_none());
    }
}
