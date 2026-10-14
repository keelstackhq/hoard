# hoard Milestone 2 — GitHub cache backend, release pipeline and the Action — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `uses: keelstack/hoard@v1` gives any Turborepo job a working cache with zero config (GitHub Actions cache) or with R2 secrets, on Linux, macOS and Windows runners.

**Architecture:** The binary from milestone 1 gains a `github` storage (OpenDAL `services-ghac`) with rate-limit handling. A tag-triggered workflow cross-compiles five targets and publishes them with SHA-256 checksums that are baked into the action bundle. A node24 TypeScript action downloads and verifies the binary, runs it detached, exports the `TURBO_*` variables, and in its post step shuts it down and writes the job summary.

**Tech Stack:** Rust (as milestone 1) + `opendal/services-ghac`; cargo-zigbuild (Linux musl), native macOS/Windows runners; TypeScript, Node 24, pnpm, esbuild, vitest, Biome, `@actions/core`, `@actions/tool-cache`.

**Spec:** `docs/superpowers/specs/2026-10-14-hoard-design.md` (§6.4, §8, §10, §11)

## Global Constraints

- `action.yml` lives at the repo root (`runs.using: node24`, `main: action/dist/main.js`, `post: action/dist/post.js`, `post-if: always()`).
- `action/dist/` is committed and must stay under 200 KB total (CI fails above).
- Targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`. Asset name `hoard-<target>.tar.gz` containing `hoard` (`hoard.exe` on Windows).
- The action never downloads a binary whose SHA-256 is not in the baked `action/src/checksums.generated.ts`.
- Cache errors never fail the job; setup errors fail it loudly with a fix.
- `TURBO_TOKEN` is passed to `core.setSecret` before it is exported.
- `storage: r2` without all four R2 secrets → fall back to `github` with a warning (fork PRs); never fail.
- Rate limit: on HTTP 429 from the GitHub cache, pause uploads for `Retry-After` seconds (default 60); reads continue.

## Review Focus

1. **Fork PR with `storage: r2` and no secrets** → job succeeds on `github` storage; summary shows the warning. Test in Task 3.
2. **`ACTIONS_RESULTS_URL` missing** (old GHES, odd runner) → `serve --storage github` exits non-zero with a message naming the variable; no silent disk fallback. Test in Task 1.
3. **Health check never answers** (binary crashes at start) → main step fails after 10 s and prints the server log. Test in Task 4.
4. **Unsupported runner (e.g. Linux s390x)** → main step fails with "unsupported platform linux/s390x"; no download attempt. Test in Task 3.
5. **The post step runs after the server already died** → post step warns, still writes whatever summary exists, does not fail the job. Test in Task 4.

---

## File Structure

```
src/store.rs                 # + Storage::Github operator
src/ratelimit.rs             # RateGate
src/config.rs                # + no_reads
action.yml
action/package.json  action/tsconfig.json  action/biome.json  action/vitest.config.ts
action/src/settings.ts       # resolveSettings()
action/src/platform.ts       # targetFor()
action/src/binary.ts         # ensureBinary()
action/src/server.ts         # startServer(), waitForHealth(), shutdownServer()
action/src/main.ts  action/src/post.ts
action/src/checksums.generated.ts   # written by the release workflow
action/test/*.test.ts
.github/workflows/release.yml
.github/workflows/self-test.yml
scripts/write-checksums.mjs
```

---

### Task 1: GitHub cache storage, `--no-reads` and the rate gate

**Files:**
- Create: `src/ratelimit.rs`; Modify: `src/store.rs`, `src/config.rs`, `src/server/turbo.rs`
- Test: `src/ratelimit.rs`, `src/config.rs`, `tests/http.rs`

**Interfaces:**
- Consumes: `Store`, `Config`, `AppState` (milestone 1)
- Produces:
  - `Config.no_reads: bool` (`--no-reads`, `HOARD_NO_READS`): GET/HEAD always miss
  - `fn ghac_operator(cache_version: &str) -> anyhow::Result<Operator>` — error text `"ACTIONS_RESULTS_URL is not set: run hoard through the GitHub Action, or use --storage r2|fs"` when the env var is missing (same for `ACTIONS_RUNTIME_TOKEN`)
  - `struct RateGate` with `fn allow_upload(&self, now: Instant) -> bool`, `fn on_rate_limited(&self, now: Instant, retry_after: Option<Duration>)`; default pause 60 s
  - Store PUT maps OpenDAL `ErrorKind::RateLimited` to `StoreError::RateLimited { retry_after }`; the PUT handler then calls `on_rate_limited`, increments `rate_limited`, returns `200`; while paused, PUTs are skipped (`200`, counted as `rate_limited`)

- [x] **Step 1: Write failing tests**

```rust
#[test] fn gate_pauses_and_resumes() {
    let g = RateGate::default(); let t0 = Instant::now();
    assert!(g.allow_upload(t0));
    g.on_rate_limited(t0, Some(Duration::from_secs(5)));
    assert!(!g.allow_upload(t0 + Duration::from_secs(4)));
    assert!(g.allow_upload(t0 + Duration::from_secs(6)));
    g.on_rate_limited(t0, None);
    assert!(!g.allow_upload(t0 + Duration::from_secs(59)));
}
#[test] fn github_storage_requires_results_url() {
    std::env::remove_var("ACTIONS_RESULTS_URL");
    assert!(ghac_operator("v1").unwrap_err().to_string().contains("ACTIONS_RESULTS_URL is not set"));
}
#[tokio::test] async fn no_reads_always_misses() { /* fs server with no_reads: PUT 200, GET 404 */ }
#[tokio::test] async fn rate_limited_put_is_200_and_counted() { /* store wrapper that returns RateLimited → PUT 200, rate_limited == 1, next PUT skipped without calling the store */ }
```

- [x] **Step 2: Run — FAIL.**  **Step 3: Implement** (`services-ghac` with `version` = `hoard-{cache_version}` so cache versions never collide).  **Step 4: Run — PASS.**
- [x] **Step 5: Commit** — `feat: GitHub Actions cache storage with rate-limit handling`

---

### Task 2: Release pipeline with checksums

**Files:**
- Create: `.github/workflows/release.yml`, `scripts/write-checksums.mjs`

**Interfaces:**
- Produces: GitHub release `vX.Y.Z` with five `hoard-<target>.tar.gz` assets; `action/src/checksums.generated.ts` exporting `export const VERSION = "X.Y.Z"; export const CHECKSUMS: Record<Target, string>`; commit `chore(release): vX.Y.Z` with rebuilt `action/dist/`; tags `vX.Y.Z` and moved major tag `vX`.

- [x] **Step 1: Write `scripts/write-checksums.mjs`** — input: directory of assets; output: the TS file above; exits non-zero unless exactly the five targets are present. Unit test it with vitest (`action/test/write-checksums.test.ts`): five fake files → file contains five 64-hex entries; four files → throws `"missing target aarch64-apple-darwin"`.
- [x] **Step 2: Write the workflow** — trigger `workflow_dispatch` with input `version`; matrix build (zigbuild for musl, native for darwin/windows), `strip`, tar.gz, upload artifacts; job `publish`: download all, run the script, `pnpm --dir action build`, check dist < 200 KB, commit, tag, create release with assets.
- [ ] **Step 3: Dry run** — run the workflow on a fork or with `version: 0.0.0-test` and `draft: true`; verify five assets and a matching `checksums.generated.ts`.
- [x] **Step 4: Commit** — `ci: cross-platform release pipeline with baked checksums`

---

### Task 3: Action settings, platform and binary download

**Files:**
- Create: `action.yml`, `action/package.json`, `action/tsconfig.json`, `action/biome.json`, `action/vitest.config.ts`, `action/src/settings.ts`, `action/src/platform.ts`, `action/src/binary.ts`
- Test: `action/test/settings.test.ts`, `action/test/platform.test.ts`, `action/test/binary.test.ts`

**Interfaces:**
- Produces:
  - `type Target` (the five triples); `function targetFor(platform: NodeJS.Platform, arch: string): Target` — throws `Error("unsupported platform <platform>/<arch>")`
  - `interface Settings { storage: "github" | "r2" | "fs"; mode: "read-write" | "read" | "off"; noReads: boolean; cacheVersion: string; r2?: { accountId: string; accessKeyId: string; secretAccessKey: string; bucket: string; prefix: string }; r2Lifecycle: "auto" | "verify" | "manual"; maxAgeDays: number; maxSize: string; warnings: string[] }`
  - `function resolveSettings(inputs: Record<string, string>, env: NodeJS.ProcessEnv): Settings` — pure
  - `async function ensureBinary(target: Target, version: string, checksums: Record<Target, string>, download: (url: string) => Promise<string>): Promise<string>` — returns the executable path; throws `Error("checksum mismatch for <target>")`

- [x] **Step 1: Write failing tests**

```ts
test("r2 without secrets falls back to github", () => {
  const s = resolveSettings({ storage: "r2", "r2-bucket": "b" }, {});
  expect(s.storage).toBe("github");
  expect(s.warnings[0]).toMatch(/R2 secrets are not available/);
});
test("ACTIONS_CACHE_MODE maps to mode", () => {
  expect(resolveSettings({}, { ACTIONS_CACHE_MODE: "read" }).mode).toBe("read");
  expect(resolveSettings({}, { ACTIONS_CACHE_MODE: "none" }).mode).toBe("off");
  expect(resolveSettings({}, { ACTIONS_CACHE_MODE: "write-only" })).toMatchObject({ mode: "read-write", noReads: true });
});
test("explicit mode input wins", () => {
  expect(resolveSettings({ mode: "read" }, { ACTIONS_CACHE_MODE: "write" }).mode).toBe("read");
});
test("targets", () => {
  expect(targetFor("linux", "x64")).toBe("x86_64-unknown-linux-musl");
  expect(targetFor("darwin", "arm64")).toBe("aarch64-apple-darwin");
  expect(targetFor("win32", "x64")).toBe("x86_64-pc-windows-msvc");
  expect(() => targetFor("linux", "s390x")).toThrow("unsupported platform linux/s390x");
});
test("checksum mismatch throws", async () => {
  const fakeDownload = async () => writeTempTarGz("not the real binary");
  await expect(ensureBinary("x86_64-unknown-linux-musl", "1.0.0", { ...CHECKSUMS_ZERO }, fakeDownload))
    .rejects.toThrow("checksum mismatch for x86_64-unknown-linux-musl");
});
```

- [x] **Step 2: Run `pnpm --dir action test` — FAIL.**
- [x] **Step 3: Implement** — `action.yml` inputs exactly as spec §4.2 plus `version`; download URL `https://github.com/keelstack/hoard/releases/download/v<version>/hoard-<target>.tar.gz`; extract and cache with `@actions/tool-cache`.
- [x] **Step 4: Run — PASS; `pnpm --dir action lint` clean.**
- [x] **Step 5: Commit** — `feat(action): settings resolution and verified binary download`

