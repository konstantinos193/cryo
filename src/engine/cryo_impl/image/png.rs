//! PNG decoding.
//!
//! A PNG is a signature followed by *chunks*, each a length, a
//! four-letter type, its data and a CRC. Only four types matter here:
//! `IHDR` says how big the image is and how its samples are laid out,
//! `PLTE` carries a palette, `IDAT` carries the pixels — possibly
//! split across many chunks that have to be joined before
//! decompressing — and `IEND` ends the file.
//!
//! The part that is not obvious is *filtering*. Every scanline is
//! prefixed by one byte saying how it was transformed before
//! compression, and the transform is in terms of the already-decoded
//! neighbours: the pixel to the left, the one above, or both. So
//! unfiltering cannot be done in parallel or out of order — each row
//! needs the finished row above it, and each pixel the finished pixel
//! beside it.
//!
//! ```text
//!   IDAT ──▶ inflate ──▶ filtered rows ──▶ unfilter ──▶ samples ──▶ RGBA
//! ```
//!
//! Known gaps, stated rather than hidden:
//! - **Adam7 interlacing** is refused. It is seven passes of the whole
//!   pipeline above over subsampled grids, and nothing on the modern
//!   web is written that way.
//! - 16-bit samples are taken down to 8 by keeping the high byte.
//! - `gAMA`, `iCCP` and the rest are ignored: samples are taken as
//!   sRGB.

use super::inflate;
use super::{Image, ImageError};

/// The eight bytes every PNG starts with.
///
/// Not an arbitrary magic number: the high bit catches a transport
/// that strips it, the `\r\n` pair catches one that converts line
/// endings, and the `^Z` stops a DOS `type` listing the rest.
pub const SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// How a scanline was transformed before compression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    None,
    Sub,
    Up,
    Average,
    Paeth,
}

impl Filter {
    fn from_byte(byte: u8) -> Result<Self, ImageError> {
        Ok(match byte {
            0 => Self::None,
            1 => Self::Sub,
            2 => Self::Up,
            3 => Self::Average,
            4 => Self::Paeth,
            _ => return Err(ImageError::Malformed("unknown scanline filter")),
        })
    }
}

/// What a sample in this file means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorType {
    Grey,
    Rgb,
    Palette,
    GreyAlpha,
    Rgba,
}

impl ColorType {
    fn from_byte(byte: u8) -> Result<Self, ImageError> {
        Ok(match byte {
            0 => Self::Grey,
            2 => Self::Rgb,
            3 => Self::Palette,
            4 => Self::GreyAlpha,
            6 => Self::Rgba,
            _ => return Err(ImageError::Malformed("unknown colour type")),
        })
    }

    /// How many samples each pixel has.
    fn channels(self) -> usize {
        match self {
            Self::Grey | Self::Palette => 1,
            Self::GreyAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }
}

/// What `IHDR` says about the image.
struct Header {
    width: u32,
    height: u32,
    depth: u8,
    color: ColorType,
}

impl Header {
    /// Bytes per pixel, rounded up — the filter's stride, which is at
    /// least one even for sub-byte depths.
    fn stride(&self) -> usize {
        (self.color.channels() * usize::from(self.depth))
            .div_ceil(8)
            .max(1)
    }

    /// Bytes in one unfiltered scanline.
    fn row_bytes(&self) -> usize {
        (self.width as usize * self.color.channels() * usize::from(self.depth)).div_ceil(8)
    }
}

/// Decode a PNG into RGBA.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
    let mut rest = bytes.get(SIGNATURE.len()..).ok_or(ImageError::Truncated)?;

    let mut header: Option<Header> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut alphas: Vec<u8> = Vec::new();
    let mut data = Vec::new();

    while !rest.is_empty() {
        let (kind, chunk, remainder) = next_chunk(rest)?;
        rest = remainder;
        match &kind {
            b"IHDR" => header = Some(parse_header(chunk)?),
            b"PLTE" => {
                palette = chunk
                    .chunks_exact(3)
                    .map(|entry| [entry[0], entry[1], entry[2]])
                    .collect();
            }
            // Per-palette-entry alpha, or a single transparent colour
            // for the non-palette types. Only the palette form is read.
            b"tRNS" => alphas = chunk.to_vec(),
            b"IDAT" => data.extend_from_slice(chunk),
            b"IEND" => break,
            _ => {}
        }
    }

    let header = header.ok_or(ImageError::Malformed("no IHDR chunk"))?;
    if data.is_empty() {
        return Err(ImageError::Malformed("no image data"));
    }

    let samples = inflate::zlib(&data)?;
    let rows = unfilter(&samples, &header)?;
    let pixels = to_rgba(&rows, &header, &palette, &alphas)?;
    Image::new(header.width, header.height, pixels)
}

