//! The basic SVG shapes, as outlines.
//!
//! `<path>` carries its own geometry and is handled by [`super::path`];
//! everything here is a shorthand for one that could have been written
//! as a path. A `<rect>` is four corners, a `<circle>` is an arc all
//! the way round. Turning them into the same polygons means the rest
//! of the renderer has one kind of thing to draw.
//!
//! Coordinates stay in the drawing's own units; the caller maps them
//! onto the page.

use crate::engine::cryo_impl::dom::ElementData;

use super::path;

/// How many segments a full circle becomes.
///
/// Forty-eight is smooth past any icon size and still cheap.
const CIRCLE_STEPS: usize = 48;

/// The outline of one shape element, or `None` if it is not one.
pub(super) fn outline(element: &ElementData, name: &str) -> Option<Vec<Vec<(f32, f32)>>> {
    let number = |attribute: &str| -> f32 {
        element
            .attr(attribute)
            .and_then(|text| text.trim().trim_end_matches("px").parse().ok())
            .unwrap_or(0.0)
    };

    match name {
        "path" => {
            let shapes = path::parse(element.attr("d")?);
            (!shapes.is_empty()).then_some(shapes)
        }
        "rect" => {
            let (x, y) = (number("x"), number("y"));
            let (width, height) = (number("width"), number("height"));
            if width <= 0.0 || height <= 0.0 {
                return None;
            }
            // `rx`/`ry` round the corners. One given alone stands for
            // both, which is the rule the attribute pair follows.
            let rx = match (element.attr("rx"), element.attr("ry")) {
                (None, None) => 0.0,
                _ => number("rx").max(number("ry")),
            };
            let radius = rx.min(width / 2.0).min(height / 2.0);
            Some(vec![rounded_rect(x, y, width, height, radius)])
        }
        "circle" => {
            let radius = number("r");
            if radius <= 0.0 {
                return None;
            }
            Some(vec![ellipse(number("cx"), number("cy"), radius, radius)])
        }
        "ellipse" => {
            let (rx, ry) = (number("rx"), number("ry"));
            if rx <= 0.0 || ry <= 0.0 {
                return None;
            }
            Some(vec![ellipse(number("cx"), number("cy"), rx, ry)])
        }
        "line" => Some(vec![vec![
            (number("x1"), number("y1")),
            (number("x2"), number("y2")),
        ]]),
        "polygon" | "polyline" => {
            let points = parse_points(element.attr("points")?);
            (points.len() > 1).then_some(vec![points])
        }
        _ => None,
    }
}

/// A rectangle, with rounded corners when asked for.
fn rounded_rect(x: f32, y: f32, width: f32, height: f32, radius: f32) -> Vec<(f32, f32)> {
    if radius <= 0.0 {
        return vec![
            (x, y),
            (x + width, y),
            (x + width, y + height),
            (x, y + height),
        ];
    }
    let steps = 8;
    let mut points = Vec::with_capacity(steps * 4 + 4);
    // Clockwise from the top-left corner, in SVG's y-down space.
    let corners = [
        (x + radius, y + radius, std::f32::consts::PI, 1.5),
        (
            x + width - radius,
            y + radius,
            1.5 * std::f32::consts::PI,
            2.0,
        ),
        (x + width - radius, y + height - radius, 0.0, 0.5),
        (
            x + radius,
            y + height - radius,
            0.5 * std::f32::consts::PI,
            1.0,
        ),
    ];
    for (centre_x, centre_y, from, to_turns) in corners {
        let to = to_turns * std::f32::consts::PI;
        for step in 0..=steps {
            let angle = from + (to - from) * (step as f32 / steps as f32);
            points.push((
                centre_x + radius * angle.cos(),
                centre_y + radius * angle.sin(),
            ));
        }
    }
    points
}

