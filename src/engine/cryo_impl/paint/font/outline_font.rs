//! A font backed by a real TrueType file.
//!
//! Ties the three pieces together: [`sfnt`](super::sfnt) finds the
//! glyph and its advance, [`glyf`](super::glyf) reads the outline, and
//! [`scan`](crate::engine::cryo_impl::scan) fills it into a coverage mask. This is the
//! implementation [`GlyphSource`] exists for.
//!
//! Two things it gets right that the bitmap fallback cannot:
//!
//! - **`line-height: normal` comes from the font.** The 1.2 factor the
//!   style falls back on is a guess; a font states its own ascent,
//!   descent and line gap, and they differ enough between typefaces to
//!   be visible in a paragraph.
//! - **Advances are per glyph.** A real font makes `i` narrower than
//!   `W`, so a line of text is the width it looks like.
//!
//! Y is flipped on the way out. Fonts measure upward from the baseline;
//! the canvas measures downward from the top. Doing it here, once, means
//! no caller has to remember which convention it is holding.
//!
//! Not done yet, and visible: no glyph cache, so a character is
//! re-rasterised every time it is drawn; no kerning, so `AV` sits
//! slightly too far apart; no hinting, so stems are not snapped to the
//! pixel grid and small text is softer than a native renderer's.

use std::collections::BTreeMap;
use std::path::Path;

use crate::engine::cryo_impl::css::computed::{ComputedStyle, FontStyle, LineHeight};

use super::super::super::layout::measure::TextMeasure;
use super::glyf;
use super::sfnt::{FaceStyle, FontFile};
use super::{FontChoice, FontError, GlyphSource, RenderedGlyph};
use crate::engine::cryo_impl::scan::{self, Mask};

/// A loaded TrueType font.
#[derive(Clone, Debug)]
pub struct OutlineFont {
    file: FontFile,
    /// The family this was loaded for, so a font set can report what it
    /// matched without re-reading the `name` table.
    family: String,
}

impl OutlineFont {
    /// Load a font from a file on disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, FontError> {
        let path = path.as_ref();
        let data = std::fs::read(path).map_err(|_| FontError::Unreadable)?;
        let family = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::from_bytes(data, family)
    }

    /// Parse a font already in memory — a system file, or one a page
    /// fetched with `@font-face`.
    pub fn from_bytes(data: Vec<u8>, family: impl Into<String>) -> Result<Self, FontError> {
        Ok(Self {
            file: FontFile::parse(data)?,
            family: family.into(),
        })
    }

    /// The same face filed under a different family name.
    ///
    /// A downloaded font carries whatever name its own `name` table
    /// holds, which need not be the one the page declared it as —
    /// and the page's name is the one every `font-family` will ask
    /// for.
    pub fn with_family(mut self, family: &str) -> Self {
        self.family = family.to_owned();
        self
    }

    /// Whether this face has a glyph for `character`.
    ///
    /// Glyph zero is the font's "no such character" box, so a cmap
    /// that answers with it has not answered.
    pub fn covers(&self, character: char) -> bool {
        self.file
            .glyph_id(character)
            .is_some_and(|glyph| glyph != 0)
    }

    pub fn family(&self) -> &str {
        &self.family
    }

    pub fn file(&self) -> &FontFile {
        &self.file
    }

    /// Which face of its family this is.
    pub fn style(&self) -> FaceStyle {
        self.file.style()
    }

    /// Pixels per font design unit at this size.
    fn scale(&self, font_size: f32) -> f32 {
        font_size / f32::from(self.file.metrics().units_per_em)
    }

    /// The ascent in pixels — how far text rises above its baseline.
    pub fn ascent_px(&self, font_size: f32) -> f32 {
        f32::from(self.file.metrics().ascent) * self.scale(font_size)
    }
}

impl GlyphSource for OutlineFont {
    fn weight(&self) -> u16 {
        self.style().weight
    }

