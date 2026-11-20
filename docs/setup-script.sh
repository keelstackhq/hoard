#!/usr/bin/env bash
# Per-repo setup script of this repo's Claude Code cloud environment (spec D23). Written by the
# bosun splice channel. Not pasted by hand: the bosun bootstrap (`bosun setup-snippet`, pasted once
# into the environment) runs it as root before a session, from inside the checkout, possibly on a
# warm container. Pins move only by reviewed PRs.
set -euo pipefail

# Pinned tool versions; the bosun channel moves them with its own versions (a reviewed PR).
NODE_MAJOR=24
PNPM_VERSION=12.15.0
SPLICE_VERSION=0.6.0
# npm's dist.integrity (sha512) of each tool's launcher and linux binary packages, from
# `npm view @keelstackhq/<package>@<version> dist.integrity`; move them with the version.
SPLICE_SHA512=dMLEVxDdl6qCCLZhCIb0qV+f5BZ1r5rRTglMtYf9cc4uRW81eWSSz1n5fuB1RlotWgMGbBbzSvyX1O7PxDHHSA==
SPLICE_LINUX_X64_SHA512=YmeezoD9hiz3TSvCk2k1VokW7JlQmvqGuSaBBfQGgOJ2DESdWufEv2jQ94xGZUYzTWpBb2TM9euoOTi51bMCvg==
SPLICE_LINUX_ARM64_SHA512=1tScSJRc6HhnKqFoDWpz0nMewjR1j3akWw3sthqFHvnzaXAL7W+BMVDmyJ2c9lPa4QF/HBEwFfbe1JmtJx/Frw==
HOIST_VERSION=0.5.1
HOIST_SHA512=vTS2qXva2OCDTuYbkAntSaN6Rf2w70f3inxOzYCHZkrvqp9bgdGAN4OX8ZMQbcg9VabUiPR3aeXbAjFqKMgmcg==
HOIST_LINUX_X64_SHA512=4bTuPUbSWvLVvMU4DgMZcKfAhqNhQN5Iy3I0W0eaUtpk9UWKPNvLCbdKp5LWxpeoMFqYArgkVq3/hKJhZj+QzQ==
HOIST_LINUX_ARM64_SHA512=11eq9L45RUw1kqgSGqMzloesUtr/ISc+/7HqRrTfpjNt9dPUlJkZMiTccgLaq3GnPt3kHhEOwvS98o2iV1uB6g==
BERTH_VERSION=0.2.1
BERTH_SHA512=8PeL8d6meSYVuF+MNmK6CyAfZn+pmDIkLxwAcZIgNzhRoGWoBBKA8pTvG/NAYTHRhF8Ym+3OtCD+N/96N+u3JA==
BERTH_LINUX_X64_SHA512=vwYKi7HGKAs4NIAWf5V228ci6aaqa+1nPvk/FqomCj61on2ouzGJzSFVZPtSv88U3/LW+I3Kmo5gAtuIIRAF7Q==
BERTH_LINUX_ARM64_SHA512=Y8nO2tpk7f9Pujvsxq42vaO9GjGF0n7jFv4MKN7F8kP8ofACPX7obGESfv+qNYik17X3WXwHgXS15WKorOlAdQ==
# cargo-deny's prebuilt binary as npm packages (@keelstackhq/cargo-deny, its launcher, and
# cargo-deny-linux-<arch>), pinned like the tools above. Empty until their first publish: then the
# Rust step builds it with `cargo install --locked cargo-deny` (minutes) instead.
CARGO_DENY_VERSION=""
CARGO_DENY_SHA512=""
CARGO_DENY_LINUX_X64_SHA512=""
CARGO_DENY_LINUX_ARM64_SHA512=""
# Prebuilt cargo tools (cargo install builds them from source: minutes). Release tarballs from
# github.com (served by release-assets.githubusercontent.com), each pinned by the sha256 of the
# tarball for this machine; a mismatch or a failed download falls back to `cargo install --locked`.
# Move a pin with its checksums: curl -fsSL <asset url> | sha256sum.
NEXTEST_VERSION=0.9.152
NEXTEST_LINUX_X64_SHA256=d039b43c3e0958441b6ad0a39c8884d018b3d2e8fb9928f209074b1100b5e17a
NEXTEST_LINUX_ARM64_SHA256=ca1f81a5b86533eb45ab97e0064aa88e925e53aab9a698ca6331a7cad1f85f07
CARGO_DENY_BIN_VERSION=0.20.2
CARGO_DENY_BIN_LINUX_X64_SHA256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f
CARGO_DENY_BIN_LINUX_ARM64_SHA256=995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d
# Claude Code plugins. `keelstack` is bosun's marketplace (.claude-plugin/marketplace.json on this
# branch of keelstackhq/bosun, owner-reviewed); it pins each plugin source by sha. A marketplace ref
# must be a branch or tag (`#<sha>` fails to clone). Move the plugin pins with marketplace.json;
# `bosun doctor` shows drift red.
KEELSTACK_MARKETPLACE_REF=main
SUPERPOWERS_SHA=22f0b463e7a5e579904dd6211a520321035526f7
CAVEMAN_SHA=a9cc1f7b82598852e8c6b8e599dde6ac2491b26e
# The bosun release this repo pins in .bosun/version.
BOSUN_VERSION=0.21.0
# Stacks of this checkout: Rust steps run only with a Cargo.toml or a toolchain file, pnpm only
# with a pnpm-lock.yaml, a pnpm-workspace.yaml or a package.json `packageManager` of pnpm.
HAS_RUST=""
for f in "./Cargo.toml" Cargo.toml "./rust-toolchain.toml" "./rust-toolchain" rust-toolchain.toml rust-toolchain; do
  if [ -f "$f" ]; then HAS_RUST=1; break; fi
