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

## Status

<!-- Written when the build loop stops. -->
