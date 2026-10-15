import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { detectTools } from "../src/tools.js";

const fixtures = fileURLToPath(new URL("../../tests/fixtures/", import.meta.url));

describe("detectTools", () => {
  test("the Turborepo fixture is turbo only", () => {
    expect(detectTools(join(fixtures, "turbo-repo"))).toEqual({ turbo: true, vp: false });
  });

  test("the Vite+ fixture is vp only", () => {
    expect(detectTools(join(fixtures, "vp-repo"))).toEqual({ turbo: false, vp: true });
  });

  describe("in a scratch repo", () => {
    let dir: string;
    beforeEach(async () => {
      dir = await mkdtemp(join(tmpdir(), "hoard-tools-"));
    });
    afterEach(async () => {
      await rm(dir, { recursive: true, force: true });
    });

    test("nothing there: neither", () => {
      expect(detectTools(dir)).toEqual({ turbo: false, vp: false });
    });

    test("both tools at once", async () => {
      await writeFile(join(dir, "turbo.json"), "{}");
      await writeFile(
        join(dir, "package.json"),
        JSON.stringify({ dependencies: { "vite-plus": "1.2.0" } }),
      );
      expect(detectTools(dir)).toEqual({ turbo: true, vp: true });
    });

    test("vite-plus must be a dependency, not just mentioned", async () => {
      await writeFile(
        join(dir, "package.json"),
        JSON.stringify({ name: "vite-plus-docs", scripts: { x: "vite-plus" } }),
      );
      expect(detectTools(dir).vp).toBe(false);
    });

    test("a broken package.json is no vp, not an error", async () => {
      await writeFile(join(dir, "package.json"), "{ not json");
      expect(detectTools(dir)).toEqual({ turbo: false, vp: false });
    });

    test("turbo.jsonc counts as turbo", async () => {
      await writeFile(join(dir, "turbo.jsonc"), "{}");
      expect(detectTools(dir).turbo).toBe(true);
    });
  });
});
