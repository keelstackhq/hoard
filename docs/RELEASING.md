# Releasing hoard

Releases are made by `.github/workflows/release.yml` (spec §10). It detects a pending version, builds the five targets, bakes
their SHA-256 checksums into `action/src/checksums.generated.ts`, rebuilds `action/dist/`, runs the
action tests against them (the release push does not trigger CI), commits
`chore(release): vX.Y.Z`, tags `vX.Y.Z`, creates the GitHub release with the five
`hoard-<target>.tar.gz` assets, verifies them, and only then moves the major tag `vX`.

## Run a release

A release starts when a version-bump PR is merged to `main`:

1. In a PR, rename `Unreleased` in `CHANGELOG.md` to the version, set `version = "X.Y.Z"` in
   `Cargo.toml`, run `cargo update -p keel-hoard` so `Cargo.lock` agrees, and get it reviewed.
2. Merge the PR to `main`. The push changes `Cargo.toml`, so `release.yml` starts. Its `detect` job
   reads the version from `Cargo.toml` and checks the tag: if `vX.Y.Z` does not exist, the build
   jobs run; if it exists, the run ends without doing anything. The merge must be done with a user
   or app token: a merge made with the workflow `GITHUB_TOKEN` does not start workflows.
3. The `publish` job (commit, tags, GitHub release) runs in the `release` environment and waits
   until the owner approves it in the run's page. Nothing is published before that approval.
4. Check: `gh release view vX.Y.Z` lists five assets, and `git show vX.Y.Z:action/src/checksums.generated.ts`
   has `VERSION = "X.Y.Z"`.
5. After the tag exists, move the previous version pins in `skills/hoard/SKILL.md`, `docs/circleci.md`,
   `README.md` and the `scripts/install.sh` examples to the new version, in a follow-up PR.

Only a change to `Cargo.toml` on `main` starts a run, and a version is released once: the `detect`
job and the `publish` job both skip a version whose tag exists, and runs queue one at a time
(`concurrency: release`). The release commit pushed by the workflow uses `GITHUB_TOKEN`, so it
does not start another run.

### Fallback: start by hand

If a release did not start (a failed run to retry, a branch other than `main`):

```bash
gh workflow run release.yml --ref main            # version of Cargo.toml
gh workflow run release.yml --ref main -f version=X.Y.Z   # must equal Cargo.toml
gh run watch "$(gh run list --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
```

Add `-f draft=true` for a draft release. A version with a suffix (`X.Y.Z-rc.1`) is published as a
pre-release. The `version` input is optional; if given, it must equal `Cargo.toml`. A push run is
never a draft.

The workflow refuses to start when `Cargo.toml` has no semver version or the input differs from it.
It pushes back to the branch it was started from, so that branch must accept pushes from
`github-actions[bot]`.

## The `release` environment

Settings → Environments → `release` must have **Required reviewers** = the owner and
**Deployment branches** = `main`. GitHub creates a missing environment on first use with no
protection, so confirm it exists before a release. With the branch rule, a release from any branch
other than `main` (for example a backport branch) needs that branch added to the rule first.

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
start the workflow by hand again (the fallback above); a push run does not repeat a version whose
tag was deleted unless `Cargo.toml` changes again.
