//! Custom properties: `--name: value` and the `var()` that reads it.
//!
//! A custom property is not a property in the usual sense. Nothing
//! parses its value, nothing validates it, and it affects no box by
//! itself — it is a *name for a list of tokens*, inherited down the
//! tree, that other declarations paste into themselves with `var()`.
//!
//! ```css
//!   :root { --brand: #0b57d0; --pad: 8px }
//!   .button { color: var(--brand); padding: var(--pad) calc(var(--pad) * 2) }
//! ```
//!
//! Substitution happens *before* a value is parsed, which is why it
//! lives here and not in `computed`: by the time a property is being
//! read as a colour or a length, every `var()` in it is already gone.
//!
//! This is not a nicety. A stylesheet from a current toolchain writes
//! most of its values this way — a third of the declarations on a
//! Tailwind page are `var()` calls — so an engine that ignores them
//! loads the rules, matches the selectors, and then computes almost
//! nothing.
//!
//! Known gaps, stated rather than hidden:
//! - `@property` is not read, so a custom property has no type, no
//!   initial value and no animation behaviour. It is tokens.
//! - A declaration whose `var()` resolves to nothing is left with the
//!   tokens around it rather than being thrown out. The spec says such
//!   a declaration is invalid at computed-value time and should fall
//!   back to the inherited or initial value; here the property simply
//!   fails to parse, which reaches the same place by a shorter road.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::parser::{trim_whitespace, ComponentValue};
use super::tokenizer::CssToken;

/// How deep a `var()` may reach through other `var()`s.
///
/// Custom properties may refer to each other, and a cycle between two
/// of them would otherwise never end. The spec makes a cycle invalid;
/// a depth limit reaches the same answer without a graph walk.
const MAX_DEPTH: u8 = 16;

/// The custom properties in scope on one element.
///
/// Inherited, like `color` — which is what lets a theme be declared
/// once on `:root` and read anywhere below it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CustomProperties {
    values: BTreeMap<String, Vec<ComponentValue>>,
}

impl CustomProperties {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// The set in scope on an element: its parent's, plus whatever it
    /// declares itself.
    ///
    /// Returns the parent's own `Rc` when the element declares none,
    /// which is almost every element on a page — a theme is written
    /// once at the root and read everywhere.
    pub fn inherit<'a>(
        parent: &Rc<Self>,
        declared: impl Iterator<Item = (&'a str, &'a [ComponentValue])>,
    ) -> Rc<Self> {
        let mut custom = declared.filter(|(name, _)| is_custom(name)).peekable();
        if custom.peek().is_none() {
            return Rc::clone(parent);
        }
        let mut values = parent.values.clone();
        for (name, value) in custom {
            values.insert(name.to_owned(), value.to_vec());
        }
        Rc::new(Self { values })
    }

    fn get(&self, name: &str) -> Option<&[ComponentValue]> {
        self.values.get(name).map(Vec::as_slice)
    }

    /// Whether anything is defined. Used to skip substitution entirely
    /// on a page that declares no custom properties.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Whether a property name is a custom property.
pub fn is_custom(name: &str) -> bool {
    name.starts_with("--")
}

/// Replace every `var()` in `values` with what it refers to.
pub fn substitute(values: &[ComponentValue], properties: &CustomProperties) -> Vec<ComponentValue> {
    // A `var()` that expands to nothing, or a fallback written with a
    // space after the comma, would otherwise leave stray whitespace at
    // the ends of the value — which some property parsers read as a
    // second component and reject.
    trim_whitespace(substitute_to_depth(values, properties, 0))
}

fn substitute_to_depth(
    values: &[ComponentValue],
    properties: &CustomProperties,
    depth: u8,
) -> Vec<ComponentValue> {
    if depth >= MAX_DEPTH {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        match value {
            ComponentValue::Function { name, arguments } if name.eq_ignore_ascii_case("var") => {
                out.extend(expand(arguments, properties, depth));
            }
            // A `var()` may sit anywhere a value may, including inside
            // another function or a block: `rgb(var(--r) 0 0)` and
            // `calc(var(--pad) * 2)` are both ordinary.
            ComponentValue::Function { name, arguments } => out.push(ComponentValue::Function {
                name: name.clone(),
                arguments: substitute_to_depth(arguments, properties, depth + 1),
            }),
            ComponentValue::Block { kind, values } => out.push(ComponentValue::Block {
                kind: *kind,
                values: substitute_to_depth(values, properties, depth + 1),
            }),
            token => out.push(token.clone()),
        }
    }
    out
}

/// One `var(--name)` or `var(--name, fallback)`.
fn expand(
    arguments: &[ComponentValue],
    properties: &CustomProperties,
    depth: u8,
) -> Vec<ComponentValue> {
    let (name, fallback) = split_arguments(arguments);
    let Some(name) = name else {
        return Vec::new();
    };

    // The referenced value may itself contain `var()`, so it is
    // substituted in turn — this is where the depth limit earns its
    // keep.
    if let Some(referenced) = properties.get(&name) {
        return substitute_to_depth(referenced, properties, depth + 1);
    }
    match fallback {
        Some(values) => trim_whitespace(substitute_to_depth(values, properties, depth + 1)),
        None => Vec::new(),
    }
}