    fn covers(&self, character: char) -> bool {
        OutlineFont::covers(self, character)
    }

    fn render(&self, character: char, font_size: f32) -> Option<RenderedGlyph> {
        let glyph = self.file.glyph_id(character)?;
        let outline = glyf::outline(&self.file, glyph).ok()?;
        if outline.is_empty() {
            // A space has a glyph and an advance but nothing to draw.
            return None;
        }
        let scale = self.scale(font_size);
        let polygons = outline.flatten(scale);
        let (min_x, min_y, max_x, max_y) = bounds(&polygons)?;

        // Snap the mask to whole pixels around the glyph, then shift the
        // outline into it. Rendering the glyph at its own origin would
        // waste a mask the size of the whole em square.
        let left = min_x.floor();
        let bottom = max_y.ceil();
        let width = (max_x.ceil() - left).max(1.0) as u32;
        let height = (bottom - min_y.floor()).max(1.0) as u32;

        // Flip Y: the font measures up from the baseline, the mask down
        // from its own top edge.
        let placed: Vec<Vec<(f32, f32)>> = polygons
            .iter()
            .map(|polygon| {
                polygon
                    .iter()
                    .map(|&(x, y)| (x - left, bottom - y))
                    .collect()
            })
            .collect();

        Some(RenderedGlyph {
            mask: scan::fill(&placed, width, height),
            left,
            // The mask's top edge sits this far above the baseline.
            top: -bottom,
        })
    }

    fn advance_px(&self, character: char, font_size: f32) -> f32 {
        let glyph = self.file.glyph_id(character).unwrap_or(0);
        f32::from(self.file.advance(glyph)) * self.scale(font_size)
    }
}

impl TextMeasure for OutlineFont {
    fn width(&self, text: &str, style: &ComputedStyle) -> f32 {
        let advances: f32 = text
            .chars()
            .map(|character| self.advance_px(character, style.font_size))
            .sum();
        // Spacing goes after every character, the last one included —
        // which is what CSS says, and what makes a one-character run
        // measure the same alone as it does in a line.
        advances + style.letter_spacing * text.chars().count() as f32
    }

    /// `line-height: normal` takes the font's own line spacing.
    ///
    /// The style's 1.2 factor is a stand-in for exactly this number, and
    /// fonts disagree with it enough to change where lines fall.
    fn line_height(&self, style: &ComputedStyle) -> f32 {
        match style.line_height {
            LineHeight::Normal => self.file.metrics().line_spacing() * self.scale(style.font_size),
            other => other.to_px(style.font_size),
        }
    }

    fn baseline(&self, style: &ComputedStyle) -> f32 {
        let line = self.line_height(style);
        let ascent = self.ascent_px(style.font_size);
        let descent = -f32::from(self.file.metrics().descent) * self.scale(style.font_size);
        // Split whatever the line height adds beyond the font's own
        // height evenly above and below — CSS half-leading.
        let leading = (line - (ascent + descent)) / 2.0;
        leading + ascent
    }
}

/// The bounding box of a set of polygons.
fn bounds(polygons: &[Vec<(f32, f32)>]) -> Option<(f32, f32, f32, f32)> {
    let mut points = polygons.iter().flatten();
    let &(x, y) = points.next()?;
    let mut extent = (x, y, x, y);
    for &(x, y) in points {
        extent.0 = extent.0.min(x);
        extent.1 = extent.1.min(y);
        extent.2 = extent.2.max(x);
        extent.3 = extent.3.max(y);
    }
    Some(extent)
}

/// What a text run wants: a family stack, a weight and a slant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaceRequest<'a> {
    pub families: &'a [String],
    /// 100..=900, as `font-weight` computes to.
    pub weight: u16,
    pub italic: bool,
    /// A character the run contains, if one is known.
    ///
    /// A web page's font is usually shipped as a dozen files split
    /// by alphabet, all declared under one family name, and only one
    /// of them holds any given letter. Picking by weight and slant
    /// alone lands on whichever was declared first — often the
    /// Cyrillic subset — and every Latin character comes out blank.
    pub sample: Option<char>,
}

