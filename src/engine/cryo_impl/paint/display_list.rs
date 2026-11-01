//! The display list: fragments become an ordered list of draw commands.
//!
//! A flat list, not a tree, and the order *is* the z-order — earlier
//! items are painted first and later ones cover them. That is the whole
//! value of this stage: every question about what is on top of what is
//! answered once, here, instead of by a rasteriser trying to walk a tree
//! in the right sequence.
//!
//! Within one box the order is fixed by CSS: **background, then borders,
//! then contents.** It matters in a case that comes up constantly — a
//! box with a background and a child with its own: the child must cover
//! the parent, so the parent's background has to be emitted before the
//! child is visited at all.
//!
//! The list is also deliberately dumb. Items carry absolute rectangles
//! and resolved colours, with no reference back to the box tree or the
//! styles, so a rasteriser needs nothing but this list — and a test can
//! assert on what *would* be drawn without a surface existing.
//!
//! Simplifications, stated:
//! - Every border paints as solid. `dotted`, `dashed` and the 3-D
//!   styles all draw a filled edge, and corners overlap rather than
//!   mitring — visible only where two edges differ in colour.
//! - Stacking contexts are not implemented, so `z-index` and
//!   `position: relative` painting order are ignored: the list follows
//!   tree order.

use crate::engine::cryo_impl::css::computed::{ComputedStyle, Corners, FontStyle, Visibility};
use crate::engine::cryo_impl::css::value::{
    BackgroundBox, BackgroundImage, Color, Gradient, LengthPercentage,
};

use super::super::dom::Document;
use super::super::layout::box_tree::BoxTree;
use super::super::layout::geometry::{Fragment, Rect};
use super::super::scan::FillRule;
use super::super::svg;
use super::clip::ClipShape;

/// One thing to draw.
#[derive(Clone, Debug, PartialEq)]
pub enum DisplayItem {
    /// A filled rectangle: a background, or one edge of a border.
    SolidRect {
        rect: Rect,
        color: Color,
        /// Corner radii, already resolved against `rect` and fitted
        /// to it. All zero for the square case, which is most boxes.
        radius: Corners<f32>,
    },
    /// A rectangle filled with a gradient.
    Gradient {
        rect: Rect,
        gradient: Gradient,
        radius: Corners<f32>,
    },
    /// Start clipping everything that follows, until the matching
    /// [`DisplayItem::PopClip`].
    ///
    /// A clip is a stencil, not a drawing, so it cannot be an item's
    /// own field: it applies to a *run* of items — the box and
    /// everything inside it. A push/pop pair is how a display list
    /// says "this range".
    PushClip {
        shape: ClipShape,
    },
    PopClip,
    /// Filled polygons in page coordinates — what an SVG becomes.
    ///
    /// Carried as geometry rather than as a shape description,
    /// because by the time paint sees it the curves are already
    /// flattened and the viewBox already applied.
    Shape {
        polygons: Vec<Vec<(f32, f32)>>,
        color: Color,
        /// Which crossings count as inside — a path may ask for
        /// even-odd so its subpaths cut holes rather than fill in.
        rule: FillRule,
        /// The element's own box, for bounds and culling.
        rect: Rect,
    },
    /// A cast shadow: a rounded rectangle, softened at the edges.
    ///
    /// Carried as its own item rather than a pre-blurred bitmap so
    /// the rasteriser can decide how to soften it.
    Shadow {
        /// The shape to cast, already offset and spread.
        rect: Rect,
        radius: Corners<f32>,
        blur: f32,
        color: Color,
    },
    /// A rectangle filled with a decoded image, stretched to it.
    ///
    /// The URL is carried rather than the pixels: the display list is
    /// built before anything has been fetched, and the same image is
    /// usually drawn in several places.
    Image {
        rect: Rect,
        url: String,
        radius: Corners<f32>,
    },
    /// A run of text.
    Text {
        /// The text's box. Its width is the measured advance.
        rect: Rect,
        /// Where the baseline sits, from the top of `rect` — glyphs sit
        /// on it, and descenders hang below.
        baseline: f32,
        text: String,
        color: Color,
        font_size: f32,
        /// Extra space after each character, from `letter-spacing`.
        letter_spacing: f32,
        weight: u16,
        style: FontStyle,
        /// The run's font stack, so paint can pick the same face
        /// layout measured with.
        families: Vec<String>,
    },
}

impl DisplayItem {
    /// The area this item covers, for damage tracking and culling.
    pub fn bounds(&self) -> Rect {
        match self {
            Self::SolidRect { rect, .. }
            | Self::Gradient { rect, .. }
            | Self::Image { rect, .. }
            | Self::Shadow { rect, .. }
            | Self::Shape { rect, .. }
            | Self::Text { rect, .. } => *rect,
            // A clip marker draws nothing, so it covers nothing.
            Self::PushClip { .. } | Self::PopClip => Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }
}

/// An ordered list of draw commands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    pub items: Vec<DisplayItem>,
}

impl DisplayList {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The union of every item's bounds — what a full repaint covers.
    pub fn bounds(&self) -> Rect {
        self.items
            .iter()
            .fold(Rect::ZERO, |total, item| total.union(&item.bounds()))
    }
}

/// Build the display list for a laid-out document.
///
/// `baseline_of` supplies where text sits within its line box; it comes
/// from the same [`TextMeasure`](crate::engine::cryo_impl::layout::measure::TextMeasure)
/// layout used, so paint and layout cannot disagree about it.
pub fn build(
    tree: &BoxTree,
    root: &Fragment,
    document: &Document,
    base: &str,
    baseline_of: &dyn Fn(&ComputedStyle) -> f32,
) -> DisplayList {
    let mut list = DisplayList::default();
    paint_fragment(tree, root, document, baseline_of, &mut list);
    resolve_images(&mut list, base);
    list
}

