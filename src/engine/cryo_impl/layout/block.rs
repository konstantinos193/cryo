//! Block layout: block-level boxes get a width, a position and a height.
//!
//! The whole of horizontal block layout is one equation (CSS 2.1
//! §10.3.3), and everything else follows from which of its terms are
//! `auto`:
//!
//! ```text
//!   margin-left + border-left + padding-left
//!     + width
//!     + padding-right + border-right + margin-right
//!   = containing block width
//! ```
//!
//! It must hold exactly, so something has to give:
//!
//! - **`width: auto`** absorbs everything left over. This is why a plain
//!   `<div>` fills its parent, and why giving it a margin makes it
//!   narrower rather than pushing it out.
//! - **One `auto` margin** takes the slack — the box sits against the
//!   other edge.
//! - **Both `auto` margins** split it, which is how `margin: 0 auto`
//!   centres.
//! - **Nothing `auto`** means the author over-constrained it, and the
//!   right margin is overridden to make the equation balance. The
//!   declared value is simply ignored, which surprises people, but every
//!   browser does it.
//!
//! Vertically there is no such equation: `height: auto` is the content's
//! height, and boxes stack. The subtlety there is **margin collapsing** —
//! two stacked paragraphs with `1em` margins are `1em` apart, not `2em`.
//!
//! Known gap: only *sibling* margins collapse here. A child's top margin
//! should also collapse through a parent that has no top border or
//! padding, and it does not yet — so a bare `<div>` wrapping a `<p>`
//! gets slightly more space above it than a browser would give. Doing it
//! needs layout split into a measure pass and a place pass, because the
//! parent's position depends on a margin that comes from inside it.

use crate::engine::cryo_impl::css::computed::{BoxSizing, ComputedStyle, Margin, Sides};
use crate::engine::cryo_impl::css::value::{LengthPercentage, Size};

use super::box_tree::{BoxId, BoxTree};
use super::geometry::{Fragment, Point, Rect};
use super::inline;
use super::measure::TextMeasure;

/// The box a child is laid out inside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContainingBlock {
    /// The content width, which every horizontal percentage resolves
    /// against — including vertical padding and margins, which is one of
    /// CSS's odder rules and the reason `padding-top: 50%` makes a
    /// square.
    pub width: f32,
    /// The content height, when it is known. `None` means the parent's
    /// own height is auto, so a percentage height has nothing to resolve
    /// against and behaves as `auto`.
    pub height: Option<f32>,
}

/// Lay a document out in a viewport.
///
/// Returns `None` for a box tree with no root — an empty document, or
/// one whose root is `display: none`.
pub fn layout_document(
    tree: &BoxTree,
    viewport_width: f32,
    viewport_height: f32,
    metrics: &dyn TextMeasure,
) -> Option<Fragment> {
    let root = tree.root()?;
    let viewport = ContainingBlock {
        width: viewport_width,
        height: Some(viewport_height),
    };
    let mut fragment = layout_box(tree, root, &viewport, Point { x: 0.0, y: 0.0 }, metrics);

    // The *initial containing block*: the viewport, which is what an
    // out-of-flow box resolves against when nothing above it is
    // positioned. The root element is usually `static`, so without
    // this a `position: fixed` header would have no containing block
    // at all.
    if !tree.get(root).style.position.is_containing_block() {
        let initial = Rect::new(0.0, 0.0, viewport_width, viewport_height);
        fragment.children.extend(super::absolute::layout_children(
            tree, root, initial, metrics,
        ));
    }
    Some(fragment)
}

/// Lay out one block-level box.
///
/// `origin` is the top-left of this box's *margin* box: `x` is the
/// containing block's content edge, `y` is where this box starts in the
/// vertical flow.
pub(super) fn layout_box(
    tree: &BoxTree,
    id: BoxId,
    containing: &ContainingBlock,
    origin: Point,
    metrics: &dyn TextMeasure,
) -> Fragment {
    layout_box_with_height(tree, id, containing, origin, None, metrics)
}