---

### Task 4: Action main and post

**Files:**
- Create: `action/src/server.ts`, `action/src/main.ts`, `action/src/post.ts`
- Test: `action/test/server.test.ts`

**Interfaces:**
- Consumes: `resolveSettings`, `targetFor`, `ensureBinary` (Task 3); binary flags and `/_hoard/*` routes (milestone 1)
- Produces:
  - `function serveArgs(s: Settings, runnerTemp: string): string[]` (pure; includes `--port 0 --port-file … --token-file … --summary-file …`)
  - `async function startServer(bin: string, args: string[], logPath: string): Promise<void>` (detached spawn, `unref`, full `process.env`)
  - `async function waitForHealth(portFile: string, timeoutMs = 10_000): Promise<number>` — polls every 100 ms, returns port; throws `Error("hoard did not start within 10s")`
  - `async function shutdownServer(port: number, token: string): Promise<boolean>` — false if unreachable
  - main: settings → binary → start → health → `core.setSecret(token)` → export `TURBO_API`, `TURBO_TOKEN`, `TURBO_TEAM=hoard` → `core.saveState("port"|"token"|"summary")`
  - post: shutdown → append summary + warnings to `$GITHUB_STEP_SUMMARY` → outputs `hits`, `misses`, `time-saved-ms` (from `GET /_hoard/stats` before shutdown) → print log on failure or `RUNNER_DEBUG=1`

