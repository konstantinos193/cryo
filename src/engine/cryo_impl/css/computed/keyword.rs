//! The keyword-valued properties: `display`, `position`, `float` and
//! the rest of the small closed vocabularies.
//!
//! Every one of these is "an enum plus a table of spellings", so they
//! share one lookup ([`keyword`]) rather than each carrying its own
//! `match` over strings. Twelve hand-written parsers would be twelve
//! places to forget case-insensitivity.
//!
//! Each type states its own initial value as `INITIAL`, because that is
//! a property of the property — not something the style struct should
//! know on its behalf. Defaulting then has exactly one place to look.
//!
//! A few of these carry behaviour, not just identity: [`Display`] knows
//! whether it makes a block, and [`WhiteSpace`] knows whether it
//! collapses runs and whether it wraps. That is deliberate — layout asks
//! those questions constantly, and the answers belong with the value
//! rather than in an `if` repeated at each site.

use crate::engine::cryo_impl::css::parser::ComponentValue;
use crate::engine::cryo_impl::css::tokenizer::CssToken;
use crate::engine::cryo_impl::css::value::single_token;

/// Look a value up in a table of keyword spellings.
///
/// Case-insensitive, and a value that is not exactly one identifier is
/// rejected — `display: block block` is invalid, not `block`.
fn keyword<T: Copy>(values: &[ComponentValue], table: &[(&str, T)]) -> Option<T> {
    let CssToken::Ident(name) = single_token(values)? else {
        return None;
    };
    table
        .iter()
        .find(|(spelling, _)| name.eq_ignore_ascii_case(spelling))
        .map(|(_, value)| *value)
}

/// What kind of box an element generates.
///
/// The table-internal values are present because the HTML parser builds
/// real `<tbody>` and `<tr>` elements and the user-agent sheet will give
/// them these displays; layout has to be able to tell them apart even
/// before table layout exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    ListItem,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    Table,
    InlineTable,
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableRow,
    TableCell,
    TableColumn,
    TableColumnGroup,
    TableCaption,
}

impl Display {
    pub const INITIAL: Self = Self::Inline;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("none", Self::None),
                ("block", Self::Block),
                ("inline", Self::Inline),
                ("inline-block", Self::InlineBlock),
                ("list-item", Self::ListItem),
                ("flex", Self::Flex),
                ("inline-flex", Self::InlineFlex),
                ("grid", Self::Grid),
                ("inline-grid", Self::InlineGrid),
                ("table", Self::Table),
                ("inline-table", Self::InlineTable),
                ("table-row-group", Self::TableRowGroup),
                ("table-header-group", Self::TableHeaderGroup),
                ("table-footer-group", Self::TableFooterGroup),
                ("table-row", Self::TableRow),
                ("table-cell", Self::TableCell),
                ("table-column", Self::TableColumn),
                ("table-column-group", Self::TableColumnGroup),
                ("table-caption", Self::TableCaption),
            ],
        )
    }

    /// Whether this element generates no box at all. Distinct from
    /// `visibility: hidden`, which generates a box and leaves its space.
    pub const fn generates_box(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether the box participates in its parent's inline flow.
    ///
    /// `inline-block` and `inline-flex` answer `true`: they sit on a line
    /// like a word, however their own contents are laid out.
    pub const fn is_inline_level(self) -> bool {
        matches!(
            self,
            Self::Inline
                | Self::InlineBlock
                | Self::InlineFlex
                | Self::InlineGrid
                | Self::InlineTable
        )
    }

    /// Whether this box lays its children out as flex items.
    pub const fn is_flex_container(self) -> bool {
        matches!(self, Self::Flex | Self::InlineFlex)
    }

    /// Whether this box lays its children out as a grid.
    pub const fn is_grid_container(self) -> bool {
        matches!(self, Self::Grid | Self::InlineGrid)
    }

    /// Whether the box lays its own children out as a block container.
    pub const fn is_block_container(self) -> bool {
        matches!(
            self,
            Self::Block | Self::InlineBlock | Self::ListItem | Self::TableCell | Self::TableCaption
        )
    }

    /// The block-level equivalent, used where a box must be blockified —
    /// a floated or absolutely positioned `inline` becomes `block`.
    pub const fn blockify(self) -> Self {
        match self {
            Self::Inline | Self::InlineBlock => Self::Block,
            Self::InlineFlex => Self::Flex,
            Self::InlineGrid => Self::Grid,
            Self::InlineTable => Self::Table,
            other => other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

impl Position {
    pub const INITIAL: Self = Self::Static;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("static", Self::Static),
                ("relative", Self::Relative),
                ("absolute", Self::Absolute),
                ("fixed", Self::Fixed),
                ("sticky", Self::Sticky),
            ],
        )
    }

    /// Whether the box is taken out of normal flow, so its siblings lay
    /// out as though it were not there.
    pub const fn is_out_of_flow(self) -> bool {
        matches!(self, Self::Absolute | Self::Fixed)
    }

    /// Whether this box is the containing block for the absolutely
    /// positioned boxes inside it.
    ///
    /// `relative` is the reason `position: relative` is written on
    /// parents that are never themselves moved: it changes nothing
    /// about the box, and everything about what `inset: 0` inside it
    /// means.
    pub const fn is_containing_block(self) -> bool {
        !matches!(self, Self::Static)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

impl Float {
    pub const INITIAL: Self = Self::None;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("none", Self::None),
                ("left", Self::Left),
                ("right", Self::Right),
            ],
        )
    }

    pub const fn is_floating(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clear {
    None,
    Left,
    Right,
    Both,
}

impl Clear {
    pub const INITIAL: Self = Self::None;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("none", Self::None),
                ("left", Self::Left),
                ("right", Self::Right),
                ("both", Self::Both),
            ],
        )
    }
}

