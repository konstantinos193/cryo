//! The cascade: of every declaration that reaches an element, which one
//! wins.
//!
//! Four things decide it, in strict order:
//!
//! 1. **Origin and importance together**, as one ranking. This is the
//!    part people get wrong, because importance *inverts* the origin
//!    order rather than acting within it:
//!
//!    ```text
//!      user-agent normal   (weakest)
//!      user normal
//!      author normal
//!      author important
//!      user important
//!      user-agent important   (strongest)
//!    ```
//!
//!    So `!important` in a user stylesheet beats `!important` in the
//!    page — which is what makes accessibility overrides possible, and
//!    why this cannot be modelled as "origin, then importance".
//! 2. **Inline style**, above any selector within the same band. It has
//!    no specificity of its own; it simply outranks.
//! 3. **Specificity** of the selector that actually matched.
//! 4. **Source order** — later wins.
//!
//! Each step only breaks ties left by the one before, so the whole thing
//! is a sort key, and the winner per property is whatever sorts last.
//!
//! What this stage deliberately does not do is interpret values. It
//! answers "which declaration applies" and hands on the raw component
//! values; turning `1em` into pixels, filling in inherited and initial
//! values, and expanding shorthands are the next stage's job. One
//! consequence worth knowing: `margin` and `margin-left` are treated as
//! unrelated properties here, because shorthand expansion has not
//! happened yet.
//!
//! Group at-rules — `@media`, `@supports`, `@layer` — are stepped
//! into when their condition holds, so the rules inside take their
//! place in document order like any other. Skipping them is not a
//! rounding error: a stylesheet built by a modern toolchain keeps
//! nearly everything it has inside a `@layer`.
//!
//! Cascade *layers* are not implemented, only entered. A layer changes
//! precedence, and the ordering rules for that are their own piece of
//! work; until then a layered rule competes on origin and specificity
//! like an unlayered one.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

use super::custom::{self, CustomProperties};
use super::font_face::FontFace;
use super::math;
use super::media::{self, Viewport};
use super::parser::{AtBlock, AtRule, ComponentValue, Declaration, Rule, StyleRule, Stylesheet};
use super::selector::{parse_selector_list, MatchState, SelectorList, Specificity};
use super::shorthand;
use super::tokenizer::CssToken;
use super::value::LengthContext;
use crate::engine::cryo_impl::dom::{Document, NodeId};

/// Where a stylesheet came from. Not an ordering on its own — see
/// [`Origin::band`], which folds in importance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The engine's own default sheet: what `<p>` means before any page
    /// says anything.
    UserAgent,
    /// The person's own overrides.
    User,
    /// The page.
    Author,
}

impl Origin {
    /// This origin's precedence band at the given importance. Higher
    /// wins; see the module docs for why important inverts the order.
    const fn band(self, important: bool) -> u8 {
        match (self, important) {
            (Self::UserAgent, false) => 0,
            (Self::User, false) => 1,
            (Self::Author, false) => 2,
            (Self::Author, true) => 3,
            (Self::User, true) => 4,
            (Self::UserAgent, true) => 5,
        }
    }
}

/// A rule with its selectors already parsed.
///
/// Parsed once at insertion rather than per element: a sheet is matched
/// against every element in the document, so re-parsing selectors in the
/// matching loop would be the single most expensive thing here.
struct PreparedRule {
    selectors: SelectorList,
    declarations: Vec<Declaration>,
    origin: Origin,
    /// Position across all inserted sheets, which is what "later in the
    /// source" means once several sheets are in play.
    order: usize,
}

/// Every rule that could apply to a document, from every origin.
#[derive(Default)]
pub struct RuleSet {
    rules: Vec<PreparedRule>,
    /// The fonts the page brings with it, in the order declared.
    ///
    /// Collected here because this is what walks every rule of every
    /// sheet, including the ones nested inside a layer or a media
    /// query — and a page that declares its webfonts inside a layer,
    /// as a toolchain does, must not lose them.
    font_faces: Vec<FontFace>,
    /// Where the sheet being inserted was fetched from.
    sheet_base: String,
    /// What this set's `@media` queries are asked about.
    viewport: Viewport,
}

/// Put a declaration in, unless something stronger is already there.
fn place(values: &mut BTreeMap<String, Won>, name: String, value: Vec<ComponentValue>, rank: Rank) {
    match values.entry(name) {
        Entry::Occupied(mut slot) if slot.get().rank < rank => {
            let beaten = slot.insert(Won {
                value,
                rank,
                runner_up: None,
            });
            // Keep the strongest it displaced, so an unparseable
            // winner can still give way to it.
            slot.get_mut().runner_up = Some(beaten.value);
        }
        Entry::Occupied(mut slot) => {
            // Weaker than the winner, but it may still be the best
            // thing left if the winner cannot be read.
            if slot.get().runner_up.is_none() {
                slot.get_mut().runner_up = Some(value);
            }
        }
        Entry::Vacant(slot) => {
            slot.insert(Won {
                value,
                rank,
                runner_up: None,
            });
        }
    }
}

