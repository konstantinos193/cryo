//! Absolute positioning: boxes taken out of flow and placed by inset.
//!
//! An absolutely positioned box is laid out against a *containing
//! block* rather than its parent's flow — the padding box of the
//! nearest ancestor with a `position` other than `static`, or the
//! viewport when there is none. Its siblings lay out as though it were
//! not there, which is the whole point: an overlay, a dropdown or a
//! full-bleed background costs the flow nothing.
//!
//! The arithmetic is one equation per axis, and it is the same shape
//! both times:
//!
//! ```text
//!   left + margin-left + border-left + padding-left + width
//!        + padding-right + border-right + margin-right + right
//!     = the containing block's width
//! ```
//!
//! Three of those terms may be `auto` — the two insets and the size —
//! and which are decides what is solved for:
//!
//! | `left` | `width` | `right` | what happens                      |
//! |--------|---------|---------|-----------------------------------|
//! | given  | auto    | given   | the width is the gap between them |
//! | given  | given   | given   | over-constrained: `right` is ignored |
//! | given  | given   | auto    | `right` takes the remainder       |
//! | auto   | given   | given   | `left` takes the remainder        |
//! | auto   | auto    | given   | shrink-to-fit, then `left`        |
//! | auto   | auto    | auto    | the static position, shrink-to-fit |
//!
//! The first row is the one that matters most in practice: `inset: 0`
//! is how every overlay, backdrop and full-bleed hero is written, and
//! without it such a box has no size at all.
//!
//! Known gaps, stated rather than hidden:
//! - The *static position* — where the box would have been in flow —
//!   is approximated as the containing block's start corner, except
//!   under a flex container, where the spec gives an exact answer and
//!   `static_position` follows it. Getting it right in normal flow
//!   means remembering a position for a box that was never laid out.
//! - `position: fixed` is treated as `absolute` against the viewport.
//!   With no scrolling yet the two are the same thing.
//! - Auto margins do not centre an absolutely positioned box; they
//!   compute to zero.
//! - There is no `z-index`. Positioned boxes paint after in-flow
//!   content, in tree order.

use crate::engine::cryo_impl::css::computed::{
    AlignItems, ComputedStyle, Inset, JustifyContent, Margin, Sides,
};

use super::block::{self, ContainingBlock};
use super::box_tree::{BoxId, BoxTree};
use super::geometry::{Fragment, Point, Rect};
use super::measure::TextMeasure;

/// Give `fragment` the out-of-flow boxes its own box is the containing
/// block for. A no-op for a box that is not positioned.
///
/// Every path that finishes a box's fragment calls this — block
/// layout, flex item layout and the document root — because any box
/// may be the one a `position: relative` was written on. The test and
/// the rectangle live here rather than at those call sites, so there
/// is one place that answers "what is a containing block, and what
/// does an inset resolve against".
pub(super) fn attach(tree: &BoxTree, fragment: &mut Fragment, metrics: &dyn TextMeasure) {
    if !tree
        .get(fragment.box_id)
        .style
        .position
        .is_containing_block()
    {
        return;
    }
    // The spec says the *padding* box, not the content box: an inset
    // of zero reaches the inside of the border, not the inside of the
    // padding.
    let containing = fragment.padding_box();
    let id = fragment.box_id;
    fragment
        .children
        .extend(layout_children(tree, id, containing, metrics));
}

/// Lay out every out-of-flow box whose containing block is `root`.
///
/// Returns their fragments, to be appended after the in-flow ones so
/// they paint on top.
pub(super) fn layout_children(
    tree: &BoxTree,
    root: BoxId,
    containing: Rect,
    metrics: &dyn TextMeasure,
) -> Vec<Fragment> {
    descendants(tree, root)
        .into_iter()
        .map(|out| layout_one(tree, out, containing, metrics))
        .collect()
}

