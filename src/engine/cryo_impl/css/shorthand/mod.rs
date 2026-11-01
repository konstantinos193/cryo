//! Shorthand expansion: `margin: 0 auto` becomes four longhands.
//!
//! This has to happen **before** the cascade, not after, and that is the
//! whole point of the module. CSS cascades per longhand, so:
//!
//! ```css
//!   p { margin: 0; margin-left: 5px }
//! ```
//!
//! must leave `margin-left` at `5px` and the rest at `0`. If the cascade
//! saw `margin` and `margin-left` as two unrelated properties, both
//! would "win" and whichever stage read them later would have to know
//! that one overrides part of the other — knowledge that would then live
//! in every reader. Expanding at insertion means the cascade only ever
//! handles longhands and the rule needs no special case.
//!
//! The one-to-four distribution is CSS's own, and the three-value case
//! is the one people misremember:
//!
//! | values | top | right | bottom | left |
//! |--------|-----|-------|--------|------|
//! | `a`         | a | a | a | a |
//! | `a b`       | a | b | a | b |
//! | `a b c`     | a | b | c | b |
//! | `a b c d`   | a | b | c | d |
//!
//! Not expanded yet: `font`, and `background` beyond a plain colour.
//! Both are multi-value grammars with their own reset semantics, and a
//! half-right expansion of either would be worse than none — a
//! `background: url(x)` that silently cleared the colour, say. They are
//! left unexpanded, so they currently style nothing.

mod logical;

use super::parser::{ComponentValue, Declaration};
use super::tokenizer::CssToken;
use super::value::{color, BackgroundBox, SpecifiedColor};

/// The four box sides, in CSS's clockwise order.
const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// The four corners, also clockwise, starting from the top left.
///
/// They take the same one-to-four distribution as the sides, which is
/// why [`distribute`] serves both: with three values the second one is
/// reused for the fourth either way.
const CORNERS: [&str; 4] = ["top-left", "top-right", "bottom-right", "bottom-left"];

/// `border-style` keywords. Needed to tell the parts of
/// `border: 1px solid red` apart, since they may be written in any order.
const BORDER_STYLES: [&str; 9] = [
    "none", "hidden", "dotted", "dashed", "solid", "double", "groove", "ridge", "outset",
];

/// `border-width` keywords, which stand in for lengths.
const BORDER_WIDTHS: [&str; 3] = ["thin", "medium", "thick"];

/// Expand a declaration into longhands.
///
/// A longhand expands to itself, so every caller can pipe every
/// declaration through this and stop caring which it had. Expansion is
/// recursive: `border` becomes `border-width`/`style`/`color`, each of
/// which becomes four longhands.
pub fn expand(declaration: &Declaration) -> Vec<Declaration> {
    // A shorthand whose value contains a `var()` cannot be taken
    // apart yet: `border: 1px solid var(--line)` has three parts only
    // once the custom property is known, and guessing which is which
    // before then would drop the whole declaration. It is held back
    // and expanded after substitution instead — see
    // `DeclaredValues::substituted`.
    if holds_a_var(&declaration.value) {
        return vec![declaration.clone()];
    }
    let mut out = Vec::new();
    expand_into(declaration, &mut out, 0);
    out
}

/// Whether a value contains a `var()` anywhere inside it.
fn holds_a_var(values: &[ComponentValue]) -> bool {
    values.iter().any(|value| match value {
        ComponentValue::Function { name, arguments } => {
            name.eq_ignore_ascii_case("var") || holds_a_var(arguments)
        }
        ComponentValue::Block { values, .. } => holds_a_var(values),
        ComponentValue::Token(_) => false,
    })
}

/// Expand every declaration in a block, in order.
pub fn expand_all(declarations: &[Declaration]) -> Vec<Declaration> {
    declarations.iter().flat_map(expand).collect()
}

/// How many times expansion may recurse. `border` -> `border-width` ->
/// `border-top-width` is two levels; the bound exists so a future
/// shorthand that expanded to itself could not loop.
const MAX_DEPTH: u8 = 4;

fn expand_into(declaration: &Declaration, out: &mut Vec<Declaration>, depth: u8) {
    if depth >= MAX_DEPTH {
        out.push(declaration.clone());
        return;
    }
    let Some(expansion) = expand_once(declaration) else {
        out.push(declaration.clone());
        return;
    };
    for inner in &expansion {
        expand_into(inner, out, depth + 1);
    }
}

