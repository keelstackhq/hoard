import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
  type ActionCore,
  type MainDeps,
  type PostDeps,
  runMain,
  runPost,
} from "../src/lifecycle.js";
import { getStats, shutdownServer, startServer, waitForHealth } from "../src/server.js";

/** Records every @actions/core call as [method, ...args]. */
function fakeCore(inputs: Record<string, string> = {}, state: Record<string, string> = {}) {
  const calls: unknown[][] = [];
  const summaries: string[] = [];
  let pending = "";
  const core: ActionCore = {
    getInput: (n) => inputs[n] ?? "",
    getState: (n) => state[n] ?? "",
    setSecret: (s) => void calls.push(["setSecret", s]),
    exportVariable: (n, v) => void calls.push(["exportVariable", n, v]),
    saveState: (n, v) => void calls.push(["saveState", n, v]),
    setOutput: (n, v) => void calls.push(["setOutput", n, v]),
    setFailed: (m) => void calls.push(["setFailed", m]),
    warning: (m) => void calls.push(["warning", m]),
    info: (m) => void calls.push(["info", m]),
    startGroup: (n) => void calls.push(["startGroup", n]),
    endGroup: () => void calls.push(["endGroup"]),
    summary: {
      addRaw(text) {
        pending += text;
        return {
          write: async () => {
            summaries.push(pending);
            pending = "";
          },
        };
      },
    },
  };
  const of = (method: string) => calls.filter((c) => c[0] === method).map((c) => c.slice(1));
  return { core, calls, summaries, of };
}

let dir: string;
beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), "hoard-lifecycle-"));
});
afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

