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
- **Naming: `keelstack` vs `keelstackhq`.** The spec and README say `uses: keelstack/hoard@v1`, but this repo is
  `keelstackhq/hoard`. The action downloads release binaries from `$GITHUB_ACTION_REPOSITORY` (falls back to
  `keelstackhq/hoard`), so it works either way. Action: decide the public name; if it is `keelstack/hoard`, create or
  transfer the repo and update nothing else; if it stays `keelstackhq`, replace `keelstack/hoard` in README.md and
  spec §0/§2 (one search-and-replace).
- **02-github-action/Task 2 Step 3 — release dry run on GitHub.** Needs Task 3's `build` script first (it lands next
  in this loop). Action: on a branch, set `version = "0.0.0-test"` in Cargo.toml (`cargo update -p keel-hoard`), push,
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

## Status

<!-- Written when the build loop stops. -->