done
HAS_PNPM=""
for d in "." .; do
  if [ -f "$d/pnpm-lock.yaml" ] || [ -f "$d/pnpm-workspace.yaml" ] || grep -Eq '"packageManager"[[:space:]]*:[[:space:]]*"pnpm@' "$d/package.json" 2>/dev/null; then HAS_PNPM=1; break; fi
done

# Rust is not pinned here: the version is the `channel` of the repo's rust-toolchain.toml (or the
# legacy rust-toolchain file), so gates and sessions share a toolchain. Without one a Rust
# checkout warns, skips the pinned toolchain and still installs cargo-deny; a checkout with neither
# Cargo.toml nor a toolchain file gets no Rust step. `bash docs/setup-script.sh --rust-version`
# prints the version and installs nothing.
RUST_VERSION=""
for f in "./rust-toolchain.toml" "./rust-toolchain" rust-toolchain.toml rust-toolchain; do
  [ -f "$f" ] || continue
  v="$(sed -nE "s/^[[:space:]]*channel[[:space:]]*=[[:space:]]*[\"']([^\"']*)[\"'].*/\1/p" "$f" | head -n1)"
  # The legacy `rust-toolchain` file may hold just the channel on its first line.
  if [ -z "$v" ] && [ "${f##*/}" = rust-toolchain ]; then
    v="$(head -n1 "$f" | tr -d '[:space:]')"
    printf '%s' "$v" | grep -Eqx '[A-Za-z0-9._-]+' || v=""
  fi
  if [ -n "$v" ]; then RUST_VERSION="$v"; break; fi
