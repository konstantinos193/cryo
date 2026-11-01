//! Linear gradients.
//!
//! A gradient is a *background image*, not a colour, and that
//! distinction is why a page can have both: a gradient paints over the
//! background colour, and a colour shows through wherever the gradient
//! is transparent. A renderer that treats `background: linear-gradient(…)`
//! as "no background" — which is what happens when the value is simply
//! not understood — leaves white text on a white page, because the
//! author put that text on a dark panel that never got drawn.
//!
//! The geometry is the fiddly part. CSS measures a gradient's angle
//! **clockwise from pointing up**, so `0deg` runs bottom-to-top and
//! `90deg` runs left-to-right — the opposite sense from the maths
//! convention, and upside down relative to screen coordinates. The
//! gradient line is then sized so that the two corners nearest each end
//! land exactly on the first and last stop, which is what makes a
//! diagonal gradient reach the corners rather than stopping short.
//!
//! Not implemented: `radial-gradient`, `conic-gradient`, repeating
//! variants, colour interpolation hints and non-sRGB interpolation. Each
//! is rejected at parse time rather than approximated, so an
//! unsupported gradient leaves the background colour visible instead of
//! painting something wrong.

use super::super::parser::ComponentValue;
use super::super::tokenizer::CssToken;
use super::{color, Color, SpecifiedColor};

/// One colour along a gradient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorStop {
    pub color: Color,
    /// Where it sits, 0 at the start of the gradient line and 1 at the
    /// end. `None` until [`Gradient::resolve_positions`] spaces
    /// the unpositioned stops evenly between their neighbours.
    pub position: Option<f32>,
}

/// A linear gradient.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// Degrees clockwise from "up", as CSS measures it. Only a linear
    /// gradient uses it.
    pub angle: f32,
    /// Whether the colours run along a line or out from a point.
    pub shape: Shape,
    pub stops: Vec<ColorStop>,
}

/// The two gradient shapes, and what each needs to know.
///
/// They differ only in how a point becomes a position along the
/// stops: a linear gradient projects onto a line, a radial one
/// measures distance from a centre. Everything after that — finding
/// the surrounding stops and blending between them — is the same, so
/// they share a type rather than duplicating it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Linear,
    /// Centre as a fraction of the box, 0..1 on each axis.
    Radial {
        centre_x: f32,
        centre_y: f32,
    },
}

impl Gradient {
    /// Multiply every stop's alpha.
    ///
    /// A gradient under `opacity` cannot be faded as one colour, so
    /// each stop is faded and the interpolation between them comes
    /// out the same — the blend is linear in alpha either way.
    pub fn fade(&mut self, alpha: f32) {
        for stop in &mut self.stops {
            stop.color.alpha *= alpha;
        }
    }

    /// Parse `linear-gradient(...)` or `radial-gradient(...)`, or
    /// `None` for anything else.
    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        let (radial, function) = values.iter().find_map(|value| match value {
            ComponentValue::Function { name, arguments }
                if name.eq_ignore_ascii_case("linear-gradient") =>
            {
                Some((false, arguments))
            }
            ComponentValue::Function { name, arguments }
                if name.eq_ignore_ascii_case("radial-gradient") =>
            {
                Some((true, arguments))
            }
            _ => None,
        })?;

        let parts = split_on_commas(function);
        if parts.len() < 2 {
            // A gradient needs a direction or two stops; one part can
            // only be a single colour, which is not a gradient.
            return None;
        }

        // The first part is a direction (or, for a radial gradient, a
        // shape and position) only if it says so; otherwise every
        // part is a stop and the defaults apply.
        let (angle, shape, stop_parts) = if radial {
            match parse_radial_position(&parts[0]) {
                Some(shape) => (0.0, shape, &parts[1..]),
                // The CSS default: centred.
                None => (
                    0.0,
                    Shape::Radial {
                        centre_x: 0.5,
                        centre_y: 0.5,
                    },
                    &parts[..],
                ),
            }
        } else {
            match parse_direction(&parts[0]) {
                Some(angle) => (angle, Shape::Linear, &parts[1..]),
                // The CSS default: top to bottom.
                None => (180.0, Shape::Linear, &parts[..]),
            }
        };

        let mut stops: Vec<ColorStop> = Vec::new();
        for part in stop_parts {
            stops.push(parse_stop(part)?);
        }
        if stops.len() < 2 {
            return None;
        }

