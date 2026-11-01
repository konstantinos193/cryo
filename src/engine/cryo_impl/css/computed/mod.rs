//! Computed values: the cascade's winning declarations become a
//! [`ComputedStyle`] with a value for *every* property.
//!
//! This is where `Option` stops. Layout should never ask "was a colour
//! declared?" — it asks what the colour is, and the answer comes from
//! the cascade, the parent, or the property's initial value, decided
//! here and nowhere else.
//!
//! Two orderings are load-bearing:
//!
//! 1. **`font-size` first.** Every other length may be written in `em`,
//!    which means the element's own font size — so it has to exist
//!    before anything else resolves. `font-size` itself resolves against
//!    the *parent's*, which is why [`font`] takes it explicitly.
//! 2. **`color` before `border-color`.** `currentColor` is the
//!    element's computed `color`, and `border-color` defaults to it.
//!
//! What stays unresolved on purpose: percentages. They need the
//! containing block, which does not exist until layout, so they travel
//! through as [`LengthPercentage`]. Resolving them here would be the
//! quiet way to get `width: 50%` wrong.

pub mod font;
pub mod keyword;

pub use font::LineHeight;
pub use keyword::{
    AlignItems, BorderStyle, BoxSizing, Clear, Display, FlexDirection, FlexWrap, Float, FontStyle,
    JustifyContent, Overflow, Position, TextAlign, TextDecorationLine, Visibility, WhiteSpace,
};

use super::cascade::DeclaredValues;
use super::math;
use super::parser::ComponentValue;
use super::tokenizer::CssToken;
use super::value::{
    color, parse_length_percentage, parse_size, single_token, BackgroundBox, BackgroundImage,
    ClipPath, Color, LengthContext, LengthPercentage, Placement, Shadow, Size, SpecifiedColor,
    Track,
};

/// A value per box side, in CSS's clockwise order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sides<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

impl<T: Copy> Sides<T> {
    pub const fn all(value: T) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    /// Apply the same transform to each side.
    pub fn map<U>(self, transform: impl Fn(T) -> U) -> Sides<U> {
        Sides {
            top: transform(self.top),
            right: transform(self.right),
            bottom: transform(self.bottom),
            left: transform(self.left),
        }
    }
}

impl Sides<f32> {
    /// The total along the inline axis — left plus right.
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }

    /// The total along the block axis — top plus bottom.
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

/// A value per box corner, clockwise from the top left.
///
/// Separate from [`Sides`] because the two are not interchangeable: a
/// side is between two corners, and writing `top` where `top_left` was
/// meant is the kind of mistake a shared type would hide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Corners<T> {
    pub top_left: T,
    pub top_right: T,
    pub bottom_right: T,
    pub bottom_left: T,
}

impl<T: Copy> Corners<T> {
    pub const fn all(value: T) -> Self {
        Self {
            top_left: value,
            top_right: value,
            bottom_right: value,
            bottom_left: value,
        }
    }

    /// Apply the same transform to each corner.
    pub fn map<U>(self, transform: impl Fn(T) -> U) -> Corners<U> {
        Corners {
            top_left: transform(self.top_left),
            top_right: transform(self.top_right),
            bottom_right: transform(self.bottom_right),
            bottom_left: transform(self.bottom_left),
        }
    }
}

impl Corners<f32> {
    /// Whether every corner is square, in which case the box can take
    /// the fast path and be filled as a plain rectangle.
    pub fn is_square(&self) -> bool {
        self.top_left <= 0.0
            && self.top_right <= 0.0
            && self.bottom_right <= 0.0
            && self.bottom_left <= 0.0
    }

    /// Shrink the radii until no two on the same edge overlap.
    ///
    /// CSS scales *all* corners by one factor rather than clipping
    /// them individually, so a pill stays a pill instead of turning
    /// into a lozenge with two flat ends.
    pub fn fitted(self, width: f32, height: f32) -> Self {
        let ratios = [
            (self.top_left + self.top_right, width),
            (self.bottom_left + self.bottom_right, width),
            (self.top_left + self.bottom_left, height),
            (self.top_right + self.bottom_right, height),
        ];
        let factor = ratios
            .iter()
            .filter(|(sum, _)| *sum > 0.0)
            .map(|(sum, extent)| extent / sum)
            .fold(1.0f32, f32::min);
        if factor >= 1.0 {
            return self;
        }
        self.map(|radius| radius * factor)
    }
}

/// A margin, which unlike a padding may be `auto` — the value that
/// centres a block and absorbs leftover space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Margin {
    Auto,
    Length(LengthPercentage),
}

impl Margin {
    pub const ZERO: Self = Self::Length(LengthPercentage::ZERO);

    fn parse(values: &[ComponentValue]) -> Option<Self> {
        if let Some(CssToken::Ident(name)) = single_token(values) {
            return name.eq_ignore_ascii_case("auto").then_some(Self::Auto);
        }
        parse_length_percentage(values).map(Self::Length)
    }

    /// Fix any length to pixels; see [`LengthPercentage::computed`].
    fn computed(self, context: &LengthContext) -> Self {
        match self {
            Self::Auto => Self::Auto,
            Self::Length(value) => Self::Length(value.computed(context)),
        }
    }
}

