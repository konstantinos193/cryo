//! Typed CSS values: the things a declaration's tokens become once the
//! property is known.
//!
//! The split that matters here is **what can be resolved when**:
//!
//! - *Absolute* units (`px`, `pt`, `cm`) resolve immediately.
//! - *Font-relative* units (`em`, `rem`) resolve during computed-value
//!   time, against a font size that is itself being computed — which is
//!   why they need a [`LengthContext`] and not just a number.
//! - *Percentages* cannot resolve at all until layout, because what they
//!   are a percentage *of* is the containing block, which does not exist
//!   yet. So [`LengthPercentage`] keeps them symbolic and carries them
//!   through to layout untouched.
//!
//! Collapsing percentages early is the classic way to get `width: 50%`
//! subtly wrong, so the type makes it impossible: there is no way to
//! turn a `LengthPercentage` into pixels without supplying the basis.

pub mod background;
pub mod clip;
pub mod color;
pub mod gradient;
pub mod grid;
pub mod shadow;

pub use background::{BackgroundBox, BackgroundImage};
pub use clip::ClipPath;
pub use color::{Color, SpecifiedColor};
pub use gradient::Gradient;
pub use grid::{Line, Placement, Track};
pub use shadow::Shadow;

use super::parser::ComponentValue;
use super::tokenizer::CssToken;

/// A CSS length unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Px,
    /// Relative to this element's own font size.
    Em,
    /// Relative to the root element's font size.
    Rem,
    /// The font's x-height.
    Ex,
    /// The advance width of `0` in the font.
    Ch,
    Vw,
    Vh,
    Vmin,
    Vmax,
    Pt,
    Pc,
    In,
    Cm,
    Mm,
    /// A quarter-millimetre.
    Q,
}

impl Unit {
    pub(in crate::engine::cryo_impl::css) fn parse(unit: &str) -> Option<Self> {
        let lowered = unit.to_ascii_lowercase();
        Some(match lowered.as_str() {
            "px" => Self::Px,
            "em" => Self::Em,
            "rem" => Self::Rem,
            "ex" => Self::Ex,
            "ch" => Self::Ch,
            // The viewport units, in all four spellings. `sv`, `lv`
            // and `dv` are the *small*, *large* and *dynamic*
            // viewports — they differ only on a phone, where a
            // toolbar slides in and out and the three answer "with
            // it", "without it" and "right now". There is no such
            // toolbar here, so all four are the same number.
            //
            // Supporting them is not cosmetic: a heading written as
            // `clamp(42px, min(5.8vw, 8.6svh), 84px)` loses its whole
            // `clamp` if one unit inside fails to parse, and comes
            // out at the inherited size.
            "vw" | "svw" | "lvw" | "dvw" => Self::Vw,
            "vh" | "svh" | "lvh" | "dvh" => Self::Vh,
            "vmin" | "svmin" | "lvmin" | "dvmin" => Self::Vmin,
            "vmax" | "svmax" | "lvmax" | "dvmax" => Self::Vmax,
            // The inline/block spellings, fixed to horizontal-tb.
            "vi" | "svi" | "lvi" | "dvi" => Self::Vw,
            "vb" | "svb" | "lvb" | "dvb" => Self::Vh,
            "pt" => Self::Pt,
            "pc" => Self::Pc,
            "in" => Self::In,
            "cm" => Self::Cm,
            "mm" => Self::Mm,
            "q" => Self::Q,
            _ => return None,
        })
    }

    /// Whether this unit needs no context at all.
    pub const fn is_absolute(self) -> bool {
        matches!(
            self,
            Self::Px | Self::Pt | Self::Pc | Self::In | Self::Cm | Self::Mm | Self::Q
        )
    }
}

/// CSS defines every absolute unit against the inch, and the inch
/// against 96 reference pixels — so a `px` is a unit of length, not a
/// device pixel. Device scaling happens later, at paint.
const PX_PER_INCH: f32 = 96.0;

/// A length: a number and a unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Length {
    pub value: f32,
    pub unit: Unit,
}

impl Length {
    pub const ZERO: Self = Self {
        value: 0.0,
        unit: Unit::Px,
    };

