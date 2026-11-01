//! Flex layout.
//!
//! The algorithm that puts a nav bar in a row and a card's stats
//! side by side — which is most of what a modern page's structure *is*.
//! Without it every flex container collapses into a vertical stack, and
//! worse: a flex child with no height of its own gets **zero** height,
//! because the thing that would have given it one is this.
//!
//! That last point is the surprising one and worth stating plainly.
//! `align-items` defaults to `stretch`, so an item with `height: auto`
//! grows to its line's cross size. A page that puts a gradient panel
//! beside a form relies on it: the panel has no content of its own, and
//! its entire height comes from being stretched to match its sibling.
//! Lay it out as a block instead and it is zero pixels tall, the
//! gradient paints nothing, and the white text on it vanishes.
//!
//! The five steps, in order, each depending on the last:
//!
//! 1. **Base sizes.** Each item's main size before flexing, from
//!    `flex-basis`, else its main-axis size property, else its content.
//! 2. **Lines.** Items are collected into lines, one unless the
//!    container wraps.
//! 3. **Flexing.** Free space on each line is handed out by
//!    `flex-grow`, or taken back by `flex-shrink`.
//! 4. **Cross sizing.** Each line is as thick as its tallest item, and
//!    `stretch` items grow to match.
//! 5. **Alignment.** `justify-content` along the main axis,
//!    `align-items` across it.
//!
//! Written axis-agnostically: everything below works in main and cross
//! terms, and [`Axis`] maps those onto x and y once. A row-specific and
//! a column-specific implementation would be two chances to get the
//! same rule wrong.
//!
//! Not implemented: `align-content` for multi-line containers, baseline
//! alignment (treated as flex-start), `order`, and percentage
//! `flex-basis` against an indefinite container.

use crate::engine::cryo_impl::css::computed::{
    AlignItems, ComputedStyle, FlexDirection, JustifyContent, Margin,
};
use crate::engine::cryo_impl::css::value::{LengthPercentage, Size};

use super::block::ContainingBlock;
use super::box_tree::{BoxId, BoxTree};
use super::geometry::{Fragment, Point, Rect};
use super::measure::TextMeasure;

/// Which way round main and cross are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Axis {
    row: bool,
    reversed: bool,
}

impl Axis {
    fn of(direction: FlexDirection) -> Self {
        Self {
            row: direction.is_row(),
            reversed: direction.is_reversed(),
        }
    }

    /// The main-axis extent of a width/height pair.
    fn main(self, width: f32, height: f32) -> f32 {
        if self.row {
            width
        } else {
            height
        }
    }

    fn cross(self, width: f32, height: f32) -> f32 {
        if self.row {
            height
        } else {
            width
        }
    }

    /// Turn a main/cross position and size back into a rectangle.
    fn rect(self, main: f32, cross: f32, main_size: f32, cross_size: f32) -> Rect {
        if self.row {
            Rect::new(main, cross, main_size, cross_size)
        } else {
            Rect::new(cross, main, cross_size, main_size)
        }
    }
}

/// One item, as the algorithm needs it.
struct Item {
    id: BoxId,
    /// Margins, borders and padding along each axis — the part of an
    /// item's outer size that never flexes.
    main_frame: f32,
    cross_frame: f32,
    /// The main size before flexing.
    base_main: f32,
    /// The cross size the item would take on its own.
    hypothetical_cross: f32,
    grow: f32,
    shrink: f32,
    /// Resolved during flexing.
    main_size: f32,
    cross_size: f32,
    align: AlignItems,
    /// Whether the cross size came from the style rather than content,
    /// which is what makes `stretch` leave it alone.
    cross_is_definite: bool,
    /// Which margins are `auto`, as (start, end) along each axis.
    ///
    /// They are not zero here the way they are in block layout: an
    /// auto margin on a flex item *absorbs free space*, and does it
    /// before `justify-content` or `align-self` get a say. That one
    /// rule is what `margin-inline: auto` relies on, and with it
    /// missing every page that centres a column that way — which is
    /// every Tailwind page, as `mx-auto` — renders flush left.
    main_auto: (bool, bool),
    cross_auto: (bool, bool),
}

impl Item {
    /// How many of this item's margins along an axis are `auto`.
    fn auto_count(pair: (bool, bool)) -> f32 {
        f32::from(u8::from(pair.0) + u8::from(pair.1))
    }

    /// The item's outer size along the cross axis.
    fn cross_outer(&self) -> f32 {
        self.cross_size + self.cross_frame
    }
}