/// A `top`/`right`/`bottom`/`left` offset.
///
/// `auto` is the default and means "wherever flow would have put it",
/// which is why an absolutely positioned box with no inset at all stays
/// at its static position instead of jumping to its container's corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Inset {
    Auto,
    Length(LengthPercentage),
}

impl Inset {
    pub const AUTO: Self = Self::Auto;

    fn parse(values: &[ComponentValue]) -> Option<Self> {
        if let Some(CssToken::Ident(name)) = single_token(values) {
            return name.eq_ignore_ascii_case("auto").then_some(Self::Auto);
        }
        parse_length_percentage(values).map(Self::Length)
    }

    /// Fix any length to pixels; see [`LengthPercentage::computed`].
    fn computed(self, context: &LengthContext) -> Self {
        match self {
            Self::Auto => Self::Auto,
            Self::Length(value) => Self::Length(value.computed(context)),
        }
    }
}

/// A `z-index`: where a positioned box sits in the paint order.
///
/// `auto` and `0` paint in the same place — tree order among the
/// positioned boxes — and differ only in whether a stacking context is
/// created, which nothing here depends on yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ZIndex {
    Auto,
    Index(i32),
}

impl ZIndex {
    pub const INITIAL: Self = Self::Auto;

    /// The layer this box paints in. `auto` counts as zero.
    pub const fn layer(self) -> i32 {
        match self {
            Self::Auto => 0,
            Self::Index(index) => index,
        }
    }

    fn parse(values: &[ComponentValue]) -> Option<Self> {
        match single_token(values)? {
            CssToken::Ident(name) if name.eq_ignore_ascii_case("auto") => Some(Self::Auto),
            // An integer, which is what `z-index` takes — `z-index: 1.5`
            // is invalid rather than rounded.
            CssToken::Number {
                value,
                integer: true,
            } => Some(Self::Index(value as i32)),
            _ => None,
        }
    }
}

/// What computed values resolve against, beyond the parent's style.
#[derive(Clone, Copy, Debug)]
pub struct ComputeContext {
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// The root element's computed font size, for `rem`. `None` while
    /// computing the root itself, where `rem` falls back to the initial
    /// font size rather than to a value that does not exist yet.
    pub root_font_size: Option<f32>,
}

impl ComputeContext {
    pub const DEFAULT: Self = Self {
        viewport_width: 1024.0,
        viewport_height: 768.0,
        root_font_size: None,
    };
}

impl Default for ComputeContext {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Every property's computed value for one element.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedStyle {
    pub display: Display,
    pub position: Position,
    /// `top`/`right`/`bottom`/`left`, which only a positioned box reads.
    pub inset: Sides<Inset>,
    /// Where a positioned box sits in the paint order.
    pub z_index: ZIndex,
    pub float: Float,
    pub clear: Clear,
    pub box_sizing: BoxSizing,
    pub visibility: Visibility,
    /// How opaque the box and everything in it is, 0..=1.
    pub opacity: f32,
    /// The shape the box is cut to, if any.
    pub clip_path: Option<ClipPath>,
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,

    pub width: Size,
    pub height: Size,
    pub min_width: LengthPercentage,
    pub min_height: LengthPercentage,
    /// `None` is `max-width: none`, which is the initial value — not an
    /// absent declaration.
    pub max_width: Option<LengthPercentage>,
    pub max_height: Option<LengthPercentage>,
    /// `aspect-ratio`, as width divided by height.
    ///
    /// It is what gives a box a size along one axis when only the
    /// other is known — an image with `height: auto` has no height
    /// at all without it.
    pub aspect_ratio: Option<f32>,

    /// Flex container properties. Meaningful only when `display` is a
    /// flex value; held unconditionally because every box has them and
    /// branching on display to read a field is worse than the space.
    /// Grid track lists. Empty means no explicit template.
    pub grid_template_columns: Vec<Track>,
    pub grid_template_rows: Vec<Track>,
    /// Where this item sits, if it says so itself.
    pub grid_column: Placement,
    pub grid_row: Placement,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub justify_content: JustifyContent,
    pub align_items: AlignItems,
    /// A flex item overriding its container, when it says so.
    pub align_self: Option<AlignItems>,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Size,
    pub row_gap: LengthPercentage,
    pub column_gap: LengthPercentage,

    pub margin: Sides<Margin>,
    pub padding: Sides<LengthPercentage>,
    /// Already in pixels: border widths take no percentages, so there is
    /// nothing left for layout to resolve.
    pub border_width: Sides<f32>,
    /// Corner radii, still as written: a percentage is of the box, and
    /// the box has no size until layout.
    pub border_radius: Corners<LengthPercentage>,
    /// Cast shadows, in the order they were written.
    pub box_shadow: Vec<Shadow>,
    pub border_style: Sides<BorderStyle>,
    pub border_color: Sides<Color>,

    pub color: Color,
    pub background_color: Color,
    /// A gradient painted over the background colour. Kept separate
    /// because a box can have both, and the colour shows through
    /// wherever the gradient is transparent.
    /// The background layers, topmost first. Empty is `none`.
    pub background_image: Vec<BackgroundImage>,
    /// Which box each layer is painted in, one per layer and cycled
    /// when shorter. Empty means the initial `border-box`.
    pub background_clip: Vec<BackgroundBox>,
    /// `mask-image`: what the box is painted through.
    ///
    /// Shares a grammar with `background-image`, and is read from the
    /// `-webkit-` spelling too — the prefixed form is still what most
    /// toolchains emit, and a page that writes both means the same
    /// thing by each.
    pub mask_image: Vec<BackgroundImage>,