/// Whether `width` sizes the content box or the border box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxSizing {
    ContentBox,
    BorderBox,
}

impl BoxSizing {
    pub const INITIAL: Self = Self::ContentBox;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("content-box", Self::ContentBox),
                ("border-box", Self::BorderBox),
            ],
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Hidden,
    Dotted,
    Dashed,
    Solid,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

impl BorderStyle {
    pub const INITIAL: Self = Self::None;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("none", Self::None),
                ("hidden", Self::Hidden),
                ("dotted", Self::Dotted),
                ("dashed", Self::Dashed),
                ("solid", Self::Solid),
                ("double", Self::Double),
                ("groove", Self::Groove),
                ("ridge", Self::Ridge),
                ("inset", Self::Inset),
                ("outset", Self::Outset),
            ],
        )
    }

    /// Whether this style draws anything.
    ///
    /// `none` and `hidden` both draw nothing *and force the computed
    /// border width to zero*, which is why a border's width cannot be
    /// resolved without consulting its style.
    pub const fn draws(self) -> bool {
        !matches!(self, Self::None | Self::Hidden)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontStyle {
    Normal,
    Italic,
    Oblique,
}

impl FontStyle {
    pub const INITIAL: Self = Self::Normal;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("normal", Self::Normal),
                ("italic", Self::Italic),
                ("oblique", Self::Oblique),
            ],
        )
    }
}

/// `text-align`. `Start` and `End` are direction-relative; `Left` and
/// `Right` are not, and the two pairs only coincide in left-to-right
/// text — so they stay distinct rather than being normalised here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Start,
    End,
    Left,
    Right,
    Center,
    Justify,
}

impl TextAlign {
    pub const INITIAL: Self = Self::Start;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("start", Self::Start),
                ("end", Self::End),
                ("left", Self::Left),
                ("right", Self::Right),
                ("center", Self::Center),
                ("justify", Self::Justify),
                // A legacy alias that still appears in real sheets.
                ("-webkit-center", Self::Center),
            ],
        )
    }
}

/// `white-space`, which controls three independent behaviours at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    Pre,
    NoWrap,
    PreWrap,
    PreLine,
    BreakSpaces,
}