/// Lay out a flex container's children.
///
/// Returns the content height used and the children's fragments, in the
/// same shape block layout returns, so the caller does not care which
/// algorithm ran.
pub fn layout(
    tree: &BoxTree,
    container: BoxId,
    inner: &ContainingBlock,
    content_origin: Point,
    metrics: &dyn TextMeasure,
) -> (f32, Vec<Fragment>) {
    let style = &tree.get(container).style;
    let axis = Axis::of(style.flex_direction);
    let children: Vec<BoxId> = tree
        .children(container)
        .iter()
        .copied()
        .filter(|&child| {
            let style = &tree.get(child).style;
            // An out-of-flow box is not a flex item: it is placed
            // against its containing block, not along the main axis.
            style.display.generates_box() && !style.position.is_out_of_flow()
        })
        .collect();
    if children.is_empty() {
        return (0.0, Vec::new());
    }

    // A row's main axis is the width, which a block container always
    // hands down; a column's is the height, which may be `auto`.
    let definite_main = if axis.row {
        Some(inner.width)
    } else {
        inner.height
    };
    let mut items: Vec<Item> = children
        .iter()
        .map(|&child| measure_item(tree, child, inner, axis, style, metrics))
        .collect();

    let gap = resolve(
        if axis.row {
            style.column_gap
        } else {
            style.row_gap
        },
        inner.width,
    );

    // With an indefinite main size there is no free space: the items
    // keep their flex base sizes, nothing grows or shrinks, and the
    // container ends up as long as they are. Calling that space zero
    // instead is what made an auto-height column shrink every item
    // down to nothing.
    let main_available = definite_main.unwrap_or_else(|| {
        items
            .iter()
            .map(|item| item.base_main + item.main_frame)
            .sum::<f32>()
            + gap * (items.len() as f32 - 1.0).max(0.0)
    });

    let lines = collect_lines(&mut items, main_available, gap, style);
    place_lines(
        tree,
        &mut items,
        &lines,
        axis,
        style,
        inner,
        content_origin,
        main_available,
        gap,
        metrics,
    )
}

/// An item's sizes before any flexing.
fn measure_item(
    tree: &BoxTree,
    id: BoxId,
    inner: &ContainingBlock,
    axis: Axis,
    container: &ComputedStyle,
    metrics: &dyn TextMeasure,
) -> Item {
    let style = &tree.get(id).style;
    let padding = style.padding.map(|value| resolve(value, inner.width));
    let margin = style.margin.map(|value| used_margin(value, inner.width));
    let frame_x = padding.horizontal() + style.border_width.horizontal() + margin.horizontal();
    let frame_y = padding.vertical() + style.border_width.vertical() + margin.vertical();

    // A declared size is a *border* box under `box-sizing:
    // border-box`, but every size this algorithm works with is a
    // content box. Skipping the conversion makes a padded item wider
    // than it asked to be by exactly its own padding, which pushes it
    // outside its container.
    let inside = |size: Option<f32>, frame: f32| {
        size.map(|value| (value - super::block::frame_inset(style, frame)).max(0.0))
    };

    // `flex-basis` wins over the size property; `auto` falls back to it.
    let main_frame = axis.main(frame_x, frame_y);
    let cross_frame = axis.cross(frame_x, frame_y);
    let basis = inside(
        match style.flex_basis {
            Size::Definite(_) => axis.definite(style.flex_basis, inner, Along::Main),
            Size::Auto => axis.definite(axis.main_size(style), inner, Along::Main),
        },
        main_frame,
    );
    let cross_definite = inside(
        axis.definite(axis.cross_size(style), inner, Along::Cross),
        cross_frame,
    );

    // An auto main size is the item's max-content size, not the space
    // available: measuring it as the latter makes two auto items look
    // like twice the container and flips growing into shrinking.
    let content_main = match basis {
        Some(_) => 0.0,
        None if axis.row => super::intrinsic::max_content_width(tree, id, metrics) - frame_x,
        // Down the column the main axis is height, which only a layout
        // pass can answer. The probe reports a *border* box, and
        // everything here works in content sizes — adding the frame
        // back on later is what makes a padded field measure as its
        // own padding taller than it is.
        None => probe(tree, id, inner, axis, metrics).0 - main_frame,
    };
    let content_cross = match cross_definite {
        Some(_) => 0.0,
        // A content size, for the same reason as above.
        None => probe(tree, id, inner, axis, metrics).1 - cross_frame,
    };

    Item {
        id,
        main_frame,
        cross_frame,
        base_main: axis.clamp(
            style,
            basis.unwrap_or(content_main).max(0.0),
            inner,
            Along::Main,
            main_frame,
        ),
        hypothetical_cross: axis.clamp(
            style,
            cross_definite.unwrap_or(content_cross).max(0.0),
            inner,
            Along::Cross,
            cross_frame,
        ),
        grow: style.flex_grow,
        shrink: style.flex_shrink,
        main_size: 0.0,
        cross_size: 0.0,
        align: style.align_self.unwrap_or(container.align_items),
        cross_is_definite: cross_definite.is_some(),
        main_auto: axis.auto_margins(style, Along::Main),
        cross_auto: axis.auto_margins(style, Along::Cross),
    }
}

impl Axis {
    /// The style property that sizes the main axis.
    fn main_size(self, style: &ComputedStyle) -> Size {
        if self.row {
            style.width
        } else {
            style.height
        }
    }

    fn cross_size(self, style: &ComputedStyle) -> Size {
        if self.row {
            style.height
        } else {
            style.width
        }
    }
}

