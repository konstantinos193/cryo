//! JPEG decoding, baseline and progressive.
//!
//! A JPEG is a sequence of *markers*, each `0xFF` followed by a code.
//! Four kinds carry what is needed: `DQT` the quantisation tables,
//! `SOF` the frame's size and components, `DHT` the Huffman tables,
//! and `SOS` a scan — after which entropy-coded data runs until the
//! next marker.
//!
//! ```text
//!   markers ──▶ scans ──▶ coefficients ──▶ dequantise ──▶ IDCT ──▶ planes ──▶ RGBA
//! ```
//!
//! Baseline and progressive differ only in the middle: one scan
//! carrying everything, or several each refining a band. Both fill
//! the same coefficient store, so everything after it is shared — see
//! [`scan`] for the four algorithms a scan may be.
//!
//! Two things about the format are easy to get wrong and worth
//! naming:
//!
//! - **The DC coefficient is a difference** from the previous block
//!   of the same component, not a value. Lose one and everything
//!   after it drifts.
//! - **Components may be sampled at different resolutions.** Chroma
//!   is usually half the luma's each way, so each component is
//!   decoded into a plane of its own size and they are only brought
//!   together at the end.
//!
//! Known gaps, stated rather than hidden:
//! - Arithmetic coding, 12-bit samples and CMYK are refused rather
//!   than approximated.
//! - Chroma is upsampled nearest-neighbour, so a strong colour edge
//!   is blockier than a browser would draw it.
//! - No colour management: samples are taken as sRGB.

mod bits;
mod idct;
mod scan;

use bits::Huffman;
use scan::{Band, Coefficients};

use super::{Image, ImageError};

/// The start-of-image marker every JPEG begins with.
pub const SIGNATURE: &[u8] = &[0xff, 0xd8];

/// One component of the frame.
#[derive(Clone, Debug)]
pub(super) struct Component {
    id: u8,
    /// Sampling factors: how many blocks of this component sit in one
    /// MCU, across and down.
    horizontal: usize,
    vertical: usize,
    quantisation: usize,
    /// Set by each scan header, not by the frame.
    dc_table: usize,
    ac_table: usize,
}

/// What `SOF` says about the image.
pub(super) struct Frame {
    width: usize,
    height: usize,
    max_horizontal: usize,
    max_vertical: usize,
    mcus_across: usize,
    mcus_down: usize,
    components: Vec<Component>,
}

/// Decode a JPEG into RGBA.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
    let mut quantisation = [[1u16; 64]; 4];
    let mut dc_tables: [Huffman; 4] = Default::default();
    let mut ac_tables: [Huffman; 4] = Default::default();
    let mut frame: Option<Frame> = None;
    let mut coefficients: Option<Coefficients> = None;
    let mut restart_interval = 0usize;

    let mut at = SIGNATURE.len();
    loop {
        let Some(marker) = next_marker(bytes, &mut at) else {
            break;
        };
        match marker {
            // End of image.
            0xd9 => break,
            // Markers with no payload.
            0x01 | 0xd0..=0xd7 => continue,
            _ => {}
        }

        let length = be_u16(bytes.get(at..at + 2).ok_or(ImageError::Truncated)?) as usize;
        if length < 2 {
            return Err(ImageError::Malformed("a segment shorter than its header"));
        }
        let segment = bytes
            .get(at + 2..at + length)
            .ok_or(ImageError::Truncated)?;
        at += length;

        match marker {
            // A frame header. `SOF0`/`SOF1` are baseline, `SOF2` is
            // progressive; the three decode the same way from here.
            0xc0..=0xc2 => {
                let parsed = parse_frame(segment)?;
                coefficients = Some(Coefficients::new(&parsed));
                frame = Some(parsed);
            }
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                return Err(ImageError::Unsupported(
                    "a JPEG coding this engine cannot read",
                ))
            }
            0xc4 => parse_huffman_tables(segment, &mut dc_tables, &mut ac_tables)?,
            0xdb => parse_quantisation_tables(segment, &mut quantisation)?,
            0xdd => {
                restart_interval = be_u16(segment.get(..2).ok_or(ImageError::Truncated)?) as usize;
            }
            0xda => {
                let frame = frame
                    .as_mut()
                    .ok_or(ImageError::Malformed("a scan before its frame"))?;
                let store = coefficients
                    .as_mut()
                    .ok_or(ImageError::Malformed("a scan before its frame"))?;
                let (components, band) = parse_scan_header(segment, frame)?;
                let entropy = bytes.get(at..).ok_or(ImageError::Truncated)?;
                scan::decode(
                    entropy,
                    frame,
                    &components,
                    band,
                    &dc_tables,
                    &ac_tables,
                    restart_interval,
                    store,
                )?;
                // Step over this scan's data to whatever marker ends
                // it. A progressive file has more scans to come.
                at += entropy_length(entropy);
            }
            // APPn, COM and anything else: skipped.
            _ => {}
        }
    }

    let frame = frame.ok_or(ImageError::Malformed("no frame header"))?;
    let store = coefficients.ok_or(ImageError::Malformed("no image data"))?;
    assemble(&frame, &store, &quantisation)
}

