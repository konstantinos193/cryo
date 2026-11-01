//! Glyphs: the port that turns characters into coverage, plus the
//! fonts that satisfy it.
//!
//! Two implementations live behind [`GlyphSource`]:
//! [`outline_font::OutlineFont`], which reads a real TrueType file, and
//! [`BuiltinFont`], a 5x7 bitmap fallback for when no system font can
//! be found. Both return a finished coverage mask, so the rasteriser
//! never learns which kind it was given.
//!
//! [`BuiltinFont`] implements **both** [`GlyphSource`] and
//! [`TextMeasure`](super::super::layout::measure::TextMeasure), and that
//! is the point of the design. Layout measures text and paint draws it;
//! if those two disagree about how wide a character is, text overflows
//! its own box or stops short of it, and the bug looks like a layout
//! fault rather than a measurement one. Making one object answer both
//! questions means they cannot disagree.
//!
//! The font itself is a 5x7 bitmap in a 9-unit em — seven units above
//! the baseline and two below, so that `g`, `p` and `y` have somewhere
//! to descend to. It covers printable ASCII. It is a fallback, not a typeface: no kerning, no hinting, no
//! anti-aliasing, one weight, and nothing outside ASCII. Scaled up it
//! is visibly blocky — at 16px each design pixel is a 2x2 block. What it
//! buys is that the whole pipeline can be looked at, and that the glyph
//! seam is exercised by real data rather than by a stub.
//!
//! When a real font backend lands it implements these same two traits
//! and nothing above here changes.

pub mod glyf;
pub mod outline_font;
pub mod sfnt;
pub mod system;

use crate::engine::cryo_impl::scan::{self, Mask};
use outline_font::FaceRequest;

use crate::engine::cryo_impl::css::computed::ComputedStyle;

/// What can go wrong reading a font.
///
/// Font bytes come from the system and, later, from the web, so every
/// variant here is a case that must produce an error rather than a
/// panic. "Unsupported" and "corrupt" are kept apart deliberately: the
/// first is a font we could read with more code, the second is one
/// nobody should.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum FontError {
    #[error("the font file could not be read")]
    Unreadable,
    #[error("the file is too short to be a font")]
    Truncated,
    #[error("not a font: unrecognised version {0:#010x}")]
    NotAFont(u32),
    #[error("OpenType/CFF outlines are not supported")]
    CffOutlines,
    #[error("font collections are not supported")]
    Collection,
    #[error("the font is missing the {0} table")]
    MissingTable(String),
    #[error("the {0} table is malformed")]
    Malformed(&'static str),
    #[error("the font has no usable character map")]
    NoCharacterMap,
    #[error("character map format {0} is not supported")]
    UnsupportedCharacterMap(u16),
}

impl FontError {
    fn malformed(table: &'static str) -> Self {
        Self::Malformed(table)
    }
}

use super::super::layout::measure::TextMeasure;

/// One glyph's coverage: 9 rows, each with [`Glyph::COLUMNS`] bits, the
/// most significant used bit leftmost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub rows: [u8; Glyph::ROWS],
}

impl Glyph {
    pub const ROWS: usize = 9;
    pub const COLUMNS: usize = 5;

    /// Whether the pixel at `(column, row)` is covered.
    pub fn covers(&self, column: usize, row: usize) -> bool {
        if column >= Self::COLUMNS || row >= Self::ROWS {
            return false;
        }
        // Bit `COLUMNS - 1` is the leftmost column.
        let bit = Self::COLUMNS - 1 - column;
        self.rows[row] & (1 << bit) != 0
    }

    /// Whether this glyph draws nothing — a space, or a blank cell.
    pub fn is_blank(&self) -> bool {
        self.rows.iter().all(|&row| row == 0)
    }
}

/// Where glyph coverage comes from.
pub trait GlyphSource {
    /// Rasterise one character at `font_size`, or `None` if this font
    /// has no glyph for it.
    ///
    /// Returning a finished mask rather than an outline is what lets a
    /// bitmap font and an outline font satisfy the same trait: the
    /// difference between them is entirely inside this call.
    fn render(&self, character: char, font_size: f32) -> Option<RenderedGlyph>;

