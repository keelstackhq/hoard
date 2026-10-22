//! Packing of Turborepo artifacts (design spec §6.5): many artifacts are stored as a few large
//! packs plus one cumulative index, so a job makes few uploads to the GitHub cache.

pub mod index;