/// The next marker's code, stepping over any fill bytes before it.
fn next_marker(bytes: &[u8], at: &mut usize) -> Option<u8> {
    while bytes.get(*at) == Some(&0xff) && bytes.get(*at + 1) == Some(&0xff) {
        *at += 1;
    }
    match bytes.get(*at..*at + 2) {
        Some([0xff, marker]) => {
            *at += 2;
            Some(*marker)
        }
        _ => None,
    }
}

/// How many bytes of entropy-coded data there are before the marker
/// that ends them.
///
/// Restart markers are part of the data; anything else ends it.
fn entropy_length(entropy: &[u8]) -> usize {
    let mut at = 0;
    while at + 1 < entropy.len() {
        if entropy[at] == 0xff {
            let next = entropy[at + 1];
            // `FF 00` is a stuffed byte, `FF D0..D7` a restart.
            if next != 0x00 && !(0xd0..=0xd7).contains(&next) {
                return at;
            }
            at += 2;
            continue;
        }
        at += 1;
    }
    entropy.len()
}

/// `SOF`: the image's size and its components.
fn parse_frame(segment: &[u8]) -> Result<Frame, ImageError> {
    let [precision, rest @ ..] = segment else {
        return Err(ImageError::Truncated);
    };
    if *precision != 8 {
        return Err(ImageError::Unsupported(
            "a JPEG with more than 8-bit samples",
        ));
    }
    let height = be_u16(rest.get(..2).ok_or(ImageError::Truncated)?) as usize;
    let width = be_u16(rest.get(2..4).ok_or(ImageError::Truncated)?) as usize;
    let count = usize::from(*rest.get(4).ok_or(ImageError::Truncated)?);
    if width == 0 || height == 0 {
        return Err(ImageError::Malformed("an image with no pixels"));
    }
    // A hostile or broken file could otherwise ask for a buffer no
    // machine has.
    if width * height > 64_000_000 {
        return Err(ImageError::Unsupported("an image too large to decode"));
    }
    if !matches!(count, 1 | 3) {
        return Err(ImageError::Unsupported(
            "a JPEG that is not greyscale or YCbCr",
        ));
    }

    let mut components = Vec::with_capacity(count);
    for index in 0..count {
        let entry = rest
            .get(5 + index * 3..8 + index * 3)
            .ok_or(ImageError::Truncated)?;
        let horizontal = usize::from(entry[1] >> 4);
        let vertical = usize::from(entry[1] & 0x0f);
        if !(1..=4).contains(&horizontal) || !(1..=4).contains(&vertical) {
            return Err(ImageError::Malformed("an impossible sampling factor"));
        }
        components.push(Component {
            id: entry[0],
            horizontal,
            vertical,
            quantisation: usize::from(entry[2] & 3),
            dc_table: 0,
            ac_table: 0,
        });
    }

    let max_horizontal = components.iter().map(|c| c.horizontal).max().unwrap_or(1);
    let max_vertical = components.iter().map(|c| c.vertical).max().unwrap_or(1);
    Ok(Frame {
        width,
        height,
        max_horizontal,
        max_vertical,
        mcus_across: width.div_ceil(8 * max_horizontal),
        mcus_down: height.div_ceil(8 * max_vertical),
        components,
    })
}