    pub font_size: f32,
    pub font_family: Vec<String>,
    pub font_weight: u16,
    pub font_style: FontStyle,
    pub line_height: LineHeight,
    /// Extra space after each character, in pixels. Negative tightens.
    pub letter_spacing: f32,
    pub text_align: TextAlign,
    pub white_space: WhiteSpace,
    pub text_decoration_line: TextDecorationLine,
}

/// The widths the `thin`/`medium`/`thick` keywords stand for.
const BORDER_WIDTH_KEYWORDS: &[(&str, f32)] = &[
    ("thin", 1.0),
    ("medium", MEDIUM_BORDER_WIDTH),
    ("thick", 5.0),
];

/// `border-width`'s initial value, which is `medium`.
const MEDIUM_BORDER_WIDTH: f32 = 3.0;

impl Default for ComputedStyle {
    fn default() -> Self {
        Self::INITIAL
    }
}

impl ComputedStyle {
    /// Every property at its initial value.
    ///
    /// Also the notional parent of the root element, which is what makes
    /// inheritance on the root well defined rather than a special case.
    pub const INITIAL: Self = Self {
        display: Display::INITIAL,
        position: Position::INITIAL,
        inset: Sides::all(Inset::AUTO),
        z_index: ZIndex::INITIAL,
        float: Float::INITIAL,
        clear: Clear::INITIAL,
        box_sizing: BoxSizing::INITIAL,
        visibility: Visibility::INITIAL,
        opacity: 1.0,
        clip_path: None,
        overflow_x: Overflow::INITIAL,
        overflow_y: Overflow::INITIAL,
        width: Size::Auto,
        height: Size::Auto,
        min_width: LengthPercentage::ZERO,
        min_height: LengthPercentage::ZERO,
        max_width: None,
        max_height: None,
        aspect_ratio: None,
        grid_template_columns: Vec::new(),
        grid_template_rows: Vec::new(),
        grid_column: Placement::AUTO,
        grid_row: Placement::AUTO,
        flex_direction: FlexDirection::INITIAL,
        flex_wrap: FlexWrap::INITIAL,
        justify_content: JustifyContent::INITIAL,
        align_items: AlignItems::INITIAL,
        align_self: None,
        flex_grow: 0.0,
        flex_shrink: 1.0,
        flex_basis: Size::Auto,
        row_gap: LengthPercentage::ZERO,
        column_gap: LengthPercentage::ZERO,
        margin: Sides::all(Margin::ZERO),
        padding: Sides::all(LengthPercentage::ZERO),
        border_width: Sides::all(0.0),
        border_radius: Corners::all(LengthPercentage::ZERO),
        box_shadow: Vec::new(),
        border_style: Sides::all(BorderStyle::INITIAL),
        border_color: Sides::all(Color::BLACK),
        color: Color::BLACK,
        background_color: Color::TRANSPARENT,
        background_image: Vec::new(),
        background_clip: Vec::new(),
        mask_image: Vec::new(),
        font_size: font::INITIAL_FONT_SIZE,
        font_family: Vec::new(),
        font_weight: font::INITIAL_FONT_WEIGHT,
        font_style: FontStyle::INITIAL,
        line_height: LineHeight::INITIAL,
        letter_spacing: 0.0,
        text_align: TextAlign::INITIAL,
        white_space: WhiteSpace::INITIAL,
        text_decoration_line: TextDecorationLine::INITIAL,
    };

    /// Compute this element's style from its parent's and the
    /// declarations that won the cascade.
    pub fn compute(parent: &Self, declared: &DeclaredValues, context: &ComputeContext) -> Self {
        // `font-size` first: every `em` below depends on it.
        let font_size = Self::compute_font_size(parent, declared, context);
        let lengths = LengthContext {
            font_size,
            root_font_size: context.root_font_size.unwrap_or(font_size),
            viewport_width: context.viewport_width,
            viewport_height: context.viewport_height,
        };

        // `calc()` and its relatives are worked out here, where the
        // lengths inside them can finally be resolved. Everything
        // below sees a plain number or dimension and needs to know
        // nothing about CSS math.
        let declared = &declared.evaluated(&lengths);

        let mut style = Self {
            font_size,
            ..Self::inheriting_from(parent)
        };

        style.apply_box(declared);
        style.apply_sizes(declared, &lengths);
        style.apply_spacing(declared, &lengths);
        style.apply_gaps(declared, &lengths);
        style.apply_text(parent, declared, &lengths);
        // After `color`, because `currentColor` resolves to it.
        style.apply_borders(declared, &lengths);
        style
    }

