//! A computed style for every element in a document.
//!
//! One top-down walk, because inheritance only works in that order: a
//! child's style needs its parent's *computed* values, not its declared
//! ones. Doing this lazily per element would recompute every ancestor
//! each time, and doing it bottom-up is simply impossible.
//!
//! The walk also owns the `rem` chicken-and-egg: `rem` means the root
//! element's font size, which is not known until the root is computed —
//! so the root is computed first, its size recorded, and only then does
//! anything below it resolve a `rem`.
//!
//! Text nodes get no entry. Text takes its style from its parent
//! element, and giving it a copy would be a second place for the same
//! facts to live.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::cascade::RuleSet;
use super::computed::{ComputeContext, ComputedStyle};
use super::custom::CustomProperties;
use super::parser::{parse_declarations, Declaration};
use super::selector::MatchState;
use crate::engine::cryo_impl::dom::{Document, NodeId};
use crate::engine::cryo_impl::html::presentation;

/// Every element's computed style, keyed by node.
#[derive(Clone, Debug, Default)]
pub struct StyleTree {
    styles: BTreeMap<NodeId, ComputedStyle>,
    /// The root element's font size, which `rem` resolved against.
    root_font_size: f32,
}

impl StyleTree {
    /// Compute styles for the whole document.
    pub fn resolve(
        doc: &Document,
        rules: &RuleSet,
        state: &MatchState,
        context: &ComputeContext,
    ) -> Self {
        let mut tree = Self {
            styles: BTreeMap::new(),
            root_font_size: ComputedStyle::INITIAL.font_size,
        };
        let Some(root) = root_element(doc) else {
            return tree;
        };

        // The root resolves against the initial style — which is what
        // makes inheritance on the root a normal case, not a special one.
        let mut context = *context;
        let empty = CustomProperties::new();
        let (root_style, root_custom) = tree.compute(
            doc,
            rules,
            state,
            &context,
            root,
            &ComputedStyle::INITIAL,
            &empty,
        );
        tree.root_font_size = root_style.font_size;
        context.root_font_size = Some(root_style.font_size);

        // Custom properties travel down beside the style, because they
        // inherit the same way and a child's `var()` reads whatever its
        // ancestors declared.
        let mut pending = vec![(root, root_style.clone(), root_custom)];
        tree.styles.insert(root, root_style);
        while let Some((parent, parent_style, parent_custom)) = pending.pop() {
            for child in doc.children(parent) {
                if doc.element(child).is_none() {
                    continue;
                }
                let (style, custom) = tree.compute(
                    doc,
                    rules,
                    state,
                    &context,
                    child,
                    &parent_style,
                    &parent_custom,
                );
                tree.styles.insert(child, style.clone());
                pending.push((child, style, custom));
            }
        }
        tree
    }

    /// One element's computed style, and the custom properties in
    /// scope below it.
    #[allow(clippy::too_many_arguments)] // every one of them is a distinct input
    fn compute(
        &self,
        doc: &Document,
        rules: &RuleSet,
        state: &MatchState,
        context: &ComputeContext,
        element: NodeId,
        parent_style: &ComputedStyle,
        parent_custom: &Rc<CustomProperties>,
    ) -> (ComputedStyle, Rc<CustomProperties>) {
        let inline = inline_declarations(doc, element);
        let declared = rules.declared_values(doc, element, &inline, state);
        // Presentational hints sit below every author rule, so they
        // fill in only what the cascade left unset — `<img width>`
        // gives the image a size when no stylesheet gave it one.
        let declared = declared.beneath(presentation::hints(doc, element));
        // The element's own `--name` declarations join its parent's
        // before anything on this element reads one: `--pad: 4px;
        // margin: var(--pad)` has to see the 4px.
        let custom = CustomProperties::inherit(parent_custom, declared.iter());
        let declared = declared.substituted(&custom);
        (
            ComputedStyle::compute(parent_style, &declared, context),
            custom,
        )
    }

    /// This element's style, or `None` for a node that is not an element.
    pub fn get(&self, element: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(&element)
    }

    /// How many elements were styled.
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    pub fn root_font_size(&self) -> f32 {
        self.root_font_size
    }
}

/// The document element — the outermost element child of the document.
fn root_element(doc: &Document) -> Option<NodeId> {
    doc.children(doc.root())
        .find(|&id| doc.element(id).is_some())
}

