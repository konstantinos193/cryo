//! The flat item list an inline subtree becomes.
//!
//! An item is a piece of inline content that cannot be broken inside:
//! a word, a run of spaces, a forced break, an atomic box, or the edge
//! of an inline box that paints.
//!
//! Flattening is what makes line breaking simple — `a<span>b</span>c`
//! breaks exactly where `abc` would, so the nesting cannot affect the
//! answer and carrying it through breaking would be structure for
//! nothing. The one thing nesting *does* affect is painting and
//! advance: a `<span>` with a background, border or padding occupies
//! horizontal space of its own and needs a rectangle to paint into.
//! That is what [`Item::Edge`] is for — the tree's only surviving
//! trace in the flat list, and only for the boxes that need it.

use crate::engine::cryo_impl::css::computed::{ComputedStyle, Sides};
use crate::engine::cryo_impl::css::value::LengthPercentage;

use super::super::block::{resolve, ContainingBlock};
use super::super::box_tree::{BoxId, BoxKind, BoxTree};
use super::super::geometry::{Fragment, Point};
use super::super::measure::TextMeasure;

/// Which end of an inline box an edge is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Side {
    Open,
    Close,
}

/// One piece of inline content that cannot be broken internally.
#[derive(Clone, Debug)]
pub(super) enum Item {
    /// Text with no break opportunity inside it.
    Word {
        text: String,
        width: f32,
        box_id: BoxId,
        style: ComputedStyle,
    },
    /// Whitespace, which may vanish at a line break.
    Space {
        text: String,
        width: f32,
        box_id: BoxId,
        style: ComputedStyle,
    },
    /// A `<br>`, or a preserved newline.
    Break { style: ComputedStyle },
    /// An inline-level box laid out as a unit.
    ///
    /// Carries its own finished fragment: an atomic is laid out before
    /// the line is broken, because its size is what decides where the
    /// break falls.
    Atomic {
        width: f32,
        height: f32,
        style: ComputedStyle,
        fragment: Box<Fragment>,
    },
    /// The start or end of an inline box that paints.
    ///
    /// An edge is as wide as that end's border and padding, which an
    /// inline box spends once — at its start and at its end, not on
    /// every line it spans.
    Edge {
        box_id: BoxId,
        side: Side,
        /// This box's padding, resolved. Needed to paint the box, and
        /// the near side of it is this edge's own width.
        padding: Sides<f32>,
        style: ComputedStyle,
    },
}

impl Item {
    pub(super) fn width(&self) -> f32 {
        match self {
            Self::Word { width, .. } | Self::Space { width, .. } | Self::Atomic { width, .. } => {
                *width
            }
            Self::Edge {
                side,
                padding,
                style,
                ..
            } => match side {
                Side::Open => padding.left + style.border_width.left,
                Side::Close => padding.right + style.border_width.right,
            },
            Self::Break { .. } => 0.0,
        }
    }

    pub(super) fn style(&self) -> &ComputedStyle {
        match self {
            Self::Word { style, .. }
            | Self::Space { style, .. }
            | Self::Break { style }
            | Self::Atomic { style, .. }
            | Self::Edge { style, .. } => style,
        }
    }

    /// How far this item rises above the baseline, and how far below.
    pub(super) fn extents(&self, metrics: &dyn TextMeasure) -> (f32, f32) {
        match self {
            // An atomic box sits on the baseline by default, so all of
            // it is above and none below.
            Self::Atomic { height, .. } => (*height, 0.0),
            other => {
                let style = other.style();
                let line = metrics.line_height(style);
                let baseline = metrics.baseline(style);
                (baseline, line - baseline)
            }
        }
    }
}

/// Whether an inline box draws anything of its own.
///
/// An inline box with no background and no border paints nothing, so it
/// needs no fragment and no edges. Most inline boxes on a page are this
/// — every `<a>`, `<em>` and bare `<span>` — and giving each one a
/// rectangle would fill the display list with no-ops.
pub(super) fn paints(style: &ComputedStyle) -> bool {
    !style.background_color.is_transparent()
        || !style.background_image.is_empty()
        || style.border_width.horizontal() > 0.0
        || style.border_width.vertical() > 0.0
}

