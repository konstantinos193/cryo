//! Selectors: what a rule applies to, and how strongly.
//!
//! A selector is read right to left. `div p > a` is "an `a` whose parent
//! is a `p` that has a `div` somewhere above it", and matching starts
//! from the `a` because that is the element being styled — walking
//! leftward only as far as the first failure. Left-to-right matching
//! would mean searching the whole subtree for every rule.
//!
//! Specificity is the other half. When two rules set the same property,
//! the more specific selector wins, counted as three numbers: ids, then
//! class-like things, then element-like things. `#main` beats
//! `.a.b.c.d`, no matter how many classes you pile up — the comparison
//! is lexicographic, not a sum, which is why specificity cannot be
//! "earned" by repetition.
//!
//! Layout here: this file owns the types and specificity, [`parse`]
//! turns component values into them, and [`matching`] answers whether an
//! element matches.

pub mod matching;
pub mod parse;

pub use matching::{matches, MatchState};
pub use parse::parse_selector_list;

use crate::engine::cryo_impl::dom::NodeId;

/// A comma-separated list of selectors, as written in one rule's
/// prelude. The rule applies to an element if *any* of them matches, and
/// the winning one's specificity is the one that counts.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SelectorList {
    pub selectors: Vec<Selector>,
}

impl SelectorList {
    /// The highest specificity among the selectors that match — `None`
    /// when none do.
    ///
    /// The cascade needs the *matching* selector's specificity, not the
    /// list's maximum: in `#main, p { … }` a plain `<p>` is matched at
    /// element specificity, not id specificity.
    pub fn match_specificity(
        &self,
        doc: &crate::engine::cryo_impl::dom::Document,
        element: NodeId,
        state: &MatchState,
    ) -> Option<Specificity> {
        self.selectors
            .iter()
            // A selector ending in a pseudo-element styles a box this
            // engine does not generate, so it styles nothing. Letting
            // it through instead applies those declarations to the
            // *originating* element, which is not a near miss: a
            // decorative `.card::after { position: absolute; inset: 0 }`
            // then takes the card itself out of flow, and the section
            // it was meant to shine across collapses to nothing.
            .filter(|selector| selector.pseudo_element.is_none())
            .filter(|selector| matches(doc, element, selector, state))
            .map(Selector::specificity)
            .max()
    }
}

/// One complex selector: a chain of compound selectors joined by
/// combinators, stored right to left.
///
/// Reversed storage is not an optimisation detail to hide — it is the
/// order matching runs in, so keeping it reversed means the matcher
/// never indexes backwards and can stop at the first failure.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    /// Innermost compound first. The combinator on each link says how it
    /// relates to the *next* one out.
    pub parts: Vec<SelectorPart>,
    /// `::before` and friends, which do not affect what element matches.
    pub pseudo_element: Option<PseudoElement>,
}

/// One compound selector plus how it attaches to the one outside it.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectorPart {
    pub compound: Compound,
    /// How to reach the next part outward. `None` on the outermost.
    pub combinator: Option<Combinator>,
}

/// How two compound selectors relate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    /// A space: any ancestor.
    Descendant,
    /// `>`: the parent.
    Child,
    /// `+`: the element immediately before.
    NextSibling,
    /// `~`: any element before, same parent.
    SubsequentSibling,
}

/// A compound selector: everything that must hold of one element, with
/// no combinator inside it — `a.link#main[href]:hover`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Compound {
    /// `None` for `*`.
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attributes: Vec<AttributeSelector>,
    pub pseudo_classes: Vec<PseudoClass>,
}