/// Turn each image item's URL into the absolute one it was fetched
/// under.
///
/// A stylesheet writes `url(/hero.jpg)` and the style system keeps it
/// exactly so — it has no idea what document it is styling. This is
/// the first place that does, which is why it happens here and not in
/// `computed`, and why `build` takes a base at all.
fn resolve_images(list: &mut DisplayList, base: &str) {
    for item in &mut list.items {
        if let DisplayItem::Image { url, .. } = item {
            // A reference that will not resolve could not have been
            // fetched either, so the item simply finds no picture and
            // draws nothing.
            if let Some(absolute) = crate::engine::cryo_impl::net::url::resolve(base, url) {
                *url = absolute;
            }
        }
    }
}

fn paint_fragment(
    tree: &BoxTree,
    fragment: &Fragment,
    document: &Document,
    baseline_of: &dyn Fn(&ComputedStyle) -> f32,
    list: &mut DisplayList,
) {
    let style = &tree.get(fragment.box_id).style;
    // `visibility: hidden` keeps its space but draws nothing — itself or
    // its children, unless a child sets `visibility: visible` again.
    let visible = style.visibility == Visibility::Visible;

    // `opacity` applies to the box *and everything inside it*, so the
    // boundary is noted here and every item emitted below it is faded
    // on the way out.
    let from = list.items.len();

    // Clips apply to the same range, but the rasteriser enforces them
    // rather than the list, so they are marked rather than applied.
    // A box may carry both a `clip-path` and a `mask-image`, and they
    // intersect — so this is a list, not a choice.
    let mut clips: Vec<ClipShape> = Vec::new();
    if let Some(path) = style.clip_path {
        clips.push(ClipShape::resolve(path, fragment.border_box()));
    }
    if let Some(mask) = mask_shape(style, fragment.border_box()) {
        clips.push(mask);
    }
    for shape in &clips {
        list.items.push(DisplayItem::PushClip {
            shape: shape.clone(),
        });
    }

    if visible {
        paint_box(fragment, style, list);
        paint_image(tree, fragment, document, style, list);
        paint_svg(tree, fragment, document, style, list);
        if let Some(text) = &fragment.text {
            paint_text(fragment, style, text, baseline_of, list);
        }
    }

    // Children after this box's own background and borders, so they
    // cover it rather than being covered.
    for child in paint_order(tree, &fragment.children) {
        paint_fragment(tree, child, document, baseline_of, list);
    }

    for _ in &clips {
        list.items.push(DisplayItem::PopClip);
    }
    if style.opacity < 1.0 {
        fade(&mut list.items[from..], style.opacity);
    }
}

/// Multiply the alpha of every item in a subtree.
///
/// CSS says a box with `opacity` is composited as a *group*: its
/// contents are drawn together and the result is faded once. Fading
/// each item instead shows through where two of them overlap, which
/// a group would have hidden. That is the stated approximation —
/// correct whenever the contents do not overlap, which is most boxes,
/// and far better than ignoring the property.
fn fade(items: &mut [DisplayItem], alpha: f32) {
    let dim = |color: &mut Color| color.alpha *= alpha;
    for item in items {
        match item {
            DisplayItem::SolidRect { color, .. }
            | DisplayItem::Shadow { color, .. }
            | DisplayItem::Shape { color, .. }
            | DisplayItem::Text { color, .. } => dim(color),
            DisplayItem::Gradient { gradient, .. } => gradient.fade(alpha),
            // An image carries its own per-pixel alpha, which the
            // rasteriser would have to scale as it samples. Noted as
            // a gap rather than silently drawn at full strength.
            DisplayItem::Image { .. } => {}
            // Markers carry no colour.
            DisplayItem::PushClip { .. } | DisplayItem::PopClip => {}
        }
    }
}

/// Draw an `<img>` element's picture.
///
/// A replaced element's content *is* the picture, so unlike every
/// other box there is nothing inside it to paint — this is the only
/// thing that puts an `<img>` on the page. The URL is as written;
/// `resolve_images` makes it absolute afterwards, the same way a
/// background's is.
fn paint_image(
    tree: &BoxTree,
    fragment: &Fragment,
    document: &Document,
    style: &ComputedStyle,
    list: &mut DisplayList,
) {
    let Some(node) = tree.get(fragment.box_id).node else {
        return;
    };
    if !document.is_html_element(node, "img") {
        return;
    }
    let Some(source) = document
        .element(node)
        .and_then(|element| element.attr("src"))
        .filter(|text| !text.trim().is_empty())
    else {
        return;
    };
    let rect = fragment.content;
    if rect.is_empty() {
        return;
    }
    list.items.push(DisplayItem::Image {
        rect,
        url: source.to_owned(),
        radius: resolve_radius(style, fragment.border_box()),
    });
}

/// Draw an `<svg>` element's contents.
///
/// The SVG subtree is in the DOM but not in the box tree in any
/// meaningful way — `<path>` has no CSS box — so this reaches back
/// into the document for it. The element's content box is where the
/// drawing goes, and its `color` is what `currentColor` resolves to,
/// which is how an icon takes the colour of the text beside it.
fn paint_svg(
    tree: &BoxTree,
    fragment: &Fragment,
    document: &Document,
    style: &ComputedStyle,
    list: &mut DisplayList,
) {
    let Some(node) = tree.get(fragment.box_id).node else {
        return;
    };
    if document.element(node).map(|e| e.name.local.as_str()) != Some("svg") {
        return;
    }
    let rect = fragment.content;
    if rect.is_empty() {
        return;
    }
    for shape in svg::render(document, node, rect, style.color) {
        list.items.push(DisplayItem::Shape {
            polygons: shape.polygons,
            color: shape.color,
            rule: shape.rule,
            rect,
        });
    }
}

