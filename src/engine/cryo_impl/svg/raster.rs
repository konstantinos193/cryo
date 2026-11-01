//! Turning a standalone `.svg` file into pixels.
//!
//! An inline `<svg>` is already in the page's DOM and goes straight to
//! [`super::render`]. A linked one — `<img src="logo.svg">` or
//! `background-image: url(logo.svg)` — arrives as bytes instead, and
//! everything downstream of the network expects a *picture*: a
//! rectangle of RGBA that can be scaled into a box.
//!
//! So a linked SVG is rasterised once, at its own intrinsic size, and
//! from there it travels the same path a PNG does.
//!
//! That is a real trade-off and worth naming: an SVG is resolution-
//! independent and this throws that away, so a logo rasterised at 64
//! pixels and drawn at 256 is blurry where a browser's would be
//! sharp. Rasterising at the box's size instead means knowing the box
//! before the image is decoded, which inverts the order layout and
//! loading happen in. The blur is the lesser problem, and the
//! intrinsic size is taken generously to keep it small.
//!
//! This returns raw bytes rather than an `Image` so that the image
//! module can depend on this one and not the other way round.

use crate::engine::cryo_impl::html::parse as parse_html;
use crate::engine::cryo_impl::layout::geometry::Rect;
use crate::engine::cryo_impl::scan;

use super::{render, Filled};

/// The size a linked SVG is rasterised at when it states none.
const DEFAULT_SIZE: f32 = 256.0;

/// An upper bound, so a file claiming to be ten thousand pixels wide
/// cannot ask for a buffer no machine has.
const MAX_SIZE: f32 = 2048.0;

/// Whether these bytes look like an SVG document.
///
/// SVG has no binary signature — it is XML — so this sniffs the first
/// markup it finds. A leading `<?xml` or a comment may come first.
pub fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start();
    (text.starts_with("<?xml") || text.starts_with("<!--") || text.starts_with('<'))
        && text.contains("<svg")
}

/// Rasterise an SVG file into straight-alpha RGBA.
///
/// Returns the size and the pixels, or `None` if the bytes hold no
/// `<svg>` element.
pub fn rasterize(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let text = String::from_utf8_lossy(bytes);
    // The HTML parser handles SVG as foreign content, which is enough
    // for a file whose root is `<svg>`: the names and attributes come
    // through, and no HTML-specific recovery applies to them.
    let document = parse_html(&text);
    let root = document.descendants(document.root()).find(|&id| {
        document
            .element(id)
            .is_some_and(|element| element.name.local.as_str() == "svg")
    })?;

    let (width, height) = intrinsic_size(&document, root);
    let rect = Rect::new(0.0, 0.0, width, height);
    // A linked file has no element to take a colour from, so
    // `currentColor` falls back to black — what a browser does for an
    // image, which has no text around it either.
    let shapes = render(
        &document,
        root,
        rect,
        crate::engine::cryo_impl::css::value::Color::BLACK,
    );

    let (pixels_wide, pixels_high) = (width as u32, height as u32);
    let mut pixels = vec![0u8; pixels_wide as usize * pixels_high as usize * 4];
    for shape in &shapes {
        paint_into(&mut pixels, pixels_wide, pixels_high, shape);
    }
    Some((pixels_wide, pixels_high, pixels))
}

/// How big the drawing says it is.
///
/// `width`/`height` attributes first, then the `viewBox`'s own size,
/// then a default — and scaled up if it is small, since a 24-unit
/// icon rasterised at 24 pixels is unusable anywhere larger.
fn intrinsic_size(
    document: &crate::engine::cryo_impl::dom::Document,
    root: crate::engine::cryo_impl::dom::NodeId,
) -> (f32, f32) {
    let element = document.element(root);
    let attribute = |name: &str| {
        element
            .and_then(|element| element.attr(name))
            .and_then(|text| text.trim().trim_end_matches("px").parse::<f32>().ok())
            .filter(|value| *value > 0.0)
    };

    let from_view_box = element
        .and_then(|element| element.attr("viewBox"))
        .and_then(|text| {
            let numbers: Vec<f32> = text
                .split(|c: char| c.is_ascii_whitespace() || c == ',')
                .filter(|part| !part.is_empty())
                .filter_map(|part| part.parse().ok())
                .collect();
            match numbers[..] {
                [_, _, width, height] if width > 0.0 && height > 0.0 => Some((width, height)),
                _ => None,
            }
        });

    let (width, height) = match (attribute("width"), attribute("height"), from_view_box) {
        (Some(width), Some(height), _) => (width, height),
        (_, _, Some(size)) => size,
        _ => (DEFAULT_SIZE, DEFAULT_SIZE),
    };

    // Scale a small drawing up so it survives being enlarged later.
    let scale = (DEFAULT_SIZE / width.max(height)).max(1.0);
    (
        (width * scale).clamp(1.0, MAX_SIZE),
        (height * scale).clamp(1.0, MAX_SIZE),
    )
}

