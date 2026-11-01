//! Grid track sizes and item placement, as written in a stylesheet.
//!
//! Two vocabularies live here, and they are not the same thing:
//!
//! - A **track** is how wide a column (or how tall a row) should be.
//!   `200px`, `auto`, `1fr`, `minmax(0, 1fr)`, and `repeat(3, 1fr)`
//!   standing for three of them.
//! - A **placement** is which tracks one item sits across.
//!   `grid-column: 2 / 4`, `span 2`, or `1 / -1` for the full width.
//!
//! The `fr` unit is what makes a grid a grid rather than a table: it
//! is a share of whatever space the fixed tracks leave, so a row of
//! `repeat(6, 1fr)` divides evenly however wide the container turns
//! out to be. Sizing it therefore cannot happen until every other
//! track is known, which is why [`Track`] keeps it as its own case
//! rather than resolving to a length here.
//!
//! A negative line number counts from the end: `-1` is the last line,
//! so `1 / -1` is "the whole thing" without knowing how many tracks
//! there are.
//!
//! Known gaps, stated rather than hidden:
//! - No named lines or areas, and no `repeat(auto-fill, …)`, which
//!   needs the container's size at parse time.
//! - `minmax` keeps both ends but the minimum is only honoured for
//!   lengths; `min-content` and `max-content` are not implemented.

use super::super::parser::ComponentValue;
use super::super::tokenizer::CssToken;
use super::{parse_length_percentage, LengthContext, LengthPercentage};

/// How one track is sized.
#[derive(Clone, Debug, PartialEq)]
pub enum Track {
    /// A length or percentage of the container.
    Fixed(LengthPercentage),
    /// As big as its content needs.
    Auto,
    /// A share of the leftover space.
    Fraction(f32),
    /// A floor and a ceiling, each itself a track.
    MinMax(Box<Track>, Box<Track>),
}

impl Track {
    /// Fix any lengths to pixels; see [`LengthPercentage::computed`].
    pub fn computed(self, context: &LengthContext) -> Self {
        match self {
            Self::Fixed(value) => Self::Fixed(value.computed(context)),
            Self::MinMax(low, high) => Self::MinMax(
                Box::new(low.computed(context)),
                Box::new(high.computed(context)),
            ),
            other => other,
        }
    }

    /// The `fr` share this track asks for, if any.
    ///
    /// A `minmax(…, 1fr)` is a flexible track whose minimum is its
    /// floor, so the fraction is read off the *maximum*.
    pub fn fraction(&self) -> Option<f32> {
        match self {
            Self::Fraction(share) => Some(*share),
            Self::MinMax(_, high) => high.fraction(),
            _ => None,
        }
    }

    /// This track's floor in pixels, against a container `basis` wide.
    pub fn minimum(&self, basis: f32) -> f32 {
        match self {
            Self::Fixed(value) => resolve(*value, basis),
            Self::MinMax(low, _) => low.minimum(basis),
            // `auto` and `fr` have no floor of their own here; their
            // size comes from content or from the leftover space.
            _ => 0.0,
        }
    }

    /// A definite size, for a track that needs no negotiation.
    pub fn definite(&self, basis: f32) -> Option<f32> {
        match self {
            Self::Fixed(value) => Some(resolve(*value, basis)),
            // A `minmax` with no flexible end is as big as it may be.
            Self::MinMax(_, high) if high.fraction().is_none() => high.definite(basis),
            _ => None,
        }
    }

    /// Parse a track list, expanding any `repeat()`.
    pub fn parse_list(values: &[ComponentValue]) -> Option<Vec<Self>> {
        let mut tracks = Vec::new();
        for value in values {
            match value {
                ComponentValue::Token(CssToken::Whitespace) => {}
                ComponentValue::Function { name, arguments }
                    if name.eq_ignore_ascii_case("repeat") =>
                {
                    tracks.extend(expand_repeat(arguments)?);
                }
                other => tracks.push(Self::parse_one(std::slice::from_ref(other))?),
            }
        }
        (!tracks.is_empty()).then_some(tracks)
    }

    /// One track.
    fn parse_one(values: &[ComponentValue]) -> Option<Self> {
        let value = values
            .iter()
            .find(|value| !matches!(value, ComponentValue::Token(CssToken::Whitespace)))?;
        match value {
            ComponentValue::Token(CssToken::Ident(word))
                if word.eq_ignore_ascii_case("auto")
                    || word.eq_ignore_ascii_case("min-content")
                    || word.eq_ignore_ascii_case("max-content") =>
            {
                Some(Self::Auto)
            }
            // `1fr` tokenises as a dimension with the unit `fr`.
            ComponentValue::Token(CssToken::Dimension { value, unit })
                if unit.eq_ignore_ascii_case("fr") =>
            {
                Some(Self::Fraction(*value as f32))
            }
            ComponentValue::Function { name, arguments } if name.eq_ignore_ascii_case("minmax") => {
                let mut parts = arguments
                    .split(|value| matches!(value, ComponentValue::Token(CssToken::Comma)));
                let low = Self::parse_one(parts.next()?)?;
                let high = Self::parse_one(parts.next()?)?;
                Some(Self::MinMax(Box::new(low), Box::new(high)))
            }
            other => parse_length_percentage(std::slice::from_ref(other)).map(Self::Fixed),
        }
    }
}