    pub const fn px(value: f32) -> Self {
        Self {
            value,
            unit: Unit::Px,
        }
    }

    /// Resolve to pixels.
    ///
    /// `ex` and `ch` are approximated as half the font size, because
    /// their real values are font metrics and no font is loaded yet.
    /// That is a stated approximation, not a guess that will be
    /// forgotten: both become exact once text shaping exists.
    pub fn to_px(self, context: &LengthContext) -> f32 {
        let factor = match self.unit {
            Unit::Px => 1.0,
            Unit::In => PX_PER_INCH,
            Unit::Cm => PX_PER_INCH / 2.54,
            Unit::Mm => PX_PER_INCH / 25.4,
            Unit::Q => PX_PER_INCH / 101.6,
            Unit::Pt => PX_PER_INCH / 72.0,
            Unit::Pc => PX_PER_INCH / 6.0,
            Unit::Em => context.font_size,
            Unit::Rem => context.root_font_size,
            Unit::Ex | Unit::Ch => context.font_size * 0.5,
            Unit::Vw => context.viewport_width / 100.0,
            Unit::Vh => context.viewport_height / 100.0,
            Unit::Vmin => context.viewport_width.min(context.viewport_height) / 100.0,
            Unit::Vmax => context.viewport_width.max(context.viewport_height) / 100.0,
        };
        self.value * factor
    }
}

/// What font-relative and viewport-relative units resolve against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LengthContext {
    /// The element's own computed font size, in pixels. `em` on any
    /// property other than `font-size` uses this; on `font-size` itself
    /// it is the *parent's*, which is the caller's job to pass.
    pub font_size: f32,
    pub root_font_size: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
}

impl LengthContext {
    /// The default: a 16px font in a 1024x768 viewport, matching the
    /// initial `font-size: medium`.
    pub const DEFAULT: Self = Self {
        font_size: 16.0,
        root_font_size: 16.0,
        viewport_width: 1024.0,
        viewport_height: 768.0,
    };
}

impl Default for LengthContext {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A length or a percentage. The percentage stays symbolic because its
/// basis is not known until layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LengthPercentage {
    Length(Length),
    /// In percent, so `50%` is `50.0`.
    Percentage(f32),
}

impl LengthPercentage {
    pub const ZERO: Self = Self::Length(Length::ZERO);

    /// Resolve against a basis — the containing block's size for this
    /// axis. There is deliberately no way to resolve without one.
    pub fn to_px(self, context: &LengthContext, basis: f32) -> f32 {
        match self {
            Self::Length(length) => length.to_px(context),
            Self::Percentage(percent) => percent / 100.0 * basis,
        }
    }

    /// Resolve only if it does not need a basis. Used where a percentage
    /// behaves as `auto` — a percentage height against an auto-height
    /// parent, for instance.
    pub fn to_px_if_absolute(self, context: &LengthContext) -> Option<f32> {
        match self {
            Self::Length(length) => Some(length.to_px(context)),
            Self::Percentage(_) => None,
        }
    }

    /// Fix the length half to pixels, leaving a percentage symbolic.
    ///
    /// This is what computed-value time does to a length: `2em` becomes
    /// a number, because the font size it depended on is known now and
    /// will be wrong later — a child's `em` is not its parent's. A
    /// percentage is the opposite: its basis is still unknown, so
    /// touching it here would be the bug.
    pub fn computed(self, context: &LengthContext) -> Self {
        match self {
            Self::Length(length) => Self::Length(Length::px(length.to_px(context))),
            Self::Percentage(_) => self,
        }
    }
}

impl Size {
    /// Fix any length to pixels; see [`LengthPercentage::computed`].
    pub fn computed(self, context: &LengthContext) -> Self {
        match self {
            Self::Auto => Self::Auto,
            Self::Definite(value) => Self::Definite(value.computed(context)),
        }
    }
}

/// A `width`, `height` or similar: a length, a percentage, or `auto`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Size {
    #[default]
    Auto,
    Definite(LengthPercentage),
}

