# hoard — Design Spec

Date: 2026-10-14 · Status: draft for implementation · Idea: `monorepo-ci-cache` (life-os)

Names: GitHub org `keelstack` (tools for bootstrapping projects), repo `keelstack/hoard`, binary
`hoard`, Rust package `keel-hoard` (the crate name `hoard` is taken), npm scope `@keelstack`, env
prefix `HOARD_`. Usage: `uses: keelstack/hoard@v1`.

## 1. Goal

Give a TypeScript monorepo (Turborepo or Vite+) a shared task cache in CI with **one line of
workflow YAML**, no account and no server to host. Storage is the GitHub Actions cache (default,
zero config) or a Cloudflare R2 bucket (no size or rate limits, shared across branches, repos and
laptops). An R2 bucket must never grow without limit.

### Non-goals (v1)

- Nx, Bazel, Gradle or any non-JS task runner.
- A hosted service, accounts or a dashboard.
- Vite+'s own remote-cache protocol (`/fetch`, `/blob`, `/store`): unreleased upstream; revisit
  when VoidZero ships it (see §12).
- GitHub Enterprise Server (it still runs the v1 cache service).

## 2. User experience

GitHub cache (default):

```yaml
- uses: keelstack/hoard@v1
- run: pnpm turbo run build test lint
```

Cloudflare R2:

```yaml
- uses: keelstack/hoard@v1
  with:
    storage: r2
    r2-account-id: ${{ secrets.R2_ACCOUNT_ID }}
    r2-access-key-id: ${{ secrets.R2_ACCESS_KEY_ID }}
    r2-secret-access-key: ${{ secrets.R2_SECRET_ACCESS_KEY }}
    r2-bucket: my-team-cache
```

Laptop (same binary, same bucket; a read-only R2 token is recommended, with `--mode read` so
hoard never writes artifacts or the lifecycle rule):

```bash
# A read-only R2 token is recommended. Credentials come from the environment:
# HOARD_R2_ACCOUNT_ID, HOARD_R2_ACCESS_KEY_ID, HOARD_R2_SECRET_ACCESS_KEY.
hoard serve --storage r2 --r2-bucket my-team-cache --mode read &   # prints how to load the env
eval "$(hoard env)"                                                # sets TURBO_API/TOKEN/TEAM
pnpm turbo run build
```

The job summary shows hits, misses, bytes, time saved and any cleanup that ran.

## 3. Architecture

```
GitHub job ─┬─ action main (node24, ~100 KB)  ── downloads + verifies binary, spawns it detached,
            │                                    waits for /_hoard/health, exports TURBO_* env
            ├─ turbo / vp steps  ──HTTP──▶  hoard serve (Rust, 127.0.0.1:<port>)
            │                                   ├─ Turborepo API (/v8/artifacts…)
            │                                   ├─ Store (OpenDAL operator: ghac | s3→R2 | fs)
            │                                   └─ stats
            └─ action post  ── POST /_hoard/shutdown → flush, prune (R2), write summary, exit
```

One Rust crate (`hoard`, lib + bin) and one small TypeScript action in `action/`.

## 4. The binary

### 4.1 CLI

| Command | Purpose |
|---|---|
| `hoard serve` | Run the cache server until shutdown. |
| `hoard env` | Print `export` lines for a running server (reads the port file and token file). |
| `hoard prune` | Enforce `max-size` on R2 (or fs) now (ignores the 24 h throttle with `--force`); prints `pruned N of M objects, freed X, Y remain (max Z)` or `skipped: last prune < 24 h ago (use --force)`. With `storage=github` it fails: `prune is only for r2/fs storage; GitHub evicts old cache entries itself`. |
| `hoard doctor` | Check storage credentials, bucket access and the R2 lifecycle rule; print fixes. The access check writes, reads and deletes `{prefix}_hoard/doctor`, also with `mode=read`. With `storage=github` it only checks that the GitHub Actions cache environment is complete and prints `✓ GitHub cache environment`; it never writes (entries are immutable and count against the quota). |
| `hoard vp restore` / `vp save` | Restore or save the Vite+ task-cache directory (milestone 3). |

