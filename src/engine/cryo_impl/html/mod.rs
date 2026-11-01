//! HTML parsing: bytes of markup in, a [`super::dom::Document`] out.
//!
//! Two stages, kept apart because they fail differently. The tokenizer is
//! a character-level state machine that turns text into tags, comments and
//! characters; tree construction decides where each of those goes, and
//! holds all the knowledge about which elements may nest inside which.
//!
//! Neither stage ever rejects a document. HTML has no parse errors that
//! stop a load — `<p><b>bold<p>more` is malformed and every browser
//! agrees on the exact tree it produces. "Error recovery" is not a
//! fallback path here, it is the main one, and matching other browsers on
//! broken markup is most of what makes a parser usable on the real web.

pub mod entities;
pub mod presentation;
pub mod tokenizer;
pub mod tree;

pub use tree::parse;
