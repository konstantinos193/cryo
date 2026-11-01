//! Font properties, which are the awkward ones because they resolve
//! against the *parent* rather than the element.
//!
//! Three traps live here:
//!
//! 1. **`font-size: 1.5em` is relative to the parent's font size**, not
//!    the element's — the element's is what is being computed. Every
//!    other property's `em` uses the element's own size. So
//!    [`parse_size`] takes the parent size explicitly and the caller
//!    must resolve `font-size` before anything else.
//! 2. **`line-height: 1.5` inherits as the number, not as pixels.** A
//!    child with a bigger font gets a proportionally bigger line box. If
//!    it were resolved to pixels at compute time the child would inherit
//!    the parent's line height and text would collide. `line-height:
//!    150%`, by contrast, *does* resolve and inherit as a length — same
//!    ratio, different inheritance. Hence [`LineHeight`] keeping them
//!    apart.
//! 3. **`font-weight: bolder` is relative too**, and not by a fixed
//!    step: the spec gives a table, so `bolder` on a 400 parent is 700
//!    but on an 800 parent is unchanged.
//!
//! `line-height: normal` is approximated at 1.2 times the font size.
//! The real value comes from the font's own ascent, descent and line
//! gap, which needs a loaded font — so this is a stated placeholder that
//! becomes exact once text shaping exists.

use super::keyword::FontStyle;
use crate::engine::cryo_impl::css::parser::ComponentValue;
use crate::engine::cryo_impl::css::tokenizer::CssToken;
use crate::engine::cryo_impl::css::value::{
    parse_length_percentage, single_token, LengthContext, LengthPercentage,
};

/// The initial font size, matching `font-size: medium`.
pub const INITIAL_FONT_SIZE: f32 = 16.0;

/// The initial weight — `normal`, as a number, since the numeric and
/// keyword forms are the same property.
pub const INITIAL_FONT_WEIGHT: u16 = 400;

/// What `line-height: normal` is approximated as. See the module docs.
const NORMAL_LINE_HEIGHT_FACTOR: f32 = 1.2;

/// The ratio each absolute size keyword bears to `medium`.
///
/// Ratios rather than fixed pixel values so that a user who raises the
/// default font size scales all of them, which is the point of the
/// keywords existing.
const ABSOLUTE_SIZES: &[(&str, f32)] = &[
    ("xx-small", 0.5625),
    ("x-small", 0.625),
    ("small", 0.8125),
    ("medium", 1.0),
    ("large", 1.125),
    ("x-large", 1.5),
    ("xx-large", 2.0),
    ("xxx-large", 3.0),
];

/// The step `larger` and `smaller` take.
const RELATIVE_SIZE_STEP: f32 = 1.2;

/// Resolve a `font-size` declaration to pixels.
///
/// `parent_size` is the parent's computed font size, which is what `em`,
/// percentages and the relative keywords all resolve against. `context`
/// must therefore carry `font_size: parent_size` — passing the element's
/// own would be circular.
pub fn parse_size(
    values: &[ComponentValue],
    parent_size: f32,
    context: &LengthContext,
) -> Option<f32> {
    if let Some(CssToken::Ident(name)) = single_token(values) {
        if let Some((_, ratio)) = ABSOLUTE_SIZES
            .iter()
            .find(|(spelling, _)| name.eq_ignore_ascii_case(spelling))
        {
            return Some(INITIAL_FONT_SIZE * ratio);
        }
        if name.eq_ignore_ascii_case("larger") {
            return Some(parent_size * RELATIVE_SIZE_STEP);
        }
        if name.eq_ignore_ascii_case("smaller") {
            return Some(parent_size / RELATIVE_SIZE_STEP);
        }
        return None;
    }

    let size = match parse_length_percentage(values)? {
        LengthPercentage::Length(length) => length.to_px(context),
        LengthPercentage::Percentage(percent) => percent / 100.0 * parent_size,
    };
    // A negative font size is invalid; dropping the declaration leaves
    // the inherited size rather than inverting text.
    (size >= 0.0).then_some(size)
}

