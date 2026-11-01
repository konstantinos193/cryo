//! The constant tables Brotli's command decoding needs.
//!
//! Every one is read straight from RFC 7932. They are gathered here
//! rather than inlined because a wrong entry does not fail loudly: it
//! produces plausible bytes that are wrong a few hundred bytes later,
//! and the only way to find it is to compare a whole decoded stream
//! against a reference. Keeping them in one file makes that
//! comparison a check of this file.

/// Length and distance commands come as one symbol of 704, which
/// splits into an insert code and a copy code. These two say which
/// range of each the symbol's low bits are an offset into.
pub(super) const INSERT_RANGE: [u16; 9] = [0, 0, 8, 8, 0, 16, 8, 16, 16];
pub(super) const COPY_RANGE: [u16; 9] = [0, 8, 0, 8, 16, 0, 16, 8, 16];

/// Insert lengths: 24 codes, each a base and a count of extra bits.
pub(super) const INSERT_LENGTH_OFFSET: [u32; 24] = [
    0, 1, 2, 3, 4, 5, 6, 8, 10, 14, 18, 26, 34, 50, 66, 98, 130, 194, 322, 578, 1090, 2114, 6210,
    22594,
];
pub(super) const INSERT_LENGTH_EXTRA: [u32; 24] = [
    0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 12, 14, 24,
];

/// Copy lengths: the same shape, and never shorter than two — a
/// one-byte copy is always cheaper written as a literal.
pub(super) const COPY_LENGTH_OFFSET: [u32; 24] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 18, 22, 30, 38, 54, 70, 102, 134, 198, 326, 582, 1094, 2118,
];
pub(super) const COPY_LENGTH_EXTRA: [u32; 24] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 24,
];

/// The sixteen short distance codes name one of the last four
/// distances, optionally nudged. The first says "the same distance
/// again", which is what makes a run of equal gaps nearly free.
pub(super) const DISTANCE_SHORT_INDEX: [u8; 16] = [3, 2, 1, 0, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2];
pub(super) const DISTANCE_SHORT_OFFSET: [i32; 16] =
    [0, 0, 0, 0, -1, 1, -2, 2, -3, 3, -1, 1, -2, 2, -3, 3];

/// Block-length codes: 26 of them, shared by all three block kinds.
pub(super) const BLOCK_LENGTH_OFFSET: [u32; 26] = [
    1, 5, 9, 13, 17, 25, 33, 41, 49, 65, 81, 97, 113, 145, 177, 209, 241, 305, 369, 497, 753, 1265,
    2289, 4337, 8433, 16625,
];
pub(super) const BLOCK_LENGTH_EXTRA: [u32; 26] = [
    2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13, 24,
];

/// How the previous two bytes become a literal's context.
///
/// The mode is per literal block type, and the difference is real: a
/// stream of English text and a stream of 16-bit samples want
/// completely different groupings, and guessing wrong costs a few
/// per cent of every literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ContextMode {
    Lsb6,
    Msb6,
    Utf8,
    Signed,
}

impl ContextMode {
    pub(super) fn from_bits(value: u32) -> Self {
        match value & 3 {
            0 => Self::Lsb6,
            1 => Self::Msb6,
            2 => Self::Utf8,
            _ => Self::Signed,
        }
    }

    /// The context id for a literal following `p1` then `p2`, where
    /// `p1` is the byte immediately before it.
    pub(super) fn context(self, p1: u8, p2: u8) -> usize {
        match self {
            Self::Lsb6 => usize::from(p1 & 0x3f),
            Self::Msb6 => usize::from(p1 >> 2),
            Self::Utf8 => {
                usize::from(UTF8_LAST[usize::from(p1)]) | usize::from(UTF8_SECOND[usize::from(p2)])
            }
            Self::Signed => {
                (usize::from(SIGNED[usize::from(p1)]) << 3) | usize::from(SIGNED[usize::from(p2)])
            }
        }
    }
}

/// The UTF-8 context model, keyed on the byte just before a literal.
///
/// Built from the rule the spec states rather than copied as 256
/// numbers: control bytes, whitespace, punctuation, digits and
/// letters each get their own class, and a continuation byte is
/// grouped by how far into a sequence it is.
pub(super) static UTF8_LAST: [u8; 256] = build_utf8_last();
pub(super) static UTF8_SECOND: [u8; 256] = build_utf8_second();
pub(super) static SIGNED: [u8; 256] = build_signed();