    /// A style with the inherited properties taken from `parent` and
    /// everything else at its initial value.
    ///
    /// **This list is the definition of "inherited" for this engine.**
    /// It has exactly one copy on purpose: [`Self::compute`] starts from
    /// it, and so does [`Self::anonymous_from`]. Two copies would drift,
    /// and the symptom — one property inheriting into real boxes but not
    /// into anonymous ones — would be almost impossible to spot.
    fn inheriting_from(parent: &Self) -> Self {
        Self {
            color: parent.color,
            font_size: parent.font_size,
            font_family: parent.font_family.clone(),
            font_weight: parent.font_weight,
            font_style: parent.font_style,
            line_height: parent.line_height,
            // A text box has no element of its own, so this is the
            // only way spacing reaches the thing that is measured.
            // Leaving it out measures every line as though it were
            // set loose, and a headline drawn to fit on two lines
            // breaks onto three.
            letter_spacing: parent.letter_spacing,
            text_align: parent.text_align,
            white_space: parent.white_space,
            visibility: parent.visibility,
            ..Self::INITIAL
        }
    }

    /// The style of an anonymous box generated inside `parent`.
    ///
    /// An anonymous box has no element, so nothing can target it and it
    /// has no declarations of its own: it inherits, and takes initial
    /// values for the rest. It is always block-level — holding an inline
    /// run among block siblings is the only reason it exists.
    pub fn anonymous_from(parent: &Self) -> Self {
        Self {
            display: Display::Block,
            ..Self::inheriting_from(parent)
        }
    }

    /// The style of a text box inside `parent`.
    ///
    /// Text has no element and nothing can target it, so like an
    /// anonymous box it inherits and takes initial values for the
    /// rest. It differs from [`Self::anonymous_from`] only in staying
    /// inline-level: text joins a line, it does not make one.
    ///
    /// Taking the parent's style *wholesale* instead is the obvious
    /// shortcut and a quiet disaster: the text would carry the
    /// parent's padding and draw it a second time, and carry its
    /// `position: absolute` and be filtered out of the flow it is the
    /// entire content of.
    pub fn text_from(parent: &Self) -> Self {
        Self::inheriting_from(parent)
    }

    /// The element's own font size, resolved against the parent's.
    fn compute_font_size(
        parent: &Self,
        declared: &DeclaredValues,
        context: &ComputeContext,
    ) -> f32 {
        let Some(values) = declared.get("font-size") else {
            return parent.font_size;
        };
        // `em` and `%` here mean the parent's size, so that is what the
        // context carries. `rem` on the root falls back to the initial
        // size, since the root's own is what is being computed.
        let lengths = LengthContext {
            font_size: parent.font_size,
            root_font_size: context.root_font_size.unwrap_or(font::INITIAL_FONT_SIZE),
            viewport_width: context.viewport_width,
            viewport_height: context.viewport_height,
        };
        // `font-size: clamp(32px, 4.2vw, 48px)` is how a responsive
        // heading is written, so the math is worked out here too — and
        // against the parent's size, for the same reason as above.
        let values = math::evaluate_all(values, &lengths);
        font::parse_size(&values, parent.font_size, &lengths).unwrap_or(parent.font_size)
    }

    /// The keyword properties that decide what kind of box this is.
    fn apply_box(&mut self, declared: &DeclaredValues) {
        self.display = resolve(declared, "display", Display::parse, self.display);
        self.position = resolve(declared, "position", Position::parse, self.position);
        self.float = resolve(declared, "float", Float::parse, self.float);
        self.clear = resolve(declared, "clear", Clear::parse, self.clear);
        self.box_sizing = resolve(declared, "box-sizing", BoxSizing::parse, self.box_sizing);
        self.visibility = resolve(declared, "visibility", Visibility::parse, self.visibility);
        // A number or a percentage, clamped — `opacity: 2` is fully
        // opaque rather than invalid.
        self.opacity = declared.get("opacity").and_then(parse_alpha).unwrap_or(1.0);
        self.overflow_x = resolve(declared, "overflow-x", Overflow::parse, self.overflow_x);
        self.overflow_y = resolve(declared, "overflow-y", Overflow::parse, self.overflow_y);

        // A floated or absolutely positioned box is block-level however
        // it was declared, so the rest of layout never has to ask.
        if self.float.is_floating() || self.position.is_out_of_flow() {
            self.display = self.display.blockify();
        }

        self.flex_direction = resolve(
            declared,
            "flex-direction",
            FlexDirection::parse,
            self.flex_direction,
        );
        self.flex_wrap = resolve(declared, "flex-wrap", FlexWrap::parse, self.flex_wrap);
        self.justify_content = resolve(
            declared,
            "justify-content",
            JustifyContent::parse,
            self.justify_content,
        );
        self.align_items = resolve(declared, "align-items", AlignItems::parse, self.align_items);
        // `auto` means "use the container's", which is this being absent.
        self.align_self = declared.get("align-self").and_then(AlignItems::parse);
        self.flex_grow = resolve(declared, "flex-grow", parse_number, self.flex_grow);
        self.flex_shrink = resolve(declared, "flex-shrink", parse_number, self.flex_shrink);
        self.flex_basis = resolve(declared, "flex-basis", parse_size, self.flex_basis);
    }

    /// Gaps, resolved with the other lengths because they take `em`.
    fn apply_gaps(&mut self, declared: &DeclaredValues, lengths: &LengthContext) {
        self.row_gap =
            resolve(declared, "row-gap", parse_length_percentage, self.row_gap).computed(lengths);
        self.column_gap = resolve(
            declared,
            "column-gap",
            parse_length_percentage,
            self.column_gap,
        )
        .computed(lengths);
    }