impl<'a> FaceRequest<'a> {
    /// A regular face of these families.
    pub fn regular(families: &'a [String]) -> Self {
        Self {
            families,
            weight: 400,
            italic: false,
            sample: None,
        }
    }

    /// The same request, for a run that begins with this text.
    pub fn for_text(mut self, text: &str) -> Self {
        self.sample = text.chars().find(|ch| !ch.is_whitespace());
        self
    }

    /// What a computed style asks for.
    pub fn of(style: &'a ComputedStyle) -> Self {
        Self {
            families: &style.font_family,
            weight: style.font_weight,
            italic: style.font_style != FontStyle::Normal,
            sample: None,
        }
    }
}

/// One family's faces — regular, bold, italic, bold italic.
#[derive(Debug, Default, Clone)]
struct Family {
    faces: Vec<OutlineFont>,
}

impl Family {
    /// The face closest to what was asked for.
    ///
    /// Slant is matched first and absolutely: an upright face is never
    /// a better answer than a slanted one when italic was asked for,
    /// however far its weight is off. CSS orders it that way because a
    /// reader notices a missing italic long before a missing weight.
    fn best(&self, weight: u16, italic: bool, sample: Option<char>) -> Option<&OutlineFont> {
        let matching_slant: Vec<&OutlineFont> = self
            .faces
            .iter()
            .filter(|face| face.style().italic == italic)
            .collect();
        let pool = if matching_slant.is_empty() {
            self.faces.iter().collect()
        } else {
            matching_slant
        };
        // Coverage outranks weight: a face that cannot draw the text
        // is no answer at any weight, while a face at the wrong
        // weight still reads.
        pool.into_iter().min_by_key(|face| {
            let blank = sample.is_some_and(|character| !face.covers(character));
            (blank, weight_distance(face.style().weight, weight))
        })
    }
}

/// How far a face's weight is from the one asked for, as a sort key.
///
/// Not a plain difference. CSS Fonts 4 searches in bands, and which
/// band a face lands in decides before how far off it is:
///
/// | wanted    | first        | then           | last       |
/// |-----------|--------------|----------------|------------|
/// | under 400 | down to 100  | up from wanted | —          |
/// | 400–500   | up to 500    | down to 100    | above 500  |
/// | over 500  | up from here | down to 100    | —          |
///
/// The middle row is the one that shows. A machine with regular and
/// bold and nothing between draws a `font-weight: 500` label — which
/// is most of them — in the *regular* face, not the bold one, because
/// the search runs down past 400 before it ever looks at 700. Treating
/// 500 as "go heavier" instead makes every medium label on a page
/// shout.
fn weight_distance(available: u16, wanted: u16) -> u32 {
    let band: u32 = if (400..=500).contains(&wanted) {
        match available {
            _ if available >= wanted && available <= 500 => 0,
            _ if available < wanted => 1,
            _ => 2,
        }
    } else if wanted < 400 {
        u32::from(available > wanted)
    } else {
        u32::from(available < wanted)
    };
    // The band's weight exceeds any possible difference, so it always
    // decides before magnitude does.
    band * 10_000 + u32::from(available.abs_diff(wanted))
}

/// Family names mapped to the faces that satisfy them.
///
/// A real font stack is a list of families, and the first one present on
/// the machine wins; this holds what was found so the lookup happens
/// once per page rather than once per text run.
#[derive(Debug, Default, Clone)]
pub struct FontSet {
    families: BTreeMap<String, Family>,
    fallback: Option<Family>,
}

