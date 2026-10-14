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

Laptop (same binary, same bucket, ideally a read-only token):

```bash
hoard serve --storage r2 --r2-bucket my-team-cache &   # prints how to load the env
eval "$(hoard env)"                                    # sets TURBO_API/TOKEN/TEAM
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
| `hoard prune` | Enforce `max-size` on R2 (or fs) now (ignores the 24 h throttle with `--force`); prints `pruned N of M objects, freed X, Y remain (max Z)` or `skipped: last prune < 24 h ago (use --force)`. |
| `hoard doctor` | Check storage credentials, bucket access and the R2 lifecycle rule; print fixes. The access check writes, reads and deletes `{prefix}_hoard/doctor`, also with `mode=read`. |
| `hoard vp restore` / `vp save` | Restore or save the Vite+ task-cache directory (milestone 3). |

### 4.2 Configuration

Precedence: CLI flag > environment variable > default. Every flag `--foo-bar` has an env var
`HOARD_FOO_BAR`. An empty `HOARD_*` variable counts as unset (GitHub Actions exports empty
secrets as `""`).

| Setting | Default | Notes |
|---|---|---|
| `storage` | `github` | `github` \| `r2` \| `fs` |
| `mode` | `read-write` | `read-write` \| `read` \| `off`; see §8 |
| `cache-version` | `v1` | Part of every key; bump to purge |
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
| `HEAD /v8/artifacts/{hash}` | `200` + `Content-Length` and `x-artifact-duration`/`x-artifact-tag` headers if present, else `404`. Must not download the body. Counts misses only (the following GET counts the hit). |
| `GET /v8/artifacts/{hash}` | `200` streaming body (`application/octet-stream`, `Content-Length`, artifact headers) or `404` |
| `PUT /v8/artifacts/{hash}` | Stream body to the store; `200 {"urls":[]}`. Existing key → also `200`. `mode=read`/`off` → `403`. Needs `Content-Length` (becomes the envelope `size`); missing, a body of a different length, or a header over 4096 bytes → not stored, `200`, counted as `put_errors`. |
| `POST /v8/artifacts/events` | Accept (body ignored); `200` |
| `POST /v8/artifacts` | Body `{"hashes":[…]}` → `200` JSON object mapping each found hash to `{"size":<u64>,"taskDurationMs":<u64\|null>}`; missing hashes are left out |
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
  success (`PutOutcome::AlreadyExists`).
- `list` and `delete` are only needed for R2 cleanup; `ghac` does not support them (GitHub evicts).
- S3/R2 requests are time-bounded (60 s per operation, 15 s per read/write IO step); a timeout is a
  storage error like any other (GET/HEAD miss, PUT counts `put_errors`), so a hung connection never
  stalls the build.

### 6.4 GitHub cache backend specifics

- Needs `ACTIONS_RUNTIME_TOKEN` and `ACTIONS_RESULTS_URL` (only present in JS-action processes;
  the action passes them to the child). If missing, `serve --storage github` **fails at startup**
  with a clear message — never a silent fallback to disk.
- Rate limits: 200 uploads/min, 1,500 downloads/min per repo. On `429`, pause all uploads for
  `Retry-After` seconds (default 60), count `rate_limited`, keep serving reads.
- Respect `ACTIONS_CACHE_MODE`: the action maps it to `--mode` (`read` → `read`, `none` → `off`,
  `write-only` → `read-write` with reads disabled via `--no-reads`); the binary only reads `--mode`
  and `--no-reads`.

## 7. R2 auto-cleanup (bucket must not grow endlessly)

### 7.1 Layer 1 — lifecycle rule (safety net, enforced by Cloudflare)

Rule ID `hoard-<prefix with / replaced by ->` (e.g. `hoard-cache-`), filter = `r2-prefix`,
`Expiration.Days = max-age-days`, `AbortIncompleteMultipartUpload.DaysAfterInitiation = 1`.
Other rules are written back with every field the S3 lifecycle model defines; elements unknown
to `aws-sdk-s3` would be lost on the Put.

On `serve` startup with `storage=r2`:

| `r2-lifecycle` | Behaviour |
|---|---|
| `auto` | `GetBucketLifecycleConfiguration`; if our rule is missing or differs, merge it into the existing rules (keep every other rule unchanged) and `PutBucketLifecycleConfiguration`. On `AccessDenied`: exit non-zero with the fix (below). |
| `verify` | Only read; if missing or different: exit non-zero with the fix. |
| `manual` | Skip; log one warning that the bucket can grow until the size cap runs. |

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
`version` (binary version, default = the action's own release). R2 secrets reach the binary through
`HOARD_R2_*` env vars, never argv.

main:
1. Resolve `mode`/`storage` (§8) and the tool (`turbo.json` present → turbo; `vite.config.*` with
   `vite-plus` in the root `package.json` → vp; both allowed).
2. Download `hoard-<target>.tar.gz` from the GitHub release of `version`, verify its SHA-256
   against `checksums.json` baked into `dist/` at release time, cache it with `@actions/tool-cache`.
3. Spawn `hoard serve` detached (`stdio` to `$RUNNER_TEMP/hoard.log`, `unref()`), passing the
   full environment (includes `ACTIONS_RUNTIME_TOKEN`, `ACTIONS_RESULTS_URL`).
4. Poll `/_hoard/health` every 100 ms up to 10 s; on timeout print the log and fail.
5. `core.setSecret(token)`; export `TURBO_API=http://127.0.0.1:<port>`, `TURBO_TOKEN`,
   `TURBO_TEAM=hoard`.

post: `POST /_hoard/shutdown`, append the summary to `$GITHUB_STEP_SUMMARY`, set outputs
(`hits`, `misses`, `time-saved-ms`), print the log only on failure or `RUNNER_DEBUG=1`.

Targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`,
`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

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