/// Lay an item out as a block to find out how big its content is.
///
/// Expensive — it is a full layout pass that gets thrown away — and the
/// honest way to get an intrinsic size without an intrinsic-sizing
/// pass. Caching it is the obvious next step.
fn probe(
    tree: &BoxTree,
    id: BoxId,
    inner: &ContainingBlock,
    axis: Axis,
    metrics: &dyn TextMeasure,
) -> (f32, f32) {
    let probe_block = ContainingBlock {
        width: inner.width,
        height: None,
    };
    let fragment =
        super::block::layout_box(tree, id, &probe_block, Point { x: 0.0, y: 0.0 }, metrics);
    let outer = fragment.border_box();
    (
        axis.main(outer.width, outer.height),
        axis.cross(outer.width, outer.height),
    )
}

/// Split items into lines, each a range of indices.
fn collect_lines(
    items: &mut [Item],
    available: f32,
    gap: f32,
    style: &ComputedStyle,
) -> Vec<(usize, usize)> {
    if !style.flex_wrap.wraps() || items.is_empty() {
        return vec![(0, items.len())];
    }
    let mut lines = Vec::new();
    let mut start = 0;
    let mut used = 0.0;
    for (index, item) in items.iter().enumerate() {
        let outer = item.base_main + item.main_frame;
        let with_gap = if index == start {
            outer
        } else {
            used + gap + outer
        };
        if index > start && with_gap > available {
            lines.push((start, index));
            start = index;
            used = outer;
        } else {
            used = with_gap;
        }
    }
    lines.push((start, items.len()));
    lines
}

/// Hand out or claw back the free space on one line.
///
/// Growing and shrinking are deliberately asymmetric, as the spec has
/// them: growth is shared by `flex-grow` alone, while shrinkage is
/// weighted by each item's *size* as well as its `flex-shrink`, so a
/// large item gives up more than a small one.
fn flex_line(items: &mut [Item], range: (usize, usize), available: f32, gap: f32) {
    let (start, end) = range;
    if start >= end {
        return;
    }
    let count = (end - start) as f32;
    let total_gap = gap * (count - 1.0).max(0.0);
    let used: f32 = items[start..end]
        .iter()
        .map(|item| item.base_main + item.main_frame)
        .sum();
    let free = available - used - total_gap;

    if free >= 0.0 {
        let total_grow: f32 = items[start..end].iter().map(|item| item.grow).sum();
        for item in &mut items[start..end] {
            item.main_size = if total_grow > 0.0 {
                item.base_main + free * (item.grow / total_grow)
            } else {
                item.base_main
            };
        }
    } else {
        let total_weight: f32 = items[start..end]
            .iter()
            .map(|item| item.shrink * item.base_main)
            .sum();
        for item in &mut items[start..end] {
            let shrunk = if total_weight > 0.0 {
                item.base_main + free * (item.shrink * item.base_main / total_weight)
            } else {
                item.base_main
            };
            // An item never shrinks below nothing.
            item.main_size = shrunk.max(0.0);
        }
    }
}