/// One level of expansion, or `None` if this is not a shorthand we know.
fn expand_once(declaration: &Declaration) -> Option<Vec<Declaration>> {
    let parts = split_parts(&declaration.value);
    match declaration.name.as_str() {
        // `inset`'s longhands are the bare side names, which is why
        // `per_side` has to cope with an empty prefix.
        "inset" => Some(per_side("", "", &parts, declaration.important)?),
        // The logical properties, which a current toolchain writes in
        // preference to the physical ones — see [`logical`].
        _ if logical::is_logical(&declaration.name) => logical::expand(declaration, &parts),
        "margin" => Some(per_side("margin", "", &parts, declaration.important)?),
        "padding" => Some(per_side("padding", "", &parts, declaration.important)?),
        "border-width" => Some(per_side("border", "-width", &parts, declaration.important)?),
        "border-style" => Some(per_side("border", "-style", &parts, declaration.important)?),
        "border-color" => Some(per_side("border", "-color", &parts, declaration.important)?),
        "border" => expand_border(&parts, declaration.important),
        "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let side = declaration.name.trim_start_matches("border-");
            expand_border_side(side, &parts, declaration.important)
        }
        "border-radius" => expand_radius(&parts, declaration.important),
        "overflow" => expand_pair("overflow-x", "overflow-y", &parts, declaration.important),
        "gap" => expand_pair("row-gap", "column-gap", &parts, declaration.important),
        "background" => expand_background(&declaration.value, declaration.important),
        _ => None,
    }
}

/// Distribute one to four values over the four sides.
fn per_side(
    prefix: &str,
    suffix: &str,
    parts: &[Vec<ComponentValue>],
    important: bool,
) -> Option<Vec<Declaration>> {
    let spread = distribute(parts)?;
    Some(
        SIDES
            .iter()
            .zip(spread)
            .map(|(side, value)| Declaration {
                name: longhand(prefix, side, suffix),
                value: value.clone(),
                important,
            })
            .collect(),
    )
}

/// `border-radius: 8px` and its one-to-four forms.
///
/// The elliptical form, `border-radius: 40px / 10px`, gives each
/// corner a horizontal and a vertical radius. Only the horizontal half
/// is read here — the part before the slash — because the rasteriser
/// draws circular corners. A stated approximation: an elliptical
/// corner comes out round rather than being dropped.
fn expand_radius(parts: &[Vec<ComponentValue>], important: bool) -> Option<Vec<Declaration>> {
    let horizontal: Vec<Vec<ComponentValue>> = parts
        .iter()
        .take_while(|part| {
            !matches!(
                part.first(),
                Some(ComponentValue::Token(CssToken::Delim('/')))
            )
        })
        .cloned()
        .collect();
    let spread = distribute(&horizontal)?;
    Some(
        CORNERS
            .iter()
            .zip(spread)
            .map(|(corner, value)| Declaration {
                name: format!("border-{corner}-radius"),
                value: value.clone(),
                important,
            })
            .collect(),
    )
}

/// One side's longhand name.
///
/// `inset` is the shorthand whose longhands have no prefix at all —
/// they are plain `top`, `right`, `bottom`, `left` — so the hyphen
/// cannot be written into the format string.
pub(super) fn longhand(prefix: &str, side: &str, suffix: &str) -> String {
    if prefix.is_empty() {
        format!("{side}{suffix}")
    } else {
        format!("{prefix}-{side}{suffix}")
    }
}

/// The one-to-four rule. See the table in the module docs.
fn distribute(parts: &[Vec<ComponentValue>]) -> Option<[&Vec<ComponentValue>; 4]> {
    match parts {
        [all] => Some([all, all, all, all]),
        [block, inline] => Some([block, inline, block, inline]),
        [top, inline, bottom] => Some([top, inline, bottom, inline]),
        [top, right, bottom, left] => Some([top, right, bottom, left]),
        // Zero or more than four values is not a valid box shorthand.
        _ => None,
    }
}

/// `overflow: hidden` and `overflow: hidden scroll`.
fn expand_pair(
    first_name: &str,
    second_name: &str,
    parts: &[Vec<ComponentValue>],
    important: bool,
) -> Option<Vec<Declaration>> {
    let (first, second) = match parts {
        [both] => (both, both),
        [first, second] => (first, second),
        _ => return None,
    };
    Some(vec![
        Declaration {
            name: first_name.to_owned(),
            value: first.clone(),
            important,
        },
        Declaration {
            name: second_name.to_owned(),
            value: second.clone(),
            important,
        },
    ])
}

