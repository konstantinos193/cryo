//! Scanline rasterisation: polygons become a coverage mask.
//!
//! Text is the reason anti-aliasing matters. A box edge snapped to a
//! pixel looks fine; a letter's curve snapped to pixels looks like a
//! staircase, and at reading sizes it is the difference between legible
//! and not. So a glyph is not filled in black-or-nothing — each pixel
//! gets a **coverage** value, how much of it the outline covers, and the
//! blend uses that as alpha.
//!
//! The default fill rule is **nonzero winding**, which TrueType
//! requires and which is what makes a letter like `o` have a hole: the
//! outer contour runs one way and the inner runs the other, so their
//! windings cancel and the middle counts as outside. An even-odd rule
//! would happen to agree on `o` and disagree on glyphs with
//! overlapping contours, which fonts do use.
//!
//! SVG lets a path choose, and the choice is not cosmetic: a logo
//! drawn as one path with `fill-rule="evenodd"` relies on it to knock
//! its inner shapes out. Fill such a path with nonzero and every hole
//! closes — the mark comes out a solid silhouette.
//!
//! Coverage is measured by sampling [`SUBSAMPLES`] horizontal lines per
//! pixel row and measuring exact span overlap along each. That gives
//! true area coverage horizontally and a quantised approximation
//! vertically — the usual trade, and far cheaper than sampling a grid.

/// Horizontal sample lines per pixel row. Four is the point where more
/// stops being visible at text sizes.
pub const SUBSAMPLES: usize = 4;

/// A rectangular field of coverage values, 0 meaning uncovered and 255
/// fully covered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

impl Mask {
    /// An all-zero mask.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            coverage: vec![0; width as usize * height as usize],
        }
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub fn is_empty(&self) -> bool {
        self.coverage.iter().all(|&value| value == 0)
    }

    /// Coverage at a pixel, or 0 outside the mask.
    /// Replace one pixel's coverage.
    ///
    /// Only a post-pass over a finished mask needs this — a shadow's
    /// blur rewrites the coverage it was given. Filling never does:
    /// it accumulates spans and has no reason to revisit a pixel.
    pub fn set(&mut self, x: u32, y: u32, coverage: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        self.coverage[y as usize * self.width as usize + x as usize] = coverage;
    }

    pub fn at(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.coverage[y as usize * self.width as usize + x as usize]
    }

    fn add(&mut self, x: u32, y: u32, amount: f32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let slot = &mut self.coverage[y as usize * self.width as usize + x as usize];
        let total = f32::from(*slot) + amount * 255.0;
        *slot = total.round().clamp(0.0, 255.0) as u8;
    }
}

/// One edge of a polygon, as the scanline walk needs it.
struct Edge {
    top: f32,
    bottom: f32,
    /// x at `top`, and how much x moves per unit of y.
    x_at_top: f32,
    slope: f32,
    /// +1 when the edge runs downward, -1 upward. The sign is what
    /// makes the winding rule work.
    direction: i32,
}

/// Which crossings count as inside a shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillRule {
    /// Inside wherever the winding is not zero. Required by TrueType
    /// and the default everywhere else.
    #[default]
    NonZero,
    /// Inside after an odd number of crossings. What an SVG path asks
    /// for with `fill-rule="evenodd"`, so that overlapping subpaths
    /// cut holes instead of filling each other in.
    EvenOdd,
}

/// Fill closed polygons into a `width` by `height` coverage mask,
/// under the nonzero rule.
///
/// Coordinates are in mask pixels: the caller has already scaled and
/// translated the outline so that the glyph sits at the origin.
pub fn fill(polygons: &[Vec<(f32, f32)>], width: u32, height: u32) -> Mask {
    fill_with(polygons, width, height, FillRule::NonZero)
}