/// Position every line and every item in it.
#[allow(clippy::too_many_arguments)]
fn place_lines(
    tree: &BoxTree,
    items: &mut [Item],
    lines: &[(usize, usize)],
    axis: Axis,
    style: &ComputedStyle,
    inner: &ContainingBlock,
    content_origin: Point,
    main_available: f32,
    gap: f32,
    metrics: &dyn TextMeasure,
) -> (f32, Vec<Fragment>) {
    let cross_gap = resolve(
        if axis.row {
            style.row_gap
        } else {
            style.column_gap
        },
        inner.width,
    );
    // A definite cross size on the container is what `stretch` fills.
    let container_cross = axis.cross(inner.width, inner.height.unwrap_or(0.0));

    let mut fragments = Vec::new();
    let mut cross_cursor = 0.0f32;
    let single_line = lines.len() == 1;

    for &range in lines {
        flex_line(items, range, main_available, gap);
        let (start, end) = range;

        // Growing and shrinking both ignore the limits, so the used
        // size is clamped afterwards — an item that was told to grow
        // still stops at its `max-width`.
        for item in &mut items[start..end] {
            let style = &tree.get(item.id).style;
            item.main_size = axis.clamp(style, item.main_size, inner, Along::Main, item.main_frame);
        }

        // The line is as thick as its thickest item, unless the
        // container's own cross size is definite and this is the only
        // line — then the line fills it, which is what lets a lone item
        // stretch to the container's height.
        let natural_cross = items[start..end]
            .iter()
            .map(|item| item.hypothetical_cross + item.cross_frame)
            .fold(0.0f32, f32::max);
        let line_cross = if single_line && container_cross > 0.0 {
            // The container's own size, *not* the larger of the two.
            // An item too big for the line overflows it; it does not
            // widen it. Taking the maximum instead lets one wide
            // child stretch every sibling past the container — a form
            // field whose padding makes it a few pixels too wide ends
            // up dragging the whole column outside the card.
            container_cross
        } else {
            natural_cross
        };

        for item in &mut items[start..end] {
            let size = match item.align {
                // The default, and the one that matters: fill the line.
                AlignItems::Stretch if !item.cross_is_definite => {
                    (line_cross - item.cross_frame).max(0.0)
                }
                _ => item.hypothetical_cross,
            };
            // Stretching obeys the limits too: a `max-height` item in a
            // tall row stays its own height.
            let style = &tree.get(item.id).style;
            item.cross_size = axis.clamp(style, size, inner, Along::Cross, item.cross_frame);
        }

        let used_main: f32 = items[start..end]
            .iter()
            .map(|item| item.main_size + item.main_frame)
            .sum::<f32>()
            + gap * ((end - start) as f32 - 1.0).max(0.0);
        // Auto margins take the free space first, and leave none for
        // `justify-content` — that is the order the spec gives, and
        // it is what makes one auto margin push an item to the far
        // end while two centre it.
        let free_main = main_available - used_main;
        let auto_main: f32 = items[start..end]
            .iter()
            .map(|item| Item::auto_count(item.main_auto))
            .sum();
        let (share, free_left) = if auto_main > 0.0 && free_main > 0.0 {
            (free_main / auto_main, 0.0)
        } else {
            (0.0, free_main)
        };
        let (start_offset, between) = distribute(style.justify_content, free_left, end - start);
        let mut main_cursor = start_offset;

        // A reversed direction places from the far end backwards.
        let order: Vec<usize> = if axis.reversed {
            (start..end).rev().collect()
        } else {
            (start..end).collect()
        };

        for index in order {
            let item = &items[index];
            if item.main_auto.0 {
                main_cursor += share;
            }
            let rect = axis.rect(
                content_origin_main(axis, content_origin) + main_cursor,
                content_origin_cross(axis, content_origin)
                    + cross_cursor
                    + cross_offset(item, line_cross),
                item.main_size + item.main_frame,
                item.cross_outer(),
            );
            fragments.push(lay_out_item(tree, item, rect, metrics));
            main_cursor += item.main_size + item.main_frame + gap + between;
            if item.main_auto.1 {
                main_cursor += share;
            }
        }

        cross_cursor += line_cross + cross_gap;
    }

    let total_cross = (cross_cursor - cross_gap).max(0.0);
    let height = if axis.row {
        total_cross
    } else {
        // In a column the main axis is vertical, so the content height
        // is how far down the items reached.
        fragments
            .iter()
            .map(|fragment| fragment.margin_box().bottom() - content_origin.y)
            .fold(0.0f32, f32::max)
    };
    (height, fragments)
}

fn content_origin_main(axis: Axis, origin: Point) -> f32 {
    if axis.row {
        origin.x
    } else {
        origin.y
    }
}

fn content_origin_cross(axis: Axis, origin: Point) -> f32 {
    if axis.row {
        origin.y
    } else {
        origin.x
    }
}

/// Lay an item out at the size flex decided, then move it into place.
fn lay_out_item(tree: &BoxTree, item: &Item, outer: Rect, metrics: &dyn TextMeasure) -> Fragment {
    let style = &tree.get(item.id).style;
    let margin = style.margin.map(|value| used_margin(value, outer.width));
    let padding = style
        .padding
        .map(|value| resolve(value, outer.width.max(0.0)));
    let border = style.border_width;

    let content = Rect::new(
        outer.x + margin.left + border.left + padding.left,
        outer.y + margin.top + border.top + padding.top,
        (outer.width - margin.horizontal() - border.horizontal() - padding.horizontal()).max(0.0),
        (outer.height - margin.vertical() - border.vertical() - padding.vertical()).max(0.0),
    );

    // The item's own children lay out inside the size it was given.
    let child_block = ContainingBlock {
        width: content.width,
        height: Some(content.height),
    };
    let (_, children) = super::block::layout_contents(
        tree,
        item.id,
        &child_block,
        Point {
            x: content.x,
            y: content.y,
        },
        metrics,
    );

    let mut fragment = Fragment {
        box_id: item.id,
        content,
        padding,
        border,
        margin,
        text: None,
        children,
    };
    // A flex item may itself be positioned, and its out-of-flow
    // descendants resolve against it like any other box's.
    super::absolute::attach(tree, &mut fragment, metrics);
    fragment
}

/// Where the first item starts, and how much extra sits between items.
fn distribute(justify: JustifyContent, free: f32, count: usize) -> (f32, f32) {
    let free = free.max(0.0);
    let gaps = count.saturating_sub(1) as f32;
    match justify {
        JustifyContent::FlexStart => (0.0, 0.0),
        JustifyContent::FlexEnd => (free, 0.0),
        JustifyContent::Center => (free / 2.0, 0.0),
        JustifyContent::SpaceBetween if gaps > 0.0 => (0.0, free / gaps),
        // With one item there is nothing to space between, so it sits
        // at the start — not centred, which is the common misreading.
        JustifyContent::SpaceBetween => (0.0, 0.0),
        JustifyContent::SpaceAround if count > 0 => {
            let each = free / count as f32;
            (each / 2.0, each)
        }
        JustifyContent::SpaceEvenly if count > 0 => {
            let each = free / (count as f32 + 1.0);
            (each, each)
        }
        _ => (0.0, 0.0),
    }
}

