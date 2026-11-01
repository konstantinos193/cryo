//! Bytes to text: which encoding a document is in, and how to decode it.
//!
//! Its own context because two very different stages need the same
//! answers. [`net`](super::net) decodes a response body, and
//! [`html::entities`](super::html::entities) maps numeric character
//! references — and both turn on the *same* windows-1252 table, because
//! both inherit the same 1990s mistake. Keeping one copy means the two
//! cannot drift; keeping it here rather than in either of them means
//! neither has to depend on the other.
//!
//! The decision order for a document's encoding is fixed and matters:
//!
//! 1. A **byte order mark**, which overrides everything including an
//!    HTTP header that disagrees.
//! 2. The **`Content-Type` charset** from the transport.
//! 3. A **`<meta charset>`** in the document's own first bytes — which
//!    is circular, and the spec resolves it by scanning the raw bytes
//!    before any decoding happens.
//! 4. **windows-1252**, the default. Not UTF-8: a legacy page with no
//!    declaration is far more likely to be windows-1252, and decoding
//!    it as UTF-8 turns every accented character into a replacement
//!    character rather than a slightly wrong one.
//!
//! Only the encodings a browser must have are implemented. A page in
//! Shift-JIS or GBK will decode wrongly rather than refusing to load,
//! which is the honest state of things and is called out at
//! [`Encoding::for_label`].

pub mod sniff;

pub use sniff::{detect, sniff_bom, sniff_meta_charset, EncodingSource};

/// The encodings this engine can decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Encoding {
    #[default]
    Utf8,
    /// The web's real default for undeclared content, and a superset of
    /// ISO-8859-1 in the range that matters.
    Windows1252,
    Utf16Le,
    Utf16Be,
}

impl Encoding {
    /// The encoding a charset label names, or `None` if unsupported.
    ///
    /// The aliases are the ones the WHATWG encoding standard lists for
    /// these four. An unknown label returns `None` rather than guessing
    /// — a caller then falls back to the default, which is at least a
    /// documented wrong answer rather than an arbitrary one.
    pub fn for_label(label: &str) -> Option<Self> {
        let trimmed = label.trim().trim_matches('"').to_ascii_lowercase();
        Some(match trimmed.as_str() {
            "utf-8" | "utf8" | "unicode-1-1-utf-8" | "ascii" | "us-ascii" => Self::Utf8,
            // ISO-8859-1 is *defined* to decode as windows-1252 on the
            // web: the C1 range is punctuation in practice, never
            // control codes, and every browser honours that.
            "windows-1252" | "cp1252" | "iso-8859-1" | "iso8859-1" | "latin1" | "l1"
            | "iso_8859-1" | "ansi_x3.4-1968" => Self::Windows1252,
            "utf-16" | "utf-16le" | "utf16" => Self::Utf16Le,
            "utf-16be" => Self::Utf16Be,
            _ => return None,
        })
    }

    /// The canonical label, for reporting what was used.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Windows1252 => "windows-1252",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
        }
    }

    /// Decode bytes to text.
    ///
    /// Never fails. Malformed input becomes replacement characters,
    /// because a page that is nine-tenths readable must render rather
    /// than refusing — a browser has no way to ask the author to fix it.
    pub fn decode(self, bytes: &[u8]) -> String {
        match self {
            Self::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            Self::Windows1252 => bytes.iter().map(|&byte| windows_1252(byte)).collect(),
            Self::Utf16Le => decode_utf16(bytes, u16::from_le_bytes),
            Self::Utf16Be => decode_utf16(bytes, u16::from_be_bytes),
        }
    }
}

/// One windows-1252 byte as a character.
pub fn windows_1252(byte: u8) -> char {
    match byte {
        // The C1 range is where windows-1252 differs from Latin-1.
        0x80..=0x9f => C1_REPLACEMENTS[(byte - 0x80) as usize],
        // Everything else is Latin-1, which is Unicode's first 256.
        other => char::from(other),
    }
}

/// What windows-1252 puts in the C1 control range.
///
/// Authoring tools emitted these bytes for curly quotes and dashes for
/// years, so the web treats 0x80..=0x9F as punctuation rather than as
/// control codes. The same table decides what `&#147;` means, which is
/// why it lives here and not inside either caller.
pub static C1_REPLACEMENTS: [char; 32] = [
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
];

