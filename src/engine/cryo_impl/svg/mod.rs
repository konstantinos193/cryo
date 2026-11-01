//! SVG: vector graphics inside a page.
//!
//! An `<svg>` element is already in the DOM — the HTML parser puts it
//! there as foreign content — so this is not a second document
//! format to load. What it needs is a way to turn those elements into
//! something the rasteriser can fill.
//!
//! ```text
//!   <svg viewBox> ──▶ shapes ──▶ polygons ──▶ page coordinates ──▶ display items
//! ```
//!
//! The `viewBox` is the whole of the coordinate handling. It states
//! the rectangle the drawing was authored in, and the element's own
//! box on the page is where it has to end up — so every point is
//! scaled between the two. Without it an icon drawn in a 24-unit grid
//! renders 24 pixels across whatever size it was given.
//!
//! This matters more than its size suggests: icons and logos on a
//! modern page are nearly all inline SVG, and an engine that skips
//! them draws a page with every glyph of interface furniture missing.
//!
//! Known gaps, stated rather than hidden:
//! - **Strokes are filled as thin quadrilaterals**, one per segment,
//!   with no joins or caps. A stroked icon looks right; a thick
//!   stroke shows gaps at sharp corners.
//! - No gradients, patterns, clips, masks or filters: a shape with
//!   `fill="url(#…)"` is skipped rather than drawn in a wrong colour.
//! - No `transform` attribute.
//! - `preserveAspectRatio` is read for `none` against everything else:
//!   `none` stretches each axis independently, and every alignment
//!   keyword is treated as the default `xMidYMid meet`. The two
//!   genuinely differ — a decorative wave authored 600x220 and given a
//!   960x400 box is a different shape under each.

mod path;
pub mod raster;
mod shape;

use crate::engine::cryo_impl::css::parser;
use crate::engine::cryo_impl::css::value::{color, Color, SpecifiedColor};
use crate::engine::cryo_impl::dom::{Document, NodeId};
use crate::engine::cryo_impl::layout::geometry::Rect;
use crate::engine::cryo_impl::scan::FillRule;

/// One filled shape, in page coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Filled {
    pub polygons: Vec<Vec<(f32, f32)>>,
    pub color: Color,
    /// Which crossings count as inside. A logo drawn as one path with
    /// `fill-rule="evenodd"` needs this to keep its holes.
    pub rule: FillRule,
}

/// Turn an `<svg>` element into shapes placed inside `rect`.
///
/// `current` is the element's computed `color`, which `currentColor`
/// resolves to — the way an icon takes the colour of the text it sits
/// beside.
pub fn render(document: &Document, svg: NodeId, rect: Rect, current: Color) -> Vec<Filled> {
    let view = ViewBox::of(document, svg, rect);
    let mut out = Vec::new();
    let inherited = Inherited {
        current,
        fill: current,
        opacity: 1.0,
        rule: FillRule::NonZero,
    };
    collect(document, svg, &view, inherited, &mut out);
    out
}

/// What an element takes from the elements above it.
#[derive(Clone, Copy)]
struct Inherited {
    /// The computed `color`, which `currentColor` resolves to.
    current: Color,
    /// The `fill` in force, set by an ancestor `<g fill="…">`.
    fill: Color,
    /// The product of every `opacity` on the way down.
    opacity: f32,
    /// The `fill-rule` in force. Inheritable, like `fill` itself.
    rule: FillRule,
}

/// How the drawing's own coordinates map onto the page.
struct ViewBox {
    scale_x: f32,
    scale_y: f32,
    offset_x: f32,
    offset_y: f32,
}

