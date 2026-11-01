//! The rasteriser: draw commands become pixels.
//!
//! A straightforward software target — RGBA8, one byte per channel, rows
//! top to bottom — because the first thing that has to work is *correct*
//! output, and a correct software path is also the reference a GPU path
//! is later checked against.
//!
//! **Alpha is straight, not premultiplied.** Premultiplying is faster to
//! composite and is what a GPU wants, but it loses colour precision at
//! low alpha and makes every intermediate value harder to assert in a
//! test. The conversion belongs at the surface boundary, not here.
//!
//! Compositing is source-over, the only operator CSS needs for
//! backgrounds and text:
//!
//! ```text
//!   out_alpha = src_a + dst_a * (1 - src_a)
//!   out_color = (src_c * src_a + dst_c * dst_a * (1 - src_a)) / out_alpha
//! ```
//!
//! The division is what straight alpha costs; it is also why a zero
//! result alpha has to be special-cased rather than producing NaN.
//!
//! **Pixel snapping:** each edge of a rectangle is rounded to the
//! nearest pixel boundary. A 0.5px-wide border therefore vanishes or
//! becomes 1px depending on where it falls, which is what every browser
//! does without anti-aliasing. Anti-aliased edges are a later step and
//! will change these numbers.

use crate::engine::cryo_impl::css::computed::{Corners, FontStyle};
use crate::engine::cryo_impl::css::value::{Color, Gradient};

use super::super::image::{Image, Images};
use super::super::layout::geometry::Rect;
use super::clip::ClipShape;
use super::display_list::{DisplayItem, DisplayList};
use super::font::outline_font::FaceRequest;
use super::font::{FontChoice, GlyphSource, RenderedGlyph};
use crate::engine::cryo_impl::scan;

/// Bytes per pixel: red, green, blue, alpha.
const CHANNELS: usize = 4;

/// A pixel buffer that can be drawn into.
#[derive(Clone, Debug, PartialEq)]
pub struct Canvas {
    width: u32,
    height: u32,
    /// Row-major RGBA8, `width * height * 4` bytes.
    pixels: Vec<u8>,
    /// The clip shapes in force, innermost last.
    ///
    /// Nested clips intersect, so a pixel has to survive every one of
    /// them — which is why this is a stack and not a single shape.
    clips: Vec<ClipShape>,
}