/// `DQT`: one or more quantisation tables, in zigzag order.
fn parse_quantisation_tables(
    mut segment: &[u8],
    tables: &mut [[u16; 64]; 4],
) -> Result<(), ImageError> {
    while let [header, rest @ ..] = segment {
        let index = usize::from(header & 3);
        // The high nibble picks 8- or 16-bit entries.
        let wide = header >> 4 != 0;
        let size = if wide { 128 } else { 64 };
        let values = rest.get(..size).ok_or(ImageError::Truncated)?;
        for (slot, entry) in tables[index].iter_mut().enumerate() {
            *entry = if wide {
                be_u16(&values[slot * 2..slot * 2 + 2])
            } else {
                u16::from(values[slot])
            };
        }
        segment = &rest[size..];
    }
    Ok(())
}

/// `DHT`: one or more Huffman tables.
fn parse_huffman_tables(
    mut segment: &[u8],
    dc: &mut [Huffman; 4],
    ac: &mut [Huffman; 4],
) -> Result<(), ImageError> {
    while let [header, rest @ ..] = segment {
        let index = usize::from(header & 3);
        let is_ac = header >> 4 != 0;
        let counts: [u8; 16] = rest
            .get(..16)
            .ok_or(ImageError::Truncated)?
            .try_into()
            .map_err(|_| ImageError::Truncated)?;
        let total: usize = counts.iter().map(|&n| usize::from(n)).sum();
        let symbols = rest
            .get(16..16 + total)
            .ok_or(ImageError::Truncated)?
            .to_vec();
        let table = Huffman::new(counts, symbols);
        if is_ac {
            ac[index] = table;
        } else {
            dc[index] = table;
        }
        segment = &rest[16 + total..];
    }
    Ok(())
}

/// `SOS`: which components this scan covers, their tables, and the
/// band of coefficients it carries.
fn parse_scan_header(segment: &[u8], frame: &mut Frame) -> Result<(Vec<usize>, Band), ImageError> {
    let count = usize::from(*segment.first().ok_or(ImageError::Truncated)?);
    let mut indices = Vec::with_capacity(count);
    for index in 0..count {
        let entry = segment
            .get(1 + index * 2..3 + index * 2)
            .ok_or(ImageError::Truncated)?;
        let position = frame
            .components
            .iter()
            .position(|c| c.id == entry[0])
            .ok_or(ImageError::Malformed("a scan for an unknown component"))?;
        scan::assign_tables(
            &mut frame.components[position],
            usize::from(entry[1] >> 4) & 3,
            usize::from(entry[1] & 0x0f) & 3,
        );
        indices.push(position);
    }

    // The three bytes after the component list. A baseline file writes
    // 0, 63, 0 here, which is the whole band at full precision — so
    // baseline needs no special case.
    let tail = segment
        .get(1 + count * 2..4 + count * 2)
        .ok_or(ImageError::Truncated)?;
    let band = Band {
        start: usize::from(tail[0]).min(63),
        end: usize::from(tail[1]).min(63),
        high: tail[2] >> 4,
        low: tail[2] & 0x0f,
    };
    if band.start > band.end {
        return Err(ImageError::Malformed(
            "a scan band that ends before it starts",
        ));
    }
    Ok((indices, band))
}

/// Dequantise, transform, and bring the planes together as RGBA.
fn assemble(
    frame: &Frame,
    coefficients: &Coefficients,
    quantisation: &[[u16; 64]; 4],
) -> Result<Image, ImageError> {
    // Each component's samples at its own resolution.
    let mut planes = Vec::with_capacity(frame.components.len());
    for (index, component) in frame.components.iter().enumerate() {
        let across = coefficients.across[index];
        let down = coefficients.down[index];
        let table = &quantisation[component.quantisation];
        let (width, height) = (across * 8, down * 8);
        let mut plane = vec![0u8; width * height];

        for block_y in 0..down {
            for block_x in 0..across {
                let offset = (block_y * across + block_x) * 64;
                let block = &coefficients.blocks[index][offset..offset + 64];

                // Dequantise into natural order. The quantisation
                // table is in zigzag order; the coefficients are not.
                let mut dequantised = [0i32; 64];
                for (slot, &factor) in table.iter().enumerate() {
                    let at = idct::ZIGZAG[slot];
                    dequantised[at] = block[at] * i32::from(factor);
                }

                let samples = idct::idct(&dequantised);
                for row in 0..8 {
                    let target = (block_y * 8 + row) * width + block_x * 8;
                    plane[target..target + 8].copy_from_slice(&samples[row * 8..row * 8 + 8]);
                }
            }
        }
        planes.push((plane, width, height));
    }

    let mut pixels = Vec::with_capacity(frame.width * frame.height * 4);
    let sample = |index: usize, x: usize, y: usize| -> u8 {
        let component = &frame.components[index];
        let (plane, width, height) = &planes[index];
        // Nearest-neighbour upsampling: a half-resolution plane is
        // read at half the coordinate.
        let sx = (x * component.horizontal / frame.max_horizontal).min(width - 1);
        let sy = (y * component.vertical / frame.max_vertical).min(height - 1);
        plane[sy * width + sx]
    };

    for y in 0..frame.height {
        for x in 0..frame.width {
            let [red, green, blue] = match frame.components.len() {
                1 => {
                    let value = sample(0, x, y);
                    [value, value, value]
                }
                3 => ycbcr_to_rgb(sample(0, x, y), sample(1, x, y), sample(2, x, y)),
                _ => return Err(ImageError::Unsupported("an unexpected component count")),
            };
            pixels.extend_from_slice(&[red, green, blue, 255]);
        }
    }
    Image::new(frame.width as u32, frame.height as u32, pixels)
}