    /// The box's own dimensions. Every length is fixed to pixels here;
    /// percentages travel on for layout to resolve.
    fn apply_sizes(&mut self, declared: &DeclaredValues, lengths: &LengthContext) {
        // Grid templates are sizes too, and they resolve their lengths
        // the same way — `repeat(3, 2rem)` has to become pixels here
        // like any other.
        let tracks = |property: &str| {
            declared
                .get(property)
                .and_then(Track::parse_list)
                .map(|list| {
                    list.into_iter()
                        .map(|track| track.computed(lengths))
                        .collect()
                })
                .unwrap_or_default()
        };
        self.aspect_ratio = declared.get("aspect-ratio").and_then(parse_ratio);
        self.clip_path = declared
            .get("clip-path")
            .and_then(ClipPath::parse)
            .map(|shape| shape.computed(lengths));
        self.grid_template_columns = tracks("grid-template-columns");
        self.grid_template_rows = tracks("grid-template-rows");
        self.grid_column = resolve(declared, "grid-column", Placement::parse, Placement::AUTO);
        self.grid_row = resolve(declared, "grid-row", Placement::parse, Placement::AUTO);

        self.width = resolve(declared, "width", parse_size, self.width).computed(lengths);
        self.height = resolve(declared, "height", parse_size, self.height).computed(lengths);
        self.min_width = resolve(
            declared,
            "min-width",
            parse_length_percentage,
            self.min_width,
        )
        .computed(lengths);
        self.min_height = resolve(
            declared,
            "min-height",
            parse_length_percentage,
            self.min_height,
        )
        .computed(lengths);
        self.max_width = resolve(declared, "max-width", parse_max, self.max_width)
            .map(|value| value.computed(lengths));
        self.max_height = resolve(declared, "max-height", parse_max, self.max_height)
            .map(|value| value.computed(lengths));
    }

    fn apply_spacing(&mut self, declared: &DeclaredValues, lengths: &LengthContext) {
        self.inset =
            sides(declared, "", "", Inset::parse, Inset::AUTO).map(|value| value.computed(lengths));
        self.z_index = resolve(declared, "z-index", ZIndex::parse, ZIndex::INITIAL);
        self.margin = sides(declared, "margin", "", Margin::parse, Margin::ZERO)
            .map(|value| value.computed(lengths));
        self.padding = sides(
            declared,
            "padding",
            "",
            parse_length_percentage,
            LengthPercentage::ZERO,
        )
        .map(|value| value.computed(lengths));
    }

    fn apply_text(&mut self, parent: &Self, declared: &DeclaredValues, lengths: &LengthContext) {
        if let Some(SpecifiedColor::Absolute(value)) = declared.get("color").and_then(color::parse)
        {
            self.color = value;
        }
        // `color: currentColor` is the inherited colour, which is
        // already in place — so the `Current` case needs no branch.

        self.background_color = declared
            .get("background-color")
            .and_then(color::parse)
            .map(|specified| self.absolute(specified))
            .unwrap_or(self.background_color);
        self.background_image = declared
            .get("background-image")
            .and_then(BackgroundImage::parse_list)
            .unwrap_or_default();
        self.background_clip = declared
            .get("background-clip")
            .and_then(BackgroundBox::parse_list)
            .unwrap_or_default();
        self.mask_image = declared
            .get("mask-image")
            .or_else(|| declared.get("-webkit-mask-image"))
            .and_then(BackgroundImage::parse_list)
            .unwrap_or_default();

        self.font_family = declared
            .get("font-family")
            .and_then(font::parse_family)
            .unwrap_or_else(|| parent.font_family.clone());
        self.font_weight = declared
            .get("font-weight")
            .and_then(|values| font::parse_weight(values, parent.font_weight))
            .unwrap_or(parent.font_weight);
        self.font_style = resolve(declared, "font-style", font::parse_style, self.font_style);
        self.line_height = declared
            .get("line-height")
            .and_then(|values| font::parse_line_height(values, lengths))
            .unwrap_or(self.line_height);
        // Inherited, and resolved against *this* element's font size:
        // an em here is an em of the text it spaces.
        self.letter_spacing = declared
            .get("letter-spacing")
            .and_then(|values| font::parse_letter_spacing(values, lengths))
            .unwrap_or(parent.letter_spacing);
        self.text_align = resolve(declared, "text-align", TextAlign::parse, self.text_align);
        self.white_space = resolve(declared, "white-space", WhiteSpace::parse, self.white_space);
        self.text_decoration_line = resolve(
            declared,
            "text-decoration-line",
            TextDecorationLine::parse,
            self.text_decoration_line,
        );
    }

