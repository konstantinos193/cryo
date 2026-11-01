//! DEFLATE decompression, as PNG's `IDAT` stream needs it.
//!
//! DEFLATE (RFC 1951) is a stream of *blocks*, each either stored
//! verbatim, compressed with a fixed Huffman table the format defines,
//! or compressed with a table the block carries itself. Inside a
//! compressed block the symbols are either literal bytes or
//! back-references — "copy `length` bytes from `distance` back" —
//! which is why the output has to be built as one growing buffer and
//! cannot be streamed in fixed-size pieces.
//!
//! The back-reference is the part worth knowing: a copy may *overlap*
//! its own output, reading bytes this very copy is writing. That is
//! not a bug to defend against but the format's way of writing runs —
//! a distance of 1 and a length of 100 repeats one byte a hundred
//! times — so the copy has to be byte by byte, never a block move.
//!
//! This is only the decompressor. Nothing here compresses, because
//! nothing in a browser needs to.

use std::fmt;

/// Why a stream could not be decompressed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InflateError {
    /// The stream ended in the middle of something.
    Truncated,
    /// A block header that is not one of the three defined kinds.
    BadBlockType,
    /// A stored block whose length and its complement disagree.
    BadStoredLength,
    /// A code that is not in the Huffman table it was read against.
    BadCode,
    /// A back-reference pointing further back than the output goes.
    BadDistance,
    /// The zlib wrapper's header is not one PNG may carry.
    BadZlibHeader,
}

impl fmt::Display for InflateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Truncated => "the compressed stream ends mid-symbol",
            Self::BadBlockType => "unknown deflate block type",
            Self::BadStoredLength => "a stored block's length check failed",
            Self::BadCode => "a huffman code with no symbol",
            Self::BadDistance => "a back-reference before the start of the output",
            Self::BadZlibHeader => "not a zlib stream",
        };
        f.write_str(text)
    }
}

impl std::error::Error for InflateError {}

/// Decompress a zlib stream: a two-byte header, deflate data, and an
/// Adler-32 checksum.
///
/// PNG wraps its image data this way. The checksum is not verified —
/// a corrupt image shows as a corrupt image, and refusing to draw one
/// that decoded fine is worse than drawing it.
pub fn zlib(bytes: &[u8]) -> Result<Vec<u8>, InflateError> {
    let [cmf, flg, rest @ ..] = bytes else {
        return Err(InflateError::Truncated);
    };
    // The low nibble is the method: 8 is deflate, and PNG allows no
    // other. The header is also a check value across both bytes.
    if cmf & 0x0f != 8 || (u16::from(*cmf) * 256 + u16::from(*flg)) % 31 != 0 {
        return Err(InflateError::BadZlibHeader);
    }
    // A preset dictionary cannot appear in PNG, and we have none.
    if flg & 0x20 != 0 {
        return Err(InflateError::BadZlibHeader);
    }
    inflate(rest)
}

/// Decompress a raw deflate stream.
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>, InflateError> {
    let mut bits = BitReader::new(bytes);
    let mut out = Vec::new();

    loop {
        let last = bits.bits(1)? == 1;
        match bits.bits(2)? {
            0 => stored_block(&mut bits, &mut out)?,
            1 => {
                let (lengths, distances) = fixed_tables();
                compressed_block(&mut bits, &mut out, &lengths, &distances)?;
            }
            2 => {
                let (lengths, distances) = dynamic_tables(&mut bits)?;
                compressed_block(&mut bits, &mut out, &lengths, &distances)?;
            }
            _ => return Err(InflateError::BadBlockType),
        }
        if last {
            return Ok(out);
        }
    }
}

/// A block copied verbatim. It starts on a byte boundary.
fn stored_block(bits: &mut BitReader, out: &mut Vec<u8>) -> Result<(), InflateError> {
    bits.align();
    let length = bits.bits(16)? as usize;
    let complement = bits.bits(16)? as usize;
    if length != !complement & 0xffff {
        return Err(InflateError::BadStoredLength);
    }
    for _ in 0..length {
        out.push(bits.byte()?);
    }
    Ok(())
}