/// `border: 1px solid red`, whose parts may come in any order.
///
/// Each part is classified by what it *is* rather than by position: a
/// length or width keyword is the width, a style keyword is the style,
/// and a colour is the colour. Omitted parts fall back to the border
/// initial values, because the shorthand resets what it does not state —
/// `border: red` really does set the width to medium and the style to
/// none, which is why it paints nothing.
fn expand_border(parts: &[Vec<ComponentValue>], important: bool) -> Option<Vec<Declaration>> {
    border_parts(parts, important, None)
}

/// `border-left: 1px solid red` and its three siblings.
///
/// A per-side border is as common in real stylesheets as the
/// all-round one — a card with a single dividing line writes
/// `border-bottom` — and leaving it unexpanded drops it entirely.
fn expand_border_side(
    side: &str,
    parts: &[Vec<ComponentValue>],
    important: bool,
) -> Option<Vec<Declaration>> {
    border_parts(parts, important, Some(side))
}

/// Pull width, style and colour out of a border shorthand.
///
/// `side` is `None` for `border`, which sets all four at once, or the
/// one side a `border-left` names.
fn border_parts(
    parts: &[Vec<ComponentValue>],
    important: bool,
    side: Option<&str>,
) -> Option<Vec<Declaration>> {
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut width = None;
    let mut style = None;
    let mut colour = None;

    for part in parts {
        if style.is_none() && is_border_style(part) {
            style = Some(part.clone());
        } else if width.is_none() && is_border_width(part) {
            width = Some(part.clone());
        } else if colour.is_none() && is_colour(part) {
            colour = Some(part.clone());
        } else {
            // An unclassifiable part makes the whole shorthand invalid.
            return None;
        }
    }

    // Without a side this yields `border-width` and friends, which
    // expand again into the four sides; with one it names that side's
    // longhands directly.
    let name = |part: &str| match side {
        Some(side) => format!("border-{side}-{part}"),
        None => format!("border-{part}"),
    };
    Some(vec![
        Declaration {
            name: name("width"),
            value: width.unwrap_or_else(|| ident("medium")),
            important,
        },
        Declaration {
            name: name("style"),
            value: style.unwrap_or_else(|| ident("none")),
            important,
        },
        Declaration {
            name: name("color"),
            value: colour.unwrap_or_else(|| ident("currentcolor")),
            important,
        },
    ])
}

/// `background: red`, `background: linear-gradient(…)`, or both.
///
/// The shorthand is a layered grammar; only the two layers this engine
/// can paint are extracted. A value with a position, repeat or
/// `url()` in it still yields whatever colour and gradient it contains,
/// because dropping the whole declaration over an unsupported keyword
/// is how a dark panel disappears and its white text becomes
/// unreadable.
fn expand_background(value: &[ComponentValue], important: bool) -> Option<Vec<Declaration>> {
    let mut expanded = Vec::new();

    let layers = layer_parts(value);
    let images: Vec<Vec<ComponentValue>> = layers
        .iter()
        .filter_map(|layer| image_part(layer))
        .collect();
    if !images.is_empty() {
        expanded.push(Declaration {
            name: "background-image".to_owned(),
            value: comma_joined(&images),
            important,
        });
        // Each layer's own box keyword, in the same order. Kept even
        // when every layer agrees, because the default is border-box
        // and a layer that said `padding-box` must not silently get
        // it — that difference is the whole gradient-border idiom.
        let clips: Vec<Vec<ComponentValue>> = layers
            .iter()
            .filter(|layer| image_part(layer).is_some())
            .map(|layer| clip_part(layer))
            .collect();
        if clips.iter().any(|clip| !clip.is_empty()) {
            let named = clips
                .into_iter()
                .map(|clip| {
                    if clip.is_empty() {
                        vec![ComponentValue::Token(CssToken::Ident(
                            "border-box".to_owned(),
                        ))]
                    } else {
                        clip
                    }
                })
                .collect::<Vec<_>>();
            expanded.push(Declaration {
                name: "background-clip".to_owned(),
                value: comma_joined(&named),
                important,
            });
        }
    }
    // A bare colour, or one sitting beside other layers.
    if let Some(colour) = colour_part(value) {
        expanded.push(Declaration {
            name: "background-color".to_owned(),
            value: colour,
            important,
        });
    }

    (!expanded.is_empty()).then_some(expanded)
}

