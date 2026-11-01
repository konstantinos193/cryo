//! Grid layout: items placed into a table of tracks.
//!
//! A grid differs from flex in what decides the geometry. Flex asks
//! the items how big they want to be and shares what is left; a grid
//! decides the *tracks* first, from the container's own template, and
//! then puts items into them. An item's size follows from its cell,
//! not the other way round.
//!
//! Three steps, in order, because each needs the one before:
//!
//! 1. **Place.** Each item gets a column range and a row range —
//!    either the one it asked for with `grid-column`/`grid-row`, or
//!    the next free cell going along the rows.
//! 2. **Size the tracks.** Fixed tracks take their length; `auto`
//!    tracks take what their content needs; `fr` tracks divide
//!    whatever is left. That order is forced: a fraction is a share
//!    of the leftover, and there is no leftover until the rest are
//!    settled.
//! 3. **Lay the items out**, each in the rectangle its tracks mark.
//!
//! ```text
//!   template ──▶ tracks ──┐
//!                         ├──▶ cell rectangles ──▶ fragments
//!   items ──▶ placement ──┘
//! ```
//!
//! Known gaps, stated rather than hidden:
//! - No named lines or `grid-template-areas`.
//! - No `grid-auto-flow: column` or `dense`; placement runs along the
//!   rows and never backfills a hole it has passed.
//! - Rows beyond the explicit template are *implicit* and sized from
//!   their content, which is right, but `grid-auto-rows` is not read.
//! - An `auto` track takes its max-content width, with no shrinking
//!   when the row overflows.

use crate::engine::cryo_impl::css::computed::AlignItems;
use crate::engine::cryo_impl::css::value::Track;

use super::block::{self, ContainingBlock};
use super::box_tree::{BoxId, BoxTree};
use super::geometry::{Fragment, Point};
use super::measure::TextMeasure;

/// Where one item sits: a half-open range of columns and of rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Area {
    columns: (usize, usize),
    rows: (usize, usize),
}

/// Lay out a grid container's children.
///
/// Returns the content height and the children's fragments, matching
/// what [`super::flex::layout`] returns so block layout can call
/// either without caring which.
pub fn layout(
    tree: &BoxTree,
    container: BoxId,
    inner: &ContainingBlock,
    content_origin: Point,
    metrics: &dyn TextMeasure,
) -> (f32, Vec<Fragment>) {
    let style = &tree.get(container).style;
    let items: Vec<BoxId> = tree
        .in_flow_children(container)
        .into_iter()
        .filter(|&child| tree.get(child).style.display.generates_box())
        .collect();
    if items.is_empty() {
        return (0.0, Vec::new());
    }

    // A grid with no template is one column wide, which is what makes
    // `display: grid` on its own stack its children.
    let columns = if style.grid_template_columns.is_empty() {
        vec![Track::Auto]
    } else {
        style.grid_template_columns.clone()
    };

    let areas = place(tree, &items, columns.len());
    let row_count = areas.iter().map(|area| area.rows.1).max().unwrap_or(0);

    let column_gap = block::resolve(style.column_gap, inner.width);
    let row_gap = block::resolve(style.row_gap, inner.width);

    let widths = size_columns(
        tree,
        &columns,
        &areas,
        &items,
        inner.width,
        column_gap,
        metrics,
    );
    let offsets = offsets_of(&widths, column_gap);

    // Each item's height, now that its width is known — which is the
    // only order that works: a paragraph's height depends on how wide
    // it was allowed to be.
    let heights = measure_rows(tree, &items, &areas, &widths, &offsets, row_count, metrics);
    let row_offsets = offsets_of(&heights, row_gap);

    let mut fragments = Vec::with_capacity(items.len());
    for (item, area) in items.iter().zip(&areas) {
        let x = content_origin.x + offsets[area.columns.0];
        let y = content_origin.y + row_offsets[area.rows.0];
        let width = span_size(&widths, area.columns, column_gap);
        let height = span_size(&heights, area.rows, row_gap);

        let cell = ContainingBlock {
            width,
            // A grid cell is a definite box, so an item that stretches
            // fills it — which is the default and why most grid items
            // need no height of their own.
            height: Some(height),
        };
        let stretch = style.align_items == AlignItems::Stretch;
        fragments.push(block::layout_box_with_height(
            tree,
            *item,
            &cell,
            Point { x, y },
            stretch.then_some(height),
            metrics,
        ));
    }

    let total = row_offsets.last().copied().unwrap_or(0.0) + heights.last().copied().unwrap_or(0.0);
    (total, fragments)
}

