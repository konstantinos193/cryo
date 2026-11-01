//! Decoding a scan's entropy-coded data into coefficients.
//!
//! A **baseline** JPEG has one scan carrying every coefficient of
//! every block. A **progressive** one has several, each carrying a
//! band of coefficients at a certain precision, so the picture can be
//! shown roughly before it has all arrived. Both end at the same
//! place — a full set of coefficients per block — so both are decoded
//! into the same store and transformed only once, at the end.
//!
//! That is the design decision worth stating: coefficients are *not*
//! transformed as they are read. A progressive scan refines values an
//! earlier scan already wrote, so a block is not finished until the
//! last scan is.
//!
//! Progressive scans come in four kinds, and they are genuinely
//! different algorithms rather than flags on one:
//!
//! | band | pass  | what it does                                  |
//! |------|-------|-----------------------------------------------|
//! | DC   | first | the DC difference, shifted up by `low`        |
//! | DC   | later | one correction bit per block                  |
//! | AC   | first | run-length coefficients, plus *end-of-band* runs |
//! | AC   | later | correction bits threaded through the runs     |
//!
//! The last of those is the subtle one: a refining AC scan reads
//! correction bits for coefficients that are already non-zero *while*
//! it walks the run-lengths for ones that are becoming non-zero, so
//! the two interleave bit by bit.

use super::super::ImageError;
use super::bits::{BitReader, Huffman};
use super::idct::ZIGZAG;
use super::{Component, Frame};

/// Which coefficients a scan carries, and at what precision.
#[derive(Clone, Copy, Debug)]
pub(super) struct Band {
    /// First and last coefficient, in zigzag order.
    pub(super) start: usize,
    pub(super) end: usize,
    /// The bit position an earlier scan reached, or zero for a first
    /// pass over this band.
    pub(super) high: u8,
    /// The bit position this scan writes at.
    pub(super) low: u8,
}

impl Band {
    fn is_dc(&self) -> bool {
        self.start == 0
    }

    fn is_refinement(&self) -> bool {
        self.high != 0
    }
}

/// The coefficients of every block, per component.
///
/// Kept across scans, because a progressive file writes the same
/// block several times.
pub(super) struct Coefficients {
    /// Per component: `blocks_across * blocks_down` blocks of 64.
    pub(super) blocks: Vec<Vec<i32>>,
    pub(super) across: Vec<usize>,
    pub(super) down: Vec<usize>,
}

impl Coefficients {
    pub(super) fn new(frame: &Frame) -> Self {
        let mut blocks = Vec::new();
        let mut across = Vec::new();
        let mut down = Vec::new();
        for component in &frame.components {
            // The padded grid: whole MCUs, since that is what an
            // interleaved scan writes.
            let wide = frame.mcus_across * component.horizontal;
            let high = frame.mcus_down * component.vertical;
            blocks.push(vec![0i32; wide * high * 64]);
            across.push(wide);
            down.push(high);
        }
        Self {
            blocks,
            across,
            down,
        }
    }

    fn block_mut(&mut self, component: usize, x: usize, y: usize) -> &mut [i32] {
        let offset = (y * self.across[component] + x) * 64;
        &mut self.blocks[component][offset..offset + 64]
    }
}

