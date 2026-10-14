import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { serverFiles } from "../src/server.js";
import { resolveSettings } from "../src/settings.js";
import { runVp, vpArgs } from "../src/vp.js";

let dir: string;
beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), "hoard-vp-"));
});
afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

async function stub(script: string): Promise<string> {
  const p = join(dir, "hoard-stub");
  await writeFile(p, `#!/bin/sh\n${script}\n`);
  await chmod(p, 0o755);
  return p;
}

describe("vpArgs", () => {
  test("the storage flags, the workspace and vp-max-size; never a secret", () => {
    const s = resolveSettings(
      {
        storage: "r2",
        "r2-access-key-id": "AKID",
        "r2-secret-access-key": "SECRET",
        "r2-bucket": "b",
        "r2-account-id": "acc",
        "vp-max-size": "500MB",
      },
      {},
    );
    const args = vpArgs(s, serverFiles(dir, "x"), "/work");
    expect(args).toEqual([
      "--storage",
      "r2",
      "--mode",
      "read-write",
      "--cache-version",
      "v1",
      "--r2-bucket",
      "b",
      "--r2-prefix",
      "cache/",
      "--r2-lifecycle",
      "auto",
      "--workspace",
      "/work",
      "--vp-max-size",
      "500MB",
    ]);
    expect(args.join(" ")).not.toMatch(/AKID|SECRET|acc/);
  });

  test("vp-max-size defaults to 2GB and fs storage shares the server's store", () => {
    const s = resolveSettings({ storage: "fs" }, {});
    const args = vpArgs(s, serverFiles(dir, "x"), "/w");
    expect(args[args.indexOf("--vp-max-size") + 1]).toBe("2GB");
    expect(args[args.indexOf("--fs-root") + 1]).toBe(serverFiles(dir, "x").fsRoot);
  });
});

describe.skipIf(process.platform === "win32")("runVp", () => {
  test("passes the command and args, returns the last stdout line", async () => {
    const bin = await stub('echo "log line" >&2\necho "$1 $2 $3"\necho "restored k (1 KB)"');
    const r = await runVp(bin, "restore", ["--storage", "fs"], {}, 5_000);
    expect(r).toEqual({ ok: true, line: "restored k (1 KB)" });
  });

  test("an exit-0 warning line is not ok", async () => {
    const bin = await stub(
      'echo "warning: Vite+ cache save failed (x); the build continues without it"',
    );
    const r = await runVp(bin, "save", [], {}, 5_000);
    expect(r.ok).toBe(false);
    expect(r.line).toBe("Vite+ cache save failed (x); the build continues without it");
  });

  test("a non-zero exit is not ok and carries the error line", async () => {
    const bin = await stub('echo "error: ACTIONS_RESULTS_URL is not set: fix it" >&2\nexit 1');
    const r = await runVp(bin, "save", [], {}, 5_000);
    expect(r).toEqual({
      ok: false,
      line: "hoard vp save exited with code 1: error: ACTIONS_RESULTS_URL is not set: fix it",
    });
  });

  test("a hung binary is killed at the timeout", async () => {
    const bin = await stub("sleep 30");
    const started = Date.now();
    const r = await runVp(bin, "restore", [], {}, 300);
    expect(Date.now() - started).toBeLessThan(5_000);
    expect(r).toEqual({ ok: false, line: "hoard vp restore timed out after 0.3s" });
  });

  test("a missing binary is not ok", async () => {
    const r = await runVp(join(dir, "nope"), "restore", [], {}, 5_000);
    expect(r.ok).toBe(false);
    expect(r.line).toMatch(/^hoard vp restore could not run/);
  });

  test("the env reaches the child", async () => {
    const bin = await stub('echo "$HOARD_R2_ACCESS_KEY_ID"');
    const r = await runVp(bin, "save", [], { HOARD_R2_ACCESS_KEY_ID: "k1" }, 5_000);
    expect(r.line).toBe("k1");
  });
});
