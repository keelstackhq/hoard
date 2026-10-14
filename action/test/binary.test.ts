import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import * as tc from "@actions/tool-cache";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { ensureBinary, releaseUrl } from "../src/binary.js";
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

test("a verified archive is extracted, made executable and cached", async () => {
  const archive = await writeTempTarGz("#!/bin/sh\necho hoard\n");
  const download = vi.fn(async (_url: string) => archive);
  const sum = (await sha256(archive)).toUpperCase(); // comparison is case-insensitive
  const exe = await ensureBinary(LINUX, "1.2.3", { [LINUX]: sum }, download, {});

  expect(basename(exe)).toBe("hoard");
  expect(await readFile(exe, "utf8")).toBe("#!/bin/sh\necho hoard\n");
  expect((await stat(exe)).mode & 0o777).toBe(0o755);
  expect(download).toHaveBeenCalledOnce();
  expect(download).toHaveBeenCalledWith(
    "https://github.com/keelstackhq/hoard/releases/download/v1.2.3/hoard-x86_64-unknown-linux-musl.tar.gz",
  );
  expect(tc.find("hoard", "1.2.3", LINUX)).not.toBe("");

  // Second run: the tool cache short-circuits the download.
  const again = await ensureBinary(LINUX, "1.2.3", { [LINUX]: sum }, download, {});
  expect(again).toBe(exe);
  expect(download).toHaveBeenCalledOnce();
});

test("a cached tool short-circuits without downloading", async () => {
  const src = join(dir, "pre");
  await mkdir(src);
  await writeFile(join(src, "hoard"), "cached");
  await tc.cacheDir(src, "hoard", "1.2.3", LINUX);
  const download = vi.fn(async () => writeTempTarGz("x"));

  const exe = await ensureBinary(LINUX, "1.2.3", CHECKSUMS_ZERO, download, {});
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