/// An out-of-flow box and the box it was written inside.
///
/// The parent is kept because the static position is the parent's
/// business, not the containing block's — they are the same box in the
/// common case, and when they are not, only the parent knows how a box
/// with no inset would have been aligned.
#[derive(Clone, Copy)]
struct OutOfFlow {
    id: BoxId,
    parent: BoxId,
}

/// The out-of-flow boxes `root` is the containing block for.
///
/// Descent stops at any box that is itself a containing block: the
/// out-of-flow boxes below *that* one are its responsibility, not
/// ours, and collecting them here would resolve their insets against
/// the wrong rectangle.
fn descendants(tree: &BoxTree, root: BoxId) -> Vec<OutOfFlow> {
    let mut found = Vec::new();
    collect(tree, root, &mut found);
    found
}

fn collect(tree: &BoxTree, parent: BoxId, found: &mut Vec<OutOfFlow>) {
    for &child in tree.children(parent) {
        let position = tree.get(child).style.position;
        if position.is_out_of_flow() {
            found.push(OutOfFlow { id: child, parent });
            // Its own out-of-flow descendants resolve against it.
            continue;
        }
        if position.is_containing_block() {
            continue;
        }
        collect(tree, child, found);
    }
}

/// Lay one out-of-flow box out against its containing block.
fn layout_one(
    tree: &BoxTree,
    out: OutOfFlow,
    containing: Rect,
    metrics: &dyn TextMeasure,
) -> Fragment {
    let id = out.id;
    let (static_x, static_y) = static_position(tree, out.parent, id);
    let style = &tree.get(id).style;
    let padding = style
        .padding
        .map(|value| block::resolve(value, containing.width).max(0.0));
    let border = style.border_width;
    let margin = used_margins(style, containing.width);

    let horizontal = solve(
        Axis {
            start: style.inset.left,
            end: style.inset.right,
            size: block::definite_width(style.width, containing.width),
            frame: padding.horizontal() + border.horizontal() + margin.horizontal(),
            basis: containing.width,
            statically: static_x,
        },
        || block::shrink_to_fit(tree, id, containing.width, metrics),
    );

    // The block axis is usually solvable before laying anything out.
    // A declared height is definite, and so is a box pinned by both
    // `top` and `bottom` — the gap between them is a number whatever
    // the content turns out to be. Only a box with neither has to
    // wait for its own layout, and such a box is never stretched, so
    // one pass is always enough.
    let frame_y = padding.vertical() + border.vertical() + margin.vertical();
    let stretched = match (
        inset(style.inset.top, containing.height),
        inset(style.inset.bottom, containing.height),
    ) {
        (Some(top), Some(bottom)) => Some((containing.height - top - bottom - frame_y).max(0.0)),
        _ => None,
    };
    let height = block::definite_height(
        style.height,
        &ContainingBlock {
            width: containing.width,
            height: Some(containing.height),
        },
    )
    .or(stretched);

    // `solve` answered with a *content* width, but a containing block
    // is an outer measurement: block layout subtracts the frame from
    // it again, so it has to go back on first.
    let inner = ContainingBlock {
        width: horizontal.size + padding.horizontal() + border.horizontal() + margin.horizontal(),
        height: Some(containing.height),
    };
    let origin = Point {
        x: containing.x + horizontal.start,
        y: containing.y,
    };
    let mut fragment = block::layout_box_with_height(tree, id, &inner, origin, height, metrics);

    // With the height settled, the same equation places the box.
    let used_height = fragment.border_box().height - border.vertical() - padding.vertical();
    let vertical = solve(
        Axis {
            start: style.inset.top,
            end: style.inset.bottom,
            size: Some(used_height),
            frame: frame_y,
            basis: containing.height,
            statically: static_y,
        },
        || used_height,
    );
    fragment.translate(0.0, containing.y + vertical.start - fragment.margin_box().y);
    fragment
}

/// One axis of the positioning equation, in that axis's own terms.
struct Axis {
    start: Inset,
    end: Inset,
    /// The declared size, if it is not `auto`.
    size: Option<f32>,
    /// Margins, borders and padding along this axis.
    frame: f32,
    /// The containing block's extent along this axis.
    basis: f32,
    /// Where the box sits when neither inset is given.
    statically: Static,
}