/// How far across its line an item sits.
///
/// Auto margins come first and override the alignment entirely: two
/// of them centre the item, one pushes it to the opposite edge. Only
/// an item with neither is aligned by `align-self`.
fn cross_offset(item: &Item, line_cross: f32) -> f32 {
    let slack = (line_cross - item.cross_outer()).max(0.0);
    match item.cross_auto {
        (true, true) => slack / 2.0,
        (true, false) => slack,
        (false, true) => 0.0,
        (false, false) => align_offset(item.align, line_cross, item.cross_outer()),
    }
}

/// How far down its line an item sits.
fn align_offset(align: AlignItems, line_cross: f32, item_cross: f32) -> f32 {
    let slack = (line_cross - item_cross).max(0.0);
    match align {
        AlignItems::FlexEnd => slack,
        AlignItems::Center => slack / 2.0,
        // Baseline alignment needs the items' baselines; treating it as
        // flex-start is wrong but stable, and stated in the module docs.
        AlignItems::FlexStart | AlignItems::Baseline | AlignItems::Stretch => 0.0,
    }
}

fn resolve(value: LengthPercentage, basis: f32) -> f32 {
    match value {
        LengthPercentage::Length(length) => length.value,
        LengthPercentage::Percentage(percent) => percent / 100.0 * basis,
    }
}

fn used_margin(margin: Margin, basis: f32) -> f32 {
    match margin {
        Margin::Auto => 0.0,
        Margin::Length(value) => resolve(value, basis),
    }
}

/// Which of a flex container's two axes a size is measured along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Along {
    Main,
    Cross,
}

impl Axis {
    /// Apply `min-`/`max-` to a size along one of the container's axes.
    ///
    /// A flex item's base size is clamped before it flexes and its used
    /// size after, which is what stops a `max-width` card from
    /// stretching across a panel it was meant to sit in the middle of.
    fn clamp(
        self,
        style: &ComputedStyle,
        value: f32,
        inner: &ContainingBlock,
        along: Along,
        frame: f32,
    ) -> f32 {
        if self.row == (along == Along::Main) {
            // The frame matters under `box-sizing: border-box`, where
            // a `max-width` covers the padding too — see
            // `block::clamp_width_inside`.
            super::block::clamp_width_inside(style, value, inner.width, frame)
        } else {
            super::block::clamp_height_inside(style, value, inner, frame)
        }
    }

    /// Which margins along one of the container's axes are `auto`,
    /// as (start, end) in that axis's own direction.
    fn auto_margins(self, style: &ComputedStyle, along: Along) -> (bool, bool) {
        let auto = |margin: Margin| margin == Margin::Auto;
        let horizontal = self.row == (along == Along::Main);
        let (start, end) = if horizontal {
            (style.margin.left, style.margin.right)
        } else {
            (style.margin.top, style.margin.bottom)
        };
        // A reversed main axis starts at the far edge, so the margins
        // swap with it — otherwise a single auto margin pushes the
        // item the wrong way.
        if along == Along::Main && self.reversed {
            (auto(end), auto(start))
        } else {
            (auto(start), auto(end))
        }
    }

    /// A definite size along one of the container's axes.
    ///
    /// Which physical axis that is depends on the direction, and the
    /// physical axis decides what a percentage is a percentage *of*.
    /// Resolving a height against the width instead is how a
    /// `height: 100%` item ends up as tall as its container is wide.
    fn definite(self, size: Size, inner: &ContainingBlock, along: Along) -> Option<f32> {
        let horizontal = self.row == (along == Along::Main);
        if horizontal {
            super::block::definite_width(size, inner.width)
        } else {
            super::block::definite_height(size, inner)
        }
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
    use crate::engine::cryo_impl::layout::block::layout_document;
    use crate::engine::cryo_impl::layout::measure::RatioMetrics;

    const UA: &str = "html, body, div, p { display: block } head { display: none }";

    /// Lay a page out and return the border boxes of elements by id.
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
        let tree = BoxTree::build(&doc, &styles);
        let root = layout_document(&tree, 600.0, 400.0, &RatioMetrics::DEFAULT).expect("a root");

        let mut out = Vec::new();
        collect(&doc, &tree, &root, &mut out);
        out
    }

    fn collect(doc: &Document, tree: &BoxTree, fragment: &Fragment, out: &mut Vec<(String, Rect)>) {
        if let Some(node) = tree.get(fragment.box_id).node {
            if let Some(id) = doc.element(node).and_then(|element| element.id()) {
                out.push((id.to_owned(), fragment.border_box()));
            }
        }
        for child in &fragment.children {
            collect(doc, tree, child, out);
        }
    }

