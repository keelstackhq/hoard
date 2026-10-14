import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import * as tc from "@actions/tool-cache";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { BinaryDownloadError, ensureBinary, releaseUrl } from "../src/binary.js";
import { TARGETS, type Target } from "../src/platform.js";

const CHECKSUMS_ZERO = Object.fromEntries(TARGETS.map((t) => [t, "0".repeat(64)])) as Record<
  Target,
  string
>;
const LINUX: Target = "x86_64-unknown-linux-musl";

let dir: string;
const saved = { cache: process.env.RUNNER_TOOL_CACHE, temp: process.env.RUNNER_TEMP };

beforeEach(async () => {
  // @actions/tool-cache logs ::debug:: and [command] lines to stdout; keep test output clean.
  const write = process.stdout.write.bind(process.stdout);
  vi.spyOn(process.stdout, "write").mockImplementation((chunk, ...rest) =>
    /^(::debug::|\[command\])/.test(String(chunk)) ? true : write(chunk, ...(rest as [])),
  );
  dir = await mkdtemp(join(tmpdir(), "hoard-binary-"));
  process.env.RUNNER_TOOL_CACHE = join(dir, "toolcache");
  process.env.RUNNER_TEMP = join(dir, "runner-temp");
  await mkdir(process.env.RUNNER_TOOL_CACHE);
  await mkdir(process.env.RUNNER_TEMP);
});

afterEach(async () => {
  vi.restoreAllMocks();
  process.env.RUNNER_TOOL_CACHE = saved.cache;
  process.env.RUNNER_TEMP = saved.temp;
  await rm(dir, { recursive: true, force: true });
});

let archives = 0;
/** Builds a real hoard-<target>.tar.gz holding one file `name` with `body`; returns its path. */
async function writeTempTarGz(body: string, name = "hoard"): Promise<string> {
  const src = join(dir, `src-${++archives}`);
  await mkdir(src);
  await writeFile(join(src, name), body);
  const out = join(dir, `archive-${archives}.tar.gz`);
  execFileSync("tar", ["-czf", out, "-C", src, name]);
  return out;
}

async function sha256(file: string): Promise<string> {
  return createHash("sha256")
    .update(await readFile(file))
    .digest("hex");
}

test("checksum mismatch throws", async () => {
  const fakeDownload = async () => writeTempTarGz("not the real binary");
  await expect(ensureBinary(LINUX, "1.0.0", { ...CHECKSUMS_ZERO }, fakeDownload)).rejects.toThrow(
    "checksum mismatch for x86_64-unknown-linux-musl",
  );
});

test("checksum mismatch does not extract or cache anything", async () => {
  const fakeDownload = async () => writeTempTarGz("not the real binary");
  await expect(ensureBinary(LINUX, "1.0.0", CHECKSUMS_ZERO, fakeDownload)).rejects.toThrow();
  expect(tc.find("hoard", "1.0.0", LINUX)).toBe("");
});

test("a missing checksum refuses before any download", async () => {
  const download = vi.fn(async () => writeTempTarGz("x"));
  await expect(ensureBinary(LINUX, "0.0.0-dev", {}, download)).rejects.toThrow(
    "no verified hoard binary for x86_64-unknown-linux-musl in this action build (0.0.0-dev)",
  );
  expect(download).not.toHaveBeenCalled();
});

const HOSTED = { RUNNER_ENVIRONMENT: "github-hosted" };

/** A download that hands out a fresh copy of `archive` each call (ensureBinary deletes it). */
function downloadOf(archive: string) {
  let n = 0;
  return vi.fn(async (_url: string) => {
    const copy = `${archive}.dl-${++n}`;
    await copyFile(archive, copy);
    downloaded.push(copy);
    return copy;
  });
}
const downloaded: string[] = [];