/// Split a `background` value on its top-level commas: one per layer.
fn layer_parts(value: &[ComponentValue]) -> Vec<Vec<ComponentValue>> {
    let mut layers = vec![Vec::new()];
    for part in value {
        if matches!(part, ComponentValue::Token(CssToken::Comma)) {
            layers.push(Vec::new());
        } else {
            layers
                .last_mut()
                .expect("one layer is always open")
                .push(part.clone());
        }
    }
    layers
}

/// Put comma-separated parts back together as one value.
fn comma_joined(parts: &[Vec<ComponentValue>]) -> Vec<ComponentValue> {
    let mut out = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            out.push(ComponentValue::Token(CssToken::Comma));
        }
        out.extend(part.iter().cloned());
    }
    out
}

/// The image a single layer names — a gradient or a `url()`.
fn image_part(layer: &[ComponentValue]) -> Option<Vec<ComponentValue>> {
    layer
        .iter()
        .find(|part| match part {
            ComponentValue::Function { name, .. } => {
                let name = name.to_ascii_lowercase();
                name.ends_with("gradient") || name == "url"
            }
            ComponentValue::Token(CssToken::Url(_)) => true,
            _ => false,
        })
        .map(|part| vec![part.clone()])
}

/// A layer's box keyword, which sets its `background-clip`.
///
/// A layer may carry two — `background: … content-box padding-box`
/// means origin then clip — so the *last* one is the clip, and a
/// single keyword sets both.
fn clip_part(layer: &[ComponentValue]) -> Vec<ComponentValue> {
    layer
        .iter()
        .rev()
        .find(|part| BackgroundBox::parse_one(std::slice::from_ref(part)).is_some())
        .map(|part| vec![part.clone()])
        .unwrap_or_default()
}

/// The colour within a `background` value, if there is one.
fn colour_part(value: &[ComponentValue]) -> Option<Vec<ComponentValue>> {
    // The whole value, when it is nothing but a colour.
    if color::parse(value).is_some() {
        return Some(value.to_vec());
    }
    // Otherwise one part of a layered value.
    split_parts(value)
        .into_iter()
        .find(|part| matches!(color::parse(part), Some(SpecifiedColor::Absolute(_))))
}

fn is_border_style(part: &[ComponentValue]) -> bool {
    matches_keyword(part, &BORDER_STYLES)
}

fn is_border_width(part: &[ComponentValue]) -> bool {
    matches_keyword(part, &BORDER_WIDTHS) || super::value::parse_length(part).is_some()
}

fn is_colour(part: &[ComponentValue]) -> bool {
    color::parse(part).is_some()
}

fn matches_keyword(part: &[ComponentValue], keywords: &[&str]) -> bool {
    match part {
        [ComponentValue::Token(CssToken::Ident(name))] => keywords
            .iter()
            .any(|keyword| name.eq_ignore_ascii_case(keyword)),
        _ => false,
    }
}

/// A one-ident value, for the defaults a shorthand fills in.
fn ident(name: &str) -> Vec<ComponentValue> {
    vec![ComponentValue::Token(CssToken::Ident(name.to_owned()))]
}