/// Decode one scan into `coefficients`.
///
/// `scan` names the components this scan covers, in order. A scan over
/// one component walks that component's own blocks; a scan over
/// several walks whole MCUs, taking each component's blocks in turn.
#[allow(clippy::too_many_arguments)] // a scan is defined by all of it
pub(super) fn decode(
    entropy: &[u8],
    frame: &Frame,
    scan: &[usize],
    band: Band,
    dc_tables: &[Huffman; 4],
    ac_tables: &[Huffman; 4],
    restart_interval: usize,
    coefficients: &mut Coefficients,
) -> Result<(), ImageError> {
    let mut bits = BitReader::new(entropy);
    let mut predictors = vec![0i32; frame.components.len()];
    let mut end_of_band = 0usize;
    let mut until_restart = restart_interval;

    // A scan over one component is *not* padded to whole MCUs: it
    // covers only the blocks that component really has. Getting this
    // wrong shears every progressive image.
    let units = if scan.len() == 1 {
        let index = scan[0];
        blocks_of(frame, index).0 * blocks_of(frame, index).1
    } else {
        frame.mcus_across * frame.mcus_down
    };

    for unit in 0..units {
        if restart_interval > 0 && until_restart == 0 {
            bits.restart()?;
            predictors.iter_mut().for_each(|p| *p = 0);
            end_of_band = 0;
            until_restart = restart_interval;
        }
        until_restart = until_restart.saturating_sub(1);

        if scan.len() == 1 {
            let index = scan[0];
            let (across, _) = blocks_of(frame, index);
            let (x, y) = (unit % across, unit / across);
            decode_one(
                &mut bits,
                frame,
                index,
                x,
                y,
                band,
                dc_tables,
                ac_tables,
                &mut predictors[index],
                &mut end_of_band,
                coefficients,
            )?;
            continue;
        }

        let (mcu_x, mcu_y) = (unit % frame.mcus_across, unit / frame.mcus_across);
        for &index in scan {
            let component = &frame.components[index];
            for block_y in 0..component.vertical {
                for block_x in 0..component.horizontal {
                    decode_one(
                        &mut bits,
                        frame,
                        index,
                        mcu_x * component.horizontal + block_x,
                        mcu_y * component.vertical + block_y,
                        band,
                        dc_tables,
                        ac_tables,
                        &mut predictors[index],
                        &mut end_of_band,
                        coefficients,
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// How many blocks a component really has, unpadded.
fn blocks_of(frame: &Frame, index: usize) -> (usize, usize) {
    let component = &frame.components[index];
    // Rounded up: a component at half sampling across an odd width
    // still has the extra column of samples.
    let samples_across = (frame.width * component.horizontal).div_ceil(frame.max_horizontal);
    let samples_down = (frame.height * component.vertical).div_ceil(frame.max_vertical);
    (
        samples_across.div_ceil(8).max(1),
        samples_down.div_ceil(8).max(1),
    )
}

/// One block, by whichever of the four algorithms this scan is.
#[allow(clippy::too_many_arguments)]
fn decode_one(
    bits: &mut BitReader,
    frame: &Frame,
    index: usize,
    x: usize,
    y: usize,
    band: Band,
    dc_tables: &[Huffman; 4],
    ac_tables: &[Huffman; 4],
    predictor: &mut i32,
    end_of_band: &mut usize,
    coefficients: &mut Coefficients,
) -> Result<(), ImageError> {
    let component = &frame.components[index];
    let (dc, ac) = (
        &dc_tables[component.dc_table],
        &ac_tables[component.ac_table],
    );

    if band.is_dc() {
        if band.is_refinement() {
            // One correction bit, which only ever turns a bit on.
            if bits.bit()? == 1 {
                coefficients.block_mut(index, x, y)[0] |= 1 << band.low;
            }
            return Ok(());
        }
        let category = dc.decode(bits)?;
        *predictor += bits.receive_extend(category)?;
        coefficients.block_mut(index, x, y)[0] = *predictor << band.low;
        // A baseline scan carries the AC coefficients in the same
        // block; a progressive DC scan stops here.
        if band.end == 0 {
            return Ok(());
        }
        return decode_ac_first(bits, ac, band, 1, end_of_band, coefficients, index, x, y);
    }

    if band.is_refinement() {
        decode_ac_refine(bits, ac, band, end_of_band, coefficients, index, x, y)
    } else {
        decode_ac_first(
            bits,
            ac,
            band,
            band.start,
            end_of_band,
            coefficients,
            index,
            x,
            y,
        )
    }
}

/// An AC band read for the first time: run-lengths and end-of-band
/// runs.
#[allow(clippy::too_many_arguments)]
fn decode_ac_first(
    bits: &mut BitReader,
    ac: &Huffman,
    band: Band,
    from: usize,
    end_of_band: &mut usize,
    coefficients: &mut Coefficients,
    index: usize,
    x: usize,
    y: usize,
) -> Result<(), ImageError> {
    // An end-of-band run says this block and the next few have nothing
    // more in this band.
    if *end_of_band > 0 {
        *end_of_band -= 1;
        return Ok(());
    }

    let mut slot = from;
    while slot <= band.end {
        let symbol = ac.decode(bits)?;
        let run = usize::from(symbol >> 4);
        let size = symbol & 0x0f;

        if size == 0 {
            if run == 15 {
                // Sixteen zeros, not an end-of-band.
                slot += 16;
                continue;
            }
            // `run` is the number of extra bits giving how many
            // blocks the band is empty for, this one included.
            *end_of_band = (1usize << run) - 1;
            if run > 0 {
                *end_of_band += bits.bits(run as u8)? as usize;
            }
            return Ok(());
        }

        slot += run;
        if slot > band.end {
            return Err(ImageError::Malformed("a coefficient run past the band"));
        }
        let value = bits.receive_extend(size)?;
        coefficients.block_mut(index, x, y)[ZIGZAG[slot]] = value << band.low;
        slot += 1;
    }
    Ok(())
}

/// An AC band read again at finer precision.
///
/// The hard one: correction bits for coefficients that are already
/// non-zero are threaded through the run-length walk for ones that are
/// only now becoming non-zero, so the two advance together.
#[allow(clippy::too_many_arguments)]
fn decode_ac_refine(
    bits: &mut BitReader,
    ac: &Huffman,
    band: Band,
    end_of_band: &mut usize,
    coefficients: &mut Coefficients,
    index: usize,
    x: usize,
    y: usize,
) -> Result<(), ImageError> {
    let positive = 1i32 << band.low;
    let mut slot = band.start;

    // Inside an end-of-band run no coefficient becomes non-zero, but
    // every one that already is still takes its correction bit.
    if *end_of_band > 0 {
        *end_of_band -= 1;
        while slot <= band.end {
            let at = ZIGZAG[slot];
            if coefficients.block_mut(index, x, y)[at] != 0 {
                refine(bits, coefficients, index, x, y, at, positive)?;
            }
            slot += 1;
        }
        return Ok(());
    }

    while slot <= band.end {
        let symbol = ac.decode(bits)?;
        // A run of 64 can never be spent inside one block, which is
        // how an end-of-band turns the rest of this block into
        // correction bits only — the run it starts counts the blocks
        // *after* this one.
        let mut run = i32::from(symbol >> 4);
        let size = symbol & 0x0f;
        let mut value = 0i32;

        if size == 0 {
            if run < 15 {
                *end_of_band = (1usize << run) - 1;
                if run > 0 {
                    *end_of_band += bits.bits(run as u8)? as usize;
                }
                run = 64;
            }
            // A run of 15 with no value is sixteen zero coefficients
            // to step over — and only ones still zero count.
        } else {
            // A coefficient becoming non-zero. Its sign is the one
            // bit the size allows.
            value = if bits.bit()? == 1 {
                positive
            } else {
                -positive
            };
        }

        while slot <= band.end {
            let at = ZIGZAG[slot];
            slot += 1;
            if coefficients.block_mut(index, x, y)[at] != 0 {
                // Already non-zero: a correction bit, and it does not
                // count against the run.
                refine(bits, coefficients, index, x, y, at, positive)?;
                continue;
            }
            if run == 0 {
                if value != 0 {
                    coefficients.block_mut(index, x, y)[at] = value;
                }
                break;
            }
            run -= 1;
        }
    }
    Ok(())
}

/// One correction bit for a coefficient that is already non-zero.
#[allow(clippy::too_many_arguments)]
fn refine(
    bits: &mut BitReader,
    coefficients: &mut Coefficients,
    index: usize,
    x: usize,
    y: usize,
    at: usize,
    positive: i32,
) -> Result<(), ImageError> {
    if bits.bit()? == 0 {
        return Ok(());
    }
    let block = coefficients.block_mut(index, x, y);
    // The bit extends the magnitude, so it moves away from zero —
    // which direction that is depends on the sign already there.
    if block[at] & positive == 0 {
        block[at] += if block[at] >= 0 { positive } else { -positive };
    }
    Ok(())
}

/// A component's scan-table assignments, set by each `SOS`.
pub(super) fn assign_tables(component: &mut Component, dc_table: usize, ac_table: usize) {
    component.dc_table = dc_table;
    component.ac_table = ac_table;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame {
            width: 16,
            height: 16,
            max_horizontal: 2,
            max_vertical: 2,
            mcus_across: 1,
            mcus_down: 1,
            components: vec![
                Component {
                    id: 1,
                    horizontal: 2,
                    vertical: 2,
                    quantisation: 0,
                    dc_table: 0,
                    ac_table: 0,
                },
                Component {
                    id: 2,
                    horizontal: 1,
                    vertical: 1,
                    quantisation: 1,
                    dc_table: 1,
                    ac_table: 1,
                },
            ],
        }
    }

    #[test]
    fn a_subsampled_component_has_fewer_blocks() {
        let frame = frame();
        // 16 pixels across at full sampling is two blocks; at half
        // sampling it is one.
        assert_eq!(blocks_of(&frame, 0), (2, 2));
        assert_eq!(blocks_of(&frame, 1), (1, 1));
    }

    #[test]
    fn the_coefficient_store_is_padded_to_whole_mcus() {
        let store = Coefficients::new(&frame());
        assert_eq!(store.across, vec![2, 1]);
        assert_eq!(store.down, vec![2, 1]);
        assert_eq!(store.blocks[0].len(), 2 * 2 * 64);
        assert_eq!(store.blocks[1].len(), 64);
    }

    #[test]
    fn a_dc_band_is_told_apart_from_an_ac_one() {
        let dc = Band {
            start: 0,
            end: 0,
            high: 0,
            low: 1,
        };
        assert!(dc.is_dc());
        assert!(!dc.is_refinement());
        let ac = Band {
            start: 1,
            end: 63,
            high: 1,
            low: 0,
        };
        assert!(!ac.is_dc());
        assert!(ac.is_refinement());
    }

    #[test]
    fn a_correction_bit_moves_a_coefficient_away_from_zero() {
        let mut store = Coefficients::new(&frame());
        store.block_mut(0, 0, 0)[5] = 4;
        // A set bit extends a positive coefficient upward.
        refine(
            &mut BitReader::new(&[0b1000_0000]),
            &mut store,
            0,
            0,
            0,
            5,
            2,
        )
        .unwrap();
        assert_eq!(store.block_mut(0, 0, 0)[5], 6);

        store.block_mut(0, 0, 0)[6] = -4;
        refine(
            &mut BitReader::new(&[0b1000_0000]),
            &mut store,
            0,
            0,
            0,
            6,
            2,
        )
        .unwrap();
        assert_eq!(store.block_mut(0, 0, 0)[6], -6);
    }

    #[test]
    fn a_clear_correction_bit_leaves_the_coefficient_alone() {
        let mut store = Coefficients::new(&frame());
        store.block_mut(0, 0, 0)[5] = 4;
        refine(
            &mut BitReader::new(&[0b0000_0000]),
            &mut store,
            0,
            0,
            0,
            5,
            2,
        )
        .unwrap();
        assert_eq!(store.block_mut(0, 0, 0)[5], 4);
    }

    #[test]
    fn a_correction_bit_already_present_is_not_added_twice() {
        let mut store = Coefficients::new(&frame());
        store.block_mut(0, 0, 0)[5] = 6;
        refine(
            &mut BitReader::new(&[0b1000_0000]),
            &mut store,
            0,
            0,
            0,
            5,
            2,
        )
        .unwrap();
        assert_eq!(store.block_mut(0, 0, 0)[5], 6, "bit 2 was already set");
    }
}