/// A box's children in the order they are painted.
///
/// CSS paints in layers, not in tree order: anything *positioned*
/// paints above everything that is not, whatever came first in the
/// markup. That is the whole reason `position: relative` is written on
/// a box that is never moved — it lifts the box out of the flow's
/// layer so a later absolute sibling cannot cover it.
///
/// Three buckets, in order:
///
/// 1. positioned boxes with a negative `z-index`
/// 2. everything not positioned, in tree order
/// 3. positioned boxes with `z-index: auto`, `0` or more
///
/// Ties inside a bucket go to tree order, which is what breaks them
/// in CSS too — but the *fragment list* is not in tree order, because
/// layout appends out-of-flow boxes to the end of their containing
/// block's children. So the tie-break reads the box tree instead: box
/// ids are handed out as the tree is walked, so among one parent's
/// children a smaller id means earlier in the markup.
///
/// That distinction is not academic. A decorative `absolute` glow
/// written *before* a `relative` bar is a common shape, and both land
/// in bucket 3. Trusting the fragment order paints the glow last, on
/// top of the thing it was meant to sit behind.
///
/// Stacking *contexts* are not implemented: a `z-index` here competes
/// with its uncles, not only its siblings. That only shows up in a
/// document that nests z-indexes deliberately.
fn paint_order<'a>(tree: &BoxTree, children: &'a [Fragment]) -> Vec<&'a Fragment> {
    /// Which bucket a child falls in, and where inside it.
    fn layer(tree: &BoxTree, fragment: &Fragment) -> (u8, i32, usize) {
        let order = fragment.box_id.index();
        let style = &tree.get(fragment.box_id).style;
        // `is_containing_block` is exactly "positioned": any
        // `position` other than `static`.
        if !style.position.is_containing_block() {
            return (1, 0, order);
        }
        let index = style.z_index.layer();
        if index < 0 {
            (0, index, order)
        } else {
            (2, index, order)
        }
    }

    let mut ordered: Vec<&Fragment> = children.iter().collect();
    ordered.sort_by_key(|fragment| layer(tree, fragment));
    ordered
}

/// Draw a box's own background and border.
///
/// The two are one job because a corner radius couples them: a square
/// box's border is four strips drawn over the background, but a
/// rounded box's is a *ring*, and a ring is drawn by filling the outer
/// rounded rectangle in the border colour and letting the background
/// cover the middle. That reverses the order the two are emitted in,
/// which is why a caller must not choose between them.
fn paint_box(fragment: &Fragment, style: &ComputedStyle, list: &mut DisplayList) {
    // `border-radius` is of the *border* box, and the padding box's
    // corners are what is left after the border is taken off.
    let outer = fragment.border_box();
    let radius = resolve_radius(style, outer);

    // Shadows go under everything the box draws, and the first one
    // written sits on top of the later ones.
    for shadow in style.box_shadow.iter().rev() {
        if !shadow.is_visible() || outer.is_empty() {
            continue;
        }
        let cast = Rect::new(
            outer.x + shadow.offset_x - shadow.spread,
            outer.y + shadow.offset_y - shadow.spread,
            outer.width + shadow.spread * 2.0,
            outer.height + shadow.spread * 2.0,
        );
        list.items.push(DisplayItem::Shadow {
            rect: cast,
            radius: radius.map(|corner| (corner + shadow.spread).max(0.0)),
            blur: shadow.blur,
            color: shadow.color,
        });
    }

    if radius.is_square() {
        // Square edges are four strips drawn over the background, so
        // the order is the one CSS states: layers, then the border.
        paint_background(fragment, style, radius, Keep::AsDeclared, list);
        paint_borders(fragment, style, list);
        return;
    }

    // A rounded border is not strips — it is "fill the outer shape,
    // then let the background cover the middle". A single colour,
    // because a rounded box with four different border colours needs
    // each corner split between two of them, a case no real page
    // asks for.
    let widths = style.border_width;
    let color = style.border_color.top;
    let ring = widths.horizontal() + widths.vertical() > 0.0
        && !color.is_transparent()
        && !outer.is_empty();
    if ring {
        list.items.push(DisplayItem::SolidRect {
            rect: outer,
            color,
            radius,
        });
    }
    // With that fill underneath, a layer clipped to the border box
    // would erase the whole ring rather than let the border show. It
    // is pulled in to the padding box instead: the band the border
    // owns is exactly the difference between the two.
    //
    // Without a ring there is nothing to protect, and the declared
    // clips stand — which is what draws a gradient border, where the
    // border colour is `transparent` on purpose and the rim *is* a
    // background layer showing through it.
    let keep = if ring {
        Keep::InsideTheBorder
    } else {
        Keep::AsDeclared
    };
    paint_background(fragment, style, radius, keep, list);
}

/// Whether a layer's declared `background-clip` is honoured.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    AsDeclared,
    /// A border-box layer is pulled in to the padding box — see
    /// [`paint_box`] on why a rounded ring forces it.
    InsideTheBorder,
}

impl Keep {
    fn apply(self, clip: BackgroundBox) -> BackgroundBox {
        match (self, clip) {
            (Self::InsideTheBorder, BackgroundBox::BorderBox) => BackgroundBox::PaddingBox,
            (_, clip) => clip,
        }
    }
}