/// Whether a `@supports` prelude starts with `not`.
fn supports_is_negated(prelude: &[ComponentValue]) -> bool {
    prelude
        .iter()
        .find(|value| !matches!(value, ComponentValue::Token(CssToken::Whitespace)))
        .is_some_and(|value| {
            matches!(value, ComponentValue::Token(CssToken::Ident(word)) if word.eq_ignore_ascii_case("not"))
        })
}

impl RuleSet {
    /// A rule set for the default viewport. See [`Viewport::DEFAULT`].
    pub fn new() -> Self {
        Self::default()
    }

    /// A rule set whose `@media` queries are asked about this viewport.
    ///
    /// The viewport is fixed when the rules are loaded rather than when
    /// they are matched, so changing it means loading the sheets again.
    /// That is the honest shape for now: nothing resizes yet.
    pub fn for_viewport(viewport: Viewport) -> Self {
        Self {
            viewport,
            ..Self::default()
        }
    }

    /// Add a parsed stylesheet's rules.
    ///
    /// Rules whose selectors do not parse are dropped here, which is the
    /// one place that discarding happens — so nothing downstream has to
    /// carry a "maybe invalid" selector around.
    pub fn insert(&mut self, origin: Origin, sheet: &Stylesheet) {
        self.insert_from(origin, sheet, "");
    }

    /// The same, for a sheet fetched from `base`.
    ///
    /// The address is kept because a `url()` inside a sheet is
    /// relative to *the sheet*. There are two entry points rather
    /// than one with a default because there are genuinely two
    /// cases: a user-agent sheet has no address at all.
    pub fn insert_from(&mut self, origin: Origin, sheet: &Stylesheet, base: &str) {
        self.sheet_base = base.to_owned();
        self.insert_rules(origin, &sheet.rules);
    }

    /// Add a list of rules, stepping into any group at-rule whose
    /// condition holds.
    fn insert_rules(&mut self, origin: Origin, rules: &[Rule]) {
        for rule in rules {
            match rule {
                Rule::Style(style) => self.insert_style(origin, style),
                Rule::At(at_rule) => match &at_rule.block {
                    Some(AtBlock::Rules(inner)) if self.condition_holds(at_rule) => {
                        self.insert_rules(origin, inner);
                    }
                    Some(AtBlock::Declarations(declarations)) if at_rule.name == "font-face" => {
                        let base = self.sheet_base.clone();
                        self.font_faces.extend(FontFace::parse(declarations, &base));
                    }
                    _ => {}
                },
            }
        }
    }

    /// Whether a group at-rule's contents apply.
    ///
    /// `@layer` always does. Cascade layers change *precedence*, not
    /// whether a rule exists, and that ordering is not implemented —
    /// so its rules take their place in document order. Skipping them
    /// instead would discard most of any stylesheet built by a modern
    /// toolchain, which puts nearly everything inside a layer.
    fn condition_holds(&self, at_rule: &AtRule) -> bool {
        match at_rule.name.as_str() {
            "layer" => true,
            "media" => media::matches(&at_rule.prelude, self.viewport),
            // `@supports` is answered yes unless it is negated. A real
            // answer means asking whether a declaration parses, which
            // no property here can be asked. Yes is the safer wrong
            // answer: the modern branch is the one a page is designed
            // around, and declarations inside it that we cannot parse
            // are dropped one by one anyway.
            "supports" => !supports_is_negated(&at_rule.prelude),
            // `@container` and `@scope` need layout and a scope root.
            _ => false,
        }
    }

    fn insert_style(&mut self, origin: Origin, style: &StyleRule) {
        let Some(selectors) = parse_selector_list(&style.prelude) else {
            return;
        };
        let order = self.rules.len();
        self.rules.push(PreparedRule {
            selectors,
            // Expanded here so the cascade only ever sees longhands:
            // `margin: 0; margin-left: 5px` has to leave the left
            // margin alone, which only works per-longhand.
            declarations: shorthand::expand_all(&style.declarations),
            origin,
            order,
        });
    }

    /// Parse and add a stylesheet in one step.
    pub fn insert_css(&mut self, origin: Origin, css: &str) {
        self.insert(origin, &super::parser::parse(css));
    }

    /// Parse and add a sheet fetched from `base`.
    pub fn insert_css_from(&mut self, origin: Origin, css: &str, base: &str) {
        self.insert_from(origin, &super::parser::parse(css), base);
    }