/// One chunk: its four-letter type, its data, and the rest of the file.
type Chunk<'a> = ([u8; 4], &'a [u8], &'a [u8]);

/// Split one chunk off the front.
fn next_chunk(bytes: &[u8]) -> Result<Chunk<'_>, ImageError> {
    let length = be_u32(bytes.get(..4).ok_or(ImageError::Truncated)?) as usize;
    let kind: [u8; 4] = bytes
        .get(4..8)
        .ok_or(ImageError::Truncated)?
        .try_into()
        .map_err(|_| ImageError::Truncated)?;
    let data = bytes.get(8..8 + length).ok_or(ImageError::Truncated)?;
    // Four more for the CRC, which is not checked: a corrupt image
    // shows as one, and refusing a file that decoded fine is worse.
    let rest = bytes.get(8 + length + 4..).unwrap_or(&[]);
    Ok((kind, data, rest))
}

fn parse_header(chunk: &[u8]) -> Result<Header, ImageError> {
    if chunk.len() < 13 {
        return Err(ImageError::Malformed("IHDR is too short"));
    }
    let width = be_u32(&chunk[0..4]);
    let height = be_u32(&chunk[4..8]);
    if width == 0 || height == 0 {
        return Err(ImageError::Malformed("an image with no pixels"));
    }
    // A hostile or broken file could otherwise ask for a buffer no
    // machine has; 64 megapixels is far past any real page's image.
    if width as u64 * height as u64 > 64_000_000 {
        return Err(ImageError::Unsupported("an image too large to decode"));
    }
    let depth = chunk[8];
    if !matches!(depth, 1 | 2 | 4 | 8 | 16) {
        return Err(ImageError::Malformed("unknown bit depth"));
    }
    if chunk[12] != 0 {
        return Err(ImageError::Unsupported("interlaced PNG"));
    }
    Ok(Header {
        width,
        height,
        depth,
        color: ColorType::from_byte(chunk[9])?,
    })
}

/// Undo the per-scanline filters, returning the raw sample rows.
///
/// Each row is reconstructed against the one above it, so this walks
/// strictly in order and keeps the previous row to hand.
fn unfilter(samples: &[u8], header: &Header) -> Result<Vec<u8>, ImageError> {
    let row_bytes = header.row_bytes();
    let stride = header.stride();
    let height = header.height as usize;

    let mut out = vec![0u8; row_bytes * height];
    let mut offset = 0;
    for row in 0..height {
        let filter = Filter::from_byte(*samples.get(offset).ok_or(ImageError::Truncated)?)?;
        offset += 1;
        let line = samples
            .get(offset..offset + row_bytes)
            .ok_or(ImageError::Truncated)?;
        offset += row_bytes;

        let start = row * row_bytes;
        for index in 0..row_bytes {
            // The three neighbours the filters are written against.
            let left = if index >= stride {
                out[start + index - stride]
            } else {
                0
            };
            let above = if row > 0 {
                out[start - row_bytes + index]
            } else {
                0
            };
            let above_left = if row > 0 && index >= stride {
                out[start - row_bytes + index - stride]
            } else {
                0
            };

            let value = line[index];
            out[start + index] = match filter {
                Filter::None => value,
                Filter::Sub => value.wrapping_add(left),
                Filter::Up => value.wrapping_add(above),
                // The average is taken in nine bits, so two large
                // bytes do not wrap before they are halved.
                Filter::Average => {
                    let average = ((u16::from(left) + u16::from(above)) / 2) as u8;
                    value.wrapping_add(average)
                }
                Filter::Paeth => value.wrapping_add(paeth(left, above, above_left)),
            };
        }
    }
    Ok(out)
}

/// The Paeth predictor: whichever neighbour the gradient points at.
///
/// It guesses the pixel from the three around it and picks the one its
/// own linear estimate lands closest to — which is why it does well on
/// both flat areas and sharp edges.
fn paeth(left: u8, above: u8, above_left: u8) -> u8 {
    let estimate = i16::from(left) + i16::from(above) - i16::from(above_left);
    let to_left = (estimate - i16::from(left)).abs();
    let to_above = (estimate - i16::from(above)).abs();
    let to_corner = (estimate - i16::from(above_left)).abs();
    if to_left <= to_above && to_left <= to_corner {
        left
    } else if to_above <= to_corner {
        above
    } else {
        above_left
    }
}

