//! Colours: the named table, the hex and functional notations, and the
//! conversion to RGBA.
//!
//! One thing to get right up front: `currentColor` is not a colour. It
//! means "whatever this element's `color` computes to", which is not
//! known until the cascade has resolved `color` itself — and `color:
//! currentColor` inherits rather than looping. So parsing yields
//! [`SpecifiedColor`], which is either an absolute colour or that
//! deferral, and only the computed-value stage can collapse it.
//!
//! Alpha is kept as `f32` in 0..=1 rather than a byte, because
//! `rgba(0,0,0,0.004)` and `rgba(0,0,0,0)` are different and rounding to
//! 8 bits here would merge them before compositing ever sees them.

use crate::engine::cryo_impl::css::parser::ComponentValue;
use crate::engine::cryo_impl::css::tokenizer::CssToken;

/// An RGBA colour, components in 0..=255 and alpha in 0..=1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: f32,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0.0);
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self::rgba(red, green, blue, 1.0)
    }

    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: f32) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    const fn from_hex(packed: u32) -> Self {
        Self::rgb(
            ((packed >> 16) & 0xff) as u8,
            ((packed >> 8) & 0xff) as u8,
            (packed & 0xff) as u8,
        )
    }

    /// Whether this colour would paint nothing. Painting can skip it
    /// entirely, which matters because `transparent` is the initial
    /// background of every element on the page.
    pub fn is_transparent(self) -> bool {
        self.alpha <= 0.0
    }

    /// The same colour, dimmed by `factor`.
    ///
    /// Multiplies rather than replaces, because every opacity in CSS
    /// and SVG composes: a shape at `fill-opacity: 0.5` inside a group
    /// at `opacity: 0.5` is a quarter opaque, and its own `rgba()`
    /// alpha is a third factor again.
    pub fn with_alpha_scaled(self, factor: f32) -> Self {
        Self {
            alpha: (self.alpha * factor).clamp(0.0, 1.0),
            ..self
        }
    }
}

/// A colour as written, before `currentColor` can be resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpecifiedColor {
    Absolute(Color),
    /// `currentColor` — the element's own computed `color`.
    Current,
}

/// Parse a colour from a declaration's value.
///
/// `None` for anything unrecognised, which the cascade treats as an
/// invalid declaration and drops — so a colour function we do not
/// support yet leaves the inherited colour in place rather than
/// painting something wrong.
pub fn parse(values: &[ComponentValue]) -> Option<SpecifiedColor> {
    let mut meaningful = values.iter().filter(|value| !is_whitespace(value));
    let only = meaningful.next()?;
    if meaningful.next().is_some() {
        // A colour is one component value; more means something else.
        return None;
    }

    match only {
        ComponentValue::Token(CssToken::Ident(name)) => parse_keyword(name),
        ComponentValue::Token(CssToken::Hash { value, .. }) => {
            parse_hex(value).map(SpecifiedColor::Absolute)
        }
        ComponentValue::Function { name, arguments } => {
            parse_function(name, arguments).map(SpecifiedColor::Absolute)
        }
        _ => None,
    }
}

fn parse_keyword(name: &str) -> Option<SpecifiedColor> {
    if name.eq_ignore_ascii_case("transparent") {
        return Some(SpecifiedColor::Absolute(Color::TRANSPARENT));
    }
    if name.eq_ignore_ascii_case("currentcolor") {
        return Some(SpecifiedColor::Current);
    }
    named(name).map(SpecifiedColor::Absolute)
}

/// Look up a CSS named colour. Case-insensitive, as all CSS keywords are.
pub fn named(name: &str) -> Option<Color> {
    let lowered = name.to_ascii_lowercase();
    NAMED
        .iter()
        .find(|(candidate, _)| *candidate == lowered)
        .map(|(_, packed)| Color::from_hex(*packed))
}

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`.
///
/// The short forms double each digit rather than scaling: `#fff` is
/// `#ffffff`, not `#f0f0f0`, which is why `f` must become `ff`.
fn parse_hex(digits: &str) -> Option<Color> {
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let nibble = |index: usize| -> Option<u8> {
        digits
            .as_bytes()
            .get(index)
            .map(|b| (*b as char).to_digit(16).unwrap_or(0) as u8)
    };
    let byte = |index: usize| -> Option<u8> { Some(nibble(index)? << 4 | nibble(index + 1)?) };
    let doubled = |index: usize| -> Option<u8> {
        let value = nibble(index)?;
        Some(value << 4 | value)
    };

    match digits.len() {
        3 => Some(Color::rgb(doubled(0)?, doubled(1)?, doubled(2)?)),
        4 => Some(Color::rgba(
            doubled(0)?,
            doubled(1)?,
            doubled(2)?,
            f32::from(doubled(3)?) / 255.0,
        )),
        6 => Some(Color::rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color::rgba(
            byte(0)?,
            byte(2)?,
            byte(4)?,
            f32::from(byte(6)?) / 255.0,
        )),
        _ => None,
    }
}