/// Flatten one inline-level box into items.
pub(super) fn collect(
    tree: &BoxTree,
    id: BoxId,
    available_width: f32,
    metrics: &dyn TextMeasure,
    items: &mut Vec<Item>,
) {
    let layout_box = tree.get(id);
    match &layout_box.kind {
        BoxKind::Text(text) => split_text(text, id, &layout_box.style, metrics, items),
        BoxKind::LineBreak => items.push(Item::Break {
            style: layout_box.style.clone(),
        }),
        // An inline box contributes its children, bracketed by its own
        // edges when it has something to draw.
        BoxKind::Inline => {
            let style = &layout_box.style;
            let decorated = paints(style);
            let padding = padding_of(style, available_width);
            let edge = |side| Item::Edge {
                box_id: id,
                side,
                padding,
                style: style.clone(),
            };
            if decorated {
                items.push(edge(Side::Open));
            }
            for child in tree.in_flow_children(id) {
                collect(tree, child, available_width, metrics, items);
            }
            if decorated {
                items.push(edge(Side::Close));
            }
        }
        // Anything else in an inline context is atomic: it is laid out
        // whole, by its own rules, and then placed on the line like a
        // very large character.
        _ => {
            // An atomic inline is shrink-to-fit, not block-filling:
            // see `block::shrink_to_fit` for why it cannot simply be
            // laid out in the line's full width. Sizing its containing
            // block to what it wants leaves nothing to absorb.
            let fits = super::super::block::shrink_to_fit(tree, id, available_width, metrics);
            let block = ContainingBlock {
                width: fits,
                height: None,
            };
            let fragment = super::super::block::layout_box(
                tree,
                id,
                &block,
                Point { x: 0.0, y: 0.0 },
                metrics,
            );
            let outer = fragment.margin_box();
            items.push(Item::Atomic {
                width: outer.width,
                height: outer.height,
                style: layout_box.style.clone(),
                fragment: Box::new(fragment),
            });
        }
    }
}

/// An inline box's padding, resolved against the line's width.
///
/// Every side, including the vertical ones: an inline box's vertical
/// padding does not affect the line's height but it does paint, which
/// is why a tall `<span>` background spills over the lines above and
/// below it. All four sides of a padding resolve a percentage against
/// the containing block's *width* — the vertical ones too.
fn padding_of(style: &ComputedStyle, available: f32) -> Sides<f32> {
    style
        .padding
        .map(|value: LengthPercentage| resolve(value, available).max(0.0))
}

/// Split a text run into words, spaces and breaks.
///
/// Whitespace handling follows `white-space`: a collapsing style turns
/// any run of spaces into one, and only a preserving style keeps a
/// newline as a break.
fn split_text(
    text: &str,
    box_id: BoxId,
    style: &ComputedStyle,
    metrics: &dyn TextMeasure,
    items: &mut Vec<Item>,
) {
    let collapses = style.white_space.collapses_spaces();
    let keeps_newlines = style.white_space.preserves_newlines();

    // A preserved newline is a break, so it does not count as space.
    let is_space = |c: char| c.is_whitespace() && !(keeps_newlines && c == '\n');

    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        if keeps_newlines && first == '\n' {
            items.push(Item::Break {
                style: style.clone(),
            });
            rest = &rest[first.len_utf8()..];
            continue;
        }

        // The run of like characters starting here: all space or all
        // not, stopping early at a newline that is a break.
        let space = is_space(first);
        let end = rest
            .char_indices()
            .find(|&(_, c)| (keeps_newlines && c == '\n') || is_space(c) != space)
            .map_or(rest.len(), |(index, _)| index);
        let run = &rest[..end];
        rest = &rest[end..];

        if space {
            // A collapsing run is one space however long it was.
            let rendered = if collapses {
                " ".to_owned()
            } else {
                run.to_owned()
            };
            items.push(Item::Space {
                width: metrics.width(&rendered, style),
                text: rendered,
                box_id,
                style: style.clone(),
            });
        } else {
            items.push(Item::Word {
                width: metrics.width(run, style),
                text: run.to_owned(),
                box_id,
                style: style.clone(),
            });
        }
    }
}