/// Give every item a column and row range.
///
/// An item that names its own lines takes them; the rest go into the
/// next free cell, walking along each row in turn. The cursor never
/// goes back, which is what `grid-auto-flow: dense` would change.
fn place(tree: &BoxTree, items: &[BoxId], columns: usize) -> Vec<Area> {
    let mut taken: Vec<Vec<bool>> = Vec::new();
    let mut areas = Vec::with_capacity(items.len());
    let mut cursor = (0usize, 0usize);

    // Mark a range as used, growing the map as rows are reached.
    let occupy = |taken: &mut Vec<Vec<bool>>, area: Area| {
        while taken.len() < area.rows.1 {
            taken.push(vec![false; columns]);
        }
        let last = area.columns.1.min(columns);
        for row in &mut taken[area.rows.0..area.rows.1] {
            for cell in &mut row[area.columns.0..last] {
                *cell = true;
            }
        }
    };

    for &item in items {
        let style = &tree.get(item).style;
        let column_span = style.grid_column.span().min(columns.max(1));
        let explicit_columns = style.grid_column.resolve(columns);
        let explicit_rows = style.grid_row.resolve(usize::MAX);

        let area = match (explicit_columns, explicit_rows) {
            // Both named: the item goes exactly there.
            (Some(columns_at), Some(rows_at)) => Area {
                columns: columns_at,
                rows: rows_at,
            },
            // Only the column: the next row with room in it.
            (Some(columns_at), None) => {
                let row = free_row(&taken, columns_at, columns);
                Area {
                    columns: columns_at,
                    rows: (row, row + 1),
                }
            }
            // Otherwise, the next free run of cells.
            _ => {
                let (row, column) = next_free(&taken, cursor, column_span, columns);
                cursor = (row, column + column_span);
                Area {
                    columns: (column, column + column_span),
                    rows: (row, row + 1),
                }
            }
        };
        occupy(&mut taken, area);
        areas.push(area);
    }
    areas
}

/// The first row where `columns` is free.
fn free_row(taken: &[Vec<bool>], range: (usize, usize), columns: usize) -> usize {
    for (index, row) in taken.iter().enumerate() {
        if (range.0..range.1.min(columns)).all(|column| !row[column]) {
            return index;
        }
    }
    taken.len()
}

/// The next run of `span` free cells at or after `cursor`.
fn next_free(
    taken: &[Vec<bool>],
    cursor: (usize, usize),
    span: usize,
    columns: usize,
) -> (usize, usize) {
    let (mut row, mut column) = cursor;
    loop {
        if column + span > columns {
            row += 1;
            column = 0;
            continue;
        }
        let occupied = taken
            .get(row)
            .is_some_and(|cells| (column..column + span).any(|at| cells[at]));
        if !occupied {
            return (row, column);
        }
        column += 1;
    }
}

/// Work out each column's width.
///
/// Fixed tracks first, then `auto` ones from their content, and only
/// then the fractions — see the module note on why that order is
/// forced.
fn size_columns(
    tree: &BoxTree,
    columns: &[Track],
    areas: &[Area],
    items: &[BoxId],
    available: f32,
    gap: f32,
    metrics: &dyn TextMeasure,
) -> Vec<f32> {
    let mut widths = vec![0.0f32; columns.len()];
    let mut flexible = Vec::new();

    for (index, track) in columns.iter().enumerate() {
        match track.definite(available) {
            Some(width) => widths[index] = width,
            None => {
                widths[index] = track.minimum(available);
                if track.fraction().is_some() {
                    flexible.push(index);
                } else {
                    // An `auto` track is as wide as the widest item
                    // that sits only in it. An item spanning several
                    // tracks says nothing about any one of them.
                    let wanted = items
                        .iter()
                        .zip(areas)
                        .filter(|(_, area)| area.columns == (index, index + 1))
                        .map(|(&item, _)| super::intrinsic::max_content_width(tree, item, metrics))
                        .fold(0.0f32, f32::max);
                    widths[index] = widths[index].max(wanted);
                }
            }
        }
    }

    let gaps = gap * (columns.len().saturating_sub(1)) as f32;
    let used: f32 = widths.iter().sum::<f32>() + gaps;
    let free = (available - used).max(0.0);
    let shares: f32 = flexible
        .iter()
        .filter_map(|&index| columns[index].fraction())
        .sum();
    if shares > 0.0 {
        for &index in &flexible {
            let share = columns[index].fraction().unwrap_or(0.0);
            widths[index] += free * share / shares;
        }
        return widths;
    }

    // With no fraction to take it, leftover space goes to the `auto`
    // tracks — they are the only ones `justify-content`'s default
    // stretch may grow. Without this a grid with no template at all
    // is as wide as its content instead of as wide as its container.
    let stretchy: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter(|(_, track)| matches!(track, Track::Auto))
        .map(|(index, _)| index)
        .collect();
    if !stretchy.is_empty() && free > 0.0 {
        let each = free / stretchy.len() as f32;
        for index in stretchy {
            widths[index] += each;
        }
    }
    widths
}