/// Decode UTF-16 of one endianness.
///
/// A trailing odd byte is dropped: it cannot be half a code unit, and
/// truncating is better than failing the whole document.
fn decode_utf16(bytes: &[u8], to_unit: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| to_unit([pair[0], pair[1]]))
        .collect();
    char::decode_utf16(units)
        .map(|result| result.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_map_to_encodings_case_insensitively() {
        assert_eq!(Encoding::for_label("UTF-8"), Some(Encoding::Utf8));
        assert_eq!(Encoding::for_label("utf-8"), Some(Encoding::Utf8));
        assert_eq!(Encoding::for_label("  UTF-8  "), Some(Encoding::Utf8));
        assert_eq!(Encoding::for_label("\"utf-8\""), Some(Encoding::Utf8));
    }

    #[test]
    fn latin1_is_really_windows_1252_on_the_web() {
        // The spec says so, and every browser does it: the C1 range is
        // punctuation in practice, never control codes.
        assert_eq!(
            Encoding::for_label("iso-8859-1"),
            Some(Encoding::Windows1252)
        );
        assert_eq!(Encoding::for_label("latin1"), Some(Encoding::Windows1252));
    }

    #[test]
    fn an_unsupported_label_is_rejected_rather_than_guessed() {
        assert_eq!(Encoding::for_label("shift_jis"), None);
        assert_eq!(Encoding::for_label("gbk"), None);
        assert_eq!(Encoding::for_label(""), None);
    }

    #[test]
    fn every_encoding_reports_a_canonical_label() {
        for encoding in [
            Encoding::Utf8,
            Encoding::Windows1252,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
        ] {
            assert_eq!(
                Encoding::for_label(encoding.label()),
                Some(encoding),
                "{} does not round-trip",
                encoding.label()
            );
        }
    }

    #[test]
    fn utf8_decodes_what_it_should() {
        assert_eq!(Encoding::Utf8.decode(b"hello"), "hello");
        assert_eq!(Encoding::Utf8.decode("café".as_bytes()), "café");
        assert_eq!(Encoding::Utf8.decode("日本語".as_bytes()), "日本語");
        assert_eq!(Encoding::Utf8.decode(b""), "");
    }

    #[test]
    fn malformed_utf8_becomes_replacement_characters_not_an_error() {
        // A page that is mostly readable must still render.
        let decoded = Encoding::Utf8.decode(&[b'a', 0xff, 0xfe, b'b']);
        assert!(decoded.starts_with('a'));
        assert!(decoded.ends_with('b'));
        assert!(decoded.contains('\u{fffd}'));
    }

    #[test]
    fn windows_1252_decodes_ascii_unchanged() {
        assert_eq!(Encoding::Windows1252.decode(b"plain text"), "plain text");
    }

    #[test]
    fn windows_1252_puts_punctuation_in_the_c1_range() {
        // 0x93 and 0x94 are curly quotes, not control codes.
        assert_eq!(
            Encoding::Windows1252.decode(&[0x93, 0x94]),
            "\u{201c}\u{201d}"
        );
        assert_eq!(Encoding::Windows1252.decode(&[0x80]), "\u{20ac}", "euro");
        assert_eq!(Encoding::Windows1252.decode(&[0x97]), "\u{2014}", "em dash");
    }

    #[test]
    fn windows_1252_decodes_the_upper_range_as_latin1() {
        assert_eq!(Encoding::Windows1252.decode(&[0xe9]), "é");
        assert_eq!(Encoding::Windows1252.decode(&[0xff]), "ÿ");
        assert_eq!(Encoding::Windows1252.decode(&[0xa0]), "\u{a0}");
    }

    #[test]
    fn every_byte_decodes_to_something_in_windows_1252() {
        // It is a single-byte encoding with no holes, so no input can
        // fail to decode.
        for byte in 0u8..=255 {
            let decoded = Encoding::Windows1252.decode(&[byte]);
            assert_eq!(decoded.chars().count(), 1, "byte {byte:#04x}");
        }
    }

    #[test]
    fn utf16_decodes_in_both_byte_orders() {
        // "hi" as UTF-16.
        assert_eq!(Encoding::Utf16Le.decode(&[0x68, 0, 0x69, 0]), "hi");
        assert_eq!(Encoding::Utf16Be.decode(&[0, 0x68, 0, 0x69]), "hi");
    }

    #[test]
    fn utf16_handles_a_surrogate_pair() {
        // U+1F600, which needs two code units.
        let emoji: Vec<u8> = [0xd83d_u16, 0xde00]
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert_eq!(Encoding::Utf16Le.decode(&emoji), "\u{1f600}");
    }

    #[test]
    fn a_lone_surrogate_becomes_a_replacement_character() {
        assert_eq!(Encoding::Utf16Le.decode(&[0x3d, 0xd8]), "\u{fffd}");
    }

    #[test]
    fn a_trailing_odd_byte_in_utf16_is_dropped() {
        // Half a code unit cannot be decoded, and truncating beats
        // failing the whole document.
        assert_eq!(Encoding::Utf16Le.decode(&[0x68, 0, 0x69]), "h");
    }

    #[test]
    fn the_c1_table_matches_single_byte_decoding() {
        // One table, two callers: this is what stops them drifting.
        for byte in 0x80u8..=0x9f {
            assert_eq!(
                windows_1252(byte),
                C1_REPLACEMENTS[(byte - 0x80) as usize],
                "byte {byte:#04x}"
            );
        }
    }

    #[test]
    fn the_default_encoding_is_utf8() {
        assert_eq!(Encoding::default(), Encoding::Utf8);
    }
}