/// Where a box with no inset on an axis is placed within its
/// containing block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Static {
    Start,
    Center,
    End,
}

impl Static {
    /// The offset from the containing block's start edge for a box
    /// whose margin box is `outer` long.
    fn offset(self, basis: f32, outer: f32) -> f32 {
        match self {
            Self::Start => 0.0,
            Self::Center => (basis - outer) / 2.0,
            Self::End => basis - outer,
        }
    }
}

/// Where an out-of-flow box would have been, per axis, as
/// (horizontal, vertical).
///
/// Flexbox §4.1 is exact about one case: an absolutely positioned
/// child of a flex container is aligned inside the container's box as
/// though it were the only flex item. That is what centres an icon
/// written as nothing but `position: absolute; left: 14px` inside a
/// `display: flex; align-items: center` field — without it the icon
/// sits on the field's top edge and the field looks two lines tall.
///
/// Everywhere else the start corner is the best this module can do;
/// see the module note.
fn static_position(tree: &BoxTree, parent: BoxId, child: BoxId) -> (Static, Static) {
    let container = &tree.get(parent).style;
    if !container.display.is_flex_container() {
        return (Static::Start, Static::Start);
    }
    let flip = |align: Static| match (container.flex_direction.is_reversed(), align) {
        (true, Static::Start) => Static::End,
        (true, Static::End) => Static::Start,
        (_, align) => align,
    };
    // A lone item leaves all the free space to be distributed, so
    // space-around and space-evenly both centre it and space-between
    // leaves it at the start.
    let main = flip(match container.justify_content {
        JustifyContent::Center | JustifyContent::SpaceAround | JustifyContent::SpaceEvenly => {
            Static::Center
        }
        JustifyContent::FlexEnd => Static::End,
        JustifyContent::FlexStart | JustifyContent::SpaceBetween => Static::Start,
    });
    // Stretch never stretches an out-of-flow box, so for one it means
    // the start edge.
    let cross = match tree
        .get(child)
        .style
        .align_self
        .unwrap_or(container.align_items)
    {
        AlignItems::Center => Static::Center,
        AlignItems::FlexEnd => Static::End,
        AlignItems::Stretch | AlignItems::FlexStart | AlignItems::Baseline => Static::Start,
    };
    if container.flex_direction.is_row() {
        (main, cross)
    } else {
        (cross, main)
    }
}

/// The solved offset and size for one axis.
struct Solved {
    /// Where the box's *margin* box starts, from the containing
    /// block's start edge. Block layout adds the margin, border and
    /// padding itself, so this must not include them.
    start: f32,
    /// The content size along this axis.
    size: f32,
}

/// Solve one axis. `intrinsic` is only called when it is needed, since
/// measuring a box costs a layout pass.
fn solve(axis: Axis, intrinsic: impl FnOnce() -> f32) -> Solved {
    let start = inset(axis.start, axis.basis);
    let end = inset(axis.end, axis.basis);

    let (offset, size) = match (start, axis.size, end) {
        // The `inset: 0` case: the size is whatever the gap leaves.
        (Some(start), None, Some(end)) => (start, (axis.basis - start - end - axis.frame).max(0.0)),
        // A declared size wins; a stated `end` is then redundant and
        // ignored, which is what over-constrained means here.
        (Some(start), Some(size), _) => (start, size),
        // Only the end is given, so the start is the remainder.
        (None, Some(size), Some(end)) => (axis.basis - end - size - axis.frame, size),
        (None, None, Some(end)) => {
            let size = intrinsic().min((axis.basis - end - axis.frame).max(0.0));
            (axis.basis - end - size - axis.frame, size)
        }
        // Nothing to go on but the content, so the box falls back to
        // where it would have been — see `static_position`.
        (Some(start), None, None) => (start, intrinsic()),
        (None, Some(size), None) => (axis.statically.offset(axis.basis, size + axis.frame), size),
        (None, None, None) => {
            let size = intrinsic();
            (axis.statically.offset(axis.basis, size + axis.frame), size)
        }
    };
    Solved {
        start: offset,
        size,
    }
}

