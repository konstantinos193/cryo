//! Media queries: does this `@media` block apply?
//!
//! A query list is `<query> [, <query>]*` and matches if *any* query
//! does — a comma is `or`. One query is an optional `not`/`only`, an
//! optional media type, and any number of `and`-joined features in
//! parentheses.
//!
//! ```text
//!   @media screen and (min-width: 40em), print { ... }
//!          ╰────╯     ╰───────────────╯   ╰───╯
//!           type           feature        another query
//! ```
//!
//! Getting this wrong is not cosmetic. Every responsive stylesheet
//! written since about 2012 puts its real layout behind `min-width`,
//! so a parser that answers "no" to every query renders the phone
//! version of the web at 1920 pixels — and one that answers "yes" to
//! every query renders all of them at once, last rule winning.
//!
//! Known gaps, stated rather than hidden:
//! - Only the features below are understood. An unknown feature makes
//!   its query false, which is what the spec says to do.
//! - `prefers-color-scheme` answers `light`, since there is no setting
//!   to read yet.
//! - Range syntax (`width >= 40em`) is not parsed; the `min-`/`max-`
//!   prefixes are.

use super::parser::{BlockKind, ComponentValue};
use super::tokenizer::CssToken;
use super::value::{parse_length_percentage, LengthContext, LengthPercentage};

/// What a media query is asked about.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    /// The size assumed before anyone says otherwise.
    ///
    /// A stylesheet has to be evaluated against *something*, and a
    /// desktop width is the honest default for a desktop browser: it
    /// is what the page would be laid out at.
    pub const DEFAULT: Self = Self {
        width: 1280.0,
        height: 800.0,
    };
}

