//! Glyph outlines: reading `loca` and `glyf`, and flattening the curves
//! into polygons.
//!
//! TrueType outlines are **quadratic** B-splines stored with a
//! compression trick that catches everyone once: between two consecutive
//! off-curve points there is an *implied* on-curve point at their
//! midpoint. A font omits it to save two bytes per curve, and a reader
//! that does not insert it back produces glyphs with corners where there
//! should be curves.
//!
//! Coordinates are delta-encoded, each axis separately, with per-point
//! flags saying whether a delta is one byte or two and whether it
//! repeats the previous one. The flags array is itself run-length
//! encoded. None of it is bounds-checked by the format, so every read
//! here is fallible.
//!
//! A glyph may also be **composite** — built from other glyphs with a
//! transform. `é` is usually `e` plus `´` rather than its own outline.
//! Components can nest, so the recursion is depth-bounded: a font with
//! a cycle must fail rather than hang.
//!
//! Everything here is in font design units. Scaling to pixels happens at
//! [`Outline::flatten`], because how finely a curve needs subdividing
//! depends on how big it will be drawn.

use super::sfnt::{i16_at, u16_at, u32_at, u8_at, FontFile};
use super::FontError;

/// How deep composite glyphs may nest before the font is rejected.
const MAX_COMPONENT_DEPTH: u8 = 8;

/// Flags in a simple glyph's point data.
const ON_CURVE: u8 = 0x01;
const X_SHORT: u8 = 0x02;
const Y_SHORT: u8 = 0x04;
const REPEAT: u8 = 0x08;
/// With `X_SHORT`, the delta is positive; without it, the delta is zero.
const X_SAME_OR_POSITIVE: u8 = 0x10;
const Y_SAME_OR_POSITIVE: u8 = 0x20;

/// Flags in a composite glyph's component records.
const ARGS_ARE_WORDS: u16 = 0x0001;
const ARGS_ARE_XY: u16 = 0x0002;
const HAS_SCALE: u16 = 0x0008;
const MORE_COMPONENTS: u16 = 0x0020;
const HAS_XY_SCALE: u16 = 0x0040;
const HAS_TWO_BY_TWO: u16 = 0x0080;

/// One point of a contour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
    /// Off-curve points are quadratic control points.
    pub on_curve: bool,
}

/// A glyph's outline, in font design units.
///
/// Y points **up**, as fonts measure it — the flip to screen
/// coordinates happens when the glyph is placed, not here, so that a
/// font's own numbers stay recognisable while reading this.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub contours: Vec<Vec<CurvePoint>>,
}

impl Outline {
    /// A glyph with no marks — a space, or a composite with no parts.
    pub fn is_empty(&self) -> bool {
        self.contours.iter().all(|contour| contour.is_empty())
    }

    /// `(min_x, min_y, max_x, max_y)`, or `None` for an empty outline.
    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut points = self.contours.iter().flatten();
        let first = points.next()?;
        let mut bounds = (first.x, first.y, first.x, first.y);
        for point in points {
            bounds.0 = bounds.0.min(point.x);
            bounds.1 = bounds.1.min(point.y);
            bounds.2 = bounds.2.max(point.x);
            bounds.3 = bounds.3.max(point.y);
        }
        Some(bounds)
    }

    /// Flatten to closed polygons, scaling design units by `scale`.
    ///
    /// Subdivision is chosen per curve from its size on screen: a curve
    /// two pixels across needs a couple of segments, one sixty pixels
    /// across needs many. A fixed count would either shimmer at small
    /// sizes or waste work at large ones.
    pub fn flatten(&self, scale: f32) -> Vec<Vec<(f32, f32)>> {
        self.contours
            .iter()
            .filter(|contour| contour.len() >= 2)
            .map(|contour| flatten_contour(contour, scale))
            .filter(|polygon| polygon.len() >= 3)
            .collect()
    }
}

