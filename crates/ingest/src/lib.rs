//! Streaming PGN ingestion, legal move validation, and bounded pipeline processing.

pub mod pipeline;
pub mod reader;

pub use pipeline::*;
pub use reader::*;
