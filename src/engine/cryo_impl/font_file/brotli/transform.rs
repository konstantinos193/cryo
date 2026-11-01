//! The 121 ways a dictionary word may be dressed before use.
//!
//! A dictionary reference does not only name a word; the number above
//! the word index picks a *transform*, and the transform is what
//! lets one stored word serve a dozen places in a text:
//!
//! ```text
//!   "work" ──▶ transform 1  ──▶ "work "
//!          ──▶ transform 2  ──▶ " work "
//!          ──▶ transform 49 ──▶ "Work"
//! ```
//!
//! Each is a prefix, a change to the word itself, and a suffix. The
//! changes are blunt by design — trim bytes off either end, or
//! uppercase — because the encoder only has to find *a* cheap way to
//! say what it means, not the linguistically right one. The
//! uppercasing in particular is a deliberate approximation of UTF-8:
//! flipping one bit of a two-byte character and a different bit of a
//! three-byte one, which is wrong for most alphabets and right often
//! enough to pay for itself.
//!
//! Font data uses transforms too, which is not obvious: a run of
//! bytes that happens to match a word plus a trailing space costs one
//! reference instead of two commands.

/// What a transform does to the word between its prefix and suffix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Identity,
    OmitLast1,
    OmitLast2,
    OmitLast3,
    OmitLast4,
    OmitLast5,
    OmitLast6,
    OmitLast7,
    OmitLast8,
    OmitLast9,
    OmitFirst1,
    OmitFirst2,
    OmitFirst3,
    OmitFirst4,
    OmitFirst5,
    OmitFirst6,
    OmitFirst7,
    // There is no OmitFirst8: the table simply never asks for
    // one, and a variant nothing constructs is a variant nothing
    // tests.
    OmitFirst9,
    UppercaseFirst,
    UppercaseAll,
}

impl Kind {
    /// How many bytes to drop, and from which end.
    const fn trim(self) -> (usize, usize) {
        match self {
            Self::OmitFirst1 => (1, 0),
            Self::OmitFirst2 => (2, 0),
            Self::OmitFirst3 => (3, 0),
            Self::OmitFirst4 => (4, 0),
            Self::OmitFirst5 => (5, 0),
            Self::OmitFirst6 => (6, 0),
            Self::OmitFirst7 => (7, 0),
            Self::OmitFirst9 => (9, 0),
            Self::OmitLast1 => (0, 1),
            Self::OmitLast2 => (0, 2),
            Self::OmitLast3 => (0, 3),
            Self::OmitLast4 => (0, 4),
            Self::OmitLast5 => (0, 5),
            Self::OmitLast6 => (0, 6),
            Self::OmitLast7 => (0, 7),
            Self::OmitLast8 => (0, 8),
            Self::OmitLast9 => (0, 9),
            _ => (0, 0),
        }
    }
}

/// One entry of the table.
pub(super) struct Transform {
    pub(super) prefix: &'static [u8],
    pub(super) kind: Kind,
    pub(super) suffix: &'static [u8],
}

/// Apply transform `index` to `word`, appending the result.
///
/// Returns `false` for an index past the table, which a malformed
/// stream can produce.
pub(super) fn apply(index: usize, word: &[u8], out: &mut Vec<u8>) -> bool {
    let Some(transform) = TRANSFORMS.get(index) else {
        return false;
    };
    out.extend_from_slice(transform.prefix);
    let (front, back) = transform.kind.trim();
    let body = word
        .get(front.min(word.len())..word.len().saturating_sub(back).max(front.min(word.len())))
        .unwrap_or(&[]);
    let start = out.len();
    out.extend_from_slice(body);
    match transform.kind {
        Kind::UppercaseFirst => uppercase(&mut out[start..], true),
        Kind::UppercaseAll => uppercase(&mut out[start..], false),
        _ => {}
    }
    out.extend_from_slice(transform.suffix);
    true
}