### 4.2 Configuration

Precedence: CLI flag > environment variable > default. Every flag `--foo-bar` has an env var
`HOARD_FOO_BAR`. An empty `HOARD_*` variable counts as unset (GitHub Actions exports empty
secrets as `""`).

| Setting | Default | Notes |
|---|---|---|
| `storage` | `github` | `github` \| `r2` \| `fs` |
| `mode` | `read-write` | `read-write` \| `read` \| `off`; see §8 |
| `no-reads` | off | Boolean flag (`HOARD_NO_READS=true` or `1`): GET/HEAD miss and the batch query finds nothing, without reading the store; PUTs are unaffected (§6.4, `write-only`) |
| `cache-version` | `v1` | Part of every key; bump to purge. One segment of `[A-Za-z0-9._-]` (not `.` or `..`, no `/`) |
| `port` | `0` | `0` = pick a free port; actual port written to `--port-file` |
| `port-file` | `$RUNNER_TEMP/hoard.port` or `./.hoard/port` | |
| `token` | random 32 bytes, hex | Written to `--token-file` (mode 0600); never logged |
| `token-file` | `$RUNNER_TEMP/hoard.token` or `./.hoard/token` | Where the token is written; read by `hoard env` |
| `summary-file` | none | Markdown summary written on shutdown |
| `fs-root` | `./.hoard/store` | `fs` storage only (tests, local use) |
| `r2-account-id`, `r2-access-key-id`, `r2-secret-access-key`, `r2-bucket` | — | required for `r2` |
| `r2-endpoint` | — | Optional S3 endpoint URL override (e.g. MinIO for tests and local use), must start with `http://` or `https://`; when set, `r2-account-id` is not required |
| `r2-prefix` | `cache/` | One or more segments of `[A-Za-z0-9._-]` (not `.` or `..`), each followed by `/`, e.g. `cache/` or `team/turbo/`. No leading `/`, `//`, whitespace or control characters: the S3 layer would normalise them and reach keys outside the prefix (`/` = the whole bucket). |
| `r2-lifecycle` | `auto` | `auto` \| `verify` \| `manual`; see §7.1 |
| `max-age-days` | `7` | R2 lifecycle expiry; 1–36500 |
| `max-size` | `5GB` | R2 size cap; accepts `KB`, `MB`, `GB`, `TB` (powers of 1000) |

R2 endpoint: `https://<account_id>.r2.cloudflarestorage.com` unless `r2-endpoint` overrides it,
region `auto`, path-style addressing. Only the configured keys are used: credentials and config
from `AWS_*` env vars, `~/.aws` and instance metadata are ignored.

## 5. HTTP API (Turborepo)

Bound to `127.0.0.1` only. Every route except `/_hoard/health` requires
`Authorization: Bearer <token>` (constant-time compare) → otherwise `401`.

| Route | Behaviour |
|---|---|
| `GET /v8/artifacts/status` | `200 {"status":"enabled"}` (`"disabled"` when `mode=off`) |
| `HEAD /v8/artifacts/{hash}` | `200` + `Content-Length` and `x-artifact-duration`/`x-artifact-tag` headers if present, else `404`. Must not download the body. Counts misses only (the following GET counts the hit). With `no-reads`: always `404`, counted as a miss, store not read. |
| `GET /v8/artifacts/{hash}` | `200` streaming body (`application/octet-stream`, `Content-Length`, artifact headers) or `404`. With `no-reads`: always `404`, counted as a miss, store not read. |
| `PUT /v8/artifacts/{hash}` | Stream body to the store; `200 {"urls":[]}`. Existing key → also `200`. `mode=read`/`off` → `403` with `{"error":{"code":"read_only","message":"hoard is in read-only mode"}}` (code `off`, message `hoard is off (mode=off)` for `off`), the error shape Turborepo prints. The code is never `forbidden` or `remote_caching_*`: Turborepo answers those by disabling the remote cache for the whole run, reads included. Needs `Content-Length` (becomes the envelope `size`); missing, a body of a different length, or a header over 4096 bytes → not stored, `200`, counted as `put_errors`. A backend rate limit → `200`, counted as `rate_limited`, and uploads pause (§6.4): while paused, a PUT (after the auth, hash and mode checks) is answered `200` and counted as `rate_limited` without touching the store or reading the body. |
| `POST /v8/artifacts/events` | Accept (body ignored); `200` |
| `POST /v8/artifacts` | Body `{"hashes":[…]}` → `200` JSON object mapping each found hash to `{"size":<u64>,"taskDurationMs":<u64\|null>}`; missing hashes are left out (`{}` with `mode=off` or `no-reads`) |
| `GET /_hoard/health` | `200 ok` (no auth) |
| `GET /_hoard/stats` | `200` JSON `Stats` (§9) |
| `POST /_hoard/shutdown` | Flush pending work, run prune (R2), write summary, respond `200`, exit 0 |

