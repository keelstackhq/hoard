# hoard on CircleCI and other CI

`circleci/hoard.yml` is a CircleCI command (storage `fs` or `r2`) built on `scripts/install.sh`,
`hoard serve --detach` and `hoard stop`. Pack it as `commands/hoard.yml` of an orb, or copy it
under `commands:` as `hoard`:

```yaml
steps:
  - checkout
  - hoard: {step: start, version: 0.4.0}
  - run: pnpm turbo run build test
  - hoard: {step: stop, version: 0.4.0}
```

`start` restores the cache directory, installs and starts hoard, and appends `TURBO_API`,
`TURBO_TOKEN` and `TURBO_TEAM` to `$BASH_ENV`. `stop` (also after a failure) waits for the
uploads, prints the markdown summary, prunes the directory to 2 GB and saves it (`fs` only). Cache keys:
`hoard-v1-<branch>-<revision>`, restored from the same branch, then `main`. The install script
(`HOARD_VERSION=X.Y.Z`, Linux) installs into `$HOARD_INSTALL_DIR` (default `~/.local/bin`) and rejects an
archive whose SHA-256 differs from `checksums.generated.ts` at the release tag.

## R2

`storage: r2` with a context that sets `HOARD_R2_ACCOUNT_ID`, `HOARD_R2_ACCESS_KEY_ID`,
`HOARD_R2_SECRET_ACCESS_KEY` and `HOARD_R2_BUCKET`. With the same bucket in the GitHub Action
(`storage: r2`), GitHub and CircleCI share one cache (hull's `both` mode). A failed download
logs a warning and the build runs without the cache.

## Plain shell

```sh
HOARD_VERSION=0.4.0 sh scripts/install.sh
hoard serve --detach --storage fs --fs-root ~/.cache/hoard
eval "$(hoard env)"
pnpm turbo run build
hoard stop --summary markdown
```