/// Read one glyph's outline. An empty outline is a valid answer — it is
/// what a space is.
pub fn outline(font: &FontFile, glyph: u16) -> Result<Outline, FontError> {
    read_glyph(font, glyph, 0)
}

fn read_glyph(font: &FontFile, glyph: u16, depth: u8) -> Result<Outline, FontError> {
    if depth > MAX_COMPONENT_DEPTH {
        return Err(FontError::Malformed("glyf"));
    }
    let Some(range) = glyph_range(font, glyph)? else {
        // `loca` gives this glyph zero bytes: no outline, like a space.
        return Ok(Outline::default());
    };
    let data = font.data();
    let malformed = || FontError::Malformed("glyf");

    let contour_count = i16_at(data, range.0).ok_or_else(malformed)?;
    if contour_count >= 0 {
        read_simple(data, range.0, contour_count as usize)
    } else {
        read_composite(font, range, depth)
    }
}

/// Where a glyph's data sits in `glyf`, or `None` when it is empty.
fn glyph_range(font: &FontFile, glyph: u16) -> Result<Option<(usize, usize)>, FontError> {
    let loca = font
        .table(b"loca")
        .ok_or_else(|| FontError::MissingTable("loca".into()))?;
    let glyf = font
        .table(b"glyf")
        .ok_or_else(|| FontError::MissingTable("glyf".into()))?;
    let data = font.data();
    let index = glyph as usize;

    let (start, end) = if font.has_long_loca() {
        (
            u32_at(data, loca.offset + index * 4).ok_or(FontError::Malformed("loca"))? as usize,
            u32_at(data, loca.offset + index * 4 + 4).ok_or(FontError::Malformed("loca"))? as usize,
        )
    } else {
        // The short format stores halved offsets, so every glyph is
        // two-byte aligned.
        (
            u16_at(data, loca.offset + index * 2).ok_or(FontError::Malformed("loca"))? as usize * 2,
            u16_at(data, loca.offset + index * 2 + 2).ok_or(FontError::Malformed("loca"))? as usize
                * 2,
        )
    };

    if end <= start {
        return Ok(None);
    }
    let (from, to) = (glyf.offset + start, glyf.offset + end);
    if to > glyf.offset + glyf.length || to > data.len() {
        return Err(FontError::Malformed("glyf"));
    }
    Ok(Some((from, to)))
}

/// A glyph with its own outline.
fn read_simple(data: &[u8], at: usize, contour_count: usize) -> Result<Outline, FontError> {
    let malformed = || FontError::Malformed("glyf");
    if contour_count == 0 {
        return Ok(Outline::default());
    }

    // Header is 10 bytes, then one end-point index per contour.
    let ends_at = at + 10;
    let mut ends = Vec::with_capacity(contour_count);
    for index in 0..contour_count {
        ends.push(u16_at(data, ends_at + index * 2).ok_or_else(malformed)? as usize);
    }
    let point_count = ends.last().ok_or_else(malformed)? + 1;

    // Hinting instructions sit between the end points and the flags.
    let instructions_at = ends_at + contour_count * 2;
    let instruction_length = u16_at(data, instructions_at).ok_or_else(malformed)? as usize;
    let mut cursor = instructions_at + 2 + instruction_length;

    let flags = read_flags(data, &mut cursor, point_count)?;
    let xs = read_coordinates(data, &mut cursor, &flags, X_SHORT, X_SAME_OR_POSITIVE)?;
    let ys = read_coordinates(data, &mut cursor, &flags, Y_SHORT, Y_SAME_OR_POSITIVE)?;

    let mut contours = Vec::with_capacity(contour_count);
    let mut start = 0;
    for end in ends {
        if end >= point_count {
            return Err(malformed());
        }
        let points = (start..=end)
            .map(|index| CurvePoint {
                x: xs[index],
                y: ys[index],
                on_curve: flags[index] & ON_CURVE != 0,
            })
            .collect();
        contours.push(points);
        start = end + 1;
    }
    Ok(Outline { contours })
}

