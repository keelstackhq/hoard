# Releasing hoard

Releases are made by `.github/workflows/release.yml` (spec §10). It builds the five targets, bakes
their SHA-256 checksums into `action/src/checksums.generated.ts`, rebuilds `action/dist/`, commits
`chore(release): vX.Y.Z`, tags `vX.Y.Z`, creates the GitHub release with the five
`hoard-<target>.tar.gz` assets, verifies them, and only then moves the major tag `vX`.

## Run a release

1. On the branch to release from (normally `main`), set `version = "X.Y.Z"` in `Cargo.toml`, run
   `cargo update -p keel-hoard` so `Cargo.lock` agrees, commit and push.
2. Start the workflow:

   ```bash
   gh workflow run release.yml --ref main -f version=X.Y.Z
   gh run watch "$(gh run list --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
   ```

   Add `-f draft=true` for a draft release. A version with a suffix (`X.Y.Z-rc.1`) is published as
   a pre-release.
3. Check: `gh release view vX.Y.Z` lists five assets, and `git show vX.Y.Z:action/src/checksums.generated.ts`
   has `VERSION = "X.Y.Z"`.

The workflow refuses to start when the version is not semver, does not equal `Cargo.toml`, or the
tag `vX.Y.Z` already exists. It pushes back to the branch it was started from, so that branch must
accept pushes from `github-actions[bot]`.

## The major tag `vX`

Users pin the major tag (`uses: <owner>/hoard@v0` while hoard is 0.x; a 0.x release moves `v0`,
1.0 and later move `v1`, `v2`, …). The workflow moves `vX` only for a stable,
non-draft release, and only forward: if a newer stable `vX.*` tag exists (a backport release), `vX`
stays where it is.

After a **draft** release, publish the draft by hand and then move the major tag yourself:

```bash
gh release edit vX.Y.Z --draft=false
git fetch --tags
git tag -f vX vX.Y.Z^{commit} && git push -f origin vX
```

Do this only when `vX.Y.Z` is the newest stable `vX.*` release.

## Cleanup after a partial failure

If the workflow fails **before** "Commit and tag", nothing was published: fix and re-run.

If it fails **after** pushing the commit and tag (release missing, or "Verify release assets"
failed), `vX` was not moved, so users are unaffected. Remove the partial release, then re-run:

```bash
gh release delete vX.Y.Z --yes || true          # if the release was created
git push origin :refs/tags/vX.Y.Z               # delete the remote tag
git tag -d vX.Y.Z 2>/dev/null || true
git pull && git revert --no-edit <sha of "chore(release): vX.Y.Z"> && git push
```

Reverting restores the previous `checksums.generated.ts` and `action/dist/`. Then
start the workflow again with the same version.