/// Resolve a `font-weight` declaration.
///
/// `parent_weight` is needed for `bolder` and `lighter`, which are
/// relative in a way no other font property is.
pub fn parse_weight(values: &[ComponentValue], parent_weight: u16) -> Option<u16> {
    match single_token(values)? {
        CssToken::Ident(name) => {
            if name.eq_ignore_ascii_case("normal") {
                Some(400)
            } else if name.eq_ignore_ascii_case("bold") {
                Some(700)
            } else if name.eq_ignore_ascii_case("bolder") {
                Some(bolder(parent_weight))
            } else if name.eq_ignore_ascii_case("lighter") {
                Some(lighter(parent_weight))
            } else {
                None
            }
        }
        // The numeric form is a range, not a set of nine steps.
        CssToken::Number { value, .. } => {
            let weight = value.round();
            (1.0..=1000.0).contains(&weight).then_some(weight as u16)
        }
        _ => None,
    }
}

/// The spec's `bolder` table. Not a fixed step: above 750 there is
/// nothing bolder to go to, so the weight stays put.
const fn bolder(inherited: u16) -> u16 {
    match inherited {
        0..=349 => 400,
        350..=549 => 700,
        550..=749 => 900,
        _ => inherited,
    }
}

/// The spec's `lighter` table, the mirror of [`bolder`].
const fn lighter(inherited: u16) -> u16 {
    match inherited {
        0..=99 => inherited,
        100..=549 => 100,
        550..=749 => 400,
        _ => 700,
    }
}

/// A computed `line-height`.
///
/// The variants differ in how they *inherit*, not only in how they are
/// written — see the module docs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    /// From the font's metrics, approximated as a factor for now.
    Normal,
    /// A multiple of the element's own font size. Inherits as the
    /// number, so a child with a larger font gets a larger line box.
    Number(f32),
    /// An absolute height in pixels. Inherits as that height.
    Length(f32),
}

impl LineHeight {
    pub const INITIAL: Self = Self::Normal;

    /// The line box height for an element with this font size.
    pub fn to_px(self, font_size: f32) -> f32 {
        match self {
            Self::Normal => font_size * NORMAL_LINE_HEIGHT_FACTOR,
            Self::Number(factor) => font_size * factor,
            Self::Length(px) => px,
        }
    }
}

/// Resolve a `line-height` declaration.
///
/// `context` must carry the element's *own* computed font size: unlike
/// `font-size`, a percentage here is of the element, and so is `em`.
pub fn parse_line_height(values: &[ComponentValue], context: &LengthContext) -> Option<LineHeight> {
    if let Some(CssToken::Ident(name)) = single_token(values) {
        return name
            .eq_ignore_ascii_case("normal")
            .then_some(LineHeight::Normal);
    }
    // A bare number is a factor, which is why this is checked before
    // the length path: `line-height: 0` is a factor of zero, not a
    // zero-length, and the two inherit differently.
    if let Some(CssToken::Number { value, .. }) = single_token(values) {
        let factor = value as f32;
        return (factor >= 0.0).then_some(LineHeight::Number(factor));
    }
    let height = match parse_length_percentage(values)? {
        LengthPercentage::Length(length) => length.to_px(context),
        LengthPercentage::Percentage(percent) => percent / 100.0 * context.font_size,
    };
    (height >= 0.0).then_some(LineHeight::Length(height))
}

/// Parse `letter-spacing`, in pixels.
///
/// `normal` is zero. A negative value is legal and is what a
/// display headline uses: set large, a typeface's default fit looks
/// loose, so a designer pulls it in by a few hundredths of an em.
/// Ignoring it is not a rounding error — at 84px, `-0.05em` takes
/// four pixels off every letter, and a headline that was drawn to sit
/// on two lines wraps onto three.
pub fn parse_letter_spacing(values: &[ComponentValue], context: &LengthContext) -> Option<f32> {
    if let Some(CssToken::Ident(name)) = single_token(values) {
        return name.eq_ignore_ascii_case("normal").then_some(0.0);
    }
    match parse_length_percentage(values)? {
        LengthPercentage::Length(length) => Some(length.to_px(context)),
        // A percentage is of the font size, the same basis every
        // other text length uses.
        LengthPercentage::Percentage(percent) => Some(percent / 100.0 * context.font_size),
    }
}