/// The per-point flags, expanding the run-length encoding.
fn read_flags(data: &[u8], cursor: &mut usize, count: usize) -> Result<Vec<u8>, FontError> {
    let malformed = || FontError::Malformed("glyf");
    let mut flags = Vec::with_capacity(count);
    while flags.len() < count {
        let flag = u8_at(data, *cursor).ok_or_else(malformed)?;
        *cursor += 1;
        flags.push(flag);
        if flag & REPEAT != 0 {
            let repeats = u8_at(data, *cursor).ok_or_else(malformed)?;
            *cursor += 1;
            for _ in 0..repeats {
                if flags.len() == count {
                    break;
                }
                flags.push(flag);
            }
        }
    }
    Ok(flags)
}

/// One axis of delta-encoded coordinates, accumulated into absolutes.
fn read_coordinates(
    data: &[u8],
    cursor: &mut usize,
    flags: &[u8],
    short_flag: u8,
    same_flag: u8,
) -> Result<Vec<f32>, FontError> {
    let malformed = || FontError::Malformed("glyf");
    let mut values = Vec::with_capacity(flags.len());
    let mut total = 0i32;
    for flag in flags {
        if flag & short_flag != 0 {
            // One unsigned byte; the "same" bit carries the sign.
            let magnitude = i32::from(u8_at(data, *cursor).ok_or_else(malformed)?);
            *cursor += 1;
            total += if flag & same_flag != 0 {
                magnitude
            } else {
                -magnitude
            };
        } else if flag & same_flag == 0 {
            // Two signed bytes.
            total += i32::from(i16_at(data, *cursor).ok_or_else(malformed)?);
            *cursor += 2;
        }
        // Otherwise the delta is zero and nothing is stored at all.
        values.push(total as f32);
    }
    Ok(values)
}

/// A glyph assembled from other glyphs.
fn read_composite(font: &FontFile, range: (usize, usize), depth: u8) -> Result<Outline, FontError> {
    let data = font.data();
    let malformed = || FontError::Malformed("glyf");
    let mut cursor = range.0 + 10;
    let mut contours = Vec::new();

    loop {
        let flags = u16_at(data, cursor).ok_or_else(malformed)?;
        let component = u16_at(data, cursor + 2).ok_or_else(malformed)?;
        cursor += 4;

        let (dx, dy) = if flags & ARGS_ARE_WORDS != 0 {
            let first = i16_at(data, cursor).ok_or_else(malformed)?;
            let second = i16_at(data, cursor + 2).ok_or_else(malformed)?;
            cursor += 4;
            (f32::from(first), f32::from(second))
        } else {
            let first = u8_at(data, cursor).ok_or_else(malformed)? as i8;
            let second = u8_at(data, cursor + 1).ok_or_else(malformed)? as i8;
            cursor += 2;
            (f32::from(first), f32::from(second))
        };
        // Point-matching placement is vanishingly rare and needs both
        // glyphs already positioned; treat it as no offset rather than
        // guessing.
        let (dx, dy) = if flags & ARGS_ARE_XY != 0 {
            (dx, dy)
        } else {
            (0.0, 0.0)
        };

        let transform = read_transform(data, &mut cursor, flags)?;
        let part = read_glyph(font, component, depth + 1)?;
        for contour in part.contours {
            contours.push(
                contour
                    .into_iter()
                    .map(|point| CurvePoint {
                        x: transform[0] * point.x + transform[2] * point.y + dx,
                        y: transform[1] * point.x + transform[3] * point.y + dy,
                        on_curve: point.on_curve,
                    })
                    .collect(),
            );
        }

        if flags & MORE_COMPONENTS == 0 {
            break;
        }
    }
    Ok(Outline { contours })
}

