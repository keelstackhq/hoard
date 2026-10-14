import { execFile } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer, type Server, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { exitWhenDone } from "../src/exit.js";

const src = fileURLToPath(new URL("../src/", import.meta.url));

let dir: string;
let blackHole: Server | undefined;
const held: Socket[] = [];
const savedExitCode = process.exitCode;
beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), "hoard-exit-"));
});
afterEach(async () => {
  process.exitCode = savedExitCode;
  for (const s of held.splice(0)) s.destroy();
  await new Promise((r) => (blackHole ? blackHole.close(r) : r(undefined)));
  blackHole = undefined;
  await rm(dir, { recursive: true, force: true });
});

test("exits with 0 after a run that did not fail", async () => {
  const exit = vi.fn();
  await exitWhenDone(async () => {}, exit);
  expect(exit).toHaveBeenCalledWith(0);
});

test("exits with the exit code core.setFailed set", async () => {
  const exit = vi.fn();
  await exitWhenDone(async () => {
    process.exitCode = 1; // what core.setFailed does
  }, exit);
  expect(exit).toHaveBeenCalledWith(1);
});

test("a download stuck past the cap does not hold the main step open", async () => {
  // Accepts connections and never answers: tc.downloadTool's request would wait minutes.
  blackHole = createServer((s) => void held.push(s));
  await new Promise<void>((r) => blackHole?.listen(0, "127.0.0.1", r));
  const port = (blackHole.address() as { port: number }).port;

  // Mirrors src/main.ts, with the real tc.downloadTool pointed at the black hole and a short cap.
  const entry = join(dir, "entry.ts");
  await writeFile(
    entry,
    `import * as tc from "@actions/tool-cache";
import { ensureBinary } from ${JSON.stringify(join(src, "binary.ts"))};
import { exitWhenDone } from ${JSON.stringify(join(src, "exit.ts"))};
import { mainDeps, runMain } from ${JSON.stringify(join(src, "lifecycle.ts"))};
const deps = mainDeps();
deps.ensureBinary = (target, version) =>
  ensureBinary(target, version, { [target]: "0".repeat(64) },
    () => tc.downloadTool("http://127.0.0.1:${port}/hoard.tar.gz"), process.env, { timeoutMs: 300 });
await exitWhenDone(() => runMain(deps));
`,
  );
  const out = join(dir, "entry.mjs");
  await build({
    entryPoints: [entry],
    outfile: out,
    bundle: true,
    platform: "node",
    format: "esm",
    banner: {
      js: 'import{createRequire as __r}from"node:module";const require=__r(import.meta.url);',
    },
    alias: { undici: join(src, "undici-stub.ts") },
    nodePaths: [fileURLToPath(new URL("../node_modules/", import.meta.url))],
    logLevel: "error",
  });
  const files = { GITHUB_ENV: join(dir, "env"), GITHUB_STATE: join(dir, "state") };
  for (const f of Object.values(files)) await writeFile(f, "");

  const started = Date.now();
  const result = await new Promise<{ code: number | null; stdout: string }>((resolve) => {
    const child = execFile(
      process.execPath,
      [out],
      {
        env: { PATH: process.env.PATH, RUNNER_TEMP: dir, RUNNER_TOOL_CACHE: dir, ...files },
        timeout: 20_000,
      },
      (_err, stdout) => resolve({ code: child.exitCode, stdout }),
    );
  });
  expect(result.stdout).toContain(
    "::warning::hoard binary could not be downloaded (no answer within 300ms); continuing without the cache",
  );
  expect(result.code).toBe(0);
  expect(Date.now() - started).toBeLessThan(5_000);
}, 30_000);
