//! `clip-path`: the shape a box is cut to.
//!
//! A clip is not a drawing — it is a stencil. The box paints as it
//! always would and then everything outside the shape is thrown away,
//! which is how a panel gets a curved edge without any of its
//! contents knowing.
//!
//! Only the basic shapes are read, and in practice only one of them
//! matters: `ellipse()` is what a page reaches for when it wants a
//! panel to bulge. A `url(#path)` reference points at an SVG shape
//! elsewhere in the document and is refused rather than approximated.
//!
//! Known gaps, stated rather than hidden:
//! - No `path()`, `polygon()` or `url()`.
//! - The reference box is always the border box; `content-box` and
//!   the rest are parsed and ignored.

use super::super::parser::ComponentValue;
use super::super::tokenizer::CssToken;
use super::{LengthContext, LengthPercentage};

/// A shape a box is clipped to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClipPath {
    /// Radii and centre, each a length or a percentage of the box.
    Ellipse {
        radius_x: LengthPercentage,
        radius_y: LengthPercentage,
        centre_x: LengthPercentage,
        centre_y: LengthPercentage,
    },
    /// A circle is an ellipse whose radii agree, but it is written
    /// with one radius, so it parses differently.
    Circle {
        radius: LengthPercentage,
        centre_x: LengthPercentage,
        centre_y: LengthPercentage,
    },
    /// Rounded-rectangle insets from each edge.
    Inset {
        top: LengthPercentage,
        right: LengthPercentage,
        bottom: LengthPercentage,
        left: LengthPercentage,
    },
}

impl ClipPath {
    /// Fix any lengths to pixels.
    pub fn computed(self, context: &LengthContext) -> Self {
        let fix = |value: LengthPercentage| value.computed(context);
        match self {
            Self::Ellipse {
                radius_x,
                radius_y,
                centre_x,
                centre_y,
            } => Self::Ellipse {
                radius_x: fix(radius_x),
                radius_y: fix(radius_y),
                centre_x: fix(centre_x),
                centre_y: fix(centre_y),
            },
            Self::Circle {
                radius,
                centre_x,
                centre_y,
            } => Self::Circle {
                radius: fix(radius),
                centre_x: fix(centre_x),
                centre_y: fix(centre_y),
            },
            Self::Inset {
                top,
                right,
                bottom,
                left,
            } => Self::Inset {
                top: fix(top),
                right: fix(right),
                bottom: fix(bottom),
                left: fix(left),
            },
        }
    }

    /// Parse `clip-path`, or `None` for `none` and anything unread.
    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        let (name, arguments) = values.iter().find_map(|value| match value {
            ComponentValue::Function { name, arguments } => {
                Some((name.to_ascii_lowercase(), arguments))
            }
            _ => None,
        })?;

        // `at` splits the shape from its position: `ellipse(100% 145%
        // at 0%)`.
        let mut sizes = Vec::new();
        let mut places = Vec::new();
        let mut after_at = false;
        for value in arguments {
            match value {
                ComponentValue::Token(CssToken::Whitespace) => {}
                ComponentValue::Token(CssToken::Ident(word)) => {
                    if word.eq_ignore_ascii_case("at") {
                        after_at = true;
                    }
                    // Keywords like `closest-side` or `border-box` are
                    // accepted and ignored; see the module note.
                }
                other => {
                    let length = super::parse_length_percentage(std::slice::from_ref(other))?;
                    if after_at {
                        places.push(length);
                    } else {
                        sizes.push(length);
                    }
                }
            }
        }

        let half = LengthPercentage::Percentage(50.0);
        let centre_x = places.first().copied().unwrap_or(half);
        let centre_y = places.get(1).copied().unwrap_or(half);

        match name.as_str() {
            "ellipse" => Some(Self::Ellipse {
                radius_x: sizes.first().copied().unwrap_or(half),
                radius_y: sizes.get(1).copied().unwrap_or(half),
                centre_x,
                centre_y,
            }),
            "circle" => Some(Self::Circle {
                radius: sizes.first().copied().unwrap_or(half),
                centre_x,
                centre_y,
            }),
            "inset" => {
                let zero = LengthPercentage::ZERO;
                // The one-to-four rule, as every box shorthand uses.
                let at = |index: usize, fallback: usize| {
                    sizes
                        .get(index)
                        .or(sizes.get(fallback))
                        .copied()
                        .unwrap_or(zero)
                };
                Some(Self::Inset {
                    top: at(0, 0),
                    right: at(1, 0),
                    bottom: at(2, 0),
                    left: at(3, 1),
                })
            }
            _ => None,
        }
    }
}