impl ViewBox {
    /// Read the `viewBox` and work out the mapping onto `rect`.
    ///
    /// `xMidYMid meet` — the default — uses one scale for both axes,
    /// the smaller of the two so nothing is cut off, with the
    /// remainder split evenly so the drawing sits in the middle.
    /// `preserveAspectRatio="none"` instead scales each axis to fit,
    /// which is how a decorative band is made to span whatever width
    /// it is given.
    fn of(document: &Document, svg: NodeId, rect: Rect) -> Self {
        let element = document.element(svg);
        let authored = element
            .and_then(|element| element.attr("viewBox"))
            .and_then(parse_view_box)
            // A file with `width` and `height` but no `viewBox` still
            // states the extent it was drawn in, and that is what has
            // to reach `rect`. Treating it as page units instead
            // leaves the artwork at its authored size in the corner
            // of a larger buffer — a 56-unit logo rasterised for a
            // 256-pixel box comes out as a speck, or nothing at all.
            .or_else(|| {
                let side = |name: &str| {
                    element
                        .and_then(|element| element.attr(name))
                        .and_then(|text| text.trim().trim_end_matches("px").parse::<f32>().ok())
                        .filter(|value| *value > 0.0)
                };
                Some((0.0, 0.0, side("width")?, side("height")?))
            });
        // With no usable extent the drawing is already in page units.
        let untransformed = Self {
            scale_x: 1.0,
            scale_y: 1.0,
            offset_x: rect.x,
            offset_y: rect.y,
        };
        let Some((min_x, min_y, width, height)) = authored else {
            return untransformed;
        };
        if width <= 0.0 || height <= 0.0 {
            return untransformed;
        }
        let stretches = element
            .and_then(|element| element.attr("preserveAspectRatio"))
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("none"));
        let (scale_x, scale_y) = if stretches {
            (rect.width / width, rect.height / height)
        } else {
            let uniform = (rect.width / width).min(rect.height / height);
            (uniform, uniform)
        };
        Self {
            scale_x,
            scale_y,
            offset_x: rect.x + (rect.width - width * scale_x) / 2.0 - min_x * scale_x,
            offset_y: rect.y + (rect.height - height * scale_y) / 2.0 - min_y * scale_y,
        }
    }

    fn map(&self, point: (f32, f32)) -> (f32, f32) {
        (
            self.offset_x + point.0 * self.scale_x,
            self.offset_y + point.1 * self.scale_y,
        )
    }

    /// The scale a stroke width is drawn at.
    ///
    /// A stroke under a non-uniform scale is an ellipse swept along
    /// the path, which these quadrilaterals cannot be; the mean of the
    /// two axes is the nearest single number.
    fn stroke_scale(&self) -> f32 {
        (self.scale_x * self.scale_y).abs().sqrt()
    }
}

/// `min-x min-y width height`, separated by spaces or commas.
fn parse_view_box(text: &str) -> Option<(f32, f32, f32, f32)> {
    let numbers: Vec<f32> = text
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    match numbers[..] {
        [min_x, min_y, width, height] => Some((min_x, min_y, width, height)),
        _ => None,
    }
}

/// Walk the subtree, turning every shape into polygons.
///
/// `inherited` carries `fill` and `opacity` down the tree, because a
/// `<g fill="…" opacity="…">` sets both for everything inside.
fn collect(
    document: &Document,
    node: NodeId,
    view: &ViewBox,
    inherited: Inherited,
    out: &mut Vec<Filled>,
) {
    for child in document.children(node) {
        let Some(element) = document.element(child) else {
            continue;
        };
        // SVG element names are case-sensitive and kept as written,
        // so this compares the string rather than folding it.
        let name = element.name.local.as_str();

        // Three cases, and they are genuinely different: no `fill`
        // attribute inherits one, `fill="none"` refuses to paint, and
        // a paint we cannot read — a gradient reference — means the
        // shape is skipped rather than drawn in some wrong colour.
        let fill = match element.attr("fill") {
            None => Some(inherited.fill),
            Some(text) => match paint(text, inherited.current) {
                Some(color) => color,
                None => continue,
            },
        };
        // `opacity` applies to the element as a whole and multiplies
        // with whatever an ancestor already dimmed.
        let opacity = inherited.opacity * alpha(element.attr("opacity"));
        let rule = match element.attr("fill-rule").map(str::trim) {
            Some(text) if text.eq_ignore_ascii_case("evenodd") => FillRule::EvenOdd,
            Some(text) if text.eq_ignore_ascii_case("nonzero") => FillRule::NonZero,
            _ => inherited.rule,
        };

        if name == "g" || name == "svg" {
            collect(
                document,
                child,
                view,
                Inherited {
                    fill: fill.unwrap_or(inherited.fill),
                    opacity,
                    rule,
                    ..inherited
                },
                out,
            );
            continue;
        }

        let Some(polygons) = shape::outline(element, name) else {
            continue;
        };
        let mapped: Vec<Vec<(f32, f32)>> = polygons
            .iter()
            .map(|ring| ring.iter().map(|&point| view.map(point)).collect())
            .collect();

        if let Some(color) = fill {
            // `fill-opacity` is not the same thing as `opacity`: it
            // dims the fill alone and leaves the stroke untouched.
            // A decorative shape drawn at `fill-opacity="0.1"` and
            // painted solid instead is not slightly wrong — it is an
            // opaque slab over whatever it was meant to tint.
            let color = color.with_alpha_scaled(opacity * alpha(element.attr("fill-opacity")));
            if !color.is_transparent() {
                out.push(Filled {
                    polygons: mapped.clone(),
                    color,
                    rule,
                });
            }
        }

        // A stroke, as quadrilaterals along each segment — see the
        // module note on joins.
        let stroke = element
            .attr("stroke")
            .and_then(|text| paint(text, inherited.current))
            .flatten();
        if let Some(color) = stroke {
            let color = color.with_alpha_scaled(opacity * alpha(element.attr("stroke-opacity")));
            if color.is_transparent() {
                continue;
            }
            let width = element
                .attr("stroke-width")
                .and_then(|text| text.trim().parse::<f32>().ok())
                .unwrap_or(1.0)
                * view.stroke_scale();
            let ribbons = mapped
                .iter()
                .flat_map(|ring| ribbon(ring, width.max(0.5)))
                .collect::<Vec<_>>();
            if !ribbons.is_empty() {
                out.push(Filled {
                    polygons: ribbons,
                    color,
                    // Each quadrilateral is its own closed shape, and
                    // they overlap at the joins — under even-odd an
                    // overlap would punch a hole in the stroke.
                    rule: FillRule::NonZero,
                });
            }
        }
    }
}

