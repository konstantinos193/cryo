//! Inline layout: inline-level boxes become lines.
//!
//! Three jobs, in order, because each depends on the one before:
//!
//! 1. **Flatten to items.** The inline subtree becomes a flat list of
//!    words, spaces, breaks, atomic boxes and the edges of inline boxes
//!    that paint — see [`item`].
//! 2. **Break into lines.** Greedy: fit what fits, break before the word
//!    that does not. Whitespace at a break point disappears, which is
//!    why a space is its own item and not part of a word.
//! 3. **Align on the baseline.** A line's height is not the tallest
//!    item's height — it is the deepest ascent plus the deepest descent,
//!    measured from a shared baseline. On a line mixing 12px and 30px
//!    text those differ, and using the maximum height would let the big
//!    text's descender sit on the small text.
//!
//! Every line also has a **strut**: an invisible item in the container's
//! own font, so an empty line still has height and a line of small text
//! in a large-font paragraph is not shorter than the paragraph's own
//! line height.
//!
//! An inline box that paints gets one fragment *per line it appears on*
//! — a `<span>` wrapped over three lines is three rectangles, not one,
//! and only the first carries its left border and the last its right.
//! That is why the open-box stack in [`position`] outlives the line it
//! started on.
//!
//! Known gaps, stated rather than hidden:
//! - An inline box's *margins* are ignored: it spends its border and
//!   padding on the line, but not its margin.
//! - An inline box's vertical padding paints without widening the line,
//!   which is correct per spec and means a tall background overlaps its
//!   neighbours. There is no overflow clipping to stop it.
//! - `text-align: justify` lays out as `start`; stretching a line needs
//!   the inter-word spaces to be adjustable, which is the next step.

mod item;

use crate::engine::cryo_impl::css::computed::{ComputedStyle, Sides, TextAlign};

use super::box_tree::{BoxId, BoxTree};
use super::geometry::{Fragment, Point, Rect};
use super::measure::TextMeasure;
use item::{collect, Item, Side};

/// One line of inline content.
#[derive(Clone, Debug, PartialEq)]
pub struct LineBox {
    pub fragments: Vec<Fragment>,
    /// The line's own box: full available width, its computed height.
    pub rect: Rect,
    /// Where the shared baseline sits, from the line's top.
    pub baseline: f32,
}

/// The result of laying inline content out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InlineFlow {
    pub lines: Vec<LineBox>,
}

impl InlineFlow {
    /// The total height of every line.
    pub fn height(&self) -> f32 {
        self.lines.iter().map(|line| line.rect.height).sum()
    }

    /// Every fragment from every line.
    pub fn fragments(&self) -> Vec<Fragment> {
        self.lines
            .iter()
            .flat_map(|line| line.fragments.iter().cloned())
            .collect()
    }
}

/// Lay out `children` as inline content in a box `available_width` wide,
/// starting at `origin`.
///
/// `container` is the block container's style, which supplies the strut
/// and the `text-align`.
pub fn layout(
    tree: &BoxTree,
    children: &[BoxId],
    container: &ComputedStyle,
    origin: Point,
    available_width: f32,
    metrics: &dyn TextMeasure,
) -> InlineFlow {
    let mut items = Vec::new();
    for &child in children {
        collect(tree, child, available_width, metrics, &mut items);
    }
    let runs = break_lines(&items, available_width);
    position(&runs, container, origin, available_width, metrics)
}

