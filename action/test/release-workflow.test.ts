import { readFileSync } from "node:fs";
import { expect, test } from "vitest";

// release.yml commits the release as github-actions[bot] and pushes only the tag, so no CI runs on
// that commit. The release itself must run the action tests against the freshly baked checksums
// before anything is committed; otherwise a test that depends on the baked VERSION fails unnoticed.
test("release.yml runs the action tests after baking checksums and before committing", () => {
  const yml = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");
  const bake = yml.indexOf("- name: Bake checksums");
  const tests = yml.indexOf("pnpm --dir action test");
  const commit = yml.indexOf("name: Commit and tag");
  expect(bake).toBeGreaterThan(-1);
  expect(commit).toBeGreaterThan(-1);
  expect(tests).toBeGreaterThan(bake);
  expect(tests).toBeLessThan(commit);
});

test("publish runs no dependency code; detect and publish share the completeness check", () => {
  const yml = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");
  const publish = yml.slice(yml.indexOf("\n  publish:"));
  const action = yml.slice(yml.indexOf("\n  action:"), yml.indexOf("\n  publish:"));
  expect(publish).not.toMatch(/pnpm|npm (install|ci)/);
  expect(action).toContain("contents: read");
  expect(action).toContain("pnpm --dir action test");
  const detect = yml.slice(yml.indexOf("\n  detect:"), yml.indexOf("\n  build:"));
  expect(detect).toContain("release-complete.sh check");
  expect(publish).toContain("release-complete.sh check");
});

const yml = () =>
  readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");

test("detect fails early (before build) on a partial release, but not on an API error", () => {
  const detect = yml().slice(yml().indexOf("\n  detect:"), yml().indexOf("\n  build:"));
  expect(detect).toMatch(/rc=\$\?/);
  expect(detect).toMatch(/\[ "\$rc" = 4 \]/); // partial: red, nothing built or approved
  expect(detect).toContain("exit 1");
  expect(detect).toContain("pending=true"); // rc 1 (API error), 3, 5: publish decides
});

test("build legs export their tarball sha256 as job outputs; publish compares them", () => {
  const y = yml();
  const build = y.slice(y.indexOf("\n  build:"), y.indexOf("\n  action:"));
  const publish = y.slice(y.indexOf("\n  publish:"));
  for (const t of [
    "aarch64_apple_darwin",
    "aarch64_unknown_linux_musl",
    "x86_64_apple_darwin",
    "x86_64_pc_windows_msvc",
    "x86_64_unknown_linux_musl",
  ]) {
    expect(build).toContain(`sha_${t}: \${{ steps.sum.outputs.sha_${t} }}`);
  }
  expect(publish).toContain("toJSON(needs.build.outputs)");
  expect(publish).toContain("scripts/verify-build-shas.sh");
});
