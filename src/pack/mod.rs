//! Packing of Turborepo artifacts (design spec §6.5): many artifacts are stored as a few large
//! packs plus one cumulative index, so a job makes few uploads to the GitHub cache.

pub mod backend;
pub mod index;
pub mod names;
pub mod reader;
pub mod writer;

use std::sync::Arc;

/// What the server needs when Turborepo artifacts are packed: the reader (index, ranged reads)
/// and, in `read-write` mode, the writer. Without a writer every PUT counts as `put_errors`.
pub struct Packs {
    pub reader: Arc<reader::PackReader>,
    pub writer: Option<writer::PackWriter>,
}
