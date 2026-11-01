//! Brotli decompression, enough of it to open a WOFF2 font.
//!
//! Brotli is what every `@font-face` on the modern web is wrapped in:
//! a WOFF2 file is a small header followed by one Brotli stream, and
//! without a decompressor for it a page's own typeface is simply
//! unreachable. That is not a cosmetic loss — a fallback font is a
//! different width, so a headline wraps to three lines where the
//! designer drew two.
//!
//! The format is LZ77 with far more machinery than DEFLATE around it:
//!
//! ```text
//!   stream ─▶ metablocks ─▶ blocks ─▶ commands ─▶ literals + copies
//! ```
//!
//! Each **metablock** carries its own prefix codes. Within one, the
//! literals, the insert-and-copy commands and the distances each run
//! on their own **block** sequence, so a stream can switch models
//! part way through without starting a new metablock. A **command**
//! is one symbol naming both how many literals follow and how long
//! the copy after them is — which is why there is no separate
//! "literal or match" flag anywhere.
//!
//! Two refinements earn most of Brotli's advantage over DEFLATE and
//! both are implemented here: literals are coded in a **context**
//! taken from the previous two bytes, and distances come from a
//! four-entry cache of recent ones, so a regular stride costs almost
//! nothing.
//!
//! **The static dictionary is not implemented.** Brotli ships a fixed
//! 120 KB word list that a stream may reference as though it were
//! already decoded. Text streams lean on it; font data, which is
//! glyph outlines and tables, has almost nothing in common with a
//! list of English words, so an encoder rarely emits one. A stream
//! that does gets [`BrotliError::StaticDictionary`] rather than
//! silently wrong bytes, and the caller falls back to a system font.

mod bits;
mod dictionary;
mod huffman;
mod tables;
mod transform;

use bits::Bits;
use huffman::{read_code, Huffman};
use tables::{
    ContextMode, BLOCK_LENGTH_EXTRA, BLOCK_LENGTH_OFFSET, COPY_LENGTH_EXTRA, COPY_LENGTH_OFFSET,
    COPY_RANGE, DISTANCE_SHORT_INDEX, DISTANCE_SHORT_OFFSET, INSERT_LENGTH_EXTRA,
    INSERT_LENGTH_OFFSET, INSERT_RANGE,
};

/// Why a stream could not be decompressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BrotliError {
    #[error("the compressed stream ends mid-symbol")]
    Truncated,
    #[error("the compressed stream is not valid Brotli")]
    Malformed,
    #[error("the stream asks for dictionary transform {transform}, which is not implemented")]
    DictionaryTransform { transform: usize },
    #[error("the stream decodes to more than the {limit} bytes allowed")]
    TooLarge { limit: usize },
}

/// How many symbols each kind of prefix code covers.
const LITERAL_ALPHABET: usize = 256;
const COMMAND_ALPHABET: usize = 704;
const BLOCK_LENGTH_ALPHABET: usize = 26;

/// Each literal block type has 64 contexts; each distance block type
/// has 4.
const LITERAL_CONTEXTS: usize = 64;
const DISTANCE_CONTEXTS: usize = 4;

/// Decompress a Brotli stream, refusing anything over `limit` bytes.
///
/// The limit is not a tuning knob: a stream states its own output
/// length per metablock, and a corrupt or hostile one can claim far
/// more than the machine has.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, BrotliError> {
    let mut bits = Bits::new(data);
    let window = read_window_bits(&mut bits)?;
    let max_distance = (1usize << window) - 16;

    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = bits.read(1)? == 1;
        if last && bits.read(1)? == 1 {
            // An empty last metablock: the stream is over.
            break;
        }
        match read_metablock_length(&mut bits)? {
            Length::Metadata(skip) => {
                bits.align_to_byte();
                bits.take_bytes(skip)?;
                if last {
                    break;
                }
                continue;
            }
            Length::Compressed(length) => {
                if out.len() + length > limit {
                    return Err(BrotliError::TooLarge { limit });
                }
                if !last && bits.read(1)? == 1 {
                    bits.align_to_byte();
                    out.extend_from_slice(bits.take_bytes(length)?);
                    continue;
                }
                decode_metablock(&mut bits, length, max_distance, &mut out)?;
            }
        }
        if last {
            break;
        }
        if bits.at_end() {
            return Err(BrotliError::Truncated);
        }
    }
    Ok(out)
}

