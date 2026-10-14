// Starts, probes and stops the detached `hoard serve` process (spec §10 main 3–4, post).
import { spawn } from "node:child_process";
import { closeSync, openSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import type { Settings } from "./settings.js";

/** Files hoard serve writes inside `$RUNNER_TEMP`. */
export function serverFiles(runnerTemp: string) {
  return {
    port: join(runnerTemp, "hoard.port"),
    token: join(runnerTemp, "hoard.token"),
    summary: join(runnerTemp, "hoard-summary.md"),
    log: join(runnerTemp, "hoard.log"),
  };
}

/** argv for `hoard serve`. Never contains a secret: the R2 keys go through `serveEnv`. */
export function serveArgs(s: Settings, runnerTemp: string): string[] {
  const files = serverFiles(runnerTemp);
  const args = [
    "serve",
    "--storage",
    s.storage,
    "--mode",
    s.mode,
    ...(s.noReads ? ["--no-reads"] : []),
    "--cache-version",
    s.cacheVersion,
    "--port",
    "0",
    "--port-file",
    files.port,
    "--token-file",
    files.token,
    "--summary-file",
    files.summary,
    "--max-age-days",
    String(s.maxAgeDays),
    "--max-size",
    s.maxSize,
  ];
  if (s.r2) {
    args.push("--r2-bucket", s.r2.bucket, "--r2-prefix", s.r2.prefix);
    args.push("--r2-lifecycle", s.r2Lifecycle);
    if (s.r2.accountId) args.push("--r2-account-id", s.r2.accountId);
    if (s.r2.endpoint) args.push("--r2-endpoint", s.r2.endpoint);
  }
  return args;
}

/**
 * The child's environment: the full runner env (the GitHub cache storage needs it, §6.4) plus
 * the R2 keys. An inherited HOARD_TOKEN is dropped so nothing outside can choose the token.
 */
export function serveEnv(s: Settings, baseEnv: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...baseEnv };
  delete env.HOARD_TOKEN;
  if (s.r2) {
    env.HOARD_R2_ACCESS_KEY_ID = s.r2.accessKeyId;
    env.HOARD_R2_SECRET_ACCESS_KEY = s.r2.secretAccessKey;
  }
  return env;
}

/** Spawns hoard detached with stdout and stderr in `logPath`; resolves with its pid. */
export async function startServer(
  bin: string,
  args: string[],
  logPath: string,
  env: NodeJS.ProcessEnv = process.env,
): Promise<number> {
  const logFd = openSync(logPath, "w");
  try {
    const child = spawn(bin, args, {
      detached: true,
      stdio: ["ignore", logFd, logFd],
      env,
      windowsHide: true,
    });
    await new Promise<void>((resolve, reject) => {
      child.once("spawn", resolve);
      child.once("error", (e) => reject(new Error(`could not start hoard (${bin}): ${e.message}`)));
    });
    child.unref();
    return child.pid ?? 0;
  } finally {
    closeSync(logFd);
  }
}

function startTimeoutText(ms: number): string {
  return ms % 1000 === 0 ? `${ms / 1000}s` : `${ms}ms`;
}

/**
 * Polls every 100 ms until hoard has written its port file and answers `/_hoard/health`;
 * resolves with the port.
 */
export async function waitForHealth(portFile: string, timeoutMs = 10_000): Promise<number> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const port = Number((await readFile(portFile, "utf8").catch(() => "")).trim());
    if (Number.isInteger(port) && port > 0) {
      const left = deadline - Date.now();
      const ok = await fetch(`http://127.0.0.1:${port}/_hoard/health`, {
        signal: AbortSignal.timeout(Math.max(1, Math.min(1_000, left))),
      })
        .then(async (r) => r.ok && (await r.text()).trim() === "ok")
        .catch(() => false);
      if (ok) return port;
    }
    await sleep(Math.max(0, Math.min(100, deadline - Date.now())));
  }
  throw new Error(`hoard did not start within ${startTimeoutText(timeoutMs)}`);
}

/** The counters of `GET /_hoard/stats` the action uses (§9); undefined if unreachable. */
export interface Stats {
  hits: number;
  misses: number;
  time_saved_ms: number;
}

export async function getStats(port: number, token: string): Promise<Stats | undefined> {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/_hoard/stats`, {
      headers: { authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(5_000),
    });
    return r.ok ? ((await r.json()) as Stats) : undefined;
  } catch {
    return undefined;
  }
}

/**
 * `POST /_hoard/shutdown`: hoard flushes, prunes (R2) and writes the summary before it answers.
 * False if hoard is unreachable or refuses.
 */
export async function shutdownServer(port: number, token: string): Promise<boolean> {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/_hoard/shutdown`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}` },
      // The R2 size-cap prune runs before the answer; give it time.
      signal: AbortSignal.timeout(300_000),
    });
    await r.body?.cancel();
    return r.ok;
  } catch {
    return false;
  }
}