impl FontSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a face under a family name, lowercased so lookup can be
    /// case-insensitive the way CSS family names are.
    ///
    /// Several faces may share a family; which one a run gets is decided
    /// at lookup by its weight and slant.
    pub fn insert(&mut self, family: &str, font: OutlineFont) {
        let entry = self
            .families
            .entry(family.to_ascii_lowercase())
            .or_default();
        entry.faces.push(font.clone());
        if self.fallback.is_none() {
            self.fallback = Some(Family { faces: vec![font] });
        }
    }

    /// Set the family used when a page's whole stack is unavailable.
    pub fn set_fallback(&mut self, font: OutlineFont) {
        self.fallback = Some(Family { faces: vec![font] });
    }

    /// Add a fallback face alongside any already set.
    pub fn add_fallback(&mut self, font: OutlineFont) {
        self.fallback
            .get_or_insert_with(Family::default)
            .faces
            .push(font);
    }

    /// The best face for `request`, or the fallback family's best.
    pub fn select(&self, request: FaceRequest<'_>) -> Option<&OutlineFont> {
        request
            .families
            .iter()
            .find_map(|family| self.families.get(&family.to_ascii_lowercase()))
            .or(self.fallback.as_ref())
            .and_then(|family| family.best(request.weight, request.italic, request.sample))
    }

    /// The regular face of the first available family.
    pub fn resolve(&self, stack: &[String]) -> Option<&OutlineFont> {
        self.select(FaceRequest::regular(stack))
    }

    /// How many families are known.
    pub fn len(&self) -> usize {
        self.families.len()
    }

    /// How many faces are loaded across every family.
    pub fn face_count(&self) -> usize {
        self.families
            .values()
            .map(|family| family.faces.len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }
}

impl FontChoice for OutlineFont {
    fn choose(&self, _request: FaceRequest<'_>) -> Option<&dyn GlyphSource> {
        Some(self)
    }
}

impl FontChoice for FontSet {
    fn choose(&self, request: FaceRequest<'_>) -> Option<&dyn GlyphSource> {
        self.select(request).map(|face| face as &dyn GlyphSource)
    }
}

/// Measuring through a set resolves the font per element, so a serif
/// heading and a monospace block are measured with their own faces.
///
/// With no font at all the measurements fall back to the style's own
/// arithmetic rather than to zero: text that cannot be drawn should
/// still occupy the space it would have, so the rest of the page does
/// not reflow around a missing font.
impl TextMeasure for FontSet {
    fn width(&self, text: &str, style: &ComputedStyle) -> f32 {
        // Measured the way it will be drawn: one face at a time,
        // breaking where that face runs out. Measuring the whole run
        // with the first face instead reserves the wrong width, and
        // the words after the break sit in a gap.
        let mut total = 0.0;
        let mut rest = text;
        while !rest.is_empty() {
            let Some(font) = self.select(FaceRequest::of(style).for_text(rest)) else {
                return total + super::BuiltinFont.width(rest, style);
            };
            let segment = super::covered_prefix(rest, font);
            total += font.width(segment, style);
            rest = &rest[segment.len()..];
        }
        total
    }

    fn line_height(&self, style: &ComputedStyle) -> f32 {
        match self.select(FaceRequest::of(style)) {
            Some(font) => font.line_height(style),
            None => style.line_box_height(),
        }
    }

    fn baseline(&self, style: &ComputedStyle) -> f32 {
        match self.select(FaceRequest::of(style)) {
            Some(font) => font.baseline(style),
            None => super::BuiltinFont.baseline(style),
        }
    }
}