describe("main", () => {
  function deps(f: ReturnType<typeof fakeCore>, over: Partial<MainDeps> = {}): MainDeps {
    return {
      core: f.core,
      env: {
        RUNNER_TEMP: dir,
        PATH: "/bin",
        HOARD_TOKEN: "stray",
        GITHUB_ACTION: "__keelstack_hoard",
      },
      platform: "linux",
      arch: "x64",
      ensureBinary: vi.fn(async () => "/opt/hoard"),
      startServer: vi.fn(async () => {
        await writeFile(join(dir, "hoard-__keelstack_hoard.token"), "tok123\n");
        return 4321;
      }),
      waitForHealth: vi.fn(async () => 40404),
      kill: vi.fn(),
      ...over,
    };
  }

  test("starts hoard, masks the token before exporting it, and saves state", async () => {
    const f = fakeCore({ storage: "r2", "r2-bucket": "b" }); // no secrets → github + warning
    const d = deps(f);
    await runMain(d);

    expect(f.of("setFailed")).toEqual([]);
    expect(d.ensureBinary).toHaveBeenCalledWith("x86_64-unknown-linux-musl", "0.0.0-dev");
    const [bin, args, log, env] = vi.mocked(d.startServer).mock.calls[0] ?? [];
    expect(bin).toBe("/opt/hoard");
    expect(args).toContain("--storage");
    expect(args?.[args.indexOf("--storage") + 1]).toBe("github");
    expect(log).toBe(join(dir, "hoard-__keelstack_hoard.log"));
    expect(args?.[args.indexOf("--token-file") + 1]).toBe(
      join(dir, "hoard-__keelstack_hoard.token"),
    );
    expect(args).not.toContain("--fs-root"); // github storage
    expect(env).toMatchObject({ PATH: "/bin" });
    expect(env).not.toHaveProperty("HOARD_TOKEN");
    expect(d.waitForHealth).toHaveBeenCalledWith(join(dir, "hoard-__keelstack_hoard.port"));
    expect(d.kill).not.toHaveBeenCalled();

    const secretAt = f.calls.findIndex((c) => c[0] === "setSecret" && c[1] === "tok123");
    const exportAt = f.calls.findIndex((c) => c[0] === "exportVariable" && c[1] === "TURBO_TOKEN");
    expect(secretAt).toBeGreaterThanOrEqual(0);
    expect(secretAt).toBeLessThan(exportAt);
    expect(f.of("exportVariable")).toEqual([
      ["TURBO_API", "http://127.0.0.1:40404"],
      ["TURBO_TOKEN", "tok123"],
      ["TURBO_TEAM", "hoard"],
    ]);
    expect(Object.fromEntries(f.of("saveState"))).toEqual({
      port: "40404",
      token: "tok123",
      pid: "4321",
      summary: join(dir, "hoard-__keelstack_hoard-summary.md"),
      log: join(dir, "hoard-__keelstack_hoard.log"),
      warnings: expect.stringContaining("R2 secrets are not available"),
    });
    expect(f.of("warning")).toEqual([[expect.stringMatching(/^R2 secrets are not available/)]]);
  });

  test("an unsupported platform fails before any download", async () => {
    const f = fakeCore();
    const d = deps(f, { platform: "linux", arch: "s390x" });
    await runMain(d);
    expect(f.of("setFailed")).toEqual([
      [expect.stringContaining("unsupported platform linux/s390x")],
    ]);
    expect(d.ensureBinary).not.toHaveBeenCalled();
    expect(d.startServer).not.toHaveBeenCalled();
  });

  test("invalid input fails with the settings message", async () => {
    const f = fakeCore({ mode: "sometimes" });
    const d = deps(f);
    await runMain(d);
    expect(f.of("setFailed")).toEqual([[expect.stringContaining('invalid mode "sometimes"')]]);
    expect(d.startServer).not.toHaveBeenCalled();
  });

  test("HOARD_ACTION_BINARY_PATH skips the verified download, with a warning", async () => {
    const f = fakeCore();
    const d = deps(f);
    d.env.HOARD_ACTION_BINARY_PATH = "/dev/hoard";
    await runMain(d);
    expect(d.ensureBinary).not.toHaveBeenCalled();
    expect(vi.mocked(d.startServer).mock.calls[0]?.[0]).toBe("/dev/hoard");
    expect(f.of("warning")).toEqual([
      ["using unverified binary from HOARD_ACTION_BINARY_PATH: /dev/hoard"],
    ]);
    expect(f.of("setFailed")).toEqual([]);
  });

  test("a binary that crashes at start fails the step and prints the server log", async () => {
    // Real spawn: node is the "binary"; `node serve …` exits at once (no such module).
    const f = fakeCore({ storage: "fs" });
    const d = deps(f, {
      startServer,
      waitForHealth: (portFile) => waitForHealth(portFile, 300),
    });
    d.env = { ...process.env, RUNNER_TEMP: dir, HOARD_ACTION_BINARY_PATH: process.execPath };
    await runMain(d);

    expect(f.of("setFailed")).toEqual([
      [expect.stringContaining("hoard did not start within 300ms")],
    ]);
    const group = f.calls.findIndex((c) => c[0] === "startGroup" && c[1] === "hoard log");
    expect(group).toBeGreaterThanOrEqual(0);
    expect(String(f.calls[group + 2]?.[1])).toContain("serve");
    expect(f.calls[group + 4]).toEqual(["endGroup"]);
    expect(f.of("exportVariable")).toEqual([]);
    expect(f.of("saveState")).toEqual([]);
  });

  test("a child that is alive but never healthy is killed before the step fails", async () => {
    const f = fakeCore();
    const d = deps(f, {
      waitForHealth: vi.fn(async () => {
        throw new Error("hoard did not start within 10s");
      }),
    });
    vi.mocked(d.kill).mockImplementation(() => void f.calls.push(["kill"]));
    await runMain(d);
    expect(d.kill).toHaveBeenCalledWith(4321);
    const killAt = f.calls.findIndex((c) => c[0] === "kill");
    const failAt = f.calls.findIndex((c) => c[0] === "setFailed");
    expect(killAt).toBeGreaterThanOrEqual(0);
    expect(killAt).toBeLessThan(failAt);
  });

  test("a failure before the spawn kills nothing", async () => {
    const f = fakeCore({ mode: "sometimes" });
    const d = deps(f);
    await runMain(d);
    expect(d.kill).not.toHaveBeenCalled();
  });

  test("fs storage keeps its store in RUNNER_TEMP, per step", async () => {
    const f = fakeCore({ storage: "fs" });
    const d = deps(f);
    await runMain(d);
    const args = vi.mocked(d.startServer).mock.calls[0]?.[1] ?? [];
    expect(args?.[args.indexOf("--fs-root") + 1]).toBe(join(dir, "hoard-__keelstack_hoard-store"));
  });

  test("two hoard steps in one job get distinct files", async () => {
    const paths: string[][] = [];
    for (const id of ["__keelstack_hoard", "__keelstack_hoard_2"]) {
      const f = fakeCore({ storage: "fs" });
      const d = deps(f, {
        startServer: vi.fn(async (_b: string, args: string[], log: string) => {
          await writeFile(args[args.indexOf("--token-file") + 1] ?? "", "t\n");
          paths.push([...args.filter((a) => a.startsWith(dir)), log]);
          return 1;
        }),
      });
      d.env.GITHUB_ACTION = id;
      await runMain(d);
      expect(f.of("setFailed")).toEqual([]);
    }
    expect(paths[0]).toHaveLength(5); // port, token, summary, fs-root, log
    for (const p of paths[0] ?? []) expect(paths[1]).not.toContain(p);
  });

  test("without GITHUB_ACTION each step gets a random id", async () => {
    const logs: string[] = [];
    for (let i = 0; i < 2; i++) {
      const f = fakeCore();
      const d = deps(f, {
        startServer: vi.fn(async (_b: string, args: string[], log: string) => {
          await writeFile(args[args.indexOf("--token-file") + 1] ?? "", "t\n");
          logs.push(log);
          return 1;
        }),
      });
      delete d.env.GITHUB_ACTION;
      await runMain(d);
    }
    expect(logs[0]).toMatch(/hoard-[0-9a-f]{12}\.log$/);
    expect(logs[0]).not.toBe(logs[1]);
  });

  test("GITHUB_ACTION is sanitised before it reaches a path", async () => {
    const f = fakeCore();
    const d = deps(f, {
      startServer: vi.fn(async (_b: string, args: string[]) => {
        await writeFile(args[args.indexOf("--token-file") + 1] ?? "", "t\n");
        return 1;
      }),
    });
    d.env.GITHUB_ACTION = "../../evil step";
    await runMain(d);
    expect(vi.mocked(d.startServer).mock.calls[0]?.[2]).toBe(
      join(dir, "hoard-______evil_step.log"),
    );
  });

  test("a missing token file fails the step", async () => {
    const f = fakeCore();
    const d = deps(f, { startServer: vi.fn(async () => 1) });
    await runMain(d);
    expect(f.of("setFailed")).toEqual([[expect.stringContaining("hoard-__keelstack_hoard.token")]]);
    expect(f.of("exportVariable")).toEqual([]);
  });
});

