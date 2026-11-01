//! Paint: geometry becomes pixels.
//!
//! Two steps, and the split is what keeps either one testable:
//!
//! ```text
//!   display_list   fragments -> an ordered list of draw commands
//!    ▼
//!   raster         draw commands -> a pixel buffer
//! ```
//!
//! The display list decides *what* is drawn and *in what order*; the
//! rasteriser decides how a rectangle or a glyph becomes bytes. Nothing
//! in the first step knows about pixels, and nothing in the second knows
//! about CSS — so paint order can be asserted without a surface, and
//! blending can be asserted without a document.

pub mod clip;
pub mod display_list;
pub mod font;
pub mod raster;

pub use display_list::{build, DisplayItem, DisplayList};
pub use font::{BuiltinFont, Glyph, GlyphSource, NoGlyphs};
pub use raster::Canvas;

#[cfg(test)]
mod tests {
    use super::font::{BuiltinFont, NoGlyphs};
    use super::{build, Canvas};
    use crate::engine::cryo_impl::css::cascade::{Origin, RuleSet};
    use crate::engine::cryo_impl::css::computed::ComputeContext;
    use crate::engine::cryo_impl::css::computed::ComputedStyle;
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::css::style_tree::StyleTree;
    use crate::engine::cryo_impl::css::value::Color;
    use crate::engine::cryo_impl::html::parse as parse_html;
    use crate::engine::cryo_impl::image::NoImages;
    use crate::engine::cryo_impl::layout::block::layout_document;
    use crate::engine::cryo_impl::layout::box_tree::BoxTree;
    use crate::engine::cryo_impl::layout::measure::{RatioMetrics, TextMeasure};