fn ellipse(centre_x: f32, centre_y: f32, rx: f32, ry: f32) -> Vec<(f32, f32)> {
    (0..CIRCLE_STEPS)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / CIRCLE_STEPS as f32;
            (centre_x + rx * angle.cos(), centre_y + ry * angle.sin())
        })
        .collect()
}

/// `points="0,0 10,0 5,10"`, where commas and spaces both separate.
fn parse_points(text: &str) -> Vec<(f32, f32)> {
    let numbers: Vec<f32> = text
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    numbers
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::html::parse as parse_html;

    /// The outline of the one shape in `markup`.
    fn outline_of(markup: &str, name: &str) -> Option<Vec<Vec<(f32, f32)>>> {
        let document = parse_html(&format!("<svg>{markup}</svg>"));
        let node = document
            .descendants(document.root())
            .find(|&id| document.is_html_element(id, name))?;
        let element = document.element(node)?;
        outline(element, name)
    }

    #[test]
    fn a_rect_is_four_corners() {
        let shape = outline_of(r#"<rect x="1" y="2" width="10" height="20"/>"#, "rect").unwrap();
        assert_eq!(
            shape[0],
            vec![(1.0, 2.0), (11.0, 2.0), (11.0, 22.0), (1.0, 22.0)]
        );
    }

    #[test]
    fn a_rect_with_no_size_draws_nothing() {
        assert!(outline_of(r#"<rect width="0" height="10"/>"#, "rect").is_none());
    }

    #[test]
    fn a_rounded_rect_has_corners_rather_than_points() {
        let shape = outline_of(r#"<rect width="20" height="20" rx="5"/>"#, "rect").unwrap();
        assert!(shape[0].len() > 8, "rounded into segments");
        // No point sits at the square corner any more.
        assert!(!shape[0].contains(&(0.0, 0.0)));
    }

    #[test]
    fn a_circle_is_closed_and_round() {
        let shape = outline_of(r#"<circle cx="10" cy="10" r="5"/>"#, "circle").unwrap();
        assert_eq!(shape[0].len(), CIRCLE_STEPS);
        for &(x, y) in &shape[0] {
            let distance = ((x - 10.0).powi(2) + (y - 10.0).powi(2)).sqrt();
            assert!((distance - 5.0).abs() < 0.01, "{x},{y}");
        }
    }

    #[test]
    fn an_ellipse_uses_both_radii() {
        let shape = outline_of(r#"<ellipse cx="0" cy="0" rx="10" ry="5"/>"#, "ellipse").unwrap();
        let widest = shape[0].iter().map(|p| p.0).fold(f32::MIN, f32::max);
        let tallest = shape[0].iter().map(|p| p.1).fold(f32::MIN, f32::max);
        assert!((widest - 10.0).abs() < 0.01);
        assert!((tallest - 5.0).abs() < 0.01);
    }

    #[test]
    fn a_polygon_reads_its_point_list() {
        let shape = outline_of(r#"<polygon points="0,0 10,0 5,10"/>"#, "polygon").unwrap();
        assert_eq!(shape[0], vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)]);
    }

    #[test]
    fn points_may_be_separated_by_spaces_alone() {
        let shape = outline_of(r#"<polyline points="0 0 10 0"/>"#, "polyline").unwrap();
        assert_eq!(shape[0], vec![(0.0, 0.0), (10.0, 0.0)]);
    }

    #[test]
    fn a_line_is_two_points() {
        let shape = outline_of(r#"<line x1="1" y1="2" x2="3" y2="4"/>"#, "line").unwrap();
        assert_eq!(shape[0], vec![(1.0, 2.0), (3.0, 4.0)]);
    }

    #[test]
    fn a_path_defers_to_the_path_parser() {
        let shape = outline_of(r#"<path d="M0,0 L10,0 Z"/>"#, "path").unwrap();
        assert_eq!(shape[0][1], (10.0, 0.0));
    }

    #[test]
    fn a_path_with_no_data_draws_nothing() {
        assert!(outline_of(r#"<path/>"#, "path").is_none());
    }
}
