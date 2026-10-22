//! The pack index: which artifact hash lives in which pack at which byte range (spec §6.5).
//!
//! Wire format: one zstd frame of compact JSON,
//! `{"v":1,"packs":[[key,created_ms,last_used_ms],…],"entries":{hash:[pack#,offset,len,size,duration_ms|null,tag|null]}}`.
//! Packs are kept ordered oldest first by `(created_ms, key)`; `pack#` indexes that order.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Most packs an index may name after `cap`.
pub const MAX_PACKS: usize = 1000;
/// Most entries an index may hold after `cap`.
pub const MAX_ENTRIES: usize = 200_000;
/// A pack not used for this long is dropped by `cap`. Jobs that only read write no index, so
/// `last used` can lag behind real use; an evicted pack is detected on read.
pub const MAX_UNUSED: Duration = Duration::from_secs(30 * 24 * 3600);
/// Decoded (JSON) size limit of an index.
pub const MAX_DECODED: u64 = 64 << 20;

const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index is not a valid zstd frame: {0}")]
    Zstd(String),
    #[error("index is not valid JSON: {0}")]
    Json(String),
    #[error("index is larger than {MAX_DECODED} bytes decoded")]
    TooLarge,
    #[error("unsupported index version {0}")]
    Version(u32),
    #[error("entry {hash} names pack #{pack}, but the index has {packs} packs")]
    PackOutOfRange {
        hash: String,
        pack: usize,
        packs: usize,
    },
}

/// Location of one artifact (its HRD1 envelope) inside a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loc {
    pub offset: u64,
    pub len: u64,
    pub size: u64,
    pub duration_ms: Option<u64>,
    pub tag: Option<String>,
}

