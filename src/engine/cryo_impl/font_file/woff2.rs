//! WOFF2: the container every `@font-face` on the modern web uses.
//!
//! A WOFF2 file is an sfnt that has been rearranged for size and then
//! Brotli-compressed whole. Two things make it more than a zip of a
//! font:
//!
//! 1. **The table directory is outside the compression**, with tags
//!    replaced by indices into a fixed list and lengths written as
//!    variable-length integers. So the directory must be read before
//!    anything is decompressed.
//! 2. **`glyf` and `loca` are transformed**, not merely compressed.
//!    The outlines are taken apart into parallel streams — one of
//!    contour counts, one of point counts, one of flags, one of
//!    coordinates — because each compresses far better on its own
//!    than interleaved. Putting them back together is
//!    [`glyf`](super::glyf), and it is most of the work here.
//!
//! What comes out is an ordinary sfnt: the same bytes a `.ttf` on
//! disk would hold, so everything downstream reads it unchanged.
//!
//! Not implemented: font collections (one file holding several
//! fonts), the optional `hmtx` transform, and the metadata and
//! private blocks, which carry licensing text rather than anything
//! drawable.

use std::collections::BTreeMap;

use super::brotli;
use super::glyf;
use super::FontFileError;

/// The four bytes a WOFF2 file starts with.
const SIGNATURE: &[u8; 4] = b"wOF2";

/// The fixed header, before the table directory.
const HEADER: usize = 48;

/// A ceiling on what a file may claim to decompress to, so a corrupt
/// or hostile header cannot ask for a buffer no machine has.
const MAX_FONT: usize = 64 * 1024 * 1024;

/// Tags the directory may name by index instead of spelling out.
///
/// The order is the format's, not alphabetical, and index 63 means
/// "the tag follows in full".
const KNOWN_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm",
    b"glyf", b"loca", b"prep", b"CFF ", b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern",
    b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS", b"GSUB", b"EBSC",
    b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar",
    b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty",
    b"just", b"lcar", b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf", b"Glat",
    b"Gloc", b"Feat", b"Sill",
];

/// Whether these bytes are a WOFF2 file.
pub fn is_woff2(data: &[u8]) -> bool {
    data.starts_with(SIGNATURE)
}

/// One table, as the directory describes it.
struct Entry {
    tag: [u8; 4],
    /// Its length in the compressed stream. The *untransformed*
    /// length is in the directory too and is deliberately ignored:
    /// the rebuilt `glyf` is a different size from the one the font
    /// shipped, so believing the stated length would truncate it.
    stored_length: usize,
    transformed: bool,
}

/// Turn a WOFF2 file into the sfnt it holds.
pub fn to_sfnt(data: &[u8]) -> Result<Vec<u8>, FontFileError> {
    if !is_woff2(data) {
        return Err(FontFileError::NotAFont);
    }
    let total_sfnt = be32(data, 16)? as usize;
    if total_sfnt > MAX_FONT {
        return Err(FontFileError::TooLarge);
    }
    let compressed_length = be32(data, 20)? as usize;
    let count = usize::from(be16(data, 12)?);

    let mut cursor = HEADER;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(read_entry(data, &mut cursor)?);
    }

    let body = data
        .get(cursor..cursor + compressed_length)
        .ok_or(FontFileError::Truncated)?;
    let font = brotli::decompress(body, MAX_FONT).map_err(FontFileError::Compression)?;

    assemble(&entries, &font)
}

/// Read one directory entry, advancing the cursor past it.
fn read_entry(data: &[u8], cursor: &mut usize) -> Result<Entry, FontFileError> {
    let flags = *data.get(*cursor).ok_or(FontFileError::Truncated)?;
    *cursor += 1;
    let index = usize::from(flags & 0x3f);
    let tag: [u8; 4] = if index == 0x3f {
        let bytes = data
            .get(*cursor..*cursor + 4)
            .ok_or(FontFileError::Truncated)?;
        *cursor += 4;
        bytes.try_into().map_err(|_| FontFileError::Truncated)?
    } else {
        **KNOWN_TAGS.get(index).ok_or(FontFileError::Malformed)?
    };

    let version = (flags >> 6) & 3;
    // The sense of the version flag is inverted for the two
    // transformed tables: for `glyf` and `loca`, 3 means *untouched*
    // and anything else means transformed; for every other table it
    // is the other way round. Getting this backwards reads a
    // transform length that is not there.
    let transformed = if &tag == b"glyf" || &tag == b"loca" {
        version != 3
    } else {
        version != 0
    };

    let original_length = base128(data, cursor)? as usize;
    let stored_length = if transformed {
        base128(data, cursor)? as usize
    } else {
        original_length
    };
    Ok(Entry {
        tag,
        stored_length,
        transformed,
    })
}