done
# The channel reaches rustup as root: only a toolchain name passes, anything else is skipped like a
# missing file (printf %q shows the rejected value safely).
if [ -n "$RUST_VERSION" ] && ! [[ "$RUST_VERSION" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
  printf 'setup-script: invalid channel %q in rust-toolchain.toml; skipping the Rust toolchain install (cargo-deny still installs with the default toolchain)\n' "$RUST_VERSION" >&2
  RUST_VERSION=""
  if [ "${1:-}" = --rust-version ]; then exit 1; fi
elif [ -z "$RUST_VERSION" ]; then
  # Never fail here: a failing setup script blocks every session of the environment.
  if [ -n "$HAS_RUST" ] || [ "${1:-}" = --rust-version ]; then
    echo "setup-script: no rust-toolchain.toml with a channel in . or $PWD; skipping the Rust toolchain install (cargo-deny still installs with the default toolchain), rustup installs the checkout's pinned toolchain on first cargo use" >&2
  fi
  if [ "${1:-}" = --rust-version ]; then exit 1; fi
fi
if [ "${1:-}" = --rust-version ]; then echo "$RUST_VERSION"; exit 0; fi

# Node 24 from NodeSource, unless it is already there (the keelstackhq tools ship through npm).
if ! node --version 2>/dev/null | grep -q "^v${NODE_MAJOR}\."; then
  curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash -
  apt-get install -y nodejs
fi

# The image may put an older Node (/opt/node22/bin) before NodeSource's /usr/bin in PATH. Make Node 24 the
# `node` that resolves first, whatever the PATH order: link it into a directory that later shells put
# first (/etc/profile.d), and replace the older `node` that resolves today. Never fails: it warns.
NODE_BIN_DIR="${SETUP_NODE_BIN_DIR:-/usr/bin}"
NODE_LINK_DIR="${SETUP_NODE_LINK_DIR:-/usr/local/lib/bosun-node}"
NODE_PROFILE="${SETUP_NODE_PROFILE:-/etc/profile.d/bosun-node.sh}"
node_major_ok() { "$1" --version 2>/dev/null | grep -q "^v${NODE_MAJOR}\."; }
if ! node_major_ok "$(command -v node || echo node)"; then
  if node_major_ok "$NODE_BIN_DIR/node"; then
    first="$(command -v node || true)"
    {
      mkdir -p "$NODE_LINK_DIR"
      for b in node npm npx; do [ -e "$NODE_BIN_DIR/$b" ] && ln -sf "$NODE_BIN_DIR/$b" "$NODE_LINK_DIR/$b"; done
      printf 'export PATH=%q:"$PATH"\n' "$NODE_LINK_DIR" >"$NODE_PROFILE"
    } 2>/dev/null || echo "setup-script: could not link Node ${NODE_MAJOR} into $NODE_LINK_DIR or write $NODE_PROFILE" >&2
    if [ -n "$first" ] && [ "$first" != "$NODE_BIN_DIR/node" ]; then
      ln -sf "$NODE_BIN_DIR/node" "$first" 2>/dev/null ||
        echo "setup-script: could not replace $first with $NODE_BIN_DIR/node" >&2
    fi
  fi
  if ! node_major_ok "$(command -v node || echo node)"; then
    echo "setup-script: node resolves to $(command -v node || echo none) ($(node --version 2>/dev/null || echo unknown)), not Node ${NODE_MAJOR} at $NODE_BIN_DIR/node" >&2
  fi
fi

# pnpm is not integrity-pinned (npm installs it by version), so its warm skip stays a version check.
# pnpm 12 ships a native binary; its install.js puts it in place.
# From /: inside the checkout pnpm may follow package.json `packageManager` instead.
if [ -z "$HAS_PNPM" ]; then
  echo "setup-script: no pnpm-lock.yaml, pnpm-workspace.yaml or packageManager pnpm@ in . or $PWD; skipping pnpm" >&2
elif [ "$(cd / && pnpm --version 2>/dev/null)" != "$PNPM_VERSION" ]; then
  npm install -g "pnpm@${PNPM_VERSION}" ||
    echo "setup-script: npm install of pnpm@${PNPM_VERSION} failed; continuing" >&2
  (cd "$(npm root -g)/pnpm" && node install.js) || true
fi

# Rust (the version read above), only in a checkout with Cargo.toml or a toolchain file.
# Without a toolchain file in the checkout rustup's stable default serves the tools, and rustup
# installs the checkout's pinned toolchain on first cargo use there. cargo-deny installs below,
# after install_tool. A failed step warns and the script goes on: a failing setup script blocks
# every session.
if [ -z "$HAS_RUST" ]; then
  echo "setup-script: no Cargo.toml or rust-toolchain.toml in . or $PWD; skipping Rust" >&2
elif [ -n "$RUST_VERSION" ]; then
  { rustup toolchain install "${RUST_VERSION}" --profile minimal --component rustfmt,clippy &&
    rustup default "${RUST_VERSION}"; } ||
    echo "setup-script: installing the Rust toolchain ${RUST_VERSION} failed; continuing" >&2
elif command -v rustup >/dev/null 2>&1 && ! rustup show active-toolchain >/dev/null 2>&1; then
  { rustup toolchain install stable --profile minimal --component rustfmt,clippy &&
    rustup default stable; } ||
    echo "setup-script: installing the stable Rust toolchain failed; continuing" >&2
fi

# The keelstackhq tools, pinned, install only from npm tarballs that match the pinned sha512:
# each launcher and this machine's binary package, side by side. A mismatch or a failed install
# skips that tool with a warning (a failing setup script blocks every session).
tool_pack() {
  f="$(cd "$2" && npm pack --silent "$1")"
  got="$(node -e 'process.stdout.write(require("crypto").createHash("sha512").update(require("fs").readFileSync(process.argv[1])).digest("base64"))' "$2/$f")"
  if [ "$got" != "$3" ]; then echo "setup-script: $1 does not match its pinned sha512; skipping it" >&2; return 1; fi
  echo "$2/$f"
}
# Where a verified install leaves its fingerprint. A later run skips a tool only when the current
# fingerprint equals the record for that tool@pin, so no other program that prints the pinned
# version is trusted.
SETUP_STATE_DIR="${SETUP_STATE_DIR:-/var/lib/bosun-setup}"
tool_arch() { case "$(uname -m)" in aarch64 | arm64) echo arm64 ;; *) echo x64 ;; esac; }
# tool_fp <name> <arch>: sha256 over the resolved command's real path and every file and symlink of
# the tool's launcher and linux packages; fails when PATH resolves to anything outside the launcher package.
tool_fp() {
  r="$(npm root -g 2>/dev/null)" && p="$(readlink -f "$(command -v "$1")" 2>/dev/null)" || return 1
  case "$p" in "$r/@keelstackhq/$1/"*) ;; *) return 1 ;; esac
  set -- "$r/@keelstackhq/$1" "$r/@keelstackhq/$1-linux-$2"
  # Symlinks count by name and target (a planted one can win Node's module lookup); byte order.
  { echo "$p"; { find "$@" -type l -printf '%p -> %l\0' 2>/dev/null || true; } | LC_ALL=C sort -z | tr '\0' '\n'
    { find "$@" -type f -print0 2>/dev/null || true; } | LC_ALL=C sort -z | xargs -0 -r sha256sum; } | sha256sum | cut -d' ' -f1
}
# tool_warm <name> <arch> <record>: true when the installed tool matches its recorded fingerprint.
tool_warm() { [ -f "$3" ] && fp="$(tool_fp "$1" "$2")" && [ "$fp" = "$(cat "$3")" ]; }
# tool_record <name> <arch> <record>: after a verified install; never fails the script.
tool_record() {
  if fp="$(tool_fp "$1" "$2")" && mkdir -p "$SETUP_STATE_DIR" && printf '%s\n' "$fp" >"$3"; then :; else
    echo "setup-script: could not record the fingerprint of $1; it installs again on the next run" >&2
  fi
}
# install_tool <name> <version> <launcher sha512> <linux-x64 sha512> <linux-arm64 sha512>
install_tool() {
  case "$(uname -m)" in aarch64 | arm64) a=arm64 s="$5" ;; *) a=x64 s="$4" ;; esac
  rec="$SETUP_STATE_DIR/$1@$2.sha256"
  # Warm container or a second checkout: skip only a tool whose fingerprint equals the one recorded
  # at its verified install.
  if tool_warm "$1" "$a" "$rec"; then return 0; fi
  t="$(mktemp -d)"
  if l="$(tool_pack "@keelstackhq/$1@$2" "$t" "$3")" && b="$(tool_pack "@keelstackhq/$1-linux-$a@$2" "$t" "$s")"; then
    # npm still reads the optional packages' metadata: a registry error must not stop the script.
    if npm install -g --ignore-scripts --omit=optional "$l" "$b"; then
      tool_record "$1" "$a" "$rec"
    else
      echo "setup-script: $1 install failed; skipping $1" >&2
    fi
  fi
}
install_tool splice "$SPLICE_VERSION" "$SPLICE_SHA512" "$SPLICE_LINUX_X64_SHA512" "$SPLICE_LINUX_ARM64_SHA512"
install_tool hoist "$HOIST_VERSION" "$HOIST_SHA512" "$HOIST_LINUX_X64_SHA512" "$HOIST_LINUX_ARM64_SHA512"
install_tool berth "$BERTH_VERSION" "$BERTH_SHA512" "$BERTH_LINUX_X64_SHA512" "$BERTH_LINUX_ARM64_SHA512"
# cargo-deny for the deps gate: the pinned npm packages when their pin is set, else (or when that
# install fails) built by cargo. The test gate's own tool (cargo-nextest) installs below. Only a verified install
# (its recorded fingerprint) counts, not another cargo-deny on PATH.
# cargo_tool <crate> <version> <url> <sha256>: the pinned prebuilt binary in $CARGO_HOME/bin, else
# `cargo install --locked`. Warm: skipped only when the binary there has the sha256 recorded at
# its verified install (a binary that merely prints the version is not trusted). Never fails.
cargo_tool() {
  crate="$1" ver="$2" url="$3" sum="$4"
  dest="${CARGO_HOME:-${HOME:-/root}/.cargo}/bin"
  rec="$SETUP_STATE_DIR/$crate@$ver.bin.sha256"
  if [ -f "$rec" ] && [ -x "$dest/$crate" ] &&
    [ "$(sha256sum "$dest/$crate" 2>/dev/null | cut -d' ' -f1)" = "$(cat "$rec")" ]; then return 0; fi
  if d="$(mktemp -d)" &&
    curl -fsSL --retry 2 --connect-timeout 15 -m 180 -o "$d/t.tgz" "$url" 2>/dev/null &&
    [ "$(sha256sum "$d/t.tgz" 2>/dev/null | cut -d' ' -f1)" = "$sum" ] &&
    tar -xzf "$d/t.tgz" -C "$d" && b="$(find "$d" -name "$crate" -type f | head -n1)" && [ -n "$b" ] &&
    mkdir -p "$dest" && install -m 0755 "$b" "$dest/$crate"; then
    rm -rf "$d"
    mkdir -p "$SETUP_STATE_DIR" && sha256sum "$dest/$crate" | cut -d' ' -f1 >"$rec" ||
      echo "setup-script: could not record the fingerprint of $crate; it installs again on the next run" >&2
    return 0
  fi
  [ -z "${d:-}" ] || rm -rf "$d"
  echo "setup-script: prebuilt $crate $ver unavailable or not matching its pinned sha256; building it with cargo (minutes)" >&2
  if command -v cargo >/dev/null 2>&1; then
    cargo install --locked "$crate" || echo "setup-script: cargo install of $crate failed; continuing without it" >&2
  else
    echo "setup-script: no cargo found; skipping $crate" >&2
  fi
}
case "$(uname -m)" in
  aarch64 | arm64) DENY_ARCH=aarch64 NEXTEST_SUM="$NEXTEST_LINUX_ARM64_SHA256" DENY_SUM="$CARGO_DENY_BIN_LINUX_ARM64_SHA256" ;;
  *) DENY_ARCH=x86_64 NEXTEST_SUM="$NEXTEST_LINUX_X64_SHA256" DENY_SUM="$CARGO_DENY_BIN_LINUX_X64_SHA256" ;;
