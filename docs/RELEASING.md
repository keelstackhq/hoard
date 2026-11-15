# Releasing hoard

Releases are made by `.github/workflows/release.yml` (spec §10). It detects a pending version, builds the five targets, bakes
their SHA-256 checksums into `action/src/checksums.generated.ts`, rebuilds `action/dist/`, runs the
action tests against them, commits `chore(release): vX.Y.Z` on the merged commit, tags that commit
`vX.Y.Z` and pushes **only the tag** (never `main`), creates the GitHub release with the five
`hoard-<target>.tar.gz` assets, verifies them, and only then moves the major tag `vX`.

## Run a release

A release starts when a version-bump PR is merged to `main`:

1. In a PR, rename `Unreleased` in `CHANGELOG.md` to the version, set `version = "X.Y.Z"` in
   `Cargo.toml`, run `cargo update -p keel-hoard` so `Cargo.lock` agrees, and get it reviewed.
2. Merge the PR to `main`. The push changes `Cargo.toml`, so `release.yml` starts. Its `detect` job
   reads the version from `Cargo.toml` and checks the tag: if `vX.Y.Z` does not exist, the build
   jobs run; if it exists, the run ends without doing anything. The merge must be done with a user
   or app token: a merge made with the workflow `GITHUB_TOKEN` does not start workflows.
3. The `publish` job (release commit, tags, GitHub release) runs in the `release` environment and waits
   until the owner approves it in the run's page. Nothing is published before that approval.
4. Check: `gh release view vX.Y.Z` lists five assets, and `git show vX.Y.Z:action/src/checksums.generated.ts`
   has `VERSION = "X.Y.Z"`.
5. After the tag exists, move the previous version pins in `skills/hoard/SKILL.md`, `docs/circleci.md`,
   `README.md` and the `scripts/install.sh` examples to the new version, in a follow-up PR.

The release commit (baked checksums and rebuilt `action/dist/`) is reachable only from the tag.
`main` keeps the previous checksums and `dist/`, which is fine: users consume `@vX.Y.Z` or `@vX`, and
`main`'s own CI and self-test use the checkout's `dist/` and a published binary of the baked
version. So merging to `main` while a release waits for approval is safe, and the workflow needs no
branch-protection bypass. Any later change to `Cargo.toml` (even a dependency bump) also starts a
run for a version that has no tag yet, for example after a failed release. The owner approval
still applies.

Only a change to `Cargo.toml` on `main` starts a run, and a version is released once: the `detect`
job and the `publish` job both skip a version whose tag **and** release exist, and runs queue
one at a time (`concurrency: release`). A tag without a release is a partial failure: `publish`
fails with an error (see "Cleanup after a partial failure"), it never ends green. The workflow
pushes tags only, which start no run.

### Fallback: start by hand

If a release did not start (for example after the cleanup below):

```bash
gh workflow run release.yml --ref main            # version of Cargo.toml
gh workflow run release.yml --ref main -f version=X.Y.Z   # must equal Cargo.toml
gh run watch "$(gh run list --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
```

Add `-f draft=true` for a draft release. A version with a suffix (`X.Y.Z-rc.1`) is published as a
pre-release. The `version` input is optional; if given, it must equal `Cargo.toml`. A push run is
never a draft.

The workflow refuses to start when `Cargo.toml` has no semver version or the input differs from it.
The `publish` job needs only to push tags (`contents: write`).

## The `release` environment

Settings → Environments → `release` must have **Required reviewers** = the owner and
**Deployment branches** = `main`. GitHub creates a missing environment on first use with no
protection, so confirm it exists before a release.

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

If the workflow fails **before** "Commit and tag (tag only)", nothing was published: fix and re-run.

If it fails **after** pushing the tag (release missing, or "Verify release assets" failed), `vX` was
not moved, so users are unaffected, and `main` was never touched. A re-run stops with an error
until you remove the partial release and its tag:

```bash
gh release delete vX.Y.Z --yes --cleanup-tag || git push origin :refs/tags/vX.Y.Z
```

(`--cleanup-tag` also deletes the tag; if no release exists, the second command deletes the tag.)
Then start the workflow by hand again (the fallback above); a push run does not repeat a version
unless `Cargo.toml` changes again.
