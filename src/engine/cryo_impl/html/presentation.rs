//! What an HTML attribute says about style.
//!
//! A handful of attributes predate CSS and still carry presentation:
//! `<img width="168">`, `<td bgcolor="red">`, `<hr size="2">`. The
//! HTML standard calls these **presentational hints** and defines
//! them as real CSS declarations at a priority *below* every author
//! rule — so a stylesheet always wins, and the attribute is the
//! fallback.
//!
//! They are not legacy trivia. `<img width height>` is how every
//! image on a modern page states its size, because a browser needs
//! that size before the bytes arrive in order to reserve the space.
//! An engine that ignores it gives the image no intrinsic size at
//! all: it shrinks to nothing and the picture never appears.
//!
//! Known gaps, stated rather than hidden:
//! - `align`, `border` and the table attributes are not read.
//! - `<font>` is not read; it is extinct outside archives.

use crate::engine::cryo_impl::css::parser::{parse_value, Declaration};
use crate::engine::cryo_impl::dom::{Document, NodeId};

/// Elements whose `width`/`height` attributes are a size in pixels.
///
/// On anything else the two attributes mean something else or
/// nothing — `<input width>` is not a size — so the list is explicit.
const SIZED: [&str; 7] = [
    "img", "canvas", "video", "embed", "object", "iframe", "table",
];

/// The declarations an element's attributes imply.
///
/// Returned as ordinary declarations so the cascade can treat them
/// like any other, at the priority the caller gives them.
pub fn hints(document: &Document, element: NodeId) -> Vec<Declaration> {
    let Some(data) = document.element(element) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    // `hidden` is an attribute, not a property, and it means exactly
    // `display: none`.
    if data.has_attr("hidden") {
        out.push(declaration("display", "none"));
    }

    if SIZED
        .iter()
        .any(|name| document.is_html_element(element, name))
    {
        for attribute in ["width", "height"] {
            if let Some(length) = dimension(data.attr(attribute)) {
                out.push(declaration(attribute, &length));
            }
        }
        // Both together also give a *ratio*, which is the mechanism
        // that stops a `height: auto` image collapsing to nothing
        // before its bytes arrive — and the one a stylesheet relies
        // on when it writes `height: auto` on purpose.
        if let (Some(width), Some(height)) =
            (number(data.attr("width")), number(data.attr("height")))
        {
            if width > 0.0 && height > 0.0 {
                out.push(declaration("aspect-ratio", &format!("{width} / {height}")));
            }
        }
    }

    if let Some(color) = data.attr("bgcolor").filter(|text| !text.trim().is_empty()) {
        out.push(declaration("background-color", color));
    }

    out
}

/// A dimension attribute as a plain number, percentages excluded.
fn number(value: Option<&str>) -> Option<f32> {
    value?.trim().parse().ok()
}

/// A dimension attribute: a bare number is pixels, a trailing `%` is
/// a percentage, and anything else is ignored.
fn dimension(value: Option<&str>) -> Option<String> {
    let text = value?.trim();
    if let Some(number) = text.strip_suffix('%') {
        let percent: f32 = number.trim().parse().ok()?;
        return (percent >= 0.0).then(|| format!("{percent}%"));
    }
    let pixels: f32 = text.parse().ok()?;
    (pixels >= 0.0).then(|| format!("{pixels}px"))
}

fn declaration(name: &str, value: &str) -> Declaration {
    Declaration {
        name: name.to_owned(),
        value: parse_value(value),
        important: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::serialize;
    use crate::engine::cryo_impl::html::parse as parse_html;

    /// The hints of the first element named `name` in `markup`.
    fn hints_of(markup: &str, name: &str) -> Vec<(String, String)> {
        let document = parse_html(markup);
        let element = document
            .descendants(document.root())
            .find(|&id| document.is_html_element(id, name))
            .expect("the element");
        hints(&document, element)
            .into_iter()
            .map(|d| (d.name, serialize(&d.value)))
            .collect()
    }

    #[test]
    fn an_images_dimensions_become_a_size() {
        // Without this the image has no intrinsic size and vanishes.
        assert_eq!(
            hints_of(r#"<img src=x width="168" height="52">"#, "img"),
            [
                ("width".to_owned(), "168px".to_owned()),
                ("height".to_owned(), "52px".to_owned()),
                // The ratio too, so a stylesheet's `height: auto`
                // still leaves the image a height.
                ("aspect-ratio".to_owned(), "168 / 52".to_owned()),
            ]
        );
    }

    #[test]
    fn a_percentage_stays_a_percentage() {
        assert_eq!(
            hints_of(r#"<img src=x width="50%">"#, "img"),
            [("width".to_owned(), "50%".to_owned())]
        );
    }

    #[test]
    fn a_dimension_that_is_not_a_number_is_ignored() {
        assert!(hints_of(r#"<img src=x width="wide">"#, "img").is_empty());
        assert!(hints_of(r#"<img src=x width="-5">"#, "img").is_empty());
    }

    #[test]
    fn width_on_an_element_that_is_not_sized_means_nothing() {
        // `<input width>` is not a size.
        assert!(hints_of(r#"<input width="20">"#, "input").is_empty());
    }

    #[test]
    fn hidden_is_display_none() {
        assert_eq!(
            hints_of("<div hidden>x</div>", "div"),
            [("display".to_owned(), "none".to_owned())]
        );
    }

    #[test]
    fn bgcolor_becomes_a_background() {
        // In a table, because a bare `<td>` is dropped by the parser.
        assert_eq!(
            hints_of(r#"<table><tr><td bgcolor="red">x</td></tr></table>"#, "td"),
            [("background-color".to_owned(), "red".to_owned())]
        );
    }

    #[test]
    fn an_element_with_no_hints_yields_none() {
        assert!(hints_of("<p>x</p>", "p").is_empty());
    }
}