test("a verified archive is extracted, made executable and cached", async () => {
  const archive = await writeTempTarGz("#!/bin/sh\necho hoard\n");
  const download = downloadOf(archive);
  const sum = (await sha256(archive)).toUpperCase(); // comparison is case-insensitive
  const exe = await ensureBinary(LINUX, "1.2.3", { [LINUX]: sum }, download, HOSTED);

  expect(basename(exe)).toBe("hoard");
  expect(await readFile(exe, "utf8")).toBe("#!/bin/sh\necho hoard\n");
  expect((await stat(exe)).mode & 0o777).toBe(0o755);
  expect(download).toHaveBeenCalledOnce();
  expect(download).toHaveBeenCalledWith(
    "https://github.com/keelstackhq/hoard/releases/download/v1.2.3/hoard-x86_64-unknown-linux-musl.tar.gz",
  );
  expect(tc.find("hoard", "1.2.3", LINUX)).not.toBe("");

  // Second run on a GitHub-hosted runner: the tool cache short-circuits the download.
  const again = await ensureBinary(LINUX, "1.2.3", { [LINUX]: sum }, download, HOSTED);
  expect(again).toBe(exe);
  expect(download).toHaveBeenCalledOnce();
});

test("the downloaded archive is deleted, after success and after a mismatch", async () => {
  const archive = await writeTempTarGz("bin");
  const download = downloadOf(archive);
  await ensureBinary(LINUX, "1.2.3", { [LINUX]: await sha256(archive) }, download, {});
  await expect(ensureBinary(LINUX, "1.2.4", CHECKSUMS_ZERO, download, {})).rejects.toThrow(
    "checksum mismatch",
  );
  expect(downloaded.slice(-2)).toHaveLength(2);
  for (const file of downloaded.slice(-2)) await expect(stat(file)).rejects.toThrow(/ENOENT/);
});

test("a self-hosted runner ignores the tool cache and verifies a fresh download", async () => {
  // A tool-cache entry on a self-hosted runner may have been planted; it is never trusted.
  const src = join(dir, "pre");
  await mkdir(src);
  await writeFile(join(src, "hoard"), "planted");
  await tc.cacheDir(src, "hoard", "1.2.3", LINUX);
  const archive = await writeTempTarGz("real");
  const download = downloadOf(archive);

  for (const env of [{}, { RUNNER_ENVIRONMENT: "self-hosted" }]) {
    const exe = await ensureBinary(
      LINUX,
      "1.2.3",
      { [LINUX]: await sha256(archive) },
      download,
      env,
    );
    expect(await readFile(exe, "utf8")).toBe("real");
  }
  expect(download).toHaveBeenCalledTimes(2);
  await expect(ensureBinary(LINUX, "1.2.3", CHECKSUMS_ZERO, download, {})).rejects.toThrow(
    "checksum mismatch",
  );
});

test("on a GitHub-hosted runner a cached tool short-circuits without downloading", async () => {
  const src = join(dir, "pre");
  await mkdir(src);
  await writeFile(join(src, "hoard"), "cached");
  await tc.cacheDir(src, "hoard", "1.2.3", LINUX);
  const download = vi.fn(async () => writeTempTarGz("x"));

  const exe = await ensureBinary(LINUX, "1.2.3", CHECKSUMS_ZERO, download, HOSTED);
  expect(await readFile(exe, "utf8")).toBe("cached");
  expect(download).not.toHaveBeenCalled();
});

test("windows archives hold hoard.exe", async () => {
  const win: Target = "x86_64-pc-windows-msvc";
  const archive = await writeTempTarGz("MZ", "hoard.exe");
  const exe = await ensureBinary(
    win,
    "1.2.3",
    { [win]: await sha256(archive) },
    async () => archive,
    {},
  );
  expect(basename(exe)).toBe("hoard.exe");
});

test("an archive without the executable throws", async () => {
  const archive = await writeTempTarGz("nope", "README");
  await expect(
    ensureBinary(LINUX, "1.2.3", { [LINUX]: await sha256(archive) }, async () => archive, {}),
  ).rejects.toThrow("hoard-x86_64-unknown-linux-musl.tar.gz does not contain hoard");
});

