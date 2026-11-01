//! Intrinsic widths: how wide a box wants to be, independent of what it
//! is given.
//!
//! Normal flow never needs this — a block takes the width it is offered
//! — which is why it did not exist until flex layout arrived. Flex does
//! need it, and the reason is specific: an item with `width: auto` has
//! a *flex base size* of its *max-content* width, and the difference
//! from "the width available" is the whole algorithm.
//!
//! Get it wrong and `flex-grow` inverts. Two auto-width items in a
//! 600px row, measured as though each wanted 600px, look like 1200px of
//! content in 600px of space — so the algorithm *shrinks* them instead
//! of sharing the free space, and the growth factors never apply.
//!
//! Two intrinsic widths exist in CSS:
//!
//! - **max-content**: as wide as the content wants, never wrapping.
//! - **min-content**: as narrow as it can be, wrapping at every chance.
//!
//! Only max-content is implemented, because that is what a flex base
//! size uses. min-content belongs with automatic minimum sizes, which
//! are a separate gap.
//!
//! Percentages are treated as zero throughout. An intrinsic width is
//! computed without a containing block, so there is nothing for a
//! percentage to be a percentage *of* — resolving it against the
//! eventual width would make the measurement depend on its own answer.

use crate::engine::cryo_impl::css::computed::{ComputedStyle, Margin, Sides};
use crate::engine::cryo_impl::css::value::{LengthPercentage, Size};

use super::box_tree::{BoxId, BoxKind, BoxTree};
use super::measure::TextMeasure;

/// How wide this box would be if nothing ever wrapped.
///
/// Includes the box's own margins, borders and padding, so the result
/// is an outer width and can be summed with its siblings' directly.
pub fn max_content_width(tree: &BoxTree, id: BoxId, metrics: &dyn TextMeasure) -> f32 {
    let layout_box = tree.get(id);
    let style = &layout_box.style;

    // A text box carries its *parent's* style, since text has none of
    // its own — so it must not contribute that parent's padding,
    // border or width a second time. Only a box with an element behind
    // it has a frame.
    if let BoxKind::Text(text) = &layout_box.kind {
        // Max-content means no line breaks, so the whole run is one
        // line however long it is.
        return metrics.width(text, style);
    }
    // A break is zero wide but still ends a line, so it contributes
    // nothing to a max-content measurement.
    if layout_box.kind == BoxKind::LineBreak {
        return 0.0;
    }

    let frame = horizontal_frame(style);
    // A stated width is the answer; nothing inside can change it.
    if let Some(width) = definite_width(style) {
        return width + frame;
    }
    children_width(tree, id, metrics) + frame
}

/// The contribution of a box's children.
///
/// Inline children share a line, so they add up. Block children each
/// get their own, so the widest one decides.
fn children_width(tree: &BoxTree, id: BoxId, metrics: &dyn TextMeasure) -> f32 {
    // Out-of-flow children contribute nothing: they are sized against
    // their containing block, not against their parent's content.
    let children = tree.in_flow_children(id);
    if children.is_empty() {
        return 0.0;
    }
    let all_inline = children
        .iter()
        .all(|&child| tree.get(child).kind.is_inline_level());

    // A flex row lays its items side by side whatever their display,
    // and puts a gap between each pair. Leaving the gaps out makes
    // the box measure narrower than it can ever be drawn, so its
    // content wraps inside a box that was sized not to need it.
    let style = &tree.get(id).style;
    let side_by_side =
        all_inline || (style.display.is_flex_container() && style.flex_direction.is_row());
    if !side_by_side {
        return children
            .iter()
            .map(|&child| max_content_width(tree, child, metrics))
            .fold(0.0, f32::max);
    }

    let gap = if style.display.is_flex_container() || style.display.is_grid_container() {
        match style.column_gap {
            LengthPercentage::Length(length) => length.value,
            // A percentage gap is of a width that is not known while
            // measuring, so it counts as nothing — the same rule the
            // rest of this module follows.
            LengthPercentage::Percentage(_) => 0.0,
        }
    } else {
        0.0
    };
    let total: f32 = children
        .iter()
        .map(|&child| max_content_width(tree, child, metrics))
        .sum();
    total + gap * (children.len().saturating_sub(1)) as f32
}

/// A width stated as a length. A percentage is not definite here — see
/// the module note on why.
fn definite_width(style: &ComputedStyle) -> Option<f32> {
    match style.width {
        Size::Definite(LengthPercentage::Length(length)) => Some(length.value.max(0.0)),
        _ => None,
    }
}