/// Split a value on top-level whitespace.
///
/// Whitespace inside a function is already nested inside its arguments,
/// so `rgb(1, 2, 3)` stays one part — which is what lets
/// `border: 1px solid rgb(1,2,3)` split into three.
fn split_parts(values: &[ComponentValue]) -> Vec<Vec<ComponentValue>> {
    let mut parts = Vec::new();
    let mut current: Vec<ComponentValue> = Vec::new();
    for value in values {
        if matches!(value, ComponentValue::Token(CssToken::Whitespace)) {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
        } else {
            current.push(value.clone());
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::{parse_declarations, serialize};

    /// Expand `css` and return `(property, value, important)` triples.
    fn expanded(css: &str) -> Vec<(String, String, bool)> {
        expand_all(&parse_declarations(css))
            .into_iter()
            .map(|declaration| {
                (
                    declaration.name,
                    serialize(&declaration.value),
                    declaration.important,
                )
            })
            .collect()
    }

    /// Just the names and values, for the common case.
    fn pairs(css: &str) -> Vec<(String, String)> {
        expanded(css)
            .into_iter()
            .map(|(name, value, _)| (name, value))
            .collect()
    }

    #[test]
    fn a_longhand_expands_to_itself() {
        assert_eq!(pairs("color: red"), [("color".into(), "red".into())]);
    }

    #[test]
    fn one_margin_value_covers_all_four_sides() {
        assert_eq!(
            pairs("margin: 0"),
            [
                ("margin-top".into(), "0".into()),
                ("margin-right".into(), "0".into()),
                ("margin-bottom".into(), "0".into()),
                ("margin-left".into(), "0".into()),
            ]
        );
    }

    #[test]
    fn two_values_split_block_and_inline() {
        assert_eq!(
            pairs("margin: 0 auto"),
            [
                ("margin-top".into(), "0".into()),
                ("margin-right".into(), "auto".into()),
                ("margin-bottom".into(), "0".into()),
                ("margin-left".into(), "auto".into()),
            ]
        );
    }

    #[test]
    fn three_values_repeat_the_middle_one_for_left() {
        // The case people misremember: `left` takes the *second* value.
        assert_eq!(
            pairs("padding: 1px 2px 3px"),
            [
                ("padding-top".into(), "1px".into()),
                ("padding-right".into(), "2px".into()),
                ("padding-bottom".into(), "3px".into()),
                ("padding-left".into(), "2px".into()),
            ]
        );
    }

    #[test]
    fn four_values_go_clockwise_from_the_top() {
        assert_eq!(
            pairs("padding: 1px 2px 3px 4px"),
            [
                ("padding-top".into(), "1px".into()),
                ("padding-right".into(), "2px".into()),
                ("padding-bottom".into(), "3px".into()),
                ("padding-left".into(), "4px".into()),
            ]
        );
    }

    #[test]
    fn more_than_four_values_is_not_expanded() {
        // Invalid, so it stays whole and is ignored downstream rather
        // than being guessed at.
        assert_eq!(
            pairs("margin: 1px 2px 3px 4px 5px"),
            [("margin".into(), "1px 2px 3px 4px 5px".into())]
        );
    }

    #[test]
    fn a_longhand_after_a_shorthand_keeps_its_own_value() {
        // The reason this module runs before the cascade at all.
        let got = pairs("margin: 0; margin-left: 5px");
        assert_eq!(got.len(), 5);
        assert_eq!(got[0], ("margin-top".into(), "0".into()));
        assert_eq!(got[4], ("margin-left".into(), "5px".into()));
    }

    #[test]
    fn importance_is_carried_onto_every_longhand() {
        let got = expanded("margin: 0 !important");
        assert_eq!(got.len(), 4);
        assert!(got.iter().all(|(_, _, important)| *important));
    }

    #[test]
    fn the_border_box_shorthands_expand_per_side() {
        assert_eq!(
            pairs("border-width: 1px 2px"),
            [
                ("border-top-width".into(), "1px".into()),
                ("border-right-width".into(), "2px".into()),
                ("border-bottom-width".into(), "1px".into()),
                ("border-left-width".into(), "2px".into()),
            ]
        );
        assert_eq!(
            pairs("border-style: solid").first(),
            Some(&("border-top-style".into(), "solid".into()))
        );
        assert_eq!(
            pairs("border-color: red").first(),
            Some(&("border-top-color".into(), "red".into()))
        );
    }

    #[test]
    fn border_expands_through_two_levels_to_twelve_longhands() {
        let got = pairs("border: 1px solid red");
        assert_eq!(got.len(), 12, "three properties times four sides");
        assert!(got.contains(&("border-top-width".into(), "1px".into())));
        assert!(got.contains(&("border-left-style".into(), "solid".into())));
        assert!(got.contains(&("border-bottom-color".into(), "red".into())));
    }

    #[test]
    fn border_parts_may_come_in_any_order() {
        let ordered = pairs("border: 1px solid red");
        let shuffled = pairs("border: red 1px solid");
        assert_eq!(ordered, shuffled);
    }

    #[test]
    fn border_fills_in_what_was_omitted() {
        // `border: red` sets style to none, so it paints nothing — the
        // shorthand resets what it does not mention.
        let got = pairs("border: red");
        assert!(got.contains(&("border-top-style".into(), "none".into())));
        assert!(got.contains(&("border-top-width".into(), "medium".into())));
        assert!(got.contains(&("border-top-color".into(), "red".into())));
    }

    #[test]
    fn border_accepts_a_width_keyword_and_a_colour_function() {
        let got = pairs("border: thick solid rgb(1, 2, 3)");
        assert!(got.contains(&("border-top-width".into(), "thick".into())));
        assert!(got.contains(&("border-top-color".into(), "rgb(1, 2, 3)".into())));
    }

    #[test]
    fn an_unclassifiable_border_part_leaves_the_shorthand_alone() {
        assert_eq!(
            pairs("border: 1px wobbly red"),
            [("border".into(), "1px wobbly red".into())]
        );
    }

    #[test]
    fn overflow_and_gap_expand_to_their_pairs() {
        assert_eq!(
            pairs("overflow: hidden"),
            [
                ("overflow-x".into(), "hidden".into()),
                ("overflow-y".into(), "hidden".into()),
            ]
        );
        assert_eq!(
            pairs("overflow: hidden scroll"),
            [
                ("overflow-x".into(), "hidden".into()),
                ("overflow-y".into(), "scroll".into()),
            ]
        );
        assert_eq!(
            pairs("gap: 1px 2px"),
            [
                ("row-gap".into(), "1px".into()),
                ("column-gap".into(), "2px".into()),
            ]
        );
    }

    #[test]
    fn a_plain_background_colour_expands() {
        assert_eq!(
            pairs("background: red"),
            [("background-color".into(), "red".into())]
        );
        assert_eq!(
            pairs("background: #fff"),
            [("background-color".into(), "#fff".into())]
        );
    }

    #[test]
    fn a_background_yields_its_image_and_drops_the_keywords_it_cannot_use() {
        // `no-repeat` is not painted yet, but dropping the whole
        // declaration over it is how a dark panel disappears and its
        // white text becomes unreadable.
        assert_eq!(
            pairs("background: url(x.png) no-repeat"),
            [("background-image".into(), "url(x.png)".into())]
        );
    }

    #[test]
    fn each_background_layer_keeps_its_own_box_keyword() {
        // The gradient-border idiom: two layers, two boxes. Keeping
        // only the first image paints the rim across the whole
        // control, and keeping only the last loses the rim.
        assert_eq!(
            pairs(
                "background: linear-gradient(#111, #111) padding-box, \
                 linear-gradient(90deg, #ccff00, #333) border-box"
            ),
            [
                (
                    "background-image".into(),
                    "linear-gradient(#111, #111),linear-gradient(90deg, #ccff00, #333)".into()
                ),
                ("background-clip".into(), "padding-box,border-box".into()),
            ]
        );
    }

    #[test]
    fn a_background_with_no_box_keyword_states_no_clip() {
        // The initial value is border-box, so saying nothing and
        // saying `border-box` must come out the same.
        let expanded = pairs("background: linear-gradient(#111, #222)");
        assert!(
            !expanded.iter().any(|(name, _)| name == "background-clip"),
            "{expanded:?}"
        );
    }

    #[test]
    fn a_value_containing_a_function_splits_on_the_right_spaces() {
        // The comma-space inside `rgb(...)` must not split the value.
        let got = pairs("border: 2px dashed rgb(10, 20, 30)");
        assert!(got.contains(&("border-top-color".into(), "rgb(10, 20, 30)".into())));
        assert_eq!(got.len(), 12);
    }

    #[test]
    fn an_empty_declaration_list_expands_to_nothing() {
        assert_eq!(expand_all(&[]), []);
        assert_eq!(pairs(""), []);
    }
    #[test]
    fn a_single_sided_border_expands_to_that_side_only() {
        // A card with one dividing line writes `border-bottom`.
        // Leaving it unexpanded drops the line entirely.
        let out = pairs("border-bottom: 1px solid red");
        assert!(
            out.contains(&("border-bottom-width".into(), "1px".into())),
            "{out:?}"
        );
        assert!(
            out.contains(&("border-bottom-style".into(), "solid".into())),
            "{out:?}"
        );
        assert!(
            out.contains(&("border-bottom-color".into(), "red".into())),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|(name, _)| name.starts_with("border-top")),
            "{out:?}"
        );
    }

    #[test]
    fn a_single_sided_border_fills_in_what_it_omits() {
        let out = pairs("border-left: red");
        assert!(
            out.contains(&("border-left-width".into(), "medium".into())),
            "{out:?}"
        );
        assert!(
            out.contains(&("border-left-style".into(), "none".into())),
            "{out:?}"
        );
    }
}