/// JPEG's colour space, which keeps brightness apart from colour so
/// the colour can be sent at lower resolution.
fn ycbcr_to_rgb(luma: u8, blue_chroma: u8, red_chroma: u8) -> [u8; 3] {
    let y = f32::from(luma);
    let cb = f32::from(blue_chroma) - 128.0;
    let cr = f32::from(red_chroma) - 128.0;
    let clamp = |value: f32| value.round().clamp(0.0, 255.0) as u8;
    [
        clamp(y + 1.402 * cr),
        clamp(y - 0.344_136 * cb - 0.714_136 * cr),
        clamp(y + 1.772 * cb),
    ]
}

fn be_u16(bytes: &[u8]) -> u16 {
    match bytes {
        [high, low, ..] => (u16::from(*high) << 8) | u16::from(*low),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_that_are_not_a_jpeg_are_refused() {
        assert!(decode(b"\xff\xd8not really").is_err());
    }

    #[test]
    fn an_arithmetic_coded_jpeg_is_refused_rather_than_rendered_as_noise() {
        // SOF9 is arithmetic-coded baseline.
        let bytes = [0xff, 0xd8, 0xff, 0xc9, 0x00, 0x02];
        assert_eq!(
            decode(&bytes),
            Err(ImageError::Unsupported(
                "a JPEG coding this engine cannot read"
            ))
        );
    }

    #[test]
    fn ycbcr_with_no_chroma_is_grey() {
        assert_eq!(ycbcr_to_rgb(100, 128, 128), [100, 100, 100]);
    }

    #[test]
    fn ycbcr_separates_brightness_from_colour() {
        // Strong red: luma low, Cr high. 82 + 1.402 * 112 is 239.
        let [red, green, blue] = ycbcr_to_rgb(82, 90, 240);
        assert!(red > 230, "red was {red}");
        assert!(green < 40, "green was {green}");
        assert!(blue < 40, "blue was {blue}");
    }

    #[test]
    fn entropy_data_ends_at_a_marker_but_not_at_a_restart() {
        // A restart marker and a stuffed byte are both data.
        assert_eq!(entropy_length(&[1, 2, 0xff, 0xd0, 3, 0xff, 0xd9]), 5);
        assert_eq!(entropy_length(&[1, 0xff, 0x00, 2, 0xff, 0xda]), 4);
        // No marker at all: all of it.
        assert_eq!(entropy_length(&[1, 2, 3]), 3);
    }

    #[test]
    fn a_frame_works_out_its_mcu_grid() {
        // 17x17 pixels with 2x2 luma sampling: MCUs are 16x16, so two
        // across and two down, the second of each mostly padding.
        let segment = [8, 0, 17, 0, 17, 3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1];
        let frame = parse_frame(&segment).unwrap();
        assert_eq!((frame.mcus_across, frame.mcus_down), (2, 2));
        assert_eq!((frame.max_horizontal, frame.max_vertical), (2, 2));
        assert_eq!(frame.components.len(), 3);
    }

    #[test]
    fn a_frame_with_no_pixels_is_refused() {
        let segment = [8, 0, 0, 0, 0, 1, 1, 0x11, 0];
        assert_eq!(
            parse_frame(&segment).err(),
            Some(ImageError::Malformed("an image with no pixels"))
        );
    }
}