Validation: `{hash}` must match `^[a-fA-F0-9]{1,128}$` → otherwise `400`. Query params `teamId`
and `slug` are accepted and ignored.

**Never fail the build.** Any storage error on GET/HEAD returns `404` (a miss); on PUT returns `200`
and is counted as `put_errors`. Errors are logged without secrets or signed URLs: every logged or
printed storage error is redacted first (the values of the query parameters `sig`, `se`, `sp`,
`sv`, `X-Amz-Signature`, `X-Amz-Credential` and `X-Amz-Security-Token` become `REDACTED`, and
`user:password@` is removed from URLs).

## 6. Storage

### 6.1 Keys

`{prefix}{tool}/{cache_version}/{hash}` — e.g. `cache/turbo/v1/9f2c…`. `tool` is `turbo` or `vp`.
For `github` storage the prefix is `hoard/` (GitHub keys are flat strings; max 512 chars).

### 6.2 Envelope format

Every stored object is one blob, so no backend needs user-metadata support and each artifact costs
exactly one upload:

```
bytes 0..4   magic  "HRD1"
bytes 4..8   u32 big-endian header length N (≤ 4096)
bytes 8..8+N JSON header {"v":1,"duration_ms":<u64|null>,"tag":<string|null>,"size":<u64>}
rest         the artifact body exactly as Turborepo sent it (already zstd; never recompress)
```

Decoding a blob with a wrong magic, `N > 4096`, or invalid JSON is a miss (and counted as
`corrupt`), not an error.

### 6.3 Store interface

All backends go through one OpenDAL `Operator` (`services-ghac`, `services-s3`, `services-fs`).

- `put` uses `write_with(...).if_not_exists(true)` when the operator's capability
  `write_with_if_not_exists` is true; otherwise `stat` first and skip if present. An existing key is
  success (`PutOutcome::AlreadyExists`), also when a backend without conditional writes rejects a
  second create with "already exists" (`ghac` answers `409` when another job created the key
  between the `stat` and the write, or — v2 — `CreateCacheEntry` `ok: false`, which
  `@actions/cache` reads as "another job may be creating this cache", e.g. matrix jobs building
  the same hash). A rate-limit error (`ErrorKind::RateLimited`, HTTP `429`) is
  `StoreError::RateLimited { retry_after }`, with `Retry-After` in seconds when the response had one,
  clamped to 10 minutes (a larger value would otherwise stop uploads for the rest of the job).
