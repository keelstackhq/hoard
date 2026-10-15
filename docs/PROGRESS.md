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

## Blocked

<!-- Task, what is missing, exact action a human must take. -->

- **01-core-r2/Task 8 — real R2 lifecycle check.** MinIO silently drops `AbortIncompleteMultipartUpload`, so the
  1-day abort clause is only tested through a verbatim proxy. Action: with a real R2 bucket and an R2 token with
  *Admin Read & Write*, run `HOARD_R2_ACCESS_KEY_ID=… HOARD_R2_SECRET_ACCESS_KEY=… hoard serve --storage r2
  --r2-account-id <id> --r2-bucket <bucket>` once, stop it, then `hoard doctor --storage r2 --r2-account-id <id>
  --r2-bucket <bucket>` — it must print `✓ lifecycle rule hoard-cache- (7 days)`. If it says the rule differs, report
  the output of `npx wrangler r2 bucket lifecycle list <bucket>`.
  In the same session, check immutable keys for large artifacts on R2: run `STORAGE=r2 tests/e2e/turbo.sh` is not
  enough (MinIO); instead PUT the same >8 MiB artifact twice through `hoard serve --storage r2` (e.g. two parallel
  `curl -X PUT --data-binary @big.bin` against `/v8/artifacts/abc`) and confirm the object is unchanged
  (`If-None-Match: *` on CompleteMultipartUpload must be honoured by R2).
- **01-core-r2 CI — workflow never run on GitHub.** `.github/workflows/ci.yml` (jobs `rust`, `e2e`) triggers only
  on pushes to `main` and on pull requests, so it has not run yet; everything it does passes locally. Action: open a
  PR from `claude/admiring-ride-kl6qd9` (or push to `main`) and check both jobs go green; if `pnpm/action-setup` or
  the MinIO image pull fails, paste the job log.
- **02-github-action/Task 2 Step 3 — release dry run on GitHub.** The action `build` script now exists. Action: on a branch, set `version = "0.0.0-test"` in Cargo.toml (`cargo update -p keel-hoard`), push,
  then `gh workflow run release.yml --ref <branch> -f version=0.0.0-test -f draft=true`. Check: five
  `hoard-<target>.tar.gz` assets on the draft release; the `chore(release): v0.0.0-test` commit's
  `action/src/checksums.generated.ts` lists the same five SHA-256 values as `sha256sum` of the assets; `v0` was NOT
  moved. Afterwards delete the draft release, the `v0.0.0-test` tag and the branch (see docs/RELEASING.md).
- **02-github-action/Task 5 Step 2 — self-test on real runners.** `.github/workflows/self-test.yml` (Linux x64/arm64,
  macOS, Windows × GitHub cache, plus Linux × R2) has never run. Action: (optional, for the R2 leg) add repository
  secrets `R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_BUCKET` (Settings → Secrets and variables →
  Actions); then run it from the Actions tab (*self-test* → *Run workflow* on `claude/admiring-ride-kl6qd9`) or open a
  PR. Expect every `hit` job to log `FULL TURBO` and `hits > 0`, and the job summary to show the hoard table. Paste the
  log of any red job — Windows/macOS legs are the likeliest first failures.
  The same run now includes the Vite+ jobs `vp-warm` / `vp-hit` (GitHub storage, ubuntu-latest): `vp-hit` must find a
  non-empty `node_modules/.vite/task-cache` and log a Vite+ cache hit. This is the first time hoard's GitHub cache
  client (`src/vp/gha_twirp.rs`) talks to the real service — it is only tested against a mock so far; paste its log if
  it fails.
- **03-vite-plus-throughput/Task 3 Step 1 — does R2 reset an object's age on self-copy?** Refresh-on-hit uses
  CopyObject onto itself (MinIO: yes). Action: with a test R2 bucket and token, run
  `HOARD_TEST_R2_ENDPOINT=https://<account>.r2.cloudflarestorage.com HOARD_TEST_R2_KEY=… HOARD_TEST_R2_SECRET=…
  HOARD_TEST_R2_BUCKET=… cargo test --test r2 -- --ignored refresh_real_r2`. If it fails, tell me: refresh then
  switches to a re-PUT of the same envelope and spec §7.3 records it. Until then refresh-on-hit stays off by default.
- **03-vite-plus-throughput/Task 4 Steps 2–3 — run the rate-limit benchmark and decide on packing.** Action: in the
  Actions tab run *bench* three times with `packages` = 100, 400 and 1000 (write-behind `auto`), copy the table from
  each run's summary into `docs/benchmarks/2026-10-rate-limit.md` (template in docs/benchmarks/README.md). Rule: if
  `throttled > 0` at ≤ 400 packages, packing is needed (tell me and I will write the spec section and plan);
  otherwise add "packing not needed (bench 2026-10)" to spec §12.

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
  - `github`, the GitHub Actions cache via OpenDAL ghac (mock-tested; see Blocked).
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
- **Pipelines (written, not yet run on GitHub):** `ci.yml`, a cross-platform `release.yml` with SHA-pinned actions,
  `self-test.yml` (four OSes × GitHub cache, R2, Vite+) and `bench.yml`.
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

In CI, once a release exists: `- uses: keelstackhq/hoard@v1`. The README
lists every input and output.

### Blocked items for you, in a sensible order

1. **CI:** the PR is open; check that `ci.yml` goes green on it (opened 2026-10-15).
2. **Self-test on real runners:** run `self-test.yml`, optionally after adding the four `R2_*` secrets. It includes
   the first real test of the GitHub cache paths (ghac and the Vite+ client).
3. **Release dry run:** `release.yml` with `0.0.0-test`, `draft=true`.
4. **Real R2 checks:**
   - the lifecycle rule with `hoard doctor`;
   - immutable >8 MiB artifacts;
   - `refresh_real_r2`, which decides whether self-copy resets the object's age.
5. **Benchmark:** run `bench.yml` with 100, 400 and 1000 packages, then apply the packing decision (rule above).

After that, cut the first real release (`docs/RELEASING.md`).

### Known limits and decisions to revisit

- Every decision taken on your behalf is in `docs/superpowers/rulings.md`, with what it costs if wrong.
- Deferred minor review findings are in the milestone reviews. For example:
  - no free-space check before spooling (std has none, and no crate was added);
  - matrix legs that run different Vite+ tasks share only the newest leg's cache.
- `CLAUDE.md` was left untouched. Its "Layout" list does not yet mention `src/writebehind.rs`, `src/redact.rs`,
  `src/ratelimit.rs`, `src/doctor.rs` or `scripts/bench/`.
- MinIO for tests is `pgsty/minio` (the `minio/minio` image was removed from Docker Hub).
- In this container, dockerd has to be started by hand.
