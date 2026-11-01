//! Turning a downloaded font file into something the rasteriser can
//! draw.
//!
//! [`paint::font`](crate::engine::cryo_impl::paint::font) finds and
//! reads the fonts a *machine* has. This is the other half: the fonts
//! a *page* brings with it through `@font-face`. They arrive as bytes
//! in a container, and only the innermost one is a font this engine
//! already understands:
//!
//! ```text
//!   .woff2 ─▶ Brotli ─▶ table directory ─▶ transforms undone ─▶ sfnt
//!   .ttf/.otf ───────────────────────────────────────────────▶ sfnt
//! ```
//!
//! So this context owns the containers and nothing else. What comes
//! out is an sfnt — byte for byte the kind of thing a `.ttf` on disk
//! is — and the font reader opens it with the code it already has.
//!
//! WOFF2 is the one that matters: a toolchain emits nothing else
//! today, so a page's own typeface is unreachable without it, and a
//! fallback face of a different width rewraps every line the designer
//! set.

pub mod brotli;
mod glyf;
pub mod woff2;

/// Why a downloaded font could not be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FontFileError {
    #[error("the bytes are not a font this engine can open")]
    NotAFont,
    #[error("the font file ends part way through")]
    Truncated,
    #[error("the font file is malformed")]
    Malformed,
    #[error("the font would decompress to more than this engine will hold")]
    TooLarge,
    #[error("the table {} is stored under a transform this engine does not undo",
        String::from_utf8_lossy(.0))]
    UnknownTransform([u8; 4]),
    #[error("the font's compressed data could not be read: {0}")]
    Compression(#[from] brotli::BrotliError),
}

/// Turn whatever arrived into an sfnt, if it is one of the shapes
/// this engine opens.
///
/// A plain `.ttf` or `.otf` is already an sfnt and is handed back
/// untouched; it is still worth passing through here so a caller has
/// one door for "bytes from the network become a font".
pub fn to_sfnt(data: &[u8]) -> Result<Vec<u8>, FontFileError> {
    if woff2::is_woff2(data) {
        return woff2::to_sfnt(data);
    }
    if is_sfnt(data) {
        return Ok(data.to_vec());
    }
    Err(FontFileError::NotAFont)
}

/// Whether these bytes are already a bare font.
///
/// The four opening bytes name the outline format: `0x00010000` and
/// `true` for TrueType, `OTTO` for one with PostScript outlines.
fn is_sfnt(data: &[u8]) -> bool {
    matches!(
        data.get(..4),
        Some(b"\x00\x01\x00\x00") | Some(b"true") | Some(b"OTTO") | Some(b"ttcf")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_font_passes_through_unchanged() {
        let mut font = b"\x00\x01\x00\x00".to_vec();
        font.extend_from_slice(&[0; 20]);
        assert_eq!(to_sfnt(&font), Ok(font.clone()));
    }

    #[test]
    fn the_sfnt_flavours_are_all_recognised() {
        for signature in [b"\x00\x01\x00\x00", b"true", b"OTTO", b"ttcf"] {
            assert!(
                is_sfnt(signature),
                "{:?}",
                String::from_utf8_lossy(signature)
            );
        }
        assert!(!is_sfnt(b"wOF2"), "a container is not a bare font");
        assert!(!is_sfnt(b"\x89PNG"));
    }

    #[test]
    fn bytes_that_are_no_kind_of_font_are_refused() {
        assert_eq!(to_sfnt(b"<html>").err(), Some(FontFileError::NotAFont));
        assert_eq!(to_sfnt(&[]).err(), Some(FontFileError::NotAFont));
    }
}