        let mut gradient = Self {
            angle,
            shape,
            stops,
        };
        gradient.resolve_positions();
        Some(gradient)
    }

    /// Give every stop a position.
    ///
    /// The first and last default to 0 and 1, and a run of unpositioned
    /// stops is spread evenly between the positioned ones either side.
    /// Positions also never decrease: a stop earlier than the one
    /// before it is clamped, which is how CSS makes a hard colour
    /// change out of two stops at the same place.
    fn resolve_positions(&mut self) {
        let count = self.stops.len();
        if count == 0 {
            return;
        }
        if self.stops[0].position.is_none() {
            self.stops[0].position = Some(0.0);
        }
        if self.stops[count - 1].position.is_none() {
            self.stops[count - 1].position = Some(1.0);
        }

        let mut index = 0;
        while index < count {
            if self.stops[index].position.is_some() {
                index += 1;
                continue;
            }
            // Find the next positioned stop and divide the gap.
            let gap_start = index - 1;
            let mut gap_end = index;
            while gap_end < count && self.stops[gap_end].position.is_none() {
                gap_end += 1;
            }
            let from = self.stops[gap_start].position.unwrap_or(0.0);
            let to = self
                .stops
                .get(gap_end)
                .and_then(|s| s.position)
                .unwrap_or(1.0);
            let steps = (gap_end - gap_start) as f32;
            for (offset, slot) in (gap_start + 1..gap_end).enumerate() {
                let fraction = (offset + 1) as f32 / steps;
                self.stops[slot].position = Some(from + (to - from) * fraction);
            }
            index = gap_end;
        }

        // Monotonic, so a later stop never precedes an earlier one.
        let mut highest = 0.0f32;
        for stop in &mut self.stops {
            let position = stop.position.unwrap_or(0.0).max(highest);
            stop.position = Some(position);
            highest = position;
        }
    }

    /// The colour at `t` along the gradient line, `t` in 0..=1.
    pub fn sample(&self, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let Some(first) = self.stops.first() else {
            return Color::TRANSPARENT;
        };
        if t <= first.position.unwrap_or(0.0) {
            return first.color;
        }
        let Some(last) = self.stops.last() else {
            return Color::TRANSPARENT;
        };
        if t >= last.position.unwrap_or(1.0) {
            return last.color;
        }

        for pair in self.stops.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            let start = from.position.unwrap_or(0.0);
            let end = to.position.unwrap_or(1.0);
            if t < start || t > end {
                continue;
            }
            // Two stops at the same place make a hard edge, not a
            // division by zero.
            if end <= start {
                return to.color;
            }
            return mix(from.color, to.color, (t - start) / (end - start));
        }
        last.color
    }

    /// Where a point falls along the gradient line of a box, as 0..=1.
    ///
    /// `(x, y)` is relative to the box's top-left, in pixels.
    pub fn progress(&self, x: f32, y: f32, width: f32, height: f32) -> f32 {
        if width <= 0.0 || height <= 0.0 {
            return 0.0;
        }
        if let Shape::Radial { centre_x, centre_y } = self.shape {
            // Distance from the centre, over the distance to the
            // furthest corner — which is `farthest-corner`, the
            // default and the only sizing keyword read here.
            let (cx, cy) = (centre_x * width, centre_y * height);
            let furthest = [(0.0, 0.0), (width, 0.0), (0.0, height), (width, height)]
                .iter()
                .map(|&(x, y)| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt())
                .fold(0.0f32, f32::max);
            if furthest <= 0.0 {
                return 0.0;
            }
            return (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / furthest).clamp(0.0, 1.0);
        }
        // CSS angles run clockwise from up; screen Y runs downward, so
        // the direction vector is (sin, -cos) negated in Y to match.
        let radians = self.angle.to_radians();
        let (direction_x, direction_y) = (radians.sin(), -radians.cos());

        // The gradient line is long enough that the corners nearest
        // each end sit exactly on the first and last stop.
        let length = (width * direction_x).abs() + (height * direction_y).abs();
        if length <= 0.0 {
            return 0.0;
        }
        let (centre_x, centre_y) = (width / 2.0, height / 2.0);
        let along = (x - centre_x) * direction_x + (y - centre_y) * direction_y;
        (along / length + 0.5).clamp(0.0, 1.0)
    }

    /// Whether this gradient would paint nothing at all.
    pub fn is_transparent(&self) -> bool {
        self.stops.iter().all(|stop| stop.color.is_transparent())
    }
}