    /// How many rules are loaded. Rules dropped for an unparseable
    /// selector are not counted, which is what makes this worth
    /// exposing: it is how a caller sees that a sheet had bad selectors.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The `@font-face` rules every inserted sheet declared.
    pub fn font_faces(&self) -> &[FontFace] {
        &self.font_faces
    }

    /// The winning declaration for each property on `element`.
    ///
    /// `inline` is the element's `style` attribute, already parsed; pass
    /// an empty slice when it has none.
    pub fn declared_values(
        &self,
        doc: &Document,
        element: NodeId,
        inline: &[Declaration],
        state: &MatchState,
    ) -> DeclaredValues {
        let mut candidates: Vec<Candidate<'_>> = Vec::new();

        for rule in &self.rules {
            let Some(specificity) = rule.selectors.match_specificity(doc, element, state) else {
                continue;
            };
            for (position, declaration) in rule.declarations.iter().enumerate() {
                candidates.push(Candidate {
                    declaration,
                    rank: Rank {
                        band: rule.origin.band(declaration.important),
                        inline: false,
                        specificity,
                        order: rule.order,
                        position,
                    },
                });
            }
        }

        // A `style` attribute holds shorthands too.
        let inline = shorthand::expand_all(inline);
        for (position, declaration) in inline.iter().enumerate() {
            candidates.push(Candidate {
                declaration,
                rank: Rank {
                    band: Origin::Author.band(declaration.important),
                    inline: true,
                    // Inline style has no specificity; the flag above is
                    // what makes it outrank selectors in its band.
                    specificity: Specificity::default(),
                    order: 0,
                    position,
                },
            });
        }

        // Ascending, so the winner sorts last. A stable sort keeps two
        // equally-ranked inline declarations in source order.
        candidates.sort_by(|left, right| left.rank.cmp(&right.rank));

        let mut values: BTreeMap<String, Won> = BTreeMap::new();
        for candidate in candidates {
            // Through `place`, so the one each winner displaced is
            // kept behind it — see `candidates` on why that matters.
            place(
                &mut values,
                candidate.declaration.name.clone(),
                candidate.declaration.value.clone(),
                candidate.rank,
            );
        }
        DeclaredValues { values }
    }
}

/// A declaration's position in the cascade. Field order *is* the
/// precedence order, which is what `derive(Ord)` turns into the
/// comparison — so the ordering cannot drift from the documented rules
/// without someone reordering these fields on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    band: u8,
    inline: bool,
    specificity: Specificity,
    /// Which rule it came from.
    order: usize,
    /// Where in that rule it was written.
    ///
    /// Two declarations in one rule tie on everything above, and the
    /// later one wins — `border: 2px; border-top-width: 9px` means
    /// nine. Without this the tie is unbroken and whichever happens
    /// to be processed first survives.
    position: usize,
}

impl Rank {
    /// Below anything a stylesheet can say — where a presentational
    /// hint sits.
    const LOWEST: Self = Self {
        band: 0,
        inline: false,
        specificity: Specificity::new(0, 0, 0),
        order: 0,
        position: 0,
    };
}

struct Candidate<'a> {
    declaration: &'a Declaration,
    rank: Rank,
}

/// The declarations that won, by property name.
///
/// Values are still raw component values: this is the cascade's answer,
/// not a computed style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeclaredValues {
    values: BTreeMap<String, Won>,
}

/// A declaration that won its property, and what it beat the others
/// with.
///
/// The rank is kept because a shorthand holding a `var()` cannot be
/// taken apart until the custom properties are known — by which time
/// the cascade has finished. Its longhands still have to compete with
/// whatever won those names in the meantime, and without the rank
/// there is nothing to compare.
#[derive(Clone, Debug, PartialEq)]
struct Won {
    value: Vec<ComponentValue>,
    rank: Rank,
    /// The strongest declaration this one beat, kept so a value that
    /// turns out to be unparseable can give way to it.
    ///
    /// One is enough in practice: the loser is nearly always the
    /// user-agent sheet's, and that is the answer a browser would
    /// have reached.
    runner_up: Option<Vec<ComponentValue>>,
}

impl DeclaredValues {
    pub fn get(&self, property: &str) -> Option<&[ComponentValue]> {
        self.values.get(property).map(|won| won.value.as_slice())
    }

    /// The winner, and then what it beat, strongest first.
    ///
    /// A browser throws a declaration away the moment it cannot
    /// parse it, so the next one down wins. This engine parses much
    /// later, by which time the losers would be gone — and the cost
    /// of that is specific: a page that writes `display: -webkit-box`
    /// beats the user-agent sheet's `p { display: block }`, fails to
    /// parse, and the paragraph falls all the way back to `inline`
    /// rather than to the block it would have been.
    pub fn candidates(&self, property: &str) -> impl Iterator<Item = &[ComponentValue]> {
        self.values.get(property).into_iter().flat_map(|won| {
            std::iter::once(won.value.as_slice()).chain(won.runner_up.iter().map(Vec::as_slice))
        })
    }

