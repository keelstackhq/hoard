# Progress

One line per finished task: `YYYY-MM-DD · plan/task · commit · test summary`.

## Done

- 2026-10-14 · 01-core-r2/Task 1 scaffold + config · cd3dead, 8d1b52d · 7 unit + 1 CLI test pass; fmt, clippy clean
- 2026-10-14 · 01-core-r2/Task 2 keys + HRD1 envelope · 49e0065 · 15 unit + 1 CLI test pass; fmt, clippy clean
- 2026-10-14 · 01-core-r2/Task 3 Store on OpenDAL (fs) · f2d9677..452076e · 15 unit + 8 store + 1 CLI test pass; fmt, clippy clean; no openssl
- 2026-10-14 · 01-core-r2/Task 4 stats + summary · 500df22 · 25 unit + 8 store + 1 CLI test pass; fmt, clippy clean
- 2026-10-14 · 01-core-r2/Task 5 Turborepo HTTP API + auth · 6c8ca0f · 30 unit + 17 http (+150 MB streaming test, ignored by default, passes) + 8 store + 1 CLI; fmt, clippy clean; no openssl
- 2026-10-14 · 01-core-r2/Task 6 serve + env commands · bd1ece0, 4dedc46 · 30 unit + 28 http + 8 store + 5 CLI (incl. SIGTERM) pass; fmt, clippy clean; manual serve→PUT→GET→shutdown smoke OK
- 2026-10-14 · 01-core-r2/Task 7 R2 storage via S3 (MinIO) · e505f6f, 86d402d · 37 unit + 13 r2 (MinIO) + 28 http + 12 store + 5 CLI pass; fmt, clippy clean; no openssl/native-tls
- 2026-10-14 · 01-core-r2/Task 8 R2 lifecycle rule + doctor · 1727e57, aa1fc62 · 44 unit + 28 r2 (MinIO + verbatim lifecycle proxy) + 28 http + 12 store + 7 CLI pass; fmt, clippy clean
- 2026-10-14 · 01-core-r2/Task 9 R2 size cap + daily prune · 835631e, e2bc4b5 · 56 unit + 36 r2 (MinIO) + 28 http + 12 store + 8 CLI pass; fmt, clippy clean
- 2026-10-14 · 01-core-r2/Task 10 e2e with real Turborepo 2.11.8 · e246728 · tests/e2e/turbo.sh PASS (fs) and STORAGE=r2 PASS (MinIO): run 2 FULL TURBO, 2 hits, 2 puts, Lifecycle ✓; cargo test all green
- 2026-10-14 · **Milestone 1 reviewed** · whole-branch review 4fcec0d..ac1b931 (0 Critical, 5 Important, 14 Minor) → fix wave f29f34b..a75362b, re-review clean · 64 unit + 39 r2 + 28 http + 12 store + 8 CLI + redaction pass; 150 MB stream OK; e2e fs + r2 PASS
- 2026-10-14 · 02-github-action/Task 1 GitHub cache storage, --no-reads, rate gate · 1c82097, 728b40b · 76 unit + 32 http + 39 r2 + 13 store + 13 CLI + redaction pass; fmt, clippy clean; e2e fs PASS (real ghac only testable on a runner → Task 5)
- 2026-10-14 · 02-github-action/Task 2 steps 1, 2, 4 (release pipeline + write-checksums) · 8f58ac4, b6e4326 · action vitest 12/12, biome + tsc clean, actionlint clean; local x86_64-musl zigbuild static binary OK · Step 3 (GitHub dry run) BLOCKED
- 2026-10-14 · 02-github-action/Task 3 action settings, platform, verified download · b6d2c04 · action vitest 51/51, biome + tsc clean, build reproducible (dist 20.9 KB), actionlint clean
- 2026-10-14 · 02-github-action/Task 4 action main + post · 7d3fc69, bdc2438 · action vitest 95/95 incl. real-binary integration (main → PUT/GET → post: hits=1, summary); biome, tsc, actionlint clean; dist 148.9 KB, fresh
- 2026-10-14 · 02-github-action/Task 5 steps 1, 3 (self-test workflow) · cf7d876, 4b932ff · actionlint clean; e2e fs + r2 PASS with the portable fixture · Step 2 (run on GitHub runners) BLOCKED
- 2026-10-14 · **Milestone 2 reviewed** · whole-branch review cd87105..49fff6b (0 Critical, 5 Important, 11 Minor) → fix wave b6a48b4..890ff79 + follow-ups c218546, 2ab55c1 (90 s download cap, explicit exit), re-reviews clean · cargo 175 pass; action 115 pass incl. real-binary integration; dist 151 KB fresh; actionlint clean; e2e fs PASS
- 2026-10-14 · 03-vite-plus-throughput/Task 1 vp restore/save · f468989, 7d04344, 2072973, e20d525 · cargo all green (96 unit, 37 vp on fs/MinIO/GitHub mock); action 141/141 incl. real binary; dist 157.8 KB; e2e fs + r2 PASS; real Vite+ 1.2.0 round-trip checked by hand · GitHub cache service only mock-tested (self-test leg pending)
- 2026-10-14 · 03-vite-plus-throughput/Task 2 write-behind uploads · b064ff2, 3885bd5 · cargo all green (18 write-behind, 100 lib); 150 MB stream on+off; e2e fs, fs+write-behind, r2 PASS; action 145/145; dist 158 KB
- 2026-10-14 · 03-vite-plus-throughput/Task 3 steps 2–5 (refresh-on-hit) · 2cc23f7 · cargo all green (45 r2 incl. 5 refresh on MinIO); action 148/148; e2e r2 with refresh PASS · Step 1's real-R2 check BLOCKED
- 2026-10-14 · 03-vite-plus-throughput/Task 4 steps 1, 4 (benchmark generator + workflow, new `throttled` stat) · 0c54198, 4744fb2 · node:test 6/6; cargo all green; action 148/148; actionlint clean; local 50-package run: 50 puts then 50 hits · Steps 2–3 BLOCKED
- 2026-10-15 · **Milestone 3 reviewed** · whole-branch review 4f28859..99e1034 (0 Critical, 3 Important, 15 Minor) → fix wave 1e158e1..1b7a69d + flaky-test fix f977bfd, re-reviews clean · cargo all green twice in a row (108 unit, 44 vp, 45 r2, 18 write-behind …); 150 MB stream OK; action 157/157 incl. real binary; dist 159 KB fresh; actionlint clean; bench 6/6; e2e fs + r2 PASS; no openssl
- 2026-10-15 · Naming decided by the owner: `keelstackhq/hoard` (README, spec, plan updated; npm scope `@keelstack` of the private action package unchanged)
- 2026-10-15 · 01-core-r2 CI + 02-github-action/Task 5 Step 2 (first runs on GitHub, PR keelstackhq/hoard#1) · 82a2472 (fix for the `fetch_update` deprecation on Rust 1.99) · ci run 38691600578: `rust`, `e2e`, `action` green; self-test run 38691600620: all 12 jobs green — every `hit` job passed its `FULL TURBO` and `hits > 0` checks on Linux x64/arm64, macOS, Windows (GitHub cache) and Linux (real R2 bucket, `r2-lifecycle: auto` started without error); `vp hit` restored the task cache and logged `vp run: cache hit`
- 2026-10-21 · 02-github-action/Task 2 Step 3 (release dry run) · release run 39659790600 (attempt 2; attempt 1's two macOS jobs got no runner and were cancelled after 15 min, nothing published) · five `hoard-<target>.tar.gz` assets on draft pre-release v0.0.0-test; `sha256sum` of each equals `action/src/checksums.generated.ts` in `chore(release): v0.0.0-test` (c5f9775); x86_64 musl binary is static and prints `hoard 0.0.0-test`; no `v0` tag created · cleanup of release, tag and branch needs the owner (see Blocked)
- 2026-10-21 · 03-vite-plus-throughput/Task 4 Steps 2–3 (rate-limit bench + packing decision) · runs 39659795765 (100), 39662254202 (400), 39664353368 (1000) · 100: throttled 0; 400: throttled 1, rate_limited 22, warm hits 378/400; 1000: throttled 0 (slower build, ~146 uploads/min) → **packing needed**, recorded in spec §12; results in `docs/benchmarks/2026-10-rate-limit.md` · packing spec section + plan not written yet
- 2026-10-22 · **0.1.0 released** · release run 39749863566 → `chore(release): v0.1.0` (5c32424), tags `v0.1.0` and `v0` on it · release v0.1.0 (stable) has the five `hoard-<target>.tar.gz`, their digests equal `checksums.generated.ts` (`VERSION = "0.1.0"`); x86_64 musl binary is static and prints `hoard 0.1.0` · docs pin `@v0` (0.x); 1.0 and `v1` wait for packing; self-test gains a `release` variant (published binary)
- 2026-10-22 · real-R2 owner checks (01-core-r2/Task 8, 03-vite-plus-throughput/Task 3 Step 1) · new `.github/workflows/r2-checks.yml`, run 39752184290 on the repository's R2 bucket · `hoard doctor`: `✓ lifecycle rule hoard-cache- (7 days)` (verify requires the 1-day multipart abort) and `✓ bucket read/write`; immutable >8 MiB: one 20 MiB key PUT twice in a row and another PUT twice in parallel, all 200, GET returns the first / one complete body, stats `puts 2, put_skipped_existing 2, put_errors 0` (the parallel loser was refused by R2's conditional create, so R2 honours If-None-Match on CompleteMultipartUpload); `refresh_real_r2` passes: self-copy moves last_modified forward (…826 → …829), so CopyObject refresh resets the object's age on R2

- 2026-10-22 · ci: `ci-ok` aggregate job (PR keelstackhq/hoard#5, merged) · 2ecc3ac · actionlint clean; ci run green incl. ci-ok
- 2026-10-22 · 04-packing/design · ae72a62, c50fcb3 · spec §6.5 + plan; opus design review (3 Critical, 6 Important) folded in, decisions recorded in the plan
- 2026-10-22 · 04-packing/Task 1 shared Twirp client + Range reads · 8bb6c1d · 8 gha_twirp + 44 vp pass
- 2026-10-22 · 04-packing/Task 2 pack setting, envelope hash, packs stat · 55cf151 · 113 unit pass
- 2026-10-22 · 04-packing/Task 3 pack index · 4e5db71 · 123 unit pass (10 index)
- 2026-10-22 · 04-packing/Task 4 pack backend + Range reads · 6cd654d · 20 pack tests (fs + GitHub mock)
- 2026-10-22 · 04-packing/Task 5 pack writer + shutdown index merge · 319e658 · 34 pack tests, 3× no flakes
- 2026-10-22 · 04-packing/Task 6 server wiring · 6251b7a · 4 http_pack; e2e fs, fs+PACK=on, r2 PASS
- 2026-10-22 · 04-packing/Task 7 bench + docs · 09e962f, 91141ea; whole-branch review fixes 2303f84; diagnostics f1e1f41 · cargo all green (37 pack); CI + self-test (10 legs on the GitHub cache) green
- 2026-10-22 · 04-packing/Task 8 bench with packing · runs 39755912769 (400), 39756606113 (1000) · 400 and 1000: 1 pack, throttled 0, rate_limited 0, put_errors 0, warm hits 400/400 and 1000/1000 → goal met
- 2026-10-22 · **0.2.0 prepared** · `Cargo.toml` and `Cargo.lock` at 0.2.0 · 0.2.0 ships packing (spec §6.5, PR keelstackhq/hoard#9) as 0.x so it runs in real use first; `@v0` moves to 0.2.0; 1.0 and `v1` come later

## Blocked

<!-- Task, what is missing, exact action a human must take. -->

- ~~**Packing (spec §12) — design and plan.**~~ Done 2026-10-22 (spec §6.5, plan 04-packing, PR keelstackhq/hoard#9).
- **Intermittent miss seen once in self-test (2026-10-22).** On PR #9 head 91141ea one of ten GitHub-cache `hit` legs
  missed one of two packed artifacts; its re-run and every later run passed. Cause unknown; hoard now logs index
  loads/writes and the self-test prints the hoard log on a failed check. Action: if it recurs, the log of that job
  shows why — send it to an agent.
- **Required check (owner).** Add `ci-ok` as the required status check in main's branch protection.

## Status

Written 2026-10-15, when the build loop stopped. All three plans are complete except the steps listed under
**Blocked**, which need GitHub, a real R2 bucket or a decision from you. Every milestone has a whole-branch review
recorded above, with its fixes merged and re-reviewed. Branch: `claude/admiring-ride-kl6qd9` (pushed).

### What works (verified locally, 2026-10-15)

- **`hoard` binary (Rust, rustls only, no OpenSSL).** It serves the Turborepo remote-cache API on 127.0.0.1 with a
  bearer token. Storage errors never fail a build: a failed read is a miss, a failed write still answers 200 and is
  counted. Artifacts are streamed, never held in memory.
- **Storage backends:**
  - `fs`, local disk;
  - `r2`, any S3 API (tested against MinIO);
  - `github`, the GitHub Actions cache via OpenDAL ghac (verified on real runners by self-test, 2026-10-15).
- **R2 stays bounded:**
  - a lifecycle rule (`auto`/`verify`/`manual`; foreign rules are kept intact);
  - a daily size cap (`prune`);
  - `doctor`;
  - optional `refresh-on-hit`.
- **Throughput:**
  - write-behind uploads (on by default for `github`), drained within a 270 s shutdown budget;
  - a 429 rate gate;
  - `throttled` / `rate_limited` / `put_errors` counters;
  - a job-summary table.
- **Vite+:** `hoard vp restore|save` archives `node_modules/.vite/task-cache` per OS and architecture. It:
  - picks the newest archive;
  - rejects path traversal and escaping symlinks;
  - skips saves when nothing changed;
  - deletes replaced archives on r2/fs.
- **GitHub Action:** `action.yml` at the root, node24, a 159 KB dist. It:
  - verifies the binary's SHA-256 against checksums baked in at release time;
  - starts hoard, exports `TURBO_*`, restores and saves Vite+;
  - in post: shuts down, writes the job summary, sets outputs and warns on upload errors or throttling.
  It never fails a job because of the cache (a failed binary download is a warning, capped at 90 s).
- **Pipelines:** `ci.yml` and `self-test.yml` (four OSes × GitHub cache, real R2, Vite+) are green on GitHub
  (PR keelstackhq/hoard#1, head 82a2472). The cross-platform `release.yml` with SHA-pinned actions and `bench.yml`
  have not run yet.
- **Last local run:**
  - `cargo fmt`/`clippy` clean, and the full `cargo test` with MinIO green twice in a row;
  - the 150 MB streaming test passes;
  - the action passes 157/157, including a run against the real binary;
  - actionlint clean;
  - `tests/e2e/turbo.sh` passes on fs and on r2 (real Turborepo 2.11.8: the second run is `FULL TURBO`).

### How to try it

```bash
# build and test (Docker needed for MinIO)
docker compose up -d --wait minio
export HOARD_TEST_S3_ENDPOINT=http://127.0.0.1:9000
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
tests/e2e/turbo.sh                      # real Turborepo against hoard on local disk
STORAGE=r2 tests/e2e/turbo.sh           # same, MinIO as R2
pnpm --dir action install && pnpm --dir action lint && pnpm --dir action test

# use it on a laptop with any Turborepo
cargo build --release
./target/release/hoard serve --storage fs &           # or --storage r2 --r2-bucket … (keys via HOARD_R2_* env)
eval "$(./target/release/hoard env)"                   # exports TURBO_API, TURBO_TOKEN, TURBO_TEAM
pnpm turbo run build                                   # second run after `rm -rf .turbo` → FULL TURBO
curl -s -X POST -H "Authorization: Bearer $TURBO_TOKEN" "$TURBO_API/_hoard/shutdown"   # prints final stats

# Vite+ task cache, by hand
./target/release/hoard vp save --storage fs     # after `vp run …`
./target/release/hoard vp restore --storage fs  # in a fresh checkout, after `pnpm install`
```

In CI: `- uses: keelstackhq/hoard@v0`. The README
lists every input and output.

### Blocked items for you, in a sensible order

1. ~~**CI**~~ and ~~**self-test on real runners**~~: green on PR keelstackhq/hoard#1 (2026-10-15).
2. ~~**Release dry run**~~: passed 2026-10-21; the owner removed the leftovers.
3. ~~**Real R2 checks**~~: all pass (2026-10-22, `r2-checks.yml`). Self-copy resets the age on R2, so
   refresh-on-hit works as built; whether to turn it on by default is your call (spec §7.3).
4. ~~**Benchmark**~~: done 2026-10-21, packing needed; its spec section and plan are open (Blocked).

~~First real release~~: 0.1.0 released 2026-10-22 (`@v0`). 0.2.0 adds packing (`@v0`); 1.0 / `@v1` waits for packing to run in real use.

### Known limits and decisions to revisit

- Every decision taken on your behalf is in `docs/superpowers/rulings.md`, with what it costs if wrong.
- Deferred minor review findings are in the milestone reviews. For example:
  - no free-space check before spooling (std has none, and no crate was added);
  - matrix legs that run different Vite+ tasks share only the newest leg's cache.
- `CLAUDE.md` was left untouched. Its "Layout" list does not yet mention `src/writebehind.rs`, `src/redact.rs`,
  `src/ratelimit.rs`, `src/doctor.rs` or `scripts/bench/`.
- MinIO for tests is `pgsty/minio` (the `minio/minio` image was removed from Docker Hub).
- In this container, dockerd has to be started by hand.
