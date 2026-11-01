//! Reading a TrueType font file: the table directory, the metrics
//! tables, and the character map.
//!
//! A font file is a directory of named tables at byte offsets, each with
//! its own format, all big-endian. Nothing is self-describing and
//! nothing bounds-checks itself, so every read here goes through helpers
//! that return `Option` — a truncated or hostile font must produce an
//! error, never a panic or a read past the buffer. Fonts come from the
//! system and, eventually, from the web.
//!
//! The tables this reads:
//!
//! | table  | what it answers                               |
//! |--------|-----------------------------------------------|
//! | `head` | units per em, and how `loca` is encoded        |
//! | `maxp` | how many glyphs there are                      |
//! | `hhea` | ascent, descent, line gap                      |
//! | `hmtx` | each glyph's advance width                     |
//! | `cmap` | which glyph a character maps to                |
//!
//! Outlines live in `glyf`/`loca` and are read by [`super::glyf`].
//!
//! **Only TrueType outlines are supported.** An OpenType font with CFF
//! outlines (`OTTO`) is rejected with a clear error rather than being
//! half-read: its glyph data is a different format entirely, and
//! pretending otherwise would produce silently empty glyphs.

use std::collections::BTreeMap;

use super::FontError;

/// A four-byte table name, as it appears in the file.
pub type Tag = [u8; 4];

/// Where a table sits in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableRange {
    pub offset: usize,
    pub length: usize,
}

/// The horizontal metrics a font declares, in font design units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontMetrics {
    /// Design units per em — the scale everything else is in. Commonly
    /// 1000 or 2048, and never assumed.
    pub units_per_em: u16,
    /// How far glyphs rise above the baseline.
    pub ascent: i16,
    /// How far they drop below it. Negative in the file; kept as-is so
    /// the sign is the font's, not ours.
    pub descent: i16,
    /// Extra space between lines, on top of ascent plus descent.
    pub line_gap: i16,
}

impl FontMetrics {
    /// The natural line height: the distance between baselines when no
    /// `line-height` overrides it.
    pub fn line_spacing(&self) -> f32 {
        f32::from(self.ascent) - f32::from(self.descent) + f32::from(self.line_gap)
    }
}

/// Which face of a family a file holds.
///
/// Read from the font, never guessed from its filename. The naming
/// conventions differ per vendor (`arialbd`, `segoeuib`, `consolab`),
/// so a filename is at best a hint about which files to open — the
/// `OS/2` table is what actually says "this is the bold one".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaceStyle {
    /// 100..=900, where 400 is regular and 700 is bold.
    pub weight: u16,
    pub italic: bool,
}

impl FaceStyle {
    pub const REGULAR: Self = Self {
        weight: 400,
        italic: false,
    };

    /// Read the face's own description.
    ///
    /// `OS/2` is authoritative and present in every modern font; `head`
    /// carries two older flags that are the only answer in some very
    /// old files, so they are the fallback rather than the first choice.
    fn parse(data: &[u8], tables: &BTreeMap<Tag, TableRange>) -> Self {
        if let Some(os2) = tables.get(b"OS/2") {
            if let (Some(weight), Some(selection)) =
                (u16_at(data, os2.offset + 4), u16_at(data, os2.offset + 62))
            {
                // Some fonts ship a weight of 0 or a wild value; clamp
                // rather than trust it, since selection sorts on this.
                let weight = if (1..=1000).contains(&weight) {
                    weight
                } else {
                    400
                };
                return Self {
                    weight,
                    italic: selection & 0x0001 != 0,
                };
            }
        }
        // `head.macStyle`: bit 0 bold, bit 1 italic.
        let mac_style = tables
            .get(b"head")
            .and_then(|head| u16_at(data, head.offset + 44))
            .unwrap_or(0);
        Self {
            weight: if mac_style & 0x0001 != 0 { 700 } else { 400 },
            italic: mac_style & 0x0002 != 0,
        }
    }
}

/// A parsed font file. Holds the bytes and the table directory; the
/// per-glyph data is read on demand.
#[derive(Clone, Debug)]
pub struct FontFile {
    data: Vec<u8>,
    tables: BTreeMap<Tag, TableRange>,
    metrics: FontMetrics,
    glyph_count: u16,
    /// `hhea`'s `numberOfHMetrics`: how many glyphs have their own
    /// advance before the trailing run that all share the last one.
    metric_count: u16,
    /// `head`'s `indexToLocFormat`: whether `loca` holds 16- or 32-bit
    /// offsets.
    long_loca: bool,
    character_map: CharacterMap,
    style: FaceStyle,
}

