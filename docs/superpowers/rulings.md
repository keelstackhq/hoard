# Controller rulings

Decisions taken on the owner's behalf while building milestones 1–3 (see docs/PROGRESS.md "Status").
Each line: what was decided — why — what it costs if wrong. Rework any you disagree with.

## Milestone 1 (01-core-r2)
- LifecycleMode lives in src/config.rs (Task 1, as the Interfaces say) and is re-exported/used by r2::lifecycle — Interfaces are more specific than the File Structure comment — cost if wrong: a move between modules.
- token-file default `$RUNNER_TEMP/hoard.token` or `./.hoard/token`, mirroring port-file; add it to spec §4.2 table — cost if wrong: rename of a default.
- docker-compose uses `pgsty/minio:RELEASE.2026-08-04T00-00-00Z` (maintained MinIO fork, same API, includes `mc`) instead of the deleted `minio/minio` — cost if wrong: swap one image line.
- Task 8 limited user is created via `mc` inside the MinIO container (compose init / test helper using `docker compose exec`, or env-provided creds `HOARD_TEST_S3_LIMITED_KEY/SECRET`); decided concretely at Task 8 dispatch — cost if wrong: test plumbing only.
- **Task 2:** decode_header returns BadMagic for <8-byte input whose bytes already mismatch "HRD1" (reviewer flagged vs "<8 → Incomplete") — controller dispatch explicitly allowed it; both are decode errors and Store treats every decode error as corrupt, so no caller distinguishes them — cost if wrong: one test line + 3-line swap.
- **Task 3:** shared backend-agnostic store checks live in tests/common/store_checks.rs, run on fs by tests/store.rs (plan put Task 3 tests in src/store.rs) — integration tests (tests/r2.rs, Task 7) cannot reach cfg(test) items — cost if wrong: moving test files.
- **Task 3:** fs atomic_write_dir = <fs_root>/.hoard-tmp (inside root, not sibling) — avoids EXDEV on mount points — cost if wrong: one path.
- **Task 5:** PUT Header.size = Content-Length; missing CL → skip store, 200, put_errors; body length mismatch → stream error → store aborts — spec §6.2 needs size before the body — cost if wrong: chunked uploads not cached.
- **Task 5:** mode=off → GET/HEAD 404 without store/stats, PUT 403; HEAD counts misses only, GET counts hits+misses; corrupt counts corrupt+misses — spec silent — cost if wrong: stat numbers.
- **Task 5:** sha2 allowed as dev-dependency (plan's own test says "assert equal sha256") — cost if wrong: one dev-dep.
- **Task 5:** oversized encoded header (huge x-artifact-tag) → skip store, 200, put_errors (closes Task 2 deferred minor).
- **Task 6:** summary is written BEFORE /_hoard/shutdown responds 200 (AppState.shutdown carries a oneshot reply) — the action reads the summary right after the POST — cost if wrong: small refactor.
- **Task 6:** fs storage key prefix "cache/"; serve --storage github fails at startup until M2 — cost if wrong: one constant.
- **Task 6:** port/token/summary files written atomically (tmp+rename), token 0600 and before the port file; port+token files removed on shutdown.
- **Task 6:** `serve` never prints the token (logs URL + `eval "$(hoard env)"` hint); spec §2 comment updated; `hoard env` prints the export lines — CLAUDE.md "never log secrets" outranks the spec comment — cost if wrong: laptop UX one extra command.
- **Task 7:** new setting r2-endpoint (HOARD_R2_ENDPOINT) overrides the R2 endpoint; account-id then optional — e2e (Task 10) and laptops need MinIO — cost if wrong: one extra public flag.
- **Task 7:** aws-sdk-s3/aws-config added in Task 7 (not Task 8) for MinIO test buckets; rustls only.
- **Task 7:** S3 writer chunk 8 MiB; .hoard-tmp list filter scoped to fs only.
- **Task 7:** S3 operator gets opendal TimeoutLayer (60 s op / 15 s io), no RetryLayer (needs backon) — a hung R2 must degrade to a miss, not stall the build — cost if wrong: two constants.
- **Task 8:** lifecycle check transient errors (network/timeout/5xx) at serve startup → warn + continue, summary "Lifecycle: not checked"; only AccessDenied / Missing (verify) fail — CLAUDE.md: only setup errors fail; prune still caps the bucket — cost if wrong: a bucket briefly without the rule on a flaky start.
- **Task 8:** the MinIO limited user (no PutLifecycleConfiguration) is created by the compose minio entrypoint, so `docker compose up -d --wait minio` and CI need no extra step — cost if wrong: compose script complexity.
- **Task 8:** rule match accepts filter.prefix, filter.and.prefix (no other conditions) or legacy prefix — R2 and MinIO return different shapes.
- **Task 8:** MinIO silently drops AbortIncompleteMultipartUpload → strict rule match kept; idempotency/doctor tests run through a test-only verbatim lifecycle proxy; real-R2 verification goes to Blocked — CLAUDE.md requires the 1-day abort — cost if wrong: test plumbing only.
- **Task 8:** LifecycleError::Missing gains `differs: bool` (interface change from brief) — needed for "missing" vs "differs" message.
- **Task 9:** everything under {prefix}_hoard/ excluded from prune size+deletion; marker is a plain overwrite; future/garbage marker → run — cost if wrong: one extra prune per day.
- **Task 9:** shutdown prune bounded by 5 min overall; failure → "Prune: failed (…)" summary line, exit 0; summary line only when objects were deleted — cost if wrong: summary wording.
- **Task 9:** `hoard prune` supports r2 and fs (fs for local/tests) — spec says R2; harmless superset.
- **Task 9:** (implementer choice accepted) no marker after any failed delete (retries next run; Class A cost negligible); CLI exits non-zero on delete failures.
- CLAUDE.md "never held fully in memory" stays; spec §6.3 records the 8 MiB-part bound instead of editing CLAUDE.md (user-owned file).
- lifecycle rule-ID collision (a-b/ vs a/b/) documented only — ID format is spec-mandated and must match the wrangler command — cost if wrong: two prefixes share one rule.
- read-mode 403 code is `read_only`, not `forbidden` (which makes turbo disable the remote cache, reads included).

## Milestone 2 (02-github-action)
- ghac env checks go through an injectable lookup (`ghac_operator_from(version, |k| env)`) and the public `ghac_operator(cache_version)` reads the process env; tests use the injectable form — avoids unsafe env mutation in parallel tests — cost if wrong: none.
- commit a placeholder `action/src/checksums.generated.ts` (VERSION "0.0.0-dev", CHECKSUMS empty `Partial<Record<Target,string>>`); ensureBinary refuses any target without a checksum ("no verified hoard binary for <target> in this action build") — never download unverified — cost if wrong: one file regenerated at release.
- release download repo = `process.env.GITHUB_ACTION_REPOSITORY` (set by GitHub to the action's own repo) falling back to constant `keelstackhq/hoard` (the real remote); spec/README still say `keelstack/hoard` — naming question goes to Blocked for the human — cost if wrong: one constant.
- **Task 2:** Task 2 scaffolds the minimal action/ package (vitest, biome, tsconfig) and a platform.ts with TARGETS/Target only — the plan's Task 2 test lives in action/test but action/ is created in Task 3 — cost if wrong: none (Task 3 extends).
- **Task 2:** CHECKSUMS typed Partial<Record<Target,string>> in both placeholder and generated file; ensureBinary rejects missing entries.
- **Task 3:** action inputs = storage, mode, cache-version, r2-* (incl. r2-endpoint), r2-lifecycle, max-age-days, max-size, version; ports/token/file paths are managed by the action, not inputs — spec §4.2 lists server settings, not all make sense as action inputs — cost if wrong: add inputs later.
- **Task 3:** `version` input must be empty or equal the baked VERSION (only the action's own release is verified) — never download unverified — cost if wrong: input mostly decorative.
- **Task 3:** R2 secrets "available" = key id + secret + bucket + (account id or endpoint).
- **Task 3:** Task 3 adds the esbuild build + stub main/post + CI action job; Task 4 replaces the stubs.
- **Task 4:** testing hook env HOARD_ACTION_BINARY_PATH makes main use a given binary (warns "unverified") — needed for the self-test before any release exists — only the workflow author can set it — cost if wrong: one env check to remove.
- **Task 4:** serveEnv drops inherited HOARD_TOKEN so a stray env var can't fix the token; R2 secrets only in child env.
- **Task 4:** tool-cache short-circuit only on RUNNER_ENVIRONMENT=github-hosted (self-hosted always re-downloads + verifies).
- **Task 5:** self-test builds hoard from source and uses HOARD_ACTION_BINARY_PATH (no release exists yet); fixture build scripts made portable (node instead of sleep/mkdir) for Windows runners; cache-version includes run_attempt.
- **Task 5:** hit job reads hits from /_hoard/stats (action outputs are set in post, unreadable by later steps in the same job) — plan's steps.cache.outputs.hits is impossible in-job — cost if wrong: none
- **Task 5 (reversed):** cache-version drops run_attempt (re-run of failed hit job could never match) and adds os+storage (per-leg isolation) — reviewer finding — cost if wrong: none
- binary exiting before healthy fails fast with the log tail in the setFailed message (reviewer raised the deferred minor to Important — agreed).
- transient release-binary download failure (after 3 retries) → warning + summary note, step succeeds, no TURBO_* export; checksum mismatch / unsupported platform / missing checksum / bad input still fail — CLAUDE.md "never fail the build because of the cache"; not a silent disk fallback — cost if wrong: a broken release URL shows as warnings instead of red builds.
- put_errors > 0 → binary summary line "Upload errors: N" + action warning.
- one focused follow-up fix (outside the one-wave rule) — outer retry 1×, no 4xx retry, 90 s cap on binary acquisition; outputs "0" + warnings when unavailable — a stalled build violates "never fail the build" — cost if wrong: slow release CDN days give "continuing without cache" sooner.

## Milestone 3 (03-vite-plus-throughput)
- GitHub vp archives go through GhaTwirp end to end (create/upload/finalize/get with restore_keys), not OpenDAL ghac — the version hash and restore-key lookup must match between save and restore — cost if wrong: two code paths to the cache service.
- write-behind workers apply the RateGate (skip + count rate_limited while paused) — the M2 rule "on 429 pause uploads" must survive the queue — cost if wrong: none.
- **Task 1:** os-arch uses Node names (linux-x64, darwin-arm64, windows-x64…) so action and binary agree; vp storage errors exit 0 (warn), only setup errors fail; restore extracts to a temp dir then swaps.
- **Task 1:** GhaTwirp uses the v2 cache service even when ACTIONS_CACHE_SERVICE_V2 is unset (v1 is being retired); version = sha256("hoard-vp-{cache_version}").
- **Task 1:** sha2 promoted to a normal dependency (already in the normal graph via aws-sdk-s3; needed for the GhaTwirp version hash) — cost if wrong: none
- **Task 1:** default run key gets an 8-hex random suffix (plan's Review Focus 5 assumed run_id-attempt is unique per job; it isn't for matrix legs) — cost if wrong: none (restore picks newest).
- **Task 2:** `write-behind` is tri-state auto|on|off (auto = on for github) — users can opt out on github — cost if wrong: one extra value.
- **Task 2:** spool under RUNNER_TEMP capped at 2 GB, synchronous fallback beyond; dedup queued keys; GET of a queued key is a miss; workers honour RateGate (re-queue while paused).
- **Task 2:** one 270 s shutdown budget shared by drain and prune; /_hoard/shutdown returns final stats JSON so post sees background failures — cost if wrong: tighter prune window when the queue is long.
- **Task 3:** Mutex<HashSet> instead of DashSet (not a dependency); refresh does one HEAD per first hit (age), CopyObject self-copy with MetadataDirective::Replace, spawned after the response, ≤4 concurrent, skip when saturated; new stat `refreshed`; real-R2 verification is an #[ignore] test for a human → Blocked.
- **Task 3:** refresh-on-hit only in read-write mode (it is a write) — accepted from implementer.
- **Task 4:** bench reads rate_limited from /_hoard/stats inside the job (action outputs only carry hits/misses/time-saved); Steps 2–3 (GitHub runs + packing decision) → Blocked. Dispatched at base fd631ac.
- **Task 4:** new stat `throttled` = every 429 response received (rate_limited keeps meaning 'uploads skipped'); bench and the Step 3 rule use throttled — without it the bench can't see throttling under write-behind — cost if wrong: one extra stat field.
- vp save skips when the tree fingerprint equals the restored one; on r2/fs the replaced archive is deleted after the pointer moves (concurrent restore of it = miss) — keeps R2 bounded and GitHub quota low — cost if wrong: a rare miss.
- plan M3 Task 1 Step 4 stays ticked (local PASS); the GitHub vp path joins the self-test Blocked item.
- no free-space check before spooling (std has none; no new crate) — spools are capped at 2 GB each and a full disk is a put_error, never a failed build — cost if wrong: a CI disk can fill.
