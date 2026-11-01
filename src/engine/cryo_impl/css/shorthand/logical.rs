//! Logical properties, rewritten as the physical ones.
//!
//! CSS has two vocabularies for the same four sides. The physical one
//! — `margin-left`, `padding-top` — names directions on the screen.
//! The logical one — `margin-inline-start`, `padding-block-start` —
//! names them relative to the writing mode, so the same stylesheet
//! lays out correctly in Arabic or Japanese without being rewritten.
//!
//! This engine lays out `horizontal-tb`, left to right, and nothing
//! else. So the mapping is fixed:
//!
//! ```text
//!   inline  ──▶ horizontal      block  ──▶ vertical
//!   start   ──▶ left/top        end    ──▶ right/bottom
//! ```
//!
//! Supporting them is not optional in practice: a current toolchain
//! emits `margin-inline: auto` where it used to emit `margin: 0 auto`,
//! so an engine that only reads the physical names fails to centre
//! anything on a modern page — the layout is not subtly off, the
//! container simply sits against the left edge.
//!
//! When the engine learns another writing mode this becomes a
//! decision made per element rather than a table, and this module is
//! where that change goes.

use super::{distribute, ComponentValue, Declaration};

/// The single-sided logical longhands, and what they mean here.
const SIDES: [(&str, &str); 24] = [
    ("margin-inline-start", "margin-left"),
    ("margin-inline-end", "margin-right"),
    ("margin-block-start", "margin-top"),
    ("margin-block-end", "margin-bottom"),
    ("padding-inline-start", "padding-left"),
    ("padding-inline-end", "padding-right"),
    ("padding-block-start", "padding-top"),
    ("padding-block-end", "padding-bottom"),
    ("inset-inline-start", "left"),
    ("inset-inline-end", "right"),
    ("inset-block-start", "top"),
    ("inset-block-end", "bottom"),
    ("border-inline-start-width", "border-left-width"),
    ("border-inline-end-width", "border-right-width"),
    ("border-block-start-width", "border-top-width"),
    ("border-block-end-width", "border-bottom-width"),
    ("border-inline-start-style", "border-left-style"),
    ("border-inline-end-style", "border-right-style"),
    ("border-block-start-style", "border-top-style"),
    ("border-block-end-style", "border-bottom-style"),
    ("border-inline-start-color", "border-left-color"),
    ("border-inline-end-color", "border-right-color"),
    ("border-block-start-color", "border-top-color"),
    ("border-block-end-color", "border-bottom-color"),
];

/// The logical *sizes*, which are plain renames.
const SIZES: [(&str, &str); 6] = [
    ("inline-size", "width"),
    ("block-size", "height"),
    ("min-inline-size", "min-width"),
    ("min-block-size", "min-height"),
    ("max-inline-size", "max-width"),
    ("max-block-size", "max-height"),
];

/// The two-value shorthands: the physical longhands they cover, in
/// the order one or two values are spread over them.
const PAIRS: [(&str, [&str; 2]); 8] = [
    ("margin-inline", ["margin-left", "margin-right"]),
    ("margin-block", ["margin-top", "margin-bottom"]),
    ("padding-inline", ["padding-left", "padding-right"]),
    ("padding-block", ["padding-top", "padding-bottom"]),
    ("inset-inline", ["left", "right"]),
    ("inset-block", ["top", "bottom"]),
    (
        "border-inline-width",
        ["border-left-width", "border-right-width"],
    ),
    (
        "border-block-width",
        ["border-top-width", "border-bottom-width"],
    ),
];

/// Whether this property is one of the logical names.
pub(super) fn is_logical(name: &str) -> bool {
    SIDES.iter().any(|(logical, _)| *logical == name)
        || SIZES.iter().any(|(logical, _)| *logical == name)
        || PAIRS.iter().any(|(logical, _)| *logical == name)
        || name == "border-inline"
        || name == "border-block"
}