/// Lay out a box whose content height something else has already
/// decided.
///
/// Only absolute positioning needs this: a box pinned by both `top`
/// and `bottom` has a height that neither its own `height` property
/// nor its content can answer, and that height is also what its own
/// children take percentages of.
pub(super) fn layout_box_with_height(
    tree: &BoxTree,
    id: BoxId,
    containing: &ContainingBlock,
    origin: Point,
    forced_height: Option<f32>,
    metrics: &dyn TextMeasure,
) -> Fragment {
    let style = &tree.get(id).style;
    let used = resolve_horizontal(style, containing.width);

    let content_x = origin.x + used.margin.left + used.border.left + used.padding.left;
    let content_y = origin.y + used.margin.top + used.border.top + used.padding.top;

    // The frame a declared height has to share with the content, or
    // sit outside of, depending on `box-sizing`.
    let frame_y = used.padding.vertical() + used.border.vertical();
    let inner = ContainingBlock {
        width: used.width,
        height: forced_height
            .or_else(|| {
                definite_height(style.height, containing)
                    .map(|height| (height - frame_inset(style, frame_y)).max(0.0))
            })
            // An `aspect-ratio` answers the axis that was left `auto`.
            // Without it an image with `height: auto` is as tall as
            // its contents, and an image has none.
            .or_else(|| {
                style
                    .aspect_ratio
                    .filter(|ratio| *ratio > 0.0)
                    .map(|ratio| (used.width / ratio).max(0.0))
            }),
    };
    let (content_height, children) = layout_contents(
        tree,
        id,
        &inner,
        Point {
            x: content_x,
            y: content_y,
        },
        metrics,
    );
    // `min-height` and `max-height` apply whether the height came
    // from the style or from the content — a box that grew past its
    // own maximum is as wrong as one that never reached its minimum.
    let height = clamp_height_inside(
        style,
        inner.height.unwrap_or(content_height),
        containing,
        frame_y,
    );

    let mut fragment = Fragment {
        box_id: id,
        content: Rect::new(content_x, content_y, used.width, height),
        padding: used.padding,
        border: used.border,
        margin: used.margin,
        text: None,
        children,
    };

    // Out-of-flow boxes last, so they paint over the in-flow ones.
    super::absolute::attach(tree, &mut fragment, metrics);
    fragment
}

/// The used horizontal values: the equation in the module docs, solved.
struct Used {
    margin: Sides<f32>,
    border: Sides<f32>,
    padding: Sides<f32>,
    width: f32,
}

fn resolve_horizontal(style: &ComputedStyle, available: f32) -> Used {
    let padding = style
        .padding
        .map(|value| resolve(value, available).max(0.0));
    let border = style.border_width;
    let frame = padding.horizontal() + border.horizontal();

    // `box-sizing: border-box` means the declared width already covers
    // the padding and border, so the content gets what is left of it.
    let declared = definite_width(style.width, available).map(|width| match style.box_sizing {
        BoxSizing::BorderBox => (width - frame).max(0.0),
        BoxSizing::ContentBox => width,
    });

    let (width, margin_x) = solve_width(style, available, frame, declared);
    // Clamping changes the width, so the margins have to be solved
    // again against the new one — otherwise a max-width box keeps the
    // margins it would have had while wider, and stops centring.
    let clamped = clamp_width_inside(style, width, available, frame);
    let (width, margin_x) = if (clamped - width).abs() > f32::EPSILON {
        solve_width(style, available, frame, Some(clamped))
    } else {
        (width, margin_x)
    };

    Used {
        margin: Sides {
            top: resolve_margin(style.margin.top, available),
            right: margin_x.1,
            bottom: resolve_margin(style.margin.bottom, available),
            left: margin_x.0,
        },
        border,
        padding,
        width,
    }
}

/// Solve the width equation, returning the width and both margins.
fn solve_width(
    style: &ComputedStyle,
    available: f32,
    frame: f32,
    declared: Option<f32>,
) -> (f32, (f32, f32)) {
    let left_auto = style.margin.left == Margin::Auto;
    let right_auto = style.margin.right == Margin::Auto;
    let left = resolve_margin(style.margin.left, available);
    let right = resolve_margin(style.margin.right, available);

    let Some(width) = declared else {
        // `auto` width takes everything the margins leave.
        let left = if left_auto { 0.0 } else { left };
        let right = if right_auto { 0.0 } else { right };
        return ((available - frame - left - right).max(0.0), (left, right));
    };

    let slack = available - frame - width;
    let margins = match (left_auto, right_auto) {
        // Centring. A negative slack goes entirely to the right, so the
        // box stays against the left edge rather than hanging off both.
        (true, true) => {
            let half = (slack / 2.0).max(0.0);
            (half, slack - half)
        }
        (true, false) => (slack - right, right),
        (false, true) => (left, slack - left),
        // Over-constrained: the right margin is whatever balances it.
        (false, false) => (left, slack - left),
    };
    (width, margins)
}