describe("post", () => {
  const summaryText = "### hoard\n| Hits | Misses |\n|---|---|\n| 1 | 0 |\n";

  async function state(over: Record<string, string> = {}) {
    const summary = join(dir, "hoard-summary.md");
    const log = join(dir, "hoard.log");
    await writeFile(log, "hoard log line\n");
    return {
      port: "40404",
      token: "tok",
      pid: "1",
      summary,
      log,
      warnings: JSON.stringify(["R2 secrets are not available (missing: r2-bucket)"]),
      ...over,
    };
  }

  function deps(f: ReturnType<typeof fakeCore>, over: Partial<PostDeps> = {}): PostDeps {
    return {
      core: f.core,
      env: {},
      getStats: vi.fn(async () => ({ hits: 3, misses: 1, time_saved_ms: 900 })),
      shutdownServer: vi.fn(async () => true),
      kill: vi.fn(),
      ...over,
    };
  }

  test("sets outputs, shuts down, appends summary and warnings", async () => {
    const s = await state();
    await writeFile(s.summary, summaryText);
    const f = fakeCore({}, s);
    const d = deps(f);
    await runPost(d);

    expect(d.getStats).toHaveBeenCalledWith(40404, "tok");
    expect(d.shutdownServer).toHaveBeenCalledWith(40404, "tok");
    expect(f.of("setOutput")).toEqual([
      ["hits", "3"],
      ["misses", "1"],
      ["time-saved-ms", "900"],
    ]);
    expect(f.summaries).toEqual([
      `${summaryText}\n> ⚠️ R2 secrets are not available (missing: r2-bucket)\n`,
    ]);
    expect(f.of("warning")).toEqual([]);
    expect(f.of("startGroup")).toEqual([]); // log only on problems or RUNNER_DEBUG
  });

  test("RUNNER_DEBUG=1 prints the log", async () => {
    const s = await state({ warnings: "[]" });
    await writeFile(s.summary, summaryText);
    const f = fakeCore({}, s);
    await runPost({ ...deps(f), env: { RUNNER_DEBUG: "1" } });
    expect(f.of("startGroup")).toEqual([["hoard log"]]);
    const [[open], [content], [close]] = f.of("info") as [[string], [string], [string]];
    expect(content).toBe("hoard log line\n");
    const t = /^::stop-commands::([0-9a-f]{32})$/.exec(open)?.[1];
    expect(t).toBeTruthy();
    expect(close).toBe(`::${t}::`);
    expect(f.of("info")).toHaveLength(3);
  });

  test("a server that already died: warns, still writes the summary, never throws", async () => {
    const s = await state({ port: "1" }); // nothing listens on port 1
    await writeFile(s.summary, summaryText);
    const f = fakeCore({}, s);
    await expect(runPost(deps(f, { getStats, shutdownServer }))).resolves.toBeUndefined();

    expect(f.of("warning")).toEqual([
      ["hoard server was not running at shutdown; the summary may be incomplete"],
    ]);
    expect(f.of("setOutput")).toEqual([
      ["hits", "0"],
      ["misses", "0"],
      ["time-saved-ms", "0"],
    ]);
    expect(f.summaries).toHaveLength(1);
    expect(f.summaries[0]).toContain("### hoard");
    expect(f.summaries[0]).toContain(
      "> ⚠️ hoard server was not running at shutdown; the summary may be incomplete",
    );
    expect(f.of("startGroup")).toEqual([["hoard log"]]);
  });

  test("stats unreachable but shutdown ok: outputs 0 and a warning", async () => {
    const s = await state({ warnings: "[]" });
    const f = fakeCore({}, s);
    await runPost(deps(f, { getStats: vi.fn(async () => undefined) }));
    expect(f.of("setOutput")).toContainEqual(["hits", "0"]);
    expect(f.of("warning")).toEqual([
      ["could not read hoard stats; the hits, misses and time-saved-ms outputs are 0"],
    ]);
  });

  test("no summary file and no warnings: nothing is appended", async () => {
    const s = await state({ warnings: "[]" });
    const f = fakeCore({}, s);
    await runPost(deps(f));
    expect(f.summaries).toEqual([]);
    expect(f.of("warning")).toEqual([]);
  });

  test("a failing summary write is a warning, not an error", async () => {
    const s = await state();
    await writeFile(s.summary, summaryText);
    const f = fakeCore({}, s);
    f.core.summary = {
      addRaw: () => ({
        write: async () => {
          throw new Error("Unable to find environment variable for $GITHUB_STEP_SUMMARY");
        },
      }),
    };
    await expect(runPost(deps(f))).resolves.toBeUndefined();
    expect(f.of("warning")).toEqual([
      [expect.stringContaining("could not write the hoard job summary")],
    ]);
  });

  test("without state (main never started hoard) post does nothing", async () => {
    const f = fakeCore();
    const d = deps(f);
    await runPost(d);
    expect(f.calls).toEqual([]);
    expect(f.summaries).toEqual([]);
    expect(d.shutdownServer).not.toHaveBeenCalled();
  });

  test("the token is masked again before any output", async () => {
    const s = await state();
    await writeFile(s.summary, summaryText);
    const f = fakeCore({}, s);
    await runPost(deps(f));
    expect(f.calls[0]).toEqual(["setSecret", "tok"]);
  });

  test("a refused shutdown kills the saved pid", async () => {
    const s = await state({ pid: "4321" });
    const f = fakeCore({}, s);
    const d = deps(f, { shutdownServer: vi.fn(async () => false) });
    await runPost(d);
    expect(d.kill).toHaveBeenCalledWith(4321);
  });

  test("a clean shutdown kills nothing", async () => {
    const s = await state({ pid: "4321" });
    const f = fakeCore({}, s);
    const d = deps(f);
    await runPost(d);
    expect(d.kill).not.toHaveBeenCalled();
  });
});