    /// How far the pen moves past this character, in pixels.
    ///
    /// Asked even for a character with no glyph, so that a missing
    /// glyph leaves a gap rather than shifting the rest of the line out
    /// from under the width layout measured.
    fn advance_px(&self, character: char, font_size: f32) -> f32;

    /// Whether this font has a glyph for `character`.
    ///
    /// A page's webfont is usually shipped cut into subsets by
    /// alphabet, so one face covering the first letter of a run says
    /// nothing about the rest of it. The caller uses this to break a
    /// run where the face runs out. A font that answers for
    /// everything says so by not overriding this.
    fn covers(&self, character: char) -> bool {
        let _ = character;
        true
    }

    /// The weight this font actually draws at, which is not always
    /// the weight that was asked for.
    ///
    /// A page may declare one variable file for every weight it uses,
    /// and this engine draws that file's default instance. The
    /// shortfall is what the rasteriser thickens to cover, so it has
    /// to be askable. Four hundred is the honest answer for a font
    /// that has no opinion.
    fn weight(&self) -> u16 {
        400
    }
}

/// How much of `text` this face can draw, from the start.
///
/// A page's webfont arrives cut into subsets by alphabet, all under
/// one family name, so one face rarely covers a whole run. Both
/// measuring and drawing have to break the run at the same places —
/// if they disagree, layout reserves one width and paint uses
/// another, and the line develops gaps that no style asked for.
/// That is why this lives here rather than beside either of them.
///
/// Always at least one character, so a face that can draw nothing
/// still makes progress. Whitespace belongs to whichever face is
/// current: it has no shape to miss.
pub fn covered_prefix<'a>(text: &'a str, font: &dyn GlyphSource) -> &'a str {
    let mut end = text.len();
    for (at, character) in text.char_indices() {
        if at > 0 && !character.is_whitespace() && !font.covers(character) {
            end = at;
            break;
        }
    }
    &text[..end]
}

/// Picks the font for one text run's family stack.
///
/// Separate from [`GlyphSource`] because they answer different
/// questions: a glyph source *is* a font, while this *chooses* one. A
/// page with a serif heading and a monospace code block needs two, and
/// the rasteriser should not have to know that — it asks for the font
/// this run wants and draws with whatever it gets.
pub trait FontChoice {
    /// The face for `request`, or `None` if nothing can serve it.
    fn choose(&self, request: FaceRequest<'_>) -> Option<&dyn GlyphSource>;
}

/// A glyph rasterised at a particular size, ready to be composited.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedGlyph {
    pub mask: Mask,
    /// The mask's left edge, relative to the pen position.
    ///
    /// Often negative: a letter like `j` leans left of where the pen
    /// stands.
    pub left: f32,
    /// The mask's top edge, relative to the **baseline**. Negative,
    /// because glyphs rise above it.
    pub top: f32,
}

/// A font with no glyphs at all.
///
/// For a caller that has no font yet: text still measures and lays out,
/// and the rasteriser reports every character it could not draw instead
/// of quietly producing a blank page.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGlyphs;

/// Measures identically to [`BuiltinFont`] and draws nothing.
///
/// Deliberately the same metrics: swapping this in must change only
/// what appears, never where anything is, so a page with no font has the
/// same layout as one with it.
impl TextMeasure for NoGlyphs {
    fn width(&self, text: &str, style: &ComputedStyle) -> f32 {
        BuiltinFont.width(text, style)
    }

    fn baseline(&self, style: &ComputedStyle) -> f32 {
        BuiltinFont.baseline(style)
    }
}

impl GlyphSource for NoGlyphs {
    fn render(&self, _character: char, _font_size: f32) -> Option<RenderedGlyph> {
        None
    }

    fn advance_px(&self, character: char, font_size: f32) -> f32 {
        BuiltinFont.advance_px(character, font_size)
    }
}

