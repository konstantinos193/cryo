//! Reading a Brotli stream bit by bit, least significant bit first.
//!
//! DEFLATE reads bits the same way, but the two differ in a detail
//! that matters here: Brotli reads *multi-bit* fields in the same
//! order, so a 4-bit field spans the low four bits of the current
//! byte, and a field crossing a byte boundary takes its low bits from
//! the first byte. Reading a field MSB-first instead produces values
//! that are plausible, small and wrong, which is the worst kind.
//!
//! The reader never panics on a short stream: running out of input is
//! a [`BrotliError::Truncated`], because a font that arrives cut off
//! must leave the page readable rather than take the process down.

use super::BrotliError;

/// A cursor over compressed bytes, positioned at a bit.
pub(super) struct Bits<'a> {
    data: &'a [u8],
    /// How many bits have been consumed from the start.
    position: usize,
}

impl<'a> Bits<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    /// Read `count` bits as a number, low bit first. `count` is at
    /// most 24, which is what every field in the format needs.
    pub(super) fn read(&mut self, count: u32) -> Result<u32, BrotliError> {
        debug_assert!(count <= 24, "a Brotli field is never wider than 24 bits");
        let mut value = 0u32;
        for index in 0..count {
            value |= self.read_one()? << index;
        }
        Ok(value)
    }

    /// Read one bit without consuming it, for the fixed prefix codes
    /// that decide how many more bits to take.
    pub(super) fn read_one(&mut self) -> Result<u32, BrotliError> {
        let byte = self
            .data
            .get(self.position / 8)
            .ok_or(BrotliError::Truncated)?;
        let bit = (byte >> (self.position % 8)) & 1;
        self.position += 1;
        Ok(u32::from(bit))
    }

    /// Discard bits up to the next byte boundary.
    ///
    /// Uncompressed metablocks and metadata are byte aligned, and the
    /// skipped bits are required to be zero — but a decoder that
    /// enforced that would reject streams other decoders accept, so
    /// they are dropped rather than checked.
    pub(super) fn align_to_byte(&mut self) {
        self.position = self.position.div_ceil(8) * 8;
    }

    /// Take `count` whole bytes from a byte-aligned position.
    pub(super) fn take_bytes(&mut self, count: usize) -> Result<&'a [u8], BrotliError> {
        debug_assert_eq!(self.position % 8, 0, "take_bytes needs byte alignment");
        let start = self.position / 8;
        let end = start.checked_add(count).ok_or(BrotliError::Truncated)?;
        let slice = self.data.get(start..end).ok_or(BrotliError::Truncated)?;
        self.position = end * 8;
        Ok(slice)
    }

    /// Whether every bit has been consumed.
    pub(super) fn at_end(&self) -> bool {
        self.position >= self.data.len() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_come_out_least_significant_first() {
        // 0b1011_0010 read one bit at a time is 0,1,0,0,1,1,0,1.
        let mut bits = Bits::new(&[0b1011_0010]);
        let read: Vec<u32> = (0..8).map(|_| bits.read_one().expect("in range")).collect();
        assert_eq!(read, [0, 1, 0, 0, 1, 1, 0, 1]);
    }

    #[test]
    fn a_multi_bit_field_takes_the_low_bits_first() {
        // The low four bits of 0b1011_0010 are 0b0010 = 2.
        let mut bits = Bits::new(&[0b1011_0010]);
        assert_eq!(bits.read(4).expect("in range"), 0b0010);
        assert_eq!(bits.read(4).expect("in range"), 0b1011);
    }

    #[test]
    fn a_field_crossing_a_byte_takes_its_low_bits_from_the_first() {
        // Six bits read from 0xFF then 0x00: the first six are all
        // ones. Then ten bits: two ones from the first byte in the
        // low places, eight zeros above them.
        let mut bits = Bits::new(&[0xFF, 0x00]);
        assert_eq!(bits.read(6).expect("in range"), 0b11_1111);
        assert_eq!(bits.read(10).expect("in range"), 0b00_0000_0011);
    }

    #[test]
    fn running_out_of_input_is_an_error_not_a_panic() {
        let mut bits = Bits::new(&[0x01]);
        assert!(bits.read(8).is_ok());
        assert_eq!(bits.read(1), Err(BrotliError::Truncated));
    }

    #[test]
    fn aligning_moves_to_the_next_byte_only_when_inside_one() {
        let mut bits = Bits::new(&[0xAA, 0xBB, 0xCC]);
        bits.align_to_byte();
        assert_eq!(bits.take_bytes(1).expect("a byte"), &[0xAA]);
        bits.read(3).expect("three bits of 0xBB");
        bits.align_to_byte();
        assert_eq!(bits.take_bytes(1).expect("a byte"), &[0xCC]);
    }

    #[test]
    fn taking_more_bytes_than_remain_is_an_error() {
        let mut bits = Bits::new(&[0x01, 0x02]);
        assert_eq!(bits.take_bytes(3), Err(BrotliError::Truncated));
    }
}