impl Canvas {
    /// A fully transparent canvas.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; Self::byte_len(width, height)],
            clips: Vec::new(),
        }
    }

    /// A canvas pre-filled with one colour — the page's base, so that
    /// nothing has to special-case "no background was painted here".
    pub fn filled(width: u32, height: u32, color: Color) -> Self {
        let mut canvas = Self::new(width, height);
        canvas.fill_rect(Rect::new(0.0, 0.0, width as f32, height as f32), color);
        canvas
    }

    fn byte_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * CHANNELS
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The raw RGBA bytes, for handing to a surface.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// One pixel's colour. `None` when out of bounds, so a caller
    /// reading near an edge cannot silently read a wrapped-around row.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Color> {
        let at = self.offset(x, y)?;
        let bytes = self.pixels.get(at..at + CHANNELS)?;
        Some(Color::rgba(
            bytes[0],
            bytes[1],
            bytes[2],
            f32::from(bytes[3]) / 255.0,
        ))
    }

    fn offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some((y as usize * self.width as usize + x as usize) * CHANNELS)
    }

    /// Composite `color` over the pixels `rect` covers.
    ///
    /// The rectangle is clipped to the canvas, so a box hanging off the
    /// edge paints its visible part rather than being dropped.
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        if color.is_transparent() {
            return;
        }
        let Some((left, top, right, bottom)) = self.snap(rect) else {
            return;
        };
        for y in top..bottom {
            for x in left..right {
                self.blend_pixel(x, y, color);
            }
        }
    }

    /// Fill `rect` with a gradient, one pixel at a time.
    ///
    /// Per-pixel because the colour varies across the box; a solid fill
    /// can blend a whole span at once, and this cannot. It is the
    /// slowest thing the rasteriser does and the first candidate for a
    /// cached span once that matters.
    pub fn fill_gradient(&mut self, rect: Rect, gradient: &Gradient) {
        let Some((left, top, right, bottom)) = self.snap(rect) else {
            return;
        };
        for y in top..bottom {
            for x in left..right {
                // Sample at the pixel's centre, relative to the box.
                let along = gradient.progress(
                    x as f32 + 0.5 - rect.x,
                    y as f32 + 0.5 - rect.y,
                    rect.width,
                    rect.height,
                );
                self.blend_pixel(x, y, gradient.sample(along));
            }
        }
    }

    /// Fill polygons given in page coordinates, anti-aliased.
    ///
    /// The same scanline filler the glyphs and the rounded corners go
    /// through — an SVG path is a closed outline like any other, so
    /// its curves get the same coverage anti-aliasing a letter's do.
    pub fn fill_shape(&mut self, polygons: &[Vec<(f32, f32)>], color: Color, rule: scan::FillRule) {
        if color.is_transparent() || polygons.is_empty() {
            return;
        }
        // The mask covers only what the shape touches, not the whole
        // element: an icon's glyph is usually a fraction of its box.
        let bounds = bounding_box(polygons);
        let Some((origin_x, origin_y, width, height)) = self.mask_area(bounds) else {
            return;
        };
        let local: Vec<Vec<(f32, f32)>> = polygons
            .iter()
            .map(|ring| {
                ring.iter()
                    .map(|&(x, y)| (x - origin_x, y - origin_y))
                    .collect()
            })
            .collect();
        let mask = scan::fill_with(&local, width, height, rule);

        for row in 0..height {
            for column in 0..width {
                let coverage = f32::from(mask.at(column, row)) / 255.0;
                if coverage <= 0.0 {
                    continue;
                }
                let (x, y) = (origin_x + column as f32, origin_y + row as f32);
                if x < 0.0 || y < 0.0 {
                    continue;
                }
                self.blend_coverage(x as u32, y as u32, color, coverage);
            }
        }
    }

    /// Draw a soft-edged shadow.
    ///
    /// The shape is rasterised into a coverage mask and the mask is
    /// blurred, rather than the pixels being blurred in place: a
    /// shadow is one colour at varying opacity, so there is only one
    /// channel worth softening and the colour never smears.
    ///
    /// The mask is grown by the blur radius on every side first, or
    /// the softening would have nowhere to spread and the shadow
    /// would come out with a hard edge where it was cropped.
    pub fn draw_shadow(&mut self, rect: Rect, radius: Corners<f32>, blur: f32, color: Color) {
        let margin = blur.max(0.0).ceil();
        let grown = Rect::new(
            rect.x - margin,
            rect.y - margin,
            rect.width + margin * 2.0,
            rect.height + margin * 2.0,
        );
        let Some((origin_x, origin_y, width, height)) = self.mask_area(grown) else {
            return;
        };

        let outline = rounded_outline(rect.x - origin_x, rect.y - origin_y, rect, radius);
        let mut mask = scan::fill(&[outline], width, height);
        if blur > 0.0 {
            blur_mask(&mut mask, width, height, blur);
        }

        for row in 0..height {
            for column in 0..width {
                let coverage = f32::from(mask.at(column, row)) / 255.0;
                if coverage <= 0.0 {
                    continue;
                }
                let (x, y) = (origin_x + column as f32, origin_y + row as f32);
                if x < 0.0 || y < 0.0 {
                    continue;
                }
                self.blend_coverage(x as u32, y as u32, color, coverage);
            }
        }
    }

    /// Draw an image stretched to fill `rect`.
    ///
    /// Stretched, not fitted: `background-size` is not implemented, so
    /// a photograph with a different aspect ratio to its box comes out
    /// distorted rather than cropped. A stated approximation — the
    /// alternative, leaving it out, shows nothing at all.
    pub fn draw_image(&mut self, rect: Rect, image: &Image, radius: Corners<f32>) {
        let sample = |x: u32, y: u32| {
            let u = (x as f32 + 0.5 - rect.x) / rect.width;
            let v = (y as f32 + 0.5 - rect.y) / rect.height;
            image.sample(u, v).unwrap_or(Color::TRANSPARENT)
        };
        if radius.is_square() {
            let Some((left, top, right, bottom)) = self.snap(rect) else {
                return;
            };
            for y in top..bottom {
                for x in left..right {
                    self.blend_pixel(x, y, sample(x, y));
                }
            }
            return;
        }
        self.fill_rounded(rect, radius, sample);
    }

    /// Fill a rounded rectangle, anti-aliased.
    ///
    /// Reuses the scanline filler the glyphs go through rather than
    /// growing a second rasteriser: a rounded rectangle is a closed
    /// outline like any other, so its corners get the same coverage
    /// anti-aliasing a curve in a letter does. The alternative — a
    /// hand-written arc loop — would be a second place for the same
    /// edge arithmetic to be slightly different.
    pub fn fill_rounded(
        &mut self,
        rect: Rect,
        radius: Corners<f32>,
        mut color: impl FnMut(u32, u32) -> Color,
    ) {
        let Some(mask_area) = self.mask_area(rect) else {
            return;
        };
        let (origin_x, origin_y, width, height) = mask_area;
        let outline = rounded_outline(rect.x - origin_x, rect.y - origin_y, rect, radius);
        let mask = scan::fill(&[outline], width, height);

        for row in 0..height {
            for column in 0..width {
                let coverage = f32::from(mask.at(column, row)) / 255.0;
                if coverage <= 0.0 {
                    continue;
                }
                let (x, y) = (origin_x + column as f32, origin_y + row as f32);
                if x < 0.0 || y < 0.0 {
                    continue;
                }
                let (x, y) = (x as u32, y as u32);
                self.blend_coverage(x, y, color(x, y), coverage);
            }
        }
    }

    /// Where a rounded fill's coverage mask sits, and how big it is.
    ///
    /// Snapped outward to whole pixels so the mask covers every pixel
    /// the shape touches, and the sub-pixel remainder is kept by
    /// offsetting the outline rather than by rounding the rectangle.
    fn mask_area(&self, rect: Rect) -> Option<(f32, f32, u32, u32)> {
        if rect.is_empty() {
            return None;
        }
        let origin_x = rect.x.floor();
        let origin_y = rect.y.floor();
        let width = (rect.right() - origin_x).ceil();
        let height = (rect.bottom() - origin_y).ceil();
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        // A box far off-canvas would otherwise allocate a mask the size
        // of wherever it was put.
        if origin_x > self.width as f32 || origin_y > self.height as f32 {
            return None;
        }
        Some((origin_x, origin_y, width as u32, height as u32))
    }

    /// Composite `color` at one pixel, weighted by `coverage` in 0..=1.
    ///
    /// Coverage is how a glyph's anti-aliased edge arrives: the colour is
    /// the text's, and only the alpha varies across the glyph.
    pub fn blend_coverage(&mut self, x: u32, y: u32, color: Color, coverage: f32) {
        let coverage = coverage * self.clip_coverage(x, y);
        if coverage <= 0.0 {
            return;
        }
        let weighted = Color {
            alpha: color.alpha * coverage.min(1.0),
            ..color
        };
        self.blend_pixel(x, y, weighted);
    }

    /// Round a rectangle to whole pixels and clip it to the canvas.
    ///
    /// `None` when nothing is left — fully outside, or so thin it
    /// rounded away.
    fn snap(&self, rect: Rect) -> Option<(u32, u32, u32, u32)> {
        let left = rect.x.round().max(0.0);
        let top = rect.y.round().max(0.0);
        let right = rect.right().round().min(self.width as f32);
        let bottom = rect.bottom().round().min(self.height as f32);
        if right <= left || bottom <= top {
            return None;
        }
        Some((left as u32, top as u32, right as u32, bottom as u32))
    }

    fn blend_pixel(&mut self, x: u32, y: u32, source: Color) {
        // A clipped pixel is faded rather than skipped, so the edge
        // of a curved clip is smooth instead of stepped.
        let kept = self.clip_coverage(x, y);
        if kept <= 0.0 {
            return;
        }
        if kept < 1.0 {
            let source = Color {
                alpha: source.alpha * kept,
                ..source
            };
            return self.blend_unclipped(x, y, source);
        }
        self.blend_unclipped(x, y, source)
    }

    /// How much of a pixel every clip in force agrees to keep.
    fn clip_coverage(&self, x: u32, y: u32) -> f32 {
        let mut kept = 1.0;
        for clip in &self.clips {
            kept *= clip.coverage(x, y);
            if kept <= 0.0 {
                return 0.0;
            }
        }
        kept
    }

    fn blend_unclipped(&mut self, x: u32, y: u32, source: Color) {
        let Some(at) = self.offset(x, y) else {
            return;
        };
        let Some(slot) = self.pixels.get_mut(at..at + CHANNELS) else {
            return;
        };

        let source_alpha = source.alpha.clamp(0.0, 1.0);
        // The common case, and worth taking: an opaque fill needs no
        // blend at all, and most backgrounds are opaque.
        if source_alpha >= 1.0 {
            slot[0] = source.red;
            slot[1] = source.green;
            slot[2] = source.blue;
            slot[3] = 255;
            return;
        }

        let destination_alpha = f32::from(slot[3]) / 255.0;
        let out_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
        if out_alpha <= 0.0 {
            // Nothing on either side; leave it transparent rather than
            // dividing by zero.
            slot.fill(0);
            return;
        }

        let mix = |source_channel: u8, destination_channel: u8| -> u8 {
            let source_value = f32::from(source_channel) * source_alpha;
            let destination_value =
                f32::from(destination_channel) * destination_alpha * (1.0 - source_alpha);
            ((source_value + destination_value) / out_alpha)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        slot[0] = mix(source.red, slot[0]);
        slot[1] = mix(source.green, slot[1]);
        slot[2] = mix(source.blue, slot[2]);
        slot[3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    /// Draw a run of text with its left edge at `x` and its baseline at
    /// `baseline_y`.
    ///
    /// Returns how many characters the font had no glyph for. Reporting
    /// them rather than drawing a blank is what stops a missing script
    /// from looking like a layout bug: the text occupied its space and
    /// simply had nothing to put there.
    #[allow(clippy::too_many_arguments)] // a glyph run needs all of them
    pub fn draw_text(
        &mut self,
        text: &str,
        x: f32,
        baseline_y: f32,
        font_size: f32,
        color: Color,
        font: &dyn GlyphSource,
        embolden: f32,
        letter_spacing: f32,
    ) -> usize {
        if color.is_transparent() || font_size <= 0.0 {
            return 0;
        }
        let mut pen = x;
        let mut missing = 0;
        for character in text.chars() {
            match font.render(character, font_size) {
                Some(mut glyph) => {
                    if embolden > 0.0 {
                        thicken(&mut glyph.mask, embolden);
                    }
                    self.draw_glyph(&glyph, pen, baseline_y, color);
                }
                // A blank glyph — a space — is not missing; only a
                // character the font cannot represent at all counts.
                None if character.is_whitespace() => {}
                None => missing += 1,
            }
            pen += font.advance_px(character, font_size) + letter_spacing;
        }
        missing
    }

    /// Composite one rasterised glyph, its mask placed against the pen
    /// position and the baseline.
    fn draw_glyph(&mut self, glyph: &RenderedGlyph, pen: f32, baseline_y: f32, color: Color) {
        // Snap the mask's corner to whole pixels: the coverage inside it
        // was computed on that grid, and resampling it to a fractional
        // offset would blur every glyph.
        let left = (pen + glyph.left).round() as i32;
        let top = (baseline_y + glyph.top).round() as i32;

        for row in 0..glyph.mask.height() {
            for column in 0..glyph.mask.width() {
                let coverage = glyph.mask.at(column, row);
                if coverage == 0 {
                    continue;
                }
                let Ok(x) = u32::try_from(left + column as i32) else {
                    continue;
                };
                let Ok(y) = u32::try_from(top + row as i32) else {
                    continue;
                };
                self.blend_coverage(x, y, color, f32::from(coverage) / 255.0);
            }
        }
    }

    /// Paint a whole display list, in order.
    ///
    /// Returns how many characters could not be drawn for want of a
    /// glyph; zero means the page rendered completely. Pass
    /// [`NoGlyphs`](super::font::NoGlyphs) to rasterise boxes only.
    pub fn render(
        &mut self,
        list: &DisplayList,
        fonts: &dyn FontChoice,
        images: &dyn Images,
    ) -> usize {
        let mut missing = 0;
        for item in &list.items {
            match item {
                // The square case stays on the fast path: most boxes
                // on a page have no radius, and a whole-rectangle fill
                // needs no coverage mask.
                DisplayItem::SolidRect {
                    rect,
                    color,
                    radius,
                } if radius.is_square() => self.fill_rect(*rect, *color),
                DisplayItem::SolidRect {
                    rect,
                    color,
                    radius,
                } => {
                    let color = *color;
                    self.fill_rounded(*rect, *radius, |_, _| color);
                }
                DisplayItem::Gradient {
                    rect,
                    gradient,
                    radius,
                } if radius.is_square() => self.fill_gradient(*rect, gradient),
                DisplayItem::Gradient {
                    rect,
                    gradient,
                    radius,
                } => {
                    let rect = *rect;
                    self.fill_rounded(rect, *radius, |x, y| {
                        let along = gradient.progress(
                            x as f32 + 0.5 - rect.x,
                            y as f32 + 0.5 - rect.y,
                            rect.width,
                            rect.height,
                        );
                        gradient.sample(along)
                    });
                }
                DisplayItem::PushClip { shape } => self.clips.push(shape.clone()),
                DisplayItem::PopClip => {
                    self.clips.pop();
                }
                DisplayItem::Shape {
                    polygons,
                    color,
                    rule,
                    ..
                } => {
                    self.fill_shape(polygons, *color, *rule);
                }
                DisplayItem::Shadow {
                    rect,
                    radius,
                    blur,
                    color,
                } => self.draw_shadow(*rect, *radius, *blur, *color),
                // An image that never arrived draws nothing and
                // leaves whatever is behind it — which is what a
                // browser shows while one is still loading.
                DisplayItem::Image { rect, url, radius } => {
                    if let Some(image) = images.get(url) {
                        self.draw_image(*rect, image, *radius);
                    }
                }
                DisplayItem::Text {
                    rect,
                    baseline,
                    text,
                    color,
                    font_size,
                    letter_spacing,
                    families,
                    weight,
                    style,
                } => {
                    // One run may need more than one face. A webfont
                    // arrives cut into subsets by alphabet, all under
                    // one family name, so the face that draws the
                    // first letter of "Agents" may hold no lowercase
                    // at all — and choosing once per run leaves the
                    // rest of the word blank.
                    let request = FaceRequest {
                        families,
                        weight: *weight,
                        italic: *style != FontStyle::Normal,
                        sample: None,
                    };
                    let mut rest = text.as_str();
                    let mut pen = rect.x;
                    while !rest.is_empty() {
                        let Some(font) = fonts.choose(request.for_text(rest)) else {
                            // No face for this run at all: every
                            // character goes undrawn, and the count
                            // has to say so.
                            missing += rest.chars().count();
                            break;
                        };
                        let segment = super::font::covered_prefix(rest, font);
                        let embolden = embolden_by(*weight, font.weight(), *font_size);
                        missing += self.draw_text(
                            segment,
                            pen,
                            rect.y + baseline,
                            *font_size,
                            *color,
                            font,
                            embolden,
                            *letter_spacing,
                        );
                        pen += segment
                            .chars()
                            .map(|character| {
                                font.advance_px(character, *font_size) + letter_spacing
                            })
                            .sum::<f32>();
                        rest = &rest[segment.len()..];
                    }
                }
            }
        }
        missing
    }
}

/// Thicken a glyph's coverage, for a weight the font does not hold.
///
/// Every browser does this, and a page needs it more than it sounds:
/// a webfont is often shipped as one *variable* file covering 400 to
/// 700, and a renderer that cannot move the weight axis has exactly
/// one weight to offer. Drawing a page's headings at its body weight
/// flattens the whole hierarchy.
///
/// The method is the old one: smear the mask sideways by a fraction
/// of an em. It thickens the stems, which is where weight is read
/// from, and leaves the horizontals alone — which is why a
/// synthesised bold looks close but never quite like a drawn one.
fn thicken(mask: &mut crate::engine::cryo_impl::scan::Mask, amount: f32) {
    let steps = amount.round().max(1.0) as u32;
    let (width, height) = (mask.width(), mask.height());
    for y in 0..height {
        // Right to left, so a pixel is never smeared into itself.
        let row: Vec<u8> = (0..width).map(|x| mask.at(x, y)).collect();
        for x in 0u32..width {
            let start = x.saturating_sub(steps);
            let widest = (start..=x)
                .filter_map(|at| row.get(at as usize))
                .copied()
                .max()
                .unwrap_or(0);
            if widest > row[x as usize] {
                mask.set(x, y, widest);
            }
        }
    }
}

/// How far to smear a face drawn at the wrong weight.
///
/// Zero when the face already has the weight asked for, which is the
/// common case and costs nothing.
fn embolden_by(requested: u16, available: u16, font_size: f32) -> f32 {
    if requested < available.saturating_add(100) {
        return 0.0;
    }
    // A fortieth of an em per 300 units of weight: about one pixel at
    // a 16px body size going from regular to bold, which is what a
    // drawn bold adds to a stem.
    let steps = f32::from(requested - available) / 300.0;
    (font_size / 40.0 * steps).clamp(0.0, font_size / 8.0)
}

/// Soften a coverage mask in place.
///
/// Three box blurs in a row, which is the standard way to get a
/// Gaussian cheaply: the first pass gives a flat average, the second
/// a triangle, and by the third the result is close enough to a bell
/// that no eye can tell. CSS defines a shadow's blur as a Gaussian of
/// half the stated radius, so the box width follows from that.
///
/// Separable, so each pass is a horizontal sweep and a vertical one
/// rather than a square of samples per pixel.
fn blur_mask(mask: &mut scan::Mask, width: u32, height: u32, blur: f32) {
    // The box width that approximates the Gaussian CSS asks for.
    let radius = ((blur / 2.0) * 1.5).round().max(1.0) as i32;
    for _ in 0..3 {
        box_pass(mask, width, height, radius, true);
        box_pass(mask, width, height, radius, false);
    }
}

/// One box-blur sweep, along the rows or down the columns.
fn box_pass(mask: &mut scan::Mask, width: u32, height: u32, radius: i32, horizontal: bool) {
    let (outer, inner) = if horizontal {
        (height, width)
    } else {
        (width, height)
    };
    let mut line = vec![0u8; inner as usize];
    let window = (radius * 2 + 1) as u32;

    for major in 0..outer {
        for minor in 0..inner {
            let mut total = 0u32;
            for offset in -radius..=radius {
                let at = minor as i32 + offset;
                // Outside the mask counts as nothing, which is what
                // makes a shadow fade off its own edges.
                if at < 0 || at >= inner as i32 {
                    continue;
                }
                let (x, y) = if horizontal {
                    (at as u32, major)
                } else {
                    (major, at as u32)
                };
                total += u32::from(mask.at(x, y));
            }
            line[minor as usize] = (total / window) as u8;
        }
        for (minor, &value) in line.iter().enumerate() {
            let (x, y) = if horizontal {
                (minor as u32, major)
            } else {
                (major, minor as u32)
            };
            mask.set(x, y, value);
        }
    }
}

/// The smallest rectangle covering every point.
fn bounding_box(polygons: &[Vec<(f32, f32)>]) -> Rect {
    let mut left = f32::MAX;
    let mut top = f32::MAX;
    let mut right = f32::MIN;
    let mut bottom = f32::MIN;
    for ring in polygons {
        for &(x, y) in ring {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
    }
    if left > right || top > bottom {
        return Rect::new(0.0, 0.0, 0.0, 0.0);
    }
    Rect::new(left, top, right - left, bottom - top)
}

/// A rounded rectangle as one closed outline, corners flattened to
/// line segments.
///
/// `offset_x`/`offset_y` place it inside its coverage mask, keeping
/// the sub-pixel part of the rectangle's position that snapping the
/// mask to whole pixels would otherwise throw away.
fn rounded_outline(
    offset_x: f32,
    offset_y: f32,
    rect: Rect,
    radius: Corners<f32>,
) -> Vec<(f32, f32)> {
    use std::f32::consts::PI;

    let (width, height) = (rect.width, rect.height);
    let mut points = Vec::new();
    // Clockwise from the top-left corner, in screen coordinates where
    // y grows downward — so angle 0 points right and PI/2 points down.
    let corners = [
        (
            radius.top_left,
            radius.top_left,
            radius.top_left,
            PI,
            1.5 * PI,
        ),
        (
            width - radius.top_right,
            radius.top_right,
            radius.top_right,
            1.5 * PI,
            2.0 * PI,
        ),
        (
            width - radius.bottom_right,
            height - radius.bottom_right,
            radius.bottom_right,
            0.0,
            0.5 * PI,
        ),
        (
            radius.bottom_left,
            height - radius.bottom_left,
            radius.bottom_left,
            0.5 * PI,
            PI,
        ),
    ];

    for (centre_x, centre_y, corner, from, to) in corners {
        // A square corner is one point, not an arc of nothing.
        if corner <= 0.0 {
            points.push((offset_x + centre_x, offset_y + centre_y));
            continue;
        }
        // Enough segments that the step along the arc stays under
        // about a pixel, which is below what the coverage mask can
        // show anyway.
        let steps = (corner * 0.8).ceil().clamp(4.0, 32.0) as usize;
        for step in 0..=steps {
            let angle = from + (to - from) * (step as f32 / steps as f32);
            points.push((
                offset_x + centre_x + corner * angle.cos(),
                offset_y + centre_y + corner * angle.sin(),
            ));
        }
    }
    points
}

#[cfg(test)]
mod tests {
    use super::super::display_list::DisplayItem;
    use super::super::font::{BuiltinFont, NoGlyphs};
    use super::*;
    use crate::engine::cryo_impl::image::NoImages;

    const RED: Color = Color::rgb(255, 0, 0);
    const BLUE: Color = Color::rgb(0, 0, 255);

    fn at(canvas: &Canvas, x: u32, y: u32) -> Color {
        canvas.pixel(x, y).expect("the pixel is in bounds")
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 0.01
    }

    #[test]
    fn a_new_canvas_is_fully_transparent() {
        let canvas = Canvas::new(4, 3);
        assert_eq!(canvas.width(), 4);
        assert_eq!(canvas.height(), 3);
        assert_eq!(canvas.pixels().len(), 4 * 3 * 4);
        assert!(canvas.pixels().iter().all(|&byte| byte == 0));
        assert!(at(&canvas, 0, 0).is_transparent());
    }

    #[test]
    fn a_filled_canvas_starts_with_its_colour_everywhere() {
        let canvas = Canvas::filled(3, 2, Color::WHITE);
        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(at(&canvas, x, y), Color::WHITE);
            }
        }
    }

    #[test]
    fn reading_outside_the_canvas_gives_nothing() {
        let canvas = Canvas::new(2, 2);
        assert!(canvas.pixel(2, 0).is_none());
        assert!(canvas.pixel(0, 2).is_none());
        assert!(canvas.pixel(99, 99).is_none());
    }

    #[test]
    fn filling_covers_exactly_the_rectangle() {
        let mut canvas = Canvas::new(5, 5);
        canvas.fill_rect(Rect::new(1.0, 1.0, 2.0, 2.0), RED);
        assert_eq!(at(&canvas, 1, 1), RED);
        assert_eq!(at(&canvas, 2, 2), RED);
        // Just outside on every side.
        assert!(at(&canvas, 0, 1).is_transparent());
        assert!(at(&canvas, 3, 1).is_transparent());
        assert!(at(&canvas, 1, 0).is_transparent());
        assert!(at(&canvas, 1, 3).is_transparent());
    }

    #[test]
    fn a_rectangle_hanging_off_the_edge_paints_its_visible_part() {
        let mut canvas = Canvas::new(3, 3);
        canvas.fill_rect(Rect::new(-5.0, -5.0, 7.0, 7.0), RED);
        assert_eq!(at(&canvas, 0, 0), RED);
        assert_eq!(at(&canvas, 1, 1), RED);
        assert!(at(&canvas, 2, 2).is_transparent());
    }

    #[test]
    fn a_rectangle_entirely_outside_paints_nothing() {
        let mut canvas = Canvas::new(3, 3);
        canvas.fill_rect(Rect::new(10.0, 10.0, 5.0, 5.0), RED);
        canvas.fill_rect(Rect::new(-10.0, 0.0, 5.0, 5.0), RED);
        assert!(canvas.pixels().iter().all(|&byte| byte == 0));
    }

    #[test]
    fn a_transparent_colour_paints_nothing() {
        let mut canvas = Canvas::filled(2, 2, Color::WHITE);
        canvas.fill_rect(Rect::new(0.0, 0.0, 2.0, 2.0), Color::TRANSPARENT);
        assert_eq!(at(&canvas, 0, 0), Color::WHITE);
    }

    #[test]
    fn an_empty_rectangle_paints_nothing() {
        let mut canvas = Canvas::new(3, 3);
        canvas.fill_rect(Rect::new(1.0, 1.0, 0.0, 5.0), RED);
        canvas.fill_rect(Rect::new(1.0, 1.0, 5.0, 0.0), RED);
        assert!(canvas.pixels().iter().all(|&byte| byte == 0));
    }

    #[test]
    fn an_opaque_fill_replaces_what_was_there() {
        let mut canvas = Canvas::filled(2, 2, RED);
        canvas.fill_rect(Rect::new(0.0, 0.0, 2.0, 2.0), BLUE);
        assert_eq!(at(&canvas, 0, 0), BLUE);
    }

    #[test]
    fn a_half_transparent_fill_lands_halfway() {
        let mut canvas = Canvas::filled(1, 1, Color::WHITE);
        canvas.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0), Color::rgba(0, 0, 0, 0.5));
        let blended = at(&canvas, 0, 0);
        // Black at 50% over white: mid grey, still opaque.
        assert_eq!(blended.red, 128);
        assert_eq!(blended.green, 128);
        assert_eq!(blended.blue, 128);
        assert!(close(blended.alpha, 1.0));
    }

    #[test]
    fn blending_onto_transparent_keeps_the_sources_colour() {
        let mut canvas = Canvas::new(1, 1);
        canvas.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0), Color::rgba(255, 0, 0, 0.5));
        let blended = at(&canvas, 0, 0);
        // The colour is not darkened toward the transparent black
        // underneath — that is the point of straight alpha.
        assert_eq!((blended.red, blended.green, blended.blue), (255, 0, 0));
        assert!(close(blended.alpha, 0.5));
    }

    #[test]
    fn two_half_transparent_fills_accumulate_alpha() {
        let mut canvas = Canvas::new(1, 1);
        let half_red = Color::rgba(255, 0, 0, 0.5);
        canvas.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0), half_red);
        canvas.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0), half_red);
        // 0.5 + 0.5 * 0.5 = 0.75
        assert!(close(at(&canvas, 0, 0).alpha, 0.75));
    }

    #[test]
    fn later_fills_cover_earlier_ones() {
        let mut canvas = Canvas::new(4, 1);
        canvas.fill_rect(Rect::new(0.0, 0.0, 4.0, 1.0), RED);
        canvas.fill_rect(Rect::new(2.0, 0.0, 2.0, 1.0), BLUE);
        assert_eq!(at(&canvas, 1, 0), RED);
        assert_eq!(at(&canvas, 2, 0), BLUE);
    }

    #[test]
    fn edges_round_to_the_nearest_pixel() {
        let mut canvas = Canvas::new(5, 1);
        // 1.4 rounds to 1, 1.4 + 2.2 = 3.6 rounds to 4.
        canvas.fill_rect(Rect::new(1.4, 0.0, 2.2, 1.0), RED);
        assert!(at(&canvas, 0, 0).is_transparent());
        assert_eq!(at(&canvas, 1, 0), RED);
        assert_eq!(at(&canvas, 3, 0), RED);
        assert!(at(&canvas, 4, 0).is_transparent());
    }

    #[test]
    fn a_sub_pixel_rectangle_survives_or_vanishes_by_where_it_falls() {
        // Both edges round, so a hairline's fate depends on its
        // position, not only its width. 1.1..1.3 rounds to 1..1 and
        // disappears; 1.1..1.5 rounds to 1..2 and becomes a full pixel.
        let mut vanishes = Canvas::new(3, 1);
        vanishes.fill_rect(Rect::new(1.1, 0.0, 0.2, 1.0), RED);
        assert!(
            vanishes.pixels().iter().all(|&byte| byte == 0),
            "1.1..1.3 should round away"
        );

        let mut survives = Canvas::new(3, 1);
        survives.fill_rect(Rect::new(1.1, 0.0, 0.4, 1.0), RED);
        assert_eq!(at(&survives, 1, 0), RED, "1.1..1.5 should round to 1..2");
        assert!(at(&survives, 2, 0).is_transparent());
    }

    #[test]
    fn coverage_scales_the_alpha_of_a_pixel() {
        let mut canvas = Canvas::filled(1, 1, Color::WHITE);
        canvas.blend_coverage(0, 0, Color::BLACK, 0.5);
        assert_eq!(at(&canvas, 0, 0).red, 128);
    }

    #[test]
    fn zero_coverage_draws_nothing_and_full_coverage_is_opaque() {
        let mut canvas = Canvas::filled(2, 1, Color::WHITE);
        canvas.blend_coverage(0, 0, Color::BLACK, 0.0);
        canvas.blend_coverage(1, 0, Color::BLACK, 1.0);
        assert_eq!(at(&canvas, 0, 0), Color::WHITE);
        assert_eq!(at(&canvas, 1, 0), Color::BLACK);
    }

    #[test]
    fn coverage_above_one_is_clamped_rather_than_overshooting() {
        let mut canvas = Canvas::filled(1, 1, Color::WHITE);
        canvas.blend_coverage(0, 0, Color::BLACK, 5.0);
        assert_eq!(at(&canvas, 0, 0), Color::BLACK);
    }

    #[test]
    fn coverage_outside_the_canvas_is_ignored() {
        let mut canvas = Canvas::new(1, 1);
        canvas.blend_coverage(5, 5, Color::BLACK, 1.0);
        assert!(at(&canvas, 0, 0).is_transparent());
    }

    #[test]
    fn rendering_a_list_paints_its_rectangles_in_order() {
        let list = DisplayList {
            items: vec![
                DisplayItem::SolidRect {
                    rect: Rect::new(0.0, 0.0, 4.0, 1.0),
                    color: RED,
                    radius: Corners::all(0.0),
                },
                DisplayItem::SolidRect {
                    rect: Rect::new(2.0, 0.0, 2.0, 1.0),
                    color: BLUE,
                    radius: Corners::all(0.0),
                },
            ],
        };
        let mut canvas = Canvas::new(4, 1);
        assert_eq!(
            canvas.render(&list, &NoGlyphs, &NoImages),
            0,
            "nothing was skipped"
        );
        assert_eq!(at(&canvas, 0, 0), RED);
        assert_eq!(at(&canvas, 3, 0), BLUE);
    }

    /// A text display item, for the render tests.
    fn text_item(text: &str, font_size: f32) -> DisplayItem {
        use crate::engine::cryo_impl::css::computed::FontStyle;
        DisplayItem::Text {
            rect: Rect::new(0.0, 0.0, 40.0, 10.0),
            baseline: 7.0,
            text: text.into(),
            color: Color::BLACK,
            font_size,
            letter_spacing: 0.0,
            weight: 400,
            style: FontStyle::Normal,
            families: Vec::new(),
        }
    }

    #[test]
    fn rendering_text_with_a_font_draws_pixels() {
        let list = DisplayList {
            items: vec![text_item("A", 9.0)],
        };
        let mut canvas = Canvas::filled(10, 10, Color::WHITE);
        assert_eq!(
            canvas.render(&list, &BuiltinFont, &NoImages),
            0,
            "nothing missing"
        );
        // `A` at 9px is its design size: the apex row is `.###.`.
        assert_eq!(at(&canvas, 0, 0), Color::WHITE);
        assert_eq!(at(&canvas, 1, 0), Color::BLACK);
        assert_eq!(at(&canvas, 3, 0), Color::BLACK);
        assert_eq!(at(&canvas, 4, 0), Color::WHITE);
    }

    #[test]
    fn rendering_text_without_a_font_reports_every_character() {
        let list = DisplayList {
            items: vec![text_item("hi", 10.0)],
        };
        let mut canvas = Canvas::new(40, 10);
        assert_eq!(
            canvas.render(&list, &NoGlyphs, &NoImages),
            2,
            "unpainted characters must be reported, not silently dropped"
        );
        assert!(canvas.pixels().iter().all(|&byte| byte == 0));
    }

    #[test]
    fn text_sits_on_its_baseline() {
        // The glyph's bottom row lands just above the baseline.
        let mut canvas = Canvas::filled(10, 20, Color::WHITE);
        canvas.draw_text("L", 0.0, 10.0, 9.0, Color::BLACK, &BuiltinFont, 0.0, 0.0);
        // Ascent 7 at scale 1: the cell spans rows 3..=9.
        assert_eq!(at(&canvas, 0, 3), Color::BLACK, "the stem starts here");
        assert_eq!(at(&canvas, 0, 9), Color::BLACK, "the foot is the last row");
        assert_eq!(at(&canvas, 0, 10), Color::WHITE, "nothing on the baseline");
        assert_eq!(at(&canvas, 0, 2), Color::WHITE, "nothing above the cell");
    }

    #[test]
    fn a_larger_font_scales_each_design_pixel_into_a_block() {
        let mut canvas = Canvas::filled(20, 20, Color::WHITE);
        // 18px is twice the 9-unit em, so one design pixel is 2x2.
        canvas.draw_text("L", 0.0, 16.0, 18.0, Color::BLACK, &BuiltinFont, 0.0, 0.0);
        assert_eq!(at(&canvas, 0, 2), Color::BLACK);
        assert_eq!(at(&canvas, 1, 3), Color::BLACK, "the same design pixel");
        assert_eq!(at(&canvas, 2, 2), Color::WHITE, "past the stem");
    }

    #[test]
    fn characters_advance_across_the_run() {
        let mut canvas = Canvas::filled(30, 12, Color::WHITE);
        canvas.draw_text("LL", 0.0, 8.0, 9.0, Color::BLACK, &BuiltinFont, 0.0, 0.0);
        // Advance is 6 design units, so the second stem starts at x=6.
        assert_eq!(at(&canvas, 0, 2), Color::BLACK);
        assert_eq!(at(&canvas, 5, 2), Color::WHITE, "the inter-glyph gap");
        assert_eq!(at(&canvas, 6, 2), Color::BLACK);
    }

    #[test]
    fn a_missing_glyph_still_advances_the_pen() {
        // Otherwise the rest of the run would shift left and no longer
        // match the width layout measured.
        let mut canvas = Canvas::filled(30, 12, Color::WHITE);
        let missing = canvas.draw_text("éL", 0.0, 8.0, 9.0, Color::BLACK, &BuiltinFont, 0.0, 0.0);
        assert_eq!(missing, 1);
        assert_eq!(at(&canvas, 6, 2), Color::BLACK, "the L kept its position");
    }

    #[test]
    fn a_space_draws_nothing_but_takes_its_width() {
        let mut canvas = Canvas::filled(30, 12, Color::WHITE);
        canvas.draw_text(" L", 0.0, 8.0, 9.0, Color::BLACK, &BuiltinFont, 0.0, 0.0);
        assert_eq!(at(&canvas, 0, 2), Color::WHITE);
        assert_eq!(at(&canvas, 6, 2), Color::BLACK);
    }

    #[test]
    fn text_takes_the_colour_it_is_given() {
        let mut canvas = Canvas::filled(10, 12, Color::WHITE);
        canvas.draw_text("L", 0.0, 8.0, 9.0, RED, &BuiltinFont, 0.0, 0.0);
        assert_eq!(at(&canvas, 0, 2), RED);
    }

    #[test]
    fn transparent_or_zero_sized_text_draws_nothing() {
        let mut canvas = Canvas::filled(10, 12, Color::WHITE);
        canvas.draw_text(
            "L",
            0.0,
            8.0,
            9.0,
            Color::TRANSPARENT,
            &BuiltinFont,
            0.0,
            0.0,
        );
        canvas.draw_text("L", 0.0, 8.0, 0.0, Color::BLACK, &BuiltinFont, 0.0, 0.0);
        assert_eq!(at(&canvas, 0, 2), Color::WHITE);
    }

    #[test]
    fn text_is_clipped_to_the_canvas() {
        let mut canvas = Canvas::filled(3, 12, Color::WHITE);
        // The run is far wider than the canvas; the visible part draws.
        canvas.draw_text(
            "LLLLLL",
            0.0,
            8.0,
            9.0,
            Color::BLACK,
            &BuiltinFont,
            0.0,
            0.0,
        );
        assert_eq!(at(&canvas, 0, 2), Color::BLACK);
        assert!(canvas.pixel(3, 2).is_none());
    }

    #[test]
    fn a_zero_sized_canvas_is_harmless() {
        let mut canvas = Canvas::new(0, 0);
        canvas.fill_rect(Rect::new(0.0, 0.0, 10.0, 10.0), RED);
        assert!(canvas.pixels().is_empty());
        assert!(canvas.pixel(0, 0).is_none());
    }

    #[test]
    fn a_weight_the_face_already_has_is_not_emboldened() {
        assert_eq!(embolden_by(400, 400, 16.0), 0.0);
        assert_eq!(embolden_by(700, 700, 16.0), 0.0);
        assert_eq!(embolden_by(400, 700, 16.0), 0.0, "never thinned");
    }

    #[test]
    fn a_face_too_light_for_the_run_is_thickened_with_the_size() {
        let small = embolden_by(700, 400, 16.0);
        let large = embolden_by(700, 400, 64.0);
        assert!(small > 0.0, "bold on a regular face: {small}");
        assert!(large > small, "a larger size needs more: {large}");
    }

    #[test]
    fn thickening_only_ever_adds_coverage() {
        let mut mask = crate::engine::cryo_impl::scan::Mask::new(6, 1);
        mask.set(2, 0, 255);
        thicken(&mut mask, 1.0);
        assert_eq!(mask.at(2, 0), 255, "the original stays");
        assert_eq!(mask.at(3, 0), 255, "and spreads one pixel right");
        assert_eq!(mask.at(1, 0), 0, "but not backwards");
    }
}