/// Margins, borders and padding along the inline axis.
fn horizontal_frame(style: &ComputedStyle) -> f32 {
    let length = |value: LengthPercentage| match value {
        LengthPercentage::Length(length) => length.value,
        LengthPercentage::Percentage(_) => 0.0,
    };
    let margin = |value: Margin| match value {
        Margin::Auto => 0.0,
        Margin::Length(inner) => length(inner),
    };
    let sides = |sides: &Sides<LengthPercentage>| length(sides.left) + length(sides.right);

    sides(&style.padding)
        + style.border_width.left
        + style.border_width.right
        + margin(style.margin.left)
        + margin(style.margin.right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::cascade::{Origin, RuleSet};
    use crate::engine::cryo_impl::css::computed::ComputeContext;
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::css::style_tree::StyleTree;
    use crate::engine::cryo_impl::html::parse as parse_html;
    use crate::engine::cryo_impl::layout::measure::RatioMetrics;

    const UA: &str = "html, body, div, p { display: block } head { display: none }";

    /// The max-content width of the element with id `target`.
    fn width_of(markup: &str, css: &str, target: &str) -> f32 {
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
        let root = tree.root().expect("a root box");
        let node = doc
            .descendants(doc.root())
            .find(|&id| doc.element(id).and_then(|e| e.id()) == Some(target))
            .unwrap_or_else(|| panic!("no element with id {target}"));
        let id = tree
            .descendants(root)
            .into_iter()
            .find(|&id| tree.get(id).node == Some(node))
            .unwrap_or_else(|| panic!("{target} has no box"));
        max_content_width(&tree, id, &RatioMetrics::DEFAULT)
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 0.5
    }

    #[test]
    fn an_empty_box_wants_no_width() {
        assert!(close(width_of("<div id=a></div>", "", "a"), 0.0));
    }

    #[test]
    fn text_wants_the_width_of_its_whole_run() {
        // 16px font at the 0.5 ratio: 8px a character, and no wrapping.
        assert!(close(width_of("<div id=a>abcd</div>", "", "a"), 32.0));
    }

    #[test]
    fn a_stated_width_is_the_answer_whatever_is_inside() {
        assert!(close(
            width_of(
                "<div id=a>a very long run of text indeed</div>",
                "#a { width: 50px }",
                "a"
            ),
            50.0
        ));
    }

    #[test]
    fn padding_and_borders_add_to_the_width() {
        assert!(close(
            width_of(
                "<div id=a>abcd</div>",
                "#a { padding: 0 10px; border-style: solid; border-width: 0 5px }",
                "a"
            ),
            32.0 + 20.0 + 10.0
        ));
    }

    #[test]
    fn a_percentage_contributes_nothing() {
        // There is no containing block to take a percentage of.
        assert!(close(
            width_of("<div id=a>abcd</div>", "#a { padding: 0 50% }", "a"),
            32.0
        ));
        assert!(close(
            width_of("<div id=a>abcd</div>", "#a { width: 50% }", "a"),
            32.0
        ));
    }

    #[test]
    fn inline_children_add_up_because_they_share_a_line() {
        // "ab" and "cd" sit side by side: 16 + 16.
        assert!(close(
            width_of("<div id=a><span>ab</span><span>cd</span></div>", "", "a"),
            32.0
        ));
    }

    #[test]
    fn block_children_take_the_widest_not_the_sum() {
        assert!(close(
            width_of("<div id=a><p>ab</p><p>abcdef</p></div>", "", "a"),
            48.0
        ));
    }

    #[test]
    fn a_wide_child_widens_its_parent() {
        assert!(close(
            width_of("<div id=a><p id=b></p></div>", "#b { width: 120px }", "a"),
            120.0
        ));
    }

    #[test]
    fn a_childs_margins_count_towards_its_parent() {
        assert!(close(
            width_of(
                "<div id=a><p id=b></p></div>",
                "#b { width: 100px; margin: 0 15px }",
                "a"
            ),
            130.0
        ));
    }

    #[test]
    fn nesting_accumulates() {
        assert!(close(
            width_of(
                "<div id=a><div><div id=b>abcd</div></div></div>",
                "#b { padding: 0 10px }",
                "a"
            ),
            52.0
        ));
    }

    #[test]
    fn max_content_does_not_wrap_however_long_the_text() {
        let short = width_of("<div id=a>ab</div>", "", "a");
        let long = width_of("<div id=a>abababababababababab</div>", "", "a");
        assert!(long > short * 5.0, "{short} then {long}");
    }

    #[test]
    fn a_line_break_adds_no_width() {
        assert!(close(width_of("<div id=a>ab<br>cd</div>", "", "a"), 32.0));
    }
    #[test]
    fn a_flex_rows_gaps_count_towards_its_max_content_width() {
        // Leaving them out sizes the box narrower than it can ever be
        // drawn, so its own content wraps inside it.
        let without = width_of(
            "<div id=a><span>ab</span><span>cd</span></div>",
            "#a { display: flex }",
            "a",
        );
        let with = width_of(
            "<div id=a><span>ab</span><span>cd</span></div>",
            "#a { display: flex; gap: 10px }",
            "a",
        );
        assert!(close(without, 32.0), "two runs of 16, got {without}");
        assert!(close(with, 42.0), "one 10px gap between them, got {with}");
    }

    #[test]
    fn a_flex_column_takes_the_widest_child_not_the_sum() {
        let width = width_of(
            "<div id=a><p>ab</p><p>abcdef</p></div>",
            "#a { display: flex; flex-direction: column; gap: 10px }",
            "a",
        );
        assert!(
            close(width, 48.0),
            "no gap along the cross axis, got {width}"
        );
    }
}
