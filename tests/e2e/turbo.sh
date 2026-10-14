#!/usr/bin/env bash
# End-to-end: a real Turborepo build round-trips through hoard's cache.
#   tests/e2e/turbo.sh                 fs storage
#   STORAGE=r2 tests/e2e/turbo.sh      MinIO as R2 (docker compose up -d --wait minio)
# HOARD_BIN overrides the binary (default: cargo build --release).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
storage="${STORAGE:-fs}"
hoard="${HOARD_BIN:-}"
if [ -z "$hoard" ]; then
	(cd "$root" && cargo build --release)
	hoard="$root/target/release/hoard"
fi

work="$(mktemp -d)"
server_pid=""
ok=0
bucket=""

cleanup() {
	if [ "$ok" != 1 ]; then
		echo "--- e2e FAILED; hoard log ---" >&2
		cat "$work/hoard.log" >&2 2>/dev/null || true
	fi
	if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
		kill "$server_pid" 2>/dev/null || true
		wait "$server_pid" 2>/dev/null || true
	fi
	if [ -n "$bucket" ]; then
		(cd "$root" && docker compose exec -T minio mc rb --force "local/$bucket" >/dev/null 2>&1) || true
	fi
	rm -rf "$work"
}
trap cleanup EXIT

fail() {
	echo "e2e: $*" >&2
	exit 1
}

cp -R "$root/tests/fixtures/turbo-repo" "$work/repo"
rm -rf "$work/repo/node_modules" "$work/repo/.turbo"

serve_args=(--storage "$storage" --port-file "$work/port" --token-file "$work/token"
	--summary-file "$work/summary.md")
case "$storage" in
fs)
	serve_args+=(--fs-root "$work/store")
	;;
r2)
	endpoint="${HOARD_TEST_S3_ENDPOINT:-http://127.0.0.1:9000}"
	bucket="hoard-e2e-$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
	(cd "$root" && docker compose exec -T minio mc mb "local/$bucket" >/dev/null) ||
		fail "could not create bucket $bucket (is MinIO up? docker compose up -d --wait minio)"
	export HOARD_R2_ACCESS_KEY_ID=minio HOARD_R2_SECRET_ACCESS_KEY=minio123
	serve_args+=(--r2-endpoint "$endpoint" --r2-bucket "$bucket")
	;;
*) fail "STORAGE must be fs or r2, got '$storage'" ;;
esac

"$hoard" serve "${serve_args[@]}" >"$work/hoard.log" 2>&1 &
server_pid=$!

# The port file is the readiness signal: hoard writes the token file first, then the port file.
for _ in $(seq 1 100); do
	[ -s "$work/port" ] && break
	kill -0 "$server_pid" 2>/dev/null || fail "hoard exited during startup"
	sleep 0.1
done
[ -s "$work/port" ] || fail "hoard did not write its port file"
[ -s "$work/token" ] || fail "hoard wrote its port file but no token file"

eval "$("$hoard" env --port-file "$work/port" --token-file "$work/token")"
export TURBO_API TURBO_TOKEN TURBO_TEAM
export TURBO_TELEMETRY_DISABLED=1 DO_NOT_TRACK=1 TURBO_NO_UPDATE_NOTIFIER=1
token="$TURBO_TOKEN"
base="http://127.0.0.1:$(cat "$work/port")"

cd "$work/repo"
pnpm install --frozen-lockfile >"$work/install.log" 2>&1 || {
	cat "$work/install.log" >&2
	fail "pnpm install failed"
}

pnpm turbo run build >"$work/run1.log" 2>&1 || {
	cat "$work/run1.log" >&2
	fail "first turbo run failed"
}
grep -q "Remote caching enabled" "$work/run1.log" || {
	cat "$work/run1.log" >&2
	fail "turbo did not enable remote caching"
}

# Only the remote cache can serve the second run.
rm -rf .turbo node_modules/.cache/turbo packages/*/dist

pnpm turbo run build >"$work/run2.log" 2>&1 || {
	cat "$work/run2.log" >&2
	fail "second turbo run failed"
}
cat "$work/run2.log"
grep -q "FULL TURBO" "$work/run2.log" || fail "second run was not FULL TURBO"
[ -f packages/b/dist/out.txt ] || fail "packages/b/dist/out.txt was not restored"

stats="$(curl -sS -H "Authorization: Bearer $token" "$base/_hoard/stats")"
puts="$(printf '%s' "$stats" | node -e 'process.stdout.write(String(JSON.parse(require("fs").readFileSync(0, "utf8")).puts))')"
[ "$puts" = 2 ] || fail "expected puts == 2, got $puts (stats: $stats)"

curl -sS -f -X POST -H "Authorization: Bearer $token" "$base/_hoard/shutdown" >/dev/null ||
	fail "shutdown request failed"
wait "$server_pid" || fail "hoard did not exit 0"
server_pid=""

echo "--- summary ---"
cat "$work/summary.md"
grep -q '^| 2 | ' "$work/summary.md" || fail "summary does not show 2 hits"

if [ "$storage" = r2 ]; then
	listing="$(cd "$root" && docker compose exec -T minio mc ls --recursive "local/$bucket/cache/turbo/v1/")"
	[ -n "$listing" ] || fail "no objects under cache/turbo/v1/ in $bucket"
	grep -q 'Lifecycle: 7 days ✓' "$work/summary.md" || fail "summary lacks 'Lifecycle: 7 days ✓'"
fi

ok=1
echo "e2e ($storage): PASS"