/// Turn raw samples into RGBA, whatever they were.
fn to_rgba(
    rows: &[u8],
    header: &Header,
    palette: &[[u8; 3]],
    alphas: &[u8],
) -> Result<Vec<u8>, ImageError> {
    let row_bytes = header.row_bytes();
    let width = header.width as usize;
    let height = header.height as usize;
    let channels = header.color.channels();
    let mut out = Vec::with_capacity(width * height * 4);

    for row in 0..height {
        let line = rows
            .get(row * row_bytes..(row + 1) * row_bytes)
            .ok_or(ImageError::Truncated)?;
        for column in 0..width {
            // A palette image stores an index, not an intensity, so
            // it is read raw — see `read_raw`.
            if header.color == ColorType::Palette {
                let index = usize::from(read_raw(line, column, header.depth)?);
                out.extend_from_slice(&from_palette(index, palette, alphas)?);
                continue;
            }
            let mut sample = [0u8; 4];
            for (channel, slot) in sample.iter_mut().take(channels).enumerate() {
                *slot = read_sample(line, column * channels + channel, header.depth)?;
            }
            out.extend_from_slice(&expand(sample, header.color, palette, alphas)?);
        }
    }
    Ok(out)
}

/// One sample, whatever the bit depth packs it as.
///
/// Depths below eight pack several samples into a byte, most
/// significant first; sixteen keeps the high byte, which is the
/// stated approximation.
fn read_sample(line: &[u8], index: usize, depth: u8) -> Result<u8, ImageError> {
    let raw = read_raw(line, index, depth)?;
    if depth >= 8 {
        return Ok(raw as u8);
    }
    // Spread a sub-byte value over the full range, so a 1-bit sample
    // reads as 0 or 255 rather than 0 or 1.
    let mask = (1u16 << depth) - 1;
    Ok((raw * 255 / mask) as u8)
}

/// One sample at its own scale, with no spreading.
///
/// A palette *index* must come through untouched: it names an entry,
/// not an intensity, so stretching it over 0..=255 points at the
/// wrong colour. Stretching a 4-bit index of 3 to 51 picks entry 51
/// of a 16-entry palette — which is to say, nothing.
fn read_raw(line: &[u8], index: usize, depth: u8) -> Result<u16, ImageError> {
    let fetch = |at: usize| line.get(at).copied().ok_or(ImageError::Truncated);
    Ok(match depth {
        8 => u16::from(fetch(index)?),
        // Sixteen bits keep the high byte; see the module note.
        16 => u16::from(fetch(index * 2)?),
        _ => {
            let per_byte = 8 / usize::from(depth);
            let byte = fetch(index / per_byte)?;
            let slot = index % per_byte;
            let shift = 8 - usize::from(depth) * (slot + 1);
            let mask = (1u16 << depth) - 1;
            (u16::from(byte) >> shift) & mask
        }
    })
}

/// One palette entry as RGBA.
///
/// `tRNS` gives per-entry alpha when it is present; an entry it does
/// not reach is opaque.
fn from_palette(index: usize, palette: &[[u8; 3]], alphas: &[u8]) -> Result<[u8; 4], ImageError> {
    let entry = palette
        .get(index)
        .copied()
        .ok_or(ImageError::Malformed("a palette index with no entry"))?;
    let alpha = alphas.get(index).copied().unwrap_or(255);
    Ok([entry[0], entry[1], entry[2], alpha])
}

/// One pixel's samples as RGBA.
fn expand(
    sample: [u8; 4],
    color: ColorType,
    palette: &[[u8; 3]],
    alphas: &[u8],
) -> Result<[u8; 4], ImageError> {
    Ok(match color {
        ColorType::Grey => [sample[0], sample[0], sample[0], 255],
        ColorType::GreyAlpha => [sample[0], sample[0], sample[0], sample[1]],
        ColorType::Rgb => [sample[0], sample[1], sample[2], 255],
        ColorType::Rgba => sample,
        // Handled before this, because an index is not a sample.
        ColorType::Palette => from_palette(usize::from(sample[0]), palette, alphas)?,
    })
}

