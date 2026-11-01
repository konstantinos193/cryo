//! SVG path data: the `d` attribute, parsed and flattened.
//!
//! A path is a string of commands — `M100,0 L200,100 C...Z` — each a
//! letter and some numbers. An uppercase letter takes absolute
//! coordinates, lowercase takes them relative to where the pen is.
//! That single rule is most of the format.
//!
//! The output is **polygons**, not curves: every curve is flattened
//! into line segments here, because the rasteriser fills polygons and
//! nothing downstream needs to know a shape was ever round. Flattening
//! at a fixed number of steps is crude at very large sizes and
//! invisible at icon sizes, which is what these are.
//!
//! The grammar is unusually permissive and the permissiveness is not
//! optional — real files rely on it:
//!
//! - Numbers may be separated by commas, spaces, or nothing at all
//!   when the sign makes the break: `1-2` is two numbers.
//! - A command letter may be followed by several sets of arguments,
//!   repeating the command: `L1,1 2,2 3,3` is three lines.
//! - A repeated `M` becomes an implicit `L`, which is the one case
//!   where the repeat changes the command.
//!
//! Known gaps, stated rather than hidden:
//! - Elliptical arcs (`A`) are flattened as a circular approximation
//!   when the two radii agree and as a straight line when they do
//!   not. Icons use them rarely; rounded rectangles use `Q` instead.

/// How many segments a curve becomes.
///
/// Sixteen is past what an icon can show and cheap enough that a
/// logo with a hundred curves costs nothing measurable.
const CURVE_STEPS: usize = 16;

