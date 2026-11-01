//! Answering whether an element matches a selector.
//!
//! Matching runs right to left from the element being styled, and it
//! **backtracks**. That is not an optimisation choice; a greedy walk is
//! simply wrong. Take `.x > .y p` against:
//!
//! ```text
//!   <div class=x><div class=y><div class=y><p>
//! ```
//!
//! The `p` has two ancestors matching `.y`. Greedily taking the nearest
//! one leaves a parent that is not `.x`, and the match fails — but the
//! selector does match, via the outer `.y`. So every descendant and
//! sibling step tries each candidate and recurses, which is why
//! [`matches_parts`] is recursive rather than a loop with a cursor.
//!
//! State pseudo-classes come from [`MatchState`] rather than the tree,
//! because the DOM cannot know what the pointer is over. With the
//! default state nothing is hovered or focused, which is what a static
//! render wants.

use super::{
    AttrMatch, AttributeSelector, Combinator, Compound, Nth, PseudoClass, Selector, SelectorList,
    SelectorPart,
};
use crate::engine::cryo_impl::dom::{Document, ElementData, NodeData, NodeId};

/// Element state the tree cannot answer on its own.
///
/// Empty by default: `:hover` matching nothing is the right answer for a
/// render with no pointer, and it means a caller that does not care
/// about interaction passes `&MatchState::default()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchState {
    pub hovered: Option<NodeId>,
    pub focused: Option<NodeId>,
    pub active: Option<NodeId>,
}

impl MatchState {
    /// The state a page is in the moment it finishes loading.
    ///
    /// Nothing is hovered or active — there has been no pointer yet —
    /// but `autofocus` is not an interaction: HTML says the first
    /// element carrying it is focused as the document loads, before
    /// anyone has touched anything. A sign-in form is the usual case,
    /// and rendering it without the focus ring its own stylesheet
    /// draws is a visible difference from every real browser.
    pub fn on_load(document: &Document) -> Self {
        Self {
            focused: document
                .descendants(document.root())
                .find(|&node| autofocuses(document, node)),
            ..Self::default()
        }
    }
}

/// Whether this element asks to be focused on load.
///
/// `autofocus` is a boolean attribute, so its presence is the answer
/// however it was written — but a disabled or hidden control cannot
/// take focus, and neither can an element that is not a control.
fn autofocuses(document: &Document, node: NodeId) -> bool {
    let Some(element) = document.element(node) else {
        return false;
    };
    if element.attr("autofocus").is_none() || element.attr("disabled").is_some() {
        return false;
    }
    matches!(
        element.name.local.as_str(),
        "input" | "textarea" | "select" | "button" | "a"
    )
}

/// Whether `element` matches `selector`.
pub fn matches(doc: &Document, element: NodeId, selector: &Selector, state: &MatchState) -> bool {
    matches_parts(doc, element, &selector.parts, state)
}

/// Match `parts` — innermost first — starting at `element`.
fn matches_parts(
    doc: &Document,
    element: NodeId,
    parts: &[SelectorPart],
    state: &MatchState,
) -> bool {
    let Some((part, rest)) = parts.split_first() else {
        return true;
    };
    if !matches_compound(doc, element, &part.compound, state) {
        return false;
    }
    let Some(combinator) = part.combinator else {
        // The outermost compound matched, so the whole selector has.
        return rest.is_empty();
    };

    match combinator {
        Combinator::Child => {
            element_parent(doc, element).is_some_and(|up| matches_parts(doc, up, rest, state))
        }
        Combinator::NextSibling => previous_element_sibling(doc, element)
            .is_some_and(|prev| matches_parts(doc, prev, rest, state)),
        // Both of these try every candidate: see the module docs.
        Combinator::Descendant => {
            let mut at = element;
            while let Some(up) = element_parent(doc, at) {
                if matches_parts(doc, up, rest, state) {
                    return true;
                }
                at = up;
            }
            false
        }
        Combinator::SubsequentSibling => {
            let mut at = element;
            while let Some(prev) = previous_element_sibling(doc, at) {
                if matches_parts(doc, prev, rest, state) {
                    return true;
                }
                at = prev;
            }
            false
        }
    }
}

