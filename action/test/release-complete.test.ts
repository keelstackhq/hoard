import { spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "vitest";

// Runs scripts/release-complete.sh against a fake `gh` that prints what the real one would print
// after its -q filter (the filters themselves are checked against real gh output by hand).
const script = new URL("../../scripts/release-complete.sh", import.meta.url).pathname;
const FIVE = [
  "hoard-aarch64-apple-darwin.tar.gz",
  "hoard-aarch64-unknown-linux-musl.tar.gz",
  "hoard-x86_64-apple-darwin.tar.gz",
  "hoard-x86_64-pc-windows-msvc.tar.gz",
  "hoard-x86_64-unknown-linux-musl.tar.gz",
].join(",");

interface World {
  refs?: string[]; // tags
  release?: string; // draft \t pre \t assets; absent: no visible release
  stable?: string[]; // published stable release tags
  shas?: Record<string, string>; // ref name -> commit
}

function run(world: World, ...args: string[]) {
  const dir = mkdtempSync(join(tmpdir(), "gh-"));
  const gh = join(dir, "gh");
  writeFileSync(
    gh,
    `#!/usr/bin/env bash
case "$1 $2" in
  "api repos/o/r/git/matching-refs/tags/"*) p=\${2#repos/o/r/git/matching-refs/tags/}
    printf '%s\\n' ${shq((world.refs ?? []).map((t) => `refs/tags/${t}`))} | grep -F "refs/tags/$p" || true ;;
  "release view") ${world.release === undefined ? "exit 1" : `printf '%s\\n' ${shq([world.release])}`} ;;
  "api repos/o/r/releases") printf '%s\\n' ${shq(world.stable ?? [])} ;;
  "api repos/o/r/commits/"*) n=\${2#repos/o/r/commits/}
    case "$n" in ${Object.entries(world.shas ?? {})
      .map(([k, v]) => `${k}) echo ${v} ;;`)
      .join(" ")} *) exit 1 ;; esac ;;
  *) echo "unexpected gh $*" >&2; exit 99 ;;
esac
`,
  );
  chmodSync(gh, 0o755);
  const r = spawnSync("bash", [script, ...args], {
    encoding: "utf8",
    env: { PATH: `${dir}:${process.env.PATH}`, GITHUB_REPOSITORY: "o/r", GH_TOKEN: "x" },
  });
  return { code: r.status, out: r.stdout + r.stderr };
}
function shq(xs: string[]) {
  return xs.map((x) => `'${x}'`).join(" ") || "''";
}
const stable = (extra: Partial<World> = {}): World => ({
  refs: ["v1.2.0", "v1"],
  release: `false\tfalse\t${FIVE}`,
  stable: ["v1.2.0", "v1.1.0"],
  shas: { "v1.2.0": "aaa", v1: "aaa" },
  ...extra,
});

test("complete stable release with vX on it passes", () => {
  expect(run(stable(), "check", "1.2.0").code).toBe(0);
});
test("no tag: exit 3 (nothing released yet)", () => {
  expect(run(stable({ refs: ["v1.1.0"] }), "check", "1.2.0").code).toBe(3);
});
test("prefix tag (v1.2.0-rc.1) is not the tag", () => {
  expect(run(stable({ refs: ["v1.2.0-rc.1"] }), "check", "1.2.0").code).toBe(3);
});
test("tag without release fails", () => {
  expect(run(stable({ release: undefined }), "check", "1.2.0").code).toBe(1);
});
test("missing, extra or wrong assets fail", () => {
  const four = FIVE.split(",").slice(1).join(",");
  expect(run(stable({ release: `false\tfalse\t${four}` }), "check", "1.2.0").code).toBe(1);
  expect(run(stable({ release: "false\tfalse\t" }), "check", "1.2.0").code).toBe(1);
  expect(
    run(stable({ release: `false\tfalse\t${FIVE},hoard-x.tar.gz` }), "check", "1.2.0").code,
  ).toBe(1);
});
test("newest stable with stale or missing vX fails", () => {
  expect(run(stable({ shas: { "v1.2.0": "aaa", v1: "bbb" } }), "check", "1.2.0").code).toBe(1);
  expect(run(stable({ shas: { "v1.2.0": "aaa" } }), "check", "1.2.0").code).toBe(1);
});
test("backport (not newest published stable) ignores vX", () => {
  const w = stable({ stable: ["v1.3.0", "v1.2.0"], shas: { "v1.2.0": "aaa", v1: "ccc" } });
  expect(run(w, "check", "1.2.0").code).toBe(0);
});
test("a draft newer release does not count as newest stable", () => {
  // the draft v1.3.0 is not in the published list, so v1.2.0 is the newest and vX must be on it
  expect(run(stable({ shas: { "v1.2.0": "aaa", v1: "bbb" } }), "check", "1.2.0").code).toBe(1);
  expect(run(stable(), "newest-stable", "v1").out.trim()).toBe("v1.2.0");
});
test("draft or prerelease with five assets is complete; vX not needed", () => {
  const w = { shas: {} };
  expect(run(stable({ ...w, release: `true\tfalse\t${FIVE}` }), "check", "1.2.0").code).toBe(0);
  expect(run(stable({ ...w, release: `false\ttrue\t${FIVE}` }), "check", "1.2.0").code).toBe(0);
});