impl WhiteSpace {
    pub const INITIAL: Self = Self::Normal;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("normal", Self::Normal),
                ("pre", Self::Pre),
                ("nowrap", Self::NoWrap),
                ("pre-wrap", Self::PreWrap),
                ("pre-line", Self::PreLine),
                ("break-spaces", Self::BreakSpaces),
            ],
        )
    }

    /// Whether runs of spaces and tabs collapse to one space.
    pub const fn collapses_spaces(self) -> bool {
        matches!(self, Self::Normal | Self::NoWrap | Self::PreLine)
    }

    /// Whether a newline in the source is a forced line break.
    ///
    /// Note `PreLine`: it collapses spaces *and* honours newlines, which
    /// is why these are three separate questions and not one enum rank.
    pub const fn preserves_newlines(self) -> bool {
        matches!(
            self,
            Self::Pre | Self::PreWrap | Self::PreLine | Self::BreakSpaces
        )
    }

    /// Whether lines may wrap when they overflow.
    pub const fn wraps(self) -> bool {
        !matches!(self, Self::Pre | Self::NoWrap)
    }
}

/// Which way a flex container lays its children out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

impl FlexDirection {
    pub const INITIAL: Self = Self::Row;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("row", Self::Row),
                ("row-reverse", Self::RowReverse),
                ("column", Self::Column),
                ("column-reverse", Self::ColumnReverse),
            ],
        )
    }

    /// Whether the main axis is horizontal.
    pub const fn is_row(self) -> bool {
        matches!(self, Self::Row | Self::RowReverse)
    }

    /// Whether items are placed from the far end backwards.
    pub const fn is_reversed(self) -> bool {
        matches!(self, Self::RowReverse | Self::ColumnReverse)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

impl FlexWrap {
    pub const INITIAL: Self = Self::NoWrap;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("nowrap", Self::NoWrap),
                ("wrap", Self::Wrap),
                ("wrap-reverse", Self::WrapReverse),
            ],
        )
    }

    pub const fn wraps(self) -> bool {
        !matches!(self, Self::NoWrap)
    }
}

/// How leftover main-axis space is distributed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JustifyContent {
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

impl JustifyContent {
    pub const INITIAL: Self = Self::FlexStart;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("flex-start", Self::FlexStart),
                ("start", Self::FlexStart),
                ("left", Self::FlexStart),
                ("normal", Self::FlexStart),
                ("flex-end", Self::FlexEnd),
                ("end", Self::FlexEnd),
                ("right", Self::FlexEnd),
                ("center", Self::Center),
                ("space-between", Self::SpaceBetween),
                ("space-around", Self::SpaceAround),
                ("space-evenly", Self::SpaceEvenly),
            ],
        )
    }
}

/// How items sit on the cross axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignItems {
    /// The initial value, and the one that catches people out: items
    /// grow to fill the container's cross size unless told otherwise.
    /// It is why a flex child with no height is as tall as its row.
    Stretch,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
}

impl AlignItems {
    pub const INITIAL: Self = Self::Stretch;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("stretch", Self::Stretch),
                ("normal", Self::Stretch),
                ("flex-start", Self::FlexStart),
                ("start", Self::FlexStart),
                ("self-start", Self::FlexStart),
                ("flex-end", Self::FlexEnd),
                ("end", Self::FlexEnd),
                ("self-end", Self::FlexEnd),
                ("center", Self::Center),
                ("baseline", Self::Baseline),
            ],
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    Hidden,
    Collapse,
}

impl Visibility {
    pub const INITIAL: Self = Self::Visible;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("visible", Self::Visible),
                ("hidden", Self::Hidden),
                ("collapse", Self::Collapse),
            ],
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
    Scroll,
    Auto,
}

impl Overflow {
    pub const INITIAL: Self = Self::Visible;

    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        keyword(
            values,
            &[
                ("visible", Self::Visible),
                ("hidden", Self::Hidden),
                ("clip", Self::Clip),
                ("scroll", Self::Scroll),
                ("auto", Self::Auto),
            ],
        )
    }

    /// Whether content outside the box is clipped away.
    pub const fn clips(self) -> bool {
        !matches!(self, Self::Visible)
    }
}

/// `text-decoration-line`, which is a set rather than one choice:
/// `underline line-through` is legal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TextDecorationLine {
    pub underline: bool,
    pub overline: bool,
    pub line_through: bool,
}

impl TextDecorationLine {
    pub const INITIAL: Self = Self {
        underline: false,
        overline: false,
        line_through: false,
    };

