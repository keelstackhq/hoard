# hoard Milestone 4 — Packing Turborepo artifacts — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a cold build of 400+ packages on the GitHub cache makes a handful of uploads instead of one per task, so it never hits the 200 uploads/min limit (`throttled = 0`, `rate_limited = 0`) and loses no artifacts; warm builds still hit every task.

**Architecture:** with `pack` on (default for `storage=github`), PUT bodies are spooled to disk, sealed into packs of ~128 MiB (HRD1 envelopes back to back) and uploaded in the background; on shutdown the last pack is sealed and a cumulative index (hash → pack, offset, length, header fields) is written. Readers load the newest index once at startup, answer HEAD/batch query from it and serve GET with one HTTP Range read of the pack. A small `PackBackend` enum hides the storage: the Twirp client for `github`, the OpenDAL operator for `r2`/`fs` (tests, e2e).

**Tech Stack:** as milestones 1–3 (no new crates): reqwest (Range GET on signed blob URLs), zstd + serde_json (index), OpenDAL (`read_with().range()`, `list`) for r2/fs.

**Spec:** `docs/superpowers/specs/2026-10-14-hoard-design.md` §6.5 (new), §4.2 `pack`, §5, §6.2 `hash`, §9 `packs`.

## Facts checked (2026-10-22)

- Benchmark (`docs/benchmarks/2026-10-rate-limit.md`): 400 packages → 233 uploads/min, 1 × 429, 22 artifacts lost; 1000 packages → 146 uploads/min, no 429. The limit is per minute, so it is the upload *rate*, not the package count, that matters.
- GitHub cache service v2 (Twirp, `src/gha_twirp.rs`): `GetCacheEntryDownloadURL` with `restore_keys` returns the newest entry whose key starts with a restore key, first in the current branch, then in the base/default branch. Entries are immutable; `CreateCacheEntry` on an existing key answers `ok: false`. The signed download URL is an Azure blob SAS URL; Azure blob GET honours `Range: bytes=a-b` (206). Range GETs go to blob storage, not to the cache service, so only URL lookups count against 1,500 downloads/min.
- GitHub evicts entries not accessed for 7 days and the least recently used ones above 10 GB per repo; resolving a download URL counts as an access.
- Turborepo (2.x) remote cache: `HEAD /v8/artifacts/{hash}` before running a task, `GET` on a hit, `PUT` after a miss; `POST /v8/artifacts` (batch query) for dry runs. Artifacts are already zstd tarballs; we never recompress.

## Decisions (made without the owner; cost if wrong in brackets)

