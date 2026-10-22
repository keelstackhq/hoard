//! Helpers to build packs and indexes by hand and put them into a `PackBackend`.

use std::path::Path;
use std::sync::Arc;

use hoard::envelope::{Header, encode_header};
use hoard::pack::backend::PackBackend;
use hoard::pack::index::{Loc, PackIndex};
use hoard::pack::names::PackNames;

use super::fs_config;

/// A pack file and where each of its artifacts sits.
pub struct Built {
    pub bytes: Vec<u8>,
    pub entries: Vec<(String, Loc)>,
}

/// The envelope of `hash` with `body`, as the writer produces it inside a pack.
pub fn envelope(hash: &str, body: &[u8]) -> Vec<u8> {
    let h = Header {
        v: 1,
        duration_ms: Some(7),
        tag: Some(format!("tag-{hash}")),
        size: body.len() as u64,
        hash: Some(hash.into()),
    };
    let mut out = encode_header(&h);
    out.extend_from_slice(body);
    out
}

/// Envelopes of `items` back to back.
pub fn build_pack(items: &[(&str, &[u8])]) -> Built {
    let mut bytes = Vec::new();
    let mut entries = Vec::new();
    for (hash, body) in items {
        let env = envelope(hash, body);
        entries.push((
            hash.to_string(),
            Loc {
                offset: bytes.len() as u64,
                len: env.len() as u64,
                size: body.len() as u64,
                duration_ms: Some(7),
                tag: Some(format!("tag-{hash}")),
            },
        ));
        bytes.extend_from_slice(&env);
    }
    Built { bytes, entries }
}

/// Uploads `bytes` as `key` through `backend.upload`.
pub async fn put_bytes(
    backend: &PackBackend,
    key: &str,
    bytes: &[u8],
) -> Result<u64, hoard::pack::backend::PackError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("upload");
    std::fs::write(&path, bytes).unwrap();
    backend.upload(key, &path).await
}

/// Uploads the pack as `key` and returns an index naming it (created at `created_ms`).
pub async fn put_pack(
    backend: &PackBackend,
    key: &str,
    built: &Built,
    created_ms: u64,
) -> PackIndex {
    put_bytes(backend, key, &built.bytes).await.unwrap();
    let mut idx = PackIndex::default();
    idx.add_pack(key, created_ms, built.entries.clone());
    idx
}

/// Uploads `idx` as `key`.
pub async fn put_index(backend: &PackBackend, key: &str, idx: &PackIndex) {
    put_bytes(backend, key, &idx.encode()).await.unwrap();
}

/// A backend over an fs store in `root`, and the operator behind it.
pub fn fs_backend(root: &Path) -> (Arc<PackBackend>, opendal::Operator) {
    let op = hoard::store::build_operator(&fs_config(root)).unwrap();
    (Arc::new(PackBackend::Store(op.clone())), op)
}

/// Names for `prefix`, cache version `v1` and the given environment variables.
pub fn names(prefix: &str, env: &[(&str, &str)]) -> Arc<PackNames> {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Arc::new(PackNames::from_env(prefix, "v1", move |k| {
        env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }))
}
