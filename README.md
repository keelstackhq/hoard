# hoard

A Turborepo (and Vite+) remote cache for GitHub Actions, in one line of YAML. No account, no
server to host. Artifacts go to the GitHub Actions cache (default, zero config) or to your own
Cloudflare R2 bucket. Part of [keelstackhq](https://github.com/keelstackhq): tools for
bootstrapping projects.

> hoard is 0.x: pin `@v0`. 1.0 (and the `@v1` tag) waits for artifact packing, which keeps large
> monorepos under the GitHub cache upload rate limit.

## Quick start

Add one step before your Turborepo commands. Nothing else to configure:

```yaml
# .github/workflows/ci.yml
name: CI
on:
  push:
    branches: [main]
  pull_request:

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: pnpm/action-setup@v4
      - uses: actions/setup-node@v5
        with: { node-version: 24, cache: pnpm }
      - run: pnpm install --frozen-lockfile
      - uses: keelstackhq/hoard@v0        # starts the cache, exports TURBO_API/TURBO_TOKEN/TURBO_TEAM
      - run: pnpm turbo run build test lint
```

The first run fills the cache. Later runs replay every unchanged task from it (`>>> FULL TURBO`
when nothing changed). npm, yarn and bun work the same way: only the hoard step matters.

What the step does:

1. Downloads the `hoard` binary of this action's release and checks its SHA-256.
2. Starts it on `127.0.0.1` with a random token. It serves the Turborepo remote-cache API.
3. Exports `TURBO_API`, `TURBO_TOKEN` and `TURBO_TEAM`, so every later `turbo` command in the job
   uses it. You do not change `turbo.json` or your scripts.
4. At the end of the job, waits for the uploads, stops the server and writes a job summary:

   ```
   ### hoard
   | Hits | Misses | Uploaded | Downloaded | Time saved |
   |---|---|---|---|---|
   | 42 | 3 | 12.4 MB | 210.0 MB | 6m 12s |
   Storage: github
   ```

The cache never fails your build. If storage is down or slow, a read is a miss and an upload is
skipped (with a warning, so a broken cache does not look like a cold one).

Pin a release tag (`@v0` or `@v0.1.0`), not `@main` or a commit: each action release can only
run the hoard binary of that same release.

## Choose a storage

| `storage` | Setup | Good for | Limits |
|---|---|---|---|
| `github` (default) | none | most repositories | GitHub's cache rules: 10 GB per repository (oldest entries evicted), about 200 uploads per minute, branch scoping (below). |
| `r2` | an R2 bucket and four secrets | large monorepos; one cache for all branches, repositories and laptops | your bucket; hoard keeps it bounded (lifecycle rule + size cap). R2 has no egress fees. |
| `fs` | none | tests, or a self-hosted runner with a persistent disk | the runner's disk |

**Branch scoping (`github`).** A GitHub cache entry is visible to the branch that wrote it and to
branches created from it. So a pull request reads what its base branch (and the default branch)
wrote, but not what other pull requests wrote. Run the workflow on `push` to `main`, as in the
quick start, so pull requests start warm.

## Use Cloudflare R2

### 1. Create the bucket and a token

1. In the Cloudflare dashboard, open **R2 Object Storage** and create a bucket, for example
   `my-team-cache`. Note your **Account ID** (shown on the R2 overview page).
2. Under **R2 → Manage API tokens**, create a token for that bucket:
   - **Admin Read & Write** with the default `r2-lifecycle: auto`, so hoard can add the bucket's
     expiry rule on the first run.
   - If you would rather not give CI admin rights: use **Object Read & Write**, set
     `r2-lifecycle: manual` (or `verify`, which also needs Admin Read), and let a bucket admin add
     the rule once. `hoard doctor` prints the exact command.
3. Copy the **Access Key ID** and **Secret Access Key**.

### 2. Add four repository secrets

**Settings → Secrets and variables → Actions → New repository secret**:

| Secret | Value |
|---|---|
| `R2_ACCOUNT_ID` | your Cloudflare Account ID |
| `R2_ACCESS_KEY_ID` | the token's Access Key ID |
| `R2_SECRET_ACCESS_KEY` | the token's Secret Access Key |
| `R2_BUCKET` | the bucket name |

### 3. Use them in the workflow

```yaml
- uses: keelstackhq/hoard@v0
  with:
    storage: r2
    r2-account-id: ${{ secrets.R2_ACCOUNT_ID }}
    r2-access-key-id: ${{ secrets.R2_ACCESS_KEY_ID }}
    r2-secret-access-key: ${{ secrets.R2_SECRET_ACCESS_KEY }}
    r2-bucket: ${{ secrets.R2_BUCKET }}
- run: pnpm turbo run build test lint
```

The job summary then shows `Storage: r2 (my-team-cache/cache/) · Lifecycle: 7 days ✓`.

### How the bucket stays bounded

- **Lifecycle rule.** Objects under `r2-prefix` are deleted `max-age-days` (default 7) after their
  last write, and unfinished uploads after 1 day. Cloudflare enforces it, even if hoard never runs
  again. hoard only adds or updates its own rule; other rules on the bucket are kept.
- **Size cap.** At the end of a job, at most once a day, hoard deletes the oldest objects until
  the prefix fits in `max-size` (default 5 GB). Do not nest prefixes in one bucket (`cache/` and
  `cache/team/`): the outer prefix's size cap would delete the inner one's objects.
- **Optional `refresh-on-hit: true`.** Renews objects that are still being read, so the lifecycle
  rule does not expire artifacts in use.

### Sign artifacts

An R2 cache is shared by every branch, including pull requests from people with write access.
Let Turborepo sign and verify artifacts: add a secret `TURBO_REMOTE_CACHE_SIGNATURE_KEY` (any long
random string), expose it to the job, and enable signatures in `turbo.json`:

```yaml
env:
  TURBO_REMOTE_CACHE_SIGNATURE_KEY: ${{ secrets.TURBO_REMOTE_CACHE_SIGNATURE_KEY }}
```

```json
{ "remoteCache": { "signature": true } }
```

Turborepo then ignores any artifact it cannot verify.

### Pull requests from forks

Forks get no secrets. With `storage: r2` and missing R2 inputs, the action uses `storage: github`
for that run and shows a warning in the log and the job summary. It does not fail the job.

### Use the same cache on a laptop

Use a read-only R2 token (Object Read) and `--mode read`, so the laptop never writes artifacts or
the lifecycle rule. Download `hoard` from the
[releases page](https://github.com/keelstackhq/hoard/releases), then:

```bash
export HOARD_R2_ACCOUNT_ID=... HOARD_R2_ACCESS_KEY_ID=... HOARD_R2_SECRET_ACCESS_KEY=...
hoard serve --storage r2 --r2-bucket my-team-cache --mode read &
eval "$(hoard env)"            # exports TURBO_API, TURBO_TOKEN, TURBO_TEAM
pnpm turbo run build
```

## Vite+

When the root `package.json` depends on `vite-plus`, the action also restores Vite Task's cache
directory (`node_modules/.vite/task-cache`, or `VITE_CACHE_PATH` when it is absolute) at the start
of the job and saves it at the end. It works with every `storage`. This replaces the
`actions/cache` steps from the Vite+ docs (Vite+ remote caching is not released upstream yet).

Put the hoard step **after** the dependency install: an install can recreate `node_modules` and
drop the restored cache.

```yaml
- uses: actions/checkout@v5
- uses: pnpm/action-setup@v4
- uses: actions/setup-node@v5
  with: { node-version: 24, cache: pnpm }
- run: pnpm install --frozen-lockfile
- uses: keelstackhq/hoard@v0     # after the install: restores node_modules/.vite/task-cache
- run: pnpm exec vp run build  # post step saves the task cache
```

Details:

- The archive restored is the newest one saved on the same OS and architecture. Matrix legs save
  one archive each, so legs that run different tasks share only one leg's task cache.
- Nothing is uploaded when the cache did not change since the restore. On r2 and fs, the archive
  it replaces is deleted.
- A cache larger than `vp-max-size` (default 2 GB) is not saved; run `vp cache clean`.
- Only the checkout root (`GITHUB_WORKSPACE`) is looked at; a repository checked out with
  `actions/checkout`'s `path:` is not.
- An absolute `VITE_CACHE_PATH` is used only when it is set at job or workflow level, where the
  hoard step sees it.
- Restore and save problems are warnings and never fail the build.

## Requirements

- **Runners:** GitHub-hosted or self-hosted, on Linux x64 and arm64, macOS x64 and arm64
  (Apple silicon), and Windows x64. The runner must be able to download from `github.com`
  release assets (and reach R2 with `storage: r2`).
- **Not supported:** GitHub Enterprise Server (its cache service is the older v1 API).
- **Turborepo:** any version that reads `TURBO_API` / `TURBO_TOKEN` / `TURBO_TEAM` (tested with
  2.11). Do not set `"remoteCache": { "enabled": false }` in `turbo.json`.
- **Permissions:** none for `storage: github`; the job's built-in cache access is enough.

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
| `refresh-on-hit` | `false` | R2 only: reset the age of objects older than `max-age-days / 2` when a cache hit reads them (once per object per run), so artifacts in use never expire. |
| `vp-max-size` | `2GB` | Vite+ only: skip saving the task cache when it is larger than this (then run `vp cache clean`). |
| `version` | empty | hoard binary version. Empty: this action's own release, the only one it can verify. |

**Cache mode.** When `mode` is empty, the workflow's cache mode (`ACTIONS_CACHE_MODE`) sets it:
`read` → `read`, `none` → `off`, `write-only` → `read-write` without reads; anything else →
`read-write`. Use `mode: read` for jobs that should use the cache but never write to it.

## Outputs

| Output | Meaning |
|---|---|
| `hits` | Number of cache hits. |
| `misses` | Number of cache misses. |
| `time-saved-ms` | Sum of the original task durations of all hits, in milliseconds. |

The outputs are set in the action's post step, at the end of the job, so later steps of the same
job cannot read them. Read them from a later job:

```yaml
jobs:
  build:
    runs-on: ubuntu-latest
    outputs:
      hits: ${{ steps.hoard.outputs.hits }}
    steps:
      # … checkout, install …
      - uses: keelstackhq/hoard@v0
        id: hoard
      - run: pnpm turbo run build
  report:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - run: echo "cache hits: ${{ needs.build.outputs.hits }}"
```

## Troubleshooting

| You see | What it means, what to do |
|---|---|
| Every task is a cache miss on every run | Look at the job summary. `Uploaded 0` with `Upload errors: N`: storage refused the writes (see the warning in the log). `Misses` only on the first run of a branch is normal with `storage: github` (branch scoping); run the workflow on `push` to `main`. Also check that `turbo.json` does not disable `remoteCache` and that the task's `outputs` are declared. |
| `hoard: N uploads failed` | The storage refused N uploads. The build still passed; the next run misses those tasks. The log line before it has the cause (credentials, bucket name, network). |
| `GitHub cache throttled N uploads` | GitHub's cache rate limit (about 200 uploads per minute per repository). Reads continue; the skipped tasks are uploaded on a later run. A busy monorepo is a good fit for `storage: r2`. |
| `hoard binary could not be downloaded …; continuing without the cache` | The runner could not reach the release asset in 90 s. The job runs without the remote cache. Check the runner's network or proxy. |
| The step fails with a setup error | Bad input or missing R2 lifecycle rule with `r2-lifecycle: verify`. The message says how to fix it. These are the only cache errors that fail a job. |
| `storage: r2` but the summary says `Storage: github` | The R2 inputs were empty (a fork pull request, or a secret name typo). See the warning in the summary. |
| `unsupported platform …` | The runner is not one of the platforms under [Requirements](#requirements). |

To check an R2 setup by hand, run `hoard doctor --storage r2 --r2-account-id <id> --r2-bucket <bucket>`
with the `HOARD_R2_*` variables set. It checks the credentials, bucket read and write and the
lifecycle rule, and prints the fixes.

## Commands

The action runs the `hoard` binary for you. You can also run it yourself:

| Command | What it does |
|---|---|
| `hoard serve` | Runs the cache server on 127.0.0.1 until `POST /_hoard/shutdown`, SIGTERM or Ctrl-C. |
| `hoard env` | Prints the `export TURBO_API/TURBO_TOKEN/TURBO_TEAM` lines for a running server. |
| `hoard prune [--force]` | Deletes the oldest objects until the R2 prefix fits in `max-size` (at most once per 24 h without `--force`). |
| `hoard doctor` | Checks the storage credentials, bucket read/write and the R2 lifecycle rule, and prints the fixes. |
| `hoard vp restore` / `hoard vp save` | Restores or saves the Vite+ task-cache directory (`--workspace`, default `.`); prints one line and exits 0 unless the setup is wrong. |

R2 token permissions for the binary: with the default `--r2-lifecycle auto`, a read-write server
needs Admin Read & Write, because it sets the bucket's lifecycle rule. `--r2-lifecycle verify`
only reads the rule (Admin Read, plus object read and write); with `--r2-lifecycle manual`,
object read and write is enough. In both cases a bucket admin adds the rule (`hoard doctor`
prints the command). With `--mode read`, object read is enough.

## Settings

Every setting is a flag (`--max-size 5GB`) and an environment variable (`HOARD_MAX_SIZE=5GB`).
The full list with defaults is the table in
[spec §4.2](docs/superpowers/specs/2026-10-14-hoard-design.md#42-configuration).

`--write-behind auto|on|off` (`HOARD_WRITE_BEHIND`, default `auto`: on for `storage=github`,
off for `r2` and `fs`) answers each PUT once the artifact is spooled to disk and uploads it in
the background; shutdown waits up to 120 s for the queue.

`--refresh-on-hit` (`HOARD_REFRESH_ON_HIT=true`; R2 only) copies an object onto itself in the
background when a cache hit reads it and it is older than `max-age-days / 2`, so artifacts in
use do not expire. It is verified on MinIO; see
[spec §7.3](docs/superpowers/specs/2026-10-14-hoard-design.md#73-layer-3--refresh-on-hit-optional)
for the one manual check against real R2.

## Security

- The server listens on `127.0.0.1` only, and every cache request needs the per-job random token.
- R2 keys and the token reach the binary through environment variables, never the command line,
  and are never logged. The token is masked in the workflow log.
- The binary is checked against SHA-256 checksums built into the action's release.

## License

[MIT](LICENSE).
