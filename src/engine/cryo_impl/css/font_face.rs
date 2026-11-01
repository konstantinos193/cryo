//! `@font-face`: a font the page brings with it.
//!
//! An ordinary `font-family` names a font and hopes the machine has
//! one by that name. `@font-face` instead *defines* the family, says
//! where to fetch it, and which weights and slants that file covers:
//!
//! ```css
//!   @font-face {
//!     font-family: Inter;
//!     font-style: normal;
//!     font-weight: 400 700;
//!     src: url(/inter.woff2) format("woff2");
//!     unicode-range: U+0000-00FF;
//!   }
//! ```
//!
//! A page typically declares the same family a dozen times over, once
//! per subset, and lets `unicode-range` decide which file a character
//! comes from. None of that matters to layout — the *metrics* are
//! what move text around — so this reads the parts that identify a
//! file and leaves the rest.
//!
//! `src` is a list in preference order, and a `local()` entry asks
//! for a font already installed. Both forms are kept: `local()` costs
//! nothing to try and is how a page avoids a download it does not
//! need.

use super::computed::font;
use super::computed::FontStyle;
use super::parser::{ComponentValue, Declaration};
use super::tokenizer::CssToken;

/// Where one candidate font comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A file to fetch, as written in the stylesheet.
    Url(String),
    /// A family to look for on the machine.
    Local(String),
}

/// One `@font-face` rule, as far as it is read here.
#[derive(Clone, Debug, PartialEq)]
pub struct FontFace {
    pub family: String,
    /// The URL of the stylesheet this rule was written in.
    ///
    /// A `url()` in CSS resolves against the sheet, not the
    /// document — and a toolchain writes font URLs as `../media/x`,
    /// which from the document's own address points nowhere.
    pub base: String,
    /// In preference order; the first that loads wins.
    pub sources: Vec<Source>,
    /// The weights this file covers. A single weight is a range of
    /// one, which is what makes a variable font and a static one the
    /// same shape here.
    pub weight: (u16, u16),
    pub italic: bool,
}

impl FontFace {
    /// Read a rule's declarations, or `None` if it names no family or
    /// no source — in which case there is nothing to load.
    pub fn parse(declarations: &[Declaration], base: &str) -> Option<Self> {
        let mut family = None;
        let mut sources = Vec::new();
        let mut weight = (400u16, 400u16);
        let mut italic = false;

        for declaration in declarations {
            match declaration.name.as_str() {
                "font-family" => family = first_string(&declaration.value),
                "src" => sources = parse_src(&declaration.value),
                "font-weight" => weight = parse_weight_range(&declaration.value),
                "font-style" => {
                    italic = font::parse_style(&declaration.value)
                        .is_some_and(|style| style != FontStyle::Normal);
                }
                _ => {}
            }
        }

        let family = family?;
        if sources.is_empty() {
            return None;
        }
        Some(Self {
            family,
            base: base.to_owned(),
            sources,
            weight,
            italic,
        })
    }

    /// The weight to file this face under.
    ///
    /// A range collapses to its lightest end, because that is the
    /// weight the face is *named* for — a 400-to-700 variable font is
    /// the regular, and asking for bold should still prefer a real
    /// bold file when the page shipped one.
    pub fn nominal_weight(&self) -> u16 {
        self.weight.0
    }
}

/// A family name, quoted or not. An unquoted name may be several
/// identifiers, as in `Instrument Sans`.
fn first_string(values: &[ComponentValue]) -> Option<String> {
    let mut words = Vec::new();
    for value in values {
        match value {
            ComponentValue::Token(CssToken::Str(text)) => return Some(text.clone()),
            ComponentValue::Token(CssToken::Ident(word)) => words.push(word.clone()),
            ComponentValue::Token(CssToken::Whitespace) => {}
            _ => break,
        }
    }
    (!words.is_empty()).then(|| words.join(" "))
}