/// How many extra bits each length code carries, and its base length.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// The symbol that ends a block.
const END_OF_BLOCK: u16 = 256;

fn compressed_block(
    bits: &mut BitReader,
    out: &mut Vec<u8>,
    lengths: &Huffman,
    distances: &Huffman,
) -> Result<(), InflateError> {
    loop {
        let symbol = lengths.decode(bits)?;
        if symbol == END_OF_BLOCK {
            return Ok(());
        }
        if symbol < END_OF_BLOCK {
            out.push(symbol as u8);
            continue;
        }

        let index = usize::from(symbol - 257);
        let (base, extra) = (
            *LENGTH_BASE.get(index).ok_or(InflateError::BadCode)?,
            LENGTH_EXTRA[index],
        );
        let length = usize::from(base) + bits.bits(extra)? as usize;

        let symbol = usize::from(distances.decode(bits)?);
        let (base, extra) = (
            *DISTANCE_BASE.get(symbol).ok_or(InflateError::BadCode)?,
            DISTANCE_EXTRA[symbol],
        );
        let distance = usize::from(base) + bits.bits(extra)? as usize;
        if distance > out.len() {
            return Err(InflateError::BadDistance);
        }

        // Byte by byte, because the copy may overlap what it writes —
        // see the module note. A block move would be wrong here.
        let start = out.len() - distance;
        for offset in 0..length {
            let byte = out[start + offset];
            out.push(byte);
        }
    }
}