esac
if [ -n "$HAS_RUST" ]; then
  if [ -n "$CARGO_DENY_VERSION" ]; then
    install_tool cargo-deny "$CARGO_DENY_VERSION" "$CARGO_DENY_SHA512" "$CARGO_DENY_LINUX_X64_SHA512" "$CARGO_DENY_LINUX_ARM64_SHA512"
  fi
  if [ -n "$CARGO_DENY_VERSION" ] &&
    tool_warm cargo-deny "$(tool_arch)" "$SETUP_STATE_DIR/cargo-deny@${CARGO_DENY_VERSION}.sha256"; then
    : # installed from its pin
  else
    cargo_tool cargo-deny "$CARGO_DENY_BIN_VERSION" \
      "https://github.com/EmbarkStudios/cargo-deny/releases/download/${CARGO_DENY_BIN_VERSION}/cargo-deny-${CARGO_DENY_BIN_VERSION}-${DENY_ARCH}-unknown-linux-musl.tar.gz" \
      "$DENY_SUM"
  fi
  # cargo-nextest only while the pinned bosun's test gate runs it (releases before 0.12.0, spec
  # §7) or a repo profile names it: `cargo install` builds it from source (minutes).
  if { [ "$(printf '%s\n0.12.0\n' "$BOSUN_VERSION" | sort -V | head -n1)" != 0.12.0 ] ||
    grep -qs nextest .bosun/profiles/rust.toml; }; then
    cargo_tool cargo-nextest "$NEXTEST_VERSION" \
      "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/cargo-nextest-${NEXTEST_VERSION}-${DENY_ARCH}-unknown-linux-gnu.tar.gz" \
      "$NEXTEST_SUM"
  fi