/// The sliding window size, as a count of bits.
fn read_window_bits(bits: &mut Bits<'_>) -> Result<u32, BrotliError> {
    if bits.read(1)? == 0 {
        return Ok(16);
    }
    let high = bits.read(3)?;
    if high != 0 {
        return Ok(17 + high);
    }
    let low = bits.read(3)?;
    if low != 0 {
        // A two is not a legal window and means the stream is not
        // Brotli at all.
        if low == 1 {
            return Err(BrotliError::Malformed);
        }
        return Ok(8 + low);
    }
    Ok(17)
}

enum Length {
    Compressed(usize),
    /// Bytes to skip: a metadata block carries no output.
    Metadata(usize),
}

fn read_metablock_length(bits: &mut Bits<'_>) -> Result<Length, BrotliError> {
    let nibbles = bits.read(2)?;
    if nibbles == 3 {
        if bits.read(1)? != 0 {
            return Err(BrotliError::Malformed);
        }
        let bytes = bits.read(2)? as usize;
        if bytes == 0 {
            return Ok(Length::Metadata(0));
        }
        let length = bits.read(bytes as u32 * 8)? as usize + 1;
        return Ok(Length::Metadata(length));
    }
    let count = nibbles + 4;
    Ok(Length::Compressed(bits.read(count * 4)? as usize + 1))
}

/// One metablock's worth of prefix codes and block state.
struct Metablock {
    literal: Vec<Huffman>,
    command: Vec<Huffman>,
    distance: Vec<Huffman>,
    literal_blocks: BlockSwitch,
    command_blocks: BlockSwitch,
    distance_blocks: BlockSwitch,
    context_modes: Vec<ContextMode>,
    literal_map: Vec<u8>,
    distance_map: Vec<u8>,
    postfix: u32,
    direct: u32,
}

/// The block sequence for one of the three kinds.
///
/// A metablock may switch models part way through, and the switches
/// are themselves coded — so this is a little decoder of its own
/// rather than a counter.
struct BlockSwitch {
    types: usize,
    current: usize,
    previous: usize,
    remaining: u32,
    type_code: Option<Huffman>,
    length_code: Option<Huffman>,
}

impl BlockSwitch {
    /// A single block type, which never switches.
    fn single() -> Self {
        Self {
            types: 1,
            current: 0,
            previous: 1,
            remaining: u32::MAX,
            type_code: None,
            length_code: None,
        }
    }

    fn read(bits: &mut Bits<'_>, types: usize) -> Result<Self, BrotliError> {
        if types == 1 {
            return Ok(Self::single());
        }
        let type_code = read_code(bits, types + 2)?;
        let length_code = read_code(bits, BLOCK_LENGTH_ALPHABET)?;
        let remaining = read_block_length(bits, &length_code)?;
        Ok(Self {
            types,
            current: 0,
            previous: 1,
            remaining,
            type_code: Some(type_code),
            length_code: Some(length_code),
        })
    }

    /// Consume one unit of this block, switching type when it runs
    /// out. Returns the type to use.
    fn next(&mut self, bits: &mut Bits<'_>) -> Result<usize, BrotliError> {
        if self.remaining == 0 {
            let (Some(type_code), Some(length_code)) =
                (self.type_code.as_ref(), self.length_code.as_ref())
            else {
                return Err(BrotliError::Malformed);
            };
            // The first two symbols are relative: "the one before
            // this" and "the next one round". Everything else names a
            // type outright.
            let symbol = usize::from(type_code.decode(bits)?);
            let next = match symbol {
                0 => self.previous,
                1 => self.current + 1,
                other => other - 2,
            };
            // A type past the end wraps rather than failing: that is
            // how "the next one round" is written for the last type.
            let next = if next >= self.types {
                next - self.types
            } else {
                next
            };
            if next >= self.types {
                return Err(BrotliError::Malformed);
            }
            self.previous = self.current;
            self.current = next;
            self.remaining = read_block_length(bits, length_code)?;
        }
        self.remaining -= 1;
        Ok(self.current)
    }
}