    /// Border styles, then widths, then colours — in that order, because
    /// a style of `none` forces the width to zero whatever was declared.
    fn apply_borders(&mut self, declared: &DeclaredValues, lengths: &LengthContext) {
        self.border_radius = corners(declared, lengths);
        // After `color`, because a shadow with no colour of its own
        // takes the element's.
        self.box_shadow = declared
            .get("box-shadow")
            .and_then(|values| Shadow::parse_list(values, lengths, self.color))
            .unwrap_or_default();
        self.border_style = sides(
            declared,
            "border",
            "-style",
            BorderStyle::parse,
            BorderStyle::INITIAL,
        );
        // The fallback is `medium`, the initial border-width — not zero.
        // A `border-style: solid` with no width stated still draws.
        let widths = sides(
            declared,
            "border",
            "-width",
            |values| parse_border_width(values, lengths),
            MEDIUM_BORDER_WIDTH,
        );
        self.border_width = Sides {
            top: zero_unless_drawn(widths.top, self.border_style.top),
            right: zero_unless_drawn(widths.right, self.border_style.right),
            bottom: zero_unless_drawn(widths.bottom, self.border_style.bottom),
            left: zero_unless_drawn(widths.left, self.border_style.left),
        };

        self.border_color = sides(
            declared,
            "border",
            "-color",
            color::parse,
            SpecifiedColor::Current,
        )
        .map(|specified| self.absolute(specified));
    }

    /// Collapse a specified colour, resolving `currentColor` against
    /// this element's own computed `color`.
    fn absolute(&self, specified: SpecifiedColor) -> Color {
        match specified {
            SpecifiedColor::Absolute(value) => value,
            SpecifiedColor::Current => self.color,
        }
    }

    /// The line box height implied by this style's font and line height.
    pub fn line_box_height(&self) -> f32 {
        self.line_height.to_px(self.font_size)
    }
}

/// A border width stays as declared only if its style draws.
fn zero_unless_drawn(width: f32, style: BorderStyle) -> f32 {
    if style.draws() {
        width
    } else {
        0.0
    }
}

/// One property: parse the declared value, or keep `fallback`.
///
/// The single place "declared, else inherited or initial" is expressed.
/// An unparseable value falls back too, which is what makes an
/// unsupported `width: calc(...)` leave the width alone instead of
/// zeroing it.
fn resolve<T>(
    declared: &DeclaredValues,
    property: &str,
    parse: impl Fn(&[ComponentValue]) -> Option<T>,
    fallback: T,
) -> T {
    declared
        .candidates(property)
        .find_map(parse)
        .unwrap_or(fallback)
}

/// Four longhands at once: `margin-top`, `margin-right`, and so on.
///
/// Shorthands were already expanded by the cascade, so only the
/// per-side names are ever looked up here.
fn sides<T: Copy>(
    declared: &DeclaredValues,
    prefix: &str,
    suffix: &str,
    parse: impl Fn(&[ComponentValue]) -> Option<T>,
    fallback: T,
) -> Sides<T> {
    let side = |name: &str| {
        resolve(
            declared,
            &super::shorthand::longhand(prefix, name, suffix),
            &parse,
            fallback,
        )
    };
    Sides {
        top: side("top"),
        right: side("right"),
        bottom: side("bottom"),
        left: side("left"),
    }
}

/// The four `border-*-radius` longhands.
///
/// Negative radii are invalid, not clamped, so a bad one falls back to
/// zero like any other unparseable value.
fn corners(declared: &DeclaredValues, lengths: &LengthContext) -> Corners<LengthPercentage> {
    let corner = |name: &str| {
        resolve(
            declared,
            &format!("border-{name}-radius"),
            parse_length_percentage,
            LengthPercentage::ZERO,
        )
        .computed(lengths)
    };
    Corners {
        top_left: corner("top-left"),
        top_right: corner("top-right"),
        bottom_right: corner("bottom-right"),
        bottom_left: corner("bottom-left"),
    }
}

/// A bare non-negative number, as the flex factors take.
fn parse_number(values: &[ComponentValue]) -> Option<f32> {
    match single_token(values)? {
        CssToken::Number { value, .. } => (value >= 0.0).then_some(value as f32),
        _ => None,
    }
}

/// `aspect-ratio: 16 / 9`, or a bare number.
///
/// Returned as width over height, so a box `r` times as wide as it is
/// tall has a ratio of `r`.
fn parse_ratio(values: &[ComponentValue]) -> Option<f32> {
    let mut numbers = Vec::new();
    for value in values {
        match value {
            ComponentValue::Token(CssToken::Whitespace) => {}
            ComponentValue::Token(CssToken::Delim('/')) => {}
            ComponentValue::Token(CssToken::Number { value, .. }) => numbers.push(*value as f32),
            // `auto` alone means no ratio of its own.
            _ => return None,
        }
    }
    match numbers[..] {
        [width] if width > 0.0 => Some(width),
        [width, height] if width > 0.0 && height > 0.0 => Some(width / height),
        _ => None,
    }
}

