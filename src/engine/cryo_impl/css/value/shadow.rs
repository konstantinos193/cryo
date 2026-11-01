//! `box-shadow`: the offsets, blur and colour of a cast shadow.
//!
//! The grammar is positional and partly optional, which is the only
//! awkward part:
//!
//! ```css
//!   box-shadow: 0 4px 12px rgba(0, 0, 0, .15);
//!               ╰┬╯ ╰─┬─╯ ╰─┬─╯ ╰──────┬────╯
//!                x    y    blur      colour
//! ```
//!
//! The lengths come in a fixed order — x, y, blur, spread — and the
//! colour may sit before or after them. A leading `inset` keyword
//! makes the shadow fall inside the box instead of behind it.
//!
//! A shadow is what makes a card look like a card. A page that writes
//! one and gets nothing looks flat in a way no amount of correct
//! layout fixes, which is why this is a value type of its own rather
//! than something to approximate with a border.
//!
//! Known gaps, stated rather than hidden:
//! - `inset` shadows are parsed and then skipped at paint: drawing
//!   one means masking to the *inside* of the box, which the
//!   rasteriser cannot do yet.
//! - A list of several shadows is read, and all of them are drawn.

use super::super::parser::ComponentValue;
use super::super::tokenizer::CssToken;
use super::{color, Color, LengthContext, LengthPercentage};

/// One cast shadow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub offset_x: f32,
    pub offset_y: f32,
    /// How far the edge is softened. Zero is a hard edge.
    pub blur: f32,
    /// How much bigger than the box the shadow is drawn.
    pub spread: f32,
    pub color: Color,
    /// Whether it falls inside the box rather than behind it.
    pub inset: bool,
}

impl Shadow {
    /// Parse a `box-shadow` list. `none` yields no shadows.
    ///
    /// `context` resolves the lengths, and `current` is the colour a
    /// shadow takes when it names none — `currentColor`, which is the
    /// element's own text colour.
    pub fn parse_list(
        values: &[ComponentValue],
        context: &LengthContext,
        current: Color,
    ) -> Option<Vec<Self>> {
        let mut shadows = Vec::new();
        for part in split_on_commas(values) {
            if is_none(&part) {
                continue;
            }
            shadows.push(Self::parse_one(&part, context, current)?);
        }
        Some(shadows)
    }

    fn parse_one(
        values: &[ComponentValue],
        context: &LengthContext,
        current: Color,
    ) -> Option<Self> {
        let mut lengths = Vec::new();
        let mut color = None;
        let mut inset = false;

        for value in values {
            match value {
                ComponentValue::Token(CssToken::Whitespace) => {}
                ComponentValue::Token(CssToken::Ident(word))
                    if word.eq_ignore_ascii_case("inset") =>
                {
                    inset = true;
                }
                // A length, or the colour — whichever this is.
                other => {
                    let single = std::slice::from_ref(other);
                    if let Some(length) = super::parse_length_percentage(single) {
                        lengths.push(match length {
                            LengthPercentage::Length(length) => length.to_px(context),
                            // A percentage is not valid in a shadow.
                            LengthPercentage::Percentage(_) => return None,
                        });
                    } else {
                        let specified = color::parse(single)?;
                        color = Some(match specified {
                            color::SpecifiedColor::Absolute(value) => value,
                            color::SpecifiedColor::Current => current,
                        });
                    }
                }
            }
        }

        // The two offsets are required; blur and spread are not.
        let [offset_x, offset_y] = lengths.get(..2)? else {
            return None;
        };
        Some(Self {
            offset_x: *offset_x,
            offset_y: *offset_y,
            blur: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
            spread: lengths.get(3).copied().unwrap_or(0.0),
            color: color.unwrap_or(current),
            inset,
        })
    }

    /// Whether this shadow would draw anything at all.
    pub fn is_visible(&self) -> bool {
        !self.color.is_transparent() && !self.inset
    }
}

