//! Brotli's static dictionary: 122,784 bytes every stream may copy
//! from as though it were already decoded.
//!
//! A Brotli copy whose distance reaches past everything produced so
//! far is not an error — it is a reference into a fixed word list the
//! format carries, which is how a stream can be shorter than its own
//! first occurrence of a word. Font data is not text, so this looked
//! skippable; it is not. A stream of 16-bit deltas happens to contain
//! runs like `00 01 02 03 04 05 06 07`, and those runs *are* in the
//! dictionary, so an encoder reaches for them. Leaving it out stops
//! a real font at the 425th byte.
//!
//! The layout is the one thing worth understanding. Words are stored
//! by length, shortest first, each length holding a power-of-two
//! count:
//!
//! ```text
//!   length 4: 1024 words ──┐
//!   length 5: 1024 words   ├─ laid end to end, no separators
//!   length 6: 2048 words ──┘
//!   …
//! ```
//!
//! So a reference carries a length and an index, and the byte offset
//! is arithmetic. The offsets are *computed* from the counts rather
//! than written down a second time: the two must agree, and a table
//! that can disagree with another table eventually does.
//!
//! What is not implemented: the 121 **transforms**. A reference may
//! ask for a word with a prefix, a suffix, a case change or some
//! bytes trimmed, and the transform number rides above the word index
//! in the same field. Transform 0 is the identity and is what binary
//! data uses; anything else gets an error rather than the untouched
//! word, because a silently untransformed word is wrong output that
//! looks like right output.

use super::BrotliError;

/// The dictionary itself.
static WORDS: &[u8] = include_bytes!("dictionary.bin");

/// The shortest and longest word the dictionary holds.
pub(super) const MIN_LENGTH: usize = 4;
pub(super) const MAX_LENGTH: usize = 24;

/// How many words of each length there are, as a power of two.
///
/// Indexed by word length, so the first four entries are padding.
const SIZE_BITS: [u8; MAX_LENGTH + 1] = [
    0, 0, 0, 0, 10, 10, 11, 11, 10, 10, 10, 10, 10, 9, 9, 8, 7, 7, 8, 7, 7, 6, 6, 5, 5,
];

/// Where each length's words start, derived from the counts.
const OFFSETS: [usize; MAX_LENGTH + 2] = build_offsets();

const fn build_offsets() -> [usize; MAX_LENGTH + 2] {
    let mut offsets = [0usize; MAX_LENGTH + 2];
    let mut length = MIN_LENGTH;
    while length <= MAX_LENGTH {
        offsets[length + 1] = offsets[length] + length * (1usize << SIZE_BITS[length]);
        length += 1;
    }
    offsets
}

/// One dictionary reference, as the distance encodes it.
pub(super) struct Reference {
    pub(super) index: usize,
    pub(super) transform: usize,
}

/// Split the word id a distance carries into an index and a
/// transform.
///
/// The index occupies the low bits — as many as this length has words
/// — and the transform number sits above them.
pub(super) fn split(length: usize, word_id: usize) -> Result<Reference, BrotliError> {
    if !(MIN_LENGTH..=MAX_LENGTH).contains(&length) {
        return Err(BrotliError::Malformed);
    }
    let bits = u32::from(SIZE_BITS[length]);
    Ok(Reference {
        index: word_id & ((1usize << bits) - 1),
        transform: word_id >> bits,
    })
}

/// The untransformed word.
pub(super) fn word(length: usize, index: usize) -> Result<&'static [u8], BrotliError> {
    if !(MIN_LENGTH..=MAX_LENGTH).contains(&length) {
        return Err(BrotliError::Malformed);
    }
    if index >= 1usize << SIZE_BITS[length] {
        return Err(BrotliError::Malformed);
    }
    let start = OFFSETS[length] + index * length;
    WORDS
        .get(start..start + length)
        .ok_or(BrotliError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counts_account_for_every_byte() {
        // The strongest check available without a second copy of the
        // table: each length's words sit end to end, so the offsets
        // computed from the counts must land exactly on the end of
        // the file. One wrong count and this misses.
        assert_eq!(OFFSETS[MAX_LENGTH + 1], WORDS.len());
        assert_eq!(WORDS.len(), 122_784);
    }

    #[test]
    fn the_dictionary_starts_with_its_four_letter_words() {
        assert_eq!(word(4, 0).expect("the first word"), b"time");
        assert_eq!(word(4, 1).expect("the second"), b"down");
        assert_eq!(word(4, 2).expect("the third"), b"life");
    }

    #[test]
    fn each_length_starts_where_the_one_before_it_ends() {
        for length in MIN_LENGTH..MAX_LENGTH {
            assert_eq!(
                OFFSETS[length + 1],
                OFFSETS[length] + length * (1 << SIZE_BITS[length]),
                "length {length}"
            );
        }
    }

    #[test]
    fn a_word_index_past_the_end_of_its_length_is_refused() {
        let last = (1usize << SIZE_BITS[4]) - 1;
        assert!(word(4, last).is_ok());
        assert_eq!(word(4, last + 1).err(), Some(BrotliError::Malformed));
    }

    #[test]
    fn a_length_the_dictionary_has_no_words_for_is_refused() {
        assert_eq!(word(3, 0).err(), Some(BrotliError::Malformed));
        assert_eq!(word(25, 0).err(), Some(BrotliError::Malformed));
    }

    #[test]
    fn a_word_id_splits_into_an_index_and_a_transform() {
        // Length 4 has 1024 words, so ten bits of index.
        let reference = split(4, 1024 + 7).expect("a valid length");
        assert_eq!((reference.index, reference.transform), (7, 1));
        let reference = split(4, 7).expect("a valid length");
        assert_eq!((reference.index, reference.transform), (7, 0));
    }
}