/// `repeat(3, 1fr)` and friends.
fn expand_repeat(arguments: &[ComponentValue]) -> Option<Vec<Track>> {
    let mut parts =
        arguments.split(|value| matches!(value, ComponentValue::Token(CssToken::Comma)));
    let count = parts.next()?.iter().find_map(|value| match value {
        ComponentValue::Token(CssToken::Number {
            value,
            integer: true,
        }) => Some(*value as i64),
        _ => None,
    })?;
    // `auto-fill` and `auto-fit` need the container's size, which is
    // not known here — see the module note.
    if !(1..=1000).contains(&count) {
        return None;
    }
    let pattern = Track::parse_list(parts.next()?)?;
    let mut out = Vec::with_capacity(pattern.len() * count as usize);
    for _ in 0..count {
        out.extend(pattern.iter().cloned());
    }
    Some(out)
}

/// Where one edge of an item sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    /// Wherever auto-placement puts it.
    Auto,
    /// A line number. Negative counts back from the end.
    Number(i32),
    /// This many tracks from the other edge.
    Span(u32),
}

/// An item's placement along one axis: `grid-column` or `grid-row`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub start: Line,
    pub end: Line,
}

impl Placement {
    pub const AUTO: Self = Self {
        start: Line::Auto,
        end: Line::Auto,
    };

    /// Parse `grid-column` / `grid-row`: one or two lines round a `/`.
    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        let mut sides =
            values.split(|value| matches!(value, ComponentValue::Token(CssToken::Delim('/'))));
        let start = parse_line(sides.next()?)?;
        let end = match sides.next() {
            Some(rest) => parse_line(rest)?,
            // One value alone: a span stays a span, a number spans one
            // track.
            None => match start {
                Line::Span(_) => Line::Auto,
                _ => Line::Auto,
            },
        };
        Some(Self { start, end })
    }

    /// The tracks this placement covers, given how many there are.
    ///
    /// Returns `None` when auto-placement has to decide, which the
    /// caller does because it alone knows what is already taken.
    pub fn resolve(&self, tracks: usize) -> Option<(usize, usize)> {
        let line = |value: Line| -> Option<usize> {
            match value {
                Line::Auto | Line::Span(_) => None,
                // Lines are numbered 1..=tracks+1, and the result is a
                // *track* index: line 1 is before track 0.
                Line::Number(number) if number > 0 => Some((number as usize).saturating_sub(1)),
                // A negative line counts back from the last: -1 is
                // line tracks+1, whose index is tracks — which is what
                // makes `1 / -1` the whole row.
                Line::Number(number) => {
                    let index = tracks as i32 + 1 + number;
                    (index >= 0).then_some(index as usize)
                }
            }
        };
        let span = |value: Line| match value {
            Line::Span(count) => count.max(1) as usize,
            _ => 1,
        };

        match (line(self.start), line(self.end)) {
            (Some(start), Some(end)) if end > start => Some((start, end)),
            (Some(start), _) => Some((start, start + span(self.end))),
            (None, Some(end)) => Some((end.saturating_sub(span(self.start)), end)),
            (None, None) => None,
        }
    }

    /// How many tracks this item needs, whatever it ends up over.
    pub fn span(&self) -> usize {
        match (self.start, self.end) {
            (Line::Span(count), _) | (_, Line::Span(count)) => count.max(1) as usize,
            _ => 1,
        }
    }
}

fn parse_line(values: &[ComponentValue]) -> Option<Line> {
    let mut span = false;
    let mut number = None;
    for value in values {
        match value {
            ComponentValue::Token(CssToken::Whitespace) => {}
            ComponentValue::Token(CssToken::Ident(word)) => {
                if word.eq_ignore_ascii_case("span") {
                    span = true;
                } else if word.eq_ignore_ascii_case("auto") {
                    return Some(Line::Auto);
                } else {
                    // A named line, which is not implemented.
                    return None;
                }
            }
            ComponentValue::Token(CssToken::Number {
                value,
                integer: true,
            }) => {
                number = Some(*value as i32);
            }
            _ => return None,
        }
    }
    match (span, number) {
        (true, Some(count)) if count > 0 => Some(Line::Span(count as u32)),
        (true, None) => Some(Line::Span(1)),
        (false, Some(number)) if number != 0 => Some(Line::Number(number)),
        _ => None,
    }
}

