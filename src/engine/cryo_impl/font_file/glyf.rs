//! Putting a WOFF2 `glyf` table back together.
//!
//! An sfnt stores a glyph as one run of bytes: contour ends, then
//! instructions, then flags, then x coordinates, then y coordinates.
//! WOFF2 stores the *same information* as seven parallel streams, one
//! per kind of value, because like values compress together and
//! unlike ones do not:
//!
//! ```text
//!   contour counts │ 2 2 1 3 2 …
//!   point counts   │ 12 9 4 …
//!   flags          │ one byte a point
//!   glyph stream   │ interleaved lengths and coordinate bytes
//!   composite      │ component records, copied through
//!   bboxes         │ only for glyphs whose box is not implied
//!   instructions   │ hinting bytecode
//! ```
//!
//! Two parts of the reversal are worth knowing about.
//!
//! **Coordinates are triplet-encoded.** Each point's flag byte picks
//! one of 128 schemes saying how many bytes the delta takes, how its
//! bits split between x and y, and what each sign is. That is where
//! most of the saving comes from — a short, mostly-horizontal step is
//! one byte — and it is why the flag byte is not simply the sfnt flag
//! byte.
//!
//! **A bounding box is usually implied.** A simple glyph's box is the
//! extent of its points, so it is not stored unless it differs. A
//! bitmap at the front of the bbox stream says which glyphs stored
//! one; composites always do, since their extent cannot be known
//! without resolving the components.

use super::FontFileError;

/// What a reconstruction produces: the table and where each glyph
/// starts in it.
pub(super) struct Rebuilt {
    pub(super) glyf: Vec<u8>,
    offsets: Vec<u32>,
}

impl Rebuilt {
    /// The matching `loca` table, always in the long form.
    ///
    /// `loca` comes in two forms and `head` says which, so the two
    /// have to agree. The short form halves each offset, which only
    /// works while every glyph starts on an even byte and the table
    /// stays under 128 KB — and the table rebuilt here is *larger*
    /// than the one the font shipped, because the coordinates are
    /// written out plainly. Choosing the long form and saying so in
    /// `head` removes the question.
    pub(super) fn loca(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.offsets.len() * 4);
        for offset in &self.offsets {
            out.extend_from_slice(&offset.to_be_bytes());
        }
        out
    }
}

/// A cursor over one of the streams.
struct Stream<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Stream<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn byte(&mut self) -> Result<u8, FontFileError> {
        let byte = *self.data.get(self.at).ok_or(FontFileError::Truncated)?;
        self.at += 1;
        Ok(byte)
    }

    fn be16(&mut self) -> Result<u16, FontFileError> {
        Ok(u16::from(self.byte()?) << 8 | u16::from(self.byte()?))
    }

    fn be32(&mut self) -> Result<u32, FontFileError> {
        Ok(u32::from(self.be16()?) << 16 | u32::from(self.be16()?))
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], FontFileError> {
        let slice = self
            .data
            .get(self.at..self.at + count)
            .ok_or(FontFileError::Truncated)?;
        self.at += count;
        Ok(slice)
    }

    /// A count written in one byte where it fits and two or three
    /// where it does not — the format's own small-integer encoding.
    fn count255(&mut self) -> Result<u16, FontFileError> {
        const WORD: u8 = 253;
        const ONE_MORE: u8 = 255;
        const TWO_MORE: u8 = 254;
        match self.byte()? {
            WORD => self.be16(),
            ONE_MORE => Ok(u16::from(self.byte()?) + u16::from(WORD)),
            TWO_MORE => Ok(u16::from(self.byte()?) + u16::from(WORD) * 2),
            value => Ok(u16::from(value)),
        }
    }
}