fn inset(value: Inset, basis: f32) -> Option<f32> {
    match value {
        Inset::Auto => None,
        Inset::Length(length) => Some(block::resolve(length, basis)),
    }
}

/// An auto margin on an absolutely positioned box computes to zero
/// here; see the module note on centring.
fn used_margins(style: &ComputedStyle, basis: f32) -> Sides<f32> {
    style.margin.map(|margin| match margin {
        Margin::Auto => 0.0,
        Margin::Length(value) => block::resolve(value, basis),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::cascade::{Origin, RuleSet};
    use crate::engine::cryo_impl::css::computed::ComputeContext;
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::css::style_tree::StyleTree;
    use crate::engine::cryo_impl::dom::Document;
    use crate::engine::cryo_impl::html::parse as parse_html;
    use crate::engine::cryo_impl::layout::block::layout_document;
    use crate::engine::cryo_impl::layout::measure::RatioMetrics;

    const UA: &str = "html, body, div, p, span { display: block } head { display: none }";

    /// Lay a page out in 600x400 and return border boxes by element id.
    fn boxes(markup: &str, css: &str) -> Vec<(String, Rect)> {
        let doc = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, UA);
        rules.insert_css(Origin::Author, css);
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = super::super::box_tree::BoxTree::build(&doc, &styles);
        let root = layout_document(&tree, 600.0, 400.0, &RatioMetrics::DEFAULT).expect("a root");
        let mut out = Vec::new();
        walk(&doc, &tree, &root, &mut out);
        out
    }

    fn walk(doc: &Document, tree: &BoxTree, fragment: &Fragment, out: &mut Vec<(String, Rect)>) {
        if let Some(node) = tree.get(fragment.box_id).node {
            if let Some(id) = doc.element(node).and_then(|element| element.id()) {
                out.push((id.to_owned(), fragment.border_box()));
            }
        }
        for child in &fragment.children {
            walk(doc, tree, child, out);
        }
    }

    fn rect_of(boxes: &[(String, Rect)], id: &str) -> Rect {
        boxes
            .iter()
            .find(|(name, _)| name == id)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("no box with id {id}, have {boxes:?}"))
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 0.5
    }

    #[test]
    fn inset_zero_fills_its_containing_block() {
        // The case every overlay and full-bleed background is written
        // as. Without it the box has no size at all.
        let laid = boxes(
            "<div id=outer><div id=inner></div></div>",
            "#outer { position: relative; width: 300px; height: 200px }
             #inner { position: absolute; inset: 0 }",
        );
        let inner = rect_of(&laid, "inner");
        let outer = rect_of(&laid, "outer");
        assert!(close(inner.x, outer.x));
        assert!(close(inner.y, outer.y));
        assert!(close(inner.width, 300.0), "{inner:?}");
        assert!(close(inner.height, 200.0), "{inner:?}");
    }

    #[test]
    fn insets_resolve_against_the_nearest_positioned_ancestor() {
        let laid = boxes(
            "<div id=static><div id=outer><div id=inner></div></div></div>",
            "#static { margin: 10px }
             #outer { position: relative; width: 300px; height: 200px }
             #inner { position: absolute; top: 20px; left: 30px;
                      width: 50px; height: 40px }",
        );
        let inner = rect_of(&laid, "inner");
        let outer = rect_of(&laid, "outer");
        assert!(close(inner.x, outer.x + 30.0));
        assert!(close(inner.y, outer.y + 20.0));
        assert!(close(inner.width, 50.0));
        assert!(close(inner.height, 40.0));
    }

    #[test]
    fn a_static_ancestor_is_not_a_containing_block() {
        // `position: static` is transparent: the inset resolves against
        // whatever is positioned further up, here the viewport.
        let laid = boxes(
            "<div id=outer><div id=inner></div></div>",
            "#outer { margin: 50px; width: 300px; height: 200px }
             #inner { position: absolute; left: 0; top: 0;
                      width: 10px; height: 10px }",
        );
        let inner = rect_of(&laid, "inner");
        assert!(close(inner.x, 0.0), "the viewport's edge, not the div's");
        assert!(close(inner.y, 0.0));
    }

    #[test]
    fn right_and_bottom_measure_from_the_far_edge() {
        let laid = boxes(
            "<div id=outer><div id=inner></div></div>",
            "#outer { position: relative; width: 300px; height: 200px }
             #inner { position: absolute; right: 10px; bottom: 20px;
                      width: 50px; height: 40px }",
        );
        let inner = rect_of(&laid, "inner");
        let outer = rect_of(&laid, "outer");
        assert!(close(inner.x, outer.x + 300.0 - 10.0 - 50.0));
        assert!(close(inner.y, outer.y + 200.0 - 20.0 - 40.0));
    }

    #[test]
    fn both_insets_with_an_auto_size_stretch_between_them() {
        let laid = boxes(
            "<div id=outer><div id=inner></div></div>",
            "#outer { position: relative; width: 300px; height: 200px }
             #inner { position: absolute; left: 20px; right: 30px;
                      top: 10px; bottom: 40px }",
        );
        let inner = rect_of(&laid, "inner");
        assert!(close(inner.width, 250.0), "300 - 20 - 30, got {inner:?}");
        assert!(close(inner.height, 150.0), "200 - 10 - 40, got {inner:?}");
    }

    #[test]
    fn a_declared_size_wins_over_an_opposing_inset() {
        // Over-constrained: `right` is the one that gives way.
        let laid = boxes(
            "<div id=outer><div id=inner></div></div>",
            "#outer { position: relative; width: 300px; height: 200px }
             #inner { position: absolute; left: 10px; right: 10px; width: 100px;
                      height: 20px }",
        );
        let inner = rect_of(&laid, "inner");
        let outer = rect_of(&laid, "outer");
        assert!(close(inner.width, 100.0));
        assert!(close(inner.x, outer.x + 10.0));
    }

    #[test]
    fn percentage_insets_are_of_the_containing_block() {
        let laid = boxes(
            "<div id=outer><div id=inner></div></div>",
            "#outer { position: relative; width: 300px; height: 200px }
             #inner { position: absolute; left: 50%; top: 25%;
                      width: 10px; height: 10px }",
        );
        let inner = rect_of(&laid, "inner");
        let outer = rect_of(&laid, "outer");
        assert!(close(inner.x, outer.x + 150.0));
        assert!(close(inner.y, outer.y + 50.0));
    }

    #[test]
    fn padding_on_the_container_moves_the_containing_block() {
        // The containing block is the *padding* box, so the container's
        // border shifts it but its padding does not.
        let laid = boxes(
            "<div id=outer><div id=inner></div></div>",
            "#outer { position: relative; width: 300px; height: 200px;
                      padding: 25px; border: 5px solid }
             #inner { position: absolute; inset: 0 }",
        );
        let inner = rect_of(&laid, "inner");
        let outer = rect_of(&laid, "outer");
        // Inside the border, outside the padding.
        assert!(close(inner.x, outer.x + 5.0));
        assert!(close(inner.width, 300.0 + 50.0), "the padding box");
    }

    #[test]
    fn an_out_of_flow_box_takes_no_room_from_its_siblings() {
        let laid = boxes(
            "<div id=outer><div id=gone></div><div id=after></div></div>",
            "#outer { position: relative }
             #gone { position: absolute; height: 500px; inset: 0 }
             #after { height: 10px }",
        );
        assert!(
            close(rect_of(&laid, "after").y, 0.0),
            "as if gone were not there"
        );
        assert!(close(rect_of(&laid, "outer").height, 10.0));
    }

    #[test]
    fn nested_positioning_uses_the_nearest_ancestor_each_time() {
        let laid = boxes(
            "<div id=a><div id=b><div id=c></div></div></div>",
            "#a { position: relative; width: 400px; height: 300px }
             #b { position: absolute; left: 100px; top: 50px;
                  width: 200px; height: 100px }
             #c { position: absolute; left: 10px; top: 10px;
                  width: 5px; height: 5px }",
        );
        let b = rect_of(&laid, "b");
        let c = rect_of(&laid, "c");
        assert!(close(b.x, 100.0));
        assert!(close(c.x, b.x + 10.0), "c is placed inside b, not a");
        assert!(close(c.y, b.y + 10.0));
    }

    #[test]
    fn a_positioned_flex_item_hosts_its_own_out_of_flow_children() {
        // A flex item's fragment is built by flex layout, not block
        // layout. Hooking only the latter made every overlay inside a
        // flex panel disappear — the panel was laid out, its absolute
        // children were skipped by the flow, and nothing placed them.
        let laid = boxes(
            "<div id=row><div id=panel><div id=overlay></div></div></div>",
            "#row { display: flex; width: 400px; height: 200px }
             #panel { position: relative; width: 150px }
             #overlay { position: absolute; inset: 0 }",
        );
        let overlay = rect_of(&laid, "overlay");
        let panel = rect_of(&laid, "panel");
        assert!(close(overlay.x, panel.x), "{overlay:?} vs {panel:?}");
        assert!(close(overlay.width, 150.0), "the panel's width");
        assert!(close(overlay.height, panel.height), "stretched to it");
    }

    #[test]
    fn an_auto_sized_box_shrinks_to_its_content() {
        let laid = boxes(
            "<div id=outer><div id=inner>abcd</div></div>",
            "#outer { position: relative; width: 300px; height: 200px }
             #inner { position: absolute; left: 0; top: 0 }",
        );
        // 16px font at the 0.5 ratio: 8px a character.
        let inner = rect_of(&laid, "inner");
        assert!(close(inner.width, 32.0), "{inner:?}");
    }

    #[test]
    fn a_flex_container_centres_an_out_of_flow_child_that_gave_no_top() {
        // The field-icon shape: `position: absolute; left: 14px` and
        // nothing else, inside `display: flex; align-items: center`.
        // Placing it at the top edge instead makes a one-line field
        // look two lines tall.
        let laid = boxes(
            "<div id=field><div id=icon></div></div>",
            "#field { display: flex; align-items: center; position: relative;
                      width: 300px; height: 48px }
             #icon { position: absolute; left: 14px; width: 16px; height: 16px }",
        );
        let icon = rect_of(&laid, "icon");
        let field = rect_of(&laid, "field");
        assert!(close(icon.x, field.x + 14.0), "{icon:?}");
        assert!(close(icon.y, field.y + 16.0), "centred in 48px: {icon:?}");
    }

    #[test]
    fn justify_content_places_an_out_of_flow_child_along_the_main_axis() {
        let laid = boxes(
            "<div id=row><div id=badge></div></div>",
            "#row { display: flex; justify-content: center; position: relative;
                    width: 300px; height: 40px }
             #badge { position: absolute; width: 20px; height: 10px }",
        );
        let badge = rect_of(&laid, "badge");
        let row = rect_of(&laid, "row");
        assert!(close(badge.x, row.x + 140.0), "{badge:?}");
    }

    #[test]
    fn a_non_flex_parent_still_places_an_out_of_flow_child_at_the_start() {
        // Normal flow has no alignment to borrow, so nothing changes
        // for the boxes that were already right.
        let laid = boxes(
            "<div id=outer><div id=inner></div></div>",
            "#outer { position: relative; width: 300px; height: 200px }
             #inner { position: absolute; width: 20px; height: 10px }",
        );
        let inner = rect_of(&laid, "inner");
        let outer = rect_of(&laid, "outer");
        assert!(
            close(inner.x, outer.x) && close(inner.y, outer.y),
            "{inner:?}"
        );
    }
}