fn resolve(value: LengthPercentage, basis: f32) -> f32 {
    match value {
        LengthPercentage::Length(length) => length.value.max(0.0),
        LengthPercentage::Percentage(percent) => (percent / 100.0 * basis).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::{parse, Rule};

    /// The declared value of the one declaration in `css`.
    fn declared(css: &str) -> Vec<ComponentValue> {
        let sheet = parse(&format!("a {{ {css} }}"));
        let Some(Rule::Style(style)) = sheet.rules.first() else {
            panic!("not a style rule");
        };
        style.declarations[0].value.clone()
    }

    fn tracks(value: &str) -> Vec<Track> {
        Track::parse_list(&declared(&format!("grid-template-columns: {value}"))).expect("parses")
    }

    #[test]
    fn a_track_list_reads_lengths_and_fractions() {
        assert_eq!(
            tracks("1fr 84px"),
            vec![
                Track::Fraction(1.0),
                Track::Fixed(LengthPercentage::Length(
                    crate::engine::cryo_impl::css::value::Length::px(84.0)
                ))
            ]
        );
    }

    #[test]
    fn auto_is_its_own_track() {
        assert_eq!(tracks("auto 1fr"), vec![Track::Auto, Track::Fraction(1.0)]);
    }

    #[test]
    fn repeat_expands_to_that_many_tracks() {
        assert_eq!(tracks("repeat(3, 1fr)").len(), 3);
        assert!(tracks("repeat(3, 1fr)")
            .iter()
            .all(|track| *track == Track::Fraction(1.0)));
    }

    #[test]
    fn repeat_expands_a_pattern_of_several_tracks() {
        // Six tracks: the pair, three times.
        assert_eq!(tracks("repeat(3, 10px 1fr)").len(), 6);
    }

    #[test]
    fn minmax_keeps_both_ends() {
        // The form every real grid uses: a floor of nothing and a
        // flexible ceiling.
        let parsed = tracks("minmax(0,1fr)");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].fraction(), Some(1.0));
        assert_eq!(parsed[0].minimum(100.0), 0.0);
    }

    #[test]
    fn a_minmax_floor_is_honoured() {
        let parsed = tracks("minmax(110px,1fr)");
        assert_eq!(parsed[0].minimum(1000.0), 110.0);
        assert_eq!(parsed[0].fraction(), Some(1.0));
    }

    #[test]
    fn repeat_with_minmax_is_the_common_card_row() {
        let parsed = tracks("repeat(6,minmax(0,1fr))");
        assert_eq!(parsed.len(), 6);
        assert!(parsed.iter().all(|track| track.fraction() == Some(1.0)));
    }

    #[test]
    fn a_fixed_track_is_definite_and_a_flexible_one_is_not() {
        assert_eq!(tracks("84px")[0].definite(500.0), Some(84.0));
        assert_eq!(tracks("50%")[0].definite(500.0), Some(250.0));
        assert_eq!(tracks("1fr")[0].definite(500.0), None);
        assert_eq!(tracks("auto")[0].definite(500.0), None);
    }

    fn placement(value: &str) -> Placement {
        Placement::parse(&declared(&format!("grid-column: {value}"))).expect("parses")
    }

    #[test]
    fn two_line_numbers_name_a_range() {
        assert_eq!(placement("2 / 4").resolve(6), Some((1, 3)));
    }

    #[test]
    fn a_negative_line_counts_from_the_end() {
        // `1 / -1` is the whole row, whatever its width.
        assert_eq!(placement("1 / -1").resolve(6), Some((0, 6)));
        assert_eq!(placement("1 / -1").resolve(3), Some((0, 3)));
    }

    #[test]
    fn a_span_without_a_start_is_left_to_auto_placement() {
        assert_eq!(placement("span 2 / span 2").resolve(6), None);
        assert_eq!(placement("span 2 / span 2").span(), 2);
    }

    #[test]
    fn a_start_with_a_span_covers_that_many_tracks() {
        assert_eq!(placement("2 / span 3").resolve(6), Some((1, 4)));
    }

    #[test]
    fn a_plain_placement_spans_one_track() {
        assert_eq!(placement("3").resolve(6), Some((2, 3)));
        assert_eq!(placement("3").span(), 1);
    }

    #[test]
    fn auto_decides_nothing_by_itself() {
        assert_eq!(Placement::AUTO.resolve(6), None);
        assert_eq!(Placement::AUTO.span(), 1);
    }
}