    /// Markup and a stylesheet, all the way to pixels.
    ///
    /// Every stage in one call: parse, cascade, compute, box tree,
    /// layout, display list, raster. If any seam between them stops
    /// lining up, this is what notices.
    fn render(markup: &str, css: &str, width: u32, height: u32) -> Canvas {
        let doc = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::UserAgent,
            "html, body, div, p { display: block } head { display: none }",
        );
        rules.insert_css(Origin::Author, css);
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext {
                viewport_width: width as f32,
                viewport_height: height as f32,
                root_font_size: None,
            },
        );
        let tree = BoxTree::build(&doc, &styles);
        let metrics = RatioMetrics::DEFAULT;
        let root = layout_document(&tree, width as f32, height as f32, &metrics)
            .expect("the document has a root box");
        let list = build(&tree, &root, &doc, "https://test.invalid/", &|style| {
            metrics.baseline(style)
        });

        let mut canvas = Canvas::filled(width, height, Color::WHITE);
        canvas.render(&list, &BuiltinFont, &NoImages);
        canvas
    }

    fn at(canvas: &Canvas, x: u32, y: u32) -> Color {
        canvas.pixel(x, y).expect("the pixel is in bounds")
    }

    #[test]
    fn a_styled_box_reaches_the_canvas() {
        let canvas = render(
            "<div></div>",
            "div { width: 20px; height: 10px; background: red }",
            40,
            20,
        );
        assert_eq!(at(&canvas, 0, 0), Color::rgb(255, 0, 0));
        assert_eq!(at(&canvas, 19, 9), Color::rgb(255, 0, 0));
        // Outside the box, the page's own white shows through.
        assert_eq!(at(&canvas, 20, 0), Color::WHITE);
        assert_eq!(at(&canvas, 0, 10), Color::WHITE);
    }

    #[test]
    fn a_centred_box_lands_where_layout_put_it() {
        // The whole chain has to agree: `margin: 0 auto` resolved in
        // block layout, positioned absolutely, painted at that position.
        let canvas = render(
            "<div></div>",
            "div { width: 10px; height: 4px; margin: 0 auto; background: blue }",
            30,
            10,
        );
        assert_eq!(at(&canvas, 9, 0), Color::WHITE, "left of the box");
        assert_eq!(at(&canvas, 10, 0), Color::rgb(0, 0, 255));
        assert_eq!(at(&canvas, 19, 3), Color::rgb(0, 0, 255));
        assert_eq!(at(&canvas, 20, 0), Color::WHITE, "right of the box");
    }

    #[test]
    fn stacked_boxes_paint_at_their_stacked_positions() {
        let canvas = render(
            "<div id=a></div><div id=b></div>",
            concat!(
                "div { height: 5px }",
                "#a { background: red }",
                "#b { background: blue }",
            ),
            10,
            20,
        );
        assert_eq!(at(&canvas, 0, 0), Color::rgb(255, 0, 0));
        assert_eq!(at(&canvas, 0, 4), Color::rgb(255, 0, 0));
        assert_eq!(at(&canvas, 0, 5), Color::rgb(0, 0, 255));
        assert_eq!(at(&canvas, 0, 9), Color::rgb(0, 0, 255));
        assert_eq!(at(&canvas, 0, 10), Color::WHITE);
    }

    #[test]
    fn a_child_background_covers_its_parents() {
        let canvas = render(
            "<div><p></p></div>",
            concat!(
                "div { height: 10px; background: red }",
                "p { height: 4px; background: blue }",
            ),
            10,
            20,
        );
        assert_eq!(at(&canvas, 0, 0), Color::rgb(0, 0, 255), "the child wins");
        assert_eq!(at(&canvas, 0, 5), Color::rgb(255, 0, 0), "below the child");
    }

    #[test]
    fn a_border_paints_around_its_background() {
        let canvas = render(
            "<div></div>",
            concat!(
                "div { width: 6px; height: 6px; background: blue;",
                "      border: 2px solid red }",
            ),
            20,
            20,
        );
        assert_eq!(at(&canvas, 0, 0), Color::rgb(255, 0, 0), "border");
        assert_eq!(at(&canvas, 1, 5), Color::rgb(255, 0, 0), "border");
        assert_eq!(at(&canvas, 2, 2), Color::rgb(0, 0, 255), "background");
        assert_eq!(at(&canvas, 9, 9), Color::rgb(255, 0, 0), "far border edge");
        assert_eq!(at(&canvas, 10, 10), Color::WHITE, "outside");
    }

    #[test]
    fn a_translucent_background_blends_with_the_page() {
        let canvas = render(
            "<div></div>",
            "div { width: 4px; height: 4px; background: rgba(0, 0, 0, 0.5) }",
            10,
            10,
        );
        assert_eq!(at(&canvas, 0, 0).red, 128, "black at 50% over white");
    }

    #[test]
    fn an_empty_page_is_just_its_background() {
        let canvas = render("<p></p>", "", 8, 8);
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(at(&canvas, x, y), Color::WHITE);
            }
        }
    }

    /// Everything through to pixels, measured with the real font so
    /// layout and paint agree about how wide the text is.
    fn render_with_font(markup: &str, css: &str, width: u32, height: u32) -> Canvas {
        let doc = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::UserAgent,
            "html, body, div, p { display: block } head { display: none }",
        );
        rules.insert_css(Origin::Author, css);
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = BoxTree::build(&doc, &styles);
        let root = layout_document(&tree, width as f32, height as f32, &BuiltinFont)
            .expect("a root fragment");
        let list = build(&tree, &root, &doc, "https://test.invalid/", &|style| {
            BuiltinFont.baseline(style)
        });

        let mut canvas = Canvas::filled(width, height, Color::WHITE);
        assert_eq!(
            canvas.render(&list, &BuiltinFont, &NoImages),
            0,
            "every character should have a glyph"
        );
        canvas
    }

    /// Does any pixel in this column differ from the page background?
    fn column_has_ink(canvas: &Canvas, x: u32) -> bool {
        (0..canvas.height()).any(|y| at(canvas, x, y) != Color::WHITE)
    }

    #[test]
    fn text_reaches_the_canvas_as_pixels() {
        let canvas = render_with_font("<p>L</p>", "p { font-size: 9px }", 40, 20);
        // `L` is a stem and a foot, so its first column has ink.
        assert!(column_has_ink(&canvas, 0));
        // And the inter-glyph gap after it does not.
        assert!(!column_has_ink(&canvas, 5));
    }

    #[test]
    fn text_is_painted_in_its_css_colour() {
        let canvas = render_with_font("<p>L</p>", "p { font-size: 9px; color: red }", 40, 20);
        let inked = (0..20)
            .flat_map(|y| (0..10).map(move |x| (x, y)))
            .find(|&(x, y)| at(&canvas, x, y) != Color::WHITE)
            .expect("some ink");
        assert_eq!(at(&canvas, inked.0, inked.1), Color::rgb(255, 0, 0));
    }

    #[test]
    fn text_sits_inside_the_box_that_measured_it() {
        // The seam this guards: layout measured the run with BuiltinFont
        // and paint drew it with the same font, so the ink cannot spill
        // past the width layout reserved.
        let canvas = render_with_font("<p>LLL</p>", "p { font-size: 9px }", 40, 20);
        let advance = 6.0 * BuiltinFont::scale(9.0);
        let run_width = (advance * 3.0).ceil() as u32;
        for x in run_width..40 {
            assert!(!column_has_ink(&canvas, x), "ink at x={x}, past the run");
        }
    }

    #[test]
    fn wrapped_text_paints_on_two_lines() {
        // 9px font, 6px advance: "LL" is 12px, so 15px fits one word.
        let canvas = render_with_font("<p>LL LL</p>", "p { font-size: 9px; width: 15px }", 40, 40);
        let inked_rows: Vec<u32> = (0..40)
            .filter(|&y| (0..40).any(|x| at(&canvas, x, y) != Color::WHITE))
            .collect();
        let first = *inked_rows.first().expect("some ink");
        let last = *inked_rows.last().expect("some ink");
        assert!(
            last - first > 8,
            "ink spans two lines, rows {first}..={last}"
        );
    }

    #[test]
    fn a_page_with_no_font_lays_out_identically_and_reports_its_characters() {
        // Swapping the font must change what appears, never where.
        let doc = parse_html("<p>hello</p>");
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, "html, body, p { display: block }");
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = BoxTree::build(&doc, &styles);

        let with_glyphs = layout_document(&tree, 100.0, 50.0, &BuiltinFont).expect("a root");
        let without = layout_document(&tree, 100.0, 50.0, &NoGlyphs).expect("a root");
        assert_eq!(with_glyphs, without, "the geometry is the same either way");

        let list = build(
            &tree,
            &with_glyphs,
            &doc,
            "https://test.invalid/",
            &|style| BuiltinFont.baseline(style),
        );
        let mut canvas = Canvas::filled(100, 50, Color::WHITE);
        assert_eq!(
            canvas.render(&list, &NoGlyphs, &NoImages),
            5,
            "one report per character of \"hello\""
        );
    }

    /// The families this machine can actually supply, or `None` on a
    /// box with no fonts installed.
    fn system_set() -> Option<super::font::outline_font::FontSet> {
        use super::font::system::SystemFonts;
        let system = SystemFonts::new();
        let set = system.load_set(&[
            "serif".to_owned(),
            "sans-serif".to_owned(),
            "monospace".to_owned(),
        ]);
        (set.len() == 3).then_some(set)
    }

    #[test]
    fn a_page_can_mix_families_and_each_run_gets_its_own_face() {
        use crate::engine::cryo_impl::css::value::Color as CssColor;
        let Some(fonts) = system_set() else { return };

        let doc = parse_html("<p class=a>WWW</p><p class=b>WWW</p>");
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, "html, body, p { display: block }");
        rules.insert_css(
            Origin::Author,
            ".a { font-family: monospace } .b { font-family: serif }",
        );
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = BoxTree::build(&doc, &styles);
        let root = layout_document(&tree, 400.0, 200.0, &fonts).expect("a root");
        let list = build(&tree, &root, &doc, "https://test.invalid/", &|style| {
            fonts.baseline(style)
        });

        // The same text in two families must measure differently, which
        // is only true if each run resolved its own face.
        let widths: Vec<f32> = list
            .items
            .iter()
            .filter_map(|item| match item {
                super::DisplayItem::Text { rect, .. } => Some(rect.width),
                super::DisplayItem::SolidRect { .. }
                | super::DisplayItem::Gradient { .. }
                | super::DisplayItem::Image { .. }
                | super::DisplayItem::Shadow { .. }
                | super::DisplayItem::Shape { .. }
                | super::DisplayItem::PushClip { .. }
                | super::DisplayItem::PopClip => None,
            })
            .collect();
        assert_eq!(widths.len(), 2, "two runs");
        assert!(
            (widths[0] - widths[1]).abs() > 0.5,
            "monospace and serif gave the same width: {widths:?}"
        );

        let mut canvas = Canvas::filled(400, 200, CssColor::WHITE);
        assert_eq!(
            canvas.render(&list, &fonts, &NoImages),
            0,
            "every glyph was drawn"
        );
    }

    #[test]
    fn a_run_with_no_available_face_is_reported_not_silently_blank() {
        use super::font::outline_font::FontSet;
        let doc = parse_html("<p>hello</p>");
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, "html, body, p { display: block }");
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = BoxTree::build(&doc, &styles);
        // An empty set resolves nothing at all.
        let empty = FontSet::new();
        let root = layout_document(&tree, 200.0, 100.0, &empty).expect("a root");
        let list = build(&tree, &root, &doc, "https://test.invalid/", &|style| {
            empty.baseline(style)
        });

        let mut canvas = Canvas::filled(200, 100, Color::WHITE);
        assert_eq!(
            canvas.render(&list, &empty, &NoImages),
            5,
            "one report per character of \"hello\""
        );
    }

    #[test]
    fn an_empty_font_set_still_lays_text_out() {
        use super::font::outline_font::FontSet;
        // Text that cannot be drawn must still take up space, or the
        // page reflows around a missing font.
        let empty = FontSet::new();
        let style = ComputedStyle {
            font_size: 16.0,
            ..ComputedStyle::INITIAL
        };
        assert!(empty.width("hello", &style) > 0.0);
        assert!(empty.baseline(&style) > 0.0);
        assert!(empty.line_height(&style) > 0.0);
    }

    #[test]
    fn the_ratio_metrics_still_lay_out_without_a_font() {
        // RatioMetrics is not a font, only a measurer; it must still
        // drive layout for callers that do not paint.
        let doc = parse_html("<p>hello</p>");
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, "html, body, p { display: block }");
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = BoxTree::build(&doc, &styles);
        let metrics = RatioMetrics::DEFAULT;
        let root = layout_document(&tree, 100.0, 50.0, &metrics).expect("a root");
        let list = build(&tree, &root, &doc, "https://test.invalid/", &|style| {
            metrics.baseline(style)
        });
        assert_eq!(list.len(), 1, "one text run");
    }
}