fi
# bosun's integrity is not known when the channel is written (the pin is the channel's release
# or bosun_version), so bosun installs by version and npm checks the registry's integrity. It is
# skipped only at its recorded fingerprint, like the other tools.
a="$(tool_arch)"
rec="$SETUP_STATE_DIR/bosun@${BOSUN_VERSION}.sha256"
if ! tool_warm bosun "$a" "$rec"; then
  npm install -g --ignore-scripts "@keelstackhq/bosun@${BOSUN_VERSION}"
  tool_record bosun "$a" "$rec"
fi

bosun --version
# The installed bosun must be the pinned one: an older bosun earlier on PATH shadows the install.
# Mismatch: put the npm-global bin first on PATH for this script and warn loudly. Never fails.
got="$(bosun --version 2>/dev/null | awk '{print $NF}')"
if [ "$got" != "$BOSUN_VERSION" ]; then
  gbin="$(npm prefix -g 2>/dev/null)/bin"
  if [ -x "$gbin/bosun" ]; then
    PATH="$gbin:$PATH"
    export PATH
    got="$(bosun --version 2>/dev/null | awk '{print $NF}')"
  fi
  if [ "$got" != "$BOSUN_VERSION" ]; then
    echo "setup-script: WARNING: bosun on PATH is '${got:-none}', not the pinned $BOSUN_VERSION; add $gbin to the front of PATH" >&2
  else
    echo "setup-script: WARNING: an older bosun shadowed the pinned $BOSUN_VERSION; put $gbin first on PATH in your shell" >&2
  fi