    pub fn contains(&self, property: &str) -> bool {
        self.values.contains_key(property)
    }

    /// Properties and their values, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[ComponentValue])> {
        self.values
            .iter()
            .map(|(name, won)| (name.as_str(), won.value.as_slice()))
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Add declarations that lose to everything already here.
    ///
    /// Presentational hints — `<img width>` and its kin — sit below
    /// every author rule, so they fill gaps and never overwrite. The
    /// cascade proper has already run by the time this is called,
    /// which is what makes "already here" the right test.
    pub fn beneath(mut self, lower: Vec<Declaration>) -> Self {
        for declaration in lower {
            self.values.entry(declaration.name).or_insert(Won {
                runner_up: None,
                value: declaration.value,
                // Below every author rule, which is what a
                // presentational hint is.
                rank: Rank::LOWEST,
            });
        }
        self
    }

    /// The same declarations with every math function worked out.
    ///
    /// Runs after `var()` substitution and before any property is
    /// parsed, because that is the order the values need: a
    /// `calc(var(--pad) * 2)` has to have its `var()` pasted in before
    /// there is any arithmetic to do.
    pub fn evaluated(&self, lengths: &LengthContext) -> Self {
        Self {
            values: self
                .values
                .iter()
                .map(|(name, won)| {
                    (
                        name.clone(),
                        Won {
                            value: math::evaluate_all(&won.value, lengths),
                            rank: won.rank,
                            runner_up: won
                                .runner_up
                                .as_ref()
                                .map(|values| math::evaluate_all(values, lengths)),
                        },
                    )
                })
                .collect(),
        }
    }

    /// The same declarations with every `var()` replaced by what it
    /// refers to.
    ///
    /// This runs between the cascade and the computed values, because
    /// that is the only place it can: `var()` is resolved after a
    /// declaration has won and before anything tries to read it as a
    /// colour or a length. The custom properties themselves are left
    /// as they are — they are what the substitution reads.
    pub fn substituted(self, properties: &CustomProperties) -> Self {
        if properties.is_empty() {
            return self;
        }
        let mut values: BTreeMap<String, Won> = BTreeMap::new();
        for (name, won) in self.values {
            if custom::is_custom(&name) {
                values.insert(name, won);
                continue;
            }
            let substituted = custom::substitute(&won.value, properties);

            // A shorthand held back by `shorthand::expand` because it
            // contained a `var()` can be taken apart now. Its
            // longhands compete on the rank the shorthand itself won
            // with — which is the whole reason the rank is carried.
            //
            // Without that comparison a reset like `input {
            // border-radius: 0 }`, expanded when the rule loaded,
            // outlives a `.field { border-radius: var(--round) }`
            // that outranks it, and every form field comes out
            // square.
            let declaration = Declaration {
                name: name.clone(),
                value: substituted,
                important: false,
            };
            let rank = won.rank;
            // Every name goes in the same way, shorthand or not: a
            // longhand read straight from the cascade still has to
            // compete with one a deferred shorthand produced, and the
            // map is walked in name order rather than cascade order.
            for longhand in shorthand::expand(&declaration) {
                place(&mut values, longhand.name, longhand.value, rank);
            }
        }
        Self { values }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::{parse_declarations, serialize};
    use crate::engine::cryo_impl::html::parse as parse_html;

    /// The winning value of `property` on the first `<p>`, as text.
    fn winner(css: &str, property: &str) -> Option<String> {
        winner_for("<p>x</p>", css, "p", property, "")
    }

    /// The winning value of `property` on the element matching `target`.
    fn winner_for(
        markup: &str,
        css: &str,
        target: &str,
        property: &str,
        inline: &str,
    ) -> Option<String> {
        let doc = parse_html(markup);
        let element = doc
            .first_html_element(target)
            .unwrap_or_else(|| panic!("no <{target}> in the markup"));
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::Author, css);
        let inline = parse_declarations(inline);
        let declared = rules.declared_values(&doc, element, &inline, &MatchState::default());
        declared.get(property).map(serialize)
    }

    /// The winner where several origins are in play.
    fn winner_across(sheets: &[(Origin, &str)], property: &str) -> Option<String> {
        let doc = parse_html("<p>x</p>");
        let element = doc.first_html_element("p").expect("the p exists");
        let mut rules = RuleSet::new();
        for (origin, css) in sheets {
            rules.insert_css(*origin, css);
        }
        let declared = rules.declared_values(&doc, element, &[], &MatchState::default());
        declared.get(property).map(serialize)
    }