/// Blend two colours, `t` from 0 at `from` to 1 at `to`.
fn mix(from: Color, to: Color, t: f32) -> Color {
    let channel = |a: u8, b: u8| {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color {
        red: channel(from.red, to.red),
        green: channel(from.green, to.green),
        blue: channel(from.blue, to.blue),
        alpha: from.alpha + (to.alpha - from.alpha) * t,
    }
}

/// `to right`, `to bottom left`, `45deg`, `0.25turn`.
///
/// `None` when the part is not a direction at all, which means the
/// gradient started straight in on its colour stops.
/// A radial gradient's first part: `circle at 30% 25%` and friends.
///
/// Only the position is read. The shape keyword (`circle`/`ellipse`)
/// and the sizing one (`farthest-corner` and the rest) are accepted
/// and ignored — every gradient here is sized to the farthest corner,
/// which is the default and what the decorative ones on a page use.
///
/// Returns `None` when the part is a colour stop instead, which is
/// how the caller tells the two apart.
fn parse_radial_position(part: &[ComponentValue]) -> Option<Shape> {
    let mut words = Vec::new();
    let mut percentages = Vec::new();
    for value in part {
        match value {
            ComponentValue::Token(CssToken::Whitespace) => {}
            ComponentValue::Token(CssToken::Ident(word)) => words.push(word.to_ascii_lowercase()),
            ComponentValue::Token(CssToken::Percentage(percent)) => {
                percentages.push(*percent as f32 / 100.0);
            }
            // A length or a colour means this is a stop, not a
            // position.
            _ => return None,
        }
    }

    const SHAPES: [&str; 6] = [
        "circle",
        "ellipse",
        "at",
        "closest-side",
        "closest-corner",
        "farthest-side",
    ];
    const PLACES: [&str; 5] = ["at", "center", "centre", "left", "top"];
    // Every word has to be one this grammar allows, or the part is a
    // colour keyword and belongs to the stops.
    if !words.iter().all(|word| {
        SHAPES.contains(&word.as_str())
            || PLACES.contains(&word.as_str())
            || word == "farthest-corner"
            || word == "right"
            || word == "bottom"
    }) {
        return None;
    }
    if words.is_empty() && percentages.is_empty() {
        return None;
    }

    // `at 30% 25%`; one percentage alone sets the horizontal and
    // centres vertically, which is what the shorthand means.
    let centre_x = percentages.first().copied().unwrap_or(0.5);
    let centre_y = percentages.get(1).copied().unwrap_or(0.5);
    Some(Shape::Radial { centre_x, centre_y })
}

fn parse_direction(part: &[ComponentValue]) -> Option<f32> {
    let tokens = meaningful(part);
    match tokens.as_slice() {
        [CssToken::Dimension { value, unit }] => to_degrees(*value as f32, unit),
        [CssToken::Ident(keyword), sides @ ..] if keyword.eq_ignore_ascii_case("to") => {
            let names: Vec<String> = sides
                .iter()
                .filter_map(|token| match token {
                    CssToken::Ident(name) => Some(name.to_ascii_lowercase()),
                    _ => None,
                })
                .collect();
            side_angle(&names)
        }
        _ => None,
    }
}

/// The angle a `to <side-or-corner>` names.
///
/// Corners are the diagonals. CSS actually aims a corner gradient so
/// the perpendicular through the opposite corner is level, which
/// depends on the box's shape; 45-degree steps are the square case and
/// are close enough until a box proves otherwise.
fn side_angle(names: &[String]) -> Option<f32> {
    let has = |name: &str| names.iter().any(|value| value == name);
    let (top, bottom) = (has("top"), has("bottom"));
    let (left, right) = (has("left"), has("right"));
    Some(match (top, right, bottom, left) {
        (true, false, false, false) => 0.0,
        (true, true, false, false) => 45.0,
        (false, true, false, false) => 90.0,
        (false, true, true, false) => 135.0,
        (false, false, true, false) => 180.0,
        (false, false, true, true) => 225.0,
        (false, false, false, true) => 270.0,
        (true, false, false, true) => 315.0,
        _ => return None,
    })
}

fn to_degrees(value: f32, unit: &str) -> Option<f32> {
    let lowered = unit.to_ascii_lowercase();
    Some(match lowered.as_str() {
        "deg" => value,
        "grad" => value * 360.0 / 400.0,
        "rad" => value.to_degrees(),
        "turn" => value * 360.0,
        _ => return None,
    })
}

/// One `<colour> [<position>]` stop.
fn parse_stop(part: &[ComponentValue]) -> Option<ColorStop> {
    // The colour may be a function, so it cannot be flattened away.
    let colour_end = part
        .iter()
        .position(|value| {
            matches!(
                value,
                ComponentValue::Token(CssToken::Percentage(_) | CssToken::Dimension { .. })
            )
        })
        .unwrap_or(part.len());
    let (colour_part, position_part) = part.split_at(colour_end);

    let color = match color::parse(colour_part)? {
        SpecifiedColor::Absolute(value) => value,
        // `currentColor` in a gradient needs the element's colour,
        // which this stage does not have; rejecting leaves the
        // background colour visible rather than painting black.
        SpecifiedColor::Current => return None,
    };

    let position = match meaningful(position_part).first() {
        Some(CssToken::Percentage(percent)) => Some(*percent as f32 / 100.0),
        // A length stop needs the box's size to resolve; treating it as
        // unpositioned spaces it evenly, which is wrong but visible
        // rather than invisible.
        Some(CssToken::Dimension { .. }) => None,
        _ => None,
    };
    Some(ColorStop { color, position })
}

/// Split component values on top-level commas.
fn split_on_commas(values: &[ComponentValue]) -> Vec<Vec<ComponentValue>> {
    let mut parts = vec![Vec::new()];
    for value in values {
        if *value == ComponentValue::Token(CssToken::Comma) {
            parts.push(Vec::new());
        } else {
            parts
                .last_mut()
                .expect("always one part")
                .push(value.clone());
        }
    }
    parts.retain(|part| !meaningful(part).is_empty() || part.iter().any(is_function));
    parts
}

fn is_function(value: &ComponentValue) -> bool {
    matches!(value, ComponentValue::Function { .. })
}

/// Plain tokens with whitespace dropped.
fn meaningful(values: &[ComponentValue]) -> Vec<CssToken> {
    values
        .iter()
        .filter_map(|value| match value {
            ComponentValue::Token(CssToken::Whitespace) => None,
            ComponentValue::Token(token) => Some(token.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::parse_declarations;

    fn values(text: &str) -> Vec<ComponentValue> {
        parse_declarations(&format!("background-image: {text}"))
            .into_iter()
            .next()
            .map(|declaration| declaration.value)
            .unwrap_or_default()
    }

    fn parse(text: &str) -> Option<Gradient> {
        Gradient::parse(&values(text))
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 0.01
    }

    const RED: Color = Color::rgb(255, 0, 0);
    const BLUE: Color = Color::rgb(0, 0, 255);

    #[test]
    fn a_two_stop_gradient_parses() {
        let gradient = parse("linear-gradient(red, blue)").expect("parses");
        assert_eq!(gradient.stops.len(), 2);
        assert_eq!(gradient.stops[0].color, RED);
        assert_eq!(gradient.stops[1].color, BLUE);
        assert!(close(gradient.angle, 180.0), "the default is downward");
    }

    #[test]
    fn an_angle_is_read_in_every_unit() {
        assert!(close(
            parse("linear-gradient(90deg, red, blue)").expect("x").angle,
            90.0
        ));
        assert!(close(
            parse("linear-gradient(0.25turn, red, blue)")
                .expect("x")
                .angle,
            90.0
        ));
        assert!(close(
            parse("linear-gradient(100grad, red, blue)")
                .expect("x")
                .angle,
            90.0
        ));
    }

    #[test]
    fn side_keywords_map_to_angles() {
        for (text, want) in [
            ("to top", 0.0),
            ("to right", 90.0),
            ("to bottom", 180.0),
            ("to left", 270.0),
            ("to top right", 45.0),
            ("to bottom left", 225.0),
        ] {
            let gradient = parse(&format!("linear-gradient({text}, red, blue)")).expect("parses");
            assert!(
                close(gradient.angle, want),
                "{text} gave {}",
                gradient.angle
            );
        }
    }

    #[test]
    fn the_first_and_last_stops_sit_at_the_ends() {
        let gradient = parse("linear-gradient(red, blue)").expect("parses");
        assert_eq!(gradient.stops[0].position, Some(0.0));
        assert_eq!(gradient.stops[1].position, Some(1.0));
    }

    #[test]
    fn unpositioned_stops_are_spread_evenly() {
        let gradient = parse("linear-gradient(red, lime, blue)").expect("parses");
        assert!(close(gradient.stops[1].position.expect("set"), 0.5));
    }

    #[test]
    fn several_unpositioned_stops_divide_the_gap() {
        let gradient = parse("linear-gradient(red, lime, white, blue)").expect("parses");
        assert!(close(gradient.stops[1].position.expect("set"), 1.0 / 3.0));
        assert!(close(gradient.stops[2].position.expect("set"), 2.0 / 3.0));
    }

    #[test]
    fn explicit_percentages_are_honoured() {
        let gradient = parse("linear-gradient(red 20%, blue 80%)").expect("parses");
        assert!(close(gradient.stops[0].position.expect("set"), 0.2));
        assert!(close(gradient.stops[1].position.expect("set"), 0.8));
    }

    #[test]
    fn positions_never_go_backwards() {
        // Two stops at one place are a hard edge, not an error.
        let gradient = parse("linear-gradient(red 60%, blue 20%)").expect("parses");
        let first = gradient.stops[0].position.expect("set");
        let second = gradient.stops[1].position.expect("set");
        assert!(second >= first, "{first} then {second}");
    }

    #[test]
    fn sampling_gives_the_end_colours_at_the_ends() {
        let gradient = parse("linear-gradient(red, blue)").expect("parses");
        assert_eq!(gradient.sample(0.0), RED);
        assert_eq!(gradient.sample(1.0), BLUE);
        // And clamps beyond them.
        assert_eq!(gradient.sample(-5.0), RED);
        assert_eq!(gradient.sample(9.0), BLUE);
    }

    #[test]
    fn sampling_interpolates_between_stops() {
        let gradient = parse("linear-gradient(black, white)").expect("parses");
        let middle = gradient.sample(0.5);
        assert!(
            (120..=135).contains(&middle.red),
            "expected mid grey, got {}",
            middle.red
        );
    }

    #[test]
    fn sampling_interpolates_alpha_too() {
        let gradient = parse("linear-gradient(rgba(0,0,0,0), rgba(0,0,0,1))").expect("parses");
        assert!(close(gradient.sample(0.5).alpha, 0.5));
    }

    #[test]
    fn a_hard_edge_switches_colour_without_blending() {
        let gradient = parse("linear-gradient(red 50%, blue 50%)").expect("parses");
        assert_eq!(gradient.sample(0.25), RED);
        assert_eq!(gradient.sample(0.75), BLUE);
    }

    #[test]
    fn a_downward_gradient_runs_top_to_bottom() {
        let gradient = parse("linear-gradient(to bottom, red, blue)").expect("parses");
        let top = gradient.progress(50.0, 0.0, 100.0, 100.0);
        let bottom = gradient.progress(50.0, 100.0, 100.0, 100.0);
        assert!(top < bottom, "{top} should precede {bottom}");
        assert!(close(top, 0.0) && close(bottom, 1.0));
    }

    #[test]
    fn an_upward_gradient_runs_the_other_way() {
        // The CSS convention trap: 0deg points up, not right.
        let gradient = parse("linear-gradient(to top, red, blue)").expect("parses");
        assert!(close(gradient.progress(50.0, 100.0, 100.0, 100.0), 0.0));
        assert!(close(gradient.progress(50.0, 0.0, 100.0, 100.0), 1.0));
    }

    #[test]
    fn a_rightward_gradient_runs_left_to_right() {
        let gradient = parse("linear-gradient(to right, red, blue)").expect("parses");
        assert!(close(gradient.progress(0.0, 50.0, 100.0, 100.0), 0.0));
        assert!(close(gradient.progress(100.0, 50.0, 100.0, 100.0), 1.0));
    }

    #[test]
    fn the_centre_of_a_box_is_always_halfway_along() {
        for angle in [0.0, 45.0, 90.0, 137.0, 180.0, 300.0] {
            let gradient =
                parse(&format!("linear-gradient({angle}deg, red, blue)")).expect("parses");
            assert!(
                close(gradient.progress(50.0, 25.0, 100.0, 50.0), 0.5),
                "at {angle}deg"
            );
        }
    }

    #[test]
    fn a_diagonal_gradient_reaches_both_corners() {
        let gradient = parse("linear-gradient(to bottom right, red, blue)").expect("parses");
        assert!(close(gradient.progress(0.0, 0.0, 100.0, 100.0), 0.0));
        assert!(close(gradient.progress(100.0, 100.0, 100.0, 100.0), 1.0));
    }

    #[test]
    fn a_zero_sized_box_does_not_divide_by_zero() {
        let gradient = parse("linear-gradient(red, blue)").expect("parses");
        assert!(close(gradient.progress(0.0, 0.0, 0.0, 0.0), 0.0));
        assert!(close(gradient.progress(5.0, 5.0, 10.0, 0.0), 0.0));
    }

    #[test]
    fn something_that_is_not_a_gradient_does_not_parse() {
        assert!(parse("red").is_none());
        assert!(parse("url(a.png)").is_none());
        assert!(parse("none").is_none());
        assert!(
            parse("linear-gradient(red)").is_none(),
            "one stop is not a gradient"
        );
    }

    #[test]
    fn unsupported_gradient_kinds_are_rejected_not_approximated() {
        // Better to show the background colour than to paint a guess.
        assert!(parse("conic-gradient(red, blue)").is_none());
        assert!(parse("repeating-linear-gradient(red, blue)").is_none());
    }

    #[test]
    fn a_radial_gradient_defaults_to_centred() {
        let gradient = parse("radial-gradient(red, blue)").expect("parses");
        assert_eq!(
            gradient.shape,
            Shape::Radial {
                centre_x: 0.5,
                centre_y: 0.5
            }
        );
        assert_eq!(gradient.stops.len(), 2);
    }

    #[test]
    fn a_radial_gradient_reads_its_position() {
        // The form a decorative blob is written in.
        let gradient =
            parse("radial-gradient(circle at 30% 25%, #fff 0%, #000 100%)").expect("parses");
        assert_eq!(
            gradient.shape,
            Shape::Radial {
                centre_x: 0.3,
                centre_y: 0.25
            }
        );
    }

    #[test]
    fn a_radial_gradient_runs_outward_from_its_centre() {
        let gradient = parse("radial-gradient(red, blue)").expect("parses");
        // At the centre of a 100x100 box, nothing; at a corner, all.
        assert!(gradient.progress(50.0, 50.0, 100.0, 100.0) < 0.01);
        assert!(gradient.progress(0.0, 0.0, 100.0, 100.0) > 0.99);
        // Halfway to a corner is halfway along the stops.
        let middle = gradient.progress(25.0, 25.0, 100.0, 100.0);
        assert!((middle - 0.5).abs() < 0.01, "{middle}");
    }

    #[test]
    fn a_linear_gradient_still_runs_along_its_angle() {
        let gradient = parse("linear-gradient(red, blue)").expect("parses");
        assert_eq!(gradient.shape, Shape::Linear);
        assert!(gradient.progress(50.0, 0.0, 100.0, 100.0) < 0.01, "top");
        assert!(
            gradient.progress(50.0, 100.0, 100.0, 100.0) > 0.99,
            "bottom"
        );
    }

    #[test]
    fn fading_a_gradient_fades_every_stop() {
        let mut gradient = parse("linear-gradient(red, blue)").expect("parses");
        gradient.fade(0.5);
        assert!(gradient
            .stops
            .iter()
            .all(|stop| (stop.color.alpha - 0.5).abs() < 1e-6));
    }

    #[test]
    fn a_gradient_with_a_colour_function_stop_parses() {
        let gradient =
            parse("linear-gradient(rgb(255, 0, 0), hsl(240, 100%, 50%))").expect("parses");
        assert_eq!(gradient.stops[0].color, RED);
        assert_eq!(gradient.stops[1].color, BLUE);
    }

    #[test]
    fn a_fully_transparent_gradient_knows_it_paints_nothing() {
        let gradient = parse("linear-gradient(transparent, transparent)").expect("parses");
        assert!(gradient.is_transparent());
        assert!(!parse("linear-gradient(red, blue)")
            .expect("parses")
            .is_transparent());
    }

    #[test]
    fn a_three_stop_gradient_with_a_middle_position_parses() {
        // The shape real pages use for a highlight.
        let gradient = parse("linear-gradient(135deg, #11304a 0%, #1d5c8a 60%, #11304a 100%)")
            .expect("parses");
        assert_eq!(gradient.stops.len(), 3);
        assert!(close(gradient.stops[1].position.expect("set"), 0.6));
        assert!(close(gradient.angle, 135.0));
    }
}