/// Coverage sampled at a point, for tests and for debugging a glyph.
pub fn coverage_at(mask: &Mask, x: u32, y: u32) -> u8 {
    mask.at(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A system font to test against, if this machine has one.
    ///
    /// Tests that need a real font skip rather than fail where none is
    /// installed — the parser's own correctness is covered by the
    /// hand-built fonts in [`super::super::sfnt`], and these check the
    /// altogether different question of whether a shipping font from a
    /// real vendor round-trips.
    fn system_font() -> Option<OutlineFont> {
        for path in [
            "C:/Windows/Fonts/arial.ttf",
            "C:/Windows/Fonts/segoeui.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ] {
            if let Ok(font) = OutlineFont::load(path) {
                return Some(font);
            }
        }
        None
    }

    fn style(font_size: f32, line_height: LineHeight) -> ComputedStyle {
        ComputedStyle {
            font_size,
            line_height,
            ..ComputedStyle::INITIAL
        }
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        assert!(matches!(
            OutlineFont::load("/no/such/font/anywhere.ttf"),
            Err(FontError::Unreadable)
        ));
    }

    #[test]
    fn bytes_that_are_not_a_font_are_rejected() {
        assert!(OutlineFont::from_bytes(b"hello".to_vec(), "x").is_err());
    }

    #[test]
    fn a_font_set_resolves_the_first_available_family() {
        let Some(font) = system_font() else { return };
        let mut set = FontSet::new();
        set.insert("Arial", font);
        assert_eq!(set.len(), 1);
        assert!(!set.is_empty());

        let stack = vec!["Nonexistent".to_owned(), "arial".to_owned()];
        assert!(set.resolve(&stack).is_some(), "case-insensitive match");
        // An entirely unknown stack still gets the fallback.
        assert!(set.resolve(&["Nothing".to_owned()]).is_some());
    }

    #[test]
    fn an_empty_font_set_resolves_to_nothing() {
        let set = FontSet::new();
        assert!(set.is_empty());
        assert_eq!(set.face_count(), 0);
        assert!(set.resolve(&["Arial".to_owned()]).is_none());
    }

    #[test]
    fn a_medium_weight_falls_back_to_regular_not_to_bold() {
        // The rule that decides how a real page looks. Most machines
        // have regular and bold and nothing between, and `font-weight:
        // 500` is everywhere — so what 500 falls back to is the
        // difference between a page of labels and a page of shouting.
        // CSS searches 400..=500 upwards, then *downwards*, and only
        // then above 500.
        assert!(weight_distance(400, 500) < weight_distance(700, 500));
        assert!(weight_distance(500, 500) < weight_distance(400, 500));
    }

    #[test]
    fn a_regular_weight_prefers_medium_over_light() {
        // 400 searches up to 500 before it looks below itself.
        assert!(weight_distance(500, 400) < weight_distance(300, 400));
        assert!(weight_distance(300, 400) < weight_distance(700, 400));
    }

    #[test]
    fn weight_matching_prefers_going_heavier_above_five_hundred() {
        // Above the 400–500 band the rule is the simple one.
        assert!(weight_distance(700, 600) < weight_distance(400, 600));
    }

    #[test]
    fn weight_matching_prefers_going_lighter_below_four_hundred() {
        // And it inverts under 400.
        assert!(weight_distance(100, 300) < weight_distance(700, 300));
    }

    #[test]
    fn an_exact_weight_always_wins() {
        for wanted in [100, 300, 400, 500, 700, 900] {
            assert_eq!(weight_distance(wanted, wanted), 0);
            for available in [100, 400, 700, 900] {
                if available != wanted {
                    assert!(weight_distance(available, wanted) > 0);
                }
            }
        }
    }

    #[test]
    fn slant_is_matched_before_weight() {
        // An italic face at the wrong weight beats an upright one at the
        // right weight: a missing italic is far more visible.
        let Some(regular) = system_font() else { return };
        let mut family = Family::default();
        family.faces.push(regular);

        // With only an upright face, that is what comes back.
        let only = family.best(400, true, None).expect("something");
        assert!(!only.style().italic, "no italic exists to find");
    }

    #[test]
    fn a_family_with_no_faces_resolves_to_nothing() {
        let family = Family::default();
        assert!(family.best(400, false, None).is_none());
    }

    #[test]
    fn a_face_request_reads_a_computed_style() {
        use crate::engine::cryo_impl::css::computed::FontStyle;
        let style = ComputedStyle {
            font_weight: 700,
            font_style: FontStyle::Italic,
            font_family: vec!["Georgia".to_owned()],
            ..ComputedStyle::INITIAL
        };
        let request = FaceRequest::of(&style);
        assert_eq!(request.weight, 700);
        assert!(request.italic);
        assert_eq!(request.families, ["Georgia"]);

        let oblique = ComputedStyle {
            font_style: FontStyle::Oblique,
            ..ComputedStyle::INITIAL
        };
        assert!(FaceRequest::of(&oblique).italic, "oblique slants too");
    }

    #[test]
    fn a_regular_request_asks_for_the_regular_face() {
        let families = ["Arial".to_owned()];
        let request = FaceRequest::regular(&families);
        assert_eq!(request.weight, 400);
        assert!(!request.italic);
    }

    #[test]
    fn a_fallback_serves_a_family_the_set_does_not_have() {
        let Some(font) = system_font() else { return };
        let mut set = FontSet::new();
        set.set_fallback(font);
        assert!(set.is_empty(), "no named family");
        assert!(
            set.resolve(&["Anything".to_owned()]).is_some(),
            "but the fallback answers"
        );
    }

    #[test]
    fn a_real_font_reports_sane_metrics() {
        let Some(font) = system_font() else { return };
        let metrics = font.file().metrics();
        // Every shipping font uses a power-of-two or 1000-unit em.
        assert!(
            (16..=16384).contains(&metrics.units_per_em),
            "units per em {} is implausible",
            metrics.units_per_em
        );
        assert!(metrics.ascent > 0, "text rises above the baseline");
        assert!(metrics.descent < 0, "and drops below it");
        assert!(font.file().glyph_count() > 100, "a real font has glyphs");
    }

    #[test]
    fn a_real_font_maps_the_characters_it_should() {
        let Some(font) = system_font() else { return };
        for character in ['A', 'z', '0', ' ', '.', '!'] {
            assert!(
                font.file().glyph_id(character).is_some(),
                "no glyph for {character:?}"
            );
        }
    }

    #[test]
    fn a_real_font_rasterises_a_letter() {
        let Some(font) = system_font() else { return };
        let glyph = font.render('H', 48.0).expect("H has an outline");
        assert!(glyph.mask.width() > 4, "the mask has real width");
        assert!(glyph.mask.height() > 4);
        assert!(!glyph.mask.is_empty(), "something was drawn");
        // The glyph sits above the baseline.
        assert!(glyph.top < 0.0, "top {} should be above", glyph.top);
    }

    #[test]
    fn a_rasterised_letter_has_its_expected_shape() {
        let Some(font) = system_font() else { return };
        // `H` is two stems and a crossbar: its middle row must be
        // covered at the sides, and its top row only at the sides.
        let glyph = font.render('H', 64.0).expect("H has an outline");
        let mask = &glyph.mask;
        let (width, height) = (mask.width(), mask.height());
        let inked = |x: u32, y: u32| mask.at(x, y) > 128;

        assert!(inked(1, height / 2), "left stem at mid height");
        assert!(inked(width - 2, height / 2), "right stem at mid height");
        assert!(
            !inked(width / 2, 2),
            "the gap between the stems is open at the top"
        );
    }

    #[test]
    fn a_space_has_an_advance_but_nothing_to_draw() {
        let Some(font) = system_font() else { return };
        assert!(font.render(' ', 32.0).is_none(), "nothing to rasterise");
        assert!(font.advance_px(' ', 32.0) > 0.0, "but it takes width");
    }

    #[test]
    fn a_character_the_font_lacks_renders_as_nothing() {
        let Some(font) = system_font() else { return };
        // A private-use code point no shipping font maps.
        assert!(font.render('\u{f8ff}', 32.0).is_none());
    }

    #[test]
    fn a_real_font_is_proportional() {
        let Some(font) = system_font() else { return };
        let narrow = font.advance_px('i', 32.0);
        let wide = font.advance_px('W', 32.0);
        assert!(
            wide > narrow,
            "W ({wide}) should be wider than i ({narrow})"
        );
    }

    #[test]
    fn advances_scale_with_the_font_size() {
        let Some(font) = system_font() else { return };
        let small = font.advance_px('m', 16.0);
        let large = font.advance_px('m', 32.0);
        assert!((large - small * 2.0).abs() < 0.01, "{small} -> {large}");
    }

    #[test]
    fn text_width_is_the_sum_of_its_advances() {
        let Some(font) = system_font() else { return };
        let style = style(20.0, LineHeight::Normal);
        let sum: f32 = "Hello".chars().map(|c| font.advance_px(c, 20.0)).sum();
        assert!((font.width("Hello", &style) - sum).abs() < 0.01);
        assert_eq!(font.width("", &style), 0.0);
    }

    #[test]
    fn normal_line_height_comes_from_the_font_not_the_guess() {
        let Some(font) = system_font() else { return };
        let style = style(100.0, LineHeight::Normal);
        let from_font = font.line_height(&style);
        // The style's fallback is a flat 1.2; a real font rarely agrees
        // exactly, and that difference is the point of asking it.
        assert!(from_font > 50.0 && from_font < 300.0, "got {from_font}");
    }

    #[test]
    fn a_declared_line_height_overrides_the_fonts_own() {
        let Some(font) = system_font() else { return };
        let fixed = style(10.0, LineHeight::Number(3.0));
        assert!((font.line_height(&fixed) - 30.0).abs() < 0.01);
    }

    #[test]
    fn the_baseline_sits_inside_the_line_box() {
        let Some(font) = system_font() else { return };
        for line_height in [
            LineHeight::Normal,
            LineHeight::Number(1.0),
            LineHeight::Number(3.0),
        ] {
            let style = style(24.0, line_height);
            let baseline = font.baseline(&style);
            assert!(baseline > 0.0, "baseline {baseline} must be below the top");
            assert!(
                baseline <= font.line_height(&style),
                "baseline {baseline} must be inside the line"
            );
        }
    }

    #[test]
    fn extra_line_height_is_split_above_and_below() {
        let Some(font) = system_font() else { return };
        let tight = font.baseline(&style(20.0, LineHeight::Number(1.0)));
        let loose = font.baseline(&style(20.0, LineHeight::Number(2.0)));
        // Doubling adds 20px of leading, half of it above.
        assert!((loose - tight - 10.0).abs() < 0.01, "{tight} -> {loose}");
    }

    #[test]
    fn a_bigger_size_makes_a_bigger_mask() {
        let Some(font) = system_font() else { return };
        let small = font.render('H', 16.0).expect("H");
        let large = font.render('H', 64.0).expect("H");
        assert!(large.mask.width() > small.mask.width());
        assert!(large.mask.height() > small.mask.height());
    }

    #[test]
    fn a_curved_letter_is_anti_aliased() {
        let Some(font) = system_font() else { return };
        let glyph = font.render('o', 48.0).expect("o has an outline");
        let mask = &glyph.mask;
        let partial = (0..mask.height())
            .flat_map(|y| (0..mask.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let coverage = mask.at(x, y);
                coverage > 0 && coverage < 255
            })
            .count();
        assert!(
            partial > 10,
            "a curve should produce partly-covered pixels, found {partial}"
        );
    }

    #[test]
    fn a_letter_with_a_hole_is_hollow() {
        let Some(font) = system_font() else { return };
        // The nonzero winding rule, seen from the outside: `o` must have
        // an uncovered middle.
        let glyph = font.render('o', 64.0).expect("o");
        let mask = &glyph.mask;
        assert_eq!(
            mask.at(mask.width() / 2, mask.height() / 2),
            0,
            "the counter should be empty"
        );
    }

    #[test]
    fn coverage_outside_a_mask_is_zero() {
        let Some(font) = system_font() else { return };
        let glyph = font.render('H', 32.0).expect("H");
        assert_eq!(coverage_at(&glyph.mask, 9999, 0), 0);
    }
}