    /// Parse one or more line keywords, or `none`.
    ///
    /// Not a `keyword` lookup, because this is the one property here
    /// whose value is a set: each identifier turns a flag on, and a
    /// repeated or unknown one invalidates the declaration.
    pub fn parse(values: &[ComponentValue]) -> Option<Self> {
        let mut lines = Self::INITIAL;
        let mut saw_none = false;
        let mut saw_any = false;

        for value in values {
            let ComponentValue::Token(token) = value else {
                return None;
            };
            let name = match token {
                CssToken::Whitespace => continue,
                CssToken::Ident(name) => name,
                _ => return None,
            };
            saw_any = true;
            let slot = if name.eq_ignore_ascii_case("none") {
                saw_none = true;
                continue;
            } else if name.eq_ignore_ascii_case("underline") {
                &mut lines.underline
            } else if name.eq_ignore_ascii_case("overline") {
                &mut lines.overline
            } else if name.eq_ignore_ascii_case("line-through") {
                &mut lines.line_through
            } else {
                return None;
            };
            if *slot {
                // `underline underline` is invalid.
                return None;
            }
            *slot = true;
        }

        if !saw_any {
            return None;
        }
        // `none` cannot be combined with a line.
        if saw_none {
            return (lines == Self::INITIAL).then_some(Self::INITIAL);
        }
        Some(lines)
    }