/// Brotli's uppercasing: ASCII properly, and a bit-flip per
/// multi-byte character that is right for Latin-1 supplements and a
/// guess everywhere else. Stated plainly because it is not a bug —
/// the encoder applies the same rule, so the two agree.
fn uppercase(bytes: &mut [u8], first_only: bool) {
    let mut index = 0;
    while index < bytes.len() {
        let step = if bytes[index] < 0xC0 {
            if bytes[index].is_ascii_lowercase() {
                bytes[index] ^= 32;
            }
            1
        } else if bytes[index] < 0xE0 {
            if index + 1 < bytes.len() {
                bytes[index + 1] ^= 32;
            }
            2
        } else {
            if index + 2 < bytes.len() {
                bytes[index + 2] ^= 5;
            }
            3
        };
        if first_only {
            return;
        }
        index += step;
    }
}

/// The table, recovered from the format itself: each entry is what
/// Brotli's own decoder answers when asked for a known word under
/// that transform number.
static TRANSFORMS: &[Transform] = &[
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"",
    }, // 0
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20",
    }, // 1
    Transform {
        prefix: b"\x20",
        kind: Kind::Identity,
        suffix: b"\x20",
    }, // 2
    Transform {
        prefix: b"",
        kind: Kind::OmitFirst1,
        suffix: b"",
    }, // 3
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x20",
    }, // 4
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x74\x68\x65\x20",
    }, // 5
    Transform {
        prefix: b"\x20",
        kind: Kind::Identity,
        suffix: b"",
    }, // 6
    Transform {
        prefix: b"\x73\x20",
        kind: Kind::Identity,
        suffix: b"\x20",
    }, // 7
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x6f\x66\x20",
    }, // 8
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"",
    }, // 9
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x61\x6e\x64\x20",
    }, // 10
    Transform {
        prefix: b"",
        kind: Kind::OmitFirst2,
        suffix: b"",
    }, // 11
    Transform {
        prefix: b"",
        kind: Kind::OmitLast1,
        suffix: b"",
    }, // 12
    Transform {
        prefix: b"\x2c\x20",
        kind: Kind::Identity,
        suffix: b"\x20",
    }, // 13
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x2c\x20",
    }, // 14
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseFirst,
        suffix: b"\x20",
    }, // 15
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x69\x6e\x20",
    }, // 16
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x74\x6f\x20",
    }, // 17
    Transform {
        prefix: b"\x65\x20",
        kind: Kind::Identity,
        suffix: b"\x20",
    }, // 18
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x22",
    }, // 19
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x2e",
    }, // 20
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x22\x3e",
    }, // 21
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x0a",
    }, // 22
    Transform {
        prefix: b"",
        kind: Kind::OmitLast3,
        suffix: b"",
    }, // 23
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x5d",
    }, // 24
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x66\x6f\x72\x20",
    }, // 25
    Transform {
        prefix: b"",
        kind: Kind::OmitFirst3,
        suffix: b"",
    }, // 26
    Transform {
        prefix: b"",
        kind: Kind::OmitLast2,
        suffix: b"",
    }, // 27
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x61\x20",
    }, // 28
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x74\x68\x61\x74\x20",
    }, // 29
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseFirst,
        suffix: b"",
    }, // 30
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x2e\x20",
    }, // 31
    Transform {
        prefix: b"\x2e",
        kind: Kind::Identity,
        suffix: b"",
    }, // 32
    Transform {
        prefix: b"\x20",
        kind: Kind::Identity,
        suffix: b"\x2c\x20",
    }, // 33
    Transform {
        prefix: b"",
        kind: Kind::OmitFirst4,
        suffix: b"",
    }, // 34
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x77\x69\x74\x68\x20",
    }, // 35
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x27",
    }, // 36
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x66\x72\x6f\x6d\x20",
    }, // 37
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x62\x79\x20",
    }, // 38
    Transform {
        prefix: b"",
        kind: Kind::OmitFirst5,
        suffix: b"",
    }, // 39
    Transform {
        prefix: b"",
        kind: Kind::OmitFirst6,
        suffix: b"",
    }, // 40
    Transform {
        prefix: b"\x20\x74\x68\x65\x20",
        kind: Kind::Identity,
        suffix: b"",
    }, // 41
    Transform {
        prefix: b"",
        kind: Kind::OmitLast4,
        suffix: b"",
    }, // 42
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x2e\x20\x54\x68\x65\x20",
    }, // 43
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"",
    }, // 44
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x6f\x6e\x20",
    }, // 45
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x61\x73\x20",
    }, // 46
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x69\x73\x20",
    }, // 47
    Transform {
        prefix: b"",
        kind: Kind::OmitLast7,
        suffix: b"",
    }, // 48
    Transform {
        prefix: b"",
        kind: Kind::OmitLast1,
        suffix: b"\x69\x6e\x67\x20",
    }, // 49
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x0a\x09",
    }, // 50
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x3a",
    }, // 51
    Transform {
        prefix: b"\x20",
        kind: Kind::Identity,
        suffix: b"\x2e\x20",
    }, // 52
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x65\x64\x20",
    }, // 53
    Transform {
        prefix: b"",
        kind: Kind::OmitFirst9,
        suffix: b"",
    }, // 54
    Transform {
        prefix: b"",
        kind: Kind::OmitFirst7,
        suffix: b"",
    }, // 55
    Transform {
        prefix: b"",
        kind: Kind::OmitLast6,
        suffix: b"",
    }, // 56
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x28",
    }, // 57
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x2c\x20",
    }, // 58
    Transform {
        prefix: b"",
        kind: Kind::OmitLast8,
        suffix: b"",
    }, // 59
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x61\x74\x20",
    }, // 60
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x6c\x79\x20",
    }, // 61
    Transform {
        prefix: b"\x20\x74\x68\x65\x20",
        kind: Kind::Identity,
        suffix: b"\x20\x6f\x66\x20",
    }, // 62
    Transform {
        prefix: b"",
        kind: Kind::OmitLast5,
        suffix: b"",
    }, // 63
    Transform {
        prefix: b"",
        kind: Kind::OmitLast9,
        suffix: b"",
    }, // 64
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseFirst,
        suffix: b"\x2c\x20",
    }, // 65
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x22",
    }, // 66
    Transform {
        prefix: b"\x2e",
        kind: Kind::Identity,
        suffix: b"\x28",
    }, // 67
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x20",
    }, // 68
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x22\x3e",
    }, // 69
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x3d\x22",
    }, // 70
    Transform {
        prefix: b"\x20",
        kind: Kind::Identity,
        suffix: b"\x2e",
    }, // 71
    Transform {
        prefix: b"\x2e\x63\x6f\x6d\x2f",
        kind: Kind::Identity,
        suffix: b"",
    }, // 72
    Transform {
        prefix: b"\x20\x74\x68\x65\x20",
        kind: Kind::Identity,
        suffix: b"\x20\x6f\x66\x20\x74\x68\x65\x20",
    }, // 73
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x27",
    }, // 74
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x2e\x20\x54\x68\x69\x73\x20",
    }, // 75
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x2c",
    }, // 76
    Transform {
        prefix: b"\x2e",
        kind: Kind::Identity,
        suffix: b"\x20",
    }, // 77
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x28",
    }, // 78
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x2e",
    }, // 79
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x20\x6e\x6f\x74\x20",
    }, // 80
    Transform {
        prefix: b"\x20",
        kind: Kind::Identity,
        suffix: b"\x3d\x22",
    }, // 81
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x65\x72\x20",
    }, // 82
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseAll,
        suffix: b"\x20",
    }, // 83
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x61\x6c\x20",
    }, // 84
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseAll,
        suffix: b"",
    }, // 85
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x3d\x27",
    }, // 86
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x22",
    }, // 87
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x2e\x20",
    }, // 88
    Transform {
        prefix: b"\x20",
        kind: Kind::Identity,
        suffix: b"\x28",
    }, // 89
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x66\x75\x6c\x20",
    }, // 90
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseFirst,
        suffix: b"\x2e\x20",
    }, // 91
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x69\x76\x65\x20",
    }, // 92
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x6c\x65\x73\x73\x20",
    }, // 93
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x27",
    }, // 94
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x65\x73\x74\x20",
    }, // 95
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseFirst,
        suffix: b"\x2e",
    }, // 96
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x22\x3e",
    }, // 97
    Transform {
        prefix: b"\x20",
        kind: Kind::Identity,
        suffix: b"\x3d\x27",
    }, // 98
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x2c",
    }, // 99
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x69\x7a\x65\x20",
    }, // 100
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x2e",
    }, // 101
    Transform {
        prefix: b"\xc2\xa0",
        kind: Kind::Identity,
        suffix: b"",
    }, // 102
    Transform {
        prefix: b"\x20",
        kind: Kind::Identity,
        suffix: b"\x2c",
    }, // 103
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x3d\x22",
    }, // 104
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x3d\x22",
    }, // 105
    Transform {
        prefix: b"",
        kind: Kind::Identity,
        suffix: b"\x6f\x75\x73\x20",
    }, // 106
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x2c\x20",
    }, // 107
    Transform {
        prefix: b"",
        kind: Kind::UppercaseFirst,
        suffix: b"\x3d\x27",
    }, // 108
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseFirst,
        suffix: b"\x2c",
    }, // 109
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseAll,
        suffix: b"\x3d\x22",
    }, // 110
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseAll,
        suffix: b"\x2c\x20",
    }, // 111
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x2c",
    }, // 112
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x28",
    }, // 113
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x2e\x20",
    }, // 114
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseAll,
        suffix: b"\x2e",
    }, // 115
    Transform {
        prefix: b"",
        kind: Kind::UppercaseAll,
        suffix: b"\x3d\x27",
    }, // 116
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseAll,
        suffix: b"\x2e\x20",
    }, // 117
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseFirst,
        suffix: b"\x3d\x22",
    }, // 118
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseAll,
        suffix: b"\x3d\x27",
    }, // 119
    Transform {
        prefix: b"\x20",
        kind: Kind::UppercaseFirst,
        suffix: b"\x3d\x27",
    }, // 120
];

