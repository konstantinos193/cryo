//! Reading JPEG's entropy-coded bitstream.
//!
//! Two things live here because they are the same concern: the bit
//! reader, and the Huffman tables it is read against.
//!
//! JPEG reads bits **most significant first**, the opposite of
//! DEFLATE, so its tables cannot be shared with
//! [`super::super::inflate`]'s however similar they look.
//!
//! The reader also undoes *byte stuffing*: inside entropy-coded data a
//! literal `0xFF` is written `0xFF 0x00`, because `0xFF` otherwise
//! starts a marker. Miss that and every bright pixel derails the
//! stream.

use super::super::ImageError;

/// A canonical Huffman table: how many codes have each length, and the
/// symbols in code order.
#[derive(Clone, Default)]
pub(super) struct Huffman {
    /// `counts[n]` is how many codes have `n + 1` bits.
    counts: [u8; 16],
    symbols: Vec<u8>,
}

impl Huffman {
    pub(super) fn new(counts: [u8; 16], symbols: Vec<u8>) -> Self {
        Self { counts, symbols }
    }

    /// Read one symbol.
    ///
    /// Canonical coding makes this a walk rather than a search: the
    /// codes of each length form a contiguous range, so a running
    /// comparison says when the code has ended.
    pub(super) fn decode(&self, bits: &mut BitReader) -> Result<u8, ImageError> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0usize;
        for length in 0..16 {
            code = (code << 1) | i32::from(bits.bit()?);
            let count = i32::from(self.counts[length]);
            if code - first < count {
                let slot = index + (code - first) as usize;
                return self
                    .symbols
                    .get(slot)
                    .copied()
                    .ok_or(ImageError::Malformed("a huffman code with no symbol"));
            }
            index += count as usize;
            first = (first + count) << 1;
        }
        Err(ImageError::Malformed("a huffman code longer than 16 bits"))
    }
}

/// Bits, most significant first, with byte stuffing undone.
pub(super) struct BitReader<'a> {
    bytes: &'a [u8],
    position: usize,
    current: u8,
    remaining: u8,
}

impl<'a> BitReader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            current: 0,
            remaining: 0,
        }
    }

    pub(super) fn bit(&mut self) -> Result<u8, ImageError> {
        if self.remaining == 0 {
            self.current = self.next_byte()?;
            self.remaining = 8;
        }
        self.remaining -= 1;
        Ok((self.current >> self.remaining) & 1)
    }

    /// `count` bits as an unsigned number.
    pub(super) fn bits(&mut self, count: u8) -> Result<i32, ImageError> {
        let mut value = 0i32;
        for _ in 0..count {
            value = (value << 1) | i32::from(self.bit()?);
        }
        Ok(value)
    }

    /// The next data byte, stepping over stuffing.
    fn next_byte(&mut self) -> Result<u8, ImageError> {
        let byte = *self.bytes.get(self.position).ok_or(ImageError::Truncated)?;
        self.position += 1;
        if byte != 0xff {
            return Ok(byte);
        }
        match self.bytes.get(self.position) {
            Some(0x00) => {
                self.position += 1;
                Ok(0xff)
            }
            // A marker: the scan is over. Feeding zeros lets the last
            // partly-read unit finish rather than failing the image.
            _ => Ok(0),
        }
    }

    /// Skip to just past the next restart marker, dropping any
    /// part-read byte — restarts always resume on a byte boundary.
    pub(super) fn restart(&mut self) -> Result<(), ImageError> {
        self.remaining = 0;
        while self.position + 1 < self.bytes.len() {
            if self.bytes[self.position] == 0xff
                && (0xd0..=0xd7).contains(&self.bytes[self.position + 1])
            {
                self.position += 2;
                return Ok(());
            }
            self.position += 1;
        }
        Err(ImageError::Truncated)
    }

    /// Read `size` bits as a signed coefficient.
    ///
    /// JPEG stores a magnitude category and then that many bits. The
    /// top half of the range is positive; the bottom half is negative
    /// and shifts down, which is the "extend" step.
    pub(super) fn receive_extend(&mut self, size: u8) -> Result<i32, ImageError> {
        if size == 0 {
            return Ok(0);
        }
        if size > 16 {
            return Err(ImageError::Malformed("a coefficient wider than 16 bits"));
        }
        let value = self.bits(size)?;
        Ok(if value < 1 << (size - 1) {
            value - (1 << size) + 1
        } else {
            value
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuffed_bytes_read_back_as_one() {
        let mut bits = BitReader::new(&[0xff, 0x00, 0x42]);
        assert_eq!(bits.next_byte().unwrap(), 0xff);
        assert_eq!(bits.next_byte().unwrap(), 0x42);
    }

    #[test]
    fn a_marker_in_the_data_ends_the_scan_without_failing() {
        let mut bits = BitReader::new(&[0xff, 0xd9]);
        assert_eq!(bits.next_byte().unwrap(), 0);
    }

    #[test]
    fn bits_are_read_most_significant_first() {
        let mut bits = BitReader::new(&[0b1010_0000]);
        assert_eq!(bits.bit().unwrap(), 1);
        assert_eq!(bits.bit().unwrap(), 0);
        assert_eq!(bits.bit().unwrap(), 1);
        assert_eq!(bits.bit().unwrap(), 0);
    }

    #[test]
    fn extend_turns_the_low_half_of_a_category_negative() {
        // Category 3 covers -7..=-4 and 4..=7.
        assert_eq!(BitReader::new(&[0b1110_0000]).receive_extend(3).unwrap(), 7);
        assert_eq!(
            BitReader::new(&[0b0000_0000]).receive_extend(3).unwrap(),
            -7
        );
        assert_eq!(BitReader::new(&[0b1000_0000]).receive_extend(3).unwrap(), 4);
        assert_eq!(
            BitReader::new(&[0b0110_0000]).receive_extend(3).unwrap(),
            -4
        );
    }

    #[test]
    fn a_zero_category_is_zero_and_reads_no_bits() {
        assert_eq!(BitReader::new(&[]).receive_extend(0).unwrap(), 0);
    }

    #[test]
    fn a_table_decodes_the_shortest_code_first() {
        // One 1-bit code for symbol 7, one 2-bit code for symbol 9.
        let mut counts = [0u8; 16];
        counts[0] = 1;
        counts[1] = 1;
        let table = Huffman::new(counts, vec![7, 9]);
        // `0` is the 1-bit code, `10` the 2-bit one.
        assert_eq!(
            table.decode(&mut BitReader::new(&[0b0000_0000])).unwrap(),
            7
        );
        assert_eq!(
            table.decode(&mut BitReader::new(&[0b1000_0000])).unwrap(),
            9
        );
    }

    #[test]
    fn a_code_with_no_symbol_is_refused() {
        let table = Huffman::default();
        assert!(table.decode(&mut BitReader::new(&[0xff, 0xff])).is_err());
    }
}
