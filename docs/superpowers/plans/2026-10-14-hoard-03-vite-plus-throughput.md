# hoard Milestone 3 — Vite+ support and throughput — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Vite+ (`vp run`) workspaces get their task cache restored and saved by the same action, Turborepo PUTs no longer slow tasks down, popular R2 artifacts stop expiring, and we know whether large repos hit GitHub's upload rate limit.

**Architecture:** Vite+ has no released remote-cache protocol, so its whole task-cache directory (`node_modules/.vite/task-cache`, or `$VITE_CACHE_PATH`) is archived as one tar.zst and stored per OS/arch with "latest" lookup. Turborepo PUTs go through a write-behind queue drained on shutdown. R2 refresh-on-hit rewrites old popular objects. A benchmark decides whether packing (spec §12) is needed.

**Tech Stack:** as milestones 1–2; `tar` + `zstd` crates; reqwest for a minimal GitHub cache Twirp client (restore-key lookup, which OpenDAL's `ghac` does not expose).

**Spec:** `docs/superpowers/specs/2026-10-14-hoard-design.md` (§4.1 `vp`, §7.3, §12)

## Global Constraints

- Vite+ archive key: `{prefix}vp/{cache_version}/{os}-{arch}/{run_id}-{run_attempt}`; restore picks the newest key with prefix `{prefix}vp/{cache_version}/{os}-{arch}/`.
- On R2 the newest is found via the pointer object `{prefix}vp/{cache_version}/{os}-{arch}/latest` (plain overwrite, contains the full key); on GitHub storage via `GetCacheEntryDownloadURL` with `restore_keys = [that prefix]`.
- `vp-max-size` default `2GB`: when the directory is larger, skip the save and warn "run `vp cache clean`"; never edit the SQLite cache.
- Write-behind is on for `storage=github`, off for `r2`/`fs` unless `--write-behind`; queue concurrency 8; drain timeout on shutdown 120 s, then remaining items are dropped and counted as `put_errors`.
- `refresh-on-hit` only for `storage=r2`, at most once per object per run, only when the object is older than `max-age-days / 2`.

## Review Focus

1. **First run of a new branch** (no archive exists) → `vp restore` exits 0 with "no cache found", the job continues. Test in Task 1.
2. **Archive from another OS** must never be restored (task cache paths are absolute/OS-specific) → keys include `{os}-{arch}`. Test in Task 1.
3. **Job cancelled during write-behind** → no partial objects: uploads use if-not-exists writes of complete envelopes only. Test in Task 2.
4. **Vite+ upgrade changes the cache schema** (`v18` → `v19`) → old archive restores harmlessly (Vite Task ignores other schema dirs). Documented; no code.
5. **Concurrent jobs on the same OS save at the same time** → both saves succeed (distinct `run_id-attempt` keys); the R2 `latest` pointer ends on one of them. Test in Task 1.

---

### Task 1: `vp restore` / `vp save`

**Files:**
- Create: `src/vp/mod.rs`, `src/vp/archive.rs`, `src/vp/gha_twirp.rs`; Modify: `src/main.rs`; Action: `action/src/main.ts` (restore when Vite+ is detected), `action/src/post.ts` (save)
- Test: `src/vp/archive.rs`, `tests/vp.rs`, `action/test/settings.test.ts`

**Interfaces:**
- Produces:
  - `fn vp_cache_dir(workspace_root: &Path, env: &HashMap<String,String>) -> PathBuf` (`VITE_CACHE_PATH` if absolute, else `node_modules/.vite/task-cache`)
  - `async fn archive_dir(dir: &Path, out: impl AsyncWrite) -> Result<u64>` / `async fn extract_archive(input: impl AsyncRead, dir: &Path) -> Result<()>` (tar + zstd level 3, rejects absolute paths and `..` entries)
  - `enum VpBackend { Store { store: Arc<Store>, prefix: String }, Github(GhaTwirp) }` — `Store` for `r2`/`fs` (pointer object), `Github` for `github` (restore keys)
  - `async fn vp_restore(backend: &VpBackend, cache_version: &str, os_arch: &str, dir: &Path) -> Result<RestoreOutcome>`; `enum RestoreOutcome { Restored { key: String, bytes: u64 }, NotFound }`
  - `async fn vp_save(backend: &VpBackend, cache_version: &str, os_arch: &str, run_key: &str, dir: &Path, max_size: u64) -> Result<SaveOutcome>`; `enum SaveOutcome { Saved { key: String, bytes: u64 }, SkippedTooLarge { bytes: u64 }, SkippedEmpty }`
  - `struct GhaTwirp` with `get_download_url(key: &str, restore_keys: &[String], version: &str) -> Result<Option<(String /*matched*/, String /*url*/)>>`, `create_entry`, `upload_blocks`, `finalize` (never logs URLs)
  - Action: `detectTools(root): { turbo: boolean; vp: boolean }` (`turbo.json` / `vite-plus` in root `package.json` dependencies)

- [x] **Step 1: Write failing tests** — archive round-trip keeps file bytes and mtimes; archive with `../evil` entry is rejected; `vp_restore` on an empty fs store → `NotFound`; save from `linux-x64` then restore for `darwin-arm64` → `NotFound`; two saves → restore returns the newer; dir larger than `max_size` → `SkippedTooLarge`; `detectTools` on fixtures.
- [x] **Step 2: Run — FAIL.**  **Step 3: Implement** (R2/fs: pointer object; GitHub: `GhaTwirp` with `restore_keys`).  **Step 4: Run — PASS** (GitHub path covered by the self-test workflow with a Vite+ fixture added to `tests/fixtures/vp-repo/`).
- [x] **Step 5: Commit** — `feat: Vite+ task-cache restore and save`

---

### Task 2: Write-behind uploads

**Files:**
- Create: `src/writebehind.rs`; Modify: `src/server/turbo.rs`, `src/serve.rs`

**Interfaces:**
- Produces: `struct WriteBehind` with `fn new(store: Arc<Store>, spool_dir: PathBuf, concurrency: usize) -> Self`, `async fn enqueue(&self, key: String, header: Header, body: impl Stream<…>) -> Result<()>` (spools to a temp file, returns when spooled), `async fn drain(&self, timeout: Duration) -> DrainReport { uploaded, dropped }`

- [x] **Step 1: Write failing tests** — with a store that sleeps 500 ms per put, PUT responds in < 100 ms; `drain` waits until uploaded; with a 50 ms drain timeout and a slow store, `dropped > 0` and counted as `put_errors`; a spool file is deleted after upload; killing the worker mid-upload leaves no object at the key (fs store, Review Focus 3).
- [x] **Step 2: Run — FAIL.**  **Step 3: Implement.**  **Step 4: Run — PASS.**
- [x] **Step 5: Commit** — `feat: write-behind uploads drained on shutdown`

---

### Task 3: R2 refresh-on-hit

**Files:**
- Modify: `src/server/turbo.rs`, `src/config.rs` (`--refresh-on-hit`), `src/r2/mod.rs`
- Test: `tests/r2.rs`

**Interfaces:**
- Produces: `async fn refresh_object(client: &aws_sdk_s3::Client, bucket: &str, key: &str) -> Result<()>` (CopyObject onto itself with `MetadataDirective::Replace`); a per-run `DashSet<String>` of refreshed keys

- [ ] **Step 1: Write failing test (MinIO first, then once by hand against a real R2 bucket)** — put object, read `last_modified`, wait 2 s, `refresh_object`, `last_modified` is newer and body unchanged. If R2 does not reset the age with a self-copy, switch to re-PUT of the same envelope and record the finding in the spec §7.3.
- [x] **Step 2: Run — FAIL.**  **Step 3: Implement** (spawned in the background after the GET response starts; never delays the response).  **Step 4: Run — PASS.**
- [x] **Step 5: Commit** — `feat: refresh popular R2 artifacts on hit`

---

### Task 4: Rate-limit benchmark (decides on packing)

**Files:**
- Create: `scripts/bench/generate-monorepo.mjs` (N packages, each a trivial `build`), `.github/workflows/bench.yml` (`workflow_dispatch`, inputs `packages` default 400)

- [ ] **Step 1: Write the generator and workflow** — cold run with `storage: github`, read `rate_limited` and wall time from the job outputs; warm run in a second job.
- [ ] **Step 2: Run with 100, 400 and 1000 packages; record the numbers in `docs/benchmarks/2026-xx-rate-limit.md`**
- [ ] **Step 3: Decide** — if `rate_limited > 0` at ≤ 400 packages, write a spec section and a new plan for packing (one pack + index per job, HTTP Range reads); otherwise record "packing not needed" in the spec §12.
- [ ] **Step 4: Commit** — `bench: GitHub cache upload rate limits`