/// Composite one filled shape into the buffer.
fn paint_into(pixels: &mut [u8], width: u32, height: u32, shape: &Filled) {
    if shape.color.is_transparent() {
        return;
    }
    let mask = scan::fill_with(&shape.polygons, width, height, shape.rule);
    for y in 0..height {
        for x in 0..width {
            let coverage = f32::from(mask.at(x, y)) / 255.0 * shape.color.alpha;
            if coverage <= 0.0 {
                continue;
            }
            let at = (y as usize * width as usize + x as usize) * 4;
            let Some(slot) = pixels.get_mut(at..at + 4) else {
                continue;
            };
            // Source-over, with straight alpha — the same blend the
            // canvas does, on a buffer instead of a screen.
            let existing = f32::from(slot[3]) / 255.0;
            let out = coverage + existing * (1.0 - coverage);
            if out <= 0.0 {
                continue;
            }
            let mix = |source: u8, destination: u8| {
                let value = (f32::from(source) * coverage
                    + f32::from(destination) * existing * (1.0 - coverage))
                    / out;
                value.round().clamp(0.0, 255.0) as u8
            };
            slot[0] = mix(shape.color.red, slot[0]);
            slot[1] = mix(shape.color.green, slot[1]);
            slot[2] = mix(shape.color.blue, slot[2]);
            slot[3] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two hashes: the colour's own `"#` would close a single one.
    const LOGO: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
        <rect width="10" height="10" fill="#ff0000"/>
    </svg>"##;

    #[test]
    fn an_svg_file_is_recognised_by_its_markup() {
        assert!(is_svg(LOGO.as_bytes()));
        assert!(is_svg(
            br#"<?xml version="1.0"?><svg viewBox="0 0 1 1"></svg>"#
        ));
        assert!(!is_svg(b"\x89PNG\r\n\x1a\n"));
        assert!(!is_svg(b"<html><body>no</body></html>"));
    }

    #[test]
    fn a_filled_square_rasterises_to_opaque_pixels() {
        let (width, height, pixels) = rasterize(LOGO.as_bytes()).expect("rasterises");
        assert!(width > 0 && height > 0);
        let middle = ((height / 2) as usize * width as usize + (width / 2) as usize) * 4;
        assert_eq!(&pixels[middle..middle + 4], &[255, 0, 0, 255]);
    }

    #[test]
    fn a_small_icon_is_scaled_up_so_it_survives_enlarging() {
        // A 24-unit icon rasterised at 24 pixels would be unusable in
        // a 64-pixel box.
        let (width, _, _) = rasterize(
            br#"<svg viewBox="0 0 24 24"><rect width="24" height="24" fill="red"/></svg>"#,
        )
        .expect("rasterises");
        assert!(width >= 256, "got {width}");
    }

    #[test]
    fn explicit_dimensions_win_over_the_view_box() {
        let (width, height, _) = rasterize(
            br#"<svg width="40" height="20" viewBox="0 0 10 10"><rect width="10" height="10" fill="red"/></svg>"#,
        )
        .expect("rasterises");
        // Scaled up together, so the ratio is kept.
        assert_eq!(width / height, 2);
    }

    #[test]
    fn an_area_no_shape_covers_stays_transparent() {
        let (width, height, pixels) =
            rasterize(br#"<svg viewBox="0 0 10 10"><rect width="2" height="2" fill="red"/></svg>"#)
                .expect("rasterises");
        let corner = ((height - 1) as usize * width as usize + (width - 1) as usize) * 4;
        assert_eq!(pixels[corner + 3], 0, "the far corner is empty");
    }

    #[test]
    fn bytes_with_no_svg_in_them_yield_nothing() {
        assert!(rasterize(b"<html><body>hello</body></html>").is_none());
    }
}