/// The tables RFC 1951 defines for a block that carries none.
fn fixed_tables() -> (Huffman, Huffman) {
    let mut lengths = [0u8; 288];
    for (symbol, bits) in lengths.iter_mut().enumerate() {
        *bits = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let distances = [5u8; 30];
    (
        Huffman::new(&lengths).expect("the fixed table is well formed"),
        Huffman::new(&distances).expect("the fixed table is well formed"),
    )
}

/// The order code-length codes are written in, which is not their
/// numeric order: the most common lengths come first so a table that
/// uses few of them can stop early.
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Read the two tables a dynamic block carries, which are themselves
/// Huffman-coded.
fn dynamic_tables(bits: &mut BitReader) -> Result<(Huffman, Huffman), InflateError> {
    let literal_count = bits.bits(5)? as usize + 257;
    let distance_count = bits.bits(5)? as usize + 1;
    let code_count = bits.bits(4)? as usize + 4;

    let mut code_lengths = [0u8; 19];
    for &index in CODE_LENGTH_ORDER.iter().take(code_count) {
        code_lengths[index] = bits.bits(3)? as u8;
    }
    let code_table = Huffman::new(&code_lengths)?;

    // Both tables are read as one run, because a repeat may carry
    // across the boundary between them.
    let total = literal_count + distance_count;
    let mut lengths = Vec::with_capacity(total);
    while lengths.len() < total {
        match code_table.decode(bits)? {
            // 0..=15 is a literal length.
            length @ 0..=15 => lengths.push(length as u8),
            // 16 repeats the previous length 3..=6 times.
            16 => {
                let previous = *lengths.last().ok_or(InflateError::BadCode)?;
                let count = 3 + bits.bits(2)? as usize;
                lengths.resize(lengths.len() + count, previous);
            }
            // 17 and 18 are runs of zero, of different lengths.
            17 => {
                let count = 3 + bits.bits(3)? as usize;
                lengths.resize(lengths.len() + count, 0);
            }
            18 => {
                let count = 11 + bits.bits(7)? as usize;
                lengths.resize(lengths.len() + count, 0);
            }
            _ => return Err(InflateError::BadCode),
        }
    }
    if lengths.len() > total {
        return Err(InflateError::BadCode);
    }
    Ok((
        Huffman::new(&lengths[..literal_count])?,
        Huffman::new(&lengths[literal_count..])?,
    ))
}

/// A canonical Huffman table, as a map from (length, code) to symbol.
///
/// DEFLATE never transmits the codes themselves, only how many bits
/// each symbol's code has. The codes follow from that: sort by length,
/// then by symbol, and count upward. So the table is rebuilt from the
/// lengths rather than read.
struct Huffman {
    /// For each code length, the first code of that length and the
    /// index into `symbols` where that length's run begins.
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, InflateError> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            if length > 15 {
                return Err(InflateError::BadCode);
            }
            counts[usize::from(length)] += 1;
        }
        // Length zero means "this symbol is not used", not a code.
        counts[0] = 0;

        let mut offsets = [0u16; 16];
        for length in 1..16 {
            offsets[length] = offsets[length - 1] + counts[length - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length == 0 {
                continue;
            }
            let slot = &mut offsets[usize::from(length)];
            symbols[usize::from(*slot)] = symbol as u16;
            *slot += 1;
        }
        Ok(Self { counts, symbols })
    }

    /// Read one symbol, one bit at a time.
    ///
    /// Canonical coding makes this a walk rather than a search: at each
    /// length, the codes of that length form a contiguous range, so a
    /// running comparison says whether the code has ended.
    fn decode(&self, bits: &mut BitReader) -> Result<u16, InflateError> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for length in 1..16 {
            code |= bits.bits(1)? as i32;
            let count = i32::from(self.counts[length]);
            if code - first < count {
                let slot = (index + code - first) as usize;
                return self.symbols.get(slot).copied().ok_or(InflateError::BadCode);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(InflateError::BadCode)
    }
}

/// Bits, least-significant first within each byte — DEFLATE's order,
/// which is the opposite of how its Huffman codes read.
struct BitReader<'a> {
    bytes: &'a [u8],
    /// The next byte to take bits from.
    position: usize,
    /// Bits already taken from the byte at `position`.
    used: u8,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            used: 0,
        }
    }

    /// The next `count` bits as a number, first bit least significant.
    fn bits(&mut self, count: u8) -> Result<u32, InflateError> {
        let mut value = 0u32;
        for index in 0..count {
            let byte = *self
                .bytes
                .get(self.position)
                .ok_or(InflateError::Truncated)?;
            let bit = (byte >> self.used) & 1;
            value |= u32::from(bit) << index;
            self.used += 1;
            if self.used == 8 {
                self.used = 0;
                self.position += 1;
            }
        }
        Ok(value)
    }

    /// Discard the rest of the current byte.
    fn align(&mut self) {
        if self.used != 0 {
            self.used = 0;
            self.position += 1;
        }
    }

    /// One whole byte, which must be aligned.
    fn byte(&mut self) -> Result<u8, InflateError> {
        let byte = *self
            .bytes
            .get(self.position)
            .ok_or(InflateError::Truncated)?;
        self.position += 1;
        Ok(byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored block holding `data`, as one final block.
    fn stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x01];
        let length = data.len() as u16;
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn a_stored_block_comes_back_verbatim() {
        assert_eq!(inflate(&stored(b"hello")).unwrap(), b"hello");
    }

    #[test]
    fn an_empty_stored_block_yields_nothing() {
        assert_eq!(inflate(&stored(b"")).unwrap(), b"");
    }

    #[test]
    fn a_stored_length_that_fails_its_check_is_refused() {
        let mut bytes = stored(b"hello");
        bytes[3] ^= 0xff;
        assert_eq!(inflate(&bytes), Err(InflateError::BadStoredLength));
    }

    /// zlib of "hello hello hello" with the fixed Huffman table.
    const FIXED_HELLO: &[u8] = &[
        0x78, 0x01, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x90, 0x00, 0x3a, 0x2e, 0x06,
        0x7d,
    ];

    #[test]
    fn a_fixed_huffman_block_decompresses() {
        assert_eq!(zlib(FIXED_HELLO).unwrap(), b"hello hello hello");
    }

    #[test]
    fn a_back_reference_may_overlap_its_own_output() {
        // "hello hello hello" is written as a literal run and then a
        // copy of length 11 from distance 6, which reads bytes the
        // copy is still writing.
        let out = zlib(FIXED_HELLO).unwrap();
        assert_eq!(out.len(), 17);
        assert_eq!(&out[6..], b"hello hello");
    }

    #[test]
    fn a_zlib_header_that_is_not_deflate_is_refused() {
        assert_eq!(zlib(&[0x79, 0x9c, 0x00]), Err(InflateError::BadZlibHeader));
    }

    #[test]
    fn a_truncated_stream_is_refused_rather_than_guessed_at() {
        assert_eq!(zlib(&FIXED_HELLO[..6]), Err(InflateError::Truncated));
        assert_eq!(inflate(&[]), Err(InflateError::Truncated));
    }

    #[test]
    fn an_unknown_block_type_is_refused() {
        // Block type 3 is reserved and means a corrupt stream.
        assert_eq!(inflate(&[0b0000_0111]), Err(InflateError::BadBlockType));
    }

    /// zlib of 600 bytes cycling through 0..=59 — long and varied
    /// enough that the encoder builds a table of its own rather than
    /// using the fixed one.
    fn dynamic_sample() -> (Vec<u8>, Vec<u8>) {
        let compressed = vec![
            0x78, 0xda, 0x63, 0x60, 0x64, 0x62, 0x66, 0x61, 0x65, 0x63, 0xe7, 0xe0, 0xe4, 0xe2,
            0xe6, 0xe1, 0xe5, 0xe3, 0x17, 0x10, 0x14, 0x12, 0x16, 0x11, 0x15, 0x13, 0x97, 0x90,
            0x94, 0x92, 0x96, 0x91, 0x95, 0x93, 0x57, 0x50, 0x54, 0x52, 0x56, 0x51, 0x55, 0x53,
            0xd7, 0xd0, 0xd4, 0xd2, 0xd6, 0xd1, 0xd5, 0xd3, 0x37, 0x30, 0x34, 0x32, 0x36, 0x31,
            0x35, 0x33, 0xb7, 0xb0, 0xb4, 0xb2, 0x66, 0x18, 0xd5, 0x3b, 0xaa, 0x17, 0x87, 0x5e,
            0x00, 0x70, 0xbe, 0x45, 0x25,
        ];
        let expected: Vec<u8> = (0..600).map(|index| (index % 60) as u8).collect();
        (compressed, expected)
    }

    #[test]
    fn a_dynamic_huffman_block_decompresses() {
        let (compressed, expected) = dynamic_sample();
        assert_eq!(zlib(&compressed).unwrap(), expected);
    }

    #[test]
    fn a_huffman_table_with_no_used_symbols_decodes_nothing() {
        let table = Huffman::new(&[0, 0, 0]).unwrap();
        let mut bits = BitReader::new(&[0xff, 0xff, 0xff]);
        assert_eq!(table.decode(&mut bits), Err(InflateError::BadCode));
    }

    #[test]
    fn bits_are_read_least_significant_first() {
        let mut bits = BitReader::new(&[0b1011_0010]);
        assert_eq!(bits.bits(1).unwrap(), 0);
        assert_eq!(bits.bits(2).unwrap(), 0b01);
        assert_eq!(bits.bits(5).unwrap(), 0b1_0110);
    }

    #[test]
    fn aligning_drops_the_rest_of_a_byte() {
        let mut bits = BitReader::new(&[0xff, 0x42]);
        bits.bits(3).unwrap();
        bits.align();
        assert_eq!(bits.byte().unwrap(), 0x42);
    }
}