/// Each row's height: the tallest item that sits only in it.
#[allow(clippy::too_many_arguments)] // every input decides a row's size
fn measure_rows(
    tree: &BoxTree,
    items: &[BoxId],
    areas: &[Area],
    widths: &[f32],
    offsets: &[f32],
    rows: usize,
    metrics: &dyn TextMeasure,
) -> Vec<f32> {
    let mut heights = vec![0.0f32; rows];
    for (&item, area) in items.iter().zip(areas) {
        // Only an item confined to one row says how tall that row is.
        if area.rows.1 - area.rows.0 != 1 {
            continue;
        }
        let width = span_size(widths, area.columns, 0.0)
            + offsets
                .get(area.columns.1.saturating_sub(1))
                .copied()
                .unwrap_or(0.0)
            - offsets.get(area.columns.0).copied().unwrap_or(0.0);
        let cell = ContainingBlock {
            width: width.max(0.0),
            height: None,
        };
        let probe = block::layout_box(tree, item, &cell, Point { x: 0.0, y: 0.0 }, metrics);
        let wanted = probe.margin_box().height;
        if let Some(height) = heights.get_mut(area.rows.0) {
            *height = height.max(wanted);
        }
    }
    heights
}

/// Where each track starts, gaps included.
fn offsets_of(sizes: &[f32], gap: f32) -> Vec<f32> {
    let mut offsets = Vec::with_capacity(sizes.len());
    let mut at = 0.0;
    for size in sizes {
        offsets.push(at);
        at += size + gap;
    }
    offsets
}

