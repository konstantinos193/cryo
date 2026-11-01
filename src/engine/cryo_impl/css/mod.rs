//! CSS: stylesheet text in, computed styles out.
//!
//! The stages mirror the HTML side, and for the same reason — each one
//! answers a different question and hands the next a finished value:
//!
//! ```text
//!   tokenizer   text -> idents, numbers, strings, delimiters
//!    ▼
//!   parser      tokens -> rules with selectors and declarations
//!    ▼
//!   selector    which elements a rule applies to, and how specifically
//!    ▼
//!   cascade     every declaration that reaches an element -> one winner
//!    ▼
//!   computed    specified values -> absolute values layout can use
//! ```
//!
//! CSS parsing never fails either, but it fails *differently* from HTML:
//! where HTML recovers by restructuring, CSS recovers by discarding. An
//! unknown property, a malformed value or an unparseable selector is
//! dropped and the rest of the stylesheet still applies. That is what
//! makes CSS forward-compatible, and it means every stage here has to
//! know how to skip exactly one construct and carry on.

pub mod cascade;
pub mod computed;
pub mod custom;
pub mod font_face;
pub mod math;
pub mod media;
pub mod parser;
pub mod selector;
pub mod shorthand;
pub mod style_tree;
pub mod tokenizer;
pub mod value;

pub use parser::{parse, parse_declarations, Declaration, Stylesheet};