describe("releaseUrl", () => {
  test("uses GITHUB_ACTION_REPOSITORY", () => {
    expect(releaseUrl(LINUX, "1.2.3", { GITHUB_ACTION_REPOSITORY: "me/hoard-fork" })).toBe(
      "https://github.com/me/hoard-fork/releases/download/v1.2.3/hoard-x86_64-unknown-linux-musl.tar.gz",
    );
  });
  test.each(["", "nope", "a/b/c", "../evil", "me/..", "a/b?x=1", "owner/re po"])(
    "falls back to keelstackhq/hoard for %j",
    (repo) => {
      expect(releaseUrl(LINUX, "1.2.3", { GITHUB_ACTION_REPOSITORY: repo })).toBe(
        "https://github.com/keelstackhq/hoard/releases/download/v1.2.3/hoard-x86_64-unknown-linux-musl.tar.gz",
      );
    },
  );
  test("ensureBinary downloads from GITHUB_ACTION_REPOSITORY", async () => {
    const archive = await writeTempTarGz("bin");
    const download = vi.fn(async (_url: string) => archive);
    await ensureBinary(LINUX, "1.2.3", { [LINUX]: await sha256(archive) }, download, {
      GITHUB_ACTION_REPOSITORY: "me/hoard-fork",
    });
    expect(download.mock.calls[0]?.[0]).toMatch(/^https:\/\/github\.com\/me\/hoard-fork\//);
  });
});

describe("download retries", () => {
  /** Instant waits; records each retry delay. */
  function fastOpts(sleeps: number[], timeoutMs = 90_000) {
    return { sleep: async (ms: number) => void sleeps.push(ms), timeoutMs };
  }

  test("a failing download is retried once, after 2 s", async () => {
    const archive = await writeTempTarGz("bin");
    const good = downloadOf(archive);
    let calls = 0;
    const download = vi.fn(async (url: string) => {
      if (++calls === 1) throw new Error("HTTP 502");
      return good(url);
    });
    const sleeps: number[] = [];
    const exe = await ensureBinary(
      LINUX,
      "2.0.0",
      { [LINUX]: await sha256(archive) },
      download,
      {},
      fastOpts(sleeps),
    );
    expect(basename(exe)).toBe("hoard");
    expect(download).toHaveBeenCalledTimes(2);
    expect(sleeps).toEqual([2_000]);
  });

  test("two failures are a BinaryDownloadError naming the last cause", async () => {
    const download = vi.fn(async () => {
      throw new Error("connect ETIMEDOUT");
    });
    const sleeps: number[] = [];
    const err = await ensureBinary(
      LINUX,
      "2.0.0",
      CHECKSUMS_ZERO,
      download,
      {},
      fastOpts(sleeps),
    ).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(BinaryDownloadError);
    expect((err as Error).message).toBe("connect ETIMEDOUT");
    expect(download).toHaveBeenCalledTimes(2);
    expect(sleeps).toEqual([2_000]);
  });

  test("an HTTP 4xx is not retried, and is still a download error", async () => {
    const download = vi.fn(async () => {
      throw new tc.HTTPError(404);
    });
    const sleeps: number[] = [];
    const err = await ensureBinary(
      LINUX,
      "2.0.0",
      CHECKSUMS_ZERO,
      download,
      {},
      fastOpts(sleeps),
    ).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(BinaryDownloadError);
    expect((err as Error).message).toContain("404");
    expect(download).toHaveBeenCalledOnce();
    expect(sleeps).toEqual([]);
  });

  test("a download that never finishes is a BinaryDownloadError at the cap", async () => {
    const download = vi.fn(() => new Promise<string>(() => {}));
    const started = Date.now();
    const err = await ensureBinary(
      LINUX,
      "2.0.0",
      CHECKSUMS_ZERO,
      download,
      {},
      {
        timeoutMs: 200,
      },
    ).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(BinaryDownloadError);
    expect((err as Error).message).toBe("no answer within 200ms");
    expect(Date.now() - started).toBeLessThan(1_500);
  });

  test("a checksum mismatch is not retried and is not a download error", async () => {
    const archive = await writeTempTarGz("not the real binary");
    const download = downloadOf(archive);
    const sleeps: number[] = [];
    const err = await ensureBinary(
      LINUX,
      "2.0.1",
      CHECKSUMS_ZERO,
      download,
      {},
      fastOpts(sleeps),
    ).catch((e: unknown) => e);
    expect((err as Error).message).toBe("checksum mismatch for x86_64-unknown-linux-musl");
    expect(err).not.toBeInstanceOf(BinaryDownloadError);
    expect(download).toHaveBeenCalledOnce();
    expect(sleeps).toEqual([]);
  });
});