    fn rect_of(boxes: &[(String, Rect)], id: &str) -> Rect {
        boxes
            .iter()
            .find(|(name, _)| name == id)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("no box with id {id}"))
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 0.5
    }

    const TWO_ITEMS: &str = "<div id=flex><div id=a></div><div id=b></div></div>";

    #[test]
    fn flex_items_sit_in_a_row() {
        // The headline difference from block layout.
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex } #a, #b { width: 100px; height: 50px }",
        );
        let a = rect_of(&laid, "a");
        let b = rect_of(&laid, "b");
        assert!(close(a.y, b.y), "same row: {} vs {}", a.y, b.y);
        assert!(close(b.x, a.right()), "side by side");
    }

    #[test]
    fn a_column_container_stacks_them() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; flex-direction: column } #a, #b { width: 100px; height: 50px }",
        );
        let a = rect_of(&laid, "a");
        let b = rect_of(&laid, "b");
        assert!(close(a.x, b.x));
        assert!(close(b.y, a.bottom()));
    }

    #[test]
    fn an_item_with_no_height_stretches_to_its_row() {
        // The bug that started this: a flex child with `height: auto`
        // takes the line's height, not zero.
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex } #a { width: 100px; height: 80px } #b { width: 100px }",
        );
        let b = rect_of(&laid, "b");
        assert!(close(b.height, 80.0), "stretched to 80, got {}", b.height);
    }

    #[test]
    fn align_items_flex_start_leaves_an_item_at_its_own_height() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; align-items: flex-start }
             #a { width: 100px; height: 80px } #b { width: 100px; height: 20px }",
        );
        let b = rect_of(&laid, "b");
        assert!(close(b.height, 20.0));
        assert!(close(b.y, rect_of(&laid, "a").y), "at the top of the line");
    }

    #[test]
    fn align_items_centre_puts_an_item_in_the_middle_of_its_line() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; align-items: center }
             #a { width: 100px; height: 100px } #b { width: 100px; height: 20px }",
        );
        let a = rect_of(&laid, "a");
        let b = rect_of(&laid, "b");
        assert!(close(b.y, a.y + 40.0), "centred: {} vs {}", b.y, a.y + 40.0);
    }

    #[test]
    fn align_items_flex_end_pushes_an_item_to_the_bottom() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; align-items: flex-end }
             #a { width: 100px; height: 100px } #b { width: 100px; height: 20px }",
        );
        assert!(close(
            rect_of(&laid, "b").bottom(),
            rect_of(&laid, "a").bottom()
        ));
    }

    #[test]
    fn flex_grow_shares_the_leftover_space() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex } #a { flex-grow: 1; height: 10px }
             #b { flex-grow: 1; height: 10px }",
        );
        let a = rect_of(&laid, "a");
        let b = rect_of(&laid, "b");
        assert!(close(a.width, b.width), "{} vs {}", a.width, b.width);
        assert!(close(a.width + b.width, 600.0), "they fill the container");
    }

    #[test]
    fn flex_grow_is_shared_in_proportion() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex } #a { flex-grow: 1; height: 10px }
             #b { flex-grow: 3; height: 10px }",
        );
        let a = rect_of(&laid, "a");
        let b = rect_of(&laid, "b");
        assert!(close(b.width, a.width * 3.0), "{} vs {}", b.width, a.width);
    }

    #[test]
    fn without_grow_items_keep_their_size() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex } #a, #b { width: 100px; height: 10px }",
        );
        assert!(close(rect_of(&laid, "a").width, 100.0));
    }

    #[test]
    fn overlarge_items_shrink_to_fit() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex } #a, #b { width: 500px; height: 10px }",
        );
        let a = rect_of(&laid, "a");
        let b = rect_of(&laid, "b");
        assert!(close(a.width + b.width, 600.0), "shrunk to the container");
        assert!(close(a.width, b.width), "equally, being the same size");
    }

    #[test]
    fn flex_shrink_zero_refuses_to_shrink() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex } #a { width: 500px; height: 10px; flex-shrink: 0 }
             #b { width: 500px; height: 10px }",
        );
        assert!(close(rect_of(&laid, "a").width, 500.0));
    }

    #[test]
    fn justify_content_centre_centres_the_row() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; justify-content: center }
             #a, #b { width: 100px; height: 10px }",
        );
        // 600 wide, 200 used, so 200 of slack each side.
        assert!(close(rect_of(&laid, "a").x, 200.0));
    }

    #[test]
    fn justify_content_flex_end_pushes_the_row_right() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; justify-content: flex-end }
             #a, #b { width: 100px; height: 10px }",
        );
        assert!(close(rect_of(&laid, "b").right(), 600.0));
    }

    #[test]
    fn space_between_pins_the_ends_and_spreads_the_rest() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; justify-content: space-between }
             #a, #b { width: 100px; height: 10px }",
        );
        assert!(close(rect_of(&laid, "a").x, 0.0));
        assert!(close(rect_of(&laid, "b").right(), 600.0));
    }

    #[test]
    fn space_between_with_one_item_leaves_it_at_the_start() {
        // The common misreading is that it centres.
        let laid = boxes(
            "<div id=flex><div id=a></div></div>",
            "#flex { display: flex; justify-content: space-between }
             #a { width: 100px; height: 10px }",
        );
        assert!(close(rect_of(&laid, "a").x, 0.0));
    }

    #[test]
    fn a_gap_separates_items() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; gap: 20px } #a, #b { width: 100px; height: 10px }",
        );
        assert!(close(rect_of(&laid, "b").x, 120.0));
    }

    #[test]
    fn row_reverse_places_from_the_right() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; flex-direction: row-reverse }
             #a, #b { width: 100px; height: 10px }",
        );
        let a = rect_of(&laid, "a");
        let b = rect_of(&laid, "b");
        assert!(b.x < a.x, "b comes first: {} then {}", b.x, a.x);
    }

    #[test]
    fn wrapping_starts_a_second_line() {
        let laid = boxes(
            "<div id=flex><div id=a></div><div id=b></div><div id=c></div></div>",
            "#flex { display: flex; flex-wrap: wrap }
             #a, #b, #c { width: 250px; height: 30px }",
        );
        let a = rect_of(&laid, "a");
        let c = rect_of(&laid, "c");
        assert!(close(a.y, rect_of(&laid, "b").y), "a and b share a line");
        assert!(c.y > a.y, "c wrapped to the next: {} vs {}", c.y, a.y);
    }

    #[test]
    fn nowrap_keeps_everything_on_one_line() {
        let laid = boxes(
            "<div id=flex><div id=a></div><div id=b></div><div id=c></div></div>",
            "#flex { display: flex } #a, #b, #c { width: 250px; height: 30px }",
        );
        let a = rect_of(&laid, "a");
        assert!(close(a.y, rect_of(&laid, "c").y));
    }

    #[test]
    fn a_container_grows_to_hold_its_line() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex } #a { width: 100px; height: 70px }
             #b { width: 100px; height: 30px }",
        );
        assert!(close(rect_of(&laid, "flex").height, 70.0));
    }

    #[test]
    fn an_empty_flex_container_has_no_height() {
        let laid = boxes("<div id=flex></div>", "#flex { display: flex }");
        assert!(close(rect_of(&laid, "flex").height, 0.0));
    }

    #[test]
    fn padding_on_the_container_insets_its_items() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; padding: 10px } #a, #b { width: 100px; height: 20px }",
        );
        assert!(close(rect_of(&laid, "a").x, 10.0));
        assert!(close(rect_of(&laid, "a").y, 10.0));
    }

    #[test]
    fn an_items_content_lays_out_inside_it() {
        let laid = boxes(
            "<div id=flex><div id=a><p id=inner>x</p></div></div>",
            "#flex { display: flex } #a { width: 200px; height: 100px }",
        );
        let inner = rect_of(&laid, "inner");
        let a = rect_of(&laid, "a");
        assert!(
            inner.x >= a.x && inner.right() <= a.right(),
            "inside its item"
        );
    }

    #[test]
    fn display_none_items_take_no_part() {
        let laid = boxes(
            "<div id=flex><div id=a></div><div id=gone></div><div id=b></div></div>",
            "#flex { display: flex } #gone { display: none }
             #a, #b { width: 100px; height: 10px }",
        );
        assert!(close(rect_of(&laid, "b").x, 100.0), "b sits right after a");
    }

    #[test]
    fn an_auto_height_column_keeps_its_items_at_their_own_size() {
        // There is no free space to shrink into when the main size is
        // indefinite. Treating it as zero shrank every item to nothing
        // and left every ancestor of a column flex zero pixels tall.
        let laid = boxes(
            "<div id=flex><div id=a></div><div id=b></div></div>",
            "#flex { display: flex; flex-direction: column }
             #a, #b { height: 50px }",
        );
        assert!(close(rect_of(&laid, "a").height, 50.0));
        assert!(close(rect_of(&laid, "b").height, 50.0));
        assert!(close(rect_of(&laid, "b").y, 50.0), "b below a");
        assert!(close(rect_of(&laid, "flex").height, 100.0), "both items");
    }

    #[test]
    fn an_auto_height_column_still_grows_an_item_into_a_definite_one() {
        // The counterpart: once the container's height is definite,
        // free space exists again and `flex-grow` has something to
        // take.
        let laid = boxes(
            "<div id=flex><div id=a></div><div id=b></div></div>",
            "#flex { display: flex; flex-direction: column; height: 300px }
             #a { height: 50px } #b { height: 50px; flex-grow: 1 }",
        );
        assert!(close(rect_of(&laid, "a").height, 50.0));
        assert!(close(rect_of(&laid, "b").height, 250.0), "b took the rest");
    }

    #[test]
    fn a_percentage_height_does_not_resolve_against_the_width() {
        // A percentage of a height that does not exist is `auto`, so
        // the item is as tall as its content. Resolving it against the
        // width instead made it as tall as the container is broad.
        let laid = boxes(
            "<div id=flex><div id=a><div id=inner></div></div></div>",
            "#flex { display: flex }
             #a { height: 100% }
             #inner { height: 20px }",
        );
        assert!(
            close(rect_of(&laid, "a").height, 20.0),
            "its content, not the 600px width"
        );
    }

    #[test]
    fn a_percentage_height_resolves_against_a_definite_one() {
        let laid = boxes(
            "<div id=flex><div id=a></div></div>",
            "#flex { display: flex; height: 200px }
             #a { height: 50% }",
        );
        assert!(close(rect_of(&laid, "a").height, 100.0));
    }
    #[test]
    fn a_border_box_item_keeps_its_declared_outer_width() {
        // `width: 100%` on a padded item is an *outer* 100% under
        // border-box sizing. Treating it as a content width makes the
        // item wider than its container by its own padding, and it
        // spills out of the card it was meant to sit in.
        let laid = boxes(
            "<div id=flex><div id=a></div></div>",
            "#flex { display: flex; flex-direction: column; width: 200px }
             #a { width: 100%; padding: 0 12px; box-sizing: border-box; height: 10px }",
        );
        let a = rect_of(&laid, "a");
        assert!(close(a.width, 200.0), "the border box, got {}", a.width);
        assert!(close(a.x, 0.0), "flush with the container");
    }

    #[test]
    fn a_line_does_not_grow_to_fit_an_item_too_big_for_it() {
        // An item that overflows overflows; it does not widen the
        // line and drag its siblings out with it.
        let laid = boxes(
            "<div id=flex><div id=wide></div><div id=narrow></div></div>",
            "#flex { display: flex; flex-direction: column; width: 100px }
             #wide { width: 300px; height: 10px }
             #narrow { height: 10px }",
        );
        assert!(
            close(rect_of(&laid, "wide").width, 300.0),
            "it still overflows"
        );
        assert!(
            close(rect_of(&laid, "narrow").width, 100.0),
            "but its sibling stays in the container, got {}",
            rect_of(&laid, "narrow").width
        );
    }

    #[test]
    fn a_padded_item_is_not_measured_as_its_own_padding_taller() {
        // The probe reports a border box; the algorithm works in
        // content sizes. Adding the frame back on makes a 45px field
        // measure 69 and every form grow by its own padding.
        let laid = boxes(
            "<div id=row><div id=field></div></div>",
            "#row { display: flex }
             #field { height: 21px; padding: 11px 0; border: 1px solid }",
        );
        assert!(
            close(rect_of(&laid, "row").height, 45.0),
            "got {}",
            rect_of(&laid, "row").height
        );
    }

    #[test]
    fn mx_auto_centres_an_item_down_a_column() {
        // The Tailwind page shape: a `flex-col` section holding one
        // `mx-auto` container of fixed width. With auto margins
        // ignored the whole page renders flush left.
        let laid = boxes(
            "<div id=flex><div id=a></div></div>",
            "#flex { display: flex; flex-direction: column; width: 600px }
             #a { width: 200px; height: 20px; margin-left: auto; margin-right: auto }",
        );
        assert!(
            close(rect_of(&laid, "a").x, 200.0),
            "{:?}",
            rect_of(&laid, "a")
        );
    }

    #[test]
    fn one_auto_margin_pushes_an_item_to_the_far_edge() {
        let laid = boxes(
            "<div id=flex><div id=a></div></div>",
            "#flex { display: flex; flex-direction: column; width: 600px }
             #a { width: 200px; height: 20px; margin-left: auto }",
        );
        assert!(
            close(rect_of(&laid, "a").x, 400.0),
            "{:?}",
            rect_of(&laid, "a")
        );
    }

    #[test]
    fn ml_auto_pushes_an_item_along_the_main_axis() {
        // `ml-auto` on the last item of a row is the other half of
        // the same rule, and is how a nav puts its actions right.
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; width: 600px }
             #a { width: 100px; height: 20px }
             #b { width: 100px; height: 20px; margin-left: auto }",
        );
        assert!(
            close(rect_of(&laid, "b").x, 500.0),
            "{:?}",
            rect_of(&laid, "b")
        );
    }

    #[test]
    fn auto_margins_take_the_space_before_justify_content_does() {
        // The spec's order. With `justify-content: center` also set,
        // the auto margin still wins and nothing is left to centre.
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; width: 600px; justify-content: center }
             #a { width: 100px; height: 20px }
             #b { width: 100px; height: 20px; margin-left: auto }",
        );
        assert!(
            close(rect_of(&laid, "a").x, 0.0),
            "{:?}",
            rect_of(&laid, "a")
        );
        assert!(
            close(rect_of(&laid, "b").x, 500.0),
            "{:?}",
            rect_of(&laid, "b")
        );
    }

    #[test]
    fn an_auto_margin_with_no_free_space_adds_nothing() {
        let laid = boxes(
            TWO_ITEMS,
            "#flex { display: flex; width: 200px }
             #a { width: 100px; height: 20px; flex-shrink: 0 }
             #b { width: 100px; height: 20px; flex-shrink: 0; margin-left: auto }",
        );
        assert!(
            close(rect_of(&laid, "b").x, 100.0),
            "{:?}",
            rect_of(&laid, "b")
        );
    }
}