/// Rebuild `glyf` from its transformed form.
pub(super) fn reconstruct(data: &[u8]) -> Result<Rebuilt, FontFileError> {
    let mut head = Stream::new(data);
    // The version is a 32-bit fixed-point number, not a count: read
    // it as two bytes and every stream size after it lands two bytes
    // early, which produces sizes in the hundreds of megabytes.
    let _version = head.be32()?;
    let glyph_count = usize::from(head.be16()?);
    let _index_format = head.be16()?;
    let sizes: Vec<usize> = (0..7)
        .map(|_| Ok(head.be32()? as usize))
        .collect::<Result<_, FontFileError>>()?;

    let mut at = head.at;
    let mut slice = |length: usize| -> Result<&[u8], FontFileError> {
        let bytes = data.get(at..at + length).ok_or(FontFileError::Truncated)?;
        at += length;
        Ok(bytes)
    };
    let mut contours = Stream::new(slice(sizes[0])?);
    let mut points = Stream::new(slice(sizes[1])?);
    let mut flags = Stream::new(slice(sizes[2])?);
    let mut glyphs = Stream::new(slice(sizes[3])?);
    let mut composites = Stream::new(slice(sizes[4])?);
    let mut boxes = Stream::new(slice(sizes[5])?);
    let mut instructions = Stream::new(slice(sizes[6])?);

    // The first bytes of the bbox stream are a bit per glyph saying
    // whether that glyph stored one.
    let bitmap_bytes = glyph_count.div_ceil(8);
    let bitmap = boxes.take(bitmap_bytes)?.to_vec();
    let has_box = |glyph: usize| {
        bitmap
            .get(glyph / 8)
            .is_some_and(|byte| byte & (0x80 >> (glyph % 8)) != 0)
    };

    let mut out = Vec::new();
    let mut offsets = Vec::with_capacity(glyph_count + 1);
    offsets.push(0u32);
    for glyph in 0..glyph_count {
        let count = contours.be16()? as i16;
        if count == 0 {
            // An empty glyph — a space — occupies no bytes at all,
            // which is how `loca` says "nothing here".
            offsets.push(out.len() as u32);
            continue;
        }
        if count < 0 {
            composite(
                &mut out,
                &mut composites,
                &mut glyphs,
                &mut instructions,
                &mut boxes,
            )?;
        } else {
            simple(
                &mut out,
                usize::from(count as u16),
                &mut points,
                &mut flags,
                &mut glyphs,
                &mut instructions,
                has_box(glyph).then_some(&mut boxes),
            )?;
        }
        while out.len() % 2 != 0 {
            out.push(0);
        }
        offsets.push(out.len() as u32);
    }
    Ok(Rebuilt { glyf: out, offsets })
}

/// A composite glyph: its component records are stored as written,
/// so they only have to be copied back out.
fn composite(
    out: &mut Vec<u8>,
    composites: &mut Stream<'_>,
    glyphs: &mut Stream<'_>,
    instructions: &mut Stream<'_>,
    boxes: &mut Stream<'_>,
) -> Result<(), FontFileError> {
    const MORE_COMPONENTS: u16 = 0x0020;
    const WE_HAVE_INSTRUCTIONS: u16 = 0x0100;
    const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
    const WE_HAVE_A_SCALE: u16 = 0x0008;
    const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
    const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

    out.extend_from_slice(&(-1i16).to_be_bytes());
    // A composite's box is never implied, so it is always stored.
    out.extend_from_slice(boxes.take(8)?);

    let mut wants_instructions;
    loop {
        let flags = composites.be16()?;
        let index = composites.be16()?;
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&index.to_be_bytes());
        let arguments = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        let transform = if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
            8
        } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            4
        } else if flags & WE_HAVE_A_SCALE != 0 {
            2
        } else {
            0
        };
        out.extend_from_slice(composites.take(arguments + transform)?);
        wants_instructions = flags & WE_HAVE_INSTRUCTIONS != 0;
        if flags & MORE_COMPONENTS == 0 {
            break;
        }
    }
    if wants_instructions {
        let length = usize::from(glyphs.count255()?);
        out.extend_from_slice(&(length as u16).to_be_bytes());
        out.extend_from_slice(instructions.take(length)?);
    }
    Ok(())
}