/// The same, under a stated rule.
pub fn fill_with(polygons: &[Vec<(f32, f32)>], width: u32, height: u32, rule: FillRule) -> Mask {
    let mut mask = Mask::new(width, height);
    let edges = collect_edges(polygons);
    if edges.is_empty() || width == 0 || height == 0 {
        return mask;
    }

    let weight = 1.0 / SUBSAMPLES as f32;
    let mut crossings: Vec<(f32, i32)> = Vec::new();
    for row in 0..height {
        for sample in 0..SUBSAMPLES {
            // Sample through the middle of each sub-row, so a shape
            // aligned to pixel boundaries is not caught on an edge.
            let y = row as f32 + (sample as f32 + 0.5) * weight;
            crossings.clear();
            for edge in &edges {
                if y < edge.top || y >= edge.bottom {
                    continue;
                }
                crossings.push((edge.x_at_top + (y - edge.top) * edge.slope, edge.direction));
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_by(|left, right| left.0.total_cmp(&right.0));
            fill_spans(&mut mask, &crossings, row, weight, rule);
        }
    }
    mask
}

/// Walk one scanline's crossings, filling the spans that are inside.
fn fill_spans(mask: &mut Mask, crossings: &[(f32, i32)], row: u32, weight: f32, rule: FillRule) {
    let mut winding = 0;
    for (index, pair) in crossings.windows(2).enumerate() {
        winding += pair[0].1;
        let inside = match rule {
            FillRule::NonZero => winding != 0,
            // Each crossing flips the state, so the spans after an
            // odd number of them are the inside ones. The direction
            // the edge ran is irrelevant here, which is the whole
            // difference from the rule above.
            FillRule::EvenOdd => index % 2 == 0,
        };
        if !inside {
            continue;
        }
        add_span(mask, pair[0].0, pair[1].0, row, weight);
    }
}

/// Add coverage for the span `from..to` on one sub-scanline.
///
/// Partial pixels at each end get their exact fraction, which is where
/// the horizontal half of the anti-aliasing comes from.
fn add_span(mask: &mut Mask, from: f32, to: f32, row: u32, weight: f32) {
    let left = from.max(0.0);
    let right = to.min(mask.width() as f32);
    if right <= left {
        return;
    }

    let first = left.floor() as u32;
    let last = (right.ceil() as u32).min(mask.width());
    for x in first..last {
        let pixel_left = x as f32;
        let covered = (right.min(pixel_left + 1.0) - left.max(pixel_left)).max(0.0);
        mask.add(x, row, covered * weight);
    }
}

/// Turn polygons into edges, dropping the horizontal ones.
///
/// A horizontal edge never crosses a horizontal scanline, so including
/// it would only ever produce a degenerate crossing.
fn collect_edges(polygons: &[Vec<(f32, f32)>]) -> Vec<Edge> {
    let mut edges = Vec::new();
    for polygon in polygons {
        if polygon.len() < 3 {
            continue;
        }
        for index in 0..polygon.len() {
            let from = polygon[index];
            let to = polygon[(index + 1) % polygon.len()];
            if (from.1 - to.1).abs() < f32::EPSILON {
                continue;
            }
            let (upper, lower, direction) = if from.1 < to.1 {
                (from, to, 1)
            } else {
                (to, from, -1)
            };
            edges.push(Edge {
                top: upper.1,
                bottom: lower.1,
                x_at_top: upper.0,
                slope: (lower.0 - upper.0) / (lower.1 - upper.1),
                direction,
            });
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rectangle as a closed polygon.
    fn rect(x: f32, y: f32, width: f32, height: f32) -> Vec<(f32, f32)> {
        vec![
            (x, y),
            (x + width, y),
            (x + width, y + height),
            (x, y + height),
        ]
    }

    /// The same rectangle wound the other way.
    fn reversed(x: f32, y: f32, width: f32, height: f32) -> Vec<(f32, f32)> {
        let mut points = rect(x, y, width, height);
        points.reverse();
        points
    }

    /// Render a mask as text, for assertions that read like the shape.
    fn draw(mask: &Mask) -> Vec<String> {
        (0..mask.height())
            .map(|y| {
                (0..mask.width())
                    .map(|x| match mask.at(x, y) {
                        0 => '.',
                        1..=200 => '+',
                        _ => '#',
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn an_empty_mask_is_blank() {
        let mask = Mask::new(3, 2);
        assert_eq!(mask.width(), 3);
        assert_eq!(mask.height(), 2);
        assert!(mask.is_empty());
        assert_eq!(mask.at(0, 0), 0);
    }

    #[test]
    fn reading_outside_a_mask_gives_no_coverage() {
        let mask = Mask::new(2, 2);
        assert_eq!(mask.at(5, 0), 0);
        assert_eq!(mask.at(0, 5), 0);
    }

    #[test]
    fn nothing_to_fill_leaves_the_mask_blank() {
        assert!(fill(&[], 4, 4).is_empty());
        // Fewer than three points encloses no area.
        assert!(fill(&[vec![(0.0, 0.0), (4.0, 4.0)]], 4, 4).is_empty());
    }

    #[test]
    fn a_pixel_aligned_rectangle_fills_completely() {
        let mask = fill(&[rect(1.0, 1.0, 2.0, 2.0)], 4, 4);
        assert_eq!(draw(&mask), ["....", ".##.", ".##.", "...."]);
        assert_eq!(mask.at(1, 1), 255, "fully covered");
    }

    #[test]
    fn a_rectangle_covering_everything_fills_every_pixel() {
        let mask = fill(&[rect(0.0, 0.0, 3.0, 3.0)], 3, 3);
        for y in 0..3 {
            for x in 0..3 {
                assert_eq!(mask.at(x, y), 255, "at {x},{y}");
            }
        }
    }

    #[test]
    fn a_half_covered_pixel_gets_about_half_coverage() {
        // The whole point of anti-aliasing: partial pixels are partial.
        let mask = fill(&[rect(0.0, 0.0, 0.5, 1.0)], 2, 1);
        let coverage = mask.at(0, 0);
        assert!(
            (120..=135).contains(&coverage),
            "expected about half, got {coverage}"
        );
        assert_eq!(mask.at(1, 0), 0);
    }

    #[test]
    fn a_quarter_covered_pixel_gets_about_a_quarter() {
        let mask = fill(&[rect(0.0, 0.0, 1.0, 0.25)], 1, 1);
        let coverage = mask.at(0, 0);
        assert!(
            (50..=78).contains(&coverage),
            "expected about a quarter, got {coverage}"
        );
    }

    #[test]
    fn a_shape_outside_the_mask_is_clipped_away() {
        let mask = fill(&[rect(10.0, 10.0, 5.0, 5.0)], 4, 4);
        assert!(mask.is_empty());
    }

    #[test]
    fn a_shape_straddling_the_edge_fills_only_the_visible_part() {
        let mask = fill(&[rect(-2.0, -2.0, 4.0, 4.0)], 4, 4);
        assert_eq!(mask.at(0, 0), 255);
        assert_eq!(mask.at(2, 0), 0, "past the shape's right edge");
    }

    #[test]
    fn a_counter_clockwise_contour_cuts_a_hole() {
        // The reason the rule is nonzero: this is how `o` gets a middle.
        let mask = fill(
            &[rect(0.0, 0.0, 5.0, 5.0), reversed(1.0, 1.0, 3.0, 3.0)],
            5,
            5,
        );
        assert_eq!(draw(&mask), ["#####", "#...#", "#...#", "#...#", "#####"]);
    }

    #[test]
    fn two_contours_wound_the_same_way_do_not_cancel() {
        // Where nonzero and even-odd disagree, and fonts rely on it.
        let mask = fill(&[rect(0.0, 0.0, 5.0, 5.0), rect(1.0, 1.0, 3.0, 3.0)], 5, 5);
        assert_eq!(mask.at(2, 2), 255, "the inner contour adds, not cuts");
    }

    #[test]
    fn overlapping_shapes_do_not_double_their_coverage() {
        let mask = fill(&[rect(0.0, 0.0, 3.0, 3.0), rect(1.0, 0.0, 3.0, 3.0)], 4, 3);
        assert_eq!(mask.at(1, 1), 255, "already full, and stays full");
        assert_eq!(mask.at(3, 1), 255);
    }

    #[test]
    fn a_triangle_is_denser_at_its_base() {
        let triangle = vec![(4.0, 0.0), (8.0, 8.0), (0.0, 8.0)];
        let mask = fill(&[triangle], 8, 8);
        let row_coverage = |y: u32| -> u32 { (0..8).map(|x| u32::from(mask.at(x, y))).sum() };
        assert!(row_coverage(1) < row_coverage(6), "it widens downward");
        assert!(row_coverage(0) < row_coverage(7));
    }

    #[test]
    fn horizontal_edges_do_not_produce_stray_crossings() {
        // A shape that is all horizontal and vertical edges must still
        // come out clean, with no half-lit rows.
        let mask = fill(&[rect(0.0, 0.0, 4.0, 2.0)], 4, 4);
        assert_eq!(draw(&mask), ["####", "####", "....", "...."]);
    }

    #[test]
    fn a_zero_sized_mask_is_harmless() {
        let mask = fill(&[rect(0.0, 0.0, 5.0, 5.0)], 0, 0);
        assert_eq!(mask.width(), 0);
        assert!(mask.is_empty());
    }

    #[test]
    fn a_degenerate_polygon_adds_nothing() {
        // All points on one line: no area, so no coverage.
        let line = vec![(0.0, 0.0), (4.0, 0.0), (2.0, 0.0)];
        assert!(fill(&[line], 4, 4).is_empty());
    }

    #[test]
    fn even_odd_cuts_a_hole_whichever_way_the_inner_ring_runs() {
        // The logo case. A designer draws the mark as one path and
        // sets `fill-rule="evenodd"`, so the inner shapes need not be
        // wound against the outer one to knock holes in it. Under
        // nonzero the same path is a solid silhouette.
        for inner in [rect(1.0, 1.0, 3.0, 3.0), reversed(1.0, 1.0, 3.0, 3.0)] {
            let mask = fill_with(&[rect(0.0, 0.0, 5.0, 5.0), inner], 5, 5, FillRule::EvenOdd);
            assert_eq!(draw(&mask), ["#####", "#...#", "#...#", "#...#", "#####"]);
        }
    }

    #[test]
    fn nonzero_fills_a_same_way_inner_ring_in() {
        // The other half of the same fact, and why the rule has to
        // travel with the shape rather than be assumed.
        let mask = fill_with(
            &[rect(0.0, 0.0, 5.0, 5.0), rect(1.0, 1.0, 3.0, 3.0)],
            5,
            5,
            FillRule::NonZero,
        );
        assert_eq!(draw(&mask), ["#####"; 5]);
    }

    #[test]
    fn the_default_entry_point_is_still_nonzero() {
        // Glyphs go through `fill`, and TrueType requires nonzero.
        let rings = [rect(0.0, 0.0, 5.0, 5.0), rect(1.0, 1.0, 3.0, 3.0)];
        assert_eq!(
            draw(&fill(&rings, 5, 5)),
            draw(&fill_with(&rings, 5, 5, FillRule::NonZero))
        );
    }
}
