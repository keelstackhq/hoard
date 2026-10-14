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

The action lives at [keelstackhq/hoard](https://github.com/keelstackhq/hoard) for now; the
`keelstack/hoard` name above is not final.

Pin a release tag (`@v1` or `@v1.2.3`), not `@main` or a commit: each action release can only
run the hoard binary of that same release, and code on `main` may need a newer binary than the
last release has.

## Action inputs

All inputs are optional. Inputs marked (r2) are used only with `storage: r2`.

| Input | Default | Meaning |
|---|---|---|
| `storage` | `github` | Where artifacts are stored: `github` (GitHub Actions cache), `r2` (Cloudflare R2) or `fs` (runner disk). |
| `mode` | empty | `read-write`, `read` or `off`. Empty: from the workflow's cache mode (see below), else `read-write`. |
| `cache-version` | `v1` | Part of every cache key; change it to start from an empty cache. |
| `r2-account-id` | — | Cloudflare account ID (r2). Not needed when `r2-endpoint` is set. |
| `r2-access-key-id` | — | R2 access key ID (r2). Pass it from a secret. |
| `r2-secret-access-key` | — | R2 secret access key (r2). Pass it from a secret. |
| `r2-bucket` | — | R2 bucket name (r2). |
| `r2-prefix` | `cache/` | Key prefix inside the bucket, ending in `/` (r2). |
| `r2-endpoint` | — | S3 endpoint URL override, e.g. MinIO (r2). Replaces `r2-account-id`. |
| `r2-lifecycle` | `auto` | `auto` (set the bucket's expiry rule), `verify` (fail if it is missing) or `manual` (r2). |
| `max-age-days` | `7` | Delete R2 objects this many days after their last write, 1–36500 (r2). |
| `max-size` | `5GB` | Size cap for the R2 prefix; the oldest objects are pruned above it (r2). |
| `version` | empty | hoard binary version. Empty: this action's own release, the only one it can verify. |

## Outputs

| Output | Meaning |
|---|---|
| `hits` | Number of cache hits. |
| `misses` | Number of cache misses. |
| `time-saved-ms` | Sum of the original task durations of all hits, in milliseconds. |

The outputs are set in the action's post step, at the end of the job. Later steps of the same
job cannot read them. Read them from a later job (through job `outputs`) or look at the job
summary.

## How the action behaves

- **Fork pull requests.** With `storage: r2` and missing R2 inputs (pull requests from forks get
  no secrets), the action uses `storage: github` for that run and shows a warning in the log and
  the job summary. It does not fail the job.
- **Cache mode.** When `mode` is empty, the workflow's cache mode (`ACTIONS_CACHE_MODE`) sets it:
  `read` → `read`, `none` → `off`, `write-only` → `read-write` without reads; anything else →
  `read-write`.
- **Signed artifacts with R2.** An R2 cache is shared across branches (and laptops). Set
  `TURBO_REMOTE_CACHE_SIGNATURE_KEY` from a secret and `"remoteCache": { "signature": true }` in
  `turbo.json`, so turbo signs every artifact it uploads and ignores artifacts it cannot verify.
- **Setup errors fail the step** with hoard's own message, which says how to fix it (for example
  a missing R2 lifecycle rule with `r2-lifecycle: verify`).
- **Storage errors never fail the build.** A failed read is a miss and a failed upload is
  skipped. If uploads failed, the action shows a warning (`hoard: N uploads failed`) and the job
  summary says `Upload errors: N`, so a broken cache does not look like a cold one.
- **Binary download failures never fail the build.** If the hoard binary cannot be downloaded
  (after 3 retries), the action shows a warning and the job runs without the remote cache. A
  checksum mismatch still fails the step.

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
