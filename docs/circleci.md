# hoard on CircleCI and other CI

CircleCI has no GitHub cache, so use `fs` (default, saved in CircleCI caches) or `r2`.
`circleci/hoard.yml` is a CircleCI command built on `scripts/install.sh` (or, with `sha256`, its
own pinned download), `serve --detach` and `stop`. It is not an orb: copy its contents into your
`.circleci/config.yml` as the `hoard` command (the token is random per job; set nothing else):

```sh
curl -fsSL https://raw.githubusercontent.com/keelstackhq/hoard/v0.6.0/circleci/hoard.yml
```

```yaml
version: 2.1
commands:
  hoard:             # paste the downloaded file here (description, parameters, steps), indented
    description: ...
jobs:
  build:             # every branch except main: restores the default-branch cache
    docker: [{image: cimg/node:24.0}]
    steps:
      - checkout
      - run: sudo corepack enable   # pnpm/yarn: see "Corepack" below
      - run: pnpm install --frozen-lockfile
      - hoard: {step: start, version: 0.6.0}
      - run: pnpm turbo run build test
      - hoard: {step: stop, version: 0.6.0}
  build-main:        # main only: cold build, saves the cache (see Trust)
    docker: [{image: cimg/node:24.0}]
    steps:
      - checkout
      - run: sudo corepack enable
      - run: pnpm install --frozen-lockfile
      - hoard: {step: start, version: 0.6.0, restore: false}
      - run: pnpm turbo run build test
      - hoard: {step: stop, version: 0.6.0}
workflows:
  ci:
    jobs:
      - build: {filters: {branches: {ignore: main}}}
      - build-main: {filters: {branches: {only: main}}}
```

The summary prints in the `stop` step's log (`Hits`, `Misses`); the second run should show
`>>> FULL TURBO`. Parameters of the command: `step` (`start`|`stop`), `version` (required), `storage` (`fs`, default, or `r2`),
`default-branch` (`main`), `max-size` (`1GB`), `mode` (`auto`, `read-write`, `read`), `sha256` and
`restore` (`true`; pass `false` on the default branch, see Trust).

`start` restores the cache (unless `restore: false`), installs and starts hoard, and appends
`TURBO_*` to `$BASH_ENV` (state files: a private `mktemp -d` per job). `stop` (also after a failure)
waits for the uploads, prints the summary, prunes the directory to `max-size` and saves it (`fs`,
default branch only).

## Corepack

`cimg/node` ships Node with corepack but without the `pnpm` and `yarn` shims, so `pnpm: command
not found` unless you enable it. `corepack enable` writes to `/usr/local/bin`, which the `circleci`
user cannot: use `sudo corepack enable` (as in the config above), or `npm install -g pnpm@<version>`.
Pin the package manager in `package.json` (`"packageManager"`). Put this before `pnpm install`,
and install before the hoard `start` step. npm needs none of it. (Not checked on a real CircleCI run.)

## Existing turbo setup

If the job already sets `TURBO_TOKEN`, `TURBO_API` or `TURBO_TEAM` (project or context
environment variables for a Vercel cache), `start` overwrites them in `$BASH_ENV` for later steps
of that job. Remove `--api`, `--token` and `--team` flags from the turbo command, and any
`save_cache` of `.turbo`. Jobs without the `start` step keep their old cache.

## Cache keys and cost (fs)

Only the default branch saves, under `hoard-v1-<default-branch>~<revision>`. `~` cannot occur in a
git ref, so the prefix `hoard-v1-main~` never matches a branch such as `main-x` (unverified: that
CircleCI accepts `~` in keys; the fallback is `:`, also forbidden in refs). PR and other branch
pipelines restore the newest default-branch cache and do not save; the default branch saves and,
with `restore: false`, does not restore. Each default-branch pipeline stores one cache of up to
`max-size`; CircleCI bills cache storage by plan, so check it before raising it.

## Install and failures

`version` must be `X.Y.Z` (a leading `v` is allowed). `install.sh` rejects an archive whose
SHA-256 differs from `checksums.generated.ts` at the release tag, and a file for another version.
`sha256` pins the archive hash: the command then downloads and checks the archive itself and
runs no script from the tag. Without `sha256` the trust root is the release tag (the install
script and the checksums file both come from it), which is why hull pins `sha256`: it vendors
the per-version SHA-256 for its executor (linux x64).
A failed install, or a server that does not start (`hoard serve --detach` exit 3), prints a warning
and the build runs without the cache. `start` exports `HOARD_SKIP=1` until hoard is installed, and
`stop` skips while it is 1 or unset (it never runs a stale `hoard` from `PATH` on a self-hosted
runner). A setup error (exit 1: bad R2 credentials, missing lifecycle rule, port in use) fails the
step.

## R2

`storage: r2` with a context that sets `HOARD_R2_ACCOUNT_ID`, `HOARD_R2_ACCESS_KEY_ID`,
`HOARD_R2_SECRET_ACCESS_KEY` and `HOARD_R2_BUCKET`. `mode: auto` makes branches other than
`default-branch` read-only. Give all branches a context with a read-only token, and the default
branch a second context with a read-write token, restricted with a CircleCI expression such as
`pipeline.git.branch == "main"` (this restriction syntax is unverified). With the same bucket in
the GitHub Action, GitHub and CircleCI share one cache (hull's `both` mode).

Trust: with `fs`, anyone who can run a job in the project can save a cache under the default
branch's prefix (with an edited config) and so feed artifacts to later builds: CircleCI caches are
not branch-scoped, and a same-repo PR runs its own config. So the default branch must not restore:
call the command with `restore: false` there (in a job that only runs on `main`, as hull's circleci
piece does). The cost: with `fs`, every default-branch build is cold (a full build, no hits); it
still saves, and only other branches get hits. `restore: false` protects only the default branch:
PRs still restore the newest `hoard-v1-main~` entry, whoever saved it. For warm default-branch
builds and strict setups use `r2` with read-only tokens and the restricted read-write context above.

## Plain shell

`HOARD_VERSION=0.6.0 sh scripts/install.sh`, `hoard serve --detach --storage fs --fs-root DIR`,
`eval "$(hoard env)"`, the build, and `hoard stop --summary markdown`.