#[cfg(test)]
mod tests {
    use super::*;

    fn transformed(index: usize, word: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        assert!(apply(index, word, &mut out), "transform {index} exists");
        out
    }

    #[test]
    fn there_are_a_hundred_and_twenty_one_of_them() {
        assert_eq!(TRANSFORMS.len(), 121);
    }

    #[test]
    fn the_first_transform_leaves_the_word_alone() {
        assert_eq!(transformed(0, b"work"), b"work");
    }

    #[test]
    fn the_common_ones_add_a_space() {
        assert_eq!(transformed(1, b"work"), b"work ");
        assert_eq!(transformed(2, b"work"), b" work ");
    }

    #[test]
    fn omitting_runs_from_either_end() {
        assert_eq!(transformed(3, b"work"), b"ork");
        let omit_last = TRANSFORMS
            .iter()
            .position(|t| t.kind == Kind::OmitLast1 && t.prefix.is_empty() && t.suffix.is_empty())
            .expect("a plain omit-last-one");
        assert_eq!(transformed(omit_last, b"work"), b"wor");
    }

    #[test]
    fn uppercasing_touches_the_first_character_or_all_of_them() {
        assert_eq!(transformed(4, b"work"), b"Work ");
        let all = TRANSFORMS
            .iter()
            .position(|t| t.kind == Kind::UppercaseAll)
            .expect("an uppercase-all");
        assert_eq!(transformed(all, b"work"), {
            let mut expected = Vec::from(TRANSFORMS[all].prefix);
            expected.extend_from_slice(b"WORK");
            expected.extend_from_slice(TRANSFORMS[all].suffix);
            expected
        });
    }

    #[test]
    fn omitting_more_than_the_word_holds_leaves_nothing() {
        let omit_nine = TRANSFORMS
            .iter()
            .position(|t| t.kind == Kind::OmitFirst9)
            .expect("an omit-first-nine");
        let out = transformed(omit_nine, b"work");
        assert_eq!(
            out.len(),
            TRANSFORMS[omit_nine].prefix.len() + TRANSFORMS[omit_nine].suffix.len()
        );
    }

    #[test]
    fn an_index_past_the_table_is_refused() {
        let mut out = Vec::new();
        assert!(!apply(121, b"work", &mut out));
        assert!(out.is_empty());
    }
}
