import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import {
  getStats,
  killServer,
  serveArgs,
  serveEnv,
  shutdownServer,
  startServer,
  waitForHealth,
} from "../src/server.js";
import type { R2Settings, Settings } from "../src/settings.js";

const baseSettings: Settings = {
  storage: "github",
  mode: "read-write",
  noReads: false,
  cacheVersion: "v1",
  r2Lifecycle: "auto",
  maxAgeDays: 7,
  maxSize: "5GB",
  version: "0.0.0-dev",
  warnings: [],
};
const r2Fixture: R2Settings = {
  accountId: "acct-123",
  accessKeyId: "AKIDSECRETVALUE",
  secretAccessKey: "s3cr3t-value-do-not-print",
  bucket: "b",
  prefix: "cache/",
  endpoint: "",
};

let dir: string;
let server: Server | undefined;
beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), "hoard-server-"));
});
afterEach(async () => {
  await new Promise((r) => (server ? server.close(r) : r(undefined)));
  server = undefined;
  await rm(dir, { recursive: true, force: true });
});

/** A local HTTP server standing in for hoard; returns its port. */
async function fakeHoard(handler: Parameters<typeof createServer>[1]): Promise<number> {
  const s = createServer(handler);
  server = s;
  await new Promise<void>((r) => s.listen(0, "127.0.0.1", r));
  return (s.address() as AddressInfo).port;
}

describe("serveArgs", () => {
  test("serveArgs for r2", () => {
    const a = serveArgs({ ...baseSettings, storage: "r2", r2: r2Fixture }, "/tmp/rt");
    expect(a).toEqual(
      expect.arrayContaining([
        "serve",
        "--storage",
        "r2",
        "--r2-bucket",
        "b",
        "--port",
        "0",
        "--port-file",
        "/tmp/rt/hoard.port",
      ]),
    );
    expect(a.join(" ")).not.toContain(r2Fixture.secretAccessKey); // secrets go via env, not argv
    expect(a.join(" ")).not.toContain(r2Fixture.accessKeyId);
  });

  test("full argv for github storage", () => {
    expect(serveArgs({ ...baseSettings, mode: "read", noReads: true }, "/tmp/rt")).toEqual([
      "serve",
      "--storage",
      "github",
      "--mode",
      "read",
      "--no-reads",
      "--cache-version",
      "v1",
      "--port",
      "0",
      "--port-file",
      "/tmp/rt/hoard.port",
      "--token-file",
      "/tmp/rt/hoard.token",
      "--summary-file",
      "/tmp/rt/hoard-summary.md",
      "--max-age-days",
      "7",
      "--max-size",
      "5GB",
    ]);
  });

  test("fs storage gets its store in the step's files", () => {
    const a = serveArgs({ ...baseSettings, storage: "fs" }, "/rt", "step_2");
    expect(a.slice(a.indexOf("--port-file"), a.indexOf("--max-age-days"))).toEqual([
      "--port-file",
      "/rt/hoard-step_2.port",
      "--token-file",
      "/rt/hoard-step_2.token",
      "--summary-file",
      "/rt/hoard-step_2-summary.md",
      "--fs-root",
      "/rt/hoard-step_2-store",
    ]);
  });

  test("r2 flags: account id or endpoint, prefix and lifecycle", () => {
    const a = serveArgs(
      {
        ...baseSettings,
        storage: "r2",
        r2: { ...r2Fixture, accountId: "", endpoint: "http://127.0.0.1:9000", prefix: "t/" },
        r2Lifecycle: "verify",
      },
      "/rt",
    );
    expect(a.slice(a.indexOf("--max-size") + 2)).toEqual([
      "--r2-bucket",
      "b",
      "--r2-prefix",
      "t/",
      "--r2-lifecycle",
      "verify",
      "--r2-endpoint",
      "http://127.0.0.1:9000",
    ]);
    expect(a).not.toContain("--r2-account-id");
  });
});

describe("serveEnv", () => {
  test("carries the R2 keys and drops an inherited HOARD_TOKEN", () => {
    const env = serveEnv(
      { ...baseSettings, storage: "r2", r2: r2Fixture },
      { PATH: "/bin", ACTIONS_RESULTS_URL: "https://results", HOARD_TOKEN: "known" },
    );
    expect(env).toEqual({
      PATH: "/bin",
      ACTIONS_RESULTS_URL: "https://results",
      HOARD_R2_ACCESS_KEY_ID: "AKIDSECRETVALUE",
      HOARD_R2_SECRET_ACCESS_KEY: "s3cr3t-value-do-not-print",
    });
  });

  test("github storage passes the environment through, without HOARD_TOKEN", () => {
    const base = { ACTIONS_RUNTIME_TOKEN: "rt", HOARD_TOKEN: "known" };
    expect(serveEnv(baseSettings, base)).toEqual({ ACTIONS_RUNTIME_TOKEN: "rt" });
    expect(base.HOARD_TOKEN).toBe("known"); // the caller's env is not mutated
  });
});