/// Put the tables back into an sfnt.
fn assemble(entries: &[Entry], font: &[u8]) -> Result<Vec<u8>, FontFileError> {
    // Slice the decompressed stream into tables, in directory order.
    let mut stored: BTreeMap<[u8; 4], &[u8]> = BTreeMap::new();
    let mut at = 0usize;
    let mut transformed_glyf = None;
    for entry in entries {
        let bytes = font
            .get(at..at + entry.stored_length)
            .ok_or(FontFileError::Truncated)?;
        at += entry.stored_length;
        if entry.transformed {
            match &entry.tag {
                b"glyf" => transformed_glyf = Some(bytes),
                // `loca` is rebuilt from `glyf`, so its stored form —
                // which is empty — is nothing to keep.
                b"loca" => {}
                _ => return Err(FontFileError::UnknownTransform(entry.tag)),
            }
            continue;
        }
        stored.insert(entry.tag, bytes);
    }

    let mut rebuilt: BTreeMap<[u8; 4], Vec<u8>> = BTreeMap::new();
    if let Some(bytes) = transformed_glyf {
        let rebuilt_glyf = glyf::reconstruct(bytes)?;
        rebuilt.insert(*b"loca", rebuilt_glyf.loca());
        rebuilt.insert(*b"glyf", rebuilt_glyf.glyf);
        // `head` names the form `loca` is written in, and the
        // rebuild always writes the long one — so `head` has to be
        // told. A font whose two halves disagree reads every glyph
        // from the wrong offset.
        let head = stored.remove(b"head").ok_or(FontFileError::Malformed)?;
        rebuilt.insert(*b"head", say_loca_is_long(head)?);
    }

    // Every table, in tag order, which is what an sfnt directory is
    // sorted by.
    let mut tables: Vec<([u8; 4], &[u8])> = stored
        .iter()
        .map(|(tag, bytes)| (*tag, *bytes))
        .chain(rebuilt.iter().map(|(tag, bytes)| (*tag, bytes.as_slice())))
        .collect();
    tables.sort_by_key(|(tag, _)| *tag);

    let header = 12 + tables.len() * 16;
    let mut out = Vec::with_capacity(header);
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    // searchRange, entrySelector and rangeShift: a binary-search hint
    // no reader here uses, written correctly anyway so the file is a
    // font to anything else that opens it.
    let entry_selector = (usize::BITS - 1 - tables.len().max(1).leading_zeros()) as u16;
    let search_range = 16u16 << entry_selector;
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&((tables.len() as u16 * 16).wrapping_sub(search_range)).to_be_bytes());

    let mut offset = header;
    let mut records = Vec::with_capacity(tables.len());
    for (tag, bytes) in &tables {
        records.push((*tag, offset, bytes.len()));
        offset += (bytes.len() + 3) & !3;
    }
    for (tag, offset, length) in &records {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(*offset as u32).to_be_bytes());
        out.extend_from_slice(&(*length as u32).to_be_bytes());
    }
    for (_, bytes) in &tables {
        out.extend_from_slice(bytes);
        // Tables are four-byte aligned, and the padding is part of
        // the file even though no table claims it.
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    Ok(out)
}

/// Set `head.indexToLocFormat` to the long form.
///
/// It is a single field two bytes from the end of the table, and the
/// whole of `head` is otherwise carried through untouched.
fn say_loca_is_long(head: &[u8]) -> Result<Vec<u8>, FontFileError> {
    const INDEX_TO_LOC_FORMAT: usize = 50;
    let mut out = head.to_vec();
    let field = out
        .get_mut(INDEX_TO_LOC_FORMAT..INDEX_TO_LOC_FORMAT + 2)
        .ok_or(FontFileError::Malformed)?;
    field.copy_from_slice(&1u16.to_be_bytes());
    Ok(out)
}