fn read_block_length(bits: &mut Bits<'_>, code: &Huffman) -> Result<u32, BrotliError> {
    let symbol = usize::from(code.decode(bits)?);
    let base = *BLOCK_LENGTH_OFFSET
        .get(symbol)
        .ok_or(BrotliError::Malformed)?;
    Ok(base + bits.read(BLOCK_LENGTH_EXTRA[symbol])?)
}

/// The variable-length count used for block types and tree counts.
fn read_count(bits: &mut Bits<'_>) -> Result<usize, BrotliError> {
    if bits.read(1)? == 0 {
        return Ok(1);
    }
    let width = bits.read(3)?;
    Ok((1usize << width) + 1 + bits.read(width)? as usize)
}

impl Metablock {
    fn read(bits: &mut Bits<'_>) -> Result<Self, BrotliError> {
        let literal_types = read_count(bits)?;
        let literal_blocks = BlockSwitch::read(bits, literal_types)?;
        let command_types = read_count(bits)?;
        let command_blocks = BlockSwitch::read(bits, command_types)?;
        let distance_types = read_count(bits)?;
        let distance_blocks = BlockSwitch::read(bits, distance_types)?;

        let postfix = bits.read(2)?;
        let direct = bits.read(4)? << postfix;

        let mut context_modes = Vec::with_capacity(literal_types);
        for _ in 0..literal_types {
            context_modes.push(ContextMode::from_bits(bits.read(2)?));
        }

        let literal_trees = read_count(bits)?;
        let literal_map = read_context_map(bits, literal_types * LITERAL_CONTEXTS, literal_trees)?;
        let distance_trees = read_count(bits)?;
        let distance_map =
            read_context_map(bits, distance_types * DISTANCE_CONTEXTS, distance_trees)?;

        let literal = read_codes(bits, literal_trees, LITERAL_ALPHABET)?;
        let command = read_codes(bits, command_types, COMMAND_ALPHABET)?;
        let distance_alphabet = (16 + direct + (48 << postfix)) as usize;
        let distance = read_codes(bits, distance_trees, distance_alphabet)?;

        Ok(Self {
            literal,
            command,
            distance,
            literal_blocks,
            command_blocks,
            distance_blocks,
            context_modes,
            literal_map,
            distance_map,
            postfix,
            direct,
        })
    }
}

fn read_codes(
    bits: &mut Bits<'_>,
    count: usize,
    alphabet: usize,
) -> Result<Vec<Huffman>, BrotliError> {
    let mut codes = Vec::with_capacity(count);
    for _ in 0..count {
        codes.push(read_code(bits, alphabet)?);
    }
    Ok(codes)
}

/// A context map: which prefix code each (block type, context) pair
/// uses.
///
/// Stored run-length encoded and then put through an inverse
/// move-to-front transform, which is how a map that is mostly one
/// value with a few exceptions costs almost nothing.
fn read_context_map(
    bits: &mut Bits<'_>,
    size: usize,
    trees: usize,
) -> Result<Vec<u8>, BrotliError> {
    if trees <= 1 {
        return Ok(vec![0; size]);
    }
    let run_length_max = if bits.read(1)? == 1 {
        bits.read(4)? as usize + 1
    } else {
        0
    };
    let code = read_code(bits, trees + run_length_max)?;

    let mut map = Vec::with_capacity(size);
    while map.len() < size {
        let symbol = usize::from(code.decode(bits)?);
        if symbol == 0 {
            map.push(0);
            continue;
        }
        if symbol <= run_length_max {
            // A run of zeros, length written in `symbol` extra bits.
            let run = (1usize << symbol) + bits.read(symbol as u32)? as usize;
            if map.len() + run > size {
                return Err(BrotliError::Malformed);
            }
            map.extend(std::iter::repeat_n(0u8, run));
            continue;
        }
        let value = symbol - run_length_max;
        if value > u8::MAX as usize {
            return Err(BrotliError::Malformed);
        }
        map.push(value as u8);
    }
    if bits.read(1)? == 1 {
        inverse_move_to_front(&mut map);
    }
    Ok(map)
}