describe("waitForHealth", () => {
  test("waitForHealth times out with a clear error", async () => {
    await expect(waitForHealth("/nonexistent/port", 300)).rejects.toThrow("hoard did not start");
  });

  test("the default timeout message says 10s", async () => {
    const started = Date.now();
    const err = await waitForHealth("/nonexistent/port").catch((e: Error) => e);
    expect(Date.now() - started).toBeGreaterThanOrEqual(9_900);
    expect((err as Error).message).toBe("hoard did not start within 10s");
  }, 15_000);

  test("returns the port once /_hoard/health answers ok", async () => {
    const port = await fakeHoard((req, res) => {
      res.end(req.url === "/_hoard/health" ? "ok" : "no");
    });
    const portFile = join(dir, "hoard.port");
    setTimeout(() => void writeFile(portFile, `${port}\n`), 250);
    expect(await waitForHealth(portFile, 3_000)).toBe(port);
  });

  test("a port file without a healthy server is not enough", async () => {
    const port = await fakeHoard((_req, res) => {
      res.statusCode = 503;
      res.end();
    });
    const portFile = join(dir, "hoard.port");
    await writeFile(portFile, String(port));
    await expect(waitForHealth(portFile, 300)).rejects.toThrow("hoard did not start within 300ms");
  });
});

describe("startServer", () => {
  test("a binary that crashes at start never becomes healthy; its output is in the log", async () => {
    const script = join(dir, "crash.mjs");
    await writeFile(script, 'console.error("boom: bad config"); process.exit(3);\n');
    const log = join(dir, "hoard.log");
    await startServer(process.execPath, [script], log, process.env);
    await expect(waitForHealth(join(dir, "hoard.port"), 300)).rejects.toThrow(
      "hoard did not start",
    );
    expect(await readFile(log, "utf8")).toContain("boom: bad config");
  });

  test("passes the given env and args to the child and returns its pid", async () => {
    const script = join(dir, "env.mjs");
    await writeFile(
      script,
      "console.log(JSON.stringify([process.argv.slice(2), process.env.HOARD_X ?? null]));\n",
    );
    const log = join(dir, "hoard.log");
    const pid = await startServer(process.execPath, [script, "serve", "--a"], log, {
      PATH: process.env.PATH,
      HOARD_X: "y",
    });
    expect(pid).toBeGreaterThan(0);
    await expect.poll(() => readFile(log, "utf8"), { timeout: 3_000 }).toContain('"y"');
    expect(JSON.parse(await readFile(log, "utf8"))).toEqual([["serve", "--a"], "y"]);
  });

  test("a missing binary rejects", async () => {
    await expect(
      startServer(join(dir, "no-such-hoard"), [], join(dir, "hoard.log"), process.env),
    ).rejects.toThrow(/could not start hoard/);
  });
});

describe("shutdownServer and getStats", () => {
  test("shutdownServer returns false when nothing listens", async () => {
    expect(await shutdownServer(1, "t")).toBe(false);
  });

  test("shutdownServer posts with the bearer token", async () => {
    const seen: string[] = [];
    const port = await fakeHoard((req, res) => {
      seen.push(`${req.method} ${req.url} ${req.headers.authorization}`);
      res.end("ok");
    });
    expect(await shutdownServer(port, "tok")).toBe(true);
    expect(seen).toEqual(["POST /_hoard/shutdown Bearer tok"]);
  });

  test("shutdownServer returns false on a non-2xx answer", async () => {
    const port = await fakeHoard((_req, res) => {
      res.statusCode = 401;
      res.end();
    });
    expect(await shutdownServer(port, "wrong")).toBe(false);
  });

  test("getStats reads /_hoard/stats with the bearer token", async () => {
    const port = await fakeHoard((req, res) => {
      if (req.url !== "/_hoard/stats" || req.headers.authorization !== "Bearer tok") {
        res.statusCode = 401;
        return res.end();
      }
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ hits: 4, misses: 2, puts: 1, time_saved_ms: 1234 }));
    });
    expect(await getStats(port, "tok")).toMatchObject({ hits: 4, misses: 2, time_saved_ms: 1234 });
    expect(await getStats(port, "bad")).toBeUndefined();
  });

  test("getStats is undefined when nothing listens", async () => {
    expect(await getStats(1, "t")).toBeUndefined();
  });
});

describe("killServer", () => {
  test("ignores pids it must never signal and processes that are gone", () => {
    expect(() => {
      killServer(0);
      killServer(1);
      killServer(Number.NaN);
      killServer(2 ** 22 + 12345); // above Linux pid_max default: no such process
    }).not.toThrow();
  });
});
