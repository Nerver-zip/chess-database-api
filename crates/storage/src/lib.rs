//! Compact game storage, binary encoding, persistent CGB3 containers, and random-access retrieval.

pub mod dataset;
pub mod format;
pub mod lock;
pub mod lookup;
pub mod search;
pub mod shard;

pub use dataset::*;
pub use format::*;
pub use lock::*;
pub use lookup::*;
pub use shard::*;