/// A component's 2x2 transform as `[a, b, c, d]`, defaulting to identity.
fn read_transform(data: &[u8], cursor: &mut usize, flags: u16) -> Result<[f32; 4], FontError> {
    let malformed = || FontError::Malformed("glyf");
    // Scales are F2Dot14: a signed 16-bit value with 14 fraction bits.
    let read = |cursor: &mut usize| -> Result<f32, FontError> {
        let raw = i16_at(data, *cursor).ok_or_else(malformed)?;
        *cursor += 2;
        Ok(f32::from(raw) / 16384.0)
    };

    if flags & HAS_SCALE != 0 {
        let scale = read(cursor)?;
        Ok([scale, 0.0, 0.0, scale])
    } else if flags & HAS_XY_SCALE != 0 {
        let x = read(cursor)?;
        let y = read(cursor)?;
        Ok([x, 0.0, 0.0, y])
    } else if flags & HAS_TWO_BY_TWO != 0 {
        Ok([read(cursor)?, read(cursor)?, read(cursor)?, read(cursor)?])
    } else {
        Ok([1.0, 0.0, 0.0, 1.0])
    }
}

/// Turn one contour into a closed polygon.
fn flatten_contour(contour: &[CurvePoint], scale: f32) -> Vec<(f32, f32)> {
    let resolved = insert_implied_points(contour);
    let Some(start) = resolved.iter().position(|point| point.on_curve) else {
        // Every point off-curve cannot happen once midpoints are
        // inserted, but an empty answer is better than a wrong one.
        return Vec::new();
    };

    let count = resolved.len();
    let at = |index: usize| resolved[(start + index) % count];
    let mut polygon = vec![scaled(at(0), scale)];
    let mut index = 1;
    while index <= count {
        let point = at(index);
        if point.on_curve {
            polygon.push(scaled(point, scale));
            index += 1;
            continue;
        }
        // An off-curve point is a control; the next point ends the curve.
        let from = *polygon.last().expect("the polygon starts with a point");
        let control = scaled(point, scale);
        let to = scaled(at(index + 1), scale);
        push_quadratic(&mut polygon, from, control, to);
        index += 2;
    }
    polygon
}

/// Insert the on-curve midpoints the format leaves implied.
fn insert_implied_points(contour: &[CurvePoint]) -> Vec<CurvePoint> {
    let mut out = Vec::with_capacity(contour.len() * 2);
    for (index, point) in contour.iter().enumerate() {
        let next = contour[(index + 1) % contour.len()];
        out.push(*point);
        if !point.on_curve && !next.on_curve {
            out.push(CurvePoint {
                x: (point.x + next.x) / 2.0,
                y: (point.y + next.y) / 2.0,
                on_curve: true,
            });
        }
    }
    out
}

fn scaled(point: CurvePoint, scale: f32) -> (f32, f32) {
    (point.x * scale, point.y * scale)
}

/// Append a quadratic Bézier as line segments.
fn push_quadratic(
    polygon: &mut Vec<(f32, f32)>,
    from: (f32, f32),
    control: (f32, f32),
    to: (f32, f32),
) {
    let steps = subdivisions(from, control, to);
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let inverse = 1.0 - t;
        polygon.push((
            inverse * inverse * from.0 + 2.0 * inverse * t * control.0 + t * t * to.0,
            inverse * inverse * from.1 + 2.0 * inverse * t * control.1 + t * t * to.1,
        ));
    }
}

/// How many segments a curve of this on-screen size needs.
fn subdivisions(from: (f32, f32), control: (f32, f32), to: (f32, f32)) -> usize {
    let length = distance(from, control) + distance(control, to);
    // Roughly one segment per three pixels of control polygon, which
    // keeps the error under half a pixel at the sizes text is read at.
    ((length / 3.0).ceil() as usize).clamp(1, 32)
}

