// Runs the bundled dist/main.js and dist/post.js against a real hoard binary with a fake
// Actions environment. Skipped unless HOARD_TEST_BIN points at a hoard build, e.g.
//   cargo build && HOARD_TEST_BIN=$PWD/target/debug/hoard pnpm --dir action test
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, expect, test } from "vitest";

const bin = process.env.HOARD_TEST_BIN;
const dist = fileURLToPath(new URL("../dist/", import.meta.url));
const run = promisify(execFile);

/** Parses a GITHUB_ENV / GITHUB_OUTPUT / GITHUB_STATE file (`name<<delim\nvalue\ndelim`). */
function parseFileCommands(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const m = /^([^<=]+)<<(.+)$/.exec(lines[i] ?? "");
    if (!m?.[1] || !m[2]) continue;
    const value: string[] = [];
    while (++i < lines.length && lines[i] !== m[2]) value.push(lines[i] ?? "");
    out[m[1]] = value.join("\n");
  }
  return out;
}

let work = "";
let pid = 0;
afterAll(async () => {
  if (pid) {
    try {
      process.kill(pid);
    } catch {
      // already exited, as it should after post
    }
  }
  if (work) await rm(work, { recursive: true, force: true });
});

test.skipIf(!bin)(
  "main starts hoard, turbo-style PUT and GET work, post reports a hit",
  async () => {
    work = await mkdtemp(join(tmpdir(), "hoard-action-it-"));
    const files = Object.fromEntries(
      ["GITHUB_ENV", "GITHUB_OUTPUT", "GITHUB_STATE", "GITHUB_STEP_SUMMARY"].map((k) => [
        k,
        join(work, `${k.toLowerCase()}.txt`),
      ]),
    );
    for (const f of Object.values(files)) await writeFile(f, "");
    const env = {
      PATH: process.env.PATH,
      RUNNER_TEMP: work,
      ...files,
      INPUT_STORAGE: "fs",
      HOARD_ACTION_BINARY_PATH: bin,
      HOARD_TOKEN: "stray-known-token",
    };

    const main = await run(process.execPath, [join(dist, "main.js")], { cwd: work, env });
    expect(main.stdout).toContain(
      "::warning::using unverified binary from HOARD_ACTION_BINARY_PATH",
    );
    expect(main.stdout).not.toContain("::error::");

    const exported = parseFileCommands(await readFile(files.GITHUB_ENV, "utf8"));
    const state = parseFileCommands(await readFile(files.GITHUB_STATE, "utf8"));
    pid = Number(state.pid);
    expect(exported.TURBO_API).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
    expect(exported.TURBO_TEAM).toBe("hoard");
    expect(exported.TURBO_TOKEN).toMatch(/^[0-9a-f]{64}$/);
    expect(exported.TURBO_TOKEN).not.toBe("stray-known-token");
    expect(main.stdout).toContain(`::add-mask::${exported.TURBO_TOKEN}`);

    const auth = { authorization: `Bearer ${exported.TURBO_TOKEN}` };
    const artifact = `${exported.TURBO_API}/v8/artifacts/abc123`;
    const body = "turbo artifact bytes";
    const put = await fetch(artifact, {
      method: "PUT",
      headers: {
        ...auth,
        "content-type": "application/octet-stream",
        "x-artifact-duration": "750",
      },
      body,
    });
    expect(put.status).toBe(200);
    const get = await fetch(artifact, { headers: auth });
    expect(get.status).toBe(200);
    expect(await get.text()).toBe(body);

    const stateEnv = Object.fromEntries(Object.entries(state).map(([k, v]) => [`STATE_${k}`, v]));
    const post = await run(process.execPath, [join(dist, "post.js")], {
      cwd: work,
      env: { ...env, ...stateEnv },
    });
    expect(post.stdout).not.toContain("::warning::");

    const outputs = parseFileCommands(await readFile(files.GITHUB_OUTPUT, "utf8"));
    expect(outputs).toMatchObject({ hits: "1", misses: "0", "time-saved-ms": "750" });
    expect(await readFile(files.GITHUB_STEP_SUMMARY, "utf8")).toContain("### hoard");
    // The fs store lives in RUNNER_TEMP under the step's id, never in ./.hoard of the checkout.
    await expect(stat(join(work, ".hoard"))).rejects.toThrow(/ENOENT/);
    expect(state.log).toMatch(/hoard-[0-9a-f]{12}\.log$/);
    await expect(stat(state.log?.replace(/\.log$/, "-store") ?? "")).resolves.toBeTruthy();

    // The shutdown answer comes after the summary is written; the process then exits.
    await expect
      .poll(
        () => {
          try {
            process.kill(pid, 0);
            return "running";
          } catch {
            return "exited";
          }
        },
        { timeout: 5_000 },
      )
      .toBe("exited");
  },
  30_000,
);