/// An element's `style` attribute, parsed. Empty when it has none.
fn inline_declarations(doc: &Document, element: NodeId) -> Vec<Declaration> {
    doc.element(element)
        .and_then(|data| data.attr("style"))
        .map(parse_declarations)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::cascade::Origin;
    use crate::engine::cryo_impl::css::computed::{Display, LineHeight};
    use crate::engine::cryo_impl::css::value::{Color, Length, LengthPercentage};
    use crate::engine::cryo_impl::html::parse as parse_html;

    /// Resolve `markup` against `css`.
    fn resolve(markup: &str, css: &str) -> (Document, StyleTree) {
        let doc = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::Author, css);
        let tree = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        (doc, tree)
    }

    /// The style of the first element named `target`.
    fn style_of(markup: &str, css: &str, target: &str) -> ComputedStyle {
        let (doc, tree) = resolve(markup, css);
        let element = doc
            .first_html_element(target)
            .unwrap_or_else(|| panic!("no <{target}>"));
        tree.get(element)
            .unwrap_or_else(|| panic!("<{target}> was not styled"))
            .clone()
    }

    fn px(value: f32) -> LengthPercentage {
        LengthPercentage::Length(Length::px(value))
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    #[test]
    fn every_element_gets_a_style() {
        let (doc, tree) = resolve("<div><p>x</p><span>y</span></div>", "");
        // html, head, body, div, p, span.
        assert_eq!(tree.len(), 6);
        assert!(!tree.is_empty());
        for id in doc.descendants(doc.root()) {
            assert_eq!(
                tree.get(id).is_some(),
                doc.element(id).is_some(),
                "elements are styled and nothing else is"
            );
        }
    }

    #[test]
    fn a_text_node_has_no_style_of_its_own() {
        let (doc, tree) = resolve("<p>text</p>", "");
        let p = doc.first_html_element("p").expect("the p exists");
        let text = doc.node(p).first_child().expect("the text node");
        assert!(tree.get(text).is_none());
    }

    #[test]
    fn an_empty_document_resolves_to_an_empty_tree() {
        let doc = Document::new();
        let tree = StyleTree::resolve(
            &doc,
            &RuleSet::new(),
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn inheritance_flows_the_whole_way_down() {
        let style = style_of(
            "<div><section><p><span>x</span></p></section></div>",
            "div { color: red }",
            "span",
        );
        assert_eq!(style.color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn a_nearer_ancestor_wins_over_a_further_one() {
        let style = style_of(
            "<div><section><p>x</p></section></div>",
            "div { color: red } section { color: blue }",
            "p",
        );
        assert_eq!(style.color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn non_inherited_properties_do_not_leak_downward() {
        let style = style_of("<div><p>x</p></div>", "div { display: block }", "p");
        assert_eq!(style.display, Display::Inline);
    }

    #[test]
    fn font_sizes_compound_through_the_chain() {
        // 16 -> 8 -> 16: each em against its own parent.
        let style = style_of(
            "<div><section><p>x</p></section></div>",
            "div { font-size: 0.5em } section { font-size: 2em }",
            "p",
        );
        assert!(close(style.font_size, 16.0), "got {}", style.font_size);
    }

    #[test]
    fn rem_resolves_against_the_root_not_the_parent() {
        let style = style_of(
            "<div><p>x</p></div>",
            "html { font-size: 10px } div { font-size: 40px } p { padding-top: 2rem }",
            "p",
        );
        assert_eq!(style.padding.top, px(20.0));
    }

    #[test]
    fn the_root_font_size_is_reported() {
        let (_, tree) = resolve("<p>x</p>", "html { font-size: 20px }");
        assert!(close(tree.root_font_size(), 20.0));
        let (_, default) = resolve("<p>x</p>", "");
        assert!(close(default.root_font_size(), 16.0));
    }

    #[test]
    fn a_style_attribute_applies_and_beats_a_selector() {
        let style = style_of("<p style='color: red'>x</p>", "p { color: blue }", "p");
        assert_eq!(style.color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn a_style_attribute_shorthand_expands() {
        let style = style_of("<p style='margin: 4px'>x</p>", "", "p");
        assert_eq!(
            style.margin.left,
            crate::engine::cryo_impl::css::computed::Margin::Length(px(4.0))
        );
    }

    #[test]
    fn an_inherited_style_attribute_value_reaches_children() {
        let style = style_of("<div style='color: red'><p>x</p></div>", "", "p");
        assert_eq!(style.color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn a_numeric_line_height_inherits_as_a_ratio_through_the_tree() {
        let style = style_of(
            "<div><p>x</p></div>",
            "div { line-height: 2 } p { font-size: 30px }",
            "p",
        );
        assert_eq!(style.line_height, LineHeight::Number(2.0));
        assert!(close(style.line_box_height(), 60.0));
    }

    #[test]
    fn the_root_element_inherits_from_the_initial_style() {
        let style = style_of("<p>x</p>", "", "html");
        assert_eq!(style.color, ComputedStyle::INITIAL.color);
        assert!(close(style.font_size, 16.0));
    }

    #[test]
    fn a_user_agent_sheet_styles_the_document_structure() {
        // What the real default sheet will do: make the block elements
        // block-level. Worth proving the plumbing carries it.
        let style = style_of(
            "<div><p>x</p></div>",
            "html, body, div, p { display: block }",
            "p",
        );
        assert_eq!(style.display, Display::Block);
    }

    #[test]
    fn deep_nesting_does_not_lose_inheritance() {
        let mut markup = String::new();
        for _ in 0..50 {
            markup.push_str("<div>");
        }
        markup.push_str("<p>x</p>");
        for _ in 0..50 {
            markup.push_str("</div>");
        }
        let style = style_of(&markup, "html { color: red }", "p");
        assert_eq!(style.color, Color::rgb(255, 0, 0));
    }
}