/// The padding box's corners: the border box's, less the border that
/// sits in front of each one.
fn inner_radius(radius: Corners<f32>, style: &ComputedStyle) -> Corners<f32> {
    let widths = style.border_width;
    Corners {
        top_left: (radius.top_left - widths.left.max(widths.top)).max(0.0),
        top_right: (radius.top_right - widths.right.max(widths.top)).max(0.0),
        bottom_right: (radius.bottom_right - widths.right.max(widths.bottom)).max(0.0),
        bottom_left: (radius.bottom_left - widths.left.max(widths.bottom)).max(0.0),
    }
}

/// This box's corner radii in pixels, fitted so none overlap.
///
/// Percentages resolve against the box's own width and height — the
/// horizontal radius against the width, the vertical against the
/// height — but only one radius per corner is kept, so the width is
/// what a percentage is taken of.
/// The mask this box paints through, if it has one this engine can
/// build.
///
/// Only a gradient mask is handled. A `url()` mask needs the image's
/// pixels, which arrive separately from the display list, and the
/// honest answer for one is no mask at all rather than a wrong one.
///
/// The painting area is the border box, which is `mask-clip`'s
/// initial value — and the part that bites, since anything a box
/// draws *outside* that box, a cast shadow above all, is masked away
/// entirely.
fn mask_shape(style: &ComputedStyle, border_box: Rect) -> Option<ClipShape> {
    style.mask_image.iter().find_map(|layer| match layer {
        BackgroundImage::Gradient(gradient) => Some(ClipShape::Mask {
            gradient: gradient.clone(),
            rect: border_box,
        }),
        BackgroundImage::Url(_) => None,
    })
}

fn resolve_radius(style: &ComputedStyle, rect: Rect) -> Corners<f32> {
    style
        .border_radius
        .map(|value| match value {
            LengthPercentage::Length(length) => length.value,
            LengthPercentage::Percentage(percent) => percent / 100.0 * rect.width,
        })
        .map(|radius| radius.max(0.0))
        .fitted(rect.width, rect.height)
}

fn paint_background(
    fragment: &Fragment,
    style: &ComputedStyle,
    outer_radius: Corners<f32>,
    keep: Keep,
    list: &mut DisplayList,
) {
    let layers = &style.background_image;
    // The colour takes the clip of the last layer, which is what the
    // spec says and what makes `background: #111 padding-box` keep
    // its colour off the border.
    let colour_clip = keep.apply(BackgroundBox::for_layer(
        &style.background_clip,
        layers.len().saturating_sub(1),
    ));
    // The initial background is transparent, which is most boxes on a
    // page — emitting them would make the list mostly no-ops.
    if !style.background_color.is_transparent() {
        let (rect, radius) = clipped_box(fragment, style, outer_radius, colour_clip);
        if !rect.is_empty() {
            list.items.push(DisplayItem::SolidRect {
                rect,
                color: style.background_color,
                radius,
            });
        }
    }
    // The layers paint over the colour, and the *last* of them is the
    // bottom one — CSS lists them topmost first, which is the reverse
    // of the order a painter wants.
    for (index, layer) in layers.iter().enumerate().rev() {
        let clip = keep.apply(BackgroundBox::for_layer(&style.background_clip, index));
        let (rect, radius) = clipped_box(fragment, style, outer_radius, clip);
        if rect.is_empty() {
            continue;
        }
        match layer {
            BackgroundImage::Gradient(gradient) if !gradient.is_transparent() => {
                list.items.push(DisplayItem::Gradient {
                    rect,
                    gradient: gradient.clone(),
                    radius,
                });
            }
            BackgroundImage::Url(url) => list.items.push(DisplayItem::Image {
                rect,
                url: url.clone(),
                radius,
            }),
            BackgroundImage::Gradient(_) => {}
        }
    }
}

/// The rectangle and corner radii a layer with this clip is painted
/// in.
///
/// Each box inward loses the edge in front of it, and its corners
/// tighten by the same amount — a 12px radius with a 2px border is a
/// 10px radius on the padding box, or the two curves would not be
/// concentric.
fn clipped_box(
    fragment: &Fragment,
    style: &ComputedStyle,
    outer_radius: Corners<f32>,
    clip: BackgroundBox,
) -> (Rect, Corners<f32>) {
    match clip {
        BackgroundBox::BorderBox => (fragment.border_box(), outer_radius),
        BackgroundBox::PaddingBox => (fragment.padding_box(), inner_radius(outer_radius, style)),
        BackgroundBox::ContentBox => {
            let padding = fragment.padding;
            let inner = inner_radius(outer_radius, style);
            (
                fragment.content,
                Corners {
                    top_left: (inner.top_left - padding.left.max(padding.top)).max(0.0),
                    top_right: (inner.top_right - padding.right.max(padding.top)).max(0.0),
                    bottom_right: (inner.bottom_right - padding.right.max(padding.bottom)).max(0.0),
                    bottom_left: (inner.bottom_left - padding.left.max(padding.bottom)).max(0.0),
                },
            )
        }
    }
}

