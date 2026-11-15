# Changelog

Newest first. The version in `Cargo.toml` is the last release; "Unreleased" ships as the next minor.

## 0.6.0 — 2026-11-14

- **Breaking:** `--token` and `--r2-secret-access-key` are rejected. A secret on the command line
  is visible to other processes. Set `HOARD_TOKEN` and `HOARD_R2_SECRET_ACCESS_KEY` instead. The
  GitHub Action is not affected: it already passes secrets through the environment.
- A non-UTF-8 command-line argument is refused ("argument N is not valid UTF-8") instead of panicking or being printed.

## 0.5.0

- CircleCI command takes a `restore` parameter.
- Run hoard outside GitHub Actions: `hoard serve --detach`, `hoard stop`, `scripts/install.sh`.
