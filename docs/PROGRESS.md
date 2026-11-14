# Progress

Live status only. Finished-task history (one line per task, 2026-10-14 to 2026-10-27) is in
`docs/archive/PROGRESS-through-2026-10-27.md`. When a task lands, replace its line below; do not
append a log here. Keep this file at 40 lines or fewer.

## Status

Updated 2026-11-14. Version **0.5.0** on `main` (`chore(release): v0.5.0`, `@v0`).
Plans 01-core-r2, 02-github-action, 03-vite-plus-throughput and 04-packing are complete and
reviewed. Since then: standalone use outside GitHub Actions (`serve --detach`, `stop`,
`scripts/install.sh`, `circleci/hoard.yml` with a `restore` parameter, 0.4.0-0.5.0) and a
`release` environment that gates publishing (#28).
Release 1.0 and the `v1` tag wait for packing to run in real use.

- Behaviour and decisions: `docs/superpowers/specs/2026-10-14-hoard-design.md`.
- Every decision taken for the owner, with the cost if wrong: `docs/superpowers/rulings.md`.
- Benchmarks and the packing decision: `docs/benchmarks/`.
- Workflows pin every `uses:` to a full SHA (`scripts/check-pinned-uses.sh`, job `workflow-lint`)
  and the Rust toolchain to `rust-toolchain.toml`.

## Blocked

- **Required check (owner).** Add `ci-ok` as the required status check in `main`'s branch
  protection. Not verifiable from this repository.
- **Intermittent self-test miss (2026-10-22).** On PR #9 one of ten GitHub-cache `hit` legs missed
  one of two packed artifacts; the re-run and later runs passed. Cause unknown. hoard logs index
  loads and writes, and self-test prints the hoard log on a failed check. If it recurs, give that
  job log to an agent.
- **CircleCI command (2026-10-27).** A real CircleCI run and `~` in cache keys are unverified;
  only offline tests cover them.
- **Org decision D-007 (workflow triggers).** Not in `docs/agents/org-decisions.md`. The trigger
  changes in this repo follow the task brief; check them against D-007 when the channel syncs.

## Known limits

- No free-space check before spooling write-behind uploads (std has none; no crate added).
- Matrix legs that run different Vite+ tasks share only the newest leg's cache.
- Tests need MinIO (`pgsty/minio`; the `minio/minio` image left Docker Hub). In a container,
  start dockerd by hand.