fn is_none(values: &[ComponentValue]) -> bool {
    let mut seen = values
        .iter()
        .filter(|value| !matches!(value, ComponentValue::Token(CssToken::Whitespace)));
    matches!(
        (seen.next(), seen.next()),
        (Some(ComponentValue::Token(CssToken::Ident(word))), None) if word.eq_ignore_ascii_case("none")
    )
}

fn split_on_commas(values: &[ComponentValue]) -> Vec<Vec<ComponentValue>> {
    let mut out = vec![Vec::new()];
    for value in values {
        if matches!(value, ComponentValue::Token(CssToken::Comma)) {
            out.push(Vec::new());
        } else {
            out.last_mut()
                .expect("one part is always open")
                .push(value.clone());
        }
    }
    out.retain(|part| {
        part.iter()
            .any(|value| !matches!(value, ComponentValue::Token(CssToken::Whitespace)))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::{parse, Rule};

    fn shadows(value: &str) -> Vec<Shadow> {
        let sheet = parse(&format!("a {{ box-shadow: {value} }}"));
        let Some(Rule::Style(style)) = sheet.rules.first() else {
            panic!("not a style rule");
        };
        Shadow::parse_list(
            &style.declarations[0].value,
            &LengthContext::DEFAULT,
            Color::BLACK,
        )
        .expect("parses")
    }

    #[test]
    fn two_offsets_are_the_shortest_form() {
        let parsed = shadows("2px 4px");
        assert_eq!(parsed.len(), 1);
        assert_eq!((parsed[0].offset_x, parsed[0].offset_y), (2.0, 4.0));
        assert_eq!(parsed[0].blur, 0.0);
        assert_eq!(parsed[0].spread, 0.0);
    }

    #[test]
    fn a_third_length_is_the_blur() {
        assert_eq!(shadows("0 4px 12px")[0].blur, 12.0);
    }

    #[test]
    fn a_fourth_length_is_the_spread() {
        let parsed = shadows("0 1px 2px 3px");
        assert_eq!(parsed[0].blur, 2.0);
        assert_eq!(parsed[0].spread, 3.0);
    }

    #[test]
    fn the_colour_may_come_after_the_lengths() {
        // The form every real stylesheet uses.
        let parsed = shadows("0 4px 12px rgba(0, 0, 0, 0.15)");
        assert_eq!(parsed[0].color.red, 0);
        assert!((parsed[0].color.alpha - 0.15).abs() < 1e-6);
    }

    #[test]
    fn the_colour_may_come_first() {
        let parsed = shadows("red 1px 2px");
        assert_eq!(parsed[0].color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn a_shadow_with_no_colour_takes_the_current_one() {
        assert_eq!(shadows("1px 1px")[0].color, Color::BLACK);
    }

    #[test]
    fn inset_is_read_and_marked_as_not_drawn() {
        let parsed = shadows("inset 0 2px 4px red");
        assert!(parsed[0].inset);
        assert!(!parsed[0].is_visible(), "inset is a stated gap");
    }

    #[test]
    fn none_yields_no_shadows() {
        assert!(shadows("none").is_empty());
    }

    #[test]
    fn a_list_keeps_every_shadow() {
        let parsed = shadows("0 1px 2px red, 0 4px 8px blue");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].color, Color::rgb(255, 0, 0));
        assert_eq!(parsed[1].offset_y, 4.0);
    }

    #[test]
    fn a_transparent_shadow_draws_nothing() {
        assert!(!shadows("0 1px 2px transparent")[0].is_visible());
    }

    #[test]
    fn a_negative_blur_is_clamped_rather_than_inverted() {
        assert_eq!(shadows("0 0 -5px red")[0].blur, 0.0);
    }

    #[test]
    fn relative_lengths_resolve_against_the_context() {
        // 1em at the default 16px font.
        assert_eq!(shadows("0 1em")[0].offset_y, 16.0);
    }
}