/// How big a run of tracks is, including the gaps inside it.
fn span_size(sizes: &[f32], range: (usize, usize), gap: f32) -> f32 {
    let covered: f32 = sizes
        .get(range.0..range.1.min(sizes.len()))
        .unwrap_or(&[])
        .iter()
        .sum();
    let count = range.1.min(sizes.len()).saturating_sub(range.0);
    covered + gap * (count.saturating_sub(1)) as f32
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
    use crate::engine::cryo_impl::layout::geometry::Rect;
    use crate::engine::cryo_impl::layout::measure::RatioMetrics;

    const UA: &str = "html, body, div, p { display: block } head { display: none }";

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
        let tree = BoxTree::build(&doc, &styles);
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

    const THREE: &str = "<div id=g><div id=a></div><div id=b></div><div id=c></div></div>";

    #[test]
    fn equal_fractions_divide_the_width() {
        let laid = boxes(
            THREE,
            "#g { display: grid; width: 300px; grid-template-columns: repeat(3, 1fr) }
             #a, #b, #c { height: 10px }",
        );
        for id in ["a", "b", "c"] {
            assert!(close(rect_of(&laid, id).width, 100.0), "{id}");
        }
        assert!(close(rect_of(&laid, "a").x, 0.0));
        assert!(close(rect_of(&laid, "b").x, 100.0));
        assert!(close(rect_of(&laid, "c").x, 200.0));
    }

    #[test]
    fn a_fixed_track_keeps_its_width_and_the_rest_flexes() {
        let laid = boxes(
            THREE,
            "#g { display: grid; width: 300px; grid-template-columns: 100px 1fr 1fr }
             #a, #b, #c { height: 10px }",
        );
        assert!(close(rect_of(&laid, "a").width, 100.0));
        assert!(close(rect_of(&laid, "b").width, 100.0));
        assert!(close(rect_of(&laid, "c").width, 100.0));
    }

    #[test]
    fn a_gap_comes_out_of_the_flexible_tracks() {
        let laid = boxes(
            THREE,
            "#g { display: grid; width: 320px; gap: 10px;
                  grid-template-columns: repeat(3, 1fr) }
             #a, #b, #c { height: 10px }",
        );
        // 320 less two 10px gaps is 300, three ways.
        assert!(close(rect_of(&laid, "a").width, 100.0));
        assert!(close(rect_of(&laid, "b").x, 110.0));
        assert!(close(rect_of(&laid, "c").x, 220.0));
    }

    #[test]
    fn items_wrap_onto_the_next_row_when_the_columns_run_out() {
        let laid = boxes(
            "<div id=g><div id=a></div><div id=b></div><div id=c></div><div id=d></div></div>",
            "#g { display: grid; width: 200px; grid-template-columns: repeat(2, 1fr) }
             #a, #b, #c, #d { height: 20px }",
        );
        assert!(
            close(rect_of(&laid, "a").y, rect_of(&laid, "b").y),
            "first row"
        );
        assert!(
            close(rect_of(&laid, "c").y, rect_of(&laid, "d").y),
            "second row"
        );
        assert!(rect_of(&laid, "c").y > rect_of(&laid, "a").y);
        assert!(close(rect_of(&laid, "c").x, 0.0), "back to the left");
    }

    #[test]
    fn an_item_may_name_its_own_columns() {
        let laid = boxes(
            THREE,
            "#g { display: grid; width: 300px; grid-template-columns: repeat(3, 1fr) }
             #a { grid-column: 2 / 4 }
             #a, #b, #c { height: 10px }",
        );
        let a = rect_of(&laid, "a");
        assert!(close(a.x, 100.0), "starts at the second line");
        assert!(close(a.width, 200.0), "two tracks wide");
    }

    #[test]
    fn a_full_width_item_spans_every_column() {
        let laid = boxes(
            THREE,
            "#g { display: grid; width: 300px; grid-template-columns: repeat(3, 1fr) }
             #a { grid-column: 1 / -1 }
             #a, #b, #c { height: 10px }",
        );
        assert!(close(rect_of(&laid, "a").width, 300.0));
    }

    #[test]
    fn a_span_takes_that_many_tracks_wherever_it_lands() {
        let laid = boxes(
            THREE,
            "#g { display: grid; width: 300px; grid-template-columns: repeat(3, 1fr) }
             #b { grid-column: span 2 }
             #a, #b, #c { height: 10px }",
        );
        assert!(close(rect_of(&laid, "b").width, 200.0));
    }

    #[test]
    fn a_row_is_as_tall_as_its_tallest_item() {
        let laid = boxes(
            "<div id=g><div id=a></div><div id=b></div></div>",
            "#g { display: grid; width: 200px; grid-template-columns: repeat(2, 1fr) }
             #a { height: 30px } #b { height: 50px }",
        );
        assert!(close(rect_of(&laid, "g").height, 50.0));
    }

    #[test]
    fn a_grid_with_no_template_is_one_column() {
        let laid = boxes(
            "<div id=g><div id=a></div><div id=b></div></div>",
            "#g { display: grid; width: 200px }
             #a, #b { height: 10px }",
        );
        assert!(close(rect_of(&laid, "a").width, 200.0));
        assert!(close(rect_of(&laid, "b").y, 10.0), "stacked, not beside");
    }

    #[test]
    fn minmax_takes_its_floor_before_it_flexes() {
        let laid = boxes(
            THREE,
            "#g { display: grid; width: 300px;
                  grid-template-columns: minmax(200px, 1fr) 1fr 1fr }
             #a, #b, #c { height: 10px }",
        );
        // The floor is 200, leaving 100 to share three ways — so the
        // first track is 200 plus its own third.
        let a = rect_of(&laid, "a");
        assert!(a.width >= 200.0, "its floor at least, got {}", a.width);
    }

    #[test]
    fn an_item_stretches_to_fill_its_cell() {
        let laid = boxes(
            "<div id=g><div id=a></div><div id=b></div></div>",
            "#g { display: grid; width: 200px; grid-template-columns: repeat(2, 1fr) }
             #a { height: 40px }",
        );
        // `align-items: stretch` is the default, so the shorter item
        // takes the row's height.
        assert!(close(rect_of(&laid, "b").height, 40.0));
    }

    #[test]
    fn an_empty_grid_lays_nothing_out() {
        let laid = boxes("<div id=g></div>", "#g { display: grid; width: 200px }");
        assert!(close(rect_of(&laid, "g").height, 0.0));
    }
}