/// An attribute selector: `[href]`, `[href^="https"]`.
#[derive(Clone, Debug, PartialEq)]
pub struct AttributeSelector {
    pub name: String,
    pub match_kind: AttrMatch,
    /// `[href="x" i]` — compare the value case-insensitively.
    pub case_insensitive: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttrMatch {
    /// `[href]` — present with any value.
    Exists,
    /// `[href="x"]`
    Equals(String),
    /// `[class~="x"]` — one of a whitespace-separated list.
    Includes(String),
    /// `[lang|="en"]` — exactly `en`, or starting `en-`.
    DashMatch(String),
    /// `[href^="x"]`
    Prefix(String),
    /// `[href$="x"]`
    Suffix(String),
    /// `[href*="x"]`
    Substring(String),
}

/// `an+b`, as `:nth-child()` and friends take it.
///
/// Stored as the coefficients rather than a predicate so that `2n+1`,
/// `odd` and `even` all become the same thing and the matcher has one
/// case to handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nth {
    pub step: i32,
    pub offset: i32,
}

impl Nth {
    /// Whether a 1-based index is selected.
    ///
    /// With no step, only the exact offset matches. With one, the index
    /// must be `offset + step * k` for some non-negative `k` — so
    /// `2n+1` is the odd positions and `-n+3` is the first three.
    pub fn matches(self, index: i32) -> bool {
        if self.step == 0 {
            return index == self.offset;
        }
        let distance = index - self.offset;
        distance % self.step == 0 && distance / self.step >= 0
    }
}

/// A pseudo-class.
///
/// Split by what answers them: structural ones the tree can answer, and
/// state ones that need [`MatchState`] because the DOM alone cannot know
/// what the pointer is over.
#[derive(Clone, Debug, PartialEq)]
pub enum PseudoClass {
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    NthChild(Nth),
    NthLastChild(Nth),
    NthOfType(Nth),
    NthLastOfType(Nth),
    /// No element or text children at all.
    Empty,
    /// The document element.
    Root,
    /// The host of a shadow tree.
    ///
    /// There are no shadow trees here, so it matches nothing — but it
    /// has to *parse*, because an unknown selector invalidates the
    /// whole rule it is in and `:root, :host { … }` is how Tailwind v4
    /// and every design system built on custom properties declares its
    /// theme. Dropping that one rule costs a page every colour,
    /// spacing step and font it owns.
    Host,
    /// `:not(...)` — matches when none of the inner selectors do.
    Not(SelectorList),
    /// `:is(...)` / `:matches(...)` — matches when any does, taking the
    /// specificity of its most specific argument.
    Is(SelectorList),
    /// `:where(...)` — like `:is`, but contributes no specificity.
    Where(SelectorList),
    /// An `<a>` or `<area>` with an `href`.
    Link,
    Hover,
    Focus,
    Active,
    Checked,
    Disabled,
    Enabled,
}

/// A pseudo-element. It does not change which element matches; it says
/// which generated box the declarations apply to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
    FirstLine,
    FirstLetter,
    Marker,
    Placeholder,
    Selection,
}

/// A selector's specificity: ids, then class-like, then element-like.
///
/// Compared lexicographically, which `Ord` on a tuple-shaped struct
/// gives for free — and which is the whole point, since no number of
/// classes ever outweighs one id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Specificity {
    pub ids: u32,
    pub classes: u32,
    pub elements: u32,
}

impl Specificity {
    pub const fn new(ids: u32, classes: u32, elements: u32) -> Self {
        Self {
            ids,
            classes,
            elements,
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            ids: self.ids + other.ids,
            classes: self.classes + other.classes,
            elements: self.elements + other.elements,
        }
    }
}

impl Selector {
    /// This selector's specificity, summed over its compounds.
    pub fn specificity(&self) -> Specificity {
        let base = self
            .parts
            .iter()
            .fold(Specificity::default(), |total, part| {
                total.add(part.compound.specificity())
            });
        match self.pseudo_element {
            // A pseudo-element counts as an element.
            Some(_) => base.add(Specificity::new(0, 0, 1)),
            None => base,
        }
    }
}

impl Compound {
    fn specificity(&self) -> Specificity {
        let tag = u32::from(self.tag.is_some());
        let id = u32::from(self.id.is_some());
        let classes = self.classes.len() as u32 + self.attributes.len() as u32;
        let base = Specificity::new(id, classes, tag);
        self.pseudo_classes
            .iter()
            .fold(base, |total, pseudo| total.add(pseudo.specificity()))
    }
}