/// The byte before a literal, classed by what usually follows it.
///
/// The classes are the high bits of the context, so they come in
/// fours. The groupings are the interesting part and they are not
/// arbitrary: an opening bracket predicts different bytes from a
/// closing one, a digit from a letter, and **vowels are split from
/// consonants**, because what follows "q" and what follows "e" have
/// little in common. That one split is most of why this mode beats
/// the simple ones on text.
const fn build_utf8_last() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut index = 0usize;
    while index < 256 {
        table[index] = match index as u8 {
            // Tab, newline and carriage return keep company with
            // space; the other controls are one class.
            9 | 10 | 13 => 4,
            0..=31 => 0,
            b' ' => 8,
            b'"' | b'\'' => 16,
            b'%' => 20,
            b'(' | b'<' | b'[' | b'{' => 24,
            b')' | b'>' | b']' | b'}' => 28,
            b',' | b':' | b';' => 32,
            b'.' => 36,
            b'=' => 40,
            b'0'..=b'9' => 44,
            b'A' | b'E' | b'I' | b'O' | b'U' => 48,
            b'B'..=b'Z' => 52,
            b'a' | b'e' | b'i' | b'o' | b'u' => 56,
            b'b'..=b'z' => 60,
            // Everything else printable: the general punctuation
            // class.
            33..=126 => 12,
            127 => 0,
            // Inside a multi-byte character the low bit carries more
            // than the rest, so it alone decides, and the two halves
            // separate continuation bytes from lead bytes.
            0x80..=0xBF => (index as u8) & 1,
            _ => 2 + ((index as u8) & 1),
        };
        index += 1;
    }
    table
}

/// The byte before *that* one, in two bits.
///
/// A coarser view of the same idea: letters, digits, punctuation and
/// everything else, with lower-case letters — far the most common —
/// given their own class.
const fn build_utf8_second() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut index = 0usize;
    while index < 256 {
        table[index] = match index as u8 {
            0..=32 => 0,
            b'0'..=b'9' => 2,
            b'A'..=b'Z' => 2,
            b'a'..=b'z' => 3,
            33..=126 => 1,
            // A lead byte of a three- or four-byte character; a
            // continuation byte or a two-byte lead falls through to
            // the default class.
            224..=255 => 2,
            _ => 0,
        };
        index += 1;
    }
    table
}

