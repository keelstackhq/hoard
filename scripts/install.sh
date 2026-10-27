#!/bin/sh
# Installs a hoard release (Linux) after a SHA-256 check. HOARD_VERSION=0.4.0 sh install.sh
# HOARD_SHA256 (64 lowercase hex) pins the archive hash for this machine's target and skips the sums file.
set -eu
# One function, called on the last line: a truncated `curl | sh` download runs nothing.
main() {
V="${HOARD_VERSION:-}"
V="${V#v}"
[ -n "$V" ] || { echo "HOARD_VERSION is required, e.g. HOARD_VERSION=0.4.0" >&2; exit 1; }
case "$V" in *[!0-9A-Za-z.-]*) echo "HOARD_VERSION has characters other than [0-9A-Za-z.-]" >&2; exit 1 ;; esac
printf '%s\n' "$V" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' ||
  { echo "HOARD_VERSION '$V' is not X.Y.Z (optionally -prerelease)" >&2; exit 1; }
pin="${HOARD_SHA256:-}"
[ -z "$pin" ] || printf '%s\n' "$pin" | grep -Eq '^[0-9a-f]{64}$' ||
  { echo "HOARD_SHA256 must be 64 lowercase hex characters" >&2; exit 1; }
[ "$(uname -s)" = Linux ] || { echo "hoard install.sh supports Linux only" >&2; exit 1; }
case "$(uname -m)" in
  x86_64 | amd64) target=x86_64-unknown-linux-musl ;;
  aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac
dir="${HOARD_INSTALL_DIR:-${HOME:?set HOME or HOARD_INSTALL_DIR}/.local/bin}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
if [ -n "$pin" ]; then
  want="$pin"
else
  curl --proto '=https' --proto-redir '=https' -fsSL --retry 3 -o "$tmp/sums.ts" \
    "https://raw.githubusercontent.com/keelstackhq/hoard/v$V/action/src/checksums.generated.ts"
  grep -Fxq "export const VERSION = \"$V\";" "$tmp/sums.ts" ||
    { echo "checksums file at v$V is for another version" >&2; exit 1; }
  want="$(sed -n "s/.*\"$target\": *\"\([0-9a-f]\{64\}\)\".*/\1/p" "$tmp/sums.ts")"
  [ -n "$want" ] || { echo "no checksum for $target in hoard v$V" >&2; exit 1; }
fi
curl --proto '=https' --proto-redir '=https' -fsSL --retry 3 -o "$tmp/hoard.tar.gz" \
  "https://github.com/keelstackhq/hoard/releases/download/v$V/hoard-$target.tar.gz"
got="$( (sha256sum "$tmp/hoard.tar.gz" 2>/dev/null || shasum -a 256 "$tmp/hoard.tar.gz") | cut -d' ' -f1)"
[ "$got" = "$want" ] || { echo "checksum mismatch for hoard-$target.tar.gz" >&2; exit 1; }
tar -xzf "$tmp/hoard.tar.gz" -C "$tmp" hoard
mkdir -p "$dir"
install -m 0755 "$tmp/hoard" "$dir/hoard"
echo "installed hoard $V to $dir/hoard"
}
main "$@"