impl Default for Viewport {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Whether a `@media` prelude matches this viewport.
///
/// An empty prelude (`@media { }`) matches: with nothing to ask, there
/// is nothing to fail.
pub fn matches(prelude: &[ComponentValue], viewport: Viewport) -> bool {
    let queries = split_on_commas(prelude);
    if queries.is_empty() {
        return true;
    }
    queries.iter().any(|query| query_matches(query, viewport))
}

/// One comma-separated query.
fn query_matches(query: &[&ComponentValue], viewport: Viewport) -> bool {
    let mut negated = false;
    let mut result = true;
    let mut seen_type = false;

    for value in query {
        match value {
            ComponentValue::Token(CssToken::Whitespace) => {}
            ComponentValue::Token(CssToken::Ident(word)) => {
                if word.eq_ignore_ascii_case("not") && !seen_type {
                    negated = true;
                } else if word.eq_ignore_ascii_case("and") || word.eq_ignore_ascii_case("only") {
                    // `and` joins, `only` is a legacy no-op.
                } else {
                    // A media type. `all` and `screen` are us; `print`
                    // and the rest are not.
                    seen_type = true;
                    result &=
                        word.eq_ignore_ascii_case("all") || word.eq_ignore_ascii_case("screen");
                }
            }
            ComponentValue::Block {
                kind: BlockKind::Paren,
                values,
            } => result &= feature_matches(values, viewport),
            // Anything else in a query is a syntax we do not read, and
            // an unreadable query does not match.
            _ => result = false,
        }
    }
    result != negated
}

/// One parenthesised feature: `(min-width: 40em)`.
fn feature_matches(values: &[ComponentValue], viewport: Viewport) -> bool {
    let mut parts = values.split(|value| matches!(value, ComponentValue::Token(CssToken::Colon)));
    let Some(name) = parts.next().and_then(single_ident) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    let Some(rest) = parts.next() else {
        // A boolean feature: `(color)` asks whether it is non-zero.
        return matches!(name.as_str(), "color" | "any-pointer" | "pointer");
    };

    match name.as_str() {
        "width" | "min-width" | "max-width" => compare(&name, rest, viewport.width, viewport),
        "height" | "min-height" | "max-height" => compare(&name, rest, viewport.height, viewport),
        "orientation" => single_ident(rest).is_some_and(|word| {
            let landscape = viewport.width >= viewport.height;
            word.eq_ignore_ascii_case(if landscape { "landscape" } else { "portrait" })
        }),
        // No setting to read, so the default scheme is the answer.
        "prefers-color-scheme" => {
            single_ident(rest).is_some_and(|word| word.eq_ignore_ascii_case("light"))
        }
        "prefers-reduced-motion" => {
            single_ident(rest).is_some_and(|word| word.eq_ignore_ascii_case("no-preference"))
        }
        // Every modern browser passes these; answering no would hide
        // the hover styles a desktop page is written around.
        "hover" => single_ident(rest).is_some_and(|word| word.eq_ignore_ascii_case("hover")),
        "pointer" => single_ident(rest).is_some_and(|word| word.eq_ignore_ascii_case("fine")),
        _ => false,
    }
}

/// Compare a `min-`/`max-`/exact feature against the viewport.
fn compare(name: &str, values: &[ComponentValue], actual: f32, viewport: Viewport) -> bool {
    let Some(wanted) = length_px(values, viewport) else {
        return false;
    };
    if name.starts_with("min-") {
        actual >= wanted
    } else if name.starts_with("max-") {
        actual <= wanted
    } else {
        (actual - wanted).abs() < 0.5
    }
}

/// A length in pixels.
///
/// A media query has no element, so `em` is the initial font size
/// rather than a computed one — there is nothing else it could mean.
/// A percentage is not valid in a media feature, so it is rejected.
fn length_px(values: &[ComponentValue], viewport: Viewport) -> Option<f32> {
    const INITIAL_FONT_SIZE: f32 = 16.0;
    let context = LengthContext {
        font_size: INITIAL_FONT_SIZE,
        root_font_size: INITIAL_FONT_SIZE,
        viewport_width: viewport.width,
        viewport_height: viewport.height,
    };
    match parse_length_percentage(values)? {
        LengthPercentage::Length(length) => Some(length.to_px(&context)),
        LengthPercentage::Percentage(_) => None,
    }
}

/// The single identifier in a run of values, ignoring whitespace.
fn single_ident(values: &[ComponentValue]) -> Option<&str> {
    let mut found = None;
    for value in values {
        match value {
            ComponentValue::Token(CssToken::Whitespace) => {}
            ComponentValue::Token(CssToken::Ident(word)) => {
                if found.is_some() {
                    return None;
                }
                found = Some(word.as_str());
            }
            _ => return None,
        }
    }
    found
}

/// Split a prelude on top-level commas.
fn split_on_commas(prelude: &[ComponentValue]) -> Vec<Vec<&ComponentValue>> {
    let mut out = vec![Vec::new()];
    for value in prelude {
        if matches!(value, ComponentValue::Token(CssToken::Comma)) {
            out.push(Vec::new());
        } else {
            out.last_mut()
                .expect("one query is always open")
                .push(value);
        }
    }
    out.retain(|query| {
        query
            .iter()
            .any(|value| !matches!(value, ComponentValue::Token(CssToken::Whitespace)))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::parse;
    use crate::engine::cryo_impl::css::parser::Rule;

    /// Evaluate the query of `@media <query> { }` at `width` x 800.
    fn at(query: &str, width: f32) -> bool {
        let sheet = parse(&format!("@media {query} {{ p {{ color: red }} }}"));
        let Some(Rule::At(at_rule)) = sheet.rules.first() else {
            panic!("not an at-rule");
        };
        matches(
            &at_rule.prelude,
            Viewport {
                width,
                height: 800.0,
            },
        )
    }

    #[test]
    fn a_min_width_holds_at_and_above_it() {
        assert!(!at("(min-width: 768px)", 767.0));
        assert!(at("(min-width: 768px)", 768.0));
        assert!(at("(min-width: 768px)", 1920.0));
    }

    #[test]
    fn a_max_width_holds_at_and_below_it() {
        assert!(at("(max-width: 768px)", 768.0));
        assert!(!at("(max-width: 768px)", 769.0));
    }

    #[test]
    fn em_in_a_query_is_the_initial_font_size() {
        // 40em is 640px, whatever the page's own font size is.
        assert!(!at("(min-width: 40em)", 639.0));
        assert!(at("(min-width: 40em)", 640.0));
    }

    #[test]
    fn screen_and_all_match_but_print_does_not() {
        assert!(at("screen", 1000.0));
        assert!(at("all", 1000.0));
        assert!(!at("print", 1000.0));
    }

    #[test]
    fn and_requires_both() {
        assert!(at("screen and (min-width: 500px)", 1000.0));
        assert!(!at("screen and (min-width: 5000px)", 1000.0));
        assert!(!at("print and (min-width: 500px)", 1000.0));
    }

    #[test]
    fn a_comma_is_or() {
        assert!(at("print, screen", 1000.0));
        assert!(at("(max-width: 100px), (min-width: 900px)", 1000.0));
        assert!(!at("(max-width: 100px), (min-width: 5000px)", 1000.0));
    }

    #[test]
    fn not_inverts_the_whole_query() {
        assert!(!at("not screen", 1000.0));
        assert!(at("not print", 1000.0));
        assert!(!at("not screen and (min-width: 500px)", 1000.0));
    }

    #[test]
    fn only_is_ignored() {
        assert!(at("only screen and (min-width: 500px)", 1000.0));
    }

    #[test]
    fn an_unknown_feature_does_not_match() {
        // The spec's rule, and the safe one: a query we cannot read
        // must not switch a layout on.
        assert!(!at("(min-resolution: 2dppx)", 1000.0));
        assert!(!at("(grid: 1)", 1000.0));
    }

    #[test]
    fn orientation_follows_the_shape_of_the_viewport() {
        assert!(at("(orientation: landscape)", 1000.0));
        assert!(!at("(orientation: portrait)", 1000.0));
        assert!(at("(orientation: portrait)", 400.0));
    }

    #[test]
    fn an_empty_query_matches() {
        assert!(at("", 1000.0));
    }

    #[test]
    fn a_zero_length_needs_no_unit() {
        assert!(at("(min-width: 0)", 1000.0));
    }
}