fn be_u32(bytes: &[u8]) -> u32 {
    let mut value = 0u32;
    for &byte in bytes.iter().take(4) {
        value = (value << 8) | u32::from(byte);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::value::Color;

    /// A 2x2 PNG: red, green on the first row; blue, white on the
    /// second. Written by a real encoder, not by hand.
    const RGB_2X2: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00, 0x00, 0xfd,
        0xd4, 0x9a, 0x73, 0x00, 0x00, 0x00, 0x14, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8,
        0xcf, 0xc0, 0xc0, 0x00, 0xc2, 0x0c, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x00, 0x1f, 0xee, 0x05,
        0xfb, 0x60, 0x6c, 0x70, 0xf2, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
        0x60, 0x82,
    ];

    #[test]
    fn a_truecolour_png_decodes_to_its_pixels() {
        let image = decode(RGB_2X2).expect("decodes");
        assert_eq!((image.width(), image.height()), (2, 2));
        assert_eq!(image.pixel(0, 0), Some(Color::rgba(255, 0, 0, 1.0)));
        assert_eq!(image.pixel(1, 0), Some(Color::rgba(0, 255, 0, 1.0)));
        assert_eq!(image.pixel(0, 1), Some(Color::rgba(0, 0, 255, 1.0)));
        assert_eq!(image.pixel(1, 1), Some(Color::rgba(255, 255, 255, 1.0)));
    }

    #[test]
    fn bytes_that_are_not_a_png_are_refused() {
        assert!(decode(b"not a png at all").is_err());
    }

    #[test]
    fn a_png_with_no_data_is_refused() {
        let mut bytes = SIGNATURE.to_vec();
        // An IHDR saying 1x1 truecolour, and nothing else.
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(decode(&bytes), Err(ImageError::Malformed("no image data")));
    }

    #[test]
    fn an_interlaced_png_is_refused_rather_than_rendered_as_noise() {
        let mut bytes = SIGNATURE.to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 1]);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            decode(&bytes),
            Err(ImageError::Unsupported("interlaced PNG"))
        );
    }

    #[test]
    fn the_paeth_predictor_picks_the_nearest_neighbour() {
        // Flat: every neighbour agrees, so any of them is right.
        assert_eq!(paeth(10, 10, 10), 10);
        // A vertical edge: the left neighbour is the better guess.
        assert_eq!(paeth(200, 10, 10), 200);
        // A horizontal one: the neighbour above.
        assert_eq!(paeth(10, 200, 10), 200);
    }

    #[test]
    fn sub_byte_samples_spread_over_the_whole_range() {
        // One bit a sample: 0b1010_0000 is on, off, on, off.
        let line = [0b1010_0000];
        assert_eq!(read_sample(&line, 0, 1).unwrap(), 255);
        assert_eq!(read_sample(&line, 1, 1).unwrap(), 0);
        assert_eq!(read_sample(&line, 2, 1).unwrap(), 255);
        assert_eq!(read_sample(&line, 3, 1).unwrap(), 0);
    }

    #[test]
    fn a_sixteen_bit_sample_keeps_its_high_byte() {
        let line = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(read_sample(&line, 0, 16).unwrap(), 0x12);
        assert_eq!(read_sample(&line, 1, 16).unwrap(), 0x56);
    }

    #[test]
    fn a_row_is_unfiltered_against_the_one_above_it() {
        // Two rows of one grey pixel: the first unfiltered at 10, the
        // second filtered with Up carrying a delta of 5.
        let header = Header {
            width: 1,
            height: 2,
            depth: 8,
            color: ColorType::Grey,
        };
        let samples = [0, 10, 2, 5];
        assert_eq!(unfilter(&samples, &header).unwrap(), vec![10, 15]);
    }
    /// A 4x1 palette image: three entries, indices 0, 1, 2, 0, with
    /// `tRNS` giving each entry its own alpha.
    const PALETTE_4X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x01, 0x08, 0x03, 0x00, 0x00, 0x00, 0xce,
        0xe2, 0xff, 0xff, 0x00, 0x00, 0x00, 0x09, 0x50, 0x4c, 0x54, 0x45, 0xff, 0x00, 0x00, 0x00,
        0xff, 0x00, 0x00, 0x00, 0xff, 0x2d, 0x4a, 0xcd, 0x8a, 0x00, 0x00, 0x00, 0x03, 0x74, 0x52,
        0x4e, 0x53, 0xff, 0x80, 0x00, 0x7f, 0x6d, 0x68, 0x78, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44,
        0x41, 0x54, 0x78, 0xda, 0x63, 0x60, 0x60, 0x64, 0x62, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x04,
        0x16, 0xc6, 0x87, 0x83, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
        0x82,
    ];

    #[test]
    fn a_palette_index_names_an_entry_rather_than_an_intensity() {
        // The bug this guards: scaling the index by the palette's
        // size. With three entries, index 2 became 0 and a logo came
        // out in the wrong colours at a fraction of its opacity.
        let image = decode(PALETTE_4X1).expect("decodes");
        assert_eq!((image.width(), image.height()), (4, 1));
        assert_eq!(image.pixel(0, 0), Some(Color::rgba(255, 0, 0, 1.0)));
        assert_eq!(
            image.pixel(1, 0),
            Some(Color::rgba(0, 255, 0, 128.0 / 255.0))
        );
        assert_eq!(image.pixel(2, 0), Some(Color::rgba(0, 0, 255, 0.0)));
        assert_eq!(image.pixel(3, 0), Some(Color::rgba(255, 0, 0, 1.0)));
    }

    #[test]
    fn a_sub_byte_sample_is_still_spread_but_an_index_is_not() {
        // Greyscale at one bit: 0 or 255, not 0 or 1.
        assert_eq!(read_sample(&[0b1000_0000], 0, 1).unwrap(), 255);
        // The same bits as an index: entry 1, not entry 255.
        assert_eq!(read_raw(&[0b1000_0000], 0, 1).unwrap(), 1);
    }
}