    #[test]
    fn a_single_rule_wins_by_default() {
        assert_eq!(winner("p { color: red }", "color").as_deref(), Some("red"));
    }

    #[test]
    fn a_property_nobody_declared_is_absent() {
        assert_eq!(winner("p { color: red }", "margin"), None);
    }

    #[test]
    fn a_rule_that_does_not_match_contributes_nothing() {
        assert_eq!(winner("div { color: red }", "color"), None);
    }

    #[test]
    fn at_equal_specificity_the_later_rule_wins() {
        assert_eq!(
            winner("p { color: red } p { color: blue }", "color").as_deref(),
            Some("blue")
        );
    }

    #[test]
    fn source_order_loses_to_specificity() {
        let css = "p.c { color: red } p { color: blue }";
        // With the class, the earlier but more specific rule wins even
        // though the plain `p` rule comes later.
        assert_eq!(
            winner_for("<p class=c>x</p>", css, "p", "color", "").as_deref(),
            Some("red")
        );
        // Without it, the `p.c` rule does not apply at all and the
        // weaker rule is the only candidate.
        assert_eq!(winner(css, "color").as_deref(), Some("blue"));
    }

    #[test]
    fn an_id_outranks_any_number_of_classes() {
        let css = "#t { color: red } p.a.b.c.d { color: blue }";
        assert_eq!(
            winner_for("<p id=t class='a b c d'>x</p>", css, "p", "color", "").as_deref(),
            Some("red")
        );
    }

    #[test]
    fn important_beats_normal_in_the_same_origin() {
        assert_eq!(
            winner("p { color: red !important } p { color: blue }", "color").as_deref(),
            Some("red")
        );
    }

    #[test]
    fn important_beats_a_more_specific_normal_declaration() {
        let css = "p { color: red !important } #t.c { color: blue }";
        assert_eq!(
            winner_for("<p id=t class=c>x</p>", css, "p", "color", "").as_deref(),
            Some("red")
        );
    }

    #[test]
    fn two_important_declarations_fall_back_to_specificity() {
        let css = "p { color: red !important } #t { color: blue !important }";
        assert_eq!(
            winner_for("<p id=t>x</p>", css, "p", "color", "").as_deref(),
            Some("blue")
        );
    }

    #[test]
    fn the_author_beats_the_user_agent_at_normal_importance() {
        assert_eq!(
            winner_across(
                &[
                    (Origin::UserAgent, "p { color: black }"),
                    (Origin::Author, "p { color: red }"),
                ],
                "color"
            )
            .as_deref(),
            Some("red")
        );
    }

    #[test]
    fn the_author_beats_the_user_agent_even_when_the_ua_is_more_specific() {
        assert_eq!(
            winner_across(
                &[
                    (Origin::UserAgent, "html body p { color: black }"),
                    (Origin::Author, "p { color: red }"),
                ],
                "color"
            )
            .as_deref(),
            Some("red"),
            "origin outranks specificity entirely"
        );
    }

    #[test]
    fn the_author_beats_the_user_at_normal_importance() {
        assert_eq!(
            winner_across(
                &[
                    (Origin::User, "p { color: green }"),
                    (Origin::Author, "p { color: red }"),
                ],
                "color"
            )
            .as_deref(),
            Some("red")
        );
    }

    #[test]
    fn a_user_important_declaration_beats_an_author_important_one() {
        // The inversion: this is what makes an accessibility override
        // work against a page that insists.
        assert_eq!(
            winner_across(
                &[
                    (Origin::Author, "p { color: red !important }"),
                    (Origin::User, "p { color: green !important }"),
                ],
                "color"
            )
            .as_deref(),
            Some("green")
        );
    }

    #[test]
    fn the_user_agent_important_declaration_beats_everything() {
        assert_eq!(
            winner_across(
                &[
                    (Origin::Author, "p { color: red !important }"),
                    (Origin::User, "p { color: green !important }"),
                    (Origin::UserAgent, "p { color: black !important }"),
                ],
                "color"
            )
            .as_deref(),
            Some("black")
        );
    }

    #[test]
    fn sheet_order_does_not_change_the_origin_ranking() {
        // Same three sheets, inserted in the opposite order.
        assert_eq!(
            winner_across(
                &[
                    (Origin::UserAgent, "p { color: black !important }"),
                    (Origin::User, "p { color: green !important }"),
                    (Origin::Author, "p { color: red !important }"),
                ],
                "color"
            )
            .as_deref(),
            Some("black")
        );
    }

    #[test]
    fn inline_style_beats_any_selector() {
        assert_eq!(
            winner_for(
                "<p id=t>x</p>",
                "#t { color: blue }",
                "p",
                "color",
                "color: red"
            )
            .as_deref(),
            Some("red")
        );
    }

