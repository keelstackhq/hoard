# hoard

One-line CI cache for TypeScript monorepos (Turborepo and Vite+), stored in the GitHub Actions
cache or your own Cloudflare R2 bucket. Part of [keelstack](https://github.com/keelstack): tools
for bootstrapping projects.

> Status: under construction. See `docs/superpowers/specs/2026-10-14-hoard-design.md`.

```yaml
- uses: keelstack/hoard@v1
- run: pnpm turbo run build test lint
```

With Cloudflare R2 (no size or rate limits, shared across branches and laptops; the bucket is
kept bounded by a lifecycle rule and a size cap):

```yaml
- uses: keelstack/hoard@v1
  with:
    storage: r2
    r2-account-id: ${{ secrets.R2_ACCOUNT_ID }}
    r2-access-key-id: ${{ secrets.R2_ACCESS_KEY_ID }}
    r2-secret-access-key: ${{ secrets.R2_SECRET_ACCESS_KEY }}
    r2-bucket: ${{ secrets.R2_BUCKET }}
```

## Commands

The action runs the `hoard` binary for you. You can also run it yourself, for example on a
laptop:

| Command | What it does |
|---|---|
| `hoard serve` | Runs the cache server on 127.0.0.1 until `POST /_hoard/shutdown`, SIGTERM or Ctrl-C. |
| `hoard env` | Prints the `export TURBO_API/TURBO_TOKEN/TURBO_TEAM` lines for a running server. |
| `hoard prune [--force]` | Deletes the oldest objects until the R2 prefix fits in `max-size` (at most once per 24 h without `--force`). |
| `hoard doctor` | Checks the storage credentials, bucket read/write and the R2 lifecycle rule, and prints the fixes. |

Laptop, sharing the CI bucket. Use a read-only R2 token (object read) and `--mode read`, so
hoard never writes artifacts or the lifecycle rule:

```bash
export HOARD_R2_ACCOUNT_ID=... HOARD_R2_ACCESS_KEY_ID=... HOARD_R2_SECRET_ACCESS_KEY=...
hoard serve --storage r2 --r2-bucket my-team-cache --mode read &
eval "$(hoard env)"
pnpm turbo run build
```

R2 token permissions: with the default `r2-lifecycle: auto`, a read-write server needs Admin
Read & Write, because it sets the bucket's lifecycle rule. `--r2-lifecycle verify` only reads
the rule (Admin Read, plus object read and write); with `--r2-lifecycle manual`, object read
and write is enough. In both cases a bucket admin adds the rule (`hoard doctor` prints the
command). With `--mode read`, object read is enough.

## Settings

Every setting is a flag (`--max-size 5GB`) and an environment variable (`HOARD_MAX_SIZE=5GB`).
The full list with defaults is the table in
[spec §4.2](docs/superpowers/specs/2026-10-14-hoard-design.md#42-configuration).

License: MIT.