fn parse_function(name: &str, arguments: &[ComponentValue]) -> Option<Color> {
    let lowered = name.to_ascii_lowercase();
    let parts = split_arguments(arguments);
    match lowered.as_str() {
        // `rgba` is a legacy alias: both take an optional alpha.
        "rgb" | "rgba" => parse_rgb(&parts),
        "hsl" | "hsla" => parse_hsl(&parts),
        _ => None,
    }
}

/// Split a function's arguments on commas *or* whitespace, and treat a
/// `/` as a separator too.
///
/// Modern CSS allows `rgb(0 0 0 / 50%)` as well as the legacy
/// `rgba(0, 0, 0, 0.5)`, and both appear in the wild. Normalising the
/// separators here means the two notations share one parser instead of
/// drifting apart.
fn split_arguments(arguments: &[ComponentValue]) -> Vec<CssToken> {
    arguments
        .iter()
        .filter_map(|value| match value {
            ComponentValue::Token(CssToken::Whitespace | CssToken::Comma) => None,
            ComponentValue::Token(CssToken::Delim('/')) => None,
            ComponentValue::Token(token) => Some(token.clone()),
            _ => None,
        })
        .collect()
}

fn parse_rgb(parts: &[CssToken]) -> Option<Color> {
    let (channels, alpha) = match parts {
        [r, g, b] => ([r, g, b], None),
        [r, g, b, a] => ([r, g, b], Some(a)),
        _ => return None,
    };
    let mut resolved = [0u8; 3];
    for (slot, token) in resolved.iter_mut().zip(channels) {
        *slot = rgb_channel(token)?;
    }
    Some(Color::rgba(
        resolved[0],
        resolved[1],
        resolved[2],
        match alpha {
            Some(token) => alpha_value(token)?,
            None => 1.0,
        },
    ))
}

/// One `rgb()` channel: a number in 0..=255 or a percentage, clamped.
///
/// Clamping rather than rejecting is deliberate — `rgb(300, 0, 0)` is
/// out of range but every browser paints it red.
fn rgb_channel(token: &CssToken) -> Option<u8> {
    let value = match token {
        CssToken::Number { value, .. } => *value,
        CssToken::Percentage(percent) => *percent / 100.0 * 255.0,
        _ => return None,
    };
    Some(value.round().clamp(0.0, 255.0) as u8)
}

/// An alpha: a number in 0..=1 or a percentage.
fn alpha_value(token: &CssToken) -> Option<f32> {
    let value = match token {
        CssToken::Number { value, .. } => *value as f32,
        CssToken::Percentage(percent) => *percent as f32 / 100.0,
        _ => return None,
    };
    Some(value.clamp(0.0, 1.0))
}