impl FontFile {
    /// Parse a font from its bytes.
    pub fn parse(data: Vec<u8>) -> Result<Self, FontError> {
        let version = u32_at(&data, 0).ok_or(FontError::Truncated)?;
        match version {
            // 'OTTO': OpenType with CFF outlines.
            0x4f54_544f => return Err(FontError::CffOutlines),
            // A TrueType collection: several fonts in one file.
            0x7474_6366 => return Err(FontError::Collection),
            0x0001_0000 | 0x7472_7565 => {}
            other => return Err(FontError::NotAFont(other)),
        }

        let tables = read_directory(&data)?;
        let head = table(&tables, b"head")?;
        let maxp = table(&tables, b"maxp")?;
        let hhea = table(&tables, b"hhea")?;

        let units_per_em = u16_at(&data, head.offset + 18).ok_or(FontError::malformed("head"))?;
        if units_per_em == 0 {
            // Everything scales by this; zero would make every glyph
            // infinitely large.
            return Err(FontError::malformed("head"));
        }
        let index_to_loc = i16_at(&data, head.offset + 50).ok_or(FontError::malformed("head"))?;

        let metrics = FontMetrics {
            units_per_em,
            ascent: i16_at(&data, hhea.offset + 4).ok_or(FontError::malformed("hhea"))?,
            descent: i16_at(&data, hhea.offset + 6).ok_or(FontError::malformed("hhea"))?,
            line_gap: i16_at(&data, hhea.offset + 8).ok_or(FontError::malformed("hhea"))?,
        };
        let metric_count = u16_at(&data, hhea.offset + 34).ok_or(FontError::malformed("hhea"))?;
        let glyph_count = u16_at(&data, maxp.offset + 4).ok_or(FontError::malformed("maxp"))?;

        let cmap = table(&tables, b"cmap")?;
        let character_map = CharacterMap::parse(&data, cmap)?;
        // Both borrow `data`, so they are read before it moves below.
        let style = FaceStyle::parse(&data, &tables);

        Ok(Self {
            data,
            tables,
            metrics,
            glyph_count,
            metric_count,
            long_loca: index_to_loc != 0,
            character_map,
            style,
        })
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub const fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    pub const fn glyph_count(&self) -> u16 {
        self.glyph_count
    }

    /// Which face of its family this file holds.
    pub const fn style(&self) -> FaceStyle {
        self.style
    }

    pub const fn has_long_loca(&self) -> bool {
        self.long_loca
    }

    /// A table's position, or `None` if the font lacks it.
    pub fn table(&self, tag: &Tag) -> Option<TableRange> {
        self.tables.get(tag).copied()
    }

    /// The glyph for `character`, or `None` when the font has none.
    ///
    /// Glyph 0 is `.notdef` — the empty box fonts draw for an unmapped
    /// character — and is reported as `None` rather than returned, so a
    /// caller can fall back to another font instead of drawing boxes.
    pub fn glyph_id(&self, character: char) -> Option<u16> {
        match self.character_map.lookup(character) {
            0 => None,
            id if id < self.glyph_count => Some(id),
            // A map pointing past the glyph store is a corrupt font.
            _ => None,
        }
    }

    /// A glyph's advance width in design units.
    ///
    /// The last entry in `hmtx` covers every glyph after it — that is
    /// how monospaced fonts store one advance for thousands of glyphs —
    /// so a glyph past the metric count takes the final value.
    pub fn advance(&self, glyph: u16) -> u16 {
        let Some(hmtx) = self.tables.get(b"hmtx") else {
            return 0;
        };
        if self.metric_count == 0 {
            return 0;
        }
        let index = glyph.min(self.metric_count - 1) as usize;
        u16_at(&self.data, hmtx.offset + index * 4).unwrap_or(0)
    }
}

/// Read the table directory that follows the file header.
fn read_directory(data: &[u8]) -> Result<BTreeMap<Tag, TableRange>, FontError> {
    let count = u16_at(data, 4).ok_or(FontError::Truncated)?;
    let mut tables = BTreeMap::new();
    for index in 0..count as usize {
        // 12-byte header, then 16 bytes per entry.
        let entry = 12 + index * 16;
        let tag: Tag = data
            .get(entry..entry + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(FontError::Truncated)?;
        let offset = u32_at(data, entry + 8).ok_or(FontError::Truncated)? as usize;
        let length = u32_at(data, entry + 12).ok_or(FontError::Truncated)? as usize;
        // A table claiming to run past the file is corrupt; rejecting it
        // here means no later read has to re-check.
        if offset > data.len() || offset + length > data.len() {
            return Err(FontError::malformed("directory"));
        }
        tables.insert(tag, TableRange { offset, length });
    }
    Ok(tables)
}

fn table(tables: &BTreeMap<Tag, TableRange>, tag: &Tag) -> Result<TableRange, FontError> {
    tables
        .get(tag)
        .copied()
        .ok_or_else(|| FontError::MissingTable(String::from_utf8_lossy(tag).into_owned()))
}

/// The character-to-glyph map, in whichever format the font uses.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CharacterMap {
    /// Format 4: segmented, covering the Basic Multilingual Plane. The
    /// format nearly every font ships.
    Segmented {
        /// `(end, start, delta, range_offset, range_offset_position)`
        /// per segment, kept resolved so lookup does no arithmetic on
        /// raw table offsets.
        segments: Vec<Segment>,
        /// The glyph id array that `range_offset` points into.
        glyph_ids: Vec<u16>,
        /// Where the glyph id array begins, in table-relative terms.
        glyph_id_base: usize,
    },
    /// Format 12: flat ranges, covering every plane. Needed for emoji
    /// and for CJK fonts beyond the BMP.
    Groups(Vec<Group>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Segment {
    end: u16,
    start: u16,
    delta: i16,
    range_offset: u16,
    /// The byte position of this segment's `range_offset` field, which
    /// the offset is measured from — the format's one real oddity.
    range_offset_position: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Group {
    start: u32,
    end: u32,
    start_glyph: u32,
}

impl CharacterMap {
    fn parse(data: &[u8], cmap: TableRange) -> Result<Self, FontError> {
        let subtable = best_subtable(data, cmap).ok_or(FontError::NoCharacterMap)?;
        let format = u16_at(data, subtable).ok_or(FontError::malformed("cmap"))?;
        match format {
            4 => Self::parse_segmented(data, subtable),
            12 => Self::parse_groups(data, subtable),
            _ => Err(FontError::UnsupportedCharacterMap(format)),
        }
    }

    fn parse_segmented(data: &[u8], at: usize) -> Result<Self, FontError> {
        let malformed = || FontError::malformed("cmap");
        // `segCountX2` is twice the segment count, for historical
        // binary-search reasons.
        let segment_count = u16_at(data, at + 6).ok_or_else(malformed)? as usize / 2;
        let ends = at + 14;
        let starts = ends + segment_count * 2 + 2;
        let deltas = starts + segment_count * 2;
        let range_offsets = deltas + segment_count * 2;
        let glyph_id_base = range_offsets + segment_count * 2;

        let mut segments = Vec::with_capacity(segment_count);
        for index in 0..segment_count {
            segments.push(Segment {
                end: u16_at(data, ends + index * 2).ok_or_else(malformed)?,
                start: u16_at(data, starts + index * 2).ok_or_else(malformed)?,
                delta: i16_at(data, deltas + index * 2).ok_or_else(malformed)?,
                range_offset: u16_at(data, range_offsets + index * 2).ok_or_else(malformed)?,
                range_offset_position: range_offsets + index * 2,
            });
        }

        // The glyph id array runs to the end of the subtable; its length
        // is not stated, so take what is there.
        let length = u16_at(data, at + 2).ok_or_else(malformed)? as usize;
        let subtable_end = (at + length).min(data.len());
        let mut glyph_ids = Vec::new();
        let mut position = glyph_id_base;
        while position + 1 < subtable_end {
            glyph_ids.push(u16_at(data, position).ok_or_else(malformed)?);
            position += 2;
        }

        Ok(Self::Segmented {
            segments,
            glyph_ids,
            glyph_id_base,
        })
    }

    fn parse_groups(data: &[u8], at: usize) -> Result<Self, FontError> {
        let malformed = || FontError::malformed("cmap");
        let count = u32_at(data, at + 12).ok_or_else(malformed)? as usize;
        let mut groups = Vec::with_capacity(count.min(0x1_0000));
        for index in 0..count {
            let entry = at + 16 + index * 12;
            groups.push(Group {
                start: u32_at(data, entry).ok_or_else(malformed)?,
                end: u32_at(data, entry + 4).ok_or_else(malformed)?,
                start_glyph: u32_at(data, entry + 8).ok_or_else(malformed)?,
            });
        }
        Ok(Self::Groups(groups))
    }

    /// The glyph for `character`, or 0 for `.notdef`.
    fn lookup(&self, character: char) -> u16 {
        let code = character as u32;
        match self {
            Self::Groups(groups) => groups
                .iter()
                .find(|group| (group.start..=group.end).contains(&code))
                .map(|group| (group.start_glyph + (code - group.start)) as u16)
                .unwrap_or(0),
            Self::Segmented {
                segments,
                glyph_ids,
                glyph_id_base,
            } => {
                let Ok(code) = u16::try_from(code) else {
                    // Format 4 cannot express anything above the BMP.
                    return 0;
                };
                let Some(segment) = segments.iter().find(|segment| segment.end >= code) else {
                    return 0;
                };
                if segment.start > code {
                    return 0;
                }
                if segment.range_offset == 0 {
                    // The simple case: the glyph is the code plus a
                    // delta, wrapping at 16 bits by design.
                    return code.wrapping_add(segment.delta as u16);
                }
                // Otherwise `range_offset` is a byte offset from its own
                // position into the glyph id array.
                let target = segment.range_offset_position
                    + segment.range_offset as usize
                    + (code - segment.start) as usize * 2;
                let Some(index) = target.checked_sub(*glyph_id_base) else {
                    return 0;
                };
                match glyph_ids.get(index / 2).copied() {
                    // A zero entry means unmapped even here.
                    Some(0) | None => 0,
                    Some(id) => id.wrapping_add(segment.delta as u16),
                }
            }
        }
    }
}

/// Pick the most capable character map the font offers.
///
/// Preference order is about coverage, not platform: a full-Unicode
/// table beats a BMP-only one, and a Windows table beats a Mac one
/// because the Mac tables in modern fonts are usually MacRoman stubs.
fn best_subtable(data: &[u8], cmap: TableRange) -> Option<usize> {
    let count = u16_at(data, cmap.offset + 2)? as usize;
    let mut best: Option<(u8, usize)> = None;
    for index in 0..count {
        let record = cmap.offset + 4 + index * 8;
        let platform = u16_at(data, record)?;
        let encoding = u16_at(data, record + 2)?;
        let offset = u32_at(data, record + 4)? as usize;
        let subtable = cmap.offset.checked_add(offset)?;

        let rank = match (platform, encoding) {
            // Windows, full Unicode.
            (3, 10) => 4,
            // Unicode, full repertoire.
            (0, 4) | (0, 6) => 3,
            // Windows, BMP.
            (3, 1) => 2,
            // Unicode, BMP.
            (0, 0..=3) => 1,
            _ => continue,
        };
        if best.is_none_or(|(best_rank, _)| rank > best_rank) {
            best = Some((rank, subtable));
        }
    }
    best.map(|(_, subtable)| subtable)
}

// ---- bounds-checked big-endian reads ------------------------------------

pub(super) fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

pub(super) fn i16_at(data: &[u8], offset: usize) -> Option<i16> {
    u16_at(data, offset).map(|value| value as i16)
}

pub(super) fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(super) fn u8_at(data: &[u8], offset: usize) -> Option<u8> {
    data.get(offset).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a font file containing exactly these tables.
    ///
    /// Hand-assembled rather than read from disk so the tests say what
    /// they depend on and run anywhere.
    fn font_with(tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]); // searchRange and friends.

        let directory = 12 + tables.len() * 16;
        let mut offset = directory;
        for (tag, body) in tables {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            offset += body.len();
        }
        for (_, body) in tables {
            out.extend_from_slice(body);
        }
        out
    }

    fn head(units_per_em: u16, long_loca: bool) -> Vec<u8> {
        let mut body = vec![0; 54];
        body[18..20].copy_from_slice(&units_per_em.to_be_bytes());
        body[50..52].copy_from_slice(&(i16::from(long_loca)).to_be_bytes());
        body
    }

    fn hhea(ascent: i16, descent: i16, line_gap: i16, metric_count: u16) -> Vec<u8> {
        let mut body = vec![0; 36];
        body[4..6].copy_from_slice(&ascent.to_be_bytes());
        body[6..8].copy_from_slice(&descent.to_be_bytes());
        body[8..10].copy_from_slice(&line_gap.to_be_bytes());
        body[34..36].copy_from_slice(&metric_count.to_be_bytes());
        body
    }

    fn maxp(glyph_count: u16) -> Vec<u8> {
        let mut body = vec![0; 6];
        body[4..6].copy_from_slice(&glyph_count.to_be_bytes());
        body
    }

    /// A format 4 cmap mapping `'A'..='C'` to glyphs 1..=3 by delta.
    fn cmap_format4() -> Vec<u8> {
        let mut subtable = Vec::new();
        subtable.extend_from_slice(&4u16.to_be_bytes()); // format
        subtable.extend_from_slice(&32u16.to_be_bytes()); // length
        subtable.extend_from_slice(&0u16.to_be_bytes()); // language
        subtable.extend_from_slice(&4u16.to_be_bytes()); // segCountX2 = 2 segments
        subtable.extend_from_slice(&[0; 6]); // search hints
        subtable.extend_from_slice(&0x0043u16.to_be_bytes()); // end 'C'
        subtable.extend_from_slice(&0xffffu16.to_be_bytes()); // end sentinel
        subtable.extend_from_slice(&0u16.to_be_bytes()); // reserved pad
        subtable.extend_from_slice(&0x0041u16.to_be_bytes()); // start 'A'
        subtable.extend_from_slice(&0xffffu16.to_be_bytes()); // start sentinel
                                                              // 'A' is 0x41 and should be glyph 1, so the delta is 1 - 0x41.
        subtable.extend_from_slice(&(1i16 - 0x41).to_be_bytes());
        subtable.extend_from_slice(&1i16.to_be_bytes()); // sentinel delta
        subtable.extend_from_slice(&0u16.to_be_bytes()); // range offset
        subtable.extend_from_slice(&0u16.to_be_bytes()); // sentinel

        let mut body = Vec::new();
        body.extend_from_slice(&0u16.to_be_bytes()); // version
        body.extend_from_slice(&1u16.to_be_bytes()); // one record
        body.extend_from_slice(&3u16.to_be_bytes()); // platform: Windows
        body.extend_from_slice(&1u16.to_be_bytes()); // encoding: BMP
        body.extend_from_slice(&12u32.to_be_bytes()); // offset to subtable
        body.extend_from_slice(&subtable);
        body
    }

    /// `hmtx` with the given advances, one entry each.
    fn hmtx(advances: &[u16]) -> Vec<u8> {
        let mut body = Vec::new();
        for advance in advances {
            body.extend_from_slice(&advance.to_be_bytes());
            body.extend_from_slice(&0i16.to_be_bytes()); // left side bearing
        }
        body
    }

    fn sample() -> FontFile {
        let data = font_with(&[
            (b"cmap", cmap_format4()),
            (b"head", head(2048, false)),
            (b"hhea", hhea(1800, -400, 100, 3)),
            (b"hmtx", hmtx(&[500, 600, 700])),
            (b"maxp", maxp(4)),
        ]);
        FontFile::parse(data).expect("the sample font parses")
    }

    #[test]
    fn a_font_reports_its_metrics() {
        let font = sample();
        let metrics = font.metrics();
        assert_eq!(metrics.units_per_em, 2048);
        assert_eq!(metrics.ascent, 1800);
        assert_eq!(metrics.descent, -400);
        assert_eq!(metrics.line_gap, 100);
        assert_eq!(font.glyph_count(), 4);
    }

    #[test]
    fn line_spacing_is_ascent_plus_descent_plus_gap() {
        // The descent is negative in the file, so it subtracts.
        assert_eq!(sample().metrics().line_spacing(), 2300.0);
    }

    #[test]
    fn the_table_directory_is_readable() {
        let font = sample();
        assert!(font.table(b"head").is_some());
        assert!(font.table(b"hmtx").is_some());
        assert!(font.table(b"glyf").is_none(), "the sample has no outlines");
    }

    #[test]
    fn characters_map_to_glyphs_through_a_delta_segment() {
        let font = sample();
        assert_eq!(font.glyph_id('A'), Some(1));
        assert_eq!(font.glyph_id('B'), Some(2));
        assert_eq!(font.glyph_id('C'), Some(3));
    }

    #[test]
    fn an_unmapped_character_has_no_glyph() {
        let font = sample();
        // Below the segment, above it, and far outside the BMP.
        assert_eq!(font.glyph_id('@'), None);
        assert_eq!(font.glyph_id('Z'), None);
        assert_eq!(font.glyph_id('😀'), None);
    }

    #[test]
    fn a_glyph_past_the_metric_count_takes_the_last_advance() {
        // How a monospaced font stores one advance for every glyph.
        let font = sample();
        assert_eq!(font.advance(0), 500);
        assert_eq!(font.advance(2), 700);
        assert_eq!(font.advance(3), 700, "past the table, so the last one");
        assert_eq!(font.advance(9999), 700);
    }

    #[test]
    fn the_loca_format_is_reported() {
        assert!(!sample().has_long_loca());
        let data = font_with(&[
            (b"cmap", cmap_format4()),
            (b"head", head(1000, true)),
            (b"hhea", hhea(800, -200, 0, 1)),
            (b"hmtx", hmtx(&[500])),
            (b"maxp", maxp(1)),
        ]);
        let font = FontFile::parse(data).expect("parses");
        assert!(font.has_long_loca());
    }

    #[test]
    fn a_cff_font_is_rejected_rather_than_half_read() {
        let mut data = font_with(&[(b"head", head(1000, false))]);
        data[0..4].copy_from_slice(b"OTTO");
        assert!(matches!(FontFile::parse(data), Err(FontError::CffOutlines)));
    }

    #[test]
    fn a_font_collection_is_rejected() {
        let mut data = font_with(&[(b"head", head(1000, false))]);
        data[0..4].copy_from_slice(b"ttcf");
        assert!(matches!(FontFile::parse(data), Err(FontError::Collection)));
    }

    #[test]
    fn something_that_is_not_a_font_is_rejected() {
        assert!(matches!(
            FontFile::parse(b"not a font at all".to_vec()),
            Err(FontError::NotAFont(_))
        ));
    }

    #[test]
    fn a_missing_table_is_named_in_the_error() {
        let data = font_with(&[(b"head", head(1000, false))]);
        let Err(FontError::MissingTable(name)) = FontFile::parse(data) else {
            panic!("expected a missing table error");
        };
        assert_eq!(name, "maxp");
    }

    #[test]
    fn a_zero_units_per_em_is_rejected() {
        // Everything scales by it, so zero would be fatal downstream.
        let data = font_with(&[
            (b"cmap", cmap_format4()),
            (b"head", head(0, false)),
            (b"hhea", hhea(800, -200, 0, 1)),
            (b"hmtx", hmtx(&[500])),
            (b"maxp", maxp(1)),
        ]);
        assert!(FontFile::parse(data).is_err());
    }

    #[test]
    fn a_truncated_font_errors_rather_than_panicking() {
        let full = font_with(&[
            (b"cmap", cmap_format4()),
            (b"head", head(1000, false)),
            (b"hhea", hhea(800, -200, 0, 1)),
            (b"hmtx", hmtx(&[500])),
            (b"maxp", maxp(1)),
        ]);
        // Every prefix must fail cleanly; none may panic.
        for length in 0..full.len() {
            let _ = FontFile::parse(full[..length].to_vec());
        }
    }

    #[test]
    fn a_table_pointing_past_the_file_is_rejected() {
        let mut data = font_with(&[
            (b"cmap", cmap_format4()),
            (b"head", head(1000, false)),
            (b"hhea", hhea(800, -200, 0, 1)),
            (b"hmtx", hmtx(&[500])),
            (b"maxp", maxp(1)),
        ]);
        // Point the first directory entry a long way past the end.
        data[20..24].copy_from_slice(&0xffff_0000u32.to_be_bytes());
        assert!(FontFile::parse(data).is_err());
    }

    #[test]
    fn big_endian_reads_are_bounds_checked() {
        let data = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(u16_at(&data, 0), Some(0x1234));
        assert_eq!(u32_at(&data, 0), Some(0x1234_5678));
        assert_eq!(i16_at(&data, 2), Some(0x5678));
        assert_eq!(u8_at(&data, 3), Some(0x78));
        assert_eq!(u16_at(&data, 3), None, "would read past the end");
        assert_eq!(u32_at(&data, 1), None);
        assert_eq!(u8_at(&data, 4), None);
    }

    #[test]
    fn negative_values_read_as_signed() {
        let data = (-400i16).to_be_bytes();
        assert_eq!(i16_at(&data, 0), Some(-400));
    }
}
