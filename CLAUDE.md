# hoard

One-line CI cache for TypeScript monorepos (Turborepo, Vite+). A Rust binary serves the Turborepo
remote-cache API on localhost and stores artifacts in the GitHub Actions cache, a Cloudflare R2
bucket, or the local filesystem. A small node24 GitHub Action downloads, starts and stops it.

- Design: `docs/superpowers/specs/2026-10-14-hoard-design.md` — the source of truth. Read the
  section you touch before changing behaviour; update the spec in the same PR when behaviour changes.
- Plans: `docs/superpowers/plans/` — one per milestone; tick checkboxes as tasks land.
- Progress: `docs/PROGRESS.md` — one line per finished task, plus anything blocked on a human.

## Layout

```
src/               Rust crate (lib + bin "hoard")
  config.rs        CLI flags, HOARD_* env vars, defaults
  key.rs           hash validation, key layout {prefix}{tool}/{version}/{hash}
  envelope.rs      HRD1 blob format (header + raw artifact body)
  store.rs         Store over an OpenDAL Operator (ghac | s3→R2 | fs)
  server/          axum router: Turborepo /v8 API, auth, /_hoard/*
  r2/              lifecycle rule (aws-sdk-s3) and size-cap prune
  vp/              Vite+ task-cache restore/save
tests/             integration tests (http.rs, r2.rs, vp.rs), e2e/, fixtures/
action.yml         GitHub Action entry (repo root)
action/            TypeScript action source (src/, test/) and committed dist/
scripts/           release and benchmark helpers
```

## Commands

```bash
docker compose up -d minio                       # S3/R2 tests need it
export HOARD_TEST_S3_ENDPOINT=http://127.0.0.1:9000
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test                                       # unit + integration
cargo test -- --ignored                          # large-artifact streaming test
tests/e2e/turbo.sh                               # real turbo, fs storage
STORAGE=r2 tests/e2e/turbo.sh                    # real turbo, MinIO as R2
pnpm --dir action install && pnpm --dir action lint && pnpm --dir action test
pnpm --dir action build                          # must keep action/dist < 200 KB
```

Run the fast checks (fmt, clippy, the tests of the crate or package you changed) before every
commit. Run the e2e script before pushing anything that touches `server/`, `store.rs` or `envelope.rs`.

## Rules that must never break

- **Never fail the user's build because of the cache.** Storage errors: GET/HEAD → 404 (miss),
  PUT → 200 and count `put_errors`. Only setup errors (bad config, missing GitHub env, missing R2
  lifecycle rule in `auto`/`verify`) fail, and their message must say how to fix it.
- **Listen on 127.0.0.1 only.** Every route except `/_hoard/health` checks the bearer token in
  constant time.
- **Never log secrets:** the token, R2 keys, `ACTIONS_RUNTIME_TOKEN`, or any URL with `sig=` or
  `X-Amz-Signature`. Secrets reach the binary through env vars, never argv.
- **Stream, don't buffer.** No artifact is ever held fully in memory. Never recompress the
  artifact body (Turborepo already sends zstd).
- **No silent fallbacks.** If GitHub cache env vars are missing, fail; do not cache to disk instead.
- **The R2 bucket must stay bounded.** Keep the lifecycle rule (expire after `max-age-days`,
  abort multipart after 1 day) and the size-cap prune working; never drop other lifecycle rules.
- **Keys are immutable.** Writes use if-not-exists; an existing key is success.
- **Hashes match `^[a-fA-F0-9]{1,128}$`** before they reach any key.
- `rustls` only; no OpenSSL (static musl release builds).

## How to work here

- Test first: write the failing test, watch it fail, implement, watch it pass, commit
  (superpowers:test-driven-development).
- Execute plans task by task (superpowers:subagent-driven-development or executing-plans); one
  commit per task, message as in the plan (`feat:`, `fix:`, `test:`, `ci:`, `docs:`).
- Keep files focused; a new responsibility gets a new module, not a bigger file.
- Public behaviour (flags, env vars, routes, key layout, envelope, action inputs/outputs) changes
  only together with the spec and the README.
- Dependencies: ask before adding a crate or npm package not listed in the plan's Tech Stack.
- Before claiming something works, show the command output (superpowers:verification-before-completion).

## Gotchas

- Only JavaScript actions receive `ACTIONS_RUNTIME_TOKEN` / `ACTIONS_RESULTS_URL`; the action
  passes them to the detached binary. `run:` steps cannot use the GitHub cache backend directly.
- GitHub cache limits: 10 GB per repo (free), 200 uploads/min and 1,500 downloads/min, branch
  scoping, immutable keys. On 429 pause uploads, keep reads.
- R2 lifecycle age counts from the last write, not the last read.
- OpenDAL ignores `if_not_exists` unless the service reports `write_with_if_not_exists`; fall back
  to `stat` first.
- Vite+ remote caching is unreleased upstream; until it ships we only save/restore its
  task-cache directory.

## Org decisions

Cross-repo decisions from keelstackhq/decisions; follow them. To change one, open a PR there.
The hook refreshes this snapshot; commit an update alone as `chore: sync org decisions`.

@docs/agents/org-decisions.md