/// A simple glyph: contour ends, instructions, then the points.
fn simple(
    out: &mut Vec<u8>,
    contour_count: usize,
    points: &mut Stream<'_>,
    flags: &mut Stream<'_>,
    glyphs: &mut Stream<'_>,
    instructions: &mut Stream<'_>,
    stored_box: Option<&mut Stream<'_>>,
) -> Result<(), FontFileError> {
    let mut ends = Vec::with_capacity(contour_count);
    let mut total = 0usize;
    for _ in 0..contour_count {
        total += usize::from(points.count255()?);
        ends.push((total - 1) as u16);
    }

    let mut xs = Vec::with_capacity(total);
    let mut ys = Vec::with_capacity(total);
    let mut on_curve = Vec::with_capacity(total);
    let (mut x, mut y) = (0i32, 0i32);
    for _ in 0..total {
        let flag = flags.byte()?;
        on_curve.push(flag & 0x80 == 0);
        let (dx, dy) = triplet(flag & 0x7f, glyphs)?;
        x += dx;
        y += dy;
        xs.push(x);
        ys.push(y);
    }

    let instruction_length = usize::from(glyphs.count255()?);
    let code = instructions.take(instruction_length)?;

    out.extend_from_slice(&(contour_count as u16).to_be_bytes());
    match stored_box {
        Some(boxes) => out.extend_from_slice(boxes.take(8)?),
        None => {
            // Implied: the extent of the points, and zero all round
            // for a glyph with none.
            let bounds = |values: &[i32]| {
                values
                    .iter()
                    .fold((i32::MAX, i32::MIN), |(low, high), value| {
                        (low.min(*value), high.max(*value))
                    })
            };
            let (min_x, max_x) = bounds(&xs);
            let (min_y, max_y) = bounds(&ys);
            let clamp = |value: i32| value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            for value in [min_x, min_y, max_x, max_y] {
                let value = if xs.is_empty() { 0 } else { value };
                out.extend_from_slice(&clamp(value).to_be_bytes());
            }
        }
    }
    for end in &ends {
        out.extend_from_slice(&end.to_be_bytes());
    }
    out.extend_from_slice(&(instruction_length as u16).to_be_bytes());
    out.extend_from_slice(code);

    write_points(out, &xs, &ys, &on_curve);
    Ok(())
}

/// Write the flags and coordinates in the sfnt's own form.
///
/// No repeat flags and no short coordinates are emitted: both are
/// optional compressions of a form the reader accepts either way, and
/// the file is about to be thrown away after parsing. Spending bytes
/// here buys simplicity where it cannot be seen.
fn write_points(out: &mut Vec<u8>, xs: &[i32], ys: &[i32], on_curve: &[bool]) {
    const ON_CURVE: u8 = 0x01;
    for on in on_curve {
        out.push(if *on { ON_CURVE } else { 0 });
    }
    let mut previous = 0i32;
    for x in xs {
        out.extend_from_slice(&((x - previous) as i16).to_be_bytes());
        previous = *x;
    }
    let mut previous = 0i32;
    for y in ys {
        out.extend_from_slice(&((y - previous) as i16).to_be_bytes());
        previous = *y;
    }
}