/// A lookup result: the pack key and the location in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub pack: Arc<str>,
    pub loc: Loc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackMeta {
    key: Arc<str>,
    created_ms: u64,
    last_used_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PackIndex {
    /// Ordered oldest first by `(created_ms, key)`.
    packs: Vec<PackMeta>,
    entries: HashMap<String, (usize, Loc)>,
}

type WireEntry<S> = (usize, u64, u64, u64, Option<u64>, Option<S>);

#[derive(Serialize)]
struct WireOut<'a> {
    v: u32,
    packs: Vec<(&'a str, u64, u64)>,
    entries: BTreeMap<&'a str, WireEntry<&'a str>>,
}

#[derive(Deserialize)]
struct WireIn {
    v: u32,
    packs: Vec<(String, u64, u64)>,
    entries: HashMap<String, WireEntry<String>>,
}

/// Reader that fails (and remembers it) once more than `left` bytes would be read.
struct Limited<R> {
    inner: R,
    left: u64,
    exceeded: Cell<bool>,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let n = self.inner.read(buf)?;
        if n as u64 > self.left {
            self.exceeded.set(true);
            return Err(io::Error::other("index too large"));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// Union the pack metadata of several lists: one pack per key, created = min, last used = max;
/// sorted oldest first by `(created_ms, key)`.
fn union_packs<'a>(lists: impl Iterator<Item = &'a PackMeta>) -> Vec<PackMeta> {
    let mut metas: HashMap<Arc<str>, PackMeta> = HashMap::new();
    for p in lists {
        metas
            .entry(p.key.clone())
            .and_modify(|m| {
                m.created_ms = m.created_ms.min(p.created_ms);
                m.last_used_ms = m.last_used_ms.max(p.last_used_ms);
            })
            .or_insert_with(|| p.clone());
    }
    let mut packs: Vec<PackMeta> = metas.into_values().collect();
    packs.sort_by(|a, b| (a.created_ms, &a.key).cmp(&(b.created_ms, &b.key)));
    packs
}

fn positions(packs: &[PackMeta]) -> HashMap<Arc<str>, usize> {
    packs
        .iter()
        .enumerate()
        .map(|(i, p)| (p.key.clone(), i))
        .collect()
}

impl PackIndex {
    pub fn get(&self, hash: &str) -> Option<Entry> {
        let (p, loc) = self.entries.get(hash)?;
        Some(Entry {
            pack: self.packs[*p].key.clone(),
            loc: loc.clone(),
        })
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.entries.contains_key(hash)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn pack_count(&self) -> usize {
        self.packs.len()
    }

    /// Add a freshly uploaded pack; its entries override older ones.
    pub fn add_pack(&mut self, key: &str, created_ms: u64, entries: Vec<(String, Loc)>) {
        if entries.is_empty() {
            return;
        }
        let one = PackIndex {
            packs: vec![PackMeta {
                key: key.into(),
                created_ms,
                last_used_ms: created_ms,
            }],
            entries: entries.into_iter().map(|(h, l)| (h, (0, l))).collect(),
        };
        self.merge(&one);
    }

    /// Union of both indexes. A hash present in several packs resolves to the newest pack by
    /// `(created_ms, key)`; a pack present in both gets `last_used = max`, `created = min`.
    /// The result does not depend on the argument order.
    pub fn merge(&mut self, other: &PackIndex) {
        let packs = union_packs(self.packs.iter().chain(&other.packs));
        let pos = positions(&packs);
        let remap_self: Vec<usize> = self.packs.iter().map(|p| pos[&p.key]).collect();

        let mut entries: HashMap<String, (usize, Loc)> = std::mem::take(&mut self.entries)
            .into_iter()
            .map(|(h, (p, l))| (h, (remap_self[p], l)))
            .collect();
        for (h, (p, l)) in &other.entries {
            let np = pos[&other.packs[*p].key];
            match entries.get_mut(h) {
                Some(cur) => {
                    let wins = np > cur.0
                        || (np == cur.0
                            && (l.offset, l.len, l.size) > (cur.1.offset, cur.1.len, cur.1.size));
                    if wins {
                        *cur = (np, l.clone());
                    }
                }
                None => {
                    entries.insert(h.clone(), (np, l.clone()));
                }
            }
        }
        self.packs = packs;
        self.entries = entries;
    }

    /// `last_used = max(last_used, now_ms)` for the named pack.
    pub fn touch(&mut self, pack: &str, now_ms: u64) {
        if let Some(p) = self.packs.iter_mut().find(|p| &*p.key == pack) {
            p.last_used_ms = p.last_used_ms.max(now_ms);
        }
    }

    pub fn remove_hashes(&mut self, hashes: &HashSet<String>) {
        self.entries.retain(|h, _| !hashes.contains(h));
        self.retain_packs(|_, _| true);
    }

    /// Drop the named packs and their entries.
    pub fn drop_packs(&mut self, missing: &HashSet<String>) {
        self.retain_packs(|_, p| !missing.contains(&*p.key));
    }

    /// Drop packs unused for longer than `max_unused`, then least recently used packs (ties:
    /// older created first) until at most `max_packs` packs and `max_entries` entries remain.
    pub fn cap(&mut self, now_ms: u64, max_unused: Duration, max_packs: usize, max_entries: usize) {
        let unused_ms = u64::try_from(max_unused.as_millis()).unwrap_or(u64::MAX);
        let cutoff = now_ms.saturating_sub(unused_ms);
        self.retain_packs(|_, p| p.last_used_ms >= cutoff);
        if self.packs.len() <= max_packs && self.entries.len() <= max_entries {
            return;
        }
        let mut counts = vec![0usize; self.packs.len()];
        for (p, _) in self.entries.values() {
            counts[*p] += 1;
        }
        let mut order: Vec<usize> = (0..self.packs.len()).collect();
        order.sort_by_key(|&i| (self.packs[i].last_used_ms, self.packs[i].created_ms, i));
        let (mut packs, mut entries) = (self.packs.len(), self.entries.len());
        let mut dropped = HashSet::new();
        for i in order {
            if packs <= max_packs && entries <= max_entries {
                break;
            }
            dropped.insert(i);
            packs -= 1;
            entries -= counts[i];
        }
        self.retain_packs(|i, _| !dropped.contains(&i));
    }

    /// Keep the packs for which `keep(old index, meta)` holds and that still have entries;
    /// remove the entries of the others and renumber.
    fn retain_packs(&mut self, keep: impl Fn(usize, &PackMeta) -> bool) {
        let mut has_entries = vec![false; self.packs.len()];
        for (p, _) in self.entries.values() {
            has_entries[*p] = true;
        }
        let mut remap: Vec<Option<usize>> = Vec::with_capacity(self.packs.len());
        let mut next = 0;
        for (i, p) in self.packs.iter().enumerate() {
            if has_entries[i] && keep(i, p) {
                remap.push(Some(next));
                next += 1;
            } else {
                remap.push(None);
            }
        }
        let mut i = 0;
        self.packs.retain(|_| {
            i += 1;
            remap[i - 1].is_some()
        });
        self.entries.retain(|_, e| match remap[e.0] {
            Some(n) => {
                e.0 = n;
                true
            }
            None => false,
        });
    }

    /// Sort packs by `(created, key)`, merge duplicate keys, renumber entries.
    fn normalize(&mut self) {
        let old = std::mem::take(&mut self.packs);
        let packs = union_packs(old.iter());
        let pos = positions(&packs);
        for e in self.entries.values_mut() {
            e.0 = pos[&old[e.0].key];
        }
        self.packs = packs;
    }

    pub fn encode(&self) -> Vec<u8> {
        let out = WireOut {
            v: 1,
            packs: self
                .packs
                .iter()
                .map(|p| (&*p.key, p.created_ms, p.last_used_ms))
                .collect(),
            entries: self
                .entries
                .iter()
                .map(|(h, (p, l))| {
                    (
                        h.as_str(),
                        (*p, l.offset, l.len, l.size, l.duration_ms, l.tag.as_deref()),
                    )
                })
                .collect(),
        };
        let json = serde_json::to_vec(&out).expect("index serializes");
        zstd::encode_all(json.as_slice(), ZSTD_LEVEL).expect("zstd to memory")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, IndexError> {
        let dec = zstd::stream::Decoder::new(bytes).map_err(|e| IndexError::Zstd(e.to_string()))?;
        let mut rd = Limited {
            inner: dec,
            left: MAX_DECODED,
            exceeded: Cell::new(false),
        };
        let wire: Result<WireIn, _> = serde_json::from_reader(&mut rd);
        if rd.exceeded.get() {
            return Err(IndexError::TooLarge);
        }
        let wire = wire.map_err(|e| {
            if e.is_io() {
                IndexError::Zstd(e.to_string())
            } else {
                IndexError::Json(e.to_string())
            }
        })?;
        if wire.v != 1 {
            return Err(IndexError::Version(wire.v));
        }
        let packs: Vec<PackMeta> = wire
            .packs
            .into_iter()
            .map(|(k, c, u)| PackMeta {
                key: k.into(),
                created_ms: c,
                last_used_ms: u,
            })
            .collect();
        let mut entries = HashMap::with_capacity(wire.entries.len());
        for (hash, (pack, offset, len, size, duration_ms, tag)) in wire.entries {
            if pack >= packs.len() {
                return Err(IndexError::PackOutOfRange {
                    hash,
                    pack,
                    packs: packs.len(),
                });
            }
            let loc = Loc {
                offset,
                len,
                size,
                duration_ms,
                tag,
            };
            entries.insert(hash, (pack, loc));
        }
        let mut idx = PackIndex { packs, entries };
        idx.normalize();
        Ok(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(offset: u64) -> Loc {
        Loc {
            offset,
            len: 10,
            size: 5,
            duration_ms: None,
            tag: None,
        }
    }

    fn one(key: &str, created: u64, hashes: &[(&str, u64)]) -> PackIndex {
        let mut i = PackIndex::default();
        i.add_pack(
            key,
            created,
            hashes
                .iter()
                .map(|(h, o)| (h.to_string(), loc(*o)))
                .collect(),
        );
        i
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn z(s: &str) -> Vec<u8> {
        zstd::encode_all(s.as_bytes(), 3).unwrap()
    }

    #[test]
    fn round_trip() {
        let mut i = PackIndex::default();
        i.add_pack(
            "p/b-1",
            200,
            vec![
                (
                    "aa".into(),
                    Loc {
                        offset: 0,
                        len: 100,
                        size: 80,
                        duration_ms: Some(1234),
                        tag: Some("t".into()),
                    },
                ),
                ("bb".into(), loc(100)),
            ],
        );
        i.add_pack("p/a-1", 100, vec![("cc".into(), loc(0))]);
        i.touch("p/a-1", 900);
        let d = PackIndex::decode(&i.encode()).unwrap();
        assert_eq!(d.len(), 3);
        assert_eq!(d.pack_count(), 2);
        for h in ["aa", "bb", "cc"] {
            assert_eq!(d.get(h), i.get(h));
        }
        assert_eq!(d.packs, i.packs);
        assert_eq!(&*d.packs[0].key, "p/a-1");
        assert_eq!(d.packs[0].last_used_ms, 900);
    }

    #[test]
    fn exact_json_shape() {
        let mut i = PackIndex::default();
        i.add_pack(
            "pk-1",
            1000,
            vec![(
                "abc".into(),
                Loc {
                    offset: 7,
                    len: 20,
                    size: 9,
                    duration_ms: None,
                    tag: Some("hit".into()),
                },
            )],
        );
        let json = String::from_utf8(zstd::decode_all(i.encode().as_slice()).unwrap()).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"packs":[["pk-1",1000,1000]],"entries":{"abc":[0,7,20,9,null,"hit"]}}"#
        );
    }

    #[test]
    fn merge_newest_wins_in_any_order() {
        let a = one("k-1", 100, &[("h1", 1), ("only_a", 2)]);
        let mut b = one("k-2", 200, &[("h1", 50), ("only_b", 3)]);
        b.touch("k-2", 5000);
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        for m in [&ab, &ba] {
            assert_eq!(m.len(), 3);
            let e = m.get("h1").unwrap();
            assert_eq!(&*e.pack, "k-2");
            assert_eq!(e.loc.offset, 50);
            assert_eq!(&*m.get("only_a").unwrap().pack, "k-1");
            assert_eq!(&*m.get("only_b").unwrap().pack, "k-2");
        }
        assert_eq!(ab.packs, ba.packs);
        assert_eq!(ab.packs[1].last_used_ms, 5000);
    }

    #[test]
    fn merge_same_pack_takes_max_last_used_min_created() {
        let mut a = one("k-1", 100, &[("h", 1)]);
        a.touch("k-1", 300);
        let mut b = one("k-1", 90, &[("h", 1)]);
        b.touch("k-1", 200);
        a.merge(&b);
        assert_eq!(a.pack_count(), 1);
        assert_eq!(a.packs[0].last_used_ms, 300);
        assert_eq!(a.packs[0].created_ms, 90);
    }

    #[test]
    fn add_pack_overrides_older_entries() {
        let mut i = one("k-1", 100, &[("h", 1)]);
        i.add_pack("k-2", 200, vec![("h".into(), loc(77))]);
        let e = i.get("h").unwrap();
        assert_eq!((&*e.pack, e.loc.offset), ("k-2", 77));
    }

    #[test]
    fn drop_packs_and_remove_hashes_renumber() {
        let mut i = one("k-1", 100, &[("a", 1), ("b", 2)]);
        i.add_pack("k-2", 200, vec![("c".into(), loc(3))]);
        i.add_pack("k-3", 300, vec![("d".into(), loc(4)), ("e".into(), loc(5))]);
        i.drop_packs(&set(&["k-2"]));
        assert_eq!((i.pack_count(), i.len()), (2, 4));
        assert!(!i.contains("c"));
        assert_eq!(&*i.get("d").unwrap().pack, "k-3");
        i.remove_hashes(&set(&["a", "b"]));
        assert_eq!((i.pack_count(), i.len()), (1, 2));
        assert_eq!(&*i.get("e").unwrap().pack, "k-3");
        let d = PackIndex::decode(&i.encode()).unwrap();
        assert_eq!(d.get("e"), i.get("e"));
    }

    #[test]
    fn cap_drops_unused_then_lru_then_entries() {
        let day = 24 * 3600 * 1000u64;
        let now = 100 * day;
        let mut i = PackIndex::default();
        i.add_pack("old", 1, vec![("h0".into(), loc(0))]);
        i.add_pack("k1", now - 3 * day, vec![("h1".into(), loc(0))]);
        i.add_pack(
            "k2",
            now - 2 * day,
            vec![("h2".into(), loc(0)), ("h2b".into(), loc(1))],
        );
        i.add_pack("k3", now - day, vec![("h3".into(), loc(0))]);
        i.touch("k1", now - 2 * day + 1); // k1 now used more recently than k2
        i.touch("old", now - 31 * day);

        let mut c = i.clone();
        c.cap(now, MAX_UNUSED, 10, 100);
        assert!(!c.contains("h0"));
        assert_eq!(c.pack_count(), 3);

        // pack limit 2: k2 is the least recently used
        let mut c = i.clone();
        c.cap(now, MAX_UNUSED, 2, 100);
        assert!(!c.contains("h2") && c.contains("h1") && c.contains("h3"));
        assert_eq!(c.pack_count(), 2);

        // entry limit 2: k2 (2 entries) goes, leaving 2 entries
        let mut c = i.clone();
        c.cap(now, MAX_UNUSED, 10, 2);
        assert_eq!((c.pack_count(), c.len()), (2, 2));
        assert!(!c.contains("h2b"));

        // tie on last_used: the older created goes first
        let mut t = PackIndex::default();
        t.add_pack("x", 10, vec![("hx".into(), loc(0))]);
        t.add_pack("y", 20, vec![("hy".into(), loc(0))]);
        t.touch("x", 50);
        t.touch("y", 50);
        t.cap(60, MAX_UNUSED, 1, 100);
        assert!(!t.contains("hx") && t.contains("hy"));
    }

    #[test]
    fn decode_rejects_bad_input() {
        assert!(PackIndex::decode(b"garbage").is_err());
        assert!(matches!(
            PackIndex::decode(&z("not json")),
            Err(IndexError::Json(_))
        ));
        assert!(matches!(
            PackIndex::decode(&z(r#"{"v":2,"packs":[],"entries":{}}"#)),
            Err(IndexError::Version(2))
        ));
        let bad_pack = r#"{"v":1,"packs":[["k",1,1]],"entries":{"aa":[1,0,1,1,null,null]}}"#;
        assert!(matches!(
            PackIndex::decode(&z(bad_pack)),
            Err(IndexError::PackOutOfRange { .. })
        ));
        let short = r#"{"v":1,"packs":[["k",1,1]],"entries":{"aa":[0,0,1,1]}}"#;
        assert!(PackIndex::decode(&z(short)).is_err());
        let ok = r#"{"v":1,"packs":[["k",1,1]],"entries":{"aa":[0,0,1,1,null,null]}}"#;
        assert!(PackIndex::decode(&z(ok)).unwrap().contains("aa"));
    }

    #[test]
    fn decode_rejects_zstd_bomb() {
        let mut json = br#"{"v":1,"packs":[],"entries":{}}"#.to_vec();
        json.resize(json.len() + (70 << 20), b' ');
        let bomb = zstd::encode_all(json.as_slice(), 3).unwrap();
        assert!(bomb.len() < 1 << 20);
        assert!(matches!(
            PackIndex::decode(&bomb),
            Err(IndexError::TooLarge)
        ));
    }

    #[test]
    fn decode_sorts_unordered_packs() {
        let src = r#"{"v":1,"packs":[["b",200,200],["a",100,100]],"entries":{"h1":[0,0,1,1,null,null],"h2":[1,0,1,1,null,null]}}"#;
        let i = PackIndex::decode(&z(src)).unwrap();
        assert_eq!(&*i.packs[0].key, "a");
        assert_eq!(&*i.get("h1").unwrap().pack, "b");
        assert_eq!(&*i.get("h2").unwrap().pack, "a");
    }
}