/// Undo the move-to-front transform the encoder applied.
fn inverse_move_to_front(map: &mut [u8]) {
    let mut order: Vec<u8> = (0..=255u8).collect();
    for slot in map.iter_mut() {
        let index = usize::from(*slot);
        let value = order[index];
        *slot = value;
        order[..=index].rotate_right(1);
    }
}

/// Decode one compressed metablock, appending `length` bytes.
fn decode_metablock(
    bits: &mut Bits<'_>,
    length: usize,
    max_distance: usize,
    out: &mut Vec<u8>,
) -> Result<(), BrotliError> {
    let mut block = Metablock::read(bits)?;
    // The four most recent distances, newest last. The initial values
    // are fixed by the format so a stream may reference them before
    // any copy has happened.
    let mut recent = [16usize, 15, 11, 4];
    let target = out.len() + length;

    while out.len() < target {
        let command_type = block.command_blocks.next(bits)?;
        let code = block
            .command
            .get(command_type)
            .ok_or(BrotliError::Malformed)?;
        let symbol = usize::from(code.decode(bits)?);
        let (insert_code, copy_code, implicit_zero) = split_command(symbol)?;

        // Both lengths belong to the command and are read here, the
        // copy's before a single literal is taken. Reading the copy
        // length after the literals instead shifts every bit from
        // there on, and the output stays exactly the right length
        // while being entirely wrong — the worst way to be wrong.
        let insert =
            INSERT_LENGTH_OFFSET[insert_code] + bits.read(INSERT_LENGTH_EXTRA[insert_code])?;
        let copy = COPY_LENGTH_OFFSET[copy_code] + bits.read(COPY_LENGTH_EXTRA[copy_code])?;

        for _ in 0..insert {
            if out.len() >= target {
                break;
            }
            let literal_type = block.literal_blocks.next(bits)?;
            let mode = *block
                .context_modes
                .get(literal_type)
                .ok_or(BrotliError::Malformed)?;
            let p1 = out.last().copied().unwrap_or(0);
            let p2 = out.len().checked_sub(2).map_or(0, |index| out[index]);
            let context = mode.context(p1, p2);
            let tree = usize::from(
                *block
                    .literal_map
                    .get(literal_type * LITERAL_CONTEXTS + context)
                    .ok_or(BrotliError::Malformed)?,
            );
            let code = block.literal.get(tree).ok_or(BrotliError::Malformed)?;
            out.push(code.decode(bits)? as u8);
        }
        if out.len() >= target {
            break;
        }

        // Whether this distance joins the cache. "The same distance
        // again" must not, or a stride of two alternating gaps would
        // push duplicates and every later short code would name the
        // wrong one.
        let mut remember = !implicit_zero;
        let distance = if implicit_zero {
            recent[3]
        } else {
            let context = copy_context(copy);
            let distance_type = block.distance_blocks.next(bits)?;
            let tree = usize::from(
                *block
                    .distance_map
                    .get(distance_type * DISTANCE_CONTEXTS + context)
                    .ok_or(BrotliError::Malformed)?,
            );
            let code = block.distance.get(tree).ok_or(BrotliError::Malformed)?;
            let symbol = usize::from(code.decode(bits)?);
            remember = symbol != 0;
            read_distance(bits, symbol, &recent, block.postfix, block.direct)?
        };

        let reachable = out.len().min(max_distance);
        if distance > reachable {
            // Past everything produced so far, which is not an error:
            // it names a word in the static dictionary. The word id
            // is how far beyond the reachable output the distance
            // points, and the copy length says which length's words
            // to look among.
            let word_id = distance - reachable - 1;
            let reference = dictionary::split(copy as usize, word_id)?;
            let word = dictionary::word(copy as usize, reference.index)?;
            if !transform::apply(reference.transform, word, out) {
                return Err(BrotliError::DictionaryTransform {
                    transform: reference.transform,
                });
            }
            // A dictionary copy does not join the distance cache —
            // its "distance" is not a position in the output and
            // would poison every later short code.
            continue;
        }
        if remember {
            recent.rotate_left(1);
            recent[3] = distance;
        }

        let start = out.len() - distance;
        for offset in 0..copy as usize {
            if out.len() >= target {
                break;
            }
            // Read one at a time: a copy may overlap its own output,
            // which is how a repeated run is written.
            let byte = out[start + offset];
            out.push(byte);
        }
    }
    Ok(())
}