/// Whether one element satisfies a compound selector — every piece of
/// it, since a compound is a conjunction.
fn matches_compound(
    doc: &Document,
    element: NodeId,
    compound: &Compound,
    state: &MatchState,
) -> bool {
    let Some(data) = doc.element(element) else {
        return false;
    };
    if let Some(tag) = &compound.tag {
        if !data.name.is_html(tag) {
            return false;
        }
    }
    if let Some(id) = &compound.id {
        if data.id() != Some(id.as_str()) {
            return false;
        }
    }
    if !compound.classes.iter().all(|class| data.has_class(class)) {
        return false;
    }
    if !compound
        .attributes
        .iter()
        .all(|attribute| matches_attribute(data, attribute))
    {
        return false;
    }
    compound
        .pseudo_classes
        .iter()
        .all(|pseudo| matches_pseudo(doc, element, data, pseudo, state))
}

fn matches_attribute(data: &ElementData, selector: &AttributeSelector) -> bool {
    let Some(actual) = data.attr(&selector.name) else {
        return false;
    };
    let equal = |a: &str, b: &str| {
        if selector.case_insensitive {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };

    match &selector.match_kind {
        AttrMatch::Exists => true,
        AttrMatch::Equals(want) => equal(actual, want),
        // An empty operand can never match, for all four of these — a
        // rule the spec states and that `starts_with("")` would get
        // wrong by returning true for everything.
        AttrMatch::Prefix(want) => !want.is_empty() && prefix_matches(actual, want, &equal),
        AttrMatch::Suffix(want) => !want.is_empty() && suffix_matches(actual, want, &equal),
        AttrMatch::Substring(want) => !want.is_empty() && substring_matches(actual, want, selector),
        AttrMatch::Includes(want) => {
            !want.is_empty()
                && actual
                    .split_ascii_whitespace()
                    .any(|piece| equal(piece, want))
        }
        // `[lang|="en"]` matches `en` and `en-GB`, which is how language
        // subtags are meant to be selected.
        AttrMatch::DashMatch(want) => {
            equal(actual, want)
                || (actual.len() > want.len()
                    && actual.as_bytes().get(want.len()) == Some(&b'-')
                    && equal(&actual[..want.len()], want))
        }
    }
}

fn prefix_matches(actual: &str, want: &str, equal: &impl Fn(&str, &str) -> bool) -> bool {
    actual.len() >= want.len() && equal(&actual[..want.len()], want)
}

fn suffix_matches(actual: &str, want: &str, equal: &impl Fn(&str, &str) -> bool) -> bool {
    actual.len() >= want.len() && equal(&actual[actual.len() - want.len()..], want)
}

fn substring_matches(actual: &str, want: &str, selector: &AttributeSelector) -> bool {
    if selector.case_insensitive {
        actual
            .to_ascii_lowercase()
            .contains(&want.to_ascii_lowercase())
    } else {
        actual.contains(want)
    }
}

fn matches_pseudo(
    doc: &Document,
    element: NodeId,
    data: &ElementData,
    pseudo: &PseudoClass,
    state: &MatchState,
) -> bool {
    match pseudo {
        PseudoClass::FirstChild => previous_element_sibling(doc, element).is_none(),
        PseudoClass::LastChild => next_element_sibling(doc, element).is_none(),
        PseudoClass::OnlyChild => {
            previous_element_sibling(doc, element).is_none()
                && next_element_sibling(doc, element).is_none()
        }
        PseudoClass::FirstOfType => sibling_index_of_type(doc, element, Direction::Backward) == 1,
        PseudoClass::LastOfType => sibling_index_of_type(doc, element, Direction::Forward) == 1,
        PseudoClass::OnlyOfType => {
            sibling_index_of_type(doc, element, Direction::Backward) == 1
                && sibling_index_of_type(doc, element, Direction::Forward) == 1
        }
        PseudoClass::NthChild(nth) => matches_nth(doc, element, *nth, Direction::Backward),
        PseudoClass::NthLastChild(nth) => matches_nth(doc, element, *nth, Direction::Forward),
        PseudoClass::NthOfType(nth) => {
            nth.matches(sibling_index_of_type(doc, element, Direction::Backward))
        }
        PseudoClass::NthLastOfType(nth) => {
            nth.matches(sibling_index_of_type(doc, element, Direction::Forward))
        }
        // Whitespace counts as content, so `<p> </p>` is not empty.
        PseudoClass::Empty => !doc.children(element).any(|child| {
            matches!(
                doc.node(child).data,
                NodeData::Element(_) | NodeData::Text(_)
            )
        }),
        PseudoClass::Root => doc
            .node(element)
            .parent()
            .is_some_and(|parent| doc.node(parent).data == NodeData::Document),
        // Nothing here is inside a shadow tree, so nothing is a host.
        // It is parsed rather than rejected so the rule around it
        // survives — see the variant's own note.
        PseudoClass::Host => false,
        PseudoClass::Not(list) => !matches_any(doc, element, list, state),
        PseudoClass::Is(list) | PseudoClass::Where(list) => matches_any(doc, element, list, state),
        PseudoClass::Link => {
            (data.name.is_html("a") || data.name.is_html("area")) && data.has_attr("href")
        }
        PseudoClass::Hover => state.hovered == Some(element),
        PseudoClass::Focus => state.focused == Some(element),
        PseudoClass::Active => state.active == Some(element),
        PseudoClass::Checked => {
            (data.name.is_html("input") && data.has_attr("checked"))
                || (data.name.is_html("option") && data.has_attr("selected"))
        }
        PseudoClass::Disabled => is_form_control(data) && data.has_attr("disabled"),
        PseudoClass::Enabled => is_form_control(data) && !data.has_attr("disabled"),
    }
}

fn matches_any(doc: &Document, element: NodeId, list: &SelectorList, state: &MatchState) -> bool {
    list.selectors
        .iter()
        .any(|selector| matches(doc, element, selector, state))
}

/// The elements `:disabled` and `:enabled` can apply to. Anything else
/// matches neither — a `<div disabled>` is not disabled.
fn is_form_control(data: &ElementData) -> bool {
    [
        "button", "input", "select", "textarea", "optgroup", "option", "fieldset",
    ]
    .iter()
    .any(|name| data.name.is_html(name))
}

/// Which way to count siblings: from the start, or from the end.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Backward,
    Forward,
}