/// The `src` list, in the order written.
///
/// `format()` and `tech()` hints are skipped rather than used: this
/// engine tries a file and finds out, which is one fewer table to
/// keep in step with what the loader can actually open.
fn parse_src(values: &[ComponentValue]) -> Vec<Source> {
    let mut sources = Vec::new();
    for value in values {
        match value {
            ComponentValue::Token(CssToken::Url(url)) => sources.push(Source::Url(url.clone())),
            ComponentValue::Function { name, arguments } => {
                let lowered = name.to_ascii_lowercase();
                if lowered == "url" {
                    if let Some(url) = first_string(arguments) {
                        sources.push(Source::Url(url));
                    }
                } else if lowered == "local" {
                    if let Some(family) = first_string(arguments) {
                        sources.push(Source::Local(family));
                    }
                }
            }
            _ => {}
        }
    }
    sources
}

/// `font-weight` here may be one number or two, the latter meaning a
/// variable font's range.
fn parse_weight_range(values: &[ComponentValue]) -> (u16, u16) {
    let numbers: Vec<u16> = values
        .iter()
        .filter_map(|value| match value {
            ComponentValue::Token(CssToken::Number { value, .. }) => {
                Some((*value).clamp(1.0, 1000.0) as u16)
            }
            ComponentValue::Token(CssToken::Ident(word)) => {
                match word.to_ascii_lowercase().as_str() {
                    "normal" => Some(400),
                    "bold" => Some(700),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    match numbers[..] {
        [single] => (single, single),
        [low, high, ..] => (low.min(high), low.max(high)),
        [] => (400, 400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::parse_declarations;

    fn face(css: &str) -> Option<FontFace> {
        FontFace::parse(&parse_declarations(css), "https://example.com/a/style.css")
    }

    #[test]
    fn a_rule_yields_its_family_and_source() {
        let face =
            face("font-family: Inter; src: url(/inter.woff2) format(\"woff2\")").expect("a face");
        assert_eq!(face.family, "Inter");
        assert_eq!(face.sources, [Source::Url("/inter.woff2".to_owned())]);
    }

    #[test]
    fn an_unquoted_family_may_be_several_words() {
        let face = face("font-family: Instrument Sans; src: url(a.woff2)").expect("a face");
        assert_eq!(face.family, "Instrument Sans");
    }

    #[test]
    fn a_quoted_family_keeps_its_spacing() {
        let face = face("font-family: \"JetBrains Mono\"; src: url(a.woff2)").expect("a face");
        assert_eq!(face.family, "JetBrains Mono");
    }

    #[test]
    fn sources_are_kept_in_preference_order() {
        let face = face(
            "font-family: X; src: local(\"Arial\"), url(a.woff2) format(\"woff2\"), url(b.ttf)",
        )
        .expect("a face");
        assert_eq!(
            face.sources,
            [
                Source::Local("Arial".to_owned()),
                Source::Url("a.woff2".to_owned()),
                Source::Url("b.ttf".to_owned()),
            ]
        );
    }

    #[test]
    fn a_weight_range_is_a_variable_fonts_two_ends() {
        let face = face("font-family: X; src: url(a); font-weight: 400 700").expect("a face");
        assert_eq!(face.weight, (400, 700));
        assert_eq!(face.nominal_weight(), 400);
    }

    #[test]
    fn a_single_weight_is_a_range_of_one() {
        let face = face("font-family: X; src: url(a); font-weight: 600").expect("a face");
        assert_eq!(face.weight, (600, 600));
    }

    #[test]
    fn the_weight_keywords_still_work() {
        let face = face("font-family: X; src: url(a); font-weight: bold").expect("a face");
        assert_eq!(face.weight, (700, 700));
    }

    #[test]
    fn an_italic_face_says_so() {
        assert!(
            face("font-family: X; src: url(a); font-style: italic")
                .expect("a face")
                .italic
        );
        assert!(
            !face("font-family: X; src: url(a)").expect("a face").italic,
            "upright by default"
        );
    }

    #[test]
    fn a_face_remembers_the_sheet_it_came_from() {
        let face = face("font-family: X; src: url(../media/x.woff2)").expect("a face");
        assert_eq!(face.base, "https://example.com/a/style.css");
    }

    #[test]
    fn a_rule_with_nothing_to_load_is_not_a_face() {
        assert!(face("font-family: X").is_none(), "no source");
        assert!(face("src: url(a.woff2)").is_none(), "no family");
    }
}
