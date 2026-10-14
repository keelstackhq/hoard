// Downloads, verifies and caches the hoard binary (spec §10). Never runs a binary whose
// SHA-256 is not baked into this action build.
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { access, chmod, rm } from "node:fs/promises";
import { join } from "node:path";
import { pipeline } from "node:stream/promises";
import { setTimeout as delay } from "node:timers/promises";
import * as tc from "@actions/tool-cache";
import type { Target } from "./platform.js";

const DEFAULT_REPO = "keelstackhq/hoard";
// owner/repo; "." and ".." segments would change the URL path, so they are refused too.
const REPO_RE = /^[\w.-]+\/[\w.-]+$/;
const isRepo = (s: string) => REPO_RE.test(s) && s.split("/").every((p) => p !== "." && p !== "..");

/** Release asset URL, from the action's own repository when GitHub names it. */
export function releaseUrl(target: Target, version: string, env: NodeJS.ProcessEnv): string {
  const fromEnv = env.GITHUB_ACTION_REPOSITORY ?? "";
  const repo = isRepo(fromEnv) ? fromEnv : DEFAULT_REPO;
  return `https://github.com/${repo}/releases/download/v${version}/hoard-${target}.tar.gz`;
}

/**
 * The release archive could not be downloaded, even after retries. Transient (network, a
 * github.com outage), so main goes on without the cache instead of failing the build (§10).
 * A checksum mismatch or a missing checksum is never this error.
 */
export class BinaryDownloadError extends Error {
  override name = "BinaryDownloadError";
}

/** Waits before the 2nd, 3rd and 4th download attempt. */
const RETRY_DELAYS_MS = [1_000, 2_000, 4_000];

async function downloadWithRetry(
  url: string,
  download: (url: string) => Promise<string>,
  sleep: (ms: number) => Promise<unknown>,
): Promise<string> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await download(url);
    } catch (e) {
      const wait = RETRY_DELAYS_MS[attempt];
      if (wait === undefined) {
        throw new BinaryDownloadError(e instanceof Error ? e.message : String(e), { cause: e });
      }
      await sleep(wait);
    }
  }
}

async function sha256(file: string): Promise<string> {
  const hash = createHash("sha256");
  await pipeline(createReadStream(file), hash);
  return hash.digest("hex");
}

/**
 * Returns the path of a verified hoard executable for `target`. The tool cache is keyed by the
 * target (used as the tool-cache "arch"); an entry there was verified before it was stored, and
 * it is only reused on GitHub-hosted runners.
 */
export async function ensureBinary(
  target: Target,
  version: string,
  checksums: Partial<Record<Target, string>>,
  download: (url: string) => Promise<string> = (url) => tc.downloadTool(url),
  env: NodeJS.ProcessEnv = process.env,
  sleep: (ms: number) => Promise<unknown> = (ms) => delay(ms),
): Promise<string> {
  const expected = checksums[target];
  if (!expected) {
    throw new Error(`no verified hoard binary for ${target} in this action build (${version})`);
  }
  const exeName = target.endsWith("-windows-msvc") ? "hoard.exe" : "hoard";

  // Only a GitHub-hosted runner's tool cache is fresh per job; on a self-hosted runner it
  // persists and anything could have been put there, so the binary is downloaded and verified.
  if (env.RUNNER_ENVIRONMENT === "github-hosted") {
    const cached = tc.find("hoard", version, target);
    if (cached) return join(cached, exeName);
  }

  const archive = await downloadWithRetry(releaseUrl(target, version, env), download, sleep);
  try {
    if ((await sha256(archive)) !== expected.toLowerCase()) {
      throw new Error(`checksum mismatch for ${target}`);
    }

    const dir = await tc.extractTar(archive);
    const exe = join(dir, exeName);
    try {
      await access(exe);
    } catch {
      throw new Error(`hoard-${target}.tar.gz does not contain ${exeName}`);
    }
    if (exeName === "hoard") await chmod(exe, 0o755);
    const toolDir = await tc.cacheDir(dir, "hoard", version, target);
    return join(toolDir, exeName);
  } finally {
    await rm(archive, { force: true });
  }
}