/// One point's delta, under the scheme its flag names.
///
/// The 128 schemes are six bands: a byte for one axis with the other
/// zero, a byte split between both, two bytes, three, or four. Within
/// a band the flag's own bits carry the high part of the value and
/// both signs, which is what lets the common case fit in a single
/// byte for the flag and one for the data.
fn triplet(flag: u8, glyphs: &mut Stream<'_>) -> Result<(i32, i32), FontFileError> {
    let signed = |flag: u8, value: i32| if flag & 1 != 0 { value } else { -value };
    let flag = i32::from(flag);
    let (dx, dy) = if flag < 10 {
        let b0 = i32::from(glyphs.byte()?);
        (0, signed(flag as u8, ((flag & 14) << 7) + b0))
    } else if flag < 20 {
        let b0 = i32::from(glyphs.byte()?);
        (signed(flag as u8, (((flag - 10) & 14) << 7) + b0), 0)
    } else if flag < 84 {
        let base = flag - 20;
        let b0 = i32::from(glyphs.byte()?);
        (
            signed(flag as u8, 1 + (base & 0x30) + (b0 >> 4)),
            signed((flag >> 1) as u8, 1 + ((base & 0x0c) << 2) + (b0 & 0x0f)),
        )
    } else if flag < 120 {
        let base = flag - 84;
        let b0 = i32::from(glyphs.byte()?);
        let b1 = i32::from(glyphs.byte()?);
        (
            signed(flag as u8, 1 + ((base / 12) << 8) + b0),
            signed((flag >> 1) as u8, 1 + (((base % 12) >> 2) << 8) + b1),
        )
    } else if flag < 124 {
        let b0 = i32::from(glyphs.byte()?);
        let b1 = i32::from(glyphs.byte()?);
        let b2 = i32::from(glyphs.byte()?);
        (
            signed(flag as u8, (b0 << 4) + (b1 >> 4)),
            signed((flag >> 1) as u8, ((b1 & 0x0f) << 8) + b2),
        )
    } else {
        let b0 = i32::from(glyphs.byte()?);
        let b1 = i32::from(glyphs.byte()?);
        let b2 = i32::from(glyphs.byte()?);
        let b3 = i32::from(glyphs.byte()?);
        (
            signed(flag as u8, (b0 << 8) + b1),
            signed((flag >> 1) as u8, (b2 << 8) + b3),
        )
    };
    Ok((dx, dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_counts_take_one_byte() {
        let mut stream = Stream::new(&[0, 7, 252]);
        assert_eq!(stream.count255(), Ok(0));
        assert_eq!(stream.count255(), Ok(7));
        assert_eq!(stream.count255(), Ok(252));
    }

    #[test]
    fn larger_counts_use_their_escapes() {
        // 255 means "add 253"; 254 means "add twice that"; 253 means
        // "a whole sixteen-bit value follows".
        let mut stream = Stream::new(&[255, 10, 254, 10, 253, 0x12, 0x34]);
        assert_eq!(stream.count255(), Ok(263));
        assert_eq!(stream.count255(), Ok(516));
        assert_eq!(stream.count255(), Ok(0x1234));
    }

    #[test]
    fn a_one_byte_triplet_moves_along_one_axis() {
        // Flags under ten move in y alone, and the low bit is the
        // sign: flag 1 is positive, flag 0 negative.
        let mut stream = Stream::new(&[5]);
        assert_eq!(triplet(1, &mut stream), Ok((0, 5)));
        let mut stream = Stream::new(&[5]);
        assert_eq!(triplet(0, &mut stream), Ok((0, -5)));
        // Ten to nineteen move in x alone.
        let mut stream = Stream::new(&[5]);
        assert_eq!(triplet(11, &mut stream), Ok((5, 0)));
    }

    #[test]
    fn a_split_triplet_takes_four_bits_each_way() {
        // Flag 21: band 20..84, base 1, so both signs come from the
        // flag and the byte splits in half.
        let mut stream = Stream::new(&[0x23]);
        let (dx, dy) = triplet(21, &mut stream).expect("a triplet");
        assert_eq!((dx, dy), (1 + 2, -(1 + 3)));
    }

    #[test]
    fn the_widest_triplet_takes_four_bytes() {
        let mut stream = Stream::new(&[0x01, 0x02, 0x03, 0x04]);
        let (dx, dy) = triplet(125, &mut stream).expect("a triplet");
        assert_eq!((dx, dy), (0x0102, -0x0304));
    }

    #[test]
    fn a_truncated_stream_is_an_error_not_a_panic() {
        let mut stream = Stream::new(&[]);
        assert_eq!(stream.byte().err(), Some(FontFileError::Truncated));
        assert_eq!(
            triplet(125, &mut stream).err(),
            Some(FontFileError::Truncated)
        );
    }

    #[test]
    fn loca_is_four_bytes_an_offset_and_one_longer_than_the_glyphs() {
        let rebuilt = Rebuilt {
            glyf: Vec::new(),
            offsets: vec![0, 4, 8],
        };
        let loca = rebuilt.loca();
        assert_eq!(loca.len(), 12, "three offsets, long form");
        assert_eq!(
            &loca[8..12],
            &8u32.to_be_bytes(),
            "the last one ends the last glyph"
        );
    }
}