/// An opacity attribute as a factor, defaulting to fully opaque.
///
/// A percentage is allowed as well as a number, and anything outside
/// 0..=1 is clamped — SVG says so, and a negative value would
/// otherwise flip a shape back into view.
fn alpha(attribute: Option<&str>) -> f32 {
    let Some(text) = attribute.map(str::trim) else {
        return 1.0;
    };
    let parsed = match text.strip_suffix('%') {
        Some(number) => number.trim().parse::<f32>().map(|value| value / 100.0),
        None => text.parse::<f32>(),
    };
    parsed.map(|value| value.clamp(0.0, 1.0)).unwrap_or(1.0)
}

/// A `fill` or `stroke` value.
///
/// Returns `None` for a paint this engine cannot read — `url(#grad)`
/// and the like — so the caller skips the shape instead of drawing it
/// in some wrong colour. `Some(None)` is `none`, an explicit refusal
/// to paint, which is different again.
#[allow(clippy::option_option)] // the three cases are genuinely distinct
fn paint(text: &str, current: Color) -> Option<Option<Color>> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("none") || text.eq_ignore_ascii_case("transparent") {
        return Some(None);
    }
    if text.eq_ignore_ascii_case("currentColor") {
        return Some(Some(current));
    }
    if text.starts_with("url(") {
        return None;
    }
    // An SVG paint is a CSS colour written where an attribute goes,
    // so it goes through the same parser.
    let values = parser::parse_value(text);
    match color::parse(&values)? {
        SpecifiedColor::Absolute(value) => Some(Some(value)),
        SpecifiedColor::Current => Some(Some(current)),
    }
}