/// Parse path data into closed polygons in the path's own coordinates.
///
/// Every subpath is returned closed, because a filled path is closed
/// implicitly whether or not it ends in `Z`.
pub(super) fn parse(data: &str) -> Vec<Vec<(f32, f32)>> {
    let mut reader = Reader::new(data);
    let mut polygons = Vec::new();
    let mut current: Vec<(f32, f32)> = Vec::new();
    // Where the pen is, and where the current subpath began — `Z`
    // returns to the latter.
    let mut pen = (0.0f32, 0.0f32);
    let mut start = pen;
    // The reflection of the last cubic or quadratic control point,
    // which `S` and `T` continue from.
    let mut last_cubic: Option<(f32, f32)> = None;
    let mut last_quadratic: Option<(f32, f32)> = None;
    let mut command = ' ';

    loop {
        reader.skip_separators();
        if let Some(letter) = reader.command() {
            command = letter;
        } else if reader.at_end() {
            break;
        } else if command == 'M' {
            // A repeated moveto is a lineto; see the module note.
            command = 'L';
        } else if command == 'm' {
            command = 'l';
        }

        let relative = command.is_ascii_lowercase();
        let absolute = |pen: (f32, f32), x: f32, y: f32| {
            if relative {
                (pen.0 + x, pen.1 + y)
            } else {
                (x, y)
            }
        };

        match command.to_ascii_uppercase() {
            'M' => {
                let Some((x, y)) = reader.pair() else { break };
                if current.len() > 1 {
                    polygons.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                pen = absolute(pen, x, y);
                start = pen;
                current.push(pen);
                last_cubic = None;
                last_quadratic = None;
            }
            'L' => {
                let Some((x, y)) = reader.pair() else { break };
                pen = absolute(pen, x, y);
                current.push(pen);
                last_cubic = None;
                last_quadratic = None;
            }
            'H' => {
                let Some(x) = reader.number() else { break };
                pen = if relative {
                    (pen.0 + x, pen.1)
                } else {
                    (x, pen.1)
                };
                current.push(pen);
                last_cubic = None;
                last_quadratic = None;
            }
            'V' => {
                let Some(y) = reader.number() else { break };
                pen = if relative {
                    (pen.0, pen.1 + y)
                } else {
                    (pen.0, y)
                };
                current.push(pen);
                last_cubic = None;
                last_quadratic = None;
            }
            'C' => {
                let (Some(first), Some(second), Some(end)) =
                    (reader.pair(), reader.pair(), reader.pair())
                else {
                    break;
                };
                let one = absolute(pen, first.0, first.1);
                let two = absolute(pen, second.0, second.1);
                let to = absolute(pen, end.0, end.1);
                cubic(&mut current, pen, one, two, to);
                last_cubic = Some(reflect(two, to));
                last_quadratic = None;
                pen = to;
            }
            'S' => {
                let (Some(second), Some(end)) = (reader.pair(), reader.pair()) else {
                    break;
                };
                // The first control point mirrors the previous one, or
                // sits on the pen when there was none.
                let one = last_cubic.unwrap_or(pen);
                let two = absolute(pen, second.0, second.1);
                let to = absolute(pen, end.0, end.1);
                cubic(&mut current, pen, one, two, to);
                last_cubic = Some(reflect(two, to));
                last_quadratic = None;
                pen = to;
            }
            'Q' => {
                let (Some(control), Some(end)) = (reader.pair(), reader.pair()) else {
                    break;
                };
                let one = absolute(pen, control.0, control.1);
                let to = absolute(pen, end.0, end.1);
                quadratic(&mut current, pen, one, to);
                last_quadratic = Some(reflect(one, to));
                last_cubic = None;
                pen = to;
            }
            'T' => {
                let Some(end) = reader.pair() else { break };
                let one = last_quadratic.unwrap_or(pen);
                let to = absolute(pen, end.0, end.1);
                quadratic(&mut current, pen, one, to);
                last_quadratic = Some(reflect(one, to));
                last_cubic = None;
                pen = to;
            }
            'A' => {
                // rx ry rotation large-arc sweep x y
                let (Some(radii), Some(_rotation), Some(flags), Some(end)) =
                    (reader.pair(), reader.number(), reader.pair(), reader.pair())
                else {
                    break;
                };
                let to = absolute(pen, end.0, end.1);
                arc(&mut current, pen, radii, flags.1 != 0.0, to);
                last_cubic = None;
                last_quadratic = None;
                pen = to;
            }
            'Z' => {
                if current.len() > 1 {
                    polygons.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                pen = start;
                current.push(pen);
                last_cubic = None;
                last_quadratic = None;
            }
            // An unknown letter means the data is not something we
            // read; stopping beats emitting a wrong shape.
            _ => break,
        }
    }

    if current.len() > 1 {
        polygons.push(current);
    }
    polygons
}

/// The mirror of `control` through `about` — what `S` and `T` use to
/// keep a curve smooth across a join.
fn reflect(control: (f32, f32), about: (f32, f32)) -> (f32, f32) {
    (2.0 * about.0 - control.0, 2.0 * about.1 - control.1)
}

fn cubic(
    out: &mut Vec<(f32, f32)>,
    from: (f32, f32),
    one: (f32, f32),
    two: (f32, f32),
    to: (f32, f32),
) {
    for step in 1..=CURVE_STEPS {
        let t = step as f32 / CURVE_STEPS as f32;
        let u = 1.0 - t;
        let x = u * u * u * from.0
            + 3.0 * u * u * t * one.0
            + 3.0 * u * t * t * two.0
            + t * t * t * to.0;
        let y = u * u * u * from.1
            + 3.0 * u * u * t * one.1
            + 3.0 * u * t * t * two.1
            + t * t * t * to.1;
        out.push((x, y));
    }
}

fn quadratic(out: &mut Vec<(f32, f32)>, from: (f32, f32), control: (f32, f32), to: (f32, f32)) {
    for step in 1..=CURVE_STEPS {
        let t = step as f32 / CURVE_STEPS as f32;
        let u = 1.0 - t;
        out.push((
            u * u * from.0 + 2.0 * u * t * control.0 + t * t * to.0,
            u * u * from.1 + 2.0 * u * t * control.1 + t * t * to.1,
        ));
    }
}

/// An elliptical arc, approximated — see the module note.
fn arc(
    out: &mut Vec<(f32, f32)>,
    from: (f32, f32),
    radii: (f32, f32),
    sweep: bool,
    to: (f32, f32),
) {
    let radius = radii.0.abs().max(radii.1.abs());
    let chord = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
    // A radius too small to reach, or none at all, is a straight line
    // — which is what the spec says to do as well.
    if radius <= 0.0 || chord <= 0.0 || radius * 2.0 < chord {
        out.push(to);
        return;
    }

    // The centre sits off the chord's midpoint, on whichever side the
    // sweep flag asks for.
    let middle = ((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0);
    let height = (radius * radius - chord * chord / 4.0).max(0.0).sqrt();
    let normal = ((to.1 - from.1) / chord, -(to.0 - from.0) / chord);
    let sign = if sweep { -1.0 } else { 1.0 };
    let centre = (
        middle.0 + normal.0 * height * sign,
        middle.1 + normal.1 * height * sign,
    );

    let angle_of = |point: (f32, f32)| (point.1 - centre.1).atan2(point.0 - centre.0);
    let start = angle_of(from);
    let mut end = angle_of(to);
    // Walk the way the sweep flag points.
    if sweep && end < start {
        end += std::f32::consts::TAU;
    } else if !sweep && end > start {
        end -= std::f32::consts::TAU;
    }
    for step in 1..=CURVE_STEPS {
        let angle = start + (end - start) * (step as f32 / CURVE_STEPS as f32);
        out.push((
            centre.0 + radius * angle.cos(),
            centre.1 + radius * angle.sin(),
        ));
    }
}

/// A position in path data, with the format's loose separator rules.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a str) -> Self {
        Self {
            bytes: data.as_bytes(),
            at: 0,
        }
    }

    fn at_end(&self) -> bool {
        self.at >= self.bytes.len()
    }

    /// Commas and whitespace both separate, and so does nothing at
    /// all — see the module note.
    fn skip_separators(&mut self) {
        while let Some(&byte) = self.bytes.get(self.at) {
            if byte.is_ascii_whitespace() || byte == b',' {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    /// The command letter here, if there is one.
    fn command(&mut self) -> Option<char> {
        let &byte = self.bytes.get(self.at)?;
        if !byte.is_ascii_alphabetic() {
            return None;
        }
        self.at += 1;
        Some(byte as char)
    }

    fn pair(&mut self) -> Option<(f32, f32)> {
        Some((self.number()?, self.number()?))
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let start = self.at;
        if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
            self.at += 1;
        }
        let mut seen_digit = false;
        let mut seen_point = false;
        while let Some(&byte) = self.bytes.get(self.at) {
            match byte {
                b'0'..=b'9' => {
                    seen_digit = true;
                    self.at += 1;
                }
                b'.' if !seen_point => {
                    seen_point = true;
                    self.at += 1;
                }
                // An exponent, and the sign that may follow it.
                b'e' | b'E' if seen_digit => {
                    self.at += 1;
                    if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
                        self.at += 1;
                    }
                }
                _ => break,
            }
        }
        if !seen_digit {
            self.at = start;
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.at])
            .ok()?
            .parse()
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 0.01
    }

    #[test]
    fn a_triangle_closes_itself() {
        let shapes = parse("M0,0 L10,0 L5,10 Z");
        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0][0], (0.0, 0.0));
        assert_eq!(shapes[0][1], (10.0, 0.0));
        assert_eq!(shapes[0][2], (5.0, 10.0));
    }

    #[test]
    fn a_path_with_no_z_is_still_closed_for_filling() {
        let shapes = parse("M0,0 L10,0 L5,10");
        assert_eq!(shapes.len(), 1, "one subpath, implicitly closed");
    }

    #[test]
    fn lowercase_commands_are_relative() {
        let shapes = parse("M10,10 l5,0 l0,5");
        assert_eq!(shapes[0][1], (15.0, 10.0));
        assert_eq!(shapes[0][2], (15.0, 15.0));
    }

    #[test]
    fn a_command_repeats_over_several_argument_sets() {
        let shapes = parse("M0,0 L1,1 2,2 3,3");
        assert_eq!(shapes[0].len(), 4);
        assert_eq!(shapes[0][3], (3.0, 3.0));
    }

    #[test]
    fn a_repeated_moveto_becomes_a_lineto() {
        // The one case where repeating changes the command.
        let shapes = parse("M0,0 1,1 2,2");
        assert_eq!(shapes.len(), 1, "not three separate subpaths");
        assert_eq!(shapes[0].len(), 3);
    }

    #[test]
    fn numbers_may_run_together_when_the_sign_separates_them() {
        let shapes = parse("M0,0L1-2");
        assert_eq!(shapes[0][1], (1.0, -2.0));
    }

    #[test]
    fn horizontal_and_vertical_keep_the_other_coordinate() {
        let shapes = parse("M5,5 H20 V30");
        assert_eq!(shapes[0][1], (20.0, 5.0));
        assert_eq!(shapes[0][2], (20.0, 30.0));
    }

    #[test]
    fn a_cubic_curve_is_flattened_and_ends_where_it_should() {
        let shapes = parse("M0,0 C0,10 10,10 10,0");
        let last = *shapes[0].last().unwrap();
        assert!(close(last.0, 10.0) && close(last.1, 0.0), "{last:?}");
        assert!(shapes[0].len() > 5, "flattened into segments");
    }

    #[test]
    fn a_smooth_curve_mirrors_the_previous_control_point() {
        // S after C should continue smoothly, not jump.
        let shapes = parse("M0,0 C0,5 5,5 5,0 S10,-5 10,0");
        let last = *shapes[0].last().unwrap();
        assert!(close(last.0, 10.0) && close(last.1, 0.0), "{last:?}");
    }

    #[test]
    fn a_quadratic_curve_ends_where_it_should() {
        let shapes = parse("M0,0 Q5,10 10,0");
        let last = *shapes[0].last().unwrap();
        assert!(close(last.0, 10.0) && close(last.1, 0.0), "{last:?}");
    }

    #[test]
    fn several_subpaths_come_back_separately() {
        let shapes = parse("M0,0 L1,0 Z M5,5 L6,5 Z");
        assert_eq!(shapes.len(), 2);
    }

    #[test]
    fn z_returns_the_pen_to_the_subpaths_start() {
        let shapes = parse("M10,10 L20,10 Z l5,0");
        // The second subpath starts where the first began, not where
        // its last line ended.
        assert_eq!(shapes[1][0], (10.0, 10.0));
        assert_eq!(shapes[1][1], (15.0, 10.0));
    }

    #[test]
    fn an_arc_ends_at_its_endpoint() {
        let shapes = parse("M0,0 A5,5 0 0 1 10,0");
        let last = *shapes[0].last().unwrap();
        assert!(close(last.0, 10.0) && close(last.1, 0.0), "{last:?}");
    }

    #[test]
    fn an_unreachable_arc_falls_back_to_a_straight_line() {
        // A radius too small to span the chord: the spec says to
        // enlarge it, and a line is the limit of doing so.
        let shapes = parse("M0,0 A1,1 0 0 1 100,0");
        assert_eq!(*shapes[0].last().unwrap(), (100.0, 0.0));
    }

    #[test]
    fn empty_or_broken_data_yields_no_shapes() {
        assert!(parse("").is_empty());
        assert!(parse("   ").is_empty());
        assert!(parse("nonsense").is_empty());
    }

    #[test]
    fn exponents_are_read() {
        let shapes = parse("M0,0 L1e2,2E1");
        assert_eq!(shapes[0][1], (100.0, 20.0));
    }
}
