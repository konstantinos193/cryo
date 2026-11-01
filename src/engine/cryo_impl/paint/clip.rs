//! A clip shape, resolved to pixels, and how much of a pixel it keeps.
//!
//! [`crate::engine::cryo_impl::css::value::ClipPath`] says what shape
//! the author asked for, in lengths and percentages. This is the same
//! shape once the box's size is known: absolute, in page coordinates,
//! and able to answer the only question paint has — *how much of this
//! pixel survives?*
//!
//! The answer is a fraction, not a yes or no. A hard test gives a
//! curved edge made of steps; the one-pixel falloff here is the same
//! trick the glyph rasteriser uses, and it is what makes a clipped
//! panel's edge look drawn rather than cut with scissors.

use crate::engine::cryo_impl::css::value::{ClipPath, Gradient, LengthPercentage};
use crate::engine::cryo_impl::layout::geometry::Rect;

/// A clip shape in page coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipShape {
    /// Centre and radii, in pixels. A circle is an ellipse whose
    /// radii agree, so both spell out the same way here.
    Ellipse {
        centre_x: f32,
        centre_y: f32,
        radius_x: f32,
        radius_y: f32,
    },
    Rectangle(Rect),
    /// A `mask-image` gradient: the mask's **alpha** at a pixel is how
    /// much of that pixel survives, and everything outside `rect` is
    /// gone entirely — a mask has a painting area, and the initial one
    /// is the border box.
    ///
    /// This is how a decorative glow is made to show only at the ends
    /// of a bar. Ignore the mask and the glow blooms evenly in every
    /// direction, which reads as a halo the designer never drew.
    Mask {
        gradient: Gradient,
        rect: Rect,
    },
}

impl ClipShape {
    /// Resolve a declared shape against the box it clips.
    ///
    /// Percentages are of the box: a radius against its width or
    /// height, a centre against the same. A circle's single radius
    /// resolves against the diagonal, which is what the spec says and
    /// is why a `circle(50%)` on a wide box is not half its width.
    pub fn resolve(path: ClipPath, box_rect: Rect) -> Self {
        let against = |value: LengthPercentage, basis: f32| match value {
            LengthPercentage::Length(length) => length.value,
            LengthPercentage::Percentage(percent) => percent / 100.0 * basis,
        };
        match path {
            ClipPath::Ellipse {
                radius_x,
                radius_y,
                centre_x,
                centre_y,
            } => Self::Ellipse {
                centre_x: box_rect.x + against(centre_x, box_rect.width),
                centre_y: box_rect.y + against(centre_y, box_rect.height),
                radius_x: against(radius_x, box_rect.width).abs(),
                radius_y: against(radius_y, box_rect.height).abs(),
            },
            ClipPath::Circle {
                radius,
                centre_x,
                centre_y,
            } => {
                // The spec's reference for a circle's percentage: the
                // diagonal, scaled so a square box behaves as expected.
                let diagonal = ((box_rect.width.powi(2) + box_rect.height.powi(2)) / 2.0).sqrt();
                let size = against(radius, diagonal).abs();
                Self::Ellipse {
                    centre_x: box_rect.x + against(centre_x, box_rect.width),
                    centre_y: box_rect.y + against(centre_y, box_rect.height),
                    radius_x: size,
                    radius_y: size,
                }
            }
            ClipPath::Inset {
                top,
                right,
                bottom,
                left,
            } => {
                let (top, bottom) = (
                    against(top, box_rect.height),
                    against(bottom, box_rect.height),
                );
                let (left, right) = (
                    against(left, box_rect.width),
                    against(right, box_rect.width),
                );
                Self::Rectangle(Rect::new(
                    box_rect.x + left,
                    box_rect.y + top,
                    (box_rect.width - left - right).max(0.0),
                    (box_rect.height - top - bottom).max(0.0),
                ))
            }
        }
    }

