//! Images: bytes on the wire become pixels on the page.
//!
//! One decoded image is a rectangle of straight-alpha RGBA, whatever
//! it arrived as — a palette PNG, a greyscale one and a baseline JPEG
//! all come out the same shape, so layout and paint need to know
//! nothing about formats.
//!
//! ```text
//!   bytes ──▶ sniff ──▶ png | jpeg ──▶ Image (RGBA)
//! ```
//!
//! The format is decided by the bytes' own *signature*, not by the
//! file extension and not by the `Content-Type`. A server that calls a
//! PNG `image/jpeg` is common enough that trusting it would mean
//! failing on real pages, and the signatures are unambiguous.
//!
//! Known gaps, stated rather than hidden:
//! - **Progressive JPEG** is not decoded, only baseline. A
//!   progressive file is refused rather than rendered as noise.
//! - **Interlaced (Adam7) PNG** is refused for the same reason.
//! - SVG is rasterised at its own size and then scaled like any
//!   other picture, which gives up its resolution independence —
//!   see [`svg::raster`].
//! - No WebP, AVIF or GIF. Each is a separate codec, and the first
//!   two are video frames in a wrapper.
//! - No colour management: an embedded ICC profile is ignored and the
//!   samples are taken as sRGB.

pub mod inflate;
pub mod jpeg;
pub mod png;
pub mod store;

use std::fmt;

pub use store::{Images, NoImages, Store};

use crate::engine::cryo_impl::css::value::Color;
use crate::engine::cryo_impl::svg;

/// A decoded image: straight-alpha RGBA, row-major, no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    width: u32,
    height: u32,
    /// Four bytes a pixel. Always `width * height * 4` long, which is
    /// the invariant [`Image::new`] exists to keep.
    pixels: Vec<u8>,
}

impl Image {
    /// Build an image, checking that the buffer matches the size.
    ///
    /// The one door in: a decoder that miscounts its rows is found
    /// here rather than by a panic in paint three stages later.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, ImageError> {
        let wanted = width as usize * height as usize * 4;
        if pixels.len() != wanted {
            return Err(ImageError::WrongSize {
                expected: wanted,
                found: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The colour at a pixel, or `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (y as usize * self.width as usize + x as usize) * 4;
        let sample = self.pixels.get(offset..offset + 4)?;
        Some(Color {
            red: sample[0],
            green: sample[1],
            blue: sample[2],
            // The engine keeps alpha as 0..1 and straight, not
            // premultiplied — the same convention a CSS colour uses.
            alpha: f32::from(sample[3]) / 255.0,
        })
    }

    /// The colour at a point given in 0..1 across the image.
    ///
    /// Nearest-neighbour: the sample is whichever pixel the point
    /// falls in. Good enough to see a photograph; a scaled-down image
    /// will alias until there is a filtered sampler to replace this.
    pub fn sample(&self, u: f32, v: f32) -> Option<Color> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let x = (u * self.width as f32)
            .floor()
            .clamp(0.0, (self.width - 1) as f32);
        let y = (v * self.height as f32)
            .floor()
            .clamp(0.0, (self.height - 1) as f32);
        self.pixel(x as u32, y as u32)
    }
}

/// Why an image could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageError {
    /// The bytes match no signature this engine knows.
    UnknownFormat,
    /// The format is recognised but a feature in the file is not.
    Unsupported(&'static str),
    /// The file is the right format and malformed.
    Malformed(&'static str),
    /// The data ran out before the image did.
    Truncated,
    /// A decoder produced the wrong number of bytes for its own
    /// dimensions — a bug caught at the boundary.
    WrongSize { expected: usize, found: usize },
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFormat => f.write_str("not an image this engine decodes"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::Malformed(what) => write!(f, "malformed image: {what}"),
            Self::Truncated => f.write_str("the image data ends early"),
            Self::WrongSize { expected, found } => {
                write!(f, "decoder produced {found} bytes, not {expected}")
            }
        }
    }
}

impl std::error::Error for ImageError {}

impl From<inflate::InflateError> for ImageError {
    fn from(error: inflate::InflateError) -> Self {
        match error {
            inflate::InflateError::Truncated => Self::Truncated,
            _ => Self::Malformed("the compressed image data is corrupt"),
        }
    }
}

/// Decode an image, choosing the decoder by the bytes' signature.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
    if bytes.starts_with(png::SIGNATURE) {
        return png::decode(bytes);
    }
    if bytes.starts_with(jpeg::SIGNATURE) {
        return jpeg::decode(bytes);
    }
    // SVG is XML and has no binary signature, so it is sniffed last —
    // after the formats that can be identified for certain.
    if svg::raster::is_svg(bytes) {
        let (width, height, pixels) =
            svg::raster::rasterize(bytes).ok_or(ImageError::Malformed("no <svg> element"))?;
        return Image::new(width, height, pixels);
    }
    Err(ImageError::UnknownFormat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_must_match_its_own_dimensions() {
        assert!(Image::new(2, 2, vec![0; 16]).is_ok());
        assert_eq!(
            Image::new(2, 2, vec![0; 15]),
            Err(ImageError::WrongSize {
                expected: 16,
                found: 15
            })
        );
    }

    #[test]
    fn a_pixel_outside_the_image_is_none() {
        let image = Image::new(1, 1, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(image.pixel(0, 0), Some(Color::rgba(1, 2, 3, 4.0 / 255.0)));
        assert!(image.pixel(1, 0).is_none());
        assert!(image.pixel(0, 1).is_none());
    }

    #[test]
    fn sampling_maps_the_unit_square_onto_the_pixels() {
        // Two pixels side by side: red then blue.
        let image = Image::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        assert_eq!(image.sample(0.0, 0.0), Some(Color::rgba(255, 0, 0, 1.0)));
        assert_eq!(image.sample(0.49, 0.5), Some(Color::rgba(255, 0, 0, 1.0)));
        assert_eq!(image.sample(0.51, 0.5), Some(Color::rgba(0, 0, 255, 1.0)));
        // The far edge belongs to the last pixel, not past it.
        assert_eq!(image.sample(1.0, 1.0), Some(Color::rgba(0, 0, 255, 1.0)));
    }

    #[test]
    fn bytes_matching_no_signature_are_refused() {
        assert_eq!(decode(b"not an image"), Err(ImageError::UnknownFormat));
        assert_eq!(decode(&[]), Err(ImageError::UnknownFormat));
    }
}