const fn build_signed() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut index = 0usize;
    while index < 256 {
        // Eight classes, mirrored about the middle: a byte read as a
        // signed value is classed by magnitude and sign, so the high
        // byte of a small positive delta and of a small negative one
        // land in classes that are each other's reflection. Zero and
        // 255 — the two that dominate a stream of 16-bit deltas —
        // get a class to themselves.
        table[index] = match index {
            0 => 0,
            1..=15 => 1,
            16..=63 => 2,
            64..=127 => 3,
            128..=191 => 4,
            192..=239 => 5,
            240..=254 => 6,
            _ => 7,
        };
        index += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_length_tables_agree_in_size() {
        assert_eq!(INSERT_LENGTH_OFFSET.len(), INSERT_LENGTH_EXTRA.len());
        assert_eq!(COPY_LENGTH_OFFSET.len(), COPY_LENGTH_EXTRA.len());
        assert_eq!(BLOCK_LENGTH_OFFSET.len(), BLOCK_LENGTH_EXTRA.len());
    }

    #[test]
    fn each_length_code_reaches_the_next_ones_base() {
        // A code's base plus everything its extra bits can add must
        // reach the next code, or some lengths are unreachable.
        for index in 0..INSERT_LENGTH_OFFSET.len() - 1 {
            let top = INSERT_LENGTH_OFFSET[index] + (1 << INSERT_LENGTH_EXTRA[index]);
            assert_eq!(top, INSERT_LENGTH_OFFSET[index + 1], "insert code {index}");
        }
        for index in 0..COPY_LENGTH_OFFSET.len() - 1 {
            let top = COPY_LENGTH_OFFSET[index] + (1 << COPY_LENGTH_EXTRA[index]);
            assert_eq!(top, COPY_LENGTH_OFFSET[index + 1], "copy code {index}");
        }
        for index in 0..BLOCK_LENGTH_OFFSET.len() - 1 {
            let top = BLOCK_LENGTH_OFFSET[index] + (1 << BLOCK_LENGTH_EXTRA[index]);
            assert_eq!(top, BLOCK_LENGTH_OFFSET[index + 1], "block code {index}");
        }
    }

    #[test]
    fn a_copy_is_never_shorter_than_two_bytes() {
        assert_eq!(COPY_LENGTH_OFFSET[0], 2);
    }

    #[test]
    fn the_first_short_distance_code_repeats_the_last_distance() {
        assert_eq!(DISTANCE_SHORT_INDEX[0], 3);
        assert_eq!(DISTANCE_SHORT_OFFSET[0], 0);
    }

    #[test]
    fn the_simple_context_modes_are_slices_of_the_last_byte() {
        assert_eq!(ContextMode::Lsb6.context(0xAB, 0x00), 0x2B);
        assert_eq!(ContextMode::Msb6.context(0xAB, 0x00), 0x2A);
    }

    /// Spot values taken from the format's own table, one per class.
    ///
    /// A context table fails quietly — a wrong class picks a wrong
    /// prefix code, and the stream decodes to plausible rubbish a few
    /// hundred bytes later. These are the cheapest way to notice.
    #[test]
    fn the_utf8_classes_match_the_format() {
        for (byte, class) in [
            (0u8, 0u8),
            (9, 4),
            (13, 4),
            (31, 0),
            (b' ', 8),
            (b'!', 12),
            (b'"', 16),
            (b'%', 20),
            (b'\'', 16),
            (b'(', 24),
            (b')', 28),
            (b',', 32),
            (b'.', 36),
            (b'/', 12),
            (b'0', 44),
            (b'9', 44),
            (b':', 32),
            (b'<', 24),
            (b'=', 40),
            (b'>', 28),
            (b'?', 12),
            (b'A', 48),
            (b'B', 52),
            (b'E', 48),
            (b'Z', 52),
            (b'[', 24),
            (b']', 28),
            (b'a', 56),
            (b'b', 60),
            (b'u', 56),
            (b'z', 60),
            (b'{', 24),
            (b'}', 28),
            (b'~', 12),
            (127, 0),
            (128, 0),
            (129, 1),
            (190, 0),
            (191, 1),
            (192, 2),
            (193, 3),
            (255, 3),
        ] {
            assert_eq!(UTF8_LAST[usize::from(byte)], class, "last byte {byte}");
        }
        for (byte, class) in [
            (0u8, 0u8),
            (32, 0),
            (33, 1),
            (47, 1),
            (48, 2),
            (57, 2),
            (58, 1),
            (64, 1),
            (65, 2),
            (90, 2),
            (91, 1),
            (96, 1),
            (97, 3),
            (122, 3),
            (123, 1),
            (126, 1),
            (127, 0),
            (223, 0),
            (224, 2),
            (255, 2),
        ] {
            assert_eq!(UTF8_SECOND[usize::from(byte)], class, "second byte {byte}");
        }
    }

    #[test]
    fn the_signed_classes_match_the_format() {
        for (byte, class) in [
            (0u8, 0u8),
            (1, 1),
            (15, 1),
            (16, 2),
            (63, 2),
            (64, 3),
            (127, 3),
            (128, 4),
            (191, 4),
            (192, 5),
            (239, 5),
            (240, 6),
            (254, 6),
            (255, 7),
        ] {
            assert_eq!(SIGNED[usize::from(byte)], class, "byte {byte}");
        }
    }

    #[test]
    fn every_context_id_fits_the_sixty_four_slots_a_block_has() {
        for mode in [
            ContextMode::Lsb6,
            ContextMode::Msb6,
            ContextMode::Utf8,
            ContextMode::Signed,
        ] {
            for p1 in 0..=255u8 {
                for p2 in [0u8, 0x41, 0x80, 0xC3, 0xFF] {
                    assert!(mode.context(p1, p2) < 64, "{mode:?} {p1} {p2}");
                }
            }
        }
    }
}
