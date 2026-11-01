//! `background-image`: a list of layers, topmost first.
//!
//! CSS lets a box carry several backgrounds at once, comma separated,
//! and the *first* in the list paints on top of the rest. That order
//! is the opposite of the one a painter wants, which is the only
//! thing about this worth remembering.
//!
//! ```css
//!   background-image: url(hero.jpg), linear-gradient(#1a3b5c, #285d7c);
//! ```
//!
//! A page writes that so the gradient shows while the photograph is
//! still arriving, and shows through wherever the photograph has
//! transparency. An engine that reads only the first layer paints the
//! photograph and never the gradient; one that reads only the last
//! does the reverse.
//!
//! A URL here is kept as written. Resolving it against the document's
//! base belongs to whoever fetches it, which knows what that base is.

use super::super::parser::ComponentValue;
use super::super::tokenizer::CssToken;
use super::gradient::Gradient;

/// One background layer.
#[derive(Clone, Debug, PartialEq)]
pub enum BackgroundImage {
    Gradient(Gradient),
    /// A URL as written in the stylesheet, not yet resolved.
    Url(String),
}

impl BackgroundImage {
    /// Parse a `background-image` list. `none` yields no layers.
    pub fn parse_list(values: &[ComponentValue]) -> Option<Vec<Self>> {
        let mut layers = Vec::new();
        for part in split_on_commas(values) {
            let trimmed: Vec<ComponentValue> = part
                .iter()
                .filter(|value| !matches!(value, ComponentValue::Token(CssToken::Whitespace)))
                .cloned()
                .collect();
            match trimmed.as_slice() {
                // `none` in a list clears that layer, and alone it
                // clears the property.
                [ComponentValue::Token(CssToken::Ident(word))]
                    if word.eq_ignore_ascii_case("none") => {}
                [ComponentValue::Token(CssToken::Url(url))] => {
                    layers.push(Self::Url(url.clone()));
                }
                // `url("x")` tokenises as a function when the argument
                // is quoted, which is how most stylesheets write it.
                [ComponentValue::Function { name, arguments }]
                    if name.eq_ignore_ascii_case("url") =>
                {
                    layers.push(Self::Url(quoted(arguments)?));
                }
                _ => layers.push(Self::Gradient(Gradient::parse(&part)?)),
            }
        }
        Some(layers)
    }

    /// The URL this layer names, if it is one.
    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Url(url) => Some(url),
            Self::Gradient(_) => None,
        }
    }
}

/// Which of a box's three rectangles a background layer is painted in.
///
/// The default is the **border** box, which surprises people: a
/// background runs *under* the border, and only shows there when the
/// border is transparent or dashed. That is not a curiosity — it is
/// how a gradient border is drawn, and the idiom is everywhere:
///
/// ```css
///   border: 1px solid transparent;
///   background: linear-gradient(#111, #111) padding-box,
///               linear-gradient(90deg, #ccff00, #333) border-box;
/// ```
///
/// Two layers, two boxes. The second fills the whole border box, the
/// first covers everything inside the border, and what is left
/// showing is a one-pixel gradient rim. Paint both layers into the
/// same rectangle and the top one simply hides the other — the rim
/// vanishes, or the whole control comes out the colour of the rim.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackgroundBox {
    #[default]
    BorderBox,
    PaddingBox,
    ContentBox,
}

impl BackgroundBox {
    /// Parse a `background-clip` or `background-origin` list, one
    /// entry per layer.
    pub fn parse_list(values: &[ComponentValue]) -> Option<Vec<Self>> {
        let mut boxes = Vec::new();
        for part in split_on_commas(values) {
            boxes.push(Self::parse_one(&part)?);
        }
        (!boxes.is_empty()).then_some(boxes)
    }

    /// One keyword, or `None` if it is not a box keyword at all.
    pub fn parse_one(values: &[ComponentValue]) -> Option<Self> {
        let mut found = None;
        for value in values {
            match value {
                ComponentValue::Token(CssToken::Whitespace) => continue,
                ComponentValue::Token(CssToken::Ident(word)) => {
                    found = Some(match word.to_ascii_lowercase().as_str() {
                        "border-box" => Self::BorderBox,
                        "padding-box" => Self::PaddingBox,
                        "content-box" => Self::ContentBox,
                        // `text` clips to the glyphs, which needs a
                        // mask this engine cannot build yet.
                        _ => return None,
                    });
                }
                _ => return None,
            }
        }
        found
    }

    /// The clip for layer `index` of a list, cycling the way CSS
    /// repeats a shorter list across more layers.
    pub fn for_layer(list: &[Self], index: usize) -> Self {
        if list.is_empty() {
            return Self::BorderBox;
        }
        list[index % list.len()]
    }
}

/// The single string inside `url("...")`.
fn quoted(arguments: &[ComponentValue]) -> Option<String> {
    arguments.iter().find_map(|value| match value {
        ComponentValue::Token(CssToken::Str(text)) => Some(text.clone()),
        ComponentValue::Token(CssToken::Ident(text)) => Some(text.clone()),
        _ => None,
    })
}

/// Split on top-level commas.
fn split_on_commas(values: &[ComponentValue]) -> Vec<Vec<ComponentValue>> {
    let mut out = vec![Vec::new()];
    for value in values {
        if matches!(value, ComponentValue::Token(CssToken::Comma)) {
            out.push(Vec::new());
        } else {
            out.last_mut()
                .expect("one part is always open")
                .push(value.clone());
        }
    }
    out.retain(|part| {
        part.iter()
            .any(|value| !matches!(value, ComponentValue::Token(CssToken::Whitespace)))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::parse;
    use crate::engine::cryo_impl::css::parser::Rule;

    /// The parsed `background-image` of the one rule in `css`.
    fn layers(value: &str) -> Vec<BackgroundImage> {
        let sheet = parse(&format!("a {{ background-image: {value} }}"));
        let Some(Rule::Style(style)) = sheet.rules.first() else {
            panic!("not a style rule");
        };
        BackgroundImage::parse_list(&style.declarations[0].value).expect("parses")
    }

    #[test]
    fn an_unquoted_url_is_one_layer() {
        assert_eq!(
            layers("url(hero.jpg)"),
            vec![BackgroundImage::Url("hero.jpg".to_owned())]
        );
    }

    #[test]
    fn a_quoted_url_is_the_same_layer() {
        assert_eq!(
            layers("url(\"hero.jpg\")"),
            vec![BackgroundImage::Url("hero.jpg".to_owned())]
        );
    }

    #[test]
    fn none_yields_no_layers() {
        assert!(layers("none").is_empty());
    }

    #[test]
    fn a_gradient_is_a_layer_of_its_own() {
        let parsed = layers("linear-gradient(red, blue)");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], BackgroundImage::Gradient(_)));
    }

    #[test]
    fn a_list_keeps_its_order_topmost_first() {
        // The case the module note is about: a photograph over a
        // gradient. Reading only one of the two is the bug.
        let parsed = layers("url(hero.jpg), linear-gradient(red, blue)");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].url(), Some("hero.jpg"));
        assert!(matches!(parsed[1], BackgroundImage::Gradient(_)));
    }

    #[test]
    fn a_url_reports_itself_and_a_gradient_does_not() {
        let parsed = layers("url(a.png), linear-gradient(red, blue)");
        assert_eq!(parsed[0].url(), Some("a.png"));
        assert_eq!(parsed[1].url(), None);
    }
}