/// Apply `min-width` and `max-width`, in that order of precedence —
/// a minimum larger than the maximum wins, per spec.
///
/// They measure the same box that `width` does,
/// so under `box-sizing: border-box` they cover the padding and the
/// border too — and the limit on the *content* is that much smaller.
///
/// Missing this is not a rounding error. A card written as
/// `max-width: 420px; padding: 0 24px; box-sizing: border-box` comes
/// out 468 pixels wide instead of 420, and everything inside it
/// inherits the mistake.
pub(super) fn clamp_width_inside(
    style: &ComputedStyle,
    width: f32,
    available: f32,
    frame: f32,
) -> f32 {
    let inset = frame_inset(style, frame);
    let mut clamped = width;
    if let Some(max) = style.max_width {
        clamped = clamped.min((resolve(max, available) - inset).max(0.0));
    }
    clamped.max((resolve(style.min_width, available) - inset).max(0.0))
}

/// Apply `min-height` and `max-height`, the same way round.
///
/// A percentage against a containing block of unknown height is no
/// constraint at all, for the reason [`definite_height`] gives: there
/// is nothing to take a percentage of.
///
/// `box-sizing` matters here for the same reason it does on the inline
/// axis, and the case is everywhere: a hero written as `min-height:
/// 100vh; padding: 48px` under the usual `* { box-sizing: border-box }`
/// reset is exactly one viewport tall. Measuring its *content* against
/// the viewport makes it 96 pixels taller, the page scrolls when it
/// should not, and everything centred in the panel below sits low by
/// half of that.
pub(super) fn clamp_height_inside(
    style: &ComputedStyle,
    height: f32,
    containing: &ContainingBlock,
    frame: f32,
) -> f32 {
    let inset = frame_inset(style, frame);
    let against = |value: LengthPercentage| match value {
        LengthPercentage::Length(length) => Some((length.value - inset).max(0.0)),
        LengthPercentage::Percentage(percent) => containing
            .height
            .map(|basis| (percent / 100.0 * basis - inset).max(0.0)),
    };
    let mut clamped = height;
    if let Some(max) = style.max_height.and_then(against) {
        clamped = clamped.min(max);
    }
    match against(style.min_height) {
        Some(min) => clamped.max(min),
        None => clamped,
    }
}

/// How much of a declared size the frame already accounts for.
///
/// Under `border-box` a declared size covers the padding and border,
/// so the content gets what is left; under `content-box` the declared
/// size *is* the content and the frame sits outside it.
pub(super) fn frame_inset(style: &ComputedStyle, frame: f32) -> f32 {
    match style.box_sizing {
        BoxSizing::BorderBox => frame,
        BoxSizing::ContentBox => 0.0,
    }
}

/// A definite `width`, or `None` for `auto`.
/// A definite size along the inline axis, or `None` for `auto`.
///
/// Shared with flex layout. The pair with [`definite_height`] exists
/// because the axis decides what a percentage is a percentage *of*,
/// and getting that wrong is not a rounding error: a `height: 100%`
/// resolved against the width comes out as wide as the page is broad.
pub(super) fn definite_width(size: Size, available: f32) -> Option<f32> {
    match size {
        Size::Auto => None,
        Size::Definite(value) => Some(resolve(value, available).max(0.0)),
    }
}

/// A definite size along the block axis, or `None` when it must come
/// from the content.
///
/// A percentage height against a containing block of unknown height is
/// `auto`: there is nothing to take a percentage of.
pub(super) fn definite_height(size: Size, containing: &ContainingBlock) -> Option<f32> {
    match size {
        Size::Auto => None,
        Size::Definite(LengthPercentage::Length(length)) => Some(length.value.max(0.0)),
        Size::Definite(LengthPercentage::Percentage(percent)) => containing
            .height
            .map(|basis| (percent / 100.0 * basis).max(0.0)),
    }
}

