import { readFileSync } from "node:fs";
import { expect, test } from "vitest";

// release.yml commits as github-actions[bot], and that push does not trigger CI. So the release
// itself must run the action tests against the freshly baked checksums before it commits;
// otherwise a test that depends on the baked VERSION turns main red unnoticed (as with 0.1.0).
test("release.yml runs the action tests after baking checksums and before committing", () => {
  const yml = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");
  const bake = yml.indexOf("- name: Bake checksums");
  const tests = yml.indexOf("pnpm --dir action test");
  const commit = yml.indexOf("- name: Commit and tag");
  expect(bake).toBeGreaterThan(-1);
  expect(commit).toBeGreaterThan(-1);
  expect(tests).toBeGreaterThan(bake);
  expect(tests).toBeLessThan(commit);
});