/// The four border edges, each a filled rectangle.
///
/// The edges span the full border box, so adjacent edges overlap at the
/// corners — see the module's note.
fn paint_borders(fragment: &Fragment, style: &ComputedStyle, list: &mut DisplayList) {
    let outer = fragment.border_box();
    let widths = fragment.border;
    let colors = style.border_color;

    let edges = [
        (
            Rect::new(outer.x, outer.y, outer.width, widths.top),
            colors.top,
        ),
        (
            Rect::new(
                outer.x,
                outer.bottom() - widths.bottom,
                outer.width,
                widths.bottom,
            ),
            colors.bottom,
        ),
        (
            Rect::new(outer.x, outer.y, widths.left, outer.height),
            colors.left,
        ),
        (
            Rect::new(
                outer.right() - widths.right,
                outer.y,
                widths.right,
                outer.height,
            ),
            colors.right,
        ),
    ];
    for (rect, color) in edges {
        if rect.is_empty() || color.is_transparent() {
            continue;
        }
        list.items.push(DisplayItem::SolidRect {
            rect,
            color,
            radius: Corners::all(0.0),
        });
    }
}

fn paint_text(
    fragment: &Fragment,
    style: &ComputedStyle,
    text: &str,
    baseline_of: &dyn Fn(&ComputedStyle) -> f32,
    list: &mut DisplayList,
) {
    // Whitespace draws nothing, but still carries geometry for
    // selection — so it is laid out and simply not painted.
    if text.trim().is_empty() || style.color.is_transparent() {
        return;
    }
    list.items.push(DisplayItem::Text {
        rect: fragment.content,
        baseline: baseline_of(style),
        text: text.to_owned(),
        color: style.color,
        font_size: style.font_size,
        letter_spacing: style.letter_spacing,
        weight: style.font_weight,
        style: style.font_style,
        families: style.font_family.clone(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::cascade::{Origin, RuleSet};
    use crate::engine::cryo_impl::css::computed::ComputeContext;
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::css::style_tree::StyleTree;
    use crate::engine::cryo_impl::html::parse as parse_html;
    use crate::engine::cryo_impl::layout::block::layout_document;
    use crate::engine::cryo_impl::layout::measure::{RatioMetrics, TextMeasure};

    const BLOCKS: &str = concat!(
        "html, body, div, p, section { display: block }",
        "head { display: none }",
    );

    /// Lay `markup` out and build its display list.
    fn paint(markup: &str, css: &str) -> DisplayList {
        let doc = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::UserAgent, BLOCKS);
        rules.insert_css(Origin::Author, css);
        let styles = StyleTree::resolve(
            &doc,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        let tree = BoxTree::build(&doc, &styles);
        let metrics = RatioMetrics::DEFAULT;
        let root = layout_document(&tree, 800.0, 600.0, &metrics).expect("a root fragment");
        build(&tree, &root, &doc, "https://test.invalid/", &|style| {
            metrics.baseline(style)
        })
    }

    /// Just the solid rectangles, as `(rect, colour)`.
    fn rects(list: &DisplayList) -> Vec<(Rect, Color)> {
        list.items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::SolidRect { rect, color, .. } => Some((*rect, *color)),
                DisplayItem::Text { .. }
                | DisplayItem::Gradient { .. }
                | DisplayItem::Image { .. }
                | DisplayItem::Shadow { .. }
                | DisplayItem::Shape { .. }
                | DisplayItem::PushClip { .. }
                | DisplayItem::PopClip => None,
            })
            .collect()
    }

    /// Just the text runs.
    fn texts(list: &DisplayList) -> Vec<String> {
        list.items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Text { text, .. } => Some(text.clone()),
                DisplayItem::SolidRect { .. }
                | DisplayItem::Gradient { .. }
                | DisplayItem::Image { .. }
                | DisplayItem::Shadow { .. }
                | DisplayItem::Shape { .. }
                | DisplayItem::PushClip { .. }
                | DisplayItem::PopClip => None,
            })
            .collect()
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    /// The colours of the solid rectangles, in paint order.
    fn rect_colors(list: &DisplayList) -> Vec<Color> {
        list.items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::SolidRect { color, .. } => Some(*color),
                DisplayItem::Text { .. }
                | DisplayItem::Gradient { .. }
                | DisplayItem::Image { .. }
                | DisplayItem::Shadow { .. }
                | DisplayItem::Shape { .. }
                | DisplayItem::PushClip { .. }
                | DisplayItem::PopClip => None,
            })
            .collect()
    }

    /// The radii of the first solid rectangle painted.
    fn first_radius(list: &DisplayList) -> Corners<f32> {
        list.items
            .iter()
            .find_map(|item| match item {
                DisplayItem::SolidRect { radius, .. } => Some(*radius),
                _ => None,
            })
            .expect("a filled rectangle")
    }

    #[test]
    fn a_shadow_is_cast_under_the_box_that_owns_it() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; background: white;
                   box-shadow: 0 4px 12px rgba(0,0,0,0.2) }",
        );
        let shadow = list
            .items
            .iter()
            .position(|item| matches!(item, DisplayItem::Shadow { .. }))
            .expect("a shadow");
        let background = list
            .items
            .iter()
            .position(|item| matches!(item, DisplayItem::SolidRect { .. }))
            .expect("a background");
        assert!(shadow < background, "the shadow goes underneath");

        let DisplayItem::Shadow { rect, blur, .. } = &list.items[shadow] else {
            unreachable!()
        };
        assert!(close(*blur, 12.0));
        assert!(close(rect.y, 4.0), "offset down by its y");
        assert!(close(rect.width, 100.0), "no spread, so the box's size");
    }

    #[test]
    fn a_spread_grows_the_shadow_and_its_corners() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; border-radius: 8px;
                   box-shadow: 0 0 0 5px red }",
        );
        let Some(DisplayItem::Shadow { rect, radius, .. }) = list
            .items
            .iter()
            .find(|item| matches!(item, DisplayItem::Shadow { .. }))
        else {
            panic!("a shadow");
        };
        assert!(close(rect.width, 110.0), "five each side");
        assert!(close(rect.x, -5.0));
        assert_eq!(*radius, Corners::all(13.0), "the corners grow with it");
    }

    #[test]
    fn an_inset_shadow_is_read_and_not_drawn() {
        // A stated gap: drawing one needs masking to the inside.
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; box-shadow: inset 0 2px 4px red }",
        );
        assert!(!list
            .items
            .iter()
            .any(|item| matches!(item, DisplayItem::Shadow { .. })));
    }

    #[test]
    fn several_shadows_paint_with_the_first_on_top() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px;
                   box-shadow: 0 1px 0 red, 0 8px 0 blue }",
        );
        let colors: Vec<Color> = list
            .items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Shadow { color, .. } => Some(*color),
                _ => None,
            })
            .collect();
        // Painted last is on top, so the first written comes second.
        assert_eq!(colors, vec![Color::rgb(0, 0, 255), Color::rgb(255, 0, 0)]);
    }

    #[test]
    fn a_border_radius_reaches_the_display_list() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; background: red; border-radius: 8px }",
        );
        assert_eq!(first_radius(&list), Corners::all(8.0));
    }

    #[test]
    fn the_radius_shorthand_distributes_like_the_side_ones() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; background: red;
                   border-radius: 1px 2px 3px 4px }",
        );
        assert_eq!(
            first_radius(&list),
            Corners {
                top_left: 1.0,
                top_right: 2.0,
                bottom_right: 3.0,
                bottom_left: 4.0,
            }
        );
    }

    #[test]
    fn two_radii_on_one_edge_are_scaled_to_fit_together() {
        // 60 + 60 does not fit across 100, so both shrink by the same
        // factor and the shape stays symmetrical.
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 100px; background: red; border-radius: 60px }",
        );
        assert_eq!(first_radius(&list), Corners::all(50.0));
    }

    #[test]
    fn a_percentage_radius_is_of_the_box() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 100px; background: red; border-radius: 50% }",
        );
        assert_eq!(first_radius(&list), Corners::all(50.0));
    }

    #[test]
    fn a_rounded_box_draws_its_border_as_one_ring_under_the_background() {
        // Four separate edge strips cannot make a rounded border, so
        // the outer shape is filled first and the background covers
        // the middle of it.
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; background: white;
                   border: 4px solid red; border-radius: 10px }",
        );
        let rects: Vec<_> = list
            .items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::SolidRect {
                    rect,
                    color,
                    radius,
                } => Some((*rect, *color, *radius)),
                _ => None,
            })
            .collect();
        assert_eq!(rects.len(), 2, "one ring and one background");
        assert_eq!(rects[0].1, Color::rgb(255, 0, 0), "the border first");
        assert_eq!(rects[1].1, Color::rgb(255, 255, 255), "then over it");
        assert!(rects[0].0.width > rects[1].0.width, "the ring is outside");
        // The inner corners are tighter by the border they sit behind.
        assert_eq!(rects[0].2, Corners::all(10.0));
        assert_eq!(rects[1].2, Corners::all(6.0));
    }

    #[test]
    fn a_transparent_border_lets_a_border_box_layer_show_through_it() {
        // The gradient-border idiom. The outer layer fills the whole
        // border box and the inner one covers everything past the
        // border, leaving a 4px rim of the outer one showing.
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; border: 4px solid transparent;
                   border-radius: 10px;
                   background: linear-gradient(#111, #111) padding-box,
                               linear-gradient(#f00, #f00) border-box }",
        );
        let rects: Vec<_> = list
            .items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Gradient { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(rects.len(), 2, "both layers paint: {:?}", list.items);
        // Bottom first: the border-box layer, then the padding-box
        // one inside it.
        assert!(
            close(rects[0].width, 108.0),
            "the border box: {:?}",
            rects[0]
        );
        assert!(
            close(rects[1].width, 100.0),
            "the padding box: {:?}",
            rects[1]
        );
    }

    #[test]
    fn an_opaque_rounded_border_still_wins_over_a_border_box_layer() {
        // Pulling the layer in is what keeps the ring visible; left
        // at the border box it would cover the border entirely.
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; border: 4px solid red;
                   border-radius: 10px;
                   background: linear-gradient(#111, #111) border-box }",
        );
        let gradient = list
            .items
            .iter()
            .find_map(|item| match item {
                DisplayItem::Gradient { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("the layer paints");
        assert!(close(gradient.width, 100.0), "pulled in: {gradient:?}");
    }

    #[test]
    fn a_square_box_still_draws_four_border_edges() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; background: white; border: 4px solid red }",
        );
        let reds = list
            .items
            .iter()
            .filter(|item| {
                matches!(item, DisplayItem::SolidRect { color, .. } if *color == Color::rgb(255, 0, 0))
            })
            .count();
        assert_eq!(reds, 4, "top, right, bottom, left");
    }

    #[test]
    fn a_positioned_box_paints_above_an_unpositioned_later_sibling() {
        // Tree order says green last; CSS says the positioned box
        // wins. This is why `position: relative` is written on boxes
        // that are never moved.
        let list = paint(
            "<div id=a></div><div id=b></div>",
            "#a { position: relative; background: red; height: 10px }
             #b { background: lime; height: 10px }",
        );
        let colors = rect_colors(&list);
        let red = colors.iter().position(|c| *c == Color::rgb(255, 0, 0));
        let green = colors.iter().position(|c| *c == Color::rgb(0, 255, 0));
        assert!(green < red, "the unpositioned box paints first: {colors:?}");
    }

    #[test]
    fn an_absolute_box_written_first_paints_under_a_relative_one_written_after() {
        // Both are positioned with no z-index, so markup order
        // decides — but layout appends out-of-flow boxes to the end
        // of the child list, which would say the opposite. A page
        // writes this shape whenever a decorative glow sits behind a
        // bar; getting it backwards covers the bar with the glow.
        let list = paint(
            "<div id=w><div id=glow></div><div id=bar></div></div>",
            "#w { position: relative; height: 20px }
             #glow { position: absolute; inset: 0; background: lime }
             #bar { position: relative; background: red; height: 20px }",
        );
        let colors = rect_colors(&list);
        let red = colors.iter().position(|c| *c == Color::rgb(255, 0, 0));
        let green = colors.iter().position(|c| *c == Color::rgb(0, 255, 0));
        assert!(green < red, "the glow is underneath: {colors:?}");
    }

    #[test]
    fn z_index_orders_positioned_siblings() {
        let list = paint(
            "<div id=a></div><div id=b></div>",
            "#a { position: relative; z-index: 2; background: red; height: 10px }
             #b { position: relative; z-index: 1; background: lime; height: 10px }",
        );
        let colors = rect_colors(&list);
        let red = colors.iter().position(|c| *c == Color::rgb(255, 0, 0));
        let green = colors.iter().position(|c| *c == Color::rgb(0, 255, 0));
        assert!(green < red, "z-index 1 before 2: {colors:?}");
    }

    #[test]
    fn a_negative_z_index_paints_below_unpositioned_content() {
        let list = paint(
            "<div id=a></div><div id=b></div>",
            "#a { position: relative; z-index: -1; background: red; height: 10px }
             #b { background: lime; height: 10px }",
        );
        let colors = rect_colors(&list);
        let red = colors.iter().position(|c| *c == Color::rgb(255, 0, 0));
        let green = colors.iter().position(|c| *c == Color::rgb(0, 255, 0));
        assert!(red < green, "the negative layer is underneath: {colors:?}");
    }

    #[test]
    fn equal_z_index_keeps_tree_order() {
        let list = paint(
            "<div id=a></div><div id=b></div>",
            "#a { position: relative; background: red; height: 10px }
             #b { position: relative; background: lime; height: 10px }",
        );
        let colors = rect_colors(&list);
        let red = colors.iter().position(|c| *c == Color::rgb(255, 0, 0));
        let green = colors.iter().position(|c| *c == Color::rgb(0, 255, 0));
        assert!(red < green, "a tie falls back to the markup: {colors:?}");
    }

    #[test]
    fn a_page_with_nothing_painted_produces_an_empty_list() {
        // Every box is transparent with no border and no text.
        let list = paint("<div></div>", "");
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn a_background_paints_over_the_padding_box() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 50px; padding: 10px; background: red }",
        );
        let painted = rects(&list);
        assert_eq!(painted.len(), 1);
        let (rect, color) = painted[0];
        assert_eq!(color, Color::rgb(255, 0, 0));
        assert!(close(rect.width, 120.0), "100 content + 20 padding");
        assert!(close(rect.height, 70.0));
    }

    #[test]
    fn a_transparent_background_paints_nothing() {
        let list = paint(
            "<div></div>",
            "div { height: 10px; background: transparent }",
        );
        assert!(rects(&list).is_empty());
    }

    #[test]
    fn a_background_on_a_zero_sized_box_paints_nothing() {
        let list = paint("<div></div>", "div { width: 0; background: red }");
        assert!(rects(&list).is_empty());
    }

    #[test]
    fn a_border_paints_four_edges() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 100px; border: 5px solid blue }",
        );
        let painted = rects(&list);
        assert_eq!(painted.len(), 4);
        assert!(painted
            .iter()
            .all(|(_, color)| *color == Color::rgb(0, 0, 255)));
    }

    #[test]
    fn border_edges_span_the_border_box() {
        let list = paint(
            "<div></div>",
            "div { width: 100px; height: 100px; border: 5px solid blue }",
        );
        let painted = rects(&list);
        let top = painted[0].0;
        assert!(close(top.width, 110.0), "100 + both 5px edges");
        assert!(close(top.height, 5.0));
        let left = painted[2].0;
        assert!(close(left.width, 5.0));
        assert!(close(left.height, 110.0));
    }

    #[test]
    fn a_zero_width_border_paints_nothing() {
        let list = paint("<div></div>", "div { height: 10px; border: 0 solid red }");
        assert!(rects(&list).is_empty());
    }

    #[test]
    fn a_border_style_of_none_paints_nothing() {
        // The computed width is zero, so there is no edge to draw.
        let list = paint("<div></div>", "div { height: 10px; border: 5px none red }");
        assert!(rects(&list).is_empty());
    }

    #[test]
    fn only_the_sides_with_a_width_are_painted() {
        let list = paint(
            "<div></div>",
            "div { height: 10px; border-style: solid; border-width: 0; border-top-width: 3px }",
        );
        assert_eq!(rects(&list).len(), 1);
    }

    #[test]
    fn the_background_is_painted_before_the_border() {
        // So a border draws over its own background, not under it.
        let list = paint(
            "<div></div>",
            "div { height: 10px; background: red; border: 1px solid blue }",
        );
        let painted = rects(&list);
        assert_eq!(painted[0].1, Color::rgb(255, 0, 0), "background first");
        assert!(painted[1..]
            .iter()
            .all(|(_, c)| *c == Color::rgb(0, 0, 255)));
    }

    #[test]
    fn a_child_is_painted_after_its_parent() {
        // The rule that makes a child's background cover its parent's.
        let list = paint(
            "<div><p></p></div>",
            "div { background: red; height: 100px } p { background: blue; height: 10px }",
        );
        let painted = rects(&list);
        assert_eq!(painted.len(), 2);
        assert_eq!(painted[0].1, Color::rgb(255, 0, 0), "parent first");
        assert_eq!(painted[1].1, Color::rgb(0, 0, 255), "child over it");
    }

    #[test]
    fn siblings_are_painted_in_tree_order() {
        let list = paint(
            "<div id=a></div><div id=b></div>",
            "div { height: 10px } #a { background: red } #b { background: blue }",
        );
        let painted = rects(&list);
        assert_eq!(painted[0].1, Color::rgb(255, 0, 0));
        assert_eq!(painted[1].1, Color::rgb(0, 0, 255));
    }

    #[test]
    fn text_is_painted_with_its_colour_and_font() {
        let list = paint("<p>hello</p>", "p { color: red; font-size: 20px }");
        let [DisplayItem::Text {
            text,
            color,
            font_size,
            ..
        }] = list.items.as_slice()
        else {
            panic!("expected one text item, got {:?}", list.items);
        };
        assert_eq!(text, "hello");
        assert_eq!(*color, Color::rgb(255, 0, 0));
        assert!(close(*font_size, 20.0));
    }

    #[test]
    fn text_carries_its_weight_and_style() {
        let list = paint("<p>x</p>", "p { font-weight: bold; font-style: italic }");
        let [DisplayItem::Text { weight, style, .. }] = list.items.as_slice() else {
            panic!("expected one text item");
        };
        assert_eq!(*weight, 700);
        assert_eq!(*style, FontStyle::Italic);
    }

    #[test]
    fn the_baseline_sits_inside_the_text_box() {
        let list = paint("<p>x</p>", "p { font-size: 20px }");
        let [DisplayItem::Text { rect, baseline, .. }] = list.items.as_slice() else {
            panic!("expected one text item");
        };
        assert!(*baseline > 0.0);
        assert!(*baseline <= rect.height);
    }

    #[test]
    fn whitespace_between_words_paints_nothing() {
        let list = paint("<p>a b</p>", "");
        // Two words painted; the space between them is not.
        assert_eq!(texts(&list), ["a", "b"]);
    }

    #[test]
    fn wrapped_text_paints_one_run_per_line() {
        let list = paint("<p>aaa bbb ccc</p>", "p { width: 30px }");
        assert_eq!(texts(&list), ["aaa", "bbb", "ccc"]);
    }

    #[test]
    fn transparent_text_paints_nothing() {
        let list = paint("<p>hidden</p>", "p { color: transparent }");
        assert!(texts(&list).is_empty());
    }

    #[test]
    fn visibility_hidden_paints_neither_the_box_nor_its_text() {
        let list = paint(
            "<div>text</div>",
            "div { visibility: hidden; background: red; border: 1px solid blue }",
        );
        assert!(list.is_empty(), "got {:?}", list.items);
    }

    #[test]
    fn visibility_hidden_still_lets_a_visible_child_paint() {
        // It is an inherited property, so a child can turn it back on.
        let list = paint(
            "<div><p>shown</p></div>",
            "div { visibility: hidden } p { visibility: visible }",
        );
        assert_eq!(texts(&list), ["shown"]);
    }

    #[test]
    fn display_none_never_reaches_paint_at_all() {
        // It generated no box, so there is nothing here to skip.
        let list = paint("<div>gone</div>", "div { display: none; background: red }");
        assert!(list.is_empty());
    }

    #[test]
    fn an_items_bounds_are_its_rectangle() {
        let list = paint(
            "<div></div>",
            "div { width: 50px; height: 20px; background: red }",
        );
        let item = &list.items[0];
        assert_eq!(item.bounds(), Rect::new(0.0, 0.0, 50.0, 20.0));
    }

    #[test]
    fn the_lists_bounds_cover_every_item() {
        let list = paint(
            "<div id=a></div><div id=b></div>",
            "div { height: 30px; background: red }",
        );
        let bounds = list.bounds();
        assert!(close(bounds.height, 60.0), "both boxes, got {bounds:?}");
        assert!(close(bounds.width, 800.0));
    }

    #[test]
    fn an_empty_lists_bounds_are_empty() {
        assert_eq!(DisplayList::default().bounds(), Rect::ZERO);
    }

    #[test]
    fn a_realistic_page_paints_in_a_sensible_order() {
        let list = paint(
            "<div class=card><p>Title</p><p>Body text</p></div>",
            concat!(
                "body { background: white }",
                ".card { background: #eee; border: 1px solid #999; padding: 10px }",
                "p { color: #222 }",
            ),
        );
        let kinds: Vec<&str> = list
            .items
            .iter()
            .map(|item| match item {
                DisplayItem::SolidRect { .. } => "rect",
                DisplayItem::Text { .. } => "text",
                DisplayItem::Gradient { .. } => "gradient",
                DisplayItem::Image { .. } => "image",
                DisplayItem::Shadow { .. } => "shadow",
                DisplayItem::Shape { .. } => "shape",
                DisplayItem::PushClip { .. } => "push-clip",
                DisplayItem::PopClip => "pop-clip",
            })
            .collect();
        // body background, card background, four card borders, then text.
        assert_eq!(kinds[0], "rect");
        assert!(kinds.iter().filter(|k| **k == "rect").count() >= 6);
        assert_eq!(texts(&list), ["Title", "Body", "text"]);
        // No text is painted before the card's own background.
        let first_text = kinds.iter().position(|k| *k == "text").expect("some text");
        assert!(first_text >= 6, "backgrounds and borders come first");
    }
}