/// Parse a single length. A bare `0` counts, since zero needs no unit.
pub fn parse_length(values: &[ComponentValue]) -> Option<Length> {
    match single_token(values)? {
        CssToken::Dimension { value, unit } => Some(Length {
            value: value as f32,
            unit: Unit::parse(&unit)?,
        }),
        // Only zero may omit its unit.
        CssToken::Number { value, .. } => (value == 0.0).then_some(Length::ZERO),
        _ => None,
    }
}

/// Parse a length or a percentage.
pub fn parse_length_percentage(values: &[ComponentValue]) -> Option<LengthPercentage> {
    if let CssToken::Percentage(percent) = single_token(values)? {
        return Some(LengthPercentage::Percentage(percent as f32));
    }
    parse_length(values).map(LengthPercentage::Length)
}

/// Parse a `width`-style value: `auto`, a length or a percentage.
pub fn parse_size(values: &[ComponentValue]) -> Option<Size> {
    if let CssToken::Ident(name) = single_token(values)? {
        if name.eq_ignore_ascii_case("auto") {
            return Some(Size::Auto);
        }
        return None;
    }
    parse_length_percentage(values).map(Size::Definite)
}

/// The one token a value consists of, or `None` if it is not exactly one.
pub(crate) fn single_token(values: &[ComponentValue]) -> Option<CssToken> {
    let mut meaningful = values
        .iter()
        .filter(|value| !matches!(value, ComponentValue::Token(CssToken::Whitespace)));
    let only = meaningful.next()?;
    if meaningful.next().is_some() {
        return None;
    }
    match only {
        ComponentValue::Token(token) => Some(token.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::parse_declarations;

    /// The value of `width: <text>` as component values.
    fn values(text: &str) -> Vec<ComponentValue> {
        parse_declarations(&format!("width: {text}"))
            .into_iter()
            .next()
            .map(|declaration| declaration.value)
            .unwrap_or_default()
    }

    fn length(text: &str) -> Option<Length> {
        parse_length(&values(text))
    }

    /// A length in pixels under the default context.
    fn px(text: &str) -> Option<f32> {
        Some(length(text)?.to_px(&LengthContext::DEFAULT))
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    #[test]
    fn pixel_lengths_pass_through() {
        assert_eq!(px("10px"), Some(10.0));
        assert_eq!(px("-4px"), Some(-4.0));
        assert_eq!(px("1.5px"), Some(1.5));
    }

    #[test]
    fn zero_needs_no_unit_but_other_numbers_do() {
        assert_eq!(length("0"), Some(Length::ZERO));
        assert_eq!(length("10"), None, "a bare non-zero number is not a length");
    }

    #[test]
    fn absolute_units_convert_against_the_reference_inch() {
        assert!(close(px("1in").expect("1in"), 96.0));
        assert!(close(px("72pt").expect("72pt"), 96.0));
        assert!(close(px("1pc").expect("1pc"), 16.0));
        assert!(close(px("2.54cm").expect("2.54cm"), 96.0));
        assert!(close(px("25.4mm").expect("25.4mm"), 96.0));
        assert!(close(px("101.6q").expect("101.6q"), 96.0));
    }

    #[test]
    fn font_relative_units_use_the_context() {
        let context = LengthContext {
            font_size: 20.0,
            root_font_size: 10.0,
            ..LengthContext::DEFAULT
        };
        assert!(close(length("2em").expect("2em").to_px(&context), 40.0));
        assert!(close(length("2rem").expect("2rem").to_px(&context), 20.0));
    }

    #[test]
    fn em_and_rem_differ_when_the_root_differs() {
        let context = LengthContext {
            font_size: 32.0,
            root_font_size: 16.0,
            ..LengthContext::DEFAULT
        };
        let em = length("1em").expect("1em").to_px(&context);
        let rem = length("1rem").expect("1rem").to_px(&context);
        assert!(close(em, 32.0));
        assert!(close(rem, 16.0));
    }

    #[test]
    fn viewport_units_use_the_viewport() {
        let context = LengthContext {
            viewport_width: 800.0,
            viewport_height: 600.0,
            ..LengthContext::DEFAULT
        };
        assert!(close(length("50vw").expect("50vw").to_px(&context), 400.0));
        assert!(close(length("50vh").expect("50vh").to_px(&context), 300.0));
        assert!(close(
            length("10vmin").expect("10vmin").to_px(&context),
            60.0
        ));
        assert!(close(
            length("10vmax").expect("10vmax").to_px(&context),
            80.0
        ));
    }

    #[test]
    fn units_are_case_insensitive() {
        assert_eq!(px("10PX"), Some(10.0));
        assert_eq!(length("1EM").map(|l| l.unit), Some(Unit::Em));
    }

    #[test]
    fn an_unknown_unit_is_not_a_length() {
        assert_eq!(length("10foo"), None);
        assert_eq!(length("10%"), None, "a percentage is not a length");
    }

    #[test]
    fn absolute_units_are_reported_as_such() {
        assert!(Unit::Px.is_absolute());
        assert!(Unit::Pt.is_absolute());
        assert!(!Unit::Em.is_absolute());
        assert!(!Unit::Vw.is_absolute());
    }

    #[test]
    fn percentages_stay_symbolic_until_a_basis_is_given() {
        let parsed = parse_length_percentage(&values("50%")).expect("50% parses");
        assert_eq!(parsed, LengthPercentage::Percentage(50.0));
        assert_eq!(parsed.to_px_if_absolute(&LengthContext::DEFAULT), None);
        assert!(close(parsed.to_px(&LengthContext::DEFAULT, 400.0), 200.0));
    }

    #[test]
    fn a_length_percentage_accepts_both_forms() {
        assert_eq!(
            parse_length_percentage(&values("10px")),
            Some(LengthPercentage::Length(Length::px(10.0)))
        );
        assert!(parse_length_percentage(&values("auto")).is_none());
    }

    #[test]
    fn an_absolute_length_percentage_resolves_without_a_basis() {
        let parsed = parse_length_percentage(&values("10px")).expect("parses");
        assert_eq!(
            parsed.to_px_if_absolute(&LengthContext::DEFAULT),
            Some(10.0)
        );
    }

    #[test]
    fn size_accepts_auto_a_length_and_a_percentage() {
        assert_eq!(parse_size(&values("auto")), Some(Size::Auto));
        assert_eq!(parse_size(&values("AUTO")), Some(Size::Auto));
        assert_eq!(
            parse_size(&values("10px")),
            Some(Size::Definite(LengthPercentage::Length(Length::px(10.0))))
        );
        assert_eq!(
            parse_size(&values("50%")),
            Some(Size::Definite(LengthPercentage::Percentage(50.0)))
        );
        assert_eq!(parse_size(&values("medium")), None);
        assert_eq!(Size::default(), Size::Auto);
    }

    #[test]
    fn a_multi_part_value_is_not_a_single_length() {
        assert_eq!(length("10px 20px"), None);
        assert_eq!(parse_size(&values("auto auto")), None);
    }

    #[test]
    fn whitespace_around_a_value_is_ignored() {
        assert_eq!(px("  10px  "), Some(10.0));
    }

    #[test]
    fn a_function_is_not_a_length_yet() {
        // `calc()` needs its own evaluator; until then it is rejected
        // rather than silently treated as zero.
        assert_eq!(length("calc(1px + 1px)"), None);
    }
    #[test]
    fn the_small_and_dynamic_viewport_units_are_read() {
        // A heading written as `clamp(42px, min(5.8vw, 8.6svh), 84px)`
        // loses the whole clamp if one unit inside fails to parse.
        for unit in ["vh", "svh", "lvh", "dvh", "vb", "dvb"] {
            assert_eq!(Unit::parse(unit), Some(Unit::Vh), "{unit}");
        }
        for unit in ["vw", "svw", "lvw", "dvw", "vi", "dvi"] {
            assert_eq!(Unit::parse(unit), Some(Unit::Vw), "{unit}");
        }
        assert_eq!(Unit::parse("svmin"), Some(Unit::Vmin));
        assert_eq!(Unit::parse("dvmax"), Some(Unit::Vmax));
    }

    #[test]
    fn a_unit_that_is_not_a_unit_is_refused() {
        assert_eq!(Unit::parse("xvh"), None);
        assert_eq!(Unit::parse("fr"), None, "a grid track, not a length");
    }
}