/// A single font serves every family asked of it.
impl FontChoice for NoGlyphs {
    fn choose(&self, _request: FaceRequest<'_>) -> Option<&dyn GlyphSource> {
        Some(self)
    }
}

impl FontChoice for BuiltinFont {
    fn choose(&self, _request: FaceRequest<'_>) -> Option<&dyn GlyphSource> {
        Some(self)
    }
}

/// The built-in 5x7 bitmap font.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinFont;

impl BuiltinFont {
    /// Design units per em: seven above the baseline and two below.
    ///
    /// Those two are what make a lowercase `g` read as a `g` rather
    /// than a `9`. With the baseline at the foot of the cell there is
    /// nowhere for a descender to go, and every tailed letter flattens
    /// into its nearest capital.
    pub const EM: f32 = 9.0;
    /// The baseline, from the top of the cell. Rows 0..=6 sit above it,
    /// rows 7 and 8 below.
    pub const ASCENT: f32 = 7.0;
    /// 5 columns plus one of spacing.
    pub const ADVANCE: f32 = 6.0;

    /// The pixel size of one design unit at this font size.
    pub fn scale(font_size: f32) -> f32 {
        font_size / Self::EM
    }
}

impl BuiltinFont {
    /// The bitmap for a character, before scaling.
    pub fn glyph(&self, character: char) -> Option<Glyph> {
        GLYPHS
            .iter()
            .find(|(candidate, _)| *candidate == character)
            .map(|(_, rows)| Glyph { rows: *rows })
    }
}

impl GlyphSource for BuiltinFont {
    /// Blow each design pixel up into a scaled square.
    ///
    /// No anti-aliasing: a bitmap font has no sub-pixel information to
    /// recover, and smoothing it would only blur hard edges that were
    /// drawn hard on purpose.
    fn render(&self, character: char, font_size: f32) -> Option<RenderedGlyph> {
        let glyph = self.glyph(character)?;
        if glyph.is_blank() {
            return None;
        }
        let scale = Self::scale(font_size);
        let width = (Glyph::COLUMNS as f32 * scale).ceil().max(1.0);
        let height = (Glyph::ROWS as f32 * scale).ceil().max(1.0);

        let mut cells = Vec::new();
        for row in 0..Glyph::ROWS {
            for column in 0..Glyph::COLUMNS {
                if glyph.covers(column, row) {
                    cells.push(vec![
                        (column as f32 * scale, row as f32 * scale),
                        ((column + 1) as f32 * scale, row as f32 * scale),
                        ((column + 1) as f32 * scale, (row + 1) as f32 * scale),
                        (column as f32 * scale, (row + 1) as f32 * scale),
                    ]);
                }
            }
        }
        Some(RenderedGlyph {
            mask: scan::fill(&cells, width as u32, height as u32),
            left: 0.0,
            top: -Self::ASCENT * scale,
        })
    }

    fn advance_px(&self, _character: char, font_size: f32) -> f32 {
        // Monospaced: every glyph advances the same.
        Self::ADVANCE * Self::scale(font_size)
    }
}

impl TextMeasure for BuiltinFont {
    fn width(&self, text: &str, style: &ComputedStyle) -> f32 {
        text.chars()
            .map(|character| self.advance_px(character, style.font_size))
            .sum()
    }

    /// The baseline from the top of the line box: the half-leading above
    /// the text, plus the font's own ascent.
    fn baseline(&self, style: &ComputedStyle) -> f32 {
        let line = self.line_height(style);
        let leading = (line - style.font_size) / 2.0;
        leading + Self::ASCENT * Self::scale(style.font_size)
    }
}