/// Greedily assign items to lines.
///
/// Returns each line's items. A space pending at a break is dropped,
/// which is what makes a wrapped line not start with a gap.
fn break_lines(items: &[Item], available_width: f32) -> Vec<Vec<Item>> {
    let mut lines: Vec<Vec<Item>> = Vec::new();
    let mut current: Vec<Item> = Vec::new();
    let mut used = 0.0;
    // A space is only placed once something follows it on the same
    // line; at a break it vanishes instead.
    let mut pending_space: Option<Item> = None;
    // An inline box's opening edge belongs to whatever comes next, so
    // it waits with it. Placing it eagerly would strand a `<span>`'s
    // left border at the end of a line with its first word on the
    // next one.
    let mut pending_open: Vec<Item> = Vec::new();

    for item in items {
        match item {
            Item::Break { .. } => {
                // The space before a break goes; a box that opened
                // before it is still open and starts the next line.
                place(&mut current, &mut used, None, &mut pending_open);
                lines.push(std::mem::take(&mut current));
                used = 0.0;
                pending_space = None;
            }
            Item::Space { .. } => {
                // A space at the start of a line is not rendered.
                if !current.is_empty() {
                    pending_space = Some(item.clone());
                }
            }
            Item::Edge {
                side: Side::Open, ..
            } => pending_open.push(item.clone()),
            // A closing edge follows content already on the line, so
            // it never moves — but it does flush an empty box's own
            // opening edge, which is all `<span></span>` consists of.
            Item::Edge {
                side: Side::Close, ..
            } => {
                let space = pending_space.take();
                place(&mut current, &mut used, space, &mut pending_open);
                used += item.width();
                current.push(item.clone());
            }
            _ => {
                let held: f32 = pending_space.as_ref().map_or(0.0, Item::width)
                    + pending_open.iter().map(Item::width).sum::<f32>();
                let wraps = item.style().white_space.wraps();
                if wraps && !current.is_empty() && used + held + item.width() > available_width {
                    lines.push(std::mem::take(&mut current));
                    used = 0.0;
                    pending_space = None;
                }
                let space = pending_space.take();
                place(&mut current, &mut used, space, &mut pending_open);
                used += item.width();
                current.push(item.clone());
            }
        }
    }
    // An inline box that opened and never got any content still paints.
    place(&mut current, &mut used, None, &mut pending_open);
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Put everything held back onto the line, in order.
fn place(current: &mut Vec<Item>, used: &mut f32, space: Option<Item>, opens: &mut Vec<Item>) {
    for held in space.into_iter().chain(opens.drain(..)) {
        *used += held.width();
        current.push(held);
    }
}

/// An inline box whose start has been placed but whose end has not.
///
/// It may outlive the line it opened on: a wrapped `<span>` resumes at
/// the next line's start, and `opens_here` is what stops it paying for
/// its left border a second time.
struct OpenBox {
    box_id: BoxId,
    style: ComputedStyle,
    padding: Sides<f32>,
    /// How deeply nested this box is, so the outer of two overlapping
    /// backgrounds paints first.
    depth: usize,
    /// Where the box's outer left edge sits on the current line.
    start: f32,
    /// Whether this line is the one the box opened on.
    opens_here: bool,
}

impl OpenBox {
    /// This box's fragment on the line it is currently on.
    ///
    /// `end` is where the box's outer right edge falls, and
    /// `closes_here` says whether that is its real end or just where
    /// the line ran out.
    fn fragment(
        &self,
        end: f32,
        line_top: f32,
        line_baseline: f32,
        closes_here: bool,
        metrics: &dyn TextMeasure,
    ) -> Fragment {
        // Only the sides that fall on this line are drawn: a box
        // continued from the line above has no left border there.
        let border = Sides {
            top: self.style.border_width.top,
            right: if closes_here {
                self.style.border_width.right
            } else {
                0.0
            },
            bottom: self.style.border_width.bottom,
            left: if self.opens_here {
                self.style.border_width.left
            } else {
                0.0
            },
        };
        let padding = Sides {
            top: self.padding.top,
            right: if closes_here { self.padding.right } else { 0.0 },
            bottom: self.padding.bottom,
            left: if self.opens_here {
                self.padding.left
            } else {
                0.0
            },
        };

        // An inline box's content box is its own line-height box, sat
        // on the line's shared baseline — not the line's full height,
        // which belongs to whatever item on it is tallest.
        let above = metrics.baseline(&self.style);
        let content = Rect::new(
            self.start + border.left + padding.left,
            line_top + line_baseline - above,
            (end - self.start - border.horizontal() - padding.horizontal()).max(0.0),
            metrics.line_height(&self.style),
        );
        Fragment {
            padding,
            border,
            ..Fragment::bare(self.box_id, content)
        }
    }
}

/// Turn lines of items into positioned fragments.
fn position(
    runs: &[Vec<Item>],
    container: &ComputedStyle,
    origin: Point,
    available_width: f32,
    metrics: &dyn TextMeasure,
) -> InlineFlow {
    // The strut: the container's own font, present on every line.
    let strut_above = metrics.baseline(container);
    let strut_below = metrics.line_height(container) - strut_above;

    let mut lines = Vec::new();
    let mut y = origin.y;
    // An inline box open at a line's end continues onto the next one,
    // so the stack outlives any one line.
    let mut open: Vec<OpenBox> = Vec::new();

    for items in runs {
        let (mut above, mut below) = (strut_above, strut_below);
        for item in items {
            let (item_above, item_below) = item.extents(metrics);
            above = above.max(item_above);
            below = below.max(item_below);
        }
        let height = above + below;
        let used_width: f32 = items.iter().map(Item::width).sum();
        let line_start = origin.x + align_offset(container.text_align, used_width, available_width);
        let mut x = line_start;

        // A box carried over from the line above resumes here, having
        // already spent its left border and padding up there.
        for carried in &mut open {
            carried.start = line_start;
            carried.opens_here = false;
        }

        let mut content = Vec::new();
        // (depth, fragment): filled in closing order, which is
        // innermost first, so it is sorted before it is used.
        let mut decorations: Vec<(usize, Fragment)> = Vec::new();

        for item in items {
            match item {
                Item::Edge {
                    box_id,
                    side: Side::Open,
                    padding,
                    style,
                } => {
                    open.push(OpenBox {
                        box_id: *box_id,
                        style: style.clone(),
                        padding: *padding,
                        depth: open.len(),
                        start: x,
                        opens_here: true,
                    });
                    x += item.width();
                }
                Item::Edge {
                    box_id,
                    side: Side::Close,
                    ..
                } => {
                    x += item.width();
                    // The innermost box with this id: the same element
                    // cannot nest inside itself, so there is only one.
                    if let Some(index) = open.iter().rposition(|box_| box_.box_id == *box_id) {
                        let closed = open.remove(index);
                        decorations
                            .push((closed.depth, closed.fragment(x, y, above, true, metrics)));
                    }
                }
                _ => {
                    let (item_above, _) = item.extents(metrics);
                    let item_height = match item {
                        Item::Atomic { height, .. } => *height,
                        other => metrics.line_height(other.style()),
                    };
                    // Sit each item so its own baseline meets the line's.
                    let top = y + above - item_above;
                    let rect = Rect::new(x, top, item.width(), item_height);
                    content.push(fragment_for(item, rect));
                    x += item.width();
                }
            }
        }
        // A box still open here runs to the end of what this line used.
        for carried in &open {
            decorations.push((carried.depth, carried.fragment(x, y, above, false, metrics)));
        }

        // Outermost first, and all of them before the content: a
        // line's fragments paint in order, and an inline box's
        // background belongs behind its own text.
        decorations.sort_by_key(|(depth, _)| *depth);
        let mut fragments: Vec<Fragment> = decorations
            .into_iter()
            .map(|(_, fragment)| fragment)
            .collect();
        fragments.extend(content);

        lines.push(LineBox {
            fragments,
            rect: Rect::new(origin.x, y, available_width, height),
            baseline: above,
        });
        y += height;
    }
    InlineFlow { lines }
}

/// How far to shift a line to satisfy `text-align`.
///
/// `justify` is treated as `start`: stretching a line means widening its
/// spaces, which the item list does not yet allow.
fn align_offset(align: TextAlign, used: f32, available: f32) -> f32 {
    let slack = (available - used).max(0.0);
    match align {
        TextAlign::Start | TextAlign::Left | TextAlign::Justify => 0.0,
        TextAlign::End | TextAlign::Right => slack,
        TextAlign::Center => slack / 2.0,
    }
}

fn fragment_for(item: &Item, rect: Rect) -> Fragment {
    match item {
        Item::Word { text, box_id, .. } | Item::Space { text, box_id, .. } => Fragment {
            text: Some(text.clone()),
            ..Fragment::bare(*box_id, rect)
        },
        // Already laid out at the origin; move it onto the line.
        Item::Atomic { fragment, .. } => {
            let mut placed = (**fragment).clone();
            let from = placed.margin_box();
            placed.translate(rect.x - from.x, rect.y - from.y);
            placed
        }
        // Breaks and edges never reach here; `position` handles them.
        Item::Break { .. } | Item::Edge { .. } => Fragment::bare(BoxId::default(), rect),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::cascade::{Origin, RuleSet};
    use crate::engine::cryo_impl::css::computed::{ComputeContext, LineHeight};
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::css::style_tree::StyleTree;
    use crate::engine::cryo_impl::html::parse as parse_html;
    use crate::engine::cryo_impl::layout::measure::RatioMetrics;

    /// Blocks for structure, so the `<p>` is a block container.
    const BLOCKS: &str = concat!(
        "html, body, div, p { display: block }",
        "head { display: none }",
    );

    /// Lay the first `<p>`'s inline content out in `width` pixels.
    fn flow(markup: &str, css: &str, width: f32) -> InlineFlow {
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

        let root = tree.root().expect("a root box");
        let paragraph = tree
            .descendants(root)
            .into_iter()
            .find(|&id| {
                tree.get(id)
                    .node
                    .is_some_and(|node| doc.is_html_element(node, "p"))
            })
            .expect("a <p> box");
        let container = tree.get(paragraph).style.clone();
        layout(
            &tree,
            tree.children(paragraph),
            &container,
            Point { x: 0.0, y: 0.0 },
            width,
            &RatioMetrics::DEFAULT,
        )
    }

    /// The text of each line, joined.
    fn line_texts(flow: &InlineFlow) -> Vec<String> {
        flow.lines
            .iter()
            .map(|line| {
                line.fragments
                    .iter()
                    .filter_map(|fragment| fragment.text.clone())
                    .collect()
            })
            .collect()
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    #[test]
    fn text_that_fits_makes_one_line() {
        // 16px font, ratio 0.5 => 8px a character. "hello" is 40px.
        let laid_out = flow("<p>hello</p>", "", 200.0);
        assert_eq!(line_texts(&laid_out), ["hello"]);
        assert!(close(laid_out.height(), 19.2), "one line of 16px/normal");
    }

    #[test]
    fn empty_inline_content_makes_no_lines() {
        let laid_out = flow("<p></p>", "", 200.0);
        assert!(laid_out.lines.is_empty());
        assert!(close(laid_out.height(), 0.0));
    }

    #[test]
    fn text_breaks_at_a_space_when_it_runs_out_of_room() {
        // Each word is 24px; 60px fits two words but not three.
        let laid_out = flow("<p>aaa bbb ccc</p>", "p { font-size: 16px }", 60.0);
        assert_eq!(line_texts(&laid_out), ["aaa bbb", "ccc"]);
    }

    #[test]
    fn a_wrapped_line_does_not_start_with_the_space_it_broke_at() {
        let laid_out = flow("<p>aaa bbb</p>", "", 30.0);
        let lines = line_texts(&laid_out);
        assert_eq!(lines, ["aaa", "bbb"]);
        assert!(!lines[1].starts_with(' '));
    }

    #[test]
    fn a_word_too_long_for_the_line_still_gets_its_own_line() {
        // Nothing to break inside a word, so it overflows rather than
        // vanishing.
        let laid_out = flow("<p>aa enormouslylongword</p>", "", 40.0);
        assert_eq!(line_texts(&laid_out), ["aa", "enormouslylongword"]);
    }

    #[test]
    fn nowrap_keeps_everything_on_one_line() {
        let laid_out = flow("<p>aaa bbb ccc ddd</p>", "p { white-space: nowrap }", 20.0);
        assert_eq!(line_texts(&laid_out).len(), 1);
    }

    #[test]
    fn a_br_forces_a_break_regardless_of_room() {
        let laid_out = flow("<p>a<br>b</p>", "", 500.0);
        assert_eq!(line_texts(&laid_out), ["a", "b"]);
    }

    #[test]
    fn consecutive_spaces_collapse_to_one() {
        let laid_out = flow("<p>a     b</p>", "", 500.0);
        assert_eq!(line_texts(&laid_out), ["a b"]);
    }

    #[test]
    fn a_newline_is_just_a_space_when_white_space_collapses() {
        let laid_out = flow("<p>a\nb</p>", "", 500.0);
        assert_eq!(line_texts(&laid_out), ["a b"]);
    }

    #[test]
    fn pre_preserves_newlines_as_breaks_and_keeps_every_space() {
        let laid_out = flow("<p>a\nb  c</p>", "p { white-space: pre }", 500.0);
        assert_eq!(line_texts(&laid_out), ["a", "b  c"]);
    }

    #[test]
    fn pre_line_keeps_newlines_but_collapses_spaces() {
        let laid_out = flow("<p>a\nb  c</p>", "p { white-space: pre-line }", 500.0);
        assert_eq!(line_texts(&laid_out), ["a", "b c"]);
    }

    #[test]
    fn nested_inline_elements_do_not_change_where_lines_break() {
        let plain = line_texts(&flow("<p>aaa bbb ccc</p>", "", 60.0));
        let nested = line_texts(&flow("<p>aaa <span>bbb</span> ccc</p>", "", 60.0));
        assert_eq!(plain, nested);
    }

    #[test]
    fn fragments_advance_along_the_line() {
        let laid_out = flow("<p>ab cd</p>", "", 500.0);
        let line = &laid_out.lines[0];
        let xs: Vec<f32> = line.fragments.iter().map(|f| f.content.x).collect();
        // "ab" at 0 (16px wide), " " at 16 (8px), "cd" at 24.
        assert!(close(xs[0], 0.0));
        assert!(close(xs[1], 16.0));
        assert!(close(xs[2], 24.0));
    }

    #[test]
    fn lines_stack_downward_from_the_origin() {
        let laid_out = flow("<p>aaa bbb</p>", "", 30.0);
        assert!(close(laid_out.lines[0].rect.y, 0.0));
        assert!(close(
            laid_out.lines[1].rect.y,
            laid_out.lines[0].rect.height
        ));
    }

    #[test]
    fn a_lines_height_comes_from_the_deepest_ascent_plus_descent() {
        // Not the tallest item's height: mixing sizes must not let the
        // larger text's descender overlap the smaller.
        let laid_out = flow(
            "<p>small <span>BIG</span></p>",
            "p { font-size: 10px } span { font-size: 40px }",
            500.0,
        );
        let line = &laid_out.lines[0];
        let big_line_height = 40.0 * 1.2;
        assert!(
            line.rect.height >= big_line_height,
            "height {} should cover the 40px text",
            line.rect.height
        );
    }

    #[test]
    fn items_share_one_baseline() {
        let laid_out = flow(
            "<p>small <span>BIG</span></p>",
            "p { font-size: 10px } span { font-size: 40px }",
            500.0,
        );
        let line = &laid_out.lines[0];
        let metrics = RatioMetrics::DEFAULT;
        // Each fragment's own baseline must land on the line's shared
        // one — that is what "aligned" means, and it is why the small
        // text is pushed down rather than sitting at the line's top.
        for fragment in &line.fragments {
            let style = ComputedStyle {
                font_size: fragment.content.height / 1.2,
                ..ComputedStyle::INITIAL
            };
            let offset = fragment.content.y - line.rect.y;
            assert!(
                close(offset + metrics.baseline(&style), line.baseline),
                "fragment at +{offset} does not meet the baseline {}",
                line.baseline
            );
        }
    }

    #[test]
    fn the_strut_gives_an_empty_line_the_containers_height() {
        // `<br>` at the start leaves a line with no items on it.
        let laid_out = flow("<p><br>x</p>", "p { font-size: 20px }", 500.0);
        assert_eq!(laid_out.lines.len(), 2);
        assert!(close(laid_out.lines[0].rect.height, 24.0), "20px * 1.2");
    }

    #[test]
    fn the_strut_keeps_a_line_of_small_text_at_the_container_height() {
        let laid_out = flow(
            "<p>x</p>",
            "p { font-size: 40px; line-height: 2 } span { font-size: 5px }",
            500.0,
        );
        assert!(close(laid_out.lines[0].rect.height, 80.0));
    }

    #[test]
    fn line_height_changes_the_line_box_not_the_text() {
        let tight = flow("<p>x</p>", "p { font-size: 10px; line-height: 1 }", 500.0);
        let loose = flow("<p>x</p>", "p { font-size: 10px; line-height: 3 }", 500.0);
        assert!(close(tight.lines[0].rect.height, 10.0));
        assert!(close(loose.lines[0].rect.height, 30.0));
    }

    #[test]
    fn centring_shifts_a_line_by_half_its_slack() {
        let laid_out = flow("<p>ab</p>", "p { text-align: center }", 100.0);
        // "ab" is 16px wide in 100px, so 42px of slack each side.
        assert!(close(laid_out.lines[0].fragments[0].content.x, 42.0));
    }

    #[test]
    fn right_alignment_pushes_a_line_to_the_far_edge() {
        let laid_out = flow("<p>ab</p>", "p { text-align: right }", 100.0);
        assert!(close(laid_out.lines[0].fragments[0].content.x, 84.0));
    }

    #[test]
    fn start_alignment_leaves_a_line_at_the_origin() {
        let laid_out = flow("<p>ab</p>", "", 100.0);
        assert!(close(laid_out.lines[0].fragments[0].content.x, 0.0));
    }

    #[test]
    fn each_line_box_spans_the_full_available_width() {
        let laid_out = flow("<p>aaa bbb</p>", "", 30.0);
        for line in &laid_out.lines {
            assert!(close(line.rect.width, 30.0));
        }
    }

    #[test]
    fn an_atomic_inline_takes_its_declared_size() {
        let laid_out = flow(
            "<p>a<span>b</span></p>",
            "span { display: inline-block; width: 50px; height: 30px }",
            500.0,
        );
        let atomic = laid_out.lines[0]
            .fragments
            .iter()
            .find(|fragment| fragment.text.is_none())
            .expect("the inline-block fragment");
        assert!(close(atomic.content.width, 50.0));
        assert!(close(atomic.content.height, 30.0));
    }

    #[test]
    fn the_flow_reports_its_fragments_and_total_height() {
        let laid_out = flow("<p>aaa bbb</p>", "", 30.0);
        assert_eq!(laid_out.lines.len(), 2);
        assert_eq!(laid_out.fragments().len(), 2, "two words, no kept space");
        assert!(close(
            laid_out.height(),
            laid_out.lines.iter().map(|l| l.rect.height).sum()
        ));
    }

    #[test]
    fn the_origin_offsets_every_line() {
        let doc = parse_html("<p>x</p>");
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, BLOCKS);
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = BoxTree::build(&doc, &styles);
        let root = tree.root().expect("a root");
        let paragraph = tree
            .descendants(root)
            .into_iter()
            .find(|&id| {
                tree.get(id)
                    .node
                    .is_some_and(|node| doc.is_html_element(node, "p"))
            })
            .expect("a <p>");
        let container = tree.get(paragraph).style.clone();
        let laid_out = layout(
            &tree,
            tree.children(paragraph),
            &container,
            Point { x: 25.0, y: 40.0 },
            100.0,
            &RatioMetrics::DEFAULT,
        );
        assert!(close(laid_out.lines[0].rect.x, 25.0));
        assert!(close(laid_out.lines[0].rect.y, 40.0));
        assert!(close(laid_out.lines[0].fragments[0].content.x, 25.0));
    }

    #[test]
    fn a_line_height_of_normal_uses_the_style_approximation() {
        let laid_out = flow("<p>x</p>", "p { font-size: 10px }", 500.0);
        assert_eq!(
            ComputedStyle {
                font_size: 10.0,
                line_height: LineHeight::Normal,
                ..ComputedStyle::INITIAL
            }
            .line_box_height(),
            laid_out.lines[0].rect.height
        );
    }

    /// The fragments on a line that draw a box rather than text.
    fn boxes(line: &LineBox) -> Vec<&Fragment> {
        line.fragments
            .iter()
            .filter(|fragment| fragment.text.is_none())
            .collect()
    }

    #[test]
    fn a_plain_inline_box_gets_no_fragment_of_its_own() {
        // Nothing to draw, so nothing to draw it into — otherwise every
        // `<a>` and `<em>` on a page would be an empty rectangle.
        let laid_out = flow("<p><span>hi</span></p>", "", 500.0);
        assert!(boxes(&laid_out.lines[0]).is_empty());
        assert_eq!(laid_out.lines[0].fragments.len(), 1);
    }

    #[test]
    fn an_inline_box_with_a_background_gets_a_fragment() {
        let laid_out = flow("<p><span>hi</span></p>", "span { background: red }", 500.0);
        let drawn = boxes(&laid_out.lines[0]);
        assert_eq!(drawn.len(), 1, "one fragment for the span");
        assert!(close(drawn[0].content.x, 0.0));
        assert!(close(drawn[0].content.width, 16.0), "as wide as \"hi\"");
    }

    #[test]
    fn an_inline_boxs_background_comes_before_its_text() {
        // A line's fragments paint in order, so the background has to
        // be first or it covers the text it belongs to.
        let laid_out = flow("<p><span>hi</span></p>", "span { background: red }", 500.0);
        let fragments = &laid_out.lines[0].fragments;
        assert!(fragments[0].text.is_none(), "the span");
        assert_eq!(fragments[1].text.as_deref(), Some("hi"));
    }

    #[test]
    fn an_inline_boxs_padding_pushes_the_text_along() {
        let laid_out = flow(
            "<p><span>hi</span></p>",
            "span { background: red; padding: 0 5px }",
            500.0,
        );
        let line = &laid_out.lines[0];
        let drawn = boxes(line);
        assert!(close(drawn[0].content.x, 5.0), "past the left padding");
        assert!(close(drawn[0].content.width, 16.0));
        assert!(close(drawn[0].padding.left, 5.0));
        assert!(close(drawn[0].padding.right, 5.0));
        let text = line
            .fragments
            .iter()
            .find(|fragment| fragment.text.is_some())
            .expect("the text");
        assert!(close(text.content.x, 5.0), "the padding moved the text");
    }

    #[test]
    fn an_inline_box_gets_one_fragment_for_every_line_it_spans() {
        // 8px a character: "aaa" and "bbb" do not share a 30px line.
        let laid_out = flow(
            "<p><span>aaa bbb</span></p>",
            "span { background: red; border: 2px solid blue }",
            30.0,
        );
        assert_eq!(laid_out.lines.len(), 2);
        let first = boxes(&laid_out.lines[0]);
        let second = boxes(&laid_out.lines[1]);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        // Only the first line carries the left border, only the last
        // the right — the span is one box, cut across two lines.
        assert!(close(first[0].border.left, 2.0));
        assert!(close(first[0].border.right, 0.0));
        assert!(close(second[0].border.left, 0.0));
        assert!(close(second[0].border.right, 2.0));
    }

    #[test]
    fn a_nested_inline_box_paints_after_the_one_around_it() {
        let laid_out = flow(
            "<p><span id=outer>a<span id=inner>b</span></span></p>",
            "#outer { background: red } #inner { background: blue }",
            500.0,
        );
        let drawn = boxes(&laid_out.lines[0]);
        assert_eq!(drawn.len(), 2);
        // The outer box spans both characters, the inner only the
        // second — so the wider one has to come first.
        assert!(close(drawn[0].content.width, 16.0), "outer: \"ab\"");
        assert!(close(drawn[1].content.width, 8.0), "inner: \"b\"");
        assert!(close(drawn[1].content.x, 8.0), "inner starts after \"a\"");
    }

    #[test]
    fn an_atomic_inlines_percentage_width_is_of_the_line() {
        // An intrinsic measurement has nothing to take a percentage
        // of, but a line does. Treating it as intrinsic is what left
        // every `width: 100%` form field a few pixels wide.
        let laid_out = flow(
            "<p><span>x</span></p>",
            "span { display: inline-block; width: 100% }",
            400.0,
        );
        let atomic = laid_out.lines[0]
            .fragments
            .iter()
            .find(|fragment| fragment.text.is_none())
            .expect("the inline-block");
        assert!(close(atomic.content.width, 400.0));
    }

    #[test]
    fn an_atomic_inline_with_a_margin_still_reports_its_own_width() {
        // The margin is real, the leftover space is not: an atomic is
        // never over-constrained, so nothing absorbs the rest of the
        // line.
        let laid_out = flow(
            "<p><span>x</span>after</p>",
            "span { display: inline-block; width: 50px; margin: 0 10px }",
            400.0,
        );
        assert_eq!(laid_out.lines.len(), 1, "both fit on one line");
        let text = laid_out.lines[0]
            .fragments
            .iter()
            .find(|fragment| fragment.text.as_deref() == Some("after"))
            .expect("the text after it");
        assert!(close(text.content.x, 70.0), "50 wide between 10 margins");
    }

    #[test]
    fn a_line_never_breaks_between_a_box_and_its_first_word() {
        // The border would otherwise be stranded at the end of a line
        // with the word it belongs to on the next one.
        let laid_out = flow(
            "<p>aaa <span>bbb</span></p>",
            "span { background: red; padding-left: 20px }",
            40.0,
        );
        assert_eq!(line_texts(&laid_out), ["aaa", "bbb"]);
        let drawn = boxes(&laid_out.lines[1]);
        assert_eq!(drawn.len(), 1);
        assert!(close(drawn[0].padding.left, 20.0));
    }
}
