//! Geometry: rectangles, and the fragments layout produces.
//!
//! **Coordinates are absolute**, in CSS pixels from the document's
//! top-left — not relative to each box's parent. Painting walks
//! fragments in any order and hit testing asks "what is under this
//! point?", and both would otherwise have to accumulate ancestor offsets
//! on every query. The cost is that moving a box means re-laying out its
//! subtree, which layout does anyway.
//!
//! A [`Fragment`] holds the **content box** plus the padding and border
//! widths around it, rather than storing all three rectangles. One
//! source of truth per box: the padding box and border box are derived
//! ([`Fragment::padding_box`], [`Fragment::border_box`]), so they cannot
//! disagree with the content box.
//!
//! Why *fragments* and not boxes: one box can produce several. An inline
//! box broken across three lines is three fragments, and so is a block
//! split across pages. The box tree is the input; this is the output.

use crate::engine::cryo_impl::css::computed::Sides;

use super::box_tree::BoxId;

/// A point in absolute document coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A rectangle: its top-left corner and its size.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// Grow by `sides` on each edge — content box to padding box.
    pub fn inflate(&self, sides: &Sides<f32>) -> Self {
        Self {
            x: self.x - sides.left,
            y: self.y - sides.top,
            width: self.width + sides.horizontal(),
            height: self.height + sides.vertical(),
        }
    }

    /// Shrink by `sides` on each edge. Never goes negative: a padding
    /// wider than its box leaves zero content, not a mirrored rectangle.
    pub fn deflate(&self, sides: &Sides<f32>) -> Self {
        Self {
            x: self.x + sides.left,
            y: self.y + sides.top,
            width: (self.width - sides.horizontal()).max(0.0),
            height: (self.height - sides.vertical()).max(0.0),
        }
    }

    /// Whether `point` is inside, counting the top and left edges but
    /// not the bottom and right — so adjacent rectangles do not both
    /// claim the same pixel.
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// The smallest rectangle covering both.
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: self.right().max(other.right()) - x,
            height: self.bottom().max(other.bottom()) - y,
        }
    }
}

/// A laid-out piece of a box: where it is and how big.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    /// The box this came from. Several fragments may share one id.
    pub box_id: BoxId,
    /// The content box, absolute.
    pub content: Rect,
    pub padding: Sides<f32>,
    pub border: Sides<f32>,
    /// The margins as used — after collapsing and after `auto` was
    /// resolved, so this is what was actually spent, not what was asked.
    pub margin: Sides<f32>,
    /// The text this fragment draws, for a text fragment.
    pub text: Option<String>,
    pub children: Vec<Fragment>,
}

impl Fragment {
    /// A fragment with no spacing, for text and anonymous content.
    pub fn bare(box_id: BoxId, content: Rect) -> Self {
        Self {
            box_id,
            content,
            padding: Sides::all(0.0),
            border: Sides::all(0.0),
            margin: Sides::all(0.0),
            text: None,
            children: Vec::new(),
        }
    }

    /// The content box plus padding — where a background paints.
    pub fn padding_box(&self) -> Rect {
        self.content.inflate(&self.padding)
    }

    /// The padding box plus borders — the box's outer edge, and what
    /// `width: border-box` sizes.
    pub fn border_box(&self) -> Rect {
        self.padding_box().inflate(&self.border)
    }

    /// The border box plus margins — the space the box occupies in flow.
    pub fn margin_box(&self) -> Rect {
        self.border_box().inflate(&self.margin)
    }

    /// Move this fragment and everything under it.
    ///
    /// Needed because an atomic inline is laid out before its
    /// position on the line is known: it is measured at the origin,
    /// then slid into place once the line has been broken.
    pub fn translate(&mut self, dx: f32, dy: f32) {
        self.content.x += dx;
        self.content.y += dy;
        for child in &mut self.children {
            child.translate(dx, dy);
        }
    }

    /// This fragment and every fragment under it, in paint order.
    pub fn flatten(&self) -> Vec<&Self> {
        let mut out = vec![self];
        for child in &self.children {
            out.extend(child.flatten());
        }
        out
    }