    #[test]
    fn an_important_author_rule_beats_normal_inline_style() {
        assert_eq!(
            winner_for(
                "<p>x</p>",
                "p { color: blue !important }",
                "p",
                "color",
                "color: red"
            )
            .as_deref(),
            Some("blue")
        );
    }

    #[test]
    fn important_inline_style_beats_an_important_author_rule() {
        assert_eq!(
            winner_for(
                "<p>x</p>",
                "p { color: blue !important }",
                "p",
                "color",
                "color: red !important"
            )
            .as_deref(),
            Some("red")
        );
    }

    #[test]
    fn the_later_of_two_inline_declarations_wins() {
        assert_eq!(
            winner_for("<p>x</p>", "", "p", "color", "color: red; color: blue").as_deref(),
            Some("blue")
        );
    }

    #[test]
    fn every_declaration_in_a_matching_rule_lands() {
        let doc = parse_html("<p>x</p>");
        let element = doc.first_html_element("p").expect("the p exists");
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            "p { color: red; margin: 0 auto; padding: 1px }",
        );
        let declared = rules.declared_values(&doc, element, &[], &MatchState::default());
        // Shorthands are gone by now: one colour, four margins, four
        // paddings.
        assert_eq!(declared.len(), 9);
        assert!(!declared.contains("margin"), "shorthands are expanded away");
        assert_eq!(
            declared.get("margin-left").map(serialize).as_deref(),
            Some("auto")
        );
        assert_eq!(
            declared.get("margin-top").map(serialize).as_deref(),
            Some("0")
        );
        let names: Vec<_> = declared.iter().map(|(name, _)| name).collect();
        assert_eq!(names.first(), Some(&"color"), "iteration is by name");
    }

    /// Every candidate for `property`, strongest first.
    fn ranked(sheets: &[(Origin, &str)], property: &str) -> Vec<String> {
        let doc = parse_html("<p>x</p>");
        let element = doc.first_html_element("p").expect("the p exists");
        let mut rules = RuleSet::new();
        for (origin, css) in sheets {
            rules.insert_css(*origin, css);
        }
        let declared = rules.declared_values(&doc, element, &[], &MatchState::default());
        declared.candidates(property).map(serialize).collect()
    }

    #[test]
    fn what_the_winner_displaced_is_kept_behind_it() {
        // A browser drops an unparseable declaration the moment it
        // reads it, so the next one down wins. This engine parses
        // much later, and without the runner-up a page writing
        // `display: -webkit-box` would take the paragraph all the way
        // back to `inline` instead of to the user agent's `block`.
        let ranked = ranked(
            &[
                (Origin::UserAgent, "p { display: block }"),
                (Origin::Author, "p { display: -webkit-box }"),
            ],
            "display",
        );
        assert_eq!(ranked, ["-webkit-box", "block"]);
    }

    #[test]
    fn a_property_declared_once_has_no_runner_up() {
        assert_eq!(
            ranked(&[(Origin::Author, "p { color: red }")], "color"),
            ["red"]
        );
    }

    #[test]
    fn the_runner_up_is_the_strongest_of_the_losers() {
        // Three in play: the winner, then the one that came closest.
        let ranked = ranked(
            &[(
                Origin::Author,
                "p { color: red } p { color: green } #x { color: blue }",
            )],
            "color",
        );
        assert_eq!(ranked, ["green", "red"], "the id selector matches nothing");
    }

    #[test]
    fn declarations_from_different_properties_do_not_compete() {
        let css = "p { color: red } p { margin: 0 }";
        assert_eq!(winner(css, "color").as_deref(), Some("red"));
        assert_eq!(winner(css, "margin-top").as_deref(), Some("0"));
    }

    #[test]
    fn a_longhand_overrides_only_its_side_of_a_shorthand() {
        // The whole reason shorthands expand before the cascade.
        let css = "p { margin: 0; margin-left: 5px }";
        assert_eq!(winner(css, "margin-left").as_deref(), Some("5px"));
        assert_eq!(winner(css, "margin-top").as_deref(), Some("0"));
        assert_eq!(winner(css, "margin-right").as_deref(), Some("0"));
    }

    #[test]
    fn a_later_shorthand_overrides_an_earlier_longhand() {
        // The other direction: source order still decides.
        let css = "p { margin-left: 5px; margin: 0 }";
        assert_eq!(winner(css, "margin-left").as_deref(), Some("0"));
    }

    #[test]
    fn an_inline_shorthand_expands_too() {
        assert_eq!(
            winner_for(
                "<p>x</p>",
                "p { margin-left: 1px }",
                "p",
                "margin-left",
                "margin: 9px"
            )
            .as_deref(),
            Some("9px")
        );
    }

    #[test]
    fn a_rule_with_an_unparseable_selector_is_dropped_at_insertion() {
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::Author, "p:bogus { color: red } p { color: blue }");
        assert_eq!(rules.len(), 1, "only the valid rule is kept");
        assert_eq!(winner("p:bogus { color: red }", "color"), None);
    }

    #[test]
    fn a_selector_list_contributes_the_specificity_that_matched() {
        // `#other, p` matches the <p> at element specificity only, so a
        // class rule beats it.
        let css = "#other, p { color: red } p.c { color: blue }";
        assert_eq!(
            winner_for("<p class=c>x</p>", css, "p", "color", "").as_deref(),
            Some("blue")
        );
    }

    #[test]
    fn a_shorthand_holding_a_var_still_expands() {
        // The real case this exists for: `border: 1px solid
        // var(--line)` cannot be taken apart when the rule loads,
        // because the colour is not known yet. Dropping it loses the
        // border entirely, which is what a card looks like with no
        // edge at all.
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            ":root { --line: red } p { border: 2px solid var(--line) }",
        );
        let doc = parse_html("<p>x</p>");
        let node = doc
            .descendants(doc.root())
            .find(|&id| doc.is_html_element(id, "p"))
            .expect("a p");
        let custom = CustomProperties::inherit(
            &CustomProperties::new(),
            [(
                "--line",
                [ComponentValue::Token(CssToken::Ident("red".to_owned()))].as_slice(),
            )]
            .into_iter(),
        );
        let declared = rules
            .declared_values(&doc, node, &[], &MatchState::default())
            .substituted(&custom);

        assert!(declared.contains("border-top-width"), "the width survived");
        assert!(declared.contains("border-top-style"), "the style survived");
        assert_eq!(
            super::super::parser::serialize(declared.get("border-top-color").expect("a colour")),
            "red",
            "and the var was pasted in"
        );
    }

    #[test]
    fn an_explicit_longhand_beats_a_deferred_shorthand() {
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            "p { border: 2px solid var(--line); border-top-width: 9px }",
        );
        let doc = parse_html("<p>x</p>");
        let node = doc
            .descendants(doc.root())
            .find(|&id| doc.is_html_element(id, "p"))
            .expect("a p");
        let custom = CustomProperties::inherit(
            &CustomProperties::new(),
            [(
                "--line",
                [ComponentValue::Token(CssToken::Ident("red".to_owned()))].as_slice(),
            )]
            .into_iter(),
        );
        let declared = rules
            .declared_values(&doc, node, &[], &MatchState::default())
            .substituted(&custom);
        assert_eq!(
            super::super::parser::serialize(declared.get("border-top-width").expect("a width")),
            "9px"
        );
    }

    #[test]
    fn a_matching_media_block_is_loaded() {
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            "@media screen { p { color: red } } p { margin: 0 }",
        );
        assert_eq!(rules.len(), 2, "both rules are loaded");
        assert!(winner("@media screen { p { color: red } }", "color").is_some());
    }

    #[test]
    fn a_media_block_that_does_not_match_is_skipped() {
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::Author, "@media print { p { color: red } }");
        assert!(rules.is_empty());
    }

    #[test]
    fn the_viewport_decides_which_media_blocks_load() {
        let narrow = Viewport {
            width: 500.0,
            height: 800.0,
        };
        let wide = Viewport {
            width: 1500.0,
            height: 800.0,
        };
        let sheet = "@media (min-width: 1000px) { p { color: red } }";

        let mut small = RuleSet::for_viewport(narrow);
        small.insert_css(Origin::Author, sheet);
        assert!(small.is_empty(), "too narrow for the query");

        let mut large = RuleSet::for_viewport(wide);
        large.insert_css(Origin::Author, sheet);
        assert_eq!(large.len(), 1);
    }

    #[test]
    fn a_layer_is_always_entered() {
        // Layer *ordering* is not implemented, but the rules inside one
        // have to exist: a Tailwind sheet is almost entirely layered.
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::Author, "@layer base { p { color: red } }");
        assert_eq!(rules.len(), 1);
        assert!(winner("@layer base { p { color: red } }", "color").is_some());
    }

    #[test]
    fn nested_group_at_rules_are_entered_to_the_bottom() {
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            "@layer a { @supports (display: flex) { @media screen { p { color: red } } } }",
        );
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn a_negated_supports_block_is_skipped() {
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            "@supports not (display: grid) { p { color: red } }",
        );
        assert!(rules.is_empty());
    }

    #[test]
    fn an_unknown_group_at_rule_is_skipped() {
        // `@container` needs layout to answer, so its rules must not
        // apply unconditionally.
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            "@container (min-width: 1px) { p { color: red } }",
        );
        assert!(rules.is_empty());
    }

    #[test]
    fn an_empty_rule_set_declares_nothing() {
        let rules = RuleSet::new();
        assert!(rules.is_empty());
        let doc = parse_html("<p>x</p>");
        let element = doc.first_html_element("p").expect("the p exists");
        let declared = rules.declared_values(&doc, element, &[], &MatchState::default());
        assert!(declared.is_empty());
        assert_eq!(declared.get("color"), None);
    }

    #[test]
    fn matching_uses_the_whole_selector_chain() {
        let css = "div > p { color: red }";
        assert_eq!(
            winner_for("<div><p>x</p></div>", css, "p", "color", "").as_deref(),
            Some("red")
        );
        assert_eq!(
            winner_for("<section><p>x</p></section>", css, "p", "color", ""),
            None
        );
    }

    #[test]
    fn state_pseudo_classes_follow_the_match_state() {
        let doc = parse_html("<a href=x>link</a>");
        let anchor = doc.first_html_element("a").expect("the anchor exists");
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::Author, "a { color: blue } a:hover { color: red }");

        let resting = rules.declared_values(&doc, anchor, &[], &MatchState::default());
        assert_eq!(resting.get("color").map(serialize).as_deref(), Some("blue"));

        let hovered = MatchState {
            hovered: Some(anchor),
            ..MatchState::default()
        };
        let active = rules.declared_values(&doc, anchor, &[], &hovered);
        assert_eq!(active.get("color").map(serialize).as_deref(), Some("red"));
    }
    #[test]
    fn two_var_shorthands_in_one_rule_both_expand() {
        // A form field writes `border` and `border-radius` together,
        // both through custom properties. If expanding the first
        // stops the second, the field comes out square.
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            "p { border: 1px solid var(--line); border-radius: var(--round) }",
        );
        let doc = parse_html("<p>x</p>");
        let node = doc
            .descendants(doc.root())
            .find(|&id| doc.is_html_element(id, "p"))
            .expect("a p");
        let custom = CustomProperties::inherit(
            &CustomProperties::new(),
            [
                (
                    "--line",
                    [ComponentValue::Token(CssToken::Ident("red".to_owned()))].as_slice(),
                ),
                (
                    "--round",
                    [ComponentValue::Token(CssToken::Dimension {
                        value: 10.0,
                        unit: "px".to_owned(),
                    })]
                    .as_slice(),
                ),
            ]
            .into_iter(),
        );
        let declared = rules
            .declared_values(&doc, node, &[], &MatchState::default())
            .substituted(&custom);
        assert!(declared.contains("border-top-width"), "the border");
        assert!(
            declared.contains("border-top-left-radius"),
            "and the radius: {:?}",
            declared.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_deferred_shorthand_still_outranks_a_weaker_reset() {
        // The real case: a framework reset writes `input {
        // border-radius: 0 }`, which expands when the rule loads. A
        // class writes `border-radius: var(--round)`, which cannot.
        // The class outranks the type selector and must still win.
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            ".field { border-radius: var(--round) } input { border-radius: 0 }",
        );
        let doc = parse_html(r#"<input class=field>"#);
        let node = doc
            .descendants(doc.root())
            .find(|&id| doc.is_html_element(id, "input"))
            .expect("an input");
        let custom = CustomProperties::inherit(
            &CustomProperties::new(),
            [(
                "--round",
                [ComponentValue::Token(CssToken::Dimension {
                    value: 10.0,
                    unit: "px".to_owned(),
                })]
                .as_slice(),
            )]
            .into_iter(),
        );
        let declared = rules
            .declared_values(&doc, node, &[], &MatchState::default())
            .substituted(&custom);
        assert_eq!(
            super::super::parser::serialize(
                declared.get("border-top-left-radius").expect("a radius")
            ),
            "10px",
            "the class wins, not the reset"
        );
    }

    #[test]
    fn a_deferred_shorthand_loses_to_a_stronger_reset() {
        // The counterpart, so the comparison is a comparison and not
        // a preference.
        let mut rules = RuleSet::new();
        rules.insert_css(
            Origin::Author,
            ".field { border-radius: var(--round) } #one { border-radius: 2px }",
        );
        let doc = parse_html(r#"<input id=one class=field>"#);
        let node = doc
            .descendants(doc.root())
            .find(|&id| doc.is_html_element(id, "input"))
            .expect("an input");
        let custom = CustomProperties::inherit(
            &CustomProperties::new(),
            [(
                "--round",
                [ComponentValue::Token(CssToken::Dimension {
                    value: 10.0,
                    unit: "px".to_owned(),
                })]
                .as_slice(),
            )]
            .into_iter(),
        );
        let declared = rules
            .declared_values(&doc, node, &[], &MatchState::default())
            .substituted(&custom);
        assert_eq!(
            super::super::parser::serialize(
                declared.get("border-top-left-radius").expect("a radius")
            ),
            "2px",
            "the id wins"
        );
    }
}
