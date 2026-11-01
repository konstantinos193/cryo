//! Brotli's prefix codes: how one is read from the stream, and how a
//! symbol is read with it.
//!
//! Two shapes exist and the first two bits of a code say which:
//!
//! - a **simple** code, up to four symbols with lengths the format
//!   fixes, written out as raw symbol numbers;
//! - a **complex** code, where the code lengths are themselves
//!   compressed with a prefix code over code lengths — the same idea
//!   DEFLATE uses, with run-length symbols for repeats.
//!
//! Both end as canonical codes: symbols sorted by length then by
//! value, codes assigned in that order. Decoding walks a bit at a
//! time rather than building a lookup table — slower per symbol, but
//! a table's size depends on the maximum length and a malformed
//! stream can claim any length it likes.

use super::bits::Bits;
use super::BrotliError;

/// The longest a Brotli prefix code may be.
const MAX_LENGTH: usize = 15;

/// The order code-length code lengths are written in.
///
/// Not ascending: the lengths most likely to be used come first, so a
/// code that needs only a few of them can stop early.
const LENGTH_ORDER: [usize; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// The fixed code that the code-length code lengths themselves use.
///
/// Lengths for symbols 0..=5, from which the canonical code is built.
const LENGTH_CODE_LENGTHS: [u8; 6] = [2, 4, 3, 2, 2, 4];

/// `16` repeats the previous non-zero length, `17` repeats a zero.
const REPEAT_PREVIOUS: usize = 16;
const REPEAT_ZERO: usize = 17;

/// A decoded prefix code.
pub(super) struct Huffman {
    /// How many symbols have each code length.
    counts: [usize; MAX_LENGTH + 1],
    /// The first code of each length, and where its symbols start in
    /// `sorted` — the canonical decoding tables.
    first_code: [u32; MAX_LENGTH + 1],
    first_index: [usize; MAX_LENGTH + 1],
    /// Symbols ordered by (length, value).
    sorted: Vec<u16>,
    /// Set when the code has exactly one symbol, which then costs no
    /// bits at all — a case the canonical walk cannot express.
    only: Option<u16>,
}

impl Huffman {
    /// Build from code lengths, rejecting an over- or under-full code.
    ///
    /// A code that does not exactly fill its space is not merely
    /// wasteful: decoding would run off the end of the table for some
    /// input, so it is refused here instead.
    pub(super) fn from_lengths(lengths: Vec<u8>) -> Result<Self, BrotliError> {
        let mut counts = [0usize; MAX_LENGTH + 1];
        for &length in &lengths {
            if usize::from(length) > MAX_LENGTH {
                return Err(BrotliError::Malformed);
            }
            counts[usize::from(length)] += 1;
        }
        let present: Vec<u16> = lengths
            .iter()
            .enumerate()
            .filter(|(_, &length)| length > 0)
            .map(|(symbol, _)| symbol as u16)
            .collect();
        if present.len() == 1 {
            return Ok(Self {
                counts,
                first_code: [0; MAX_LENGTH + 1],
                first_index: [0; MAX_LENGTH + 1],
                sorted: present.clone(),
                only: Some(present[0]),
            });
        }

        let mut first_code = [0u32; MAX_LENGTH + 1];
        let mut first_index = [0usize; MAX_LENGTH + 1];
        let mut code = 0u32;
        let mut index = 0usize;
        let mut space = 1u32 << MAX_LENGTH;
        for length in 1..=MAX_LENGTH {
            first_code[length] = code;
            first_index[length] = index;
            let count = counts[length] as u32;
            space = space
                .checked_sub(count << (MAX_LENGTH - length))
                .ok_or(BrotliError::Malformed)?;
            code = (code + count) << 1;
            index += counts[length];
        }
        if space != 0 {
            return Err(BrotliError::Malformed);
        }

        let mut sorted = vec![0u16; index];
        let mut next = first_index;
        for (symbol, &length) in lengths.iter().enumerate() {
            if length == 0 {
                continue;
            }
            let slot = &mut next[usize::from(length)];
            sorted[*slot] = symbol as u16;
            *slot += 1;
        }

        Ok(Self {
            counts,
            first_code,
            first_index,
            sorted,
            only: None,
        })
    }

    /// Read one symbol.
    pub(super) fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, BrotliError> {
        if let Some(symbol) = self.only {
            return Ok(symbol);
        }
        let mut code = 0u32;
        for length in 1..=MAX_LENGTH {
            // Codes are written most significant bit first even though
            // the stream is read least significant bit first, so each
            // new bit joins at the bottom.
            code = (code << 1) | bits.read_one()?;
            let count = self.counts[length];
            if count > 0 && code < self.first_code[length] + count as u32 {
                let offset = (code - self.first_code[length]) as usize;
                return self
                    .sorted
                    .get(self.first_index[length] + offset)
                    .copied()
                    .ok_or(BrotliError::Malformed);
            }
        }
        Err(BrotliError::Malformed)
    }
}

/// Read a prefix code over `alphabet_size` symbols from the stream.
pub(super) fn read_code(bits: &mut Bits<'_>, alphabet_size: usize) -> Result<Huffman, BrotliError> {
    let kind = bits.read(2)?;
    if kind == 1 {
        return read_simple(bits, alphabet_size);
    }
    read_complex(bits, alphabet_size, kind as usize)
}

/// Up to four symbols, with lengths the format fixes by count.
fn read_simple(bits: &mut Bits<'_>, alphabet_size: usize) -> Result<Huffman, BrotliError> {
    let width = alphabet_bits(alphabet_size);
    let count = bits.read(2)? as usize + 1;
    let mut symbols = Vec::with_capacity(count);
    for _ in 0..count {
        let symbol = bits.read(width)? as usize;
        if symbol >= alphabet_size || symbols.contains(&symbol) {
            return Err(BrotliError::Malformed);
        }
        symbols.push(symbol);
    }

    let mut lengths = vec![0u8; alphabet_size];
    match count {
        1 => lengths[symbols[0]] = 0,
        2 => {
            // Two symbols share one bit, and the smaller comes first.
            symbols.sort_unstable();
            lengths[symbols[0]] = 1;
            lengths[symbols[1]] = 1;
        }
        3 => {
            // The last two share the longer code, in numeric order.
            let (first, rest) = symbols.split_first().expect("three symbols");
            let mut rest = rest.to_vec();
            rest.sort_unstable();
            lengths[*first] = 1;
            lengths[rest[0]] = 2;
            lengths[rest[1]] = 2;
        }
        _ => {
            // One bit says whether the four are balanced or skewed.
            if bits.read(1)? == 0 {
                symbols.sort_unstable();
                for symbol in &symbols {
                    lengths[*symbol] = 2;
                }
            } else {
                // Lengths 1, 2, 3, 3 — handed out in the order the
                // symbols were *written*, not in value order. Only
                // the pair sharing the longest code is sorted, and
                // that is because they are equal in every other way.
                //
                // Sorting the middle one into place as well looks
                // harmless and is not: it gives a different symbol
                // the two-bit code, so a later symbol costs a
                // different number of bits and the whole stream
                // slides.
                lengths[symbols[0]] = 1;
                lengths[symbols[1]] = 2;
                lengths[symbols[2]] = 3;
                lengths[symbols[3]] = 3;
            }
        }
    }

    if count == 1 {
        let mut counts = [0usize; MAX_LENGTH + 1];
        counts[0] = lengths.len();
        return Ok(Huffman {
            counts,
            first_code: [0; MAX_LENGTH + 1],
            first_index: [0; MAX_LENGTH + 1],
            sorted: vec![symbols[0] as u16],
            only: Some(symbols[0] as u16),
        });
    }
    Huffman::from_lengths(lengths)
}

/// Code lengths compressed with a code over code lengths.
fn read_complex(
    bits: &mut Bits<'_>,
    alphabet_size: usize,
    skip: usize,
) -> Result<Huffman, BrotliError> {
    let meta = Huffman::from_lengths(LENGTH_CODE_LENGTHS.to_vec())?;

    // The code-length code's own lengths, in the order above. It
    // stops as soon as the lengths read can fill a complete code —
    // the remaining entries are then zero by definition.
    let mut code_lengths = [0u8; 18];
    let mut space = 32u32;
    let mut non_zero = 0;
    for &slot in LENGTH_ORDER.iter().skip(skip) {
        let length = meta.decode(bits)? as usize;
        code_lengths[slot] = length as u8;
        if length > 0 {
            space = space
                .checked_sub(32 >> length)
                .ok_or(BrotliError::Malformed)?;
            non_zero += 1;
            if space == 0 {
                break;
            }
        }
    }
    if non_zero != 1 && space != 0 {
        return Err(BrotliError::Malformed);
    }
    let lengths_code = Huffman::from_lengths(code_lengths.to_vec())?;

    // Now the symbol lengths themselves, with two run-length codes.
    let mut lengths = vec![0u8; alphabet_size];
    let mut index = 0usize;
    let mut previous = 8u8;
    let mut repeat = 0u32;
    let mut repeat_symbol = 0u8;
    let mut space = 1u32 << MAX_LENGTH;
    while index < alphabet_size && space > 0 {
        let symbol = lengths_code.decode(bits)? as usize;
        if symbol < REPEAT_PREVIOUS {
            lengths[index] = symbol as u8;
            index += 1;
            if symbol != 0 {
                previous = symbol as u8;
                space -= (1 << MAX_LENGTH) >> symbol;
            }
            repeat = 0;
            continue;
        }

        // A repeat *extends* the run it continues rather than
        // starting over, which is how a run of hundreds is written in
        // a handful of symbols. The number of new entries is the
        // difference between the run's new length and its old one —
        // not its length after the shift, which is an intermediate
        // value and counts the same entries twice.
        let extra_bits = if symbol == REPEAT_ZERO { 3 } else { 2 };
        let value = if symbol == REPEAT_PREVIOUS {
            previous
        } else {
            0
        };
        if repeat_symbol != value {
            repeat = 0;
            repeat_symbol = value;
        }
        let old = repeat;
        if repeat > 0 {
            repeat = (repeat - 2) << extra_bits;
        }
        let total = repeat + 3 + bits.read(extra_bits)?;
        let added = total - old;
        for _ in 0..added {
            if index >= alphabet_size {
                return Err(BrotliError::Malformed);
            }
            lengths[index] = value;
            index += 1;
            if value != 0 {
                space = space
                    .checked_sub((1 << MAX_LENGTH) >> value)
                    .ok_or(BrotliError::Malformed)?;
            }
        }
        repeat = total;
    }
    Huffman::from_lengths(lengths)
}

/// How many bits a raw symbol number takes for this alphabet.
fn alphabet_bits(alphabet_size: usize) -> u32 {
    let mut bits = 0;
    while (1usize << bits) < alphabet_size {
        bits += 1;
    }
    bits.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_symbol_code_costs_no_bits() {
        let code = Huffman {
            counts: [0; MAX_LENGTH + 1],
            first_code: [0; MAX_LENGTH + 1],
            first_index: [0; MAX_LENGTH + 1],
            sorted: vec![5],
            only: Some(5),
        };
        let mut bits = Bits::new(&[]);
        assert_eq!(code.decode(&mut bits), Ok(5), "and reads nothing");
    }

    #[test]
    fn a_balanced_code_decodes_its_symbols() {
        // Four symbols, two bits each: codes 00, 01, 10, 11 for
        // symbols 0, 1, 2, 3 in canonical order.
        let code = Huffman::from_lengths(vec![2, 2, 2, 2]).expect("a full code");
        // Bits are read low first, and each new bit joins at the
        // bottom of the code — so the byte 0b11_10_01_00 yields
        // 0, 2, 1, 3.
        let mut bits = Bits::new(&[0b1110_0100]);
        let read: Vec<u16> = (0..4)
            .map(|_| code.decode(&mut bits).expect("ok"))
            .collect();
        assert_eq!(read, [0, 2, 1, 3]);
    }

    #[test]
    fn an_incomplete_code_is_refused() {
        // Three symbols of length two leave a quarter of the space
        // unused, so some input would decode to nothing.
        assert_eq!(
            Huffman::from_lengths(vec![2, 2, 2]).err(),
            Some(BrotliError::Malformed)
        );
    }

    #[test]
    fn an_over_full_code_is_refused() {
        assert_eq!(
            Huffman::from_lengths(vec![1, 1, 1]).err(),
            Some(BrotliError::Malformed)
        );
    }

    #[test]
    fn the_alphabet_width_is_the_bits_a_symbol_number_needs() {
        assert_eq!(alphabet_bits(2), 1);
        assert_eq!(alphabet_bits(3), 2);
        assert_eq!(alphabet_bits(256), 8);
        assert_eq!(alphabet_bits(704), 10);
    }
}