/// A `var()`'s name and its fallback, split at the first comma.
fn split_arguments(arguments: &[ComponentValue]) -> (Option<String>, Option<&[ComponentValue]>) {
    let comma = arguments
        .iter()
        .position(|value| matches!(value, ComponentValue::Token(CssToken::Comma)));
    let (head, tail) = match comma {
        Some(index) => (&arguments[..index], Some(&arguments[index + 1..])),
        None => (arguments, None),
    };

    let name = head.iter().find_map(|value| match value {
        ComponentValue::Token(CssToken::Ident(word)) if is_custom(word) => Some(word.clone()),
        _ => None,
    });
    (name, tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::Rule;
    use crate::engine::cryo_impl::css::parser::{parse, Declaration};

    /// The declarations of the one style rule in `css`.
    fn declarations(css: &str) -> Vec<Declaration> {
        let sheet = parse(css);
        match sheet.rules.into_iter().next() {
            Some(Rule::Style(style)) => style.declarations,
            other => panic!("expected a style rule, got {other:?}"),
        }
    }

    /// Name/value pairs, as the cascade hands them over.
    fn pairs(declarations: &[Declaration]) -> impl Iterator<Item = (&str, &[ComponentValue])> {
        declarations
            .iter()
            .map(|d| (d.name.as_str(), d.value.as_slice()))
    }

    /// Substitute `property` in `css` against the properties in it.
    fn resolved(css: &str, property: &str) -> String {
        let declarations = declarations(css);
        let properties = CustomProperties::inherit(&CustomProperties::new(), pairs(&declarations));
        let value = declarations
            .iter()
            .find(|d| d.name == property)
            .unwrap_or_else(|| panic!("no {property} declared"));
        super::super::parser::serialize(&substitute(&value.value, &properties))
    }

    #[test]
    fn a_var_takes_the_value_of_its_property() {
        assert_eq!(
            resolved("a { --brand: red; color: var(--brand) }", "color"),
            "red"
        );
    }

    #[test]
    fn a_var_can_stand_among_other_tokens() {
        assert_eq!(
            resolved("a { --pad: 8px; margin: 0 var(--pad) 4px }", "margin"),
            "0 8px 4px"
        );
    }

    #[test]
    fn an_undefined_var_falls_back() {
        assert_eq!(
            resolved("a { color: var(--missing, blue) }", "color"),
            "blue"
        );
    }

    #[test]
    fn a_defined_var_ignores_its_fallback() {
        assert_eq!(
            resolved("a { --brand: red; color: var(--brand, blue) }", "color"),
            "red"
        );
    }

    #[test]
    fn an_undefined_var_with_no_fallback_leaves_nothing() {
        assert_eq!(resolved("a { color: var(--missing) }", "color"), "");
    }

    #[test]
    fn a_var_resolves_through_another_var() {
        assert_eq!(
            resolved("a { --a: red; --b: var(--a); color: var(--b) }", "color"),
            "red"
        );
    }

    #[test]
    fn a_var_inside_a_function_is_substituted() {
        assert_eq!(
            resolved("a { --x: 4px; width: calc(var(--x) * 2) }", "width"),
            "calc(4px * 2)"
        );
    }

    #[test]
    fn a_cycle_resolves_to_nothing_rather_than_hanging() {
        // Two properties naming each other. The spec calls this
        // invalid; the depth limit reaches the same answer.
        let out = resolved(
            "a { --a: var(--b); --b: var(--a); color: var(--a) }",
            "color",
        );
        assert_eq!(out, "");
    }

    #[test]
    fn a_fallback_may_itself_be_a_var() {
        assert_eq!(
            resolved("a { --b: green; color: var(--a, var(--b)) }", "color"),
            "green"
        );
    }

    #[test]
    fn custom_properties_inherit() {
        let root = CustomProperties::inherit(
            &CustomProperties::new(),
            pairs(&declarations("a { --brand: red }")),
        );
        let child = CustomProperties::inherit(&root, pairs(&declarations("a { color: blue }")));
        assert_eq!(
            child.get("--brand").map(super::super::parser::serialize),
            Some("red".to_owned())
        );
    }

    #[test]
    fn a_child_may_shadow_an_inherited_property() {
        let root = CustomProperties::inherit(
            &CustomProperties::new(),
            pairs(&declarations("a { --brand: red }")),
        );
        let child = CustomProperties::inherit(&root, pairs(&declarations("a { --brand: blue }")));
        assert_eq!(
            child.get("--brand").map(super::super::parser::serialize),
            Some("blue".to_owned())
        );
        // The parent is untouched: the map is copied, not mutated.
        assert_eq!(
            root.get("--brand").map(super::super::parser::serialize),
            Some("red".to_owned())
        );
    }

    #[test]
    fn an_element_declaring_none_shares_its_parents_set() {
        let root = CustomProperties::inherit(
            &CustomProperties::new(),
            pairs(&declarations("a { --brand: red }")),
        );
        let child = CustomProperties::inherit(&root, pairs(&declarations("a { color: blue }")));
        assert!(Rc::ptr_eq(&root, &child), "no copy for the common case");
    }

    #[test]
    fn a_name_that_is_not_custom_is_not_one() {
        assert!(is_custom("--brand"));
        assert!(!is_custom("color"));
        assert!(!is_custom("-webkit-box"));
    }
}