    /// The rectangle covering this fragment and all its descendants.
    pub fn bounds(&self) -> Rect {
        self.children
            .iter()
            .fold(self.border_box(), |total, child| {
                total.union(&child.bounds())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sides(top: f32, right: f32, bottom: f32, left: f32) -> Sides<f32> {
        Sides {
            top,
            right,
            bottom,
            left,
        }
    }

    #[test]
    fn a_rectangles_edges_are_derived_from_its_origin_and_size() {
        let rect = Rect::new(10.0, 20.0, 30.0, 40.0);
        assert_eq!(rect.right(), 40.0);
        assert_eq!(rect.bottom(), 60.0);
    }

    #[test]
    fn inflating_grows_each_edge_outward() {
        let rect = Rect::new(10.0, 10.0, 100.0, 50.0);
        let grown = rect.inflate(&sides(1.0, 2.0, 3.0, 4.0));
        assert_eq!(grown, Rect::new(6.0, 9.0, 106.0, 54.0));
    }

    #[test]
    fn deflating_is_the_inverse_of_inflating() {
        let rect = Rect::new(10.0, 10.0, 100.0, 50.0);
        let spacing = sides(1.0, 2.0, 3.0, 4.0);
        assert_eq!(rect.inflate(&spacing).deflate(&spacing), rect);
    }

    #[test]
    fn deflating_past_zero_leaves_nothing_not_a_negative_box() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        let squeezed = rect.deflate(&sides(20.0, 20.0, 20.0, 20.0));
        assert_eq!(squeezed.width, 0.0);
        assert_eq!(squeezed.height, 0.0);
        assert!(squeezed.is_empty());
    }

    #[test]
    fn containment_includes_the_top_left_but_excludes_the_bottom_right() {
        // So two adjacent boxes never both claim a point.
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(rect.contains(Point { x: 0.0, y: 0.0 }));
        assert!(rect.contains(Point { x: 9.99, y: 9.99 }));
        assert!(!rect.contains(Point { x: 10.0, y: 5.0 }));
        assert!(!rect.contains(Point { x: 5.0, y: 10.0 }));
        assert!(!rect.contains(Point { x: -0.1, y: 5.0 }));
    }

    #[test]
    fn an_empty_rectangle_contains_nothing() {
        let empty = Rect::new(5.0, 5.0, 0.0, 10.0);
        assert!(empty.is_empty());
        assert!(!empty.contains(Point { x: 5.0, y: 5.0 }));
    }

    #[test]
    fn a_union_covers_both_rectangles() {
        let left = Rect::new(0.0, 0.0, 10.0, 10.0);
        let right = Rect::new(20.0, 5.0, 10.0, 10.0);
        assert_eq!(left.union(&right), Rect::new(0.0, 0.0, 30.0, 15.0));
    }

    #[test]
    fn a_union_with_an_empty_rectangle_is_the_other_one() {
        let rect = Rect::new(1.0, 2.0, 3.0, 4.0);
        assert_eq!(rect.union(&Rect::ZERO), rect);
        assert_eq!(Rect::ZERO.union(&rect), rect);
    }

    #[test]
    fn the_three_box_rectangles_nest_outward() {
        let fragment = Fragment {
            box_id: BoxId::default(),
            content: Rect::new(100.0, 100.0, 200.0, 50.0),
            padding: Sides::all(10.0),
            border: Sides::all(2.0),
            margin: Sides::all(20.0),
            text: None,
            children: Vec::new(),
        };
        assert_eq!(fragment.content, Rect::new(100.0, 100.0, 200.0, 50.0));
        assert_eq!(fragment.padding_box(), Rect::new(90.0, 90.0, 220.0, 70.0));
        assert_eq!(fragment.border_box(), Rect::new(88.0, 88.0, 224.0, 74.0));
        assert_eq!(fragment.margin_box(), Rect::new(68.0, 68.0, 264.0, 114.0));
    }

    #[test]
    fn a_bare_fragment_has_no_spacing() {
        let fragment = Fragment::bare(BoxId::default(), Rect::new(0.0, 0.0, 5.0, 5.0));
        assert_eq!(fragment.border_box(), fragment.content);
        assert_eq!(fragment.margin_box(), fragment.content);
        assert!(fragment.children.is_empty());
    }

    #[test]
    fn flattening_yields_the_fragment_and_its_descendants() {
        let leaf = Fragment::bare(BoxId::default(), Rect::new(0.0, 0.0, 1.0, 1.0));
        let mut parent = Fragment::bare(BoxId::default(), Rect::new(0.0, 0.0, 10.0, 10.0));
        parent.children = vec![leaf.clone(), leaf];
        assert_eq!(parent.flatten().len(), 3);
    }

    #[test]
    fn bounds_cover_children_that_stick_out() {
        let mut parent = Fragment::bare(BoxId::default(), Rect::new(0.0, 0.0, 10.0, 10.0));
        parent.children = vec![Fragment::bare(
            BoxId::default(),
            Rect::new(5.0, 5.0, 100.0, 100.0),
        )];
        assert_eq!(parent.bounds(), Rect::new(0.0, 0.0, 105.0, 105.0));
    }
}
