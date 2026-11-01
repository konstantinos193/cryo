//! Cryo's own web engine: our document tree, our HTML parser, our CSS
//! cascade, our layout, our paint. No `wry`, no WebView2, no Servo — the
//! pipeline from response bytes to a display list is ours end to end.
//!
//! The one thing we do not write is a JavaScript VM. Servo runs
//! SpiderMonkey and Blink runs V8 for the same reason: a competitive JIT
//! and garbage collector is a decade of specialist work that has nothing
//! to do with owning a document pipeline. Scripting links a VM and binds
//! it to the DOM in [`script`] once the rendering path is real.
//!
//! The stages, in the order bytes move through them:
//!
//! ```text
//!   net     bytes + content type off the wire
//!    ▼
//!   html    tokenizer -> tree construction -> dom::Document
//!    ▼
//!   css     tokenizer -> selectors -> cascade -> ComputedStyle per element
//!    ▼
//!   layout  box tree -> block/inline/flex -> fragments with geometry
//!    ▼
//!   paint   fragments -> display list
//!    ▼
//!   raster  display list -> pixels -> the view's OS surface
//! ```
//!
//! Each stage owns one question and hands the next a finished value: the
//! tokenizer never consults style, layout never re-reads markup. That is
//! what keeps a rendering bug findable — a wrong pixel is wrong in exactly
//! one stage, and the stage boundaries are where the tests sit.
//!
//! Nothing here names a platform type. The OS surface the rasteriser
//! finally blits to is reached through a port, so the pipeline above it is
//! testable with no window at all.

pub mod css;

pub mod dom;
pub mod encoding;
pub mod font_file;
pub mod html;
pub mod image;
pub mod layout;
pub mod loader;
pub mod net;
pub mod paint;
pub mod scan;
pub mod svg;
