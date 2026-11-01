//! Layout: styled boxes become geometry.
//!
//! This is the first stage that produces numbers rather than structure,
//! and it runs in two steps for a reason:
//!
//! ```text
//!   box_tree    DOM + styles -> boxes, with anonymous boxes inserted
//!    ▼
//!   (layout)    boxes -> fragments with a position and a size
//! ```
//!
//! The box tree is built once per style change; the geometry is computed
//! again whenever the viewport resizes. Keeping them apart means a
//! resize re-measures without rebuilding, and — more importantly while
//! this is being written — the tree's shape can be tested without any
//! numbers being right yet.

pub mod absolute;
pub mod block;
pub mod box_tree;
pub mod flex;
pub mod geometry;
pub mod grid;
pub mod inline;
pub mod intrinsic;
pub mod measure;

pub use block::{layout_document, ContainingBlock};
pub use box_tree::{BoxId, BoxKind, BoxTree, LayoutBox};
pub use geometry::{Fragment, Point, Rect};
pub use inline::{InlineFlow, LineBox};
pub use measure::{RatioMetrics, TextMeasure};
