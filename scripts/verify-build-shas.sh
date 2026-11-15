#!/usr/bin/env bash
# Used by the publish job of .github/workflows/release.yml.
#   BUILD_OUTPUTS='<toJSON(needs.build.outputs)>' verify-build-shas.sh <dir-with-hoard-*.tar.gz>
# Each build leg exported the sha256 of its tarball as job output sha_<target with - as _>. Every
# tarball in <dir> (re-downloaded as an artifact) must match its output; a missing or empty
# output, a mismatch, or no tarball at all fails.
set -euo pipefail
dir=${1:?usage: verify-build-shas.sh <dir>}
: "${BUILD_OUTPUTS:?BUILD_OUTPUTS is not set}"
n=0 bad=0
for f in "$dir"/hoard-*.tar.gz; do
  [ -e "$f" ] || break
  n=$((n + 1))
  t=${f##*/hoard-}; t=${t%.tar.gz}
  want=$(printf '%s' "$BUILD_OUTPUTS" | jq -r --arg k "sha_${t//-/_}" '.[$k] // ""')
  got=$( (sha256sum "$f" 2>/dev/null || shasum -a 256 "$f") | cut -d' ' -f1)
  if [ -z "$want" ] || [ "$want" != "$got" ]; then
    echo "::error::${f##*/}: sha256 $got differs from the build job's output '${want:-<missing>}'"
    bad=1
  fi
done
[ "$n" -gt 0 ] || { echo "::error::no hoard-*.tar.gz in $dir"; exit 1; }
exit "$bad"