    pub const fn draws(self) -> bool {
        self.underline || self.overline || self.line_through
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::parser::parse_declarations;

    /// The component values of `x: <text>`.
    fn values(text: &str) -> Vec<ComponentValue> {
        parse_declarations(&format!("x: {text}"))
            .into_iter()
            .next()
            .map(|declaration| declaration.value)
            .unwrap_or_default()
    }

    #[test]
    fn display_keywords_parse() {
        assert_eq!(Display::parse(&values("block")), Some(Display::Block));
        assert_eq!(
            Display::parse(&values("inline-block")),
            Some(Display::InlineBlock)
        );
        assert_eq!(
            Display::parse(&values("table-row-group")),
            Some(Display::TableRowGroup)
        );
        assert_eq!(Display::parse(&values("none")), Some(Display::None));
    }

    #[test]
    fn keywords_are_case_insensitive() {
        assert_eq!(Display::parse(&values("BLOCK")), Some(Display::Block));
        assert_eq!(
            Position::parse(&values("Absolute")),
            Some(Position::Absolute)
        );
    }

    #[test]
    fn an_unknown_keyword_is_invalid() {
        assert_eq!(Display::parse(&values("blork")), None);
        assert_eq!(Display::parse(&values("10px")), None);
    }

    #[test]
    fn a_two_keyword_value_is_invalid_for_a_single_choice() {
        assert_eq!(Display::parse(&values("block block")), None);
        assert_eq!(Display::parse(&values("block inline")), None);
    }

    #[test]
    fn whitespace_around_a_keyword_is_ignored() {
        assert_eq!(Display::parse(&values("  block  ")), Some(Display::Block));
    }

    #[test]
    fn the_initial_display_is_inline() {
        assert_eq!(Display::INITIAL, Display::Inline);
        assert_eq!(Position::INITIAL, Position::Static);
        assert_eq!(Float::INITIAL, Float::None);
        assert_eq!(BoxSizing::INITIAL, BoxSizing::ContentBox);
        assert_eq!(BorderStyle::INITIAL, BorderStyle::None);
        assert_eq!(WhiteSpace::INITIAL, WhiteSpace::Normal);
        assert_eq!(TextAlign::INITIAL, TextAlign::Start);
    }

    #[test]
    fn display_none_generates_no_box() {
        assert!(!Display::None.generates_box());
        assert!(Display::Block.generates_box());
        // `visibility: hidden` is a different question entirely.
        assert!(Display::Inline.generates_box());
    }

    #[test]
    fn inline_block_is_inline_level_but_a_block_container() {
        assert!(Display::InlineBlock.is_inline_level());
        assert!(Display::InlineBlock.is_block_container());
        assert!(!Display::Block.is_inline_level());
        assert!(Display::Block.is_block_container());
        assert!(!Display::Inline.is_block_container());
    }

    #[test]
    fn blockify_maps_each_inline_form_to_its_block_form() {
        assert_eq!(Display::Inline.blockify(), Display::Block);
        assert_eq!(Display::InlineBlock.blockify(), Display::Block);
        assert_eq!(Display::InlineFlex.blockify(), Display::Flex);
        assert_eq!(Display::InlineGrid.blockify(), Display::Grid);
        assert_eq!(Display::InlineTable.blockify(), Display::Table);
        // Already block-level, or not a box at all: unchanged.
        assert_eq!(Display::Block.blockify(), Display::Block);
        assert_eq!(Display::None.blockify(), Display::None);
        assert_eq!(Display::TableCell.blockify(), Display::TableCell);
    }

    #[test]
    fn absolute_and_fixed_leave_the_flow_but_relative_does_not() {
        assert!(Position::Absolute.is_out_of_flow());
        assert!(Position::Fixed.is_out_of_flow());
        assert!(!Position::Relative.is_out_of_flow());
        assert!(!Position::Sticky.is_out_of_flow());
        assert!(!Position::Static.is_out_of_flow());
    }

    #[test]
    fn border_styles_that_draw_nothing_are_identified() {
        assert!(!BorderStyle::None.draws());
        assert!(!BorderStyle::Hidden.draws());
        assert!(BorderStyle::Solid.draws());
        assert!(BorderStyle::Dotted.draws());
    }

    #[test]
    fn white_space_answers_its_three_questions_independently() {
        // `pre-line` is the one that mixes them: collapses spaces, keeps
        // newlines, and wraps.
        assert!(WhiteSpace::PreLine.collapses_spaces());
        assert!(WhiteSpace::PreLine.preserves_newlines());
        assert!(WhiteSpace::PreLine.wraps());

        assert!(WhiteSpace::Normal.collapses_spaces());
        assert!(!WhiteSpace::Normal.preserves_newlines());
        assert!(WhiteSpace::Normal.wraps());

        assert!(!WhiteSpace::Pre.collapses_spaces());
        assert!(WhiteSpace::Pre.preserves_newlines());
        assert!(!WhiteSpace::Pre.wraps());

        assert!(WhiteSpace::NoWrap.collapses_spaces());
        assert!(!WhiteSpace::NoWrap.wraps());

        assert!(!WhiteSpace::PreWrap.collapses_spaces());
        assert!(WhiteSpace::PreWrap.wraps());
    }

    #[test]
    fn overflow_other_than_visible_clips() {
        assert!(!Overflow::Visible.clips());
        assert!(Overflow::Hidden.clips());
        assert!(Overflow::Auto.clips());
        assert!(Overflow::Scroll.clips());
    }

    #[test]
    fn text_decoration_accepts_a_set_of_lines() {
        let underline = TextDecorationLine::parse(&values("underline")).expect("parses");
        assert!(underline.underline && !underline.overline);
        assert!(underline.draws());

        let both = TextDecorationLine::parse(&values("underline line-through")).expect("parses");
        assert!(both.underline && both.line_through && !both.overline);
    }

    #[test]
    fn text_decoration_none_draws_nothing() {
        let none = TextDecorationLine::parse(&values("none")).expect("parses");
        assert_eq!(none, TextDecorationLine::INITIAL);
        assert!(!none.draws());
    }

    #[test]
    fn text_decoration_rejects_repeats_combinations_and_nonsense() {
        assert_eq!(
            TextDecorationLine::parse(&values("underline underline")),
            None
        );
        assert_eq!(TextDecorationLine::parse(&values("none underline")), None);
        assert_eq!(TextDecorationLine::parse(&values("wavy")), None);
        assert_eq!(TextDecorationLine::parse(&values("")), None);
    }

    #[test]
    fn text_align_keeps_logical_and_physical_values_apart() {
        assert_eq!(TextAlign::parse(&values("start")), Some(TextAlign::Start));
        assert_eq!(TextAlign::parse(&values("left")), Some(TextAlign::Left));
        assert_ne!(TextAlign::Start, TextAlign::Left);
    }

    #[test]
    fn the_legacy_webkit_center_alias_parses() {
        assert_eq!(
            TextAlign::parse(&values("-webkit-center")),
            Some(TextAlign::Center)
        );
    }

    #[test]
    fn every_keyword_property_rejects_a_non_ident_value() {
        assert_eq!(Position::parse(&values("1px")), None);
        assert_eq!(Float::parse(&values("\"left\"")), None);
        assert_eq!(BoxSizing::parse(&values("50%")), None);
        assert_eq!(Visibility::parse(&values("rgb(1,2,3)")), None);
    }
}