fn matches_nth(doc: &Document, element: NodeId, nth: Nth, direction: Direction) -> bool {
    nth.matches(sibling_index(doc, element, direction))
}

/// This element's 1-based position among its element siblings.
fn sibling_index(doc: &Document, element: NodeId, direction: Direction) -> i32 {
    let mut index = 1;
    let mut at = element;
    loop {
        let next = match direction {
            Direction::Backward => previous_element_sibling(doc, at),
            Direction::Forward => next_element_sibling(doc, at),
        };
        match next {
            Some(sibling) => {
                index += 1;
                at = sibling;
            }
            None => return index,
        }
    }
}

/// This element's 1-based position among siblings of the same tag.
fn sibling_index_of_type(doc: &Document, element: NodeId, direction: Direction) -> i32 {
    let Some(name) = doc.element(element).map(|data| data.name.clone()) else {
        return 0;
    };
    let mut index = 1;
    let mut at = element;
    loop {
        let next = match direction {
            Direction::Backward => previous_element_sibling(doc, at),
            Direction::Forward => next_element_sibling(doc, at),
        };
        let Some(sibling) = next else { return index };
        if doc.element(sibling).is_some_and(|data| data.name == name) {
            index += 1;
        }
        at = sibling;
    }
}

/// The parent, if it is an element. The document root is not one, which
/// is what stops an ancestor walk escaping into it.
fn element_parent(doc: &Document, element: NodeId) -> Option<NodeId> {
    let parent = doc.node(element).parent()?;
    doc.element(parent).map(|_| parent)
}