    /// How much of the pixel at `(x, y)` this shape keeps, 0..=1.
    ///
    /// The pixel is sampled at its centre, and the edge fades over
    /// roughly one pixel — see the module note on why.
    pub fn coverage(&self, x: u32, y: u32) -> f32 {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        match *self {
            Self::Ellipse {
                centre_x,
                centre_y,
                radius_x,
                radius_y,
            } => {
                if radius_x <= 0.0 || radius_y <= 0.0 {
                    return 0.0;
                }
                let (dx, dy) = ((px - centre_x) / radius_x, (py - centre_y) / radius_y);
                let distance = (dx * dx + dy * dy).sqrt();
                // `distance` is 1 exactly on the edge. One pixel of
                // falloff, expressed in those units — so the band is
                // a pixel wide on screen however big the ellipse is.
                let softness = 1.0 / radius_x.min(radius_y).max(1.0);
                ((1.0 - distance) / softness).clamp(0.0, 1.0)
            }
            Self::Rectangle(rect) => {
                let inside = px >= rect.x
                    && py >= rect.y
                    && px < rect.x + rect.width
                    && py < rect.y + rect.height;
                f32::from(u8::from(inside))
            }
            Self::Mask { ref gradient, rect } => {
                if rect.width <= 0.0 || rect.height <= 0.0 {
                    return 0.0;
                }
                let inside = px >= rect.x
                    && py >= rect.y
                    && px < rect.x + rect.width
                    && py < rect.y + rect.height;
                if !inside {
                    return 0.0;
                }
                let along = gradient.progress(px - rect.x, py - rect.y, rect.width, rect.height);
                gradient.sample(along).alpha.clamp(0.0, 1.0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::value::Length;

    fn percent(value: f32) -> LengthPercentage {
        LengthPercentage::Percentage(value)
    }

    #[test]
    fn an_ellipse_resolves_its_percentages_against_the_box() {
        let shape = ClipShape::resolve(
            ClipPath::Ellipse {
                radius_x: percent(100.0),
                radius_y: percent(145.0),
                centre_x: percent(0.0),
                centre_y: percent(50.0),
            },
            Rect::new(0.0, 0.0, 200.0, 100.0),
        );
        assert_eq!(
            shape,
            ClipShape::Ellipse {
                centre_x: 0.0,
                centre_y: 50.0,
                radius_x: 200.0,
                radius_y: 145.0,
            }
        );
    }

    #[test]
    fn a_pixel_inside_the_ellipse_is_kept_whole() {
        let shape = ClipShape::Ellipse {
            centre_x: 50.0,
            centre_y: 50.0,
            radius_x: 40.0,
            radius_y: 40.0,
        };
        assert!(shape.coverage(50, 50) > 0.99);
    }

    #[test]
    fn a_pixel_outside_it_is_thrown_away() {
        let shape = ClipShape::Ellipse {
            centre_x: 50.0,
            centre_y: 50.0,
            radius_x: 10.0,
            radius_y: 10.0,
        };
        assert_eq!(shape.coverage(90, 90), 0.0);
    }

    #[test]
    fn the_edge_is_a_fraction_rather_than_a_step() {
        // What stops a curved clip looking like a staircase.
        let shape = ClipShape::Ellipse {
            centre_x: 0.0,
            centre_y: 0.0,
            radius_x: 100.0,
            radius_y: 100.0,
        };
        // A pixel straddling the edge keeps part of itself.
        let edge = shape.coverage(99, 0);
        assert!(edge > 0.0 && edge < 1.0, "got {edge}");
        // Well inside is whole, well outside is nothing.
        assert!(shape.coverage(10, 10) > 0.99);
        assert_eq!(shape.coverage(120, 0), 0.0);
    }

    #[test]
    fn an_ellipse_with_no_size_keeps_nothing() {
        let shape = ClipShape::Ellipse {
            centre_x: 0.0,
            centre_y: 0.0,
            radius_x: 0.0,
            radius_y: 10.0,
        };
        assert_eq!(shape.coverage(0, 0), 0.0);
    }

    #[test]
    fn an_inset_becomes_the_rectangle_it_leaves() {
        let shape = ClipShape::resolve(
            ClipPath::Inset {
                top: LengthPercentage::Length(Length::px(10.0)),
                right: LengthPercentage::Length(Length::px(20.0)),
                bottom: LengthPercentage::Length(Length::px(10.0)),
                left: LengthPercentage::Length(Length::px(20.0)),
            },
            Rect::new(0.0, 0.0, 100.0, 100.0),
        );
        assert_eq!(
            shape,
            ClipShape::Rectangle(Rect::new(20.0, 10.0, 60.0, 80.0))
        );
        assert_eq!(shape.coverage(50, 50), 1.0);
        assert_eq!(shape.coverage(5, 50), 0.0);
    }

    #[test]
    fn a_circles_percentage_is_of_the_diagonal() {
        // Not of the width: a `circle(50%)` on a wide box is bigger
        // than half its height and smaller than half its width.
        let shape = ClipShape::resolve(
            ClipPath::Circle {
                radius: percent(50.0),
                centre_x: percent(50.0),
                centre_y: percent(50.0),
            },
            Rect::new(0.0, 0.0, 100.0, 100.0),
        );
        let ClipShape::Ellipse { radius_x, .. } = shape else {
            panic!("a circle resolves to an ellipse");
        };
        assert!((radius_x - 50.0).abs() < 0.01, "got {radius_x}");
    }

    /// The mask a page writes to show a decoration only at the ends
    /// of a bar: opaque at both edges, clear across the middle.
    fn ends_only() -> ClipShape {
        let gradient = Gradient::parse(&crate::engine::cryo_impl::css::parser::parse_value(
            "linear-gradient(90deg, black 0%, transparent 25%, transparent 75%, black 100%)",
        ))
        .expect("it parses");
        ClipShape::Mask {
            gradient,
            rect: Rect::new(0.0, 0.0, 100.0, 20.0),
        }
    }

    #[test]
    fn a_gradient_mask_keeps_what_its_alpha_keeps() {
        let shape = ends_only();
        assert!(shape.coverage(0, 10) > 0.9, "opaque at the left edge");
        assert!(shape.coverage(99, 10) > 0.9, "and at the right");
        assert!(shape.coverage(50, 10) < 0.05, "clear across the middle");
    }

    #[test]
    fn a_mask_clips_everything_outside_its_painting_area() {
        // The initial `mask-clip` is the border box, so a cast
        // shadow reaching past it is masked away entirely — which is
        // the whole reason a masked glow does not bloom.
        let shape = ends_only();
        assert_eq!(shape.coverage(0, 40), 0.0, "below the box");
        assert_eq!(shape.coverage(200, 10), 0.0, "past its right edge");
    }

    #[test]
    fn an_empty_mask_area_keeps_nothing() {
        let ClipShape::Mask { gradient, .. } = ends_only() else {
            panic!("a mask");
        };
        let shape = ClipShape::Mask {
            gradient,
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
        };
        assert_eq!(shape.coverage(0, 0), 0.0);
    }
}