impl PseudoClass {
    /// What this pseudo-class contributes.
    ///
    /// The functional ones take their argument's specificity rather than
    /// a flat value: `:not(#main)` is as specific as `#main`. `:where()`
    /// is the deliberate exception, and the reason it exists — it lets a
    /// library ship defaults that any author rule can override.
    fn specificity(&self) -> Specificity {
        match self {
            Self::Not(list) | Self::Is(list) => list
                .selectors
                .iter()
                .map(Selector::specificity)
                .max()
                .unwrap_or_default(),
            Self::Where(_) => Specificity::default(),
            _ => Specificity::new(0, 1, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specificity_compares_lexicographically_not_by_sum() {
        let one_id = Specificity::new(1, 0, 0);
        let many_classes = Specificity::new(0, 10, 10);
        assert!(
            one_id > many_classes,
            "an id outranks any number of classes"
        );
    }

    #[test]
    fn specificity_orders_within_a_rank() {
        assert!(Specificity::new(0, 2, 0) > Specificity::new(0, 1, 5));
        assert!(Specificity::new(0, 0, 2) > Specificity::new(0, 0, 1));
        assert_eq!(Specificity::new(1, 1, 1), Specificity::new(1, 1, 1));
    }

    #[test]
    fn an_exact_nth_matches_only_its_index() {
        let third = Nth { step: 0, offset: 3 };
        assert!(third.matches(3));
        assert!(!third.matches(1));
        assert!(!third.matches(6));
    }

    #[test]
    fn a_stepping_nth_matches_an_arithmetic_series() {
        // `2n+1` — the odd positions.
        let odd = Nth { step: 2, offset: 1 };
        assert!(odd.matches(1));
        assert!(odd.matches(3));
        assert!(!odd.matches(2));

        // `2n` — the even ones. Index 0 does not exist, so only 2, 4, ...
        let even = Nth { step: 2, offset: 0 };
        assert!(even.matches(2));
        assert!(even.matches(4));
        assert!(!even.matches(1));
    }

    #[test]
    fn a_negative_step_selects_a_prefix() {
        // `-n+3` is the first three elements and nothing after.
        let first_three = Nth {
            step: -1,
            offset: 3,
        };
        assert!(first_three.matches(1));
        assert!(first_three.matches(3));
        assert!(!first_three.matches(4));
    }

    #[test]
    fn nth_never_matches_a_nonexistent_index() {
        let every = Nth { step: 1, offset: 0 };
        assert!(every.matches(1));
        assert!(every.matches(99));
        // `n+0` with index 0: there is no zeroth child.
        assert!(every.matches(0), "the arithmetic admits it");
        // ...which is why the matcher only ever passes 1-based indices.
    }

    #[test]
    fn where_contributes_no_specificity_but_is_does() {
        let inner = SelectorList {
            selectors: vec![Selector {
                parts: vec![SelectorPart {
                    compound: Compound {
                        id: Some("main".into()),
                        ..Compound::default()
                    },
                    combinator: None,
                }],
                pseudo_element: None,
            }],
        };
        assert_eq!(
            PseudoClass::Is(inner.clone()).specificity(),
            Specificity::new(1, 0, 0)
        );
        assert_eq!(
            PseudoClass::Not(inner.clone()).specificity(),
            Specificity::new(1, 0, 0)
        );
        assert_eq!(
            PseudoClass::Where(inner).specificity(),
            Specificity::default()
        );
    }

    #[test]
    fn a_plain_pseudo_class_counts_as_a_class() {
        assert_eq!(PseudoClass::Hover.specificity(), Specificity::new(0, 1, 0));
        assert_eq!(
            PseudoClass::FirstChild.specificity(),
            Specificity::new(0, 1, 0)
        );
    }
}