/// One stroked polyline as a run of quadrilaterals.
fn ribbon(points: &[(f32, f32)], width: f32) -> Vec<Vec<(f32, f32)>> {
    let half = width / 2.0;
    let mut out = Vec::new();
    for pair in points.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = (dx * dx + dy * dy).sqrt();
        if length <= 0.0 {
            continue;
        }
        // The segment's normal, scaled to half the stroke width.
        let (nx, ny) = (-dy / length * half, dx / length * half);
        out.push(vec![
            (from.0 + nx, from.1 + ny),
            (to.0 + nx, to.1 + ny),
            (to.0 - nx, to.1 - ny),
            (from.0 - nx, from.1 - ny),
        ]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::html::parse as parse_html;

    /// Render the first `<svg>` in `markup` into a 100x100 box.
    fn shapes(markup: &str) -> Vec<Filled> {
        let document = parse_html(markup);
        let svg = document
            .descendants(document.root())
            .find(|&id| document.is_html_element(id, "svg"))
            .expect("an svg element");
        render(
            &document,
            svg,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Color::rgb(1, 2, 3),
        )
    }

    #[test]
    fn a_path_becomes_a_filled_polygon() {
        let out =
            shapes(r#"<svg viewBox="0 0 10 10"><path d="M0,0 L10,0 L5,10 Z" fill="red"/></svg>"#);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].color, Color::rgb(255, 0, 0));
        // The 10-unit box scales to the 100-pixel one.
        assert_eq!(out[0].polygons[0][1], (100.0, 0.0));
    }

    #[test]
    fn the_view_box_scales_the_drawing_onto_its_element() {
        // A 24-unit icon in a 100-pixel box is drawn at 100 pixels,
        // not 24 — the whole reason viewBox exists.
        let out =
            shapes(r#"<svg viewBox="0 0 24 24"><rect width="24" height="24" fill="red"/></svg>"#);
        let corners = &out[0].polygons[0];
        assert!(corners.contains(&(100.0, 100.0)), "{corners:?}");
    }

    #[test]
    fn a_non_square_view_box_is_centred_rather_than_stretched() {
        // 20 wide by 10 tall in a 100x100 box: scaled by 5, leaving
        // 25 pixels of space above and below.
        let out =
            shapes(r#"<svg viewBox="0 0 20 10"><rect width="20" height="10" fill="red"/></svg>"#);
        let corners = &out[0].polygons[0];
        assert!(corners.contains(&(0.0, 25.0)), "{corners:?}");
        assert!(corners.contains(&(100.0, 75.0)), "{corners:?}");
    }

    #[test]
    fn current_color_takes_the_elements_own_colour() {
        // How an icon matches the text it sits beside.
        let out = shapes(
            r#"<svg viewBox="0 0 10 10"><path d="M0,0 L10,0 L5,10 Z" fill="currentColor"/></svg>"#,
        );
        assert_eq!(out[0].color, Color::rgb(1, 2, 3));
    }

    #[test]
    fn fill_inherits_from_a_group() {
        let out = shapes(
            r#"<svg viewBox="0 0 10 10"><g fill="blue"><path d="M0,0 L10,0 L5,10 Z"/></g></svg>"#,
        );
        assert_eq!(out[0].color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn fill_none_draws_nothing() {
        let out = shapes(r#"<svg viewBox="0 0 10 10"><path d="M0,0 L10,0 Z" fill="none"/></svg>"#);
        assert!(out.is_empty());
    }

    #[test]
    fn a_paint_we_cannot_read_skips_the_shape() {
        // A gradient reference, drawn in no colour rather than a
        // wrong one.
        let out =
            shapes(r#"<svg viewBox="0 0 10 10"><path d="M0,0 L10,0 Z" fill="url(#g)"/></svg>"#);
        assert!(out.is_empty());
    }

    #[test]
    fn a_stroke_becomes_a_ribbon_along_the_line() {
        let out = shapes(
            r#"<svg viewBox="0 0 10 10"><path d="M0,5 L10,5" fill="none" stroke="red" stroke-width="2"/></svg>"#,
        );
        assert_eq!(out.len(), 1, "the stroke, with no fill");
        assert_eq!(out[0].color, Color::rgb(255, 0, 0));
        // Two units wide at scale 10 is twenty pixels.
        let quad = &out[0].polygons[0];
        let top = quad.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        let bottom = quad.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        assert!((bottom - top - 20.0).abs() < 0.01, "{top}..{bottom}");
    }

    #[test]
    fn an_svg_with_no_view_box_is_already_in_page_units() {
        let out = shapes(r#"<svg><rect width="10" height="10" fill="red"/></svg>"#);
        assert!(out[0].polygons[0].contains(&(10.0, 10.0)));
    }

    #[test]
    fn width_and_height_stand_in_for_a_missing_view_box() {
        // A stock-logo file states `width="56" height="56"` and no
        // viewBox. Rasterised into a bigger box it must scale, or the
        // artwork sits in the corner at its authored size.
        let out = shapes(
            r#"<svg width="20" height="20"><rect width="20" height="20" fill="red"/></svg>"#,
        );
        let corners = &out[0].polygons[0];
        assert!(corners.contains(&(100.0, 100.0)), "{corners:?}");
    }

    #[test]
    fn an_empty_svg_yields_nothing() {
        assert!(shapes(r#"<svg viewBox="0 0 10 10"></svg>"#).is_empty());
    }

    #[test]
    fn fill_opacity_dims_the_fill() {
        // The decorative-wave case: painted solid instead, a shape at
        // a tenth opacity is an opaque slab over the picture below.
        let out = shapes(
            r##"<svg viewBox="0 0 10 10"><path d="M0,0 L10,0 L5,10 Z"
                 fill="#ffffff" fill-opacity="0.1"/></svg>"##,
        );
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].color.alpha - 0.1).abs() < 0.001,
            "{:?}",
            out[0].color
        );
        assert_eq!(out[0].color.red, 255, "the colour itself is unchanged");
    }

    #[test]
    fn opacity_on_a_group_reaches_every_shape_inside() {
        let out = shapes(
            r#"<svg viewBox="0 0 10 10"><g opacity="0.5">
                 <path d="M0,0 L10,0 L5,10 Z" fill="red" fill-opacity="0.5"/>
               </g></svg>"#,
        );
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].color.alpha - 0.25).abs() < 0.001,
            "{:?}",
            out[0].color
        );
    }

    #[test]
    fn an_opacity_of_zero_paints_nothing() {
        let out = shapes(
            r#"<svg viewBox="0 0 10 10"><path d="M0,0 L10,0 L5,10 Z"
                 fill="red" fill-opacity="0"/></svg>"#,
        );
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_missing_or_unreadable_opacity_is_fully_opaque() {
        for attribute in [None, Some("inherit"), Some("")] {
            assert_eq!(alpha(attribute), 1.0, "{attribute:?}");
        }
        assert_eq!(alpha(Some("50%")), 0.5);
        assert_eq!(
            alpha(Some("-2")),
            0.0,
            "clamped, not flipped back into view"
        );
        assert_eq!(alpha(Some("3")), 1.0);
    }

    #[test]
    fn fill_rule_is_read_and_inherited() {
        let evenodd = shapes(
            r#"<svg viewBox="0 0 10 10"><path d="M0,0 L10,0 L5,10 Z"
                 fill="red" fill-rule="evenodd"/></svg>"#,
        );
        assert_eq!(evenodd[0].rule, FillRule::EvenOdd);

        let from_group = shapes(
            r#"<svg viewBox="0 0 10 10"><g fill-rule="evenodd">
                 <path d="M0,0 L10,0 L5,10 Z" fill="red"/></g></svg>"#,
        );
        assert_eq!(from_group[0].rule, FillRule::EvenOdd, "it inherits");

        let overridden = shapes(
            r#"<svg viewBox="0 0 10 10"><g fill-rule="evenodd">
                 <path d="M0,0 L10,0 L5,10 Z" fill="red" fill-rule="nonzero"/></g></svg>"#,
        );
        assert_eq!(overridden[0].rule, FillRule::NonZero, "and is overridable");
    }

    #[test]
    fn a_path_with_no_fill_rule_uses_nonzero() {
        let out =
            shapes(r#"<svg viewBox="0 0 10 10"><path d="M0,0 L10,0 L5,10 Z" fill="red"/></svg>"#);
        assert_eq!(out[0].rule, FillRule::NonZero);
    }

    #[test]
    fn preserve_aspect_ratio_none_stretches_both_axes() {
        // A band authored 20 wide by 10 tall, told to fill a square:
        // `none` says stretch, so it reaches both far corners.
        let out = shapes(
            r#"<svg viewBox="0 0 20 10" preserveAspectRatio="none">
                 <rect width="20" height="10" fill="red"/></svg>"#,
        );
        let corners = &out[0].polygons[0];
        assert!(corners.contains(&(100.0, 100.0)), "{corners:?}");
    }

    #[test]
    fn the_default_preserve_aspect_ratio_still_fits_and_centres() {
        // Same drawing without the attribute: scaled by 5 and centred,
        // so its bottom edge is at 75, not 100.
        let out =
            shapes(r#"<svg viewBox="0 0 20 10"><rect width="20" height="10" fill="red"/></svg>"#);
        let corners = &out[0].polygons[0];
        assert!(corners.contains(&(100.0, 75.0)), "{corners:?}");
    }
}