/// The nearest preceding sibling that is an element, skipping text and
/// comments — `:first-child` ignores the newline before it.
fn previous_element_sibling(doc: &Document, element: NodeId) -> Option<NodeId> {
    let mut at = doc.node(element).prev_sibling();
    while let Some(id) = at {
        if doc.element(id).is_some() {
            return Some(id);
        }
        at = doc.node(id).prev_sibling();
    }
    None
}

fn next_element_sibling(doc: &Document, element: NodeId) -> Option<NodeId> {
    let mut at = doc.node(element).next_sibling();
    while let Some(id) = at {
        if doc.element(id).is_some() {
            return Some(id);
        }
        at = doc.node(id).next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::{parse as parse_css, Rule};
    use crate::engine::cryo_impl::css::selector::parse_selector_list;
    use crate::engine::cryo_impl::html::parse as parse_html;

    /// Parse markup, then report which elements match `selector`, as the
    /// text content of each match.
    fn matched_text(markup: &str, selector: &str) -> Vec<String> {
        matched_text_with(markup, selector, &MatchState::default())
    }

    fn matched_text_with(markup: &str, selector: &str, state: &MatchState) -> Vec<String> {
        let doc = parse_html(markup);
        let sheet = parse_css(&format!("{selector} {{ color: red }}"));
        let Some(Rule::Style(rule)) = sheet.rules.first() else {
            panic!("{selector} did not parse as a rule");
        };
        let list = parse_selector_list(&rule.prelude)
            .unwrap_or_else(|| panic!("{selector} is not a valid selector"));

        doc.descendants(doc.root())
            .filter(|&id| {
                doc.element(id).is_some()
                    && list.selectors.iter().any(|s| matches(&doc, id, s, state))
            })
            .map(|id| doc.text_content(id))
            .collect()
    }

    /// The ids of matching elements, for cases where text is ambiguous.
    fn matched_ids(markup: &str, selector: &str) -> Vec<String> {
        let doc = parse_html(markup);
        let sheet = parse_css(&format!("{selector} {{ color: red }}"));
        let Some(Rule::Style(rule)) = sheet.rules.first() else {
            panic!("{selector} did not parse");
        };
        let list = parse_selector_list(&rule.prelude).expect("valid selector");
        let state = MatchState::default();
        doc.descendants(doc.root())
            .filter_map(|id| {
                let data = doc.element(id)?;
                let matched = list.selectors.iter().any(|s| matches(&doc, id, s, &state));
                matched.then(|| data.id().unwrap_or("?").to_owned())
            })
            .collect()
    }

    #[test]
    fn a_type_selector_matches_every_such_element() {
        assert_eq!(
            matched_text("<p>one</p><div>two</div><p>three</p>", "p"),
            ["one", "three"]
        );
    }

    #[test]
    fn type_selectors_ignore_case_in_html() {
        assert_eq!(matched_text("<P>x</P>", "p"), ["x"]);
        assert_eq!(matched_text("<p>x</p>", "P"), ["x"]);
    }

    #[test]
    fn the_universal_selector_matches_everything() {
        // html, head, body, p — four elements.
        assert_eq!(matched_text("<p>x</p>", "*").len(), 4);
    }

    #[test]
    fn class_and_id_selectors_match() {
        let markup = "<p class='a b' id=first>one</p><p class=b>two</p>";
        assert_eq!(matched_text(markup, ".a"), ["one"]);
        assert_eq!(matched_text(markup, ".b"), ["one", "two"]);
        assert_eq!(matched_text(markup, ".a.b"), ["one"]);
        assert_eq!(matched_text(markup, "#first"), ["one"]);
    }

    #[test]
    fn class_and_id_matching_is_case_sensitive() {
        assert_eq!(
            matched_text("<p class=Card>x</p>", ".card"),
            [] as [String; 0]
        );
        assert_eq!(matched_text("<p class=Card>x</p>", ".Card"), ["x"]);
    }

    #[test]
    fn the_child_combinator_requires_the_direct_parent() {
        let markup = "<div><p>direct</p><section><p>nested</p></section></div>";
        assert_eq!(matched_text(markup, "div > p"), ["direct"]);
        assert_eq!(matched_text(markup, "div p"), ["direct", "nested"]);
    }

    #[test]
    fn the_descendant_combinator_backtracks() {
        // The case a greedy matcher gets wrong: the `p`'s nearest `.y`
        // ancestor has no `.x` parent, but an outer `.y` does.
        let markup = "<div class=x><div class=y><div class=y><p>hit</p></div></div></div>";
        assert_eq!(matched_text(markup, ".x > .y p"), ["hit"]);
    }

    #[test]
    fn the_sibling_combinator_backtracks_too() {
        let markup = "<i id=a></i><i id=b></i><i id=c></i><p id=target></p>";
        // `#a ~ i + p` needs the `i` to be `#b` or later, with `#a` before.
        assert_eq!(matched_ids(markup, "#a ~ i + p"), ["target"]);
    }

    #[test]
    fn the_next_sibling_combinator_needs_adjacency() {
        let markup = "<h1>h</h1><p>first</p><p>second</p>";
        assert_eq!(matched_text(markup, "h1 + p"), ["first"]);
        assert_eq!(matched_text(markup, "h1 ~ p"), ["first", "second"]);
    }

    #[test]
    fn siblings_skip_text_and_comments() {
        let markup = "<div><!--c--> <p>only</p> </div>";
        assert_eq!(matched_text(markup, "p:first-child"), ["only"]);
        assert_eq!(matched_text(markup, "p:last-child"), ["only"]);
        assert_eq!(matched_text(markup, "p:only-child"), ["only"]);
    }

    #[test]
    fn structural_pseudo_classes_count_element_siblings() {
        let markup = "<ul><li>a</li><li>b</li><li>c</li></ul>";
        assert_eq!(matched_text(markup, "li:first-child"), ["a"]);
        assert_eq!(matched_text(markup, "li:last-child"), ["c"]);
        assert_eq!(matched_text(markup, "li:nth-child(2)"), ["b"]);
        assert_eq!(matched_text(markup, "li:nth-last-child(1)"), ["c"]);
        assert_eq!(matched_text(markup, "li:nth-child(odd)"), ["a", "c"]);
        assert_eq!(matched_text(markup, "li:nth-child(even)"), ["b"]);
        assert_eq!(matched_text(markup, "li:nth-child(-n+2)"), ["a", "b"]);
    }

    #[test]
    fn of_type_pseudo_classes_count_only_matching_tags() {
        let markup = "<div><span>s1</span><p>p1</p><span>s2</span><p>p2</p></div>";
        assert_eq!(matched_text(markup, "p:first-of-type"), ["p1"]);
        assert_eq!(matched_text(markup, "p:last-of-type"), ["p2"]);
        assert_eq!(matched_text(markup, "span:nth-of-type(2)"), ["s2"]);
        assert_eq!(matched_text(markup, "p:nth-last-of-type(2)"), ["p1"]);
    }

    #[test]
    fn only_of_type_needs_a_unique_tag_among_siblings() {
        let markup = "<div><span>s</span><p>p1</p><p>p2</p></div>";
        assert_eq!(matched_text(markup, "span:only-of-type"), ["s"]);
        assert_eq!(matched_text(markup, "p:only-of-type"), [] as [String; 0]);
    }

    #[test]
    fn empty_treats_whitespace_as_content() {
        let markup = "<p id=a></p><p id=b> </p><p id=c><i></i></p>";
        assert_eq!(matched_ids(markup, "p:empty"), ["a"]);
        assert_eq!(matched_ids(markup, "i:empty"), ["?"]);
    }

    #[test]
    fn root_matches_only_the_document_element() {
        assert_eq!(matched_text("<p>x</p>", ":root").len(), 1);
        let doc = parse_html("<p>x</p>");
        let html = doc.first_html_element("html").expect("html exists");
        let sheet = parse_css(":root { color: red }");
        let Some(Rule::Style(rule)) = sheet.rules.first() else {
            panic!("no rule");
        };
        let list = parse_selector_list(&rule.prelude).expect("valid");
        assert!(matches(
            &doc,
            html,
            &list.selectors[0],
            &MatchState::default()
        ));
    }

    #[test]
    fn attribute_selectors_match_their_operators() {
        let markup = concat!(
            "<a id=1 href='https://a.test/x.png'>one</a>",
            "<a id=2 href='http://b.test/y'>two</a>",
            "<a id=3>three</a>",
        );
        assert_eq!(matched_ids(markup, "a[href]"), ["1", "2"]);
        assert_eq!(matched_ids(markup, "a[href^='https']"), ["1"]);
        assert_eq!(matched_ids(markup, "a[href$='.png']"), ["1"]);
        assert_eq!(matched_ids(markup, "a[href*='b.test']"), ["2"]);
        assert_eq!(matched_ids(markup, "a[href='http://b.test/y']"), ["2"]);
    }

    #[test]
    fn an_empty_attribute_operand_never_matches() {
        // `starts_with("")` would be true for everything.
        let markup = "<a href='x'>one</a>";
        assert_eq!(matched_text(markup, "a[href^='']"), [] as [String; 0]);
        assert_eq!(matched_text(markup, "a[href$='']"), [] as [String; 0]);
        assert_eq!(matched_text(markup, "a[href*='']"), [] as [String; 0]);
    }

    #[test]
    fn includes_matches_one_word_of_a_list() {
        let markup = "<p class='alpha beta'>x</p>";
        assert_eq!(matched_text(markup, "[class~='beta']"), ["x"]);
        assert_eq!(matched_text(markup, "[class~='bet']"), [] as [String; 0]);
    }

    #[test]
    fn dash_match_selects_language_subtags() {
        let markup = "<p id=1 lang=en>a</p><p id=2 lang=en-GB>b</p><p id=3 lang=england>c</p>";
        assert_eq!(matched_ids(markup, "[lang|='en']"), ["1", "2"]);
    }

    #[test]
    fn the_case_insensitive_flag_applies_to_the_value() {
        let markup = "<a href='HTTPS://x'>one</a>";
        assert_eq!(matched_text(markup, "a[href^='https']"), [] as [String; 0]);
        assert_eq!(matched_text(markup, "a[href^='https' i]"), ["one"]);
        assert_eq!(matched_text(markup, "a[href*='TTPS' i]"), ["one"]);
    }

    #[test]
    fn not_inverts_a_match() {
        let markup = "<ul><li class=skip>a</li><li>b</li></ul>";
        assert_eq!(matched_text(markup, "li:not(.skip)"), ["b"]);
        assert_eq!(matched_text(markup, "li:not(.nothing)"), ["a", "b"]);
    }

    #[test]
    fn is_and_where_match_any_argument() {
        let markup = "<h1>a</h1><h2>b</h2><h3>c</h3>";
        assert_eq!(matched_text(markup, ":is(h1, h3)"), ["a", "c"]);
        assert_eq!(matched_text(markup, ":where(h2)"), ["b"]);
    }

    #[test]
    fn not_accepts_a_compound_argument() {
        let markup = "<p id=1 class=a>one</p><p id=2>two</p>";
        assert_eq!(matched_ids(markup, "p:not(p.a)"), ["2"]);
    }

    #[test]
    fn link_needs_an_href() {
        let markup = "<a href=x>yes</a><a>no</a>";
        assert_eq!(matched_text(markup, "a:link"), ["yes"]);
    }

    #[test]
    fn checked_and_disabled_read_their_attributes() {
        let markup = "<input id=1 checked><input id=2 disabled><input id=3>";
        assert_eq!(matched_ids(markup, "input:checked"), ["1"]);
        assert_eq!(matched_ids(markup, "input:disabled"), ["2"]);
        assert_eq!(matched_ids(markup, "input:enabled"), ["1", "3"]);
    }

    #[test]
    fn disabled_does_not_apply_to_arbitrary_elements() {
        assert_eq!(
            matched_text("<div disabled>x</div>", "div:disabled"),
            [] as [String; 0]
        );
    }

    #[test]
    fn hover_matches_nothing_by_default_and_the_hovered_node_when_set() {
        let markup = "<a href=x>link</a>";
        assert_eq!(matched_text(markup, "a:hover"), [] as [String; 0]);

        let doc = parse_html(markup);
        let anchor = doc.first_html_element("a").expect("the anchor exists");
        let state = MatchState {
            hovered: Some(anchor),
            ..MatchState::default()
        };
        assert_eq!(matched_text_with(markup, "a:hover", &state), ["link"]);
    }

    #[test]
    fn a_long_chain_matches_end_to_end() {
        let markup = concat!(
            "<div id=page><main><section class=cards>",
            "<article class=card><h2 class=title>Title</h2></article>",
            "</section></main></div>",
        );
        assert_eq!(
            matched_text(markup, "#page main .cards > .card h2.title"),
            ["Title"]
        );
    }

    #[test]
    fn a_chain_that_almost_matches_does_not() {
        let markup = "<div id=page><section class=cards><h2>x</h2></section></div>";
        assert_eq!(
            matched_text(markup, "#page .cards > .card h2"),
            [] as [String; 0]
        );
    }

    #[test]
    fn a_pseudo_element_rule_does_not_style_the_element_itself() {
        // `.card::after { position: absolute }` must not make the
        // card absolute. The generated box is not drawn yet, so the
        // rule applies to nothing — which is the right nothing.
        let doc = parse_html("<div class=card>a</div>");
        let list = parse_selector_list(
            &match parse_css(".card::after { color: red }").rules.first() {
                Some(Rule::Style(rule)) => rule.prelude.clone(),
                _ => panic!("not a style rule"),
            },
        )
        .expect("it parses");
        let card = doc
            .descendants(doc.root())
            .find(|&id| doc.element(id).is_some_and(|e| e.has_class("card")))
            .expect("the card");
        assert_eq!(
            list.match_specificity(&doc, card, &MatchState::default()),
            None,
            "the rule selects a box the element does not own"
        );
    }

    #[test]
    fn host_matches_nothing_in_a_document_with_no_shadow_trees() {
        // It parses so the rule survives; it must not then match the
        // root and apply a shadow tree's styles to the page.
        assert!(matched_text("<p>a</p>", ":host").is_empty());
        // The `p` still matches, so the list itself is live — only
        // the `:host` member found nothing.
        assert_eq!(matched_text("<p>a</p>", ":host, p"), ["a"]);
    }

    #[test]
    fn autofocus_is_focused_the_moment_the_page_loads() {
        let doc = parse_html("<form><input id=user autofocus><input id=pass></form>");
        let state = MatchState::on_load(&doc);
        let focused = state.focused.expect("the autofocus input");
        assert_eq!(doc.element(focused).and_then(|e| e.id()), Some("user"));
    }

    #[test]
    fn only_the_first_autofocus_wins() {
        // A page may carry more than one by mistake; focus is single.
        let doc = parse_html("<input id=a autofocus><input id=b autofocus>");
        let focused = MatchState::on_load(&doc).focused.expect("one of them");
        assert_eq!(doc.element(focused).and_then(|e| e.id()), Some("a"));
    }

    #[test]
    fn a_disabled_control_does_not_take_focus() {
        let doc = parse_html("<input id=a autofocus disabled><input id=b autofocus>");
        let focused = MatchState::on_load(&doc).focused.expect("the enabled one");
        assert_eq!(doc.element(focused).and_then(|e| e.id()), Some("b"));
    }

    #[test]
    fn a_page_with_no_autofocus_loads_with_nothing_focused() {
        let doc = parse_html("<div autofocus><input id=a></div>");
        assert_eq!(MatchState::on_load(&doc).focused, None);
    }

    #[test]
    fn the_focus_pseudo_class_matches_what_on_load_focused() {
        // The whole point of `on_load`: a sign-in form's own
        // `:focus` rule should draw its ring without anyone clicking.
        // An input holds no text, so the sibling after it stands in
        // for which one matched.
        let markup = "<input autofocus><span>user</span><input><span>pass</span>";
        let doc = parse_html(markup);
        assert_eq!(
            matched_text_with(markup, "input:focus + span", &MatchState::on_load(&doc)),
            ["user"]
        );
        assert!(
            matched_text(markup, "input:focus + span").is_empty(),
            "and the default state still focuses nothing"
        );
    }
}