/// Rewrite one logical declaration as physical ones.
pub(super) fn expand(
    declaration: &Declaration,
    parts: &[Vec<ComponentValue>],
) -> Option<Vec<Declaration>> {
    let name = declaration.name.as_str();
    let rename = |to: &str| {
        vec![Declaration {
            name: to.to_owned(),
            value: declaration.value.clone(),
            important: declaration.important,
        }]
    };

    if let Some((_, physical)) = SIDES.iter().find(|(logical, _)| *logical == name) {
        return Some(rename(physical));
    }
    if let Some((_, physical)) = SIZES.iter().find(|(logical, _)| *logical == name) {
        return Some(rename(physical));
    }
    if let Some((_, [first, second])) = PAIRS.iter().find(|(logical, _)| *logical == name) {
        // One value covers both ends, two give one each — the same
        // rule the four-sided shorthands use, cut to two.
        let spread = distribute(parts)?;
        return Some(vec![
            Declaration {
                name: (*first).to_owned(),
                value: spread[0].clone(),
                important: declaration.important,
            },
            Declaration {
                name: (*second).to_owned(),
                value: spread[1].clone(),
                important: declaration.important,
            },
        ]);
    }
    // `border-inline: 1px solid red` sets both inline sides at once.
    // It expands to the physical pair, which `border-left` and
    // `border-right` then take apart themselves.
    match name {
        "border-inline" => Some(vec![
            side_border("left", declaration),
            side_border("right", declaration),
        ]),
        "border-block" => Some(vec![
            side_border("top", declaration),
            side_border("bottom", declaration),
        ]),
        _ => None,
    }
}

fn side_border(side: &str, declaration: &Declaration) -> Declaration {
    Declaration {
        name: format!("border-{side}"),
        value: declaration.value.clone(),
        important: declaration.important,
    }
}

#[cfg(test)]
mod tests {
    use super::super::expand_all;
    use crate::engine::cryo_impl::css::parser::{parse, serialize, Rule};

    /// Expand a declaration block and return `name: value` pairs.
    fn expanded(css: &str) -> Vec<String> {
        let sheet = parse(&format!("a {{ {css} }}"));
        let Some(Rule::Style(style)) = sheet.rules.first() else {
            panic!("not a style rule");
        };
        expand_all(&style.declarations)
            .iter()
            .map(|d| format!("{}: {}", d.name, serialize(&d.value)))
            .collect()
    }

    #[test]
    fn margin_inline_auto_centres_the_way_margin_zero_auto_did() {
        // The case this module exists for.
        assert_eq!(
            expanded("margin-inline: auto"),
            ["margin-left: auto", "margin-right: auto"]
        );
    }

    #[test]
    fn two_values_give_one_to_each_end() {
        assert_eq!(
            expanded("padding-inline: 4px 8px"),
            ["padding-left: 4px", "padding-right: 8px"]
        );
    }

    #[test]
    fn the_block_axis_is_the_vertical_one() {
        assert_eq!(
            expanded("margin-block: 10px"),
            ["margin-top: 10px", "margin-bottom: 10px"]
        );
    }

    #[test]
    fn a_single_sided_logical_longhand_is_renamed() {
        assert_eq!(
            expanded("padding-inline-start: 12px"),
            ["padding-left: 12px"]
        );
        assert_eq!(expanded("inset-block-end: 0"), ["bottom: 0"]);
    }

    #[test]
    fn logical_sizes_are_plain_renames() {
        assert_eq!(expanded("inline-size: 50%"), ["width: 50%"]);
        assert_eq!(expanded("max-block-size: 100px"), ["max-height: 100px"]);
    }

    #[test]
    fn a_logical_border_expands_through_the_physical_one() {
        // `border-inline` becomes `border-left`/`border-right`, each of
        // which expands again into width, style and colour.
        let out = expanded("border-inline: 1px solid red");
        assert!(
            out.contains(&"border-left-width: 1px".to_owned()),
            "{out:?}"
        );
        assert!(
            out.contains(&"border-right-color: red".to_owned()),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|d| d.starts_with("border-top")),
            "only the inline sides: {out:?}"
        );
    }

    #[test]
    fn inset_inline_sets_the_horizontal_offsets() {
        assert_eq!(expanded("inset-inline: 0"), ["left: 0", "right: 0"]);
    }

    #[test]
    fn a_physical_property_is_left_alone() {
        assert_eq!(expanded("margin-left: 5px"), ["margin-left: 5px"]);
    }
}