/// Printable ASCII as 5x7 bitmaps, most significant bit leftmost.
///
/// Written in binary so a wrong pixel is visible in review rather than
/// hidden in hex.
#[rustfmt::skip]
static GLYPHS: &[(char, [u8; Glyph::ROWS])] = &[
    (' ',   [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000]),
    ('!',   [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100, 0b00000, 0b00000]),
    ('"',   [0b01010, 0b01010, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000]),
    ('#',   [0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010, 0b00000, 0b00000]),
    ('$',   [0b00100, 0b01111, 0b10100, 0b01110, 0b00101, 0b11110, 0b00100, 0b00000, 0b00000]),
    ('%',   [0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011, 0b00000, 0b00000]),
    ('&',   [0b01100, 0b10010, 0b10100, 0b01000, 0b10101, 0b10010, 0b01101, 0b00000, 0b00000]),
    ('\'',  [0b00100, 0b00100, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000]),
    ('(',   [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010, 0b00000, 0b00000]),
    (')',   [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000, 0b00000, 0b00000]),
    ('*',   [0b00000, 0b00100, 0b10101, 0b01110, 0b10101, 0b00100, 0b00000, 0b00000, 0b00000]),
    ('+',   [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000, 0b00000, 0b00000]),
    (',',   [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b00100, 0b01000]),
    ('-',   [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000]),
    ('.',   [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100, 0b00000, 0b00000]),
    ('/',   [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000, 0b00000, 0b00000]),
    ('0',   [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('1',   [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110, 0b00000, 0b00000]),
    ('2',   [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111, 0b00000, 0b00000]),
    ('3',   [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('4',   [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010, 0b00000, 0b00000]),
    ('5',   [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('6',   [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('7',   [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00000, 0b00000]),
    ('8',   [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('9',   [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100, 0b00000, 0b00000]),
    (':',   [0b00000, 0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000, 0b00000]),
    (';',   [0b00000, 0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b00100, 0b01000, 0b00000]),
    ('<',   [0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010, 0b00000, 0b00000]),
    ('=',   [0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000, 0b00000]),
    ('>',   [0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000, 0b00000, 0b00000]),
    ('?',   [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100, 0b00000, 0b00000]),
    ('@',   [0b01110, 0b10001, 0b00001, 0b01101, 0b10101, 0b10101, 0b01110, 0b00000, 0b00000]),
    ('A',   [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001, 0b00000, 0b00000]),
    ('B',   [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110, 0b00000, 0b00000]),
    ('C',   [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('D',   [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110, 0b00000, 0b00000]),
    ('E',   [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111, 0b00000, 0b00000]),
    ('F',   [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000, 0b00000, 0b00000]),
    ('G',   [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111, 0b00000, 0b00000]),
    ('H',   [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001, 0b00000, 0b00000]),
    ('I',   [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110, 0b00000, 0b00000]),
    ('J',   [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100, 0b00000, 0b00000]),
    ('K',   [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001, 0b00000, 0b00000]),
    ('L',   [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111, 0b00000, 0b00000]),
    ('M',   [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001, 0b00000, 0b00000]),
    ('N',   [0b10001, 0b11001, 0b11001, 0b10101, 0b10011, 0b10011, 0b10001, 0b00000, 0b00000]),
    ('O',   [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('P',   [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000, 0b00000, 0b00000]),
    ('Q',   [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10011, 0b01101, 0b00000, 0b00000]),
    ('R',   [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001, 0b00000, 0b00000]),
    ('S',   [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110, 0b00000, 0b00000]),
    ('T',   [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00000]),
    ('U',   [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('V',   [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100, 0b00000, 0b00000]),
    ('W',   [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001, 0b00000, 0b00000]),
    ('X',   [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001, 0b00000, 0b00000]),
    ('Y',   [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00000]),
    ('Z',   [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111, 0b00000, 0b00000]),
    ('[',   [0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110, 0b00000, 0b00000]),
    ('\\',  [0b10000, 0b01000, 0b01000, 0b00100, 0b00010, 0b00010, 0b00001, 0b00000, 0b00000]),
    (']',   [0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110, 0b00000, 0b00000]),
    ('^',   [0b00100, 0b01010, 0b10001, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000]),
    ('_',   [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111]),
    ('`',   [0b01000, 0b00100, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000]),
    ('a',   [0b00000, 0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111, 0b00000, 0b00000]),
    ('b',   [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b11110, 0b00000, 0b00000]),
    ('c',   [0b00000, 0b00000, 0b01110, 0b10001, 0b10000, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('d',   [0b00001, 0b00001, 0b01111, 0b10001, 0b10001, 0b10001, 0b01111, 0b00000, 0b00000]),
    ('e',   [0b00000, 0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110, 0b00000, 0b00000]),
    ('f',   [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000, 0b00000, 0b00000]),
    ('g',   [0b00000, 0b00000, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110]),
    ('h',   [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b00000, 0b00000]),
    ('i',   [0b00100, 0b00000, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110, 0b00000, 0b00000]),
    ('j',   [0b00010, 0b00000, 0b00110, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100]),
    ('k',   [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b00000, 0b00000]),
    ('l',   [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110, 0b00000, 0b00000]),
    ('m',   [0b00000, 0b00000, 0b11010, 0b10101, 0b10101, 0b10101, 0b10101, 0b00000, 0b00000]),
    ('n',   [0b00000, 0b00000, 0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b00000, 0b00000]),
    ('o',   [0b00000, 0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110, 0b00000, 0b00000]),
    ('p',   [0b00000, 0b00000, 0b11110, 0b10001, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000]),
    ('q',   [0b00000, 0b00000, 0b01111, 0b10001, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001]),
    ('r',   [0b00000, 0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000, 0b00000, 0b00000]),
    ('s',   [0b00000, 0b00000, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110, 0b00000, 0b00000]),
    ('t',   [0b01000, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110, 0b00000, 0b00000]),
    ('u',   [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101, 0b00000, 0b00000]),
    ('v',   [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100, 0b00000, 0b00000]),
    ('w',   [0b00000, 0b00000, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010, 0b00000, 0b00000]),
    ('x',   [0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b00000, 0b00000]),
    ('y',   [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110]),
    ('z',   [0b00000, 0b00000, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111, 0b00000, 0b00000]),
    ('{',   [0b00110, 0b01000, 0b01000, 0b11000, 0b01000, 0b01000, 0b00110, 0b00000, 0b00000]),
    ('|',   [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00000]),
    ('}',   [0b01100, 0b00010, 0b00010, 0b00011, 0b00010, 0b00010, 0b01100, 0b00000, 0b00000]),
    ('~',   [0b00000, 0b00000, 0b01000, 0b10101, 0b00010, 0b00000, 0b00000, 0b00000, 0b00000]),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A font that draws only the characters it was told to.
    struct Subset(&'static str);

    impl GlyphSource for Subset {
        fn covers(&self, character: char) -> bool {
            self.0.contains(character)
        }

        fn render(&self, character: char, font_size: f32) -> Option<RenderedGlyph> {
            self.covers(character)
                .then(|| BuiltinFont.render(character, font_size))
                .flatten()
        }

        fn advance_px(&self, character: char, font_size: f32) -> f32 {
            BuiltinFont.advance_px(character, font_size)
        }
    }

    #[test]
    fn a_run_breaks_where_its_face_runs_out() {
        // The webfont-subset case: one face holds the capital and
        // another the rest of the word.
        assert_eq!(covered_prefix("Agents", &Subset("A")), "A");
        assert_eq!(covered_prefix("gents", &Subset("gents")), "gents");
    }

    #[test]
    fn whitespace_belongs_to_whichever_face_is_drawing() {
        // A space has no shape to miss, so it must not split a run.
        assert_eq!(covered_prefix("ab ab", &Subset("ab")), "ab ab");
    }

    #[test]
    fn a_face_that_covers_nothing_still_makes_progress() {
        // Otherwise the run would never advance and the loop that
        // uses this would not end.
        assert_eq!(covered_prefix("xyz", &Subset("")), "x");
    }

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

    /// A glyph drawn as text, for assertions that read like the shape.
    fn draw(character: char) -> Vec<String> {
        let glyph = BuiltinFont.glyph(character).expect("the glyph exists");
        (0..Glyph::ROWS)
            .map(|row| {
                (0..Glyph::COLUMNS)
                    .map(|column| if glyph.covers(column, row) { '#' } else { '.' })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn every_printable_ascii_character_has_a_glyph() {
        for code in 0x20u8..=0x7e {
            let character = char::from(code);
            assert!(
                BuiltinFont.glyph(character).is_some(),
                "no glyph for {character:?}"
            );
        }
    }

    #[test]
    fn the_table_has_no_duplicate_or_out_of_order_entries() {
        let characters: Vec<char> = GLYPHS.iter().map(|(c, _)| *c).collect();
        let mut sorted = characters.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            characters.len(),
            "a character is listed twice"
        );
        assert_eq!(characters, sorted, "the table is kept in code order");
        assert_eq!(characters.len(), 95, "0x20..=0x7e inclusive");
    }

    #[test]
    fn a_character_outside_the_font_has_no_glyph() {
        assert!(BuiltinFont.glyph('é').is_none());
        assert!(BuiltinFont.glyph('日').is_none());
        assert!(BuiltinFont.glyph('\n').is_none());
    }

    #[test]
    fn a_space_is_blank_and_a_letter_is_not() {
        assert!(BuiltinFont.glyph(' ').expect("space").is_blank());
        assert!(!BuiltinFont.glyph('A').expect("A").is_blank());
    }

    #[test]
    fn the_capital_a_has_its_crossbar_and_open_apex() {
        assert_eq!(
            draw('A'),
            [".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#", ".....", ".....",]
        );
    }

    #[test]
    fn the_capital_l_is_a_corner() {
        assert_eq!(
            draw('L'),
            ["#....", "#....", "#....", "#....", "#....", "#....", "#####", ".....", ".....",]
        );
    }

    #[test]
    fn a_hyphen_draws_only_a_middle_row() {
        let rows = draw('-');
        assert_eq!(rows[3], "#####");
        for (index, row) in rows.iter().enumerate() {
            if index != 3 {
                assert_eq!(row, ".....", "row {index} should be empty");
            }
        }
    }

    #[test]
    fn a_full_stop_sits_at_the_bottom() {
        let rows = draw('.');
        assert_eq!(rows[5], ".##..");
        assert_eq!(rows[6], ".##..");
        assert_eq!(rows[0], ".....");
    }

    #[test]
    fn the_letters_x_and_i_are_horizontally_symmetrical() {
        for character in ['X', 'I', 'A', 'T', 'O', 'W'] {
            let glyph = BuiltinFont.glyph(character).expect("the glyph");
            for row in 0..Glyph::ROWS {
                for column in 0..Glyph::COLUMNS {
                    let mirrored = Glyph::COLUMNS - 1 - column;
                    assert_eq!(
                        glyph.covers(column, row),
                        glyph.covers(mirrored, row),
                        "{character:?} row {row} is not symmetrical"
                    );
                }
            }
        }
    }

    #[test]
    fn a_descender_uses_the_bottom_row_and_a_plain_letter_does_not() {
        // Rows 7 and 8 are below the baseline. `o` sits entirely above
        // it; `p` puts its stem down there, which is the whole reason
        // the cell is taller than the ascent.
        assert!(draw('o')[7..].iter().all(|row| row == "....."));
        assert_eq!(draw('p')[7], "#....");
        assert_eq!(draw('p')[8], "#....");
        for tailed in ['g', 'p', 'q', 'y', 'j'] {
            assert!(
                draw(tailed)[7..].iter().any(|row| row.contains('#')),
                "{tailed:?} should descend"
            );
        }
    }

    #[test]
    fn coverage_outside_the_cell_is_false() {
        let glyph = BuiltinFont.glyph('#').expect("the glyph");
        assert!(!glyph.covers(Glyph::COLUMNS, 0));
        assert!(!glyph.covers(0, Glyph::ROWS));
        assert!(!glyph.covers(99, 99));
    }

    #[test]
    fn width_is_the_advance_times_the_character_count() {
        // 9-unit em, 6-unit advance: at 9px a character is 6px wide.
        let style = with_font(9.0, LineHeight::Normal);
        assert!(close(BuiltinFont.width("abc", &style), 18.0));
        assert!(close(BuiltinFont.width("", &style), 0.0));
    }

    #[test]
    fn width_scales_with_the_font_size() {
        assert!(close(
            BuiltinFont.width("a", &with_font(9.0, LineHeight::Normal)),
            6.0
        ));
        assert!(close(
            BuiltinFont.width("a", &with_font(36.0, LineHeight::Normal)),
            24.0
        ));
    }

    #[test]
    fn every_glyph_advances_the_same_because_it_is_monospaced() {
        assert!(close(
            BuiltinFont.advance_px('i', 9.0),
            BuiltinFont.advance_px('W', 9.0)
        ));
    }

    #[test]
    fn rendering_a_letter_produces_a_covered_mask() {
        let glyph = BuiltinFont.render('L', 9.0).expect("L has a bitmap");
        assert_eq!(glyph.mask.width(), Glyph::COLUMNS as u32);
        assert_eq!(glyph.mask.height(), Glyph::ROWS as u32);
        assert!(!glyph.mask.is_empty());
        // `L` is a stem down the left and a foot along the bottom.
        assert_eq!(glyph.mask.at(0, 0), 255, "the stem");
        assert_eq!(glyph.mask.at(4, 0), 0, "nothing to its right");
        assert_eq!(glyph.mask.at(4, 6), 255, "the end of the foot");
    }

    #[test]
    fn a_rendered_glyph_sits_above_the_baseline() {
        let glyph = BuiltinFont.render('L', 9.0).expect("L");
        assert!(close(glyph.top, -7.0), "the ascent, upward");
        assert!(close(glyph.left, 0.0));
    }

    #[test]
    fn a_blank_glyph_renders_as_nothing() {
        assert!(BuiltinFont.render(' ', 9.0).is_none());
    }

    #[test]
    fn a_character_outside_the_font_renders_as_nothing() {
        assert!(BuiltinFont.render('é', 9.0).is_none());
    }

    #[test]
    fn a_larger_size_makes_a_larger_mask() {
        let small = BuiltinFont.render('L', 9.0).expect("L");
        let large = BuiltinFont.render('L', 18.0).expect("L");
        assert_eq!(large.mask.width(), small.mask.width() * 2);
        assert_eq!(large.mask.height(), small.mask.height() * 2);
    }

    #[test]
    fn the_baseline_is_the_ascent_plus_the_half_leading() {
        // At 8px with line-height 1 there is no leading, so the baseline
        // is the ascent exactly.
        let tight = with_font(9.0, LineHeight::Number(1.0));
        assert!(close(BuiltinFont.baseline(&tight), 7.0));

        // Doubling the line height adds half the extra above.
        let loose = with_font(9.0, LineHeight::Number(2.0));
        assert!(close(BuiltinFont.baseline(&loose), 11.5));
    }

    #[test]
    fn the_baseline_leaves_room_for_descenders() {
        let style = with_font(16.0, LineHeight::Number(1.0));
        let baseline = BuiltinFont.baseline(&style);
        let line = BuiltinFont.line_height(&style);
        assert!(baseline < line, "something must hang below the baseline");
    }

    #[test]
    fn one_design_unit_scales_to_the_expected_pixel_size() {
        assert!(close(BuiltinFont::scale(9.0), 1.0));
        assert!(close(BuiltinFont::scale(18.0), 2.0));
        assert!(close(BuiltinFont::scale(4.5), 0.5));
    }

    #[test]
    fn the_empty_font_measures_but_draws_nothing() {
        let style = with_font(9.0, LineHeight::Normal);
        // Text still occupies space, so layout is unaffected...
        assert!(close(NoGlyphs.width("abc", &style), 18.0));
        // ...but nothing can be drawn.
        assert!(NoGlyphs.render('a', 9.0).is_none());
        // And the advance matches, so swapping it in moves nothing.
        assert!(close(
            NoGlyphs.advance_px('a', 9.0),
            BuiltinFont.advance_px('a', 9.0)
        ));
    }
}