/// An `opacity`: a number or a percentage, clamped to 0..=1.
///
/// Out-of-range values are clamped rather than rejected, which is
/// what the spec says — `opacity: 2` is fully opaque.
fn parse_alpha(values: &[ComponentValue]) -> Option<f32> {
    match single_token(values)? {
        CssToken::Number { value, .. } => Some((value as f32).clamp(0.0, 1.0)),
        CssToken::Percentage(percent) => Some((percent as f32 / 100.0).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// `max-width: none` and `max-width: <length>`.
fn parse_max(values: &[ComponentValue]) -> Option<Option<LengthPercentage>> {
    if let Some(CssToken::Ident(name)) = single_token(values) {
        return name.eq_ignore_ascii_case("none").then_some(None);
    }
    parse_length_percentage(values).map(Some)
}

/// A border width: a keyword or a length, never a percentage.
fn parse_border_width(values: &[ComponentValue], lengths: &LengthContext) -> Option<f32> {
    if let Some(CssToken::Ident(name)) = single_token(values) {
        return BORDER_WIDTH_KEYWORDS
            .iter()
            .find(|(spelling, _)| name.eq_ignore_ascii_case(spelling))
            .map(|(_, width)| *width);
    }
    match parse_length_percentage(values)? {
        LengthPercentage::Length(length) => {
            let width = length.to_px(lengths);
            (width >= 0.0).then_some(width)
        }
        LengthPercentage::Percentage(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::cascade::{Origin, RuleSet};
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::dom::NodeId;
    use crate::engine::cryo_impl::html::parse as parse_html;

    /// Compute the style of the element matching `target`, with the
    /// styles of its ancestors applied in order.
    fn style_of(markup: &str, css: &str, target: &str) -> ComputedStyle {
        let doc = parse_html(markup);
        let element = doc
            .first_html_element(target)
            .unwrap_or_else(|| panic!("no <{target}> in the markup"));
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::Author, css);

        // Ancestors outermost-first, so inheritance flows downward.
        let mut chain: Vec<NodeId> = doc.ancestors(element).collect();
        chain.retain(|&id| doc.element(id).is_some());
        chain.reverse();
        chain.push(element);

        let mut style = ComputedStyle::INITIAL;
        let mut context = ComputeContext::DEFAULT;
        for (depth, id) in chain.into_iter().enumerate() {
            let declared = rules.declared_values(&doc, id, &[], &MatchState::default());
            style = ComputedStyle::compute(&style, &declared, &context);
            if depth == 0 {
                // Everything below the root measures `rem` against it.
                context.root_font_size = Some(style.font_size);
            }
        }
        style
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    #[test]
    fn an_undeclared_property_takes_its_initial_value() {
        let style = style_of("<p>x</p>", "", "p");
        assert_eq!(style.display, Display::Inline, "the initial display");
        assert_eq!(style.width, Size::Auto);
        assert_eq!(style.background_color, Color::TRANSPARENT);
        assert_eq!(style.border_width, Sides::all(0.0));
    }

    #[test]
    fn a_declared_property_is_used() {
        let style = style_of("<p>x</p>", "p { display: block }", "p");
        assert_eq!(style.display, Display::Block);
    }

    #[test]
    fn colour_inherits_but_display_does_not() {
        let style = style_of(
            "<div><p>x</p></div>",
            "div { color: red; display: block }",
            "p",
        );
        assert_eq!(style.color, Color::rgb(255, 0, 0), "colour inherits");
        assert_eq!(style.display, Display::Inline, "display does not");
    }

    #[test]
    fn font_properties_inherit() {
        let style = style_of(
            "<div><p>x</p></div>",
            "div { font-weight: bold; font-style: italic; font-family: Arial }",
            "p",
        );
        assert_eq!(style.font_weight, 700);
        assert_eq!(style.font_style, FontStyle::Italic);
        assert_eq!(style.font_family, ["Arial"]);
    }

    #[test]
    fn font_size_compounds_down_the_tree() {
        // 16 -> 32 -> 48: each `em` is against its own parent.
        let style = style_of(
            "<div><section><p>x</p></section></div>",
            "div { font-size: 2em } section { font-size: 1.5em }",
            "p",
        );
        assert!(close(style.font_size, 48.0), "got {}", style.font_size);
    }

    #[test]
    fn em_lengths_use_the_elements_own_font_size() {
        let style = style_of("<p>x</p>", "p { font-size: 20px; padding-top: 2em }", "p");
        assert_eq!(
            style.padding.top,
            LengthPercentage::Length(crate::engine::cryo_impl::css::value::Length::px(40.0))
        );
    }

    #[test]
    fn rem_lengths_use_the_root_font_size() {
        let style = style_of(
            "<div><p>x</p></div>",
            "html { font-size: 10px } div { font-size: 40px } p { padding-top: 2rem }",
            "p",
        );
        assert_eq!(
            style.padding.top,
            LengthPercentage::Length(crate::engine::cryo_impl::css::value::Length::px(20.0)),
            "2rem is 20px, not 80px"
        );
    }

    #[test]
    fn percentages_are_carried_through_unresolved() {
        let style = style_of("<p>x</p>", "p { width: 50%; padding-left: 10% }", "p");
        assert_eq!(
            style.width,
            Size::Definite(LengthPercentage::Percentage(50.0))
        );
        assert_eq!(style.padding.left, LengthPercentage::Percentage(10.0));
    }

    #[test]
    fn a_shorthand_reaches_every_side() {
        let style = style_of("<p>x</p>", "p { padding: 1px 2px 3px 4px }", "p");
        let px = |value: f32| {
            LengthPercentage::Length(crate::engine::cryo_impl::css::value::Length::px(value))
        };
        assert_eq!(style.padding.top, px(1.0));
        assert_eq!(style.padding.right, px(2.0));
        assert_eq!(style.padding.bottom, px(3.0));
        assert_eq!(style.padding.left, px(4.0));
    }

    #[test]
    fn auto_margins_survive_as_auto() {
        let style = style_of("<p>x</p>", "p { margin: 0 auto }", "p");
        assert_eq!(style.margin.left, Margin::Auto);
        assert_eq!(style.margin.right, Margin::Auto);
        assert_eq!(style.margin.top, Margin::ZERO);
    }

    #[test]
    fn a_border_style_of_none_forces_its_width_to_zero() {
        // `border: 10px none red` paints nothing and occupies nothing.
        let style = style_of("<p>x</p>", "p { border: 10px none red }", "p");
        assert_eq!(style.border_width, Sides::all(0.0));

        let drawn = style_of("<p>x</p>", "p { border: 10px solid red }", "p");
        assert_eq!(drawn.border_width, Sides::all(10.0));
    }

    #[test]
    fn border_width_keywords_resolve_to_pixels() {
        let style = style_of("<p>x</p>", "p { border: thick solid red }", "p");
        assert_eq!(style.border_width, Sides::all(5.0));
        let medium = style_of("<p>x</p>", "p { border-style: solid }", "p");
        assert_eq!(
            medium.border_width,
            Sides::all(3.0),
            "medium is the initial"
        );
    }

    #[test]
    fn a_percentage_border_width_is_invalid_and_falls_back() {
        let style = style_of(
            "<p>x</p>",
            "p { border-style: solid; border-width: 50% }",
            "p",
        );
        assert_eq!(style.border_width, Sides::all(3.0), "stays at medium");
    }

    #[test]
    fn border_colour_defaults_to_the_elements_own_colour() {
        // The `currentColor` default, which is why a red-texted box gets
        // a red border without saying so.
        let style = style_of("<p>x</p>", "p { color: red; border-style: solid }", "p");
        assert_eq!(style.border_color, Sides::all(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn current_color_in_a_background_resolves_to_the_colour() {
        let style = style_of(
            "<p>x</p>",
            "p { color: red; background-color: currentColor }",
            "p",
        );
        assert_eq!(style.background_color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn a_numeric_line_height_inherits_as_a_ratio() {
        // The child's larger font must give a larger line box.
        let style = style_of(
            "<div><p>x</p></div>",
            "div { line-height: 1.5; font-size: 10px } p { font-size: 20px }",
            "p",
        );
        assert_eq!(style.line_height, LineHeight::Number(1.5));
        assert!(close(style.line_box_height(), 30.0));
    }

    #[test]
    fn a_percentage_line_height_inherits_as_a_fixed_length() {
        // Resolved on the parent, so the child keeps the parent's height.
        let style = style_of(
            "<div><p>x</p></div>",
            "div { line-height: 150%; font-size: 10px } p { font-size: 20px }",
            "p",
        );
        assert_eq!(style.line_height, LineHeight::Length(15.0));
        assert!(close(style.line_box_height(), 15.0));
    }

    #[test]
    fn floating_blockifies_an_inline_box() {
        let style = style_of("<span>x</span>", "span { float: left }", "span");
        assert_eq!(style.display, Display::Block);
        assert!(style.float.is_floating());
    }

    #[test]
    fn absolute_positioning_blockifies_too() {
        let style = style_of("<span>x</span>", "span { position: absolute }", "span");
        assert_eq!(style.display, Display::Block);
    }

    #[test]
    fn display_none_is_not_blockified_by_a_float() {
        let style = style_of(
            "<span>x</span>",
            "span { display: none; float: left }",
            "span",
        );
        assert_eq!(style.display, Display::None);
    }

    #[test]
    fn an_unparseable_value_leaves_the_property_alone() {
        // `calc()` is not supported yet; the width must stay `auto`
        // rather than collapsing to zero.
        let style = style_of("<p>x</p>", "p { width: calc(100% - 10px) }", "p");
        assert_eq!(style.width, Size::Auto);
    }

    #[test]
    fn max_sizes_distinguish_none_from_a_length() {
        let initial = style_of("<p>x</p>", "", "p");
        assert_eq!(initial.max_width, None);
        let limited = style_of("<p>x</p>", "p { max-width: 10px }", "p");
        assert!(limited.max_width.is_some());
        let explicit = style_of("<p>x</p>", "p { max-width: none }", "p");
        assert_eq!(explicit.max_width, None);
    }

    #[test]
    fn overflow_expands_from_its_shorthand() {
        let style = style_of("<p>x</p>", "p { overflow: hidden }", "p");
        assert_eq!(style.overflow_x, Overflow::Hidden);
        assert_eq!(style.overflow_y, Overflow::Hidden);
    }

    #[test]
    fn side_totals_add_up() {
        let sides = Sides {
            top: 1.0,
            right: 2.0,
            bottom: 4.0,
            left: 8.0,
        };
        assert!(close(sides.horizontal(), 10.0));
        assert!(close(sides.vertical(), 5.0));
    }

    #[test]
    fn the_initial_style_is_the_default() {
        assert_eq!(ComputedStyle::default(), ComputedStyle::INITIAL);
        assert!(close(ComputedStyle::INITIAL.font_size, 16.0));
        assert!(close(ComputedStyle::INITIAL.line_box_height(), 19.2));
    }
}