fn be16(data: &[u8], at: usize) -> Result<u16, FontFileError> {
    data.get(at..at + 2)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_be_bytes)
        .ok_or(FontFileError::Truncated)
}

fn be32(data: &[u8], at: usize) -> Result<u32, FontFileError> {
    data.get(at..at + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or(FontFileError::Truncated)
}

/// A length written seven bits at a time, high bits first, with the
/// top bit of each byte saying whether more follow.
///
/// Leading zero bytes are rejected: the format insists on the
/// shortest encoding, so a padded one marks a file built by
/// something that was not following the rules.
fn base128(data: &[u8], cursor: &mut usize) -> Result<u32, FontFileError> {
    let mut value: u32 = 0;
    for index in 0..5 {
        let byte = *data.get(*cursor).ok_or(FontFileError::Truncated)?;
        *cursor += 1;
        if index == 0 && byte == 0x80 {
            return Err(FontFileError::Malformed);
        }
        if value & 0xfe00_0000 != 0 {
            return Err(FontFileError::Malformed);
        }
        value = (value << 7) | u32::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(FontFileError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_is_what_identifies_the_format() {
        assert!(is_woff2(b"wOF2\0\0\0\0"));
        assert!(!is_woff2(b"wOFF\0\0\0\0"), "woff 1 is a different format");
        assert!(!is_woff2(b"\x00\x01\x00\x00"), "a plain sfnt is not one");
        assert!(!is_woff2(b"wOF"));
    }

    #[test]
    fn a_base128_length_reads_seven_bits_a_byte() {
        let mut cursor = 0;
        assert_eq!(base128(&[0x3f], &mut cursor), Ok(0x3f));
        assert_eq!(cursor, 1);
        let mut cursor = 0;
        // 0x81 0x00 is one more byte, then zero: 1 << 7.
        assert_eq!(base128(&[0x81, 0x00], &mut cursor), Ok(128));
        assert_eq!(cursor, 2);
    }

    #[test]
    fn a_padded_base128_length_is_refused() {
        // The shortest encoding is the only legal one.
        let mut cursor = 0;
        assert_eq!(
            base128(&[0x80, 0x01], &mut cursor).err(),
            Some(FontFileError::Malformed)
        );
    }

    #[test]
    fn a_base128_length_that_never_ends_is_refused() {
        let mut cursor = 0;
        assert_eq!(
            base128(&[0x81, 0x81, 0x81, 0x81, 0x81, 0x81], &mut cursor).err(),
            Some(FontFileError::Malformed)
        );
    }

    #[test]
    fn the_transform_flag_means_the_opposite_for_glyf_and_loca() {
        // Version 0 transforms `glyf`; the same value leaves `cmap`
        // alone. Reading it the same way for both looks for a
        // transform length that was never written.
        let mut cursor = 0;
        // Flags 0b00_001010 = version 0, known tag 10 (`glyf`), then
        // two lengths.
        let entry = read_entry(&[0x0a, 0x10, 0x08], &mut cursor).expect("an entry");
        assert_eq!(&entry.tag, b"glyf");
        assert!(entry.transformed);
        assert_eq!(entry.stored_length, 0x08);

        let mut cursor = 0;
        // Known tag 0 (`cmap`) at version 0: untouched, one length.
        let entry = read_entry(&[0x00, 0x10], &mut cursor).expect("an entry");
        assert_eq!(&entry.tag, b"cmap");
        assert!(!entry.transformed);
        assert_eq!(entry.stored_length, 0x10);
    }

    #[test]
    fn a_tag_may_be_spelled_out_instead_of_indexed() {
        let mut cursor = 0;
        let entry = read_entry(b"\x3fWXYZ\x20", &mut cursor).expect("an entry");
        assert_eq!(&entry.tag, b"WXYZ");
        assert_eq!(entry.stored_length, 0x20);
    }

    #[test]
    fn bytes_that_are_not_a_woff2_are_refused() {
        assert_eq!(to_sfnt(b"not a font").err(), Some(FontFileError::NotAFont));
    }
}