/// A length or percentage against the width it is a percentage of.
///
/// Shared with inline layout, which resolves an inline box's padding
/// the same way — a second copy would be a second set of rounding and
/// clamping rules to keep in step.
pub(super) fn resolve(value: LengthPercentage, basis: f32) -> f32 {
    match value {
        // Computed lengths are already in pixels.
        LengthPercentage::Length(length) => length.value,
        LengthPercentage::Percentage(percent) => percent / 100.0 * basis,
    }
}

fn resolve_margin(margin: Margin, basis: f32) -> f32 {
    match margin {
        Margin::Auto => 0.0,
        Margin::Length(value) => resolve(value, basis),
    }
}

/// The outer width an atomic inline occupies on a line `available`
/// wide — shrink-to-fit, margins included.
///
/// A declared width is used as declared, including a percentage, which
/// has the line to measure against even though an intrinsic
/// measurement does not. An `auto` width is the content's max-content
/// width, capped at the space on the line.
///
/// This exists because an atomic is never *over-constrained*: it is
/// sized to fit, so there is no leftover space for a margin to absorb.
/// Laying one out in the line's full width instead hands the remainder
/// to its right margin, and the box reports an outer width of the
/// whole line — which pushes everything after it onto the next one.
pub(super) fn shrink_to_fit(
    tree: &BoxTree,
    id: BoxId,
    available: f32,
    metrics: &dyn TextMeasure,
) -> f32 {
    let style = &tree.get(id).style;
    if definite_width(style.width, available).is_none() {
        return super::intrinsic::max_content_width(tree, id, metrics).min(available);
    }
    let used = resolve_horizontal(style, available);
    // The solved right margin may have absorbed slack that an atomic
    // does not have, so the declared margins are what count. An auto
    // margin on an atomic is zero, which `resolve_margin` already says.
    used.width
        + used.padding.horizontal()
        + used.border.horizontal()
        + resolve_margin(style.margin.left, available)
        + resolve_margin(style.margin.right, available)
}

/// Lay out a box's contents, returning their height and fragments.
///
/// A block container holds *either* block-level children or inline
/// content, never both — the box tree guaranteed that by inserting
/// anonymous boxes — so this is a clean either/or.
pub(super) fn layout_contents(
    tree: &BoxTree,
    id: BoxId,
    inner: &ContainingBlock,
    content_origin: Point,
    metrics: &dyn TextMeasure,
) -> (f32, Vec<Fragment>) {
    let children = tree.in_flow_children(id);
    if children.is_empty() {
        return (0.0, Vec::new());
    }

    // A flex container ignores the block/inline distinction entirely:
    // every child becomes a flex item, text included.
    if tree.get(id).style.display.is_flex_container() {
        return super::flex::layout(tree, id, inner, content_origin, metrics);
    }
    // A grid container ignores the block/inline distinction too: its
    // children go into cells, not into a flow.
    if tree.get(id).style.display.is_grid_container() {
        return super::grid::layout(tree, id, inner, content_origin, metrics);
    }

    let all_inline = children
        .iter()
        .all(|&child| tree.get(child).kind.is_inline_level());
    if all_inline {
        let flow = inline::layout(
            tree,
            &children,
            &tree.get(id).style,
            content_origin,
            inner.width,
            metrics,
        );
        return (flow.height(), flow.fragments());
    }
    stack_blocks(tree, &children, inner, content_origin, metrics)
}

/// Stack block-level children, collapsing the margins between them.
fn stack_blocks(
    tree: &BoxTree,
    children: &[BoxId],
    inner: &ContainingBlock,
    content_origin: Point,
    metrics: &dyn TextMeasure,
) -> (f32, Vec<Fragment>) {
    let mut fragments = Vec::new();
    let mut y = content_origin.y;
    // The previous sibling's bottom margin, waiting to collapse with the
    // next sibling's top one.
    let mut pending_margin = 0.0;
    let mut first = true;

    for &child in children {
        let style = &tree.get(child).style;
        let top = resolve_margin(style.margin.top, inner.width);
        // `layout_block` consumes the child's own top margin, so this
        // must pass the *margin box* top, not the border box top: the
        // collapsed gap minus the margin that will be re-added.
        let margin_top = if first {
            // Nothing above the first child to collapse with.
            y
        } else {
            y + collapse(pending_margin, top) - top
        };
        first = false;

        let fragment = layout_box(
            tree,
            child,
            inner,
            Point {
                x: content_origin.x,
                y: margin_top,
            },
            metrics,
        );
        // The box occupies its border box; its own top margin was part
        // of the gap already applied.
        y = fragment.border_box().bottom();
        pending_margin = fragment.margin.bottom;
        fragments.push(fragment);
    }

    // The last child's bottom margin still belongs inside the parent,
    // since parent-child collapsing is not implemented — see the module
    // docs.
    y += pending_margin;
    (y - content_origin.y, fragments)
}