fn parse_hsl(parts: &[CssToken]) -> Option<Color> {
    let (hue, saturation, lightness, alpha) = match parts {
        [h, s, l] => (h, s, l, None),
        [h, s, l, a] => (h, s, l, Some(a)),
        _ => return None,
    };

    // Hue is an angle: bare numbers are degrees, and `deg`/`turn`/`rad`
    // are all spelled as dimensions.
    let degrees = match hue {
        CssToken::Number { value, .. } => *value as f32,
        CssToken::Dimension { value, unit } => to_degrees(*value as f32, unit)?,
        _ => return None,
    };
    let saturation = percent_fraction(saturation)?;
    let lightness = percent_fraction(lightness)?;
    let alpha = match alpha {
        Some(token) => alpha_value(token)?,
        None => 1.0,
    };

    let (red, green, blue) = hsl_to_rgb(degrees, saturation, lightness);
    Some(Color::rgba(red, green, blue, alpha))
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

/// A percentage as a 0..=1 fraction. `hsl()` requires percentages for
/// saturation and lightness, so a bare number is invalid.
fn percent_fraction(token: &CssToken) -> Option<f32> {
    match token {
        CssToken::Percentage(percent) => Some((*percent as f32 / 100.0).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// The standard HSL conversion, hue in degrees and the rest in 0..=1.
fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    // Hue is periodic, so wrap rather than clamp: 480deg is 120deg.
    let hue = hue.rem_euclid(360.0) / 60.0;
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let second = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
    let (red, green, blue) = match hue as u32 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let base = lightness - chroma / 2.0;
    let to_byte = |channel: f32| ((channel + base) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_byte(red), to_byte(green), to_byte(blue))
}

fn is_whitespace(value: &ComponentValue) -> bool {
    matches!(value, ComponentValue::Token(CssToken::Whitespace))
}

/// The CSS named colours.
///
/// The full list, not a subset: it is fixed, mechanical, and the cost of
/// a missing entry is a silently wrong colour on a real page.
static NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::parse_declarations;

    /// Parse `prop: <value>` and return the colour it specifies.
    fn colour(text: &str) -> Option<SpecifiedColor> {
        let declarations = parse_declarations(&format!("color: {text}"));
        parse(&declarations.first()?.value)
    }

    fn absolute(text: &str) -> Option<Color> {
        match colour(text)? {
            SpecifiedColor::Absolute(color) => Some(color),
            SpecifiedColor::Current => None,
        }
    }

    #[test]
    fn named_colours_resolve() {
        assert_eq!(absolute("red"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(absolute("white"), Some(Color::WHITE));
        assert_eq!(absolute("rebeccapurple"), Some(Color::rgb(102, 51, 153)));
        assert_eq!(absolute("tomato"), Some(Color::rgb(255, 99, 71)));
    }

    #[test]
    fn named_colours_are_case_insensitive() {
        assert_eq!(absolute("RED"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(absolute("DarkSlateGray"), Some(Color::rgb(47, 79, 79)));
    }

    #[test]
    fn both_spellings_of_grey_exist_and_agree() {
        assert_eq!(absolute("gray"), absolute("grey"));
        assert_eq!(absolute("darkgray"), absolute("darkgrey"));
    }

    #[test]
    fn an_unknown_name_is_not_a_colour() {
        assert_eq!(colour("blurple"), None);
        assert_eq!(colour("notacolour"), None);
    }

    #[test]
    fn transparent_is_black_with_zero_alpha() {
        let parsed = absolute("transparent").expect("transparent parses");
        assert_eq!(parsed, Color::TRANSPARENT);
        assert!(parsed.is_transparent());
    }

    #[test]
    fn current_color_defers_rather_than_resolving() {
        assert_eq!(colour("currentColor"), Some(SpecifiedColor::Current));
        assert_eq!(colour("CURRENTCOLOR"), Some(SpecifiedColor::Current));
    }

    #[test]
    fn short_hex_doubles_each_digit() {
        // `#fff` is white, not `#f0f0f0`.
        assert_eq!(absolute("#fff"), Some(Color::WHITE));
        assert_eq!(absolute("#abc"), Some(Color::rgb(0xaa, 0xbb, 0xcc)));
    }

    #[test]
    fn long_hex_reads_byte_pairs() {
        assert_eq!(absolute("#ff8000"), Some(Color::rgb(255, 128, 0)));
        assert_eq!(absolute("#000000"), Some(Color::BLACK));
    }

    #[test]
    fn hex_with_alpha_parses_in_both_lengths() {
        let short = absolute("#0000").expect("#0000 parses");
        assert!(short.is_transparent());
        let long = absolute("#ff000080").expect("#ff000080 parses");
        assert_eq!((long.red, long.green, long.blue), (255, 0, 0));
        assert!((long.alpha - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn hex_is_case_insensitive() {
        assert_eq!(absolute("#FF8000"), absolute("#ff8000"));
    }

    #[test]
    fn a_hex_of_the_wrong_length_is_invalid() {
        assert_eq!(colour("#ff"), None);
        assert_eq!(colour("#fffff"), None);
        assert_eq!(colour("#fffffff"), None);
    }

    #[test]
    fn rgb_accepts_the_legacy_and_modern_notations() {
        let want = Color::rgb(1, 2, 3);
        assert_eq!(absolute("rgb(1, 2, 3)"), Some(want));
        assert_eq!(absolute("rgb(1 2 3)"), Some(want));
        assert_eq!(absolute("rgba(1, 2, 3)"), Some(want));
    }

    #[test]
    fn rgb_alpha_parses_from_a_number_or_a_percentage() {
        let half = absolute("rgba(0, 0, 0, 0.5)").expect("parses");
        assert!((half.alpha - 0.5).abs() < 1e-6);
        let modern = absolute("rgb(0 0 0 / 50%)").expect("parses");
        assert!((modern.alpha - 0.5).abs() < 1e-6);
    }

    #[test]
    fn rgb_percentage_channels_scale_to_bytes() {
        assert_eq!(
            absolute("rgb(100%, 0%, 50%)"),
            Some(Color::rgb(255, 0, 128))
        );
    }

    #[test]
    fn out_of_range_channels_clamp_rather_than_failing() {
        // `rgb(300, -20, 0)` is invalid but every browser paints it.
        assert_eq!(absolute("rgb(300, -20, 0)"), Some(Color::rgb(255, 0, 0)));
        let over = absolute("rgba(0, 0, 0, 5)").expect("parses");
        assert_eq!(over.alpha, 1.0);
    }

    #[test]
    fn rgb_with_the_wrong_number_of_arguments_is_invalid() {
        assert_eq!(colour("rgb(1, 2)"), None);
        assert_eq!(colour("rgb(1, 2, 3, 4, 5)"), None);
        assert_eq!(colour("rgb()"), None);
    }

    #[test]
    fn hsl_converts_the_primaries() {
        assert_eq!(absolute("hsl(0, 100%, 50%)"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(absolute("hsl(120, 100%, 50%)"), Some(Color::rgb(0, 255, 0)));
        assert_eq!(absolute("hsl(240, 100%, 50%)"), Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn hsl_handles_the_achromatic_ends() {
        assert_eq!(absolute("hsl(0, 0%, 0%)"), Some(Color::BLACK));
        assert_eq!(absolute("hsl(0, 0%, 100%)"), Some(Color::WHITE));
        assert_eq!(
            absolute("hsl(90, 0%, 50%)"),
            Some(Color::rgb(128, 128, 128))
        );
    }

    #[test]
    fn hue_wraps_rather_than_clamping() {
        // 480deg is 120deg; clamping would give red instead of green.
        assert_eq!(
            absolute("hsl(480, 100%, 50%)"),
            absolute("hsl(120, 100%, 50%)")
        );
        assert_eq!(
            absolute("hsl(-120, 100%, 50%)"),
            absolute("hsl(240, 100%, 50%)")
        );
    }

    #[test]
    fn hue_accepts_angle_units() {
        let red = absolute("hsl(0, 100%, 50%)");
        assert_eq!(absolute("hsl(0deg, 100%, 50%)"), red);
        assert_eq!(absolute("hsl(1turn, 100%, 50%)"), red);
        assert_eq!(absolute("hsl(400grad, 100%, 50%)"), red);
    }

    #[test]
    fn hsl_requires_percentages_for_saturation_and_lightness() {
        assert_eq!(colour("hsl(0, 1, 0.5)"), None);
    }

    #[test]
    fn hsla_takes_an_alpha() {
        let parsed = absolute("hsla(0, 100%, 50%, 0.25)").expect("parses");
        assert_eq!((parsed.red, parsed.green, parsed.blue), (255, 0, 0));
        assert!((parsed.alpha - 0.25).abs() < 1e-6);
    }

    #[test]
    fn an_unknown_colour_function_is_invalid() {
        // Not yet supported, and painting a guess would be worse than
        // leaving the inherited colour alone.
        assert_eq!(colour("lab(50% 40 59)"), None);
        assert_eq!(colour("color-mix(in srgb, red, blue)"), None);
    }

    #[test]
    fn two_values_are_not_a_colour() {
        assert_eq!(colour("red blue"), None);
        assert_eq!(colour("1px solid red"), None);
    }

    #[test]
    fn whitespace_around_a_colour_is_ignored() {
        assert_eq!(absolute("  red  "), Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn the_named_table_has_no_duplicate_entries() {
        let mut names: Vec<&str> = NAMED.iter().map(|(name, _)| *name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "a name is listed twice");
    }

    #[test]
    fn the_named_table_is_sorted_so_it_stays_reviewable() {
        let names: Vec<&str> = NAMED.iter().map(|(name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn the_named_table_is_all_lowercase_so_lookup_can_fold_once() {
        for (name, _) in NAMED {
            assert_eq!(*name, name.to_ascii_lowercase(), "{name} is not folded");
        }
    }
}