fi

# Prewarm the Claude Code plugins (user scope) so the first session does not clone them. Every
# failure, an outage or a pin mismatch, only warns (owner decision): `bosun doctor` shows it red.
# No token: the marketplace and the plugins are public.
if command -v claude >/dev/null 2>&1; then
  claude plugin marketplace add "https://github.com/keelstackhq/bosun.git#$KEELSTACK_MARKETPLACE_REF" >/dev/null ||
    echo "setup-script: adding the keelstack plugin marketplace failed; continuing" >&2
  plugins_file="${CLAUDE_CONFIG_DIR:-${HOME:-/root}/.claude}/plugins/installed_plugins.json"
  # plugin_prewarm <plugin@keelstack> <pinned sha>
  plugin_prewarm() {
    claude plugin install "$1" --scope user >/dev/null ||
      echo "setup-script: claude plugin install $1 failed; continuing" >&2
    got="$(node -e 'try{const p=require(process.argv[1]).plugins[process.argv[2]]||[];process.stdout.write(String((p[0]||{}).gitCommitSha||""))}catch(e){}' "$plugins_file" "$1" 2>/dev/null || true)"
    if [ -z "$got" ]; then
      echo "setup-script: $1 is not installed; cannot check its pin" >&2
    elif [ "$got" != "$2" ]; then
      echo "setup-script: $1 is at $got, not its pinned $2" >&2
    fi
  }
  plugin_prewarm superpowers@keelstack "$SUPERPOWERS_SHA"
  plugin_prewarm caveman@keelstack "$CAVEMAN_SHA"
else
  echo "setup-script: claude is not on PATH; skipping the plugin prewarm" >&2
fi
