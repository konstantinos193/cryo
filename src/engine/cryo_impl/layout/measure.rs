//! Text measurement, as a port.
//!
//! Layout cannot break a line without knowing how wide a word is, and
//! that answer comes from a font file — glyph advances, kerning,
//! ligatures, shaping for scripts where the glyph depends on its
//! neighbours. None of that exists yet.
//!
//! So layout asks through [`TextMeasure`] and gets whatever adapter it
//! was given. That is the seam the domain declares (AGENTS §4): the real
//! font-backed implementation slots in later without layout changing,
//! and until then [`RatioMetrics`] gives proportional answers that make
//! every structural property of line breaking testable — where breaks
//! fall, how many lines, how tall they are — while the absolute widths
//! stay approximate.
//!
//! The thing to be clear about: a page laid out with [`RatioMetrics`]
//! has **correct structure and wrong pixel widths**. It is not a
//! substitute for shaping, and no test here should assert a width that
//! only a real font could produce.

use crate::engine::cryo_impl::css::computed::ComputedStyle;

/// How wide text is, and how tall a line of it stands.
pub trait TextMeasure {
    /// The advance width of `text` in CSS pixels.
    ///
    /// `text` is a single run in one style with no line breaks in it.
    fn width(&self, text: &str, style: &ComputedStyle) -> f32;

    /// The height of a line box holding text in this style.
    ///
    /// Defaulted because it follows from the style alone — `line-height`
    /// against `font-size` — so only an implementation with real font
    /// metrics for `normal` needs to override it.
    fn line_height(&self, style: &ComputedStyle) -> f32 {
        style.line_box_height()
    }

    /// How far below the top of a line box the text baseline sits.
    ///
    /// Approximated from the line height by default. With real metrics
    /// this comes from the font's ascent and the half-leading above it.
    fn baseline(&self, style: &ComputedStyle) -> f32 {
        let line = self.line_height(style);
        let leading = (line - style.font_size) / 2.0;
        leading + style.font_size * TYPICAL_ASCENT_RATIO
    }
}

/// Roughly where the baseline sits within an em for Latin text. A
/// placeholder constant, replaced by the font's own ascent.
const TYPICAL_ASCENT_RATIO: f32 = 0.8;

/// Every character the same fraction of the font size wide.
///
/// Exact for a monospace font at the right ratio, and wrong in detail
/// for everything else — an `i` is not as wide as a `W`. Good enough to
/// drive and test line breaking, and nothing more.
#[derive(Clone, Copy, Debug)]
pub struct RatioMetrics {
    /// Character width as a fraction of the font size.
    pub ratio: f32,
}

impl RatioMetrics {
    /// A reasonable stand-in for a proportional font's average.
    pub const DEFAULT: Self = Self { ratio: 0.5 };
}

impl Default for RatioMetrics {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TextMeasure for RatioMetrics {
    fn width(&self, text: &str, style: &ComputedStyle) -> f32 {
        // Characters, not bytes: one `é` is one character wide.
        text.chars().count() as f32 * style.font_size * self.ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::computed::LineHeight;

    fn with_font(font_size: f32, line_height: LineHeight) -> ComputedStyle {
        ComputedStyle {
            font_size,
            line_height,
            ..ComputedStyle::INITIAL
        }
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    #[test]
    fn width_scales_with_the_character_count_and_font_size() {
        let metrics = RatioMetrics::DEFAULT;
        let style = with_font(20.0, LineHeight::Normal);
        assert!(close(metrics.width("abcd", &style), 40.0));
        assert!(close(metrics.width("", &style), 0.0));
        assert!(close(
            metrics.width("ab", &with_font(10.0, LineHeight::Normal)),
            10.0
        ));
    }

    #[test]
    fn width_counts_characters_not_bytes() {
        let metrics = RatioMetrics::DEFAULT;
        let style = with_font(10.0, LineHeight::Normal);
        // Four characters, more than four bytes.
        assert!(close(metrics.width("café", &style), 20.0));
        assert!(close(metrics.width("日本語", &style), 15.0));
    }

    #[test]
    fn the_ratio_is_configurable() {
        let wide = RatioMetrics { ratio: 1.0 };
        let style = with_font(10.0, LineHeight::Normal);
        assert!(close(wide.width("abc", &style), 30.0));
    }

    #[test]
    fn line_height_follows_the_style() {
        let metrics = RatioMetrics::DEFAULT;
        assert!(close(
            metrics.line_height(&with_font(10.0, LineHeight::Number(2.0))),
            20.0
        ));
        assert!(close(
            metrics.line_height(&with_font(10.0, LineHeight::Length(7.0))),
            7.0
        ));
        // `normal` is the 1.2 approximation from the style.
        assert!(close(
            metrics.line_height(&with_font(10.0, LineHeight::Normal)),
            12.0
        ));
    }

    #[test]
    fn the_baseline_sits_inside_the_line_box() {
        let metrics = RatioMetrics::DEFAULT;
        let tall = with_font(10.0, LineHeight::Number(3.0));
        let baseline = metrics.baseline(&tall);
        assert!(baseline > 0.0);
        assert!(baseline < metrics.line_height(&tall));
    }

    #[test]
    fn extra_line_height_is_split_above_and_below_the_text() {
        // Half-leading: doubling the line height moves the baseline down
        // by half the extra, not all of it.
        let metrics = RatioMetrics::DEFAULT;
        let tight = metrics.baseline(&with_font(10.0, LineHeight::Number(1.0)));
        let loose = metrics.baseline(&with_font(10.0, LineHeight::Number(2.0)));
        assert!(close(loose - tight, 5.0), "{tight} -> {loose}");
    }

    #[test]
    fn the_default_metrics_are_the_documented_ratio() {
        assert!(close(RatioMetrics::default().ratio, 0.5));
    }
}