1. **Packing replaces write-behind for Turborepo artifacts on github** (`pack=auto` → on for github only). Write-behind stays for `pack=off` and r2. [If packs misbehave in the field: `HOARD_PACK=off` restores the old path; no data migration since no release exists yet.]
2. **No per-artifact fallback on reads when `pack` is on.** A hash not in the index is a miss without a storage request. Saves one cache-service call per miss (cold builds) and keeps reads simple. [A job with `pack=off` on the same `cache-version` writes entries that `pack=on` jobs do not see; documented.]
3. **One cumulative index per job, newest wins, merged again at shutdown.** No listing exists on the GitHub cache, so "newest by prefix" is the only discovery. [Two jobs finishing in the same second can drop one's entries; those tasks miss once and are re-packed.]
4. **Pack target 128 MiB, at most 2 pack uploads in flight; spool cap 2 GB.** A job uploads `ceil(bytes/128 MiB) + 1` entries; the last pack (≤128 MiB) uploads well within the 120 s drain on hosted runners. [Very slow uploads could time out the drain; then that job's last pack is lost (counted `put_errors`).]
5. **Index caps: 1000 packs, 200,000 entries, decoded ≤ 256 MiB.** Oldest packs are dropped first. Memory stays well under 100 MB. [A repo with >200k distinct live artifacts loses its oldest ones; they rebuild.]
6. **Envelope header gets an optional `hash`**, written only inside packs and checked on read, so a wrong offset can never serve another task's artifact. Single-object envelopes are unchanged (field omitted).
7. **Signed pack URLs are resolved lazily, once per pack per server**, and re-resolved once on 401/403 (an expired SAS).
8. **Index load runs in the background**, readers wait for it at most 30 s after startup; startup and `/_hoard/health` are never delayed.
9. **`pack=on` works on r2/fs via OpenDAL** (newest index = newest `last_modified`, then key). It is how the packing path is tested locally and in e2e; `auto` keeps r2 unpacked (no rate limit there, and per-object lifecycle is simpler).
10. **No new action input.** Like `write-behind`, `pack` is set with `HOARD_PACK` in the job env (the action passes the full environment). [If users need it often, add an input later; additive.]
11. **The bench reads final stats from `POST /_hoard/shutdown`** when packing is on (uploads happen at shutdown); the action's post step then only warns that the server already stopped.

## Global Constraints

- Keys: pack `{prefix}turbo-pack/{cache_version}/{run_key}-{n}`, index `{prefix}turbo-index/{cache_version}/{run_key}`; `run_key` from `vp::default_run_key`. GitHub entry version: SHA-256 hex of `hoard-pack-{cache_version}`.
- Index wire format exactly as spec §6.5 (zstd frame of JSON, `v: 1`, packs `[key, created_ms]` oldest first).
- Never fail the build: every pack/index error is a warning + miss / `put_errors`; only bad config fails startup.
- Stream, don't buffer: artifact bodies are streamed spool→pack file→upload and pack→response; only the index is held in memory (bounded).
- No URL with `sig=` and no token in any log or error (`without_url()`, `redacted`).
- Rate gate: pack and index uploads wait while paused; a 429 counts `throttled`, pauses, retries.

## Review Focus

1. **Wrong bytes served** — Range offsets off by one, or a stale index → envelope `hash`/`size` check makes it `corrupt` (miss). Test in Task 4.
2. **Lost artifacts at shutdown** — the last partial pack must be sealed and uploaded before the index is written, and the index must include it. Test in Task 5.
3. **Concurrent jobs** — index B written after A loads: A's shutdown merge keeps B's entries. Test in Task 5.
4. **Evicted pack** — URL lookup not found → miss, pack marked missing, left out of the next index. Test in Task 4/5.
5. **Rate limit on a pack upload** — 429 → `throttled`, pause, retry, eventually uploaded. Test in Task 5.
6. **Leaks** — no signed URL in logs on errors (mock `MOCKSIG`). Test in Task 1/4.

---

### Task 1: Shared Twirp client with Range reads

**Files:** Move `src/vp/gha_twirp.rs` → `src/gha_twirp.rs` (update `vp` imports, `lib.rs`); Modify `tests/common/gha_mock.rs` (honour `Range`, record it). Test: `tests/gha_twirp.rs` (new).

**Interfaces (produces):**
- `pub fn pack_version(cache_version: &str) -> String` (SHA-256 hex of `hoard-pack-{v}`)
- `pub async fn download_range(&self, url: &str, offset: u64, len: u64) -> Result<impl Stream<Item = io::Result<Bytes>> + Send + use<>, GhaError>` — `Range: bytes={offset}-{offset+len-1}`, requires `206` (a `200` means the server ignored Range → `GhaError::Protocol`), 401/403 → `GhaError::Status{status}`.

- [x] Step 1: failing tests — upload a blob through the mock (create/upload_blocks/finalize), `get_download_url` exact key, `download_range(url, 3, 5)` yields bytes 3..8; wrong sig → `Status{403}` and the error text has no `MOCKSIG`; `pack_version("v1") != vp_version("v1")`.
- [x] Step 2: run — FAIL. Step 3: implement (mock: parse `Range: bytes=a-b`, answer 206 with the slice). Step 4: run — PASS; fmt, clippy, `cargo test --test vp` still green.
- [x] Step 5: commit `refactor: share the GitHub cache Twirp client; add Range reads`

### Task 2: `pack` setting, envelope `hash`, `packs` stat

**Files:** `src/config.rs` (`PackMode { Auto, On, Off }`, flag `--pack`, env `HOARD_PACK`, resolved `Config.pack: bool` = auto→github), `src/envelope.rs` (`Header.hash: Option<String>`, `#[serde(default, skip_serializing_if = "Option::is_none")]`; update every `Header {..}` literal), `src/stats.rs` (`packs` counter + snapshot field), `src/serve.rs` (summary line `Packs: N uploaded` after `Throttled`).

- [ ] Step 1: failing tests — `--pack` auto on github only, `on`/`off` honoured, `HOARD_PACK=off`; envelope round-trips with `hash`, JSON without `hash` decodes to `None` and encodes without the key (existing exact-shape test unchanged); snapshot has `packs`; summary line order.
- [ ] Step 2–4: FAIL → implement → PASS (whole `cargo test`).
- [ ] Step 5: commit `feat: pack setting, envelope hash and packs counter`

### Task 3: Pack index model

**Files:** Create `src/pack/mod.rs`, `src/pack/index.rs`; `lib.rs`.

**Interfaces (produces):**
```rust
pub struct Loc { pub offset: u64, pub len: u64, pub size: u64, pub duration_ms: Option<u64>, pub tag: Option<String> }
pub struct Entry { pub pack: Arc<str>, pub loc: Loc }   // returned by lookups (clone)
pub struct PackIndex { /* packs: Vec<(Arc<str>, u64 created_ms)>, entries: HashMap<String,(usize, Loc)> */ }
impl PackIndex {
  pub fn get(&self, hash: &str) -> Option<Entry>;
  pub fn contains(&self, hash: &str) -> bool;
  pub fn len(&self) -> usize; pub fn pack_count(&self) -> usize;
  pub fn add_pack(&mut self, key: &str, created_ms: u64, entries: Vec<(String, Loc)>);
  pub fn merge(&mut self, other: &PackIndex);            // union; for a hash in several packs the newest pack (created, key) wins
  pub fn drop_packs(&mut self, missing: &HashSet<String>);
  pub fn cap(&mut self, max_packs: usize, max_entries: usize); // drop oldest packs until both hold; also drops packs with no entries
  pub fn encode(&self) -> Vec<u8>;                         // zstd(JSON), spec §6.5
  pub fn decode(bytes: &[u8]) -> Result<Self, IndexError>; // ≤ 256 MiB decoded, v == 1, pack # in range
}
pub const MAX_PACKS: usize = 1000; pub const MAX_ENTRIES: usize = 200_000;
```
- [ ] Step 1: failing unit tests — encode/decode round-trip; exact JSON shape of a one-entry index (decompress and compare); merge newest-wins regardless of argument order; drop_packs removes entries and renumbers; cap drops oldest first; decode rejects garbage, `v:2`, out-of-range pack #, oversize (use a zstd bomb of zeros > cap).
- [ ] Step 2–4. Step 5: commit `feat: pack index`

### Task 4: Pack backend and read path

**Files:** Create `src/pack/backend.rs`, `src/pack/reader.rs`. Test: `tests/pack.rs` (fs + gha mock; MinIO when `HOARD_TEST_S3_ENDPOINT` is set).

**Interfaces (produces):**
```rust
pub enum PackBackend { Store(opendal::Operator), Github { client: GhaTwirp, version: String, urls: Mutex<HashMap<String, String>> } }
pub enum PackError { RateLimited { retry_after: Option<Duration> }, Other(String /* redacted, no URL */) }
pub enum Uploaded { Created(u64), AlreadyExists }
impl PackBackend {
  pub fn from_config(cfg: &Config) -> anyhow::Result<Self>;            // github → GhaTwirp::from_env; r2/fs → store::build_operator
  pub async fn newest(&self, prefix: &str, max_bytes: u64) -> Result<Option<(String, Vec<u8>)>, PackError>;
  pub async fn upload(&self, key: &str, path: &Path) -> Result<Uploaded, PackError>;  // if-not-exists; github: create/upload_blocks/finalize
  pub async fn read_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<BodyStream>, PackError>; // None = pack not found
}
pub struct PackReader { /* backend, index: watch/OnceCell<PackIndex>, deadline, missing: Mutex<HashSet<String>> */ }
pub enum PackGet { Hit(Header, BodyStream), Miss, Corrupt }
impl PackReader {
  pub fn start(backend: Arc<PackBackend>, index_prefix: String, load: bool, wait: Duration) -> Arc<Self>; // spawns the load
  pub async fn lookup(&self, hash: &str) -> Option<Entry>;   // waits for the load (≤ wait); None if pack missing
  pub async fn get(&self, hash: &str) -> Result<PackGet, PackError>; // Range read; checks envelope hash+size; not found → mark missing, Miss
  pub async fn loaded(&self) -> PackIndex;  // snapshot for the writer's merge
  pub fn missing(&self) -> HashSet<String>;
}
```
Header parsing from the range stream: reuse the store's prefix-decoding helper if one exists (`store.rs` `open`), else factor one out (`envelope::read_header(stream) -> (Header, rest)`), bounded to 4104 bytes.

- [ ] Step 1: failing tests — fs: write a pack file + index by hand (via `PackIndex`), `PackReader` finds hash, `get` streams exact body; `newest` picks the newer of two indexes; an entry whose offset points at another envelope → `Corrupt`; deleted pack → `Miss` and `missing()` contains it, second lookup is `None` without a storage call; gha mock: same round trip, URL resolved once for two GETs from one pack, 403 once → re-resolved; load timeout → empty index; no `MOCKSIG` in any error string.
- [ ] Step 2–4. Step 5: commit `feat: pack backend and Range reads`

### Task 5: Pack writer, drain and index write

**Files:** Create `src/pack/writer.rs`. Test: `tests/pack.rs`.

**Interfaces (produces):**
```rust
pub struct PackWriter { /* backend, reader, stats, gate, spool_dir, keys (prefix, cache_version, run_key), state: Mutex<{accepted: HashSet, unsealed: Vec<Spooled>, unsealed_bytes, spooled_bytes, closed, seq}>, uploads: JoinSet, new_index: Mutex<PackIndex> */ }
pub enum Accepted<S> { Spooled, Duplicate, NotCached(&'static str) }
pub struct PackDrain { pub packs: u64, pub dropped: u64, pub index_written: bool }
impl PackWriter {
  pub fn new(backend, reader: Arc<PackReader>, stats, gate, spool_dir, prefix, cache_version, run_key) -> Self;
  pub fn with_target(self, bytes: u64) -> Self; pub fn with_spool_cap(self, bytes: u64) -> Self; // tests
  pub async fn put(&self, hash: &str, header: Header, body: S) -> io::Result<Accepted<S>>;
  pub async fn drain(&self, timeout: Duration) -> PackDrain;  // seal rest, wait uploads, merge + write index, remove spool dir
}
```
Behaviour exactly as spec §6.5: duplicate if in reader's index or accepted; spool with `ExactLength`; seal at target into `pack-<n>` (write `encode_header(header with hash)` + body per item, record offsets, delete spool files); upload task waits on the gate, 429 → `throttled` + pause + retry, other error → `put_errors += items`; success → `puts += items`, `bytes_up += Σsize`, `packs += 1`, entries added to `new_index`. Drain: close, seal, await uploads (abort + count dropped on timeout), if `new_index` non-empty or reader has missing packs: `newest` again → merge(loaded, newest, new) → drop missing → cap → upload `turbo-index/{v}/{run_key}` (gate-aware).

- [ ] Step 1: failing tests (fs backend, small target e.g. 1 KiB) — 3 PUTs below target → no upload before drain, one pack + one index after, a fresh reader sees all 3; PUTs over target → 2 packs; duplicate hash → `Duplicate`; drain with nothing new and nothing missing → no index written; concurrent index written by "another job" between load and drain is merged; mock 429 on first pack upload (gha mock `fail_with(429)` once) → `throttled == 1`, still uploaded; failing backend → `put_errors == n`, no index entry for them; spool cap → `NotCached`; drain timeout with a hung upload → dropped counted.
- [ ] Step 2–4. Step 5: commit `feat: pack writer with shutdown index merge`

### Task 6: Wire packing into the server

**Files:** `src/server/mod.rs` (`AppState.packs: Option<Arc<Packs>>` where `Packs { reader, writer: Option<PackWriter> }`), `src/server/turbo.rs` (HEAD/GET/query/PUT branch on `packs`), `src/serve.rs` (start: `pack` on and mode ≠ off → backend + reader (load unless `no_reads`) + writer when read-write; write-behind not started; shutdown: drain packs before prune; summary `Packs: N uploaded`; info line on start). Test: `tests/http.rs` (pack on fs), `tests/e2e/turbo.sh` (`PACK=on`), `.github/workflows/ci.yml` (one more e2e step with `PACK=on`).

- [ ] Step 1: failing tests — HTTP on fs with pack on: PUT 3 → GET misses before shutdown; shutdown; new server on same root → HEAD 200 with headers, GET body equal, batch query lists all; `mode=read` server reads packs, PUT 403; `no-reads` → misses; second job adds 1 artifact → index has 4; stats `puts`, `packs`; e2e `PACK=on tests/e2e/turbo.sh` second run FULL TURBO.
- [ ] Step 2–4 (run e2e fs, fs+PACK=on, r2). Step 5: commit `feat: serve Turborepo artifacts from packs`

### Task 7: Bench and docs

**Files:** `.github/workflows/bench.yml` (input `pack` auto/on/off → `HOARD_PACK`; when packing is on read final stats from `POST /_hoard/shutdown`; report column `pack`), `README.md` (`--pack`), `docs/benchmarks/README.md` if it lists columns, `CLAUDE.md` layout (`pack/`, `gha_twirp.rs`), `docs/PROGRESS.md`.

- [ ] Step 1: actionlint clean; local fs smoke: `hoard serve --pack on` + generator 50 packages twice → 50 hits.
- [ ] Step 2: commit `ci: bench packing; docs`

### Task 8: Bench on GitHub

- [ ] Run `bench.yml` on the PR branch with 400 and 1000 packages (`pack: auto`), one at a time; record rows in `docs/benchmarks/2026-10-rate-limit.md` (new table "with packing"); goal `throttled = 0`, `rate_limited = 0`, warm hits = packages. Commit `docs: rate-limit benchmark with packing`.
