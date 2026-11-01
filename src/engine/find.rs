//! Native document-search results and the asynchronous reporting port.

use super::EngineError;

/// One-based active index; zero when nothing matches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindMatches {
    pub active: u32,
    pub total: u32,
}

pub type FindHandler = Box<dyn Fn(Result<FindMatches, EngineError>)>;