/// Collapse two adjacent margins.
///
/// Positives take the larger, negatives the more negative, and a mixed
/// pair adds — so a `-10px` bottom against a `20px` top leaves `10px`.
fn collapse(first: f32, second: f32) -> f32 {
    if first >= 0.0 && second >= 0.0 {
        first.max(second)
    } else if first < 0.0 && second < 0.0 {
        first.min(second)
    } else {
        first + second
    }
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
    use crate::engine::cryo_impl::layout::measure::RatioMetrics;

    const BLOCKS: &str = concat!(
        "html, body, div, p, section { display: block }",
        "head { display: none }",
    );

    /// Lay `markup` out in a viewport and return the root fragment with
    /// the document it came from.
    fn run(markup: &str, css: &str, width: f32) -> (Document, BoxTree, Fragment) {
        let doc = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, BLOCKS);
        rules.insert_css(Origin::Author, css);
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = BoxTree::build(&doc, &styles);
        let root = layout_document(&tree, width, 600.0, &RatioMetrics::DEFAULT)
            .expect("the document has a root box");
        (doc, tree, root)
    }

    /// The content rect of the first element named `tag`.
    fn rect_of(markup: &str, css: &str, width: f32, tag: &str) -> Rect {
        let (doc, tree, root) = run(markup, css, width);
        let target = doc
            .first_html_element(tag)
            .unwrap_or_else(|| panic!("no <{tag}>"));
        *root
            .flatten()
            .into_iter()
            .find(|fragment| tree.get(fragment.box_id).node == Some(target))
            .map(|fragment| &fragment.content)
            .unwrap_or_else(|| panic!("<{tag}> produced no fragment"))
    }

    /// The border box of the first element named `tag`.
    fn border_box_of(markup: &str, css: &str, width: f32, tag: &str) -> Rect {
        let (doc, tree, root) = run(markup, css, width);
        let target = doc
            .first_html_element(tag)
            .unwrap_or_else(|| panic!("no <{tag}>"));
        root.flatten()
            .into_iter()
            .find(|fragment| tree.get(fragment.box_id).node == Some(target))
            .map(Fragment::border_box)
            .unwrap_or_else(|| panic!("<{tag}> produced no fragment"))
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    #[test]
    fn an_auto_width_block_fills_its_containing_block() {
        let rect = rect_of("<div></div>", "", 800.0, "div");
        assert!(close(rect.width, 800.0));
        assert!(close(rect.x, 0.0));
    }

    #[test]
    fn margins_make_an_auto_width_block_narrower_not_wider() {
        // The equation must hold, so the width gives way.
        let rect = rect_of("<div></div>", "div { margin: 0 50px }", 800.0, "div");
        assert!(close(rect.width, 700.0));
        assert!(close(rect.x, 50.0));
    }

    #[test]
    fn padding_and_borders_also_come_out_of_an_auto_width() {
        let rect = rect_of(
            "<div></div>",
            "div { padding: 0 10px; border: 0 solid; border-left-width: 5px }",
            800.0,
            "div",
        );
        assert!(close(rect.width, 775.0), "800 - 20 padding - 5 border");
        assert!(close(rect.x, 15.0), "border then padding");
    }

    #[test]
    fn a_declared_width_is_used_as_given() {
        let rect = rect_of("<div></div>", "div { width: 300px }", 800.0, "div");
        assert!(close(rect.width, 300.0));
    }

    #[test]
    fn auto_margins_centre_a_fixed_width_block() {
        let rect = rect_of(
            "<div></div>",
            "div { width: 400px; margin: 0 auto }",
            800.0,
            "div",
        );
        assert!(close(rect.x, 200.0));
        assert!(close(rect.width, 400.0));
    }

    #[test]
    fn one_auto_margin_pushes_the_block_to_the_other_edge() {
        let right_aligned = rect_of(
            "<div></div>",
            "div { width: 200px; margin-left: auto; margin-right: 0 }",
            800.0,
            "div",
        );
        assert!(close(right_aligned.x, 600.0));

        let left_aligned = rect_of(
            "<div></div>",
            "div { width: 200px; margin-left: 0; margin-right: auto }",
            800.0,
            "div",
        );
        assert!(close(left_aligned.x, 0.0));
    }

    #[test]
    fn an_over_constrained_box_keeps_its_left_margin_and_width() {
        // The declared right margin is ignored; every browser does this.
        let rect = rect_of(
            "<div></div>",
            "div { width: 100px; margin-left: 50px; margin-right: 999px }",
            800.0,
            "div",
        );
        assert!(close(rect.x, 50.0));
        assert!(close(rect.width, 100.0));
    }

    #[test]
    fn border_box_sizing_puts_the_padding_inside_the_width() {
        let content = rect_of(
            "<div></div>",
            "div { width: 200px; padding: 0 20px; box-sizing: border-box }",
            800.0,
            "div",
        );
        assert!(close(content.width, 160.0), "200 - 40 padding");

        let outer = border_box_of(
            "<div></div>",
            "div { width: 200px; padding: 0 20px; box-sizing: border-box }",
            800.0,
            "div",
        );
        assert!(
            close(outer.width, 200.0),
            "the declared width is the outside"
        );
    }

    #[test]
    fn content_box_sizing_adds_padding_outside_the_width() {
        let outer = border_box_of(
            "<div></div>",
            "div { width: 200px; padding: 0 20px }",
            800.0,
            "div",
        );
        assert!(close(outer.width, 240.0));
    }

    #[test]
    fn a_percentage_width_resolves_against_the_containing_block() {
        let rect = rect_of(
            "<section><div></div></section>",
            "section { width: 400px } div { width: 50% }",
            800.0,
            "div",
        );
        assert!(close(rect.width, 200.0));
    }

    #[test]
    fn max_width_caps_a_block_and_keeps_it_centred() {
        let rect = rect_of(
            "<div></div>",
            "div { max-width: 300px; margin: 0 auto }",
            800.0,
            "div",
        );
        assert!(close(rect.width, 300.0));
        assert!(close(rect.x, 250.0), "re-centred at the clamped width");
    }

    #[test]
    fn min_width_wins_over_max_width() {
        let rect = rect_of(
            "<div></div>",
            "div { width: 100px; min-width: 400px; max-width: 200px }",
            800.0,
            "div",
        );
        assert!(close(rect.width, 400.0));
    }

    #[test]
    fn a_declared_height_is_used_instead_of_the_content_height() {
        let rect = rect_of("<div>text</div>", "div { height: 250px }", 800.0, "div");
        assert!(close(rect.height, 250.0));
    }

    #[test]
    fn an_auto_height_comes_from_the_content() {
        // One line of 16px text at the default line height.
        let rect = rect_of("<div>text</div>", "", 800.0, "div");
        assert!(close(rect.height, 19.2), "got {}", rect.height);
    }

    #[test]
    fn an_empty_block_has_no_height() {
        let rect = rect_of("<div></div>", "", 800.0, "div");
        assert!(close(rect.height, 0.0));
    }

    #[test]
    fn a_parent_grows_to_hold_its_children() {
        let rect = rect_of(
            "<section><div></div><div></div></section>",
            "div { height: 30px }",
            800.0,
            "section",
        );
        assert!(close(rect.height, 60.0));
    }

    #[test]
    fn blocks_stack_vertically() {
        let (doc, tree, root) = run(
            "<div id=a></div><div id=b></div>",
            "div { height: 40px }",
            800.0,
        );
        let ys: Vec<f32> = ["a", "b"]
            .iter()
            .map(|id| {
                let node = doc
                    .descendants(doc.root())
                    .find(|&n| doc.element(n).and_then(|e| e.id()) == Some(id))
                    .expect("the element");
                root.flatten()
                    .into_iter()
                    .find(|fragment| tree.get(fragment.box_id).node == Some(node))
                    .expect("its fragment")
                    .content
                    .y
            })
            .collect();
        assert!(close(ys[0], 0.0));
        assert!(close(ys[1], 40.0));
    }

    #[test]
    fn adjacent_margins_collapse_to_the_larger() {
        // 30 and 20 become 30, not 50.
        let (doc, tree, root) = run(
            "<div id=a></div><div id=b></div>",
            "div { height: 10px } #a { margin-bottom: 30px } #b { margin-top: 20px }",
            800.0,
        );
        let second = doc
            .descendants(doc.root())
            .find(|&n| doc.element(n).and_then(|e| e.id()) == Some("b"))
            .expect("the second div");
        let rect = root
            .flatten()
            .into_iter()
            .find(|fragment| tree.get(fragment.box_id).node == Some(second))
            .expect("its fragment")
            .content;
        assert!(
            close(rect.y, 40.0),
            "10 height + 30 collapsed, got {}",
            rect.y
        );
    }

    #[test]
    fn two_negative_margins_collapse_to_the_more_negative() {
        let (doc, tree, root) = run(
            "<div id=a></div><div id=b></div>",
            "div { height: 10px } #a { margin-bottom: -5px } #b { margin-top: -20px }",
            800.0,
        );
        let second = doc
            .descendants(doc.root())
            .find(|&n| doc.element(n).and_then(|e| e.id()) == Some("b"))
            .expect("the second div");
        let rect = root
            .flatten()
            .into_iter()
            .find(|fragment| tree.get(fragment.box_id).node == Some(second))
            .expect("its fragment")
            .content;
        assert!(close(rect.y, -10.0), "10 height - 20, got {}", rect.y);
    }

    #[test]
    fn a_mixed_pair_of_margins_adds_rather_than_collapsing() {
        assert!(close(collapse(20.0, -10.0), 10.0));
        assert!(close(collapse(-10.0, 20.0), 10.0));
        assert!(close(collapse(30.0, 20.0), 30.0));
        assert!(close(collapse(-5.0, -20.0), -20.0));
    }

    #[test]
    fn text_wraps_inside_the_block_it_is_in() {
        // 16px font, 0.5 ratio: "aaa" is 24px, so 60px fits two words.
        let rect = rect_of(
            "<div>aaa bbb ccc</div>",
            "div { width: 60px }",
            800.0,
            "div",
        );
        assert!(
            close(rect.height, 19.2 * 2.0),
            "two lines, got {}",
            rect.height
        );
    }

    #[test]
    fn a_narrower_block_wraps_text_into_more_lines() {
        let wide = rect_of(
            "<div>aaa bbb ccc</div>",
            "div { width: 500px }",
            800.0,
            "div",
        );
        let narrow = rect_of(
            "<div>aaa bbb ccc</div>",
            "div { width: 30px }",
            800.0,
            "div",
        );
        assert!(close(wide.height, 19.2));
        assert!(close(narrow.height, 19.2 * 3.0));
    }

    #[test]
    fn padding_shifts_the_content_box_inward() {
        let rect = rect_of("<div>x</div>", "div { padding: 10px 20px }", 800.0, "div");
        assert!(close(rect.x, 20.0));
        assert!(close(rect.y, 10.0));
        assert!(close(rect.width, 760.0));
    }

    #[test]
    fn a_percentage_height_resolves_against_a_definite_parent() {
        let rect = rect_of(
            "<section><div></div></section>",
            "section { height: 400px } div { height: 25% }",
            800.0,
            "div",
        );
        assert!(close(rect.height, 100.0));
    }

    #[test]
    fn a_percentage_height_against_an_auto_parent_behaves_as_auto() {
        // Nothing to take a percentage of, so the content decides.
        let rect = rect_of(
            "<section><div>x</div></section>",
            "div { height: 50% }",
            800.0,
            "div",
        );
        assert!(close(rect.height, 19.2), "got {}", rect.height);
    }

    #[test]
    fn an_empty_document_lays_out_to_nothing() {
        let doc = Document::new();
        let styles = StyleTree::default();
        let tree = BoxTree::build(&doc, &styles);
        assert!(layout_document(&tree, 800.0, 600.0, &RatioMetrics::DEFAULT).is_none());
    }

    #[test]
    fn nested_blocks_each_see_their_own_containing_block() {
        let rect = rect_of(
            "<section><div></div></section>",
            "section { width: 300px; padding: 0 25px }",
            800.0,
            "div",
        );
        assert!(close(rect.width, 300.0), "the section's content width");
        assert!(close(rect.x, 25.0));
    }

    #[test]
    fn a_fragment_knows_all_four_of_its_boxes() {
        let (doc, tree, root) = run(
            "<div>x</div>",
            "div { width: 100px; height: 50px; padding: 5px; border: 2px solid; margin: 10px }",
            800.0,
        );
        let target = doc.first_html_element("div").expect("the div");
        let fragment = root
            .flatten()
            .into_iter()
            .find(|fragment| tree.get(fragment.box_id).node == Some(target))
            .expect("its fragment");
        assert!(close(fragment.content.width, 100.0));
        assert!(close(fragment.padding_box().width, 110.0));
        assert!(close(fragment.border_box().width, 114.0));
        assert!(
            close(fragment.content.x, 17.0),
            "10 margin + 2 border + 5 padding"
        );
        // The box is over-constrained — a fixed width with two fixed
        // margins in an 800px parent — so the right margin was
        // overridden and the margin box spans the containing block.
        assert!(close(fragment.margin_box().width, 800.0));
        assert!(close(fragment.margin.left, 10.0));
        assert!(close(fragment.margin.right, 676.0));
    }
    #[test]
    fn a_max_width_under_border_box_sizing_covers_the_padding() {
        // The real case: `max-width: 420px; padding: 0 24px;
        // box-sizing: border-box` is 420 wide in total, so its
        // content is 372. Treating the limit as a content width makes
        // the card 468 and everything inside inherits the mistake.
        let rect = rect_of(
            "<div></div>",
            "div { max-width: 420px; padding: 0 24px; box-sizing: border-box }",
            800.0,
            "div",
        );
        assert!(close(rect.width, 372.0), "got {}", rect.width);
    }

    #[test]
    fn a_max_width_under_content_box_sizing_does_not() {
        let rect = rect_of(
            "<div></div>",
            "div { max-width: 420px; padding: 0 24px; box-sizing: content-box }",
            800.0,
            "div",
        );
        assert!(close(rect.width, 420.0), "got {}", rect.width);
    }

    #[test]
    fn a_min_width_under_border_box_sizing_covers_it_too() {
        let rect = rect_of(
            "<div></div>",
            "div { width: 10px; min-width: 200px; padding: 0 20px;
                   box-sizing: border-box }",
            800.0,
            "div",
        );
        assert!(close(rect.width, 160.0), "got {}", rect.width);
    }

    #[test]
    fn a_min_height_raises_a_block_that_would_be_shorter() {
        // It did nothing at all before: block layout settled its
        // height from the content and never looked at the limits.
        let rect = border_box_of(
            "<div>one line</div>",
            "div { min-height: 300px }",
            800.0,
            "div",
        );
        assert!(close(rect.height, 300.0), "got {}", rect.height);
    }

    #[test]
    fn a_max_height_caps_a_block_that_would_be_taller() {
        let rect = border_box_of(
            "<div><p></p><p></p></div>",
            "p { height: 400px } div { max-height: 100px }",
            800.0,
            "div",
        );
        assert!(close(rect.height, 100.0), "got {}", rect.height);
    }

    #[test]
    fn a_min_height_under_border_box_sizing_covers_the_padding() {
        // The hero-panel shape: `min-height: 100vh; padding: 48px`
        // under the usual `* { box-sizing: border-box }` reset is one
        // viewport tall, not a viewport plus its own padding.
        let rect = border_box_of(
            "<div></div>",
            "div { min-height: 300px; padding: 48px; box-sizing: border-box }",
            800.0,
            "div",
        );
        assert!(close(rect.height, 300.0), "got {}", rect.height);
    }

    #[test]
    fn a_min_height_under_content_box_sizing_does_not() {
        let rect = border_box_of(
            "<div></div>",
            "div { min-height: 300px; padding: 48px; box-sizing: content-box }",
            800.0,
            "div",
        );
        assert!(close(rect.height, 396.0), "got {}", rect.height);
    }

    #[test]
    fn a_declared_height_under_border_box_sizing_covers_the_padding_too() {
        let rect = border_box_of(
            "<div></div>",
            "div { height: 200px; padding: 20px; box-sizing: border-box }",
            800.0,
            "div",
        );
        assert!(close(rect.height, 200.0), "got {}", rect.height);
    }
}