- `get` is one streaming read of the whole object: the header is parsed from its first bytes
  and the body is the rest of the same read, so a hit costs one storage request (this matters
  under GitHub's download rate limit). `head` (HEAD and the batch query) is one bounded range
  read of the first 4104 bytes and never reads the body.
- `list` and `delete` are only needed for R2 cleanup; `ghac` does not support them (GitHub evicts).
- Memory per request is bounded: S3 uploads buffer at most one 8 MiB multipart part (an artifact
  smaller than that is one part); `ghac` uploads go up in 8 MiB blocks with up to 4 in flight, so
  one `ghac` PUT holds at most about 40 MiB (the 8 MiB chunk being filled plus 4 × 8 MiB blocks
  uploading); fs and GET paths stream; nothing is buffered beyond that bound.
- S3/R2 and `ghac` requests are time-bounded (60 s per operation, 15 s per read/write IO step); a timeout is a
  storage error like any other (GET/HEAD miss, PUT counts `put_errors`), so a hung connection never
  stalls the build.

### 6.4 GitHub cache backend specifics

- Needs `ACTIONS_RUNTIME_TOKEN` and `ACTIONS_RESULTS_URL` (only present in JS-action processes;
  the action passes them to the child; an empty value counts as unset). The action must pass the
  binary its full environment (§10): besides those two, OpenDAL reads `ACTIONS_CACHE_SERVICE_V2`,
  `GITHUB_SERVER_URL` and `ACTIONS_CACHE_URL` from the process environment itself. If missing,
  `serve --storage github` **fails at startup**, before it listens, with
  `ACTIONS_RESULTS_URL is not set: run hoard through the GitHub Action, or use --storage r2|fs`
  (`ACTIONS_RESULTS_URL` is checked first, then `ACTIONS_RUNTIME_TOKEN`) — never a silent
  fallback to disk. OpenDAL picks the cache service version (v2 when `ACTIONS_CACHE_SERVICE_V2` is set and
  the server is not GHES, else v1) and the endpoint (v2: `ACTIONS_RESULTS_URL`; v1:
  `ACTIONS_CACHE_URL`, else `ACTIONS_RESULTS_URL`).
- Cache entries are versioned `hoard-{cache_version}`, so different cache versions never collide.
- Rate limits: 200 uploads/min, 1,500 downloads/min per repo. On `429`, pause all uploads for
  `Retry-After` seconds (default 60, at most 600; a pause is only ever extended), count `rate_limited`, keep
  serving reads. One warning is logged when the pause starts, not one per skipped upload.
- Respect `ACTIONS_CACHE_MODE`: the action maps it to `--mode` (`read` → `read`, `none` → `off`,
  `write-only` → `read-write` with reads disabled via `--no-reads`); the binary only reads `--mode`
  and `--no-reads`.

## 7. R2 auto-cleanup (bucket must not grow endlessly)

### 7.1 Layer 1 — lifecycle rule (safety net, enforced by Cloudflare)

Rule ID `hoard-<prefix with / replaced by ->` (e.g. `hoard-cache-`), filter = `r2-prefix`,
`Expiration.Days = max-age-days`, `AbortIncompleteMultipartUpload.DaysAfterInitiation = 1`.
Other rules are written back with every field the S3 lifecycle model defines; elements unknown
to `aws-sdk-s3` would be lost on the Put. Two prefixes can map to the same rule ID (`a-b/` and `a/b/` both
give `hoard-a-b-`), so do not use both in one bucket: their servers would keep replacing each
other's rule.

On `serve` startup with `storage=r2`:

| `r2-lifecycle` | Behaviour |
|---|---|
| `auto` | `GetBucketLifecycleConfiguration`; if our rule is missing or differs, merge it into the existing rules (keep every other rule unchanged) and `PutBucketLifecycleConfiguration`. On `AccessDenied`: exit non-zero with the fix (below). |
| `verify` | Only read; if missing or different: exit non-zero with the fix. |
| `manual` | Skip; log one warning that the bucket can grow until the size cap runs. |

With `mode=read` or `mode=off` hoard never writes the rule: `auto` only verifies it, and a
missing rule, `AccessDenied` or any other error is a warning (the summary shows
`Lifecycle: not checked (read-only)`, or `Lifecycle: <days> days ✓` when the rule is in place).
`verify` and `manual` behave as in the table. The size cap (§7.2) does not run either.

Any other error while reading or writing the rules (network, timeout, 5xx) is transient: `serve`
logs a warning and starts anyway (the size cap still bounds the bucket); the summary shows
`Lifecycle: not checked (<reason>)`. The whole startup check (Get, maybe Put, retries) is
bounded by 5 s, because it runs before the port file is written and the action waits 10 s for
health; a slower check is a transient error with the summary line
`Lifecycle: not checked (timed out)`. Every lifecycle error message, transient ones included, contains the
fix command. `hoard doctor` always verifies (never writes) the rule.

The fix message must contain the exact command:

```
npx wrangler r2 bucket lifecycle add <bucket> hoard-cache- cache/ --expire-days 7 --abort-multipart-days 1
```

Implemented with `aws-sdk-s3` (rustls) against the R2 endpoint; OpenDAL has no lifecycle API.

### 7.2 Layer 2 — size cap

On shutdown (`storage=r2`, `mode=read-write` only) and `hoard prune`: if the marker
`{prefix}_hoard/last-prune` is younger than 24 h (and not `--force`), skip; a missing,
unparseable or future marker does not throttle. Otherwise list `{prefix}`, sum sizes, delete
objects in ascending `last_modified` order (ties by key) until total ≤ `max-size`, then
overwrite the marker with the current RFC 3339 time (also when nothing was deleted).
Everything under `{prefix}_hoard/` (the marker, `doctor`'s object) is neither counted nor
deleted. A failed delete is counted and the rest go on; the marker is then not written, so the
next run retries. Deletes are free on R2; listing costs one Class A op per 1,000 objects.
`PruneReport { listed, deleted, freed_bytes, remaining_bytes }`. On shutdown the prune is
bounded by 5 minutes and never fails the run: the summary gets `Pruned: N objects, X` when
something was deleted, `Prune: failed (reason)` on an error or timeout, nothing otherwise.
The size cap counts everything under `{prefix}`, so do not nest prefixes in one bucket
(`cache/` and `cache/team/`): a prune of the outer prefix deletes the inner one's objects.

### 7.3 Layer 3 — refresh-on-hit (optional, milestone 3)

`refresh-on-hit: true`: on a GET hit of an object older than `max-age-days / 2`, re-write it once
per run (copy to itself or re-PUT) so popular artifacts never expire. Must be validated against R2
first (does a same-key copy reset the age?).

## 8. Modes and trust

- Default `read-write`.
- The action sets `mode=read` when the workflow's `cache-mode` gives `ACTIONS_CACHE_MODE=read`, or
  when the input `mode: read` is set.
- `storage: r2` on a run without the R2 secrets (fork PRs): the action falls back to
  `storage: github` and writes a warning to the summary; it never fails the job for this.
- Docs recommend `TURBO_REMOTE_CACHE_SIGNATURE_KEY` with R2 (cache shared across branches).

## 9. Stats and job summary

`Stats { hits, misses, puts, put_skipped_existing, put_errors, rate_limited, corrupt,
bytes_down, bytes_up, time_saved_ms }` — `time_saved_ms` sums `duration_ms` of every hit.
The storage line is `Storage: github`, `Storage: r2 (<bucket>/<prefix>)` or `Storage: fs (<root>)`;
when `put_errors > 0` the line ends with `Upload errors: N` (so a broken storage never looks like
a cold cache), then, when `rate_limited > 0`, with `Rate limited: N uploads skipped`.

Summary markdown (written to `summary-file`, appended by the action to `$GITHUB_STEP_SUMMARY`):

```
### hoard
| Hits | Misses | Uploaded | Downloaded | Time saved |
|---|---|---|---|---|
| 42 | 3 | 12.4 MB | 210.0 MB | 6m 12s |
Storage: r2 (my-team-cache/cache/) · Lifecycle: 7 days ✓ · Pruned: 18 objects, 1.2 GB
```

## 10. The GitHub Action (`action/`)

`action.yml` at the repo root (so `uses: keelstack/hoard@v1` works): `runs.using: node24`,
`main: action/dist/main.js`, `post: action/dist/post.js`, `post-if: always()`. Source in
`action/src/`, bundled with esbuild; committed `action/dist/` < 200 KB. Inputs mirror §4.2 plus
`version` (binary version, default = the action's own release; any other value fails, because the
action can only verify its own release — change `uses:` instead). The action manages `port`,
`token`, `port-file`, `token-file`, `summary-file` and `fs-root` itself, so they are not inputs.
Outputs: `hits`, `misses`, `time-saved-ms`, set in the post step (so a later step of the same job
cannot read them; a later job or the job summary can). The R2 account ID and keys reach the binary
through `HOARD_R2_*` env vars, never argv. The storage flags (`--storage`, `--mode`, `--no-reads`,
`--cache-version`, `--fs-root`, `--r2-bucket`, `--r2-prefix`, `--r2-lifecycle`, `--r2-endpoint`)
come from one helper, so every hoard command the action runs gets the same storage.

main:
1. Resolve `mode`/`storage` (§8) and (milestone 3) the tool (`turbo.json` present → turbo;
   `vite.config.*` with `vite-plus` in the root `package.json` → vp; both allowed).
2. Resolve the target from `process.platform`/`process.arch` (anything else fails with
   `unsupported platform <platform>/<arch>`, before any download). Refuse a target with no
   checksum in `action/src/checksums.generated.ts` (written at release time, bundled into `dist/`)
   without downloading. Otherwise use the `@actions/tool-cache` entry if present (GitHub-hosted
   runners only, `RUNNER_ENVIRONMENT=github-hosted`; a self-hosted runner's tool cache persists
   between jobs, so there the binary is always downloaded and verified again), else download
   `hoard-<target>.tar.gz` from the release `v<version>` of the action's own repository
   (`GITHUB_ACTION_REPOSITORY`), verify its SHA-256 before extracting, and cache it. The
   downloaded archive is deleted afterwards. For testing, `HOARD_ACTION_BINARY_PATH` names a
   local hoard binary to use instead (with a warning); it bypasses checksum verification, so only
   the workflow author can set it.

   A failed download is retried 3 times, after 1 s, 2 s and 4 s. If it still fails, the cause is
   transient (network, a github.com outage), not the user's config, so the cache must not fail
   the build: main emits the warning
   `hoard binary could not be downloaded (<cause>); continuing without the cache`, saves it as
   state (`unavailable`) so post writes it as one `> ⚠️` line to the job summary, exports no
   `TURBO_*` (turbo then runs without a remote cache) and succeeds. This is not a silent fallback:
   nothing is cached anywhere else. A checksum mismatch, an unsupported platform, a target with no
   checksum, an archive without the executable and an invalid input are not transient and still
   fail the step.
3. Spawn `hoard serve --port 0` detached (`stdio` to `$RUNNER_TEMP/hoard-<id>.log`, `unref()`)
   with the storage flags, `--port-file`, `--token-file` and `--summary-file` in `$RUNNER_TEMP`
   (`hoard-<id>.port`, `hoard-<id>.token`, `hoard-<id>-summary.md`, removed first) and, for
   `storage: fs`, `--fs-root $RUNNER_TEMP/hoard-<id>-store` (never inside the checkout, where
   untracked files could change task hashes). `<id>` is the step's `GITHUB_ACTION` with every
   character outside `[A-Za-z0-9_-]` replaced by `_`, else a random id, so two hoard steps in one
   job never share files. Pass the full environment (includes
   `ACTIONS_RUNTIME_TOKEN`, `ACTIONS_RESULTS_URL`) plus `HOARD_R2_ACCOUNT_ID` (when set),
   `HOARD_R2_ACCESS_KEY_ID` / `HOARD_R2_SECRET_ACCESS_KEY`, and without any inherited
   `HOARD_TOKEN`.
4. Poll every 100 ms, up to 10 s, for the port file and a `200 ok` from `/_hoard/health`, while
   watching the child. If it exits first (a setup error: bad config, missing GitHub env, missing
   R2 lifecycle rule), fail at once with `hoard exited with code <n> before it was ready`; on
   timeout (a hung server) fail with `hoard did not start within 10s`. Whenever main fails after
   the spawn, it first kills the child (SIGTERM, errors ignored) so no server is orphaned, puts
   the last 20 lines of the log (already redacted by hoard; they carry its error, which says how
   to fix it) in the failure message after `Last lines of the hoard log:`, and prints the log.
5. `core.setSecret(token)`; export `TURBO_API=http://127.0.0.1:<port>`, `TURBO_TOKEN`,
   `TURBO_TEAM=hoard`; save the binary path (for `hoard vp save` in post, milestone 3), port,
   token, pid, summary and log paths and the settings warnings as step state. Each settings
   warning (e.g. the R2 → github fallback, §8) is also a `core.warning`.

post (never fails the job; problems are warnings): if main saved `unavailable` (the binary
download failed), write that one line to `$GITHUB_STEP_SUMMARY` and stop. If main saved no other
state, do nothing. Mask the saved token again with `core.setSecret` before any output (post is a
separate process). Read `GET /_hoard/stats` and set the outputs (`hits`, `misses`,
`time-saved-ms`; `0` when unreachable); when `put_errors > 0`, warn
`hoard: N uploads failed; see the hoard log` (a broken storage must not look like a cold cache)
and print the log. Then `POST /_hoard/shutdown` (hoard answers after writing the summary file;
unreachable → warning `hoard server was not running at shutdown; the summary may be incomplete`,
and the saved pid is killed in case the server is alive but not answering), append the summary
file, if any, plus one `> ⚠️ <warning>` line per warning to `$GITHUB_STEP_SUMMARY`, and print the
log only on a problem or with `RUNNER_DEBUG=1`. A printed log (main or post) is wrapped in
`::stop-commands::<random>` … `::<random>::`, so no log line can act as a workflow command.

Targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`,
`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

Release (`.github/workflows/release.yml`, `workflow_dispatch` with `version` and `draft`; see
`docs/RELEASING.md`): build the five targets, write the checksums file, rebuild `dist/`, commit
`chore(release): vX.Y.Z` and push it with tag `vX.Y.Z` → create the GitHub release → verify it has
exactly the five assets → only then move `vX` to it. `vX` moves only for stable, non-draft releases
and only forward (never below the newest stable `vX.*` tag, so a backport leaves it alone).

## 11. Testing

- Unit tests next to each module (`cargo test`).
- HTTP integration tests run the server in-process on the `fs` store and drive it with `reqwest`.
- S3/R2 tests run against MinIO (`docker compose up minio`), enabled by
  `HOARD_TEST_S3_ENDPOINT`; CI always sets it. MinIO implements lifecycle configuration and
  conditional PUT, which covers §6.3 and most of §7.1.
  MinIO drops `AbortIncompleteMultipartUpload` from lifecycle rules, so the lifecycle tests
  that read the rule back run through a test proxy that stores lifecycle configurations
  verbatim; the rule itself is verified against real R2 by a human.
- End-to-end: `tests/e2e/turbo.sh` builds a fixture Turborepo twice against `hoard serve`,
  deleting `.turbo` and `node_modules/.cache` in between; the second run must print `FULL TURBO`.
- Action: vitest unit tests for input resolution and target selection; a self-test workflow uses
  the action on the fixture with `storage: github` and `storage: r2`.

## 12. Later

- Vite+ remote protocol adapter (`/fetch`, `/blob/{id}`, `/store`, CBOR) once released upstream.
- Packing many small artifacts into one GitHub cache entry per job if large repos hit 200
  uploads/min (milestone 3, gated by a benchmark).
- Nx adapter.

## 13. Milestones

1. **Core + R2 + local** — binary with fs and R2 storage, Turborepo API, lifecycle and size cap,
   `serve`/`env`/`prune`/`doctor`. Usable on laptops and any CI with R2.
2. **GitHub Action + GitHub cache** — `ghac` backend, rate-limit handling, release pipeline, the
   node24 wrapper, self-test workflow.
3. **Vite+ and throughput** — `vp restore/save`, write-behind uploads, refresh-on-hit, packing
   (only if the benchmark shows rate limiting).
