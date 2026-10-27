# hoard on CircleCI and other CI

`circleci/hoard.yml` is a CircleCI command (`fs` or `r2` storage) built on `scripts/install.sh`
(or, with `sha256`, its own pinned download), `serve --detach` and `stop`. Copy it under
`commands:` as `hoard` (or pack it as `commands/hoard.yml`):

```yaml
steps: [checkout, hoard: {step: start, version: 0.4.0}, run: pnpm turbo run build test,
        hoard: {step: stop, version: 0.4.0}]
```

Parameters: `step` (`start`|`stop`), `version` (required), `storage` (`fs`, default, or `r2`),
`default-branch` (`main`), `max-size` (`1GB`), `mode` (`auto`, `read-write`, `read`) and `sha256`.

`start` restores the cache, installs and starts hoard, and appends `TURBO_*` to `$BASH_ENV` (state
files: a private `mktemp -d` per job). `stop` (also after a failure) waits for the uploads, prints
the summary, prunes the directory to `max-size` and saves it (`fs`, default branch only).

## Cache keys and cost (fs)

Only the default branch saves, under `hoard-v1-<default-branch>~<revision>`. `~` cannot occur in a
git ref, so the prefix `hoard-v1-main~` never matches a branch such as `main-x` (unverified: that
CircleCI accepts `~` in keys; the fallback is `:`, also forbidden in refs). Every pipeline restores
the newest default-branch cache; PR pipelines do not save. Each default-branch pipeline stores one
cache of up to `max-size`; CircleCI bills cache storage by plan, so check it before raising it.

## Install and failures

`version` must be `X.Y.Z` (a leading `v` is allowed). `install.sh` rejects an archive whose
SHA-256 differs from `checksums.generated.ts` at the release tag, and a file for another version.
`sha256` pins the archive hash: the command then downloads and checks the archive itself and
runs no script from the tag. Without `sha256` the trust root is the release tag (the install
script and the checksums file both come from it), which is why hull pins `sha256`: it vendors
the per-version SHA-256 for its executor (linux x64).
A failed install, or a server that does not start (`hoard serve --detach` exit 3), prints a warning
and the build runs without the cache. `start` exports `HOARD_SKIP=1` until hoard is installed, so
`stop` then skips (it never runs a stale `hoard` from `PATH` on a self-hosted runner). A setup error
(exit 1: bad R2 credentials, missing lifecycle rule, port in use) fails the step.

## R2

`storage: r2` with a context that sets `HOARD_R2_ACCOUNT_ID`, `HOARD_R2_ACCESS_KEY_ID`,
`HOARD_R2_SECRET_ACCESS_KEY` and `HOARD_R2_BUCKET`. `mode: auto` makes branches other than
`default-branch` read-only. Give all branches a context with a read-only token, and the default
branch a second context with a read-write token, restricted with a CircleCI expression such as
`pipeline.git.branch == "main"` (this restriction syntax is unverified). With the same bucket in
the GitHub Action, GitHub and CircleCI share one cache (hull's `both` mode).

Trust: with `fs`, anyone who can run a job in the project can save a cache under the default
branch's prefix (with an edited config) and so feed artifacts to later builds. For strict setups
use `r2` with read-only tokens and the restricted read-write context above.

## Plain shell

`HOARD_VERSION=0.4.0 sh scripts/install.sh`, `hoard serve --detach --storage fs --fs-root DIR`,
`eval "$(hoard env)"`, the build, and `hoard stop --summary markdown`.