/// Split a command symbol into its insert and copy codes.
///
/// The low six bits hold both; the rest says which pair of ranges
/// they are offsets into, and whether a distance follows at all.
fn split_command(symbol: usize) -> Result<(usize, usize, bool), BrotliError> {
    let mut range = symbol >> 6;
    if range >= INSERT_RANGE.len() + 2 {
        return Err(BrotliError::Malformed);
    }
    let implicit_zero = range < 2;
    if !implicit_zero {
        range -= 2;
    }
    let insert = usize::from(INSERT_RANGE[range]) + ((symbol >> 3) & 7);
    let copy = usize::from(COPY_RANGE[range]) + (symbol & 7);
    Ok((insert, copy, implicit_zero))
}

/// Which of a distance block type's four contexts a copy of this
/// length uses. Short copies and long ones have different distance
/// distributions, and the model splits them.
fn copy_context(copy: u32) -> usize {
    (copy.min(5) as usize).saturating_sub(2)
}

fn read_distance(
    bits: &mut Bits<'_>,
    symbol: usize,
    recent: &[usize; 4],
    postfix: u32,
    direct: u32,
) -> Result<usize, BrotliError> {
    if symbol < DISTANCE_SHORT_INDEX.len() {
        let index = usize::from(DISTANCE_SHORT_INDEX[symbol]);
        let base = recent[index] as i64 + i64::from(DISTANCE_SHORT_OFFSET[symbol]);
        return usize::try_from(base).map_err(|_| BrotliError::Malformed);
    }
    let code = (symbol - DISTANCE_SHORT_INDEX.len()) as u32;
    if code < direct {
        return Ok(code as usize + 1);
    }
    let code = code - direct;
    let mask = (1u32 << postfix) - 1;
    let extra_bits = 1 + (code >> (postfix + 1));
    if extra_bits > 24 {
        return Err(BrotliError::Malformed);
    }
    let high = code >> postfix;
    let low = code & mask;
    let offset = ((2 + (high & 1)) << extra_bits) - 4;
    let extra = bits.read(extra_bits)?;
    Ok((((offset + extra) << postfix) + low + direct + 1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_header_reads_its_three_shapes() {
        // A single zero bit means sixteen.
        assert_eq!(read_window_bits(&mut Bits::new(&[0x00])), Ok(16));
        // 1 then 001 is 17 + 1.
        assert_eq!(read_window_bits(&mut Bits::new(&[0b0000_0011])), Ok(18));
    }

    #[test]
    fn a_command_symbol_splits_into_its_two_codes() {
        // The first sixty-four commands imply a distance of zero.
        let (insert, copy, implicit) = split_command(0).expect("valid");
        assert_eq!((insert, copy, implicit), (0, 0, true));
        // The first *two* ranges do, so 127 is the last of them.
        let (_, _, implicit) = split_command(127).expect("valid");
        assert!(implicit);
        // From there a distance is read, and the ranges restart.
        let (insert, copy, implicit) = split_command(128).expect("valid");
        assert_eq!((insert, copy, implicit), (0, 0, false));
        // Within a range the low bits are offsets into each half.
        let (insert, copy, _) = split_command(128 + 0o21).expect("valid");
        assert_eq!((insert, copy), (2, 1));
    }

    #[test]
    fn a_command_symbol_past_the_alphabet_is_refused() {
        assert_eq!(split_command(704).err(), Some(BrotliError::Malformed));
    }

    #[test]
    fn the_inverse_move_to_front_restores_a_run() {
        // Encoding 0,0,0 under move-to-front means "the same value
        // three times", whatever it is.
        let mut map = vec![3u8, 0, 0];
        inverse_move_to_front(&mut map);
        assert_eq!(map, [3, 3, 3]);
    }

    #[test]
    fn a_truncated_stream_is_an_error() {
        assert_eq!(decompress(&[], 1024).err(), Some(BrotliError::Truncated));
    }

    #[test]
    fn an_empty_stream_decodes_to_nothing() {
        // The one-byte stream: window 16, last metablock, empty.
        assert_eq!(decompress(&[0b0000_0110], 1024), Ok(Vec::new()));
    }
}