fn distance(from: (f32, f32), to: (f32, f32)) -> f32 {
    ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(x: f32, y: f32) -> CurvePoint {
        CurvePoint {
            x,
            y,
            on_curve: true,
        }
    }

    fn off(x: f32, y: f32) -> CurvePoint {
        CurvePoint {
            x,
            y,
            on_curve: false,
        }
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    #[test]
    fn an_empty_outline_reports_itself_as_empty() {
        assert!(Outline::default().is_empty());
        assert_eq!(Outline::default().bounds(), None);
        assert!(Outline::default().flatten(1.0).is_empty());
    }

    #[test]
    fn bounds_cover_every_point_including_controls() {
        let outline = Outline {
            contours: vec![vec![on(0.0, 0.0), off(50.0, 100.0), on(10.0, -5.0)]],
        };
        assert_eq!(outline.bounds(), Some((0.0, -5.0, 50.0, 100.0)));
    }

    #[test]
    fn a_square_flattens_to_its_own_corners() {
        // All points on-curve: no subdivision should happen at all.
        let outline = Outline {
            contours: vec![vec![
                on(0.0, 0.0),
                on(10.0, 0.0),
                on(10.0, 10.0),
                on(0.0, 10.0),
            ]],
        };
        let polygon = &outline.flatten(1.0)[0];
        assert_eq!(polygon.len(), 5, "four corners, closed back to the first");
        assert_eq!(polygon[0], (0.0, 0.0));
        assert_eq!(polygon[2], (10.0, 10.0));
        assert_eq!(*polygon.last().expect("closed"), (0.0, 0.0));
    }

    #[test]
    fn flattening_applies_the_scale() {
        let outline = Outline {
            contours: vec![vec![on(0.0, 0.0), on(10.0, 0.0), on(10.0, 10.0)]],
        };
        let polygon = &outline.flatten(2.0)[0];
        assert_eq!(polygon[1], (20.0, 0.0));
    }

    #[test]
    fn a_quadratic_is_subdivided_into_segments() {
        let outline = Outline {
            contours: vec![vec![on(0.0, 0.0), off(50.0, 100.0), on(100.0, 0.0)]],
        };
        let polygon = &outline.flatten(1.0)[0];
        assert!(polygon.len() > 4, "the curve was subdivided");
        // The curve's midpoint is the Bézier's, not the control point's.
        let middle = polygon[polygon.len() / 4];
        assert!(middle.1 < 100.0, "the curve stays below its control");
    }

    #[test]
    fn a_bigger_curve_gets_more_segments() {
        let small = Outline {
            contours: vec![vec![on(0.0, 0.0), off(2.0, 2.0), on(4.0, 0.0)]],
        };
        let large = Outline {
            contours: vec![vec![on(0.0, 0.0), off(100.0, 100.0), on(200.0, 0.0)]],
        };
        assert!(large.flatten(1.0)[0].len() > small.flatten(1.0)[0].len());
    }

    #[test]
    fn two_consecutive_controls_imply_a_midpoint() {
        // The compression trick: without the implied point this curve
        // would come out as a corner.
        let contour = vec![
            on(0.0, 0.0),
            off(10.0, 10.0),
            off(20.0, 10.0),
            on(30.0, 0.0),
        ];
        let resolved = insert_implied_points(&contour);
        assert_eq!(resolved.len(), 5);
        let implied = resolved[2];
        assert!(implied.on_curve);
        assert!(close(implied.x, 15.0) && close(implied.y, 10.0));
    }

    #[test]
    fn a_contour_starting_off_curve_is_rotated_to_start_on_one() {
        let outline = Outline {
            contours: vec![vec![off(5.0, 10.0), on(10.0, 0.0), on(0.0, 0.0)]],
        };
        let polygon = &outline.flatten(1.0)[0];
        assert!(
            polygon[0] == (10.0, 0.0) || polygon[0] == (0.0, 0.0),
            "starts on a real point, got {:?}",
            polygon[0]
        );
    }

    #[test]
    fn a_degenerate_contour_is_dropped() {
        let outline = Outline {
            contours: vec![vec![], vec![on(0.0, 0.0)], vec![on(0.0, 0.0), on(1.0, 1.0)]],
        };
        // None of these enclose any area.
        assert!(outline.flatten(1.0).iter().all(|p| p.len() >= 3));
    }

    #[test]
    fn subdivision_is_bounded_at_both_ends() {
        assert_eq!(subdivisions((0.0, 0.0), (0.0, 0.0), (0.0, 0.0)), 1);
        let huge = subdivisions((0.0, 0.0), (10_000.0, 0.0), (20_000.0, 0.0));
        assert_eq!(huge, 32, "clamped, so a vast curve cannot hang the raster");
    }

    #[test]
    fn an_f2dot14_scale_reads_as_a_fraction() {
        // 0x4000 is 1.0, 0x2000 is 0.5, 0xC000 is -1.0.
        let data = [0x40, 0x00, 0x20, 0x00, 0xc0, 0x00];
        let mut cursor = 0;
        let transform = read_transform(&data, &mut cursor, HAS_SCALE).expect("reads");
        assert!(close(transform[0], 1.0) && close(transform[3], 1.0));

        let mut cursor = 2;
        let half = read_transform(&data, &mut cursor, HAS_SCALE).expect("reads");
        assert!(close(half[0], 0.5));

        let mut cursor = 4;
        let negative = read_transform(&data, &mut cursor, HAS_SCALE).expect("reads");
        assert!(close(negative[0], -1.0));
    }

    #[test]
    fn no_transform_flags_give_the_identity() {
        let mut cursor = 0;
        let transform = read_transform(&[], &mut cursor, 0).expect("reads");
        assert_eq!(transform, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(cursor, 0, "nothing was consumed");
    }

    #[test]
    fn repeated_flags_expand_to_one_per_point() {
        // One flag with REPEAT set, saying "and three more like me".
        let data = [ON_CURVE | REPEAT, 3, X_SHORT];
        let mut cursor = 0;
        let flags = read_flags(&data, &mut cursor, 5).expect("reads");
        assert_eq!(flags.len(), 5);
        assert!(flags[..4].iter().all(|flag| flag & ON_CURVE != 0));
        assert_eq!(flags[4], X_SHORT);
    }

    #[test]
    fn short_coordinates_take_their_sign_from_the_same_flag() {
        let flags = [X_SHORT | X_SAME_OR_POSITIVE, X_SHORT];
        let data = [10, 4];
        let mut cursor = 0;
        let values =
            read_coordinates(&data, &mut cursor, &flags, X_SHORT, X_SAME_OR_POSITIVE).expect("ok");
        // Deltas accumulate: +10 then -4.
        assert_eq!(values, vec![10.0, 6.0]);
    }

    #[test]
    fn a_repeated_coordinate_stores_no_bytes_at_all() {
        // `SAME` without `SHORT` means a delta of zero.
        let flags = [X_SHORT | X_SAME_OR_POSITIVE, X_SAME_OR_POSITIVE];
        let data = [7];
        let mut cursor = 0;
        let values =
            read_coordinates(&data, &mut cursor, &flags, X_SHORT, X_SAME_OR_POSITIVE).expect("ok");
        assert_eq!(values, vec![7.0, 7.0]);
        assert_eq!(cursor, 1, "only the first delta was stored");
    }

    #[test]
    fn long_coordinates_are_signed_sixteen_bit() {
        let flags = [0, 0];
        let mut data = Vec::new();
        data.extend_from_slice(&300i16.to_be_bytes());
        data.extend_from_slice(&(-500i16).to_be_bytes());
        let mut cursor = 0;
        let values =
            read_coordinates(&data, &mut cursor, &flags, X_SHORT, X_SAME_OR_POSITIVE).expect("ok");
        assert_eq!(values, vec![300.0, -200.0]);
    }

    #[test]
    fn truncated_point_data_errors_rather_than_panicking() {
        let flags = [0, 0];
        let data = [0x01];
        let mut cursor = 0;
        assert!(read_coordinates(&data, &mut cursor, &flags, X_SHORT, X_SAME_OR_POSITIVE).is_err());
    }
}