- [x] **Step 1: Write failing tests**

```ts
test("serveArgs for r2", () => {
  const a = serveArgs({ ...baseSettings, storage: "r2", r2: r2Fixture }, "/tmp/rt");
  expect(a).toEqual(expect.arrayContaining(["serve", "--storage", "r2", "--r2-bucket", "b", "--port", "0", "--port-file", "/tmp/rt/hoard.port"]));
  expect(a.join(" ")).not.toContain(r2Fixture.secretAccessKey); // secrets go via env, not argv
});
test("waitForHealth times out with a clear error", async () => {
  await expect(waitForHealth("/nonexistent/port", 300)).rejects.toThrow("hoard did not start");
});
test("shutdownServer returns false when nothing listens", async () => {
  expect(await shutdownServer(1, "t")).toBe(false);
});
```

- [x] **Step 2: Run — FAIL.**  **Step 3: Implement** — R2 secrets are passed as `HOARD_R2_SECRET_ACCESS_KEY` etc. in the child env, never argv. Post never throws (wrap and `core.warning`).  **Step 4: Run — PASS.**
- [x] **Step 5: Commit** — `feat(action): start, export and shut down hoard`

---

### Task 5: Self-test workflow on real runners

**Files:**
- Create: `.github/workflows/self-test.yml`

- [ ] **Step 1: Write the workflow** — matrix `os: [ubuntu-latest, ubuntu-24.04-arm, macos-latest, windows-latest]` × `storage: [github]`, plus `ubuntu-latest` × `storage: r2` (only when `secrets.R2_ACCESS_KEY_ID != ''`). Job `warm` runs the action (`uses: ./`) and builds `tests/fixtures/turbo-repo`; job `hit` (needs `warm`, same matrix) runs it again in a fresh checkout and asserts `steps.cache.outputs.hits` > 0 and the turbo output contains `FULL TURBO`. Uses a per-run `cache-version: run-${{ github.run_id }}` so runs never see each other.
- [ ] **Step 2: Run on a branch — expect all green; the job summary shows the hoard table**
- [ ] **Step 3: Commit** — `ci: self-test the action on Linux, macOS, Windows and R2`