/// Parse a `font-family` list.
///
/// Unquoted names may be several identifiers long — `Times New Roman`
/// is three — so each comma-separated entry joins its idents with
/// single spaces. Generic families (`serif`, `sans-serif`) come through
/// as ordinary names; choosing a real font for them is the font
/// system's job, not the parser's.
pub fn parse_family(values: &[ComponentValue]) -> Option<Vec<String>> {
    let mut families = Vec::new();
    let mut current: Vec<String> = Vec::new();

    for value in values {
        let ComponentValue::Token(token) = value else {
            return None;
        };
        match token {
            CssToken::Whitespace => {}
            CssToken::Comma => {
                if current.is_empty() {
                    // `a, , b` — an empty entry is invalid.
                    return None;
                }
                families.push(current.join(" "));
                current.clear();
            }
            CssToken::Ident(name) => current.push(name.clone()),
            // A quoted name is one family however many words it has.
            CssToken::Str(name) => {
                if !current.is_empty() {
                    return None;
                }
                current.push(name.clone());
            }
            _ => return None,
        }
    }

    if current.is_empty() {
        return None;
    }
    families.push(current.join(" "));
    Some(families)
}

/// Parse `font-style`. Re-exported through this module so every font
/// property is reachable from one place.
pub fn parse_style(values: &[ComponentValue]) -> Option<FontStyle> {
    FontStyle::parse(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::parse_declarations;

    fn values(text: &str) -> Vec<ComponentValue> {
        parse_declarations(&format!("x: {text}"))
            .into_iter()
            .next()
            .map(|declaration| declaration.value)
            .unwrap_or_default()
    }

    /// A font size against a 16px parent.
    fn size(text: &str) -> Option<f32> {
        size_with(text, 16.0)
    }

    fn size_with(text: &str, parent: f32) -> Option<f32> {
        let context = LengthContext {
            font_size: parent,
            ..LengthContext::DEFAULT
        };
        parse_size(&values(text), parent, &context)
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    #[test]
    fn absolute_size_keywords_scale_from_medium() {
        assert!(close(size("medium").expect("medium"), 16.0));
        assert!(close(size("large").expect("large"), 18.0));
        assert!(close(size("small").expect("small"), 13.0));
        assert!(close(size("xx-large").expect("xx-large"), 32.0));
    }

    #[test]
    fn absolute_keywords_ignore_the_parent_size() {
        // They are absolute: a `large` inside a `large` is not larger.
        assert!(close(size_with("large", 40.0).expect("large"), 18.0));
    }

    #[test]
    fn relative_keywords_step_from_the_parent() {
        assert!(close(size_with("larger", 10.0).expect("larger"), 12.0));
        assert!(close(size_with("smaller", 12.0).expect("smaller"), 10.0));
    }

    #[test]
    fn em_on_font_size_is_relative_to_the_parent() {
        // The trap: 1.5em here means 1.5x the *parent*, not the element.
        assert!(close(size_with("1.5em", 20.0).expect("1.5em"), 30.0));
        assert!(close(size_with("2em", 8.0).expect("2em"), 16.0));
    }

    #[test]
    fn a_percentage_font_size_is_of_the_parent() {
        assert!(close(size_with("50%", 20.0).expect("50%"), 10.0));
        assert!(close(size_with("150%", 10.0).expect("150%"), 15.0));
    }

    #[test]
    fn an_absolute_length_font_size_passes_through() {
        assert!(close(size("12px").expect("12px"), 12.0));
        assert!(close(size("12pt").expect("12pt"), 16.0));
    }

    #[test]
    fn a_negative_font_size_is_invalid() {
        assert_eq!(size("-1px"), None);
        assert_eq!(size("-50%"), None);
    }

    #[test]
    fn an_unknown_font_size_keyword_is_invalid() {
        assert_eq!(size("enormous"), None);
    }

    #[test]
    fn weight_keywords_map_to_numbers() {
        assert_eq!(parse_weight(&values("normal"), 400), Some(400));
        assert_eq!(parse_weight(&values("bold"), 400), Some(700));
        assert_eq!(parse_weight(&values("NORMAL"), 400), Some(400));
    }

    #[test]
    fn numeric_weights_span_the_whole_range() {
        assert_eq!(parse_weight(&values("350"), 400), Some(350));
        assert_eq!(parse_weight(&values("1"), 400), Some(1));
        assert_eq!(parse_weight(&values("1000"), 400), Some(1000));
    }

    #[test]
    fn out_of_range_weights_are_invalid() {
        assert_eq!(parse_weight(&values("0"), 400), None);
        assert_eq!(parse_weight(&values("1001"), 400), None);
        assert_eq!(parse_weight(&values("-400"), 400), None);
    }

    #[test]
    fn bolder_follows_the_table_rather_than_a_fixed_step() {
        assert_eq!(parse_weight(&values("bolder"), 100), Some(400));
        assert_eq!(parse_weight(&values("bolder"), 400), Some(700));
        assert_eq!(parse_weight(&values("bolder"), 700), Some(900));
        // Nothing bolder to reach for.
        assert_eq!(parse_weight(&values("bolder"), 900), Some(900));
    }

    #[test]
    fn lighter_mirrors_bolder() {
        assert_eq!(parse_weight(&values("lighter"), 900), Some(700));
        assert_eq!(parse_weight(&values("lighter"), 700), Some(400));
        assert_eq!(parse_weight(&values("lighter"), 400), Some(100));
        assert_eq!(parse_weight(&values("lighter"), 50), Some(50));
    }

    #[test]
    fn line_height_normal_uses_the_font_metric_placeholder() {
        let parsed = parse_line_height(&values("normal"), &LengthContext::DEFAULT);
        assert_eq!(parsed, Some(LineHeight::Normal));
        assert!(close(LineHeight::Normal.to_px(20.0), 24.0));
    }

    #[test]
    fn a_numeric_line_height_stays_a_number() {
        // So that it inherits as a ratio, not as pixels.
        let parsed = parse_line_height(&values("1.5"), &LengthContext::DEFAULT);
        assert_eq!(parsed, Some(LineHeight::Number(1.5)));
        assert!(close(LineHeight::Number(1.5).to_px(10.0), 15.0));
        assert!(close(LineHeight::Number(1.5).to_px(20.0), 30.0));
    }

    #[test]
    fn a_percentage_line_height_resolves_to_a_length() {
        let context = LengthContext {
            font_size: 20.0,
            ..LengthContext::DEFAULT
        };
        let parsed = parse_line_height(&values("150%"), &context).expect("150%");
        assert_eq!(parsed, LineHeight::Length(30.0));
        // And having resolved, it no longer scales with the font.
        assert!(close(parsed.to_px(10.0), 30.0));
    }

    #[test]
    fn a_length_line_height_resolves_against_the_elements_own_font() {
        let context = LengthContext {
            font_size: 20.0,
            ..LengthContext::DEFAULT
        };
        let parsed = parse_line_height(&values("2em"), &context).expect("2em");
        assert_eq!(parsed, LineHeight::Length(40.0));
    }

    #[test]
    fn zero_line_height_is_a_factor_not_a_length() {
        assert_eq!(
            parse_line_height(&values("0"), &LengthContext::DEFAULT),
            Some(LineHeight::Number(0.0))
        );
    }

    #[test]
    fn a_negative_line_height_is_invalid() {
        assert_eq!(
            parse_line_height(&values("-1"), &LengthContext::DEFAULT),
            None
        );
        assert_eq!(
            parse_line_height(&values("-1px"), &LengthContext::DEFAULT),
            None
        );
    }

    #[test]
    fn a_font_family_list_splits_on_commas() {
        assert_eq!(
            parse_family(&values("Arial, sans-serif")),
            Some(vec!["Arial".into(), "sans-serif".into()])
        );
    }

    #[test]
    fn an_unquoted_multi_word_family_joins_its_words() {
        assert_eq!(
            parse_family(&values("Times New Roman, serif")),
            Some(vec!["Times New Roman".into(), "serif".into()])
        );
    }

    #[test]
    fn a_quoted_family_is_one_name() {
        assert_eq!(
            parse_family(&values("\"Helvetica Neue\", Arial")),
            Some(vec!["Helvetica Neue".into(), "Arial".into()])
        );
    }

    #[test]
    fn a_single_family_needs_no_comma() {
        assert_eq!(
            parse_family(&values("monospace")),
            Some(vec!["monospace".into()])
        );
    }

    #[test]
    fn an_empty_or_malformed_family_list_is_invalid() {
        assert_eq!(parse_family(&values("")), None);
        assert_eq!(parse_family(&values("Arial, , serif")), None);
        assert_eq!(parse_family(&values("Arial,")), None);
        assert_eq!(parse_family(&values("12px")), None);
    }

    #[test]
    fn font_style_parses_through_this_module_too() {
        assert_eq!(parse_style(&values("italic")), Some(FontStyle::Italic));
        assert_eq!(parse_style(&values("sideways")), None);
    }
}
