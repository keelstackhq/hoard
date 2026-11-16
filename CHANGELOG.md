# Changelog

Newest first. The version in `Cargo.toml` is the last release; "Unreleased" ships as the next minor.

## 0.6.1 — 2026-11-16

- The action prints a notice when `TURBO_API`, `TURBO_TOKEN` or `TURBO_TEAM` was already set (it replaces them) and says that a step-level `env:` or turbo flags still win. New README section "Add it to an existing repository"; pins moved to 0.6.0; the 429 pause and the read breaker (not applied to packed reads) are documented; CircleCI guide: corepack, existing turbo setup.
- Release workflow: build legs export each tarball's sha256 and publish compares them with the re-downloaded files; detect fails before any build when `scripts/release-complete.sh` reports a partial release (exit 4), and the script tells an API error (1) from a tag with no visible release (5).
- Releases start when the version-bump PR is merged to `main` (the owner still approves the `release` environment). `workflow_dispatch` stays as a fallback and its `version` input is optional. The release commit (checksums, `action/dist`) is now reachable only from the `vX.Y.Z` tag: the workflow pushes tags, never `main`, so merging meanwhile is safe and no branch-protection bypass is needed. A tag without a release (partial failure) fails the run instead of ending green.

## 0.6.0 — 2026-11-14

- **Breaking:** `--token` and `--r2-secret-access-key` are rejected. A secret on the command line
  is visible to other processes. Set `HOARD_TOKEN` and `HOARD_R2_SECRET_ACCESS_KEY` instead. The
  GitHub Action is not affected: it already passes secrets through the environment.
- A non-UTF-8 command-line argument is refused ("argument N is not valid UTF-8") instead of panicking or being printed.
- A batch query with a non-string hash no longer turns every lookup in the batch into a miss.
- A busy `--port` no longer fails the build: hoard falls back to a free 127.0.0.1 port and warns. The port file holds the real port.
- Read circuit breaker: after three read errors in a row, reads miss for 30 s, then probe again, so dead or slow storage cannot stall every task. Writes are not affected.
- Release builds use `[profile.release]` with fat LTO, one codegen unit and stripped symbols.
- New CircleCI guide: `docs/circleci.md`, with a full config.

## 0.5.0

- CircleCI command takes a `restore` parameter.
- Run hoard outside GitHub Actions: `hoard serve --detach`, `hoard stop`, `scripts/install.sh`.
