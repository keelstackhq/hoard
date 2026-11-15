#!/usr/bin/env bash
# Shared by the detect and publish jobs of .github/workflows/release.yml.
#
#   release-complete.sh check <X.Y.Z[-pre]>   is release v<version> complete?
#   release-complete.sh newest-stable <vN>    newest published stable v<N>.*.* release tag
#
# check exits 0 only when ALL hold: the tag exists; a release for it exists with exactly the five
# hoard-<target>.tar.gz assets; and, if it is a stable non-draft release and the newest one of its
# major, the major tag v<N> points at the release commit. Exit 3: the tag does not exist (nothing
# was released yet). Exit 4: a partial release (the release is visible but wrong: assets, or a
# major tag that is missing or stale). Exit 5: the tag exists but no release is visible (a
# read-only token cannot see drafts, so a draft looks like this too). Exit 1: an API error (the
# state is unknown). The reason is printed on stdout. Needs gh, GH_TOKEN and GITHUB_REPOSITORY.
set -euo pipefail

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}
# Same five targets as TARGETS in action/src/platform.ts.
expected="hoard-aarch64-apple-darwin.tar.gz,hoard-aarch64-unknown-linux-musl.tar.gz,hoard-x86_64-apple-darwin.tar.gz,hoard-x86_64-pc-windows-msvc.tar.gz,hoard-x86_64-unknown-linux-musl.tar.gz"

# Newest stable, published (not draft, not prerelease) tag v<major>.<minor>.<patch>.
newest_stable() {
  local major=$1 list
  list=$(gh api "repos/$repo/releases" --paginate -q '.[] | select(.draft == false and .prerelease == false) | .tag_name') || return 1
  printf '%s\n' "$list" | { grep -E "^$major\.[0-9]+\.[0-9]+$" || true; } | sort -V | tail -n1
}

check() {
  local version=$1 refs rel draft pre assets major newest have want
  # Exact match: matching-refs is a prefix search (v1.0.0 also lists v1.0.0-rc.1).
  refs=$(gh api "repos/$repo/git/matching-refs/tags/v$version" --paginate -q '.[].ref') \
    || { echo "cannot list the tags of $repo"; return 1; }
  printf '%s\n' "$refs" | grep -Fxq "refs/tags/v$version" || { echo "tag v$version does not exist"; return 3; }
  local err
  err=$(mktemp)
  if ! rel=$(gh release view "v$version" --json isDraft,isPrerelease,assets \
    -q '[.isDraft, .isPrerelease, ([.assets[] | select(.state == "uploaded") | .name | select(test("^hoard-.*[.]tar[.]gz$"))] | sort | join(","))] | @tsv' 2>"$err"); then
    if grep -qi "release not found" "$err"; then
      rm -f "$err"; echo "tag v$version exists but it has no visible GitHub release (a draft is invisible to a read-only token)"; return 5
    fi
    echo "cannot read the release v$version: $(head -c 200 "$err" | tr '\n' ' ')"; rm -f "$err"; return 1
  fi
  rm -f "$err"
  IFS=$'\t' read -r draft pre assets <<< "$rel"
  [ "$assets" = "$expected" ] || { echo "release v$version has assets [$assets], expected exactly the five hoard-<target>.tar.gz"; return 4; }
  if [ "$draft" = true ] || [ "$pre" = true ]; then
    echo "release v$version is complete (draft=$draft prerelease=$pre); the major tag is not moved for it"
    return 0
  fi
  major="v${version%%.*}"
  newest=$(newest_stable "$major") || { echo "cannot list the releases of $repo"; return 1; }
  if [ "$newest" = "v$version" ]; then
    have=$(gh api "repos/$repo/commits/$major" -q .sha 2>/dev/null || true)
    want=$(gh api "repos/$repo/commits/v$version" -q .sha) || { echo "cannot resolve tag v$version"; return 1; }
    [ "$have" = "$want" ] || { echo "release v$version is the newest stable $major.* but $major does not point to it (${have:-missing})"; return 4; }
  fi
  echo "release v$version is complete"
}

case "${1:-}" in
  check) check "${2:?usage: release-complete.sh check <version>}" ;;
  newest-stable) newest_stable "${2:?usage: release-complete.sh newest-stable <vN>}" ;;
  *) echo "usage: release-complete.sh check <version> | newest-stable <vN>" >&2; exit 2 ;;
esac
