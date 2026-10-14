// The main and post steps (spec §10). Dependencies are injected so the orchestration is tested
// without a runner; src/main.ts and src/post.ts call these with the real ones.
import { randomBytes } from "node:crypto";
import { readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import * as actionsCore from "@actions/core";
import { BinaryDownloadError, ensureBinary } from "./binary.js";
import { CHECKSUMS } from "./checksums.generated.js";
import { type Target, targetFor } from "./platform.js";
import {
  getStats,
  killServer,
  type ServerProcess,
  serveArgs,
  serveEnv,
  serverFiles,
  shutdownServer,
  startServer,
  stepId,
  waitForHealth,
} from "./server.js";
import { resolveSettings } from "./settings.js";

/** The part of @actions/core the action uses. */
export interface ActionCore {
  getInput(name: string): string;
  getState(name: string): string;
  setSecret(secret: string): void;
  exportVariable(name: string, value: string): void;
  saveState(name: string, value: string): void;
  setOutput(name: string, value: string): void;
  setFailed(message: string): void;
  warning(message: string): void;
  info(message: string): void;
  startGroup(name: string): void;
  endGroup(): void;
  summary: { addRaw(text: string): { write(): Promise<unknown> } };
}

const INPUTS = [
  "storage",
  "mode",
  "cache-version",
  "r2-account-id",
  "r2-access-key-id",
  "r2-secret-access-key",
  "r2-bucket",
  "r2-prefix",
  "r2-endpoint",
  "r2-lifecycle",
  "max-age-days",
  "max-size",
  "version",
] as const;

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Lines of the server log quoted in main's failure message. */
const LOG_TAIL_LINES = 20;

/** The last lines of the hoard log (already redacted by hoard), for the failure message. */
async function logTail(logPath: string): Promise<string> {
  const log = await readFile(logPath, "utf8").catch(() => "");
  return log.trimEnd().split(/\r?\n/).slice(-LOG_TAIL_LINES).join("\n");
}

async function printLog(core: ActionCore, logPath: string): Promise<void> {
  const log = await readFile(logPath, "utf8").catch(() => "(no hoard log)");
  // A log line starting with `::` must not become a workflow command.
  const stop = randomBytes(16).toString("hex");
  core.startGroup("hoard log");
  core.info(`::stop-commands::${stop}`);
  core.info(log);
  core.info(`::${stop}::`);
  core.endGroup();
}

export interface MainDeps {
  core: ActionCore;
  env: NodeJS.ProcessEnv;
  platform: NodeJS.Platform;
  arch: string;
  ensureBinary(target: Target, version: string): Promise<string>;
  startServer(
    bin: string,
    args: string[],
    logPath: string,
    env: NodeJS.ProcessEnv,
  ): Promise<ServerProcess>;
  waitForHealth(portFile: string, exited: Promise<string>): Promise<number>;
  kill(pid: number): void;
}

export function mainDeps(): MainDeps {
  return {
    core: actionsCore,
    env: process.env,
    platform: process.platform,
    arch: process.arch,
    ensureBinary: (target, version) =>
      ensureBinary(target, version, CHECKSUMS, undefined, process.env),
    startServer,
    waitForHealth: (portFile, exited) => waitForHealth(portFile, 10_000, exited),
    kill: killServer,
  };
}

/**
 * main: settings → target → binary → start → health → mask and export the token → state.
 * A binary that cannot be downloaded (after retries) is a warning: the build goes on without
 * the cache. Every other error fails the step.
 */
export async function runMain(d: MainDeps = mainDeps()): Promise<void> {
  const { core, env } = d;
  const runnerTemp = env.RUNNER_TEMP || tmpdir();
  const id = stepId(env);
  const files = serverFiles(runnerTemp, id);
  let pid = 0;
  try {
    const inputs = Object.fromEntries(INPUTS.map((name) => [name, core.getInput(name)]));
    const settings = resolveSettings(inputs, env);
    for (const w of settings.warnings) core.warning(w);
    const target = targetFor(d.platform, d.arch); // before any network call

    const override = env.HOARD_ACTION_BINARY_PATH;
    let bin: string;
    if (override) {
      core.warning(`using unverified binary from HOARD_ACTION_BINARY_PATH: ${override}`);
      bin = override;
    } else {
      try {
        bin = await d.ensureBinary(target, settings.version);
      } catch (e) {
        if (!(e instanceof BinaryDownloadError)) throw e;
        const note = `hoard binary could not be downloaded (${e.message}); continuing without the cache`;
        core.warning(note);
        core.saveState("unavailable", note); // post puts it in the job summary
        return;
      }
    }

    // A port or token file left by an earlier run would look like a ready server.
    await Promise.all([files.port, files.token, files.summary].map((f) => rm(f, { force: true })));
    const child = await d.startServer(
      bin,
      serveArgs(settings, runnerTemp, id),
      files.log,
      serveEnv(settings, env),
    );
    pid = child.pid;
    const port = await d.waitForHealth(files.port, child.exited);
    const token = (await readFile(files.token, "utf8")).trim();
    if (!token) throw new Error(`hoard wrote an empty token file ${files.token}`);

    core.setSecret(token);
    core.exportVariable("TURBO_API", `http://127.0.0.1:${port}`);
    core.exportVariable("TURBO_TOKEN", token);
    core.exportVariable("TURBO_TEAM", "hoard");

    core.saveState("bin", bin); // post runs `hoard vp save` with it (milestone 3)
    core.saveState("port", String(port));
    core.saveState("token", token);
    core.saveState("pid", String(pid));
    core.saveState("summary", files.summary);
    core.saveState("log", files.log);
    core.saveState("warnings", JSON.stringify(settings.warnings));
  } catch (e) {
    if (!pid) {
      core.setFailed(message(e));
      return;
    }
    d.kill(pid); // spawned but not usable: do not leave it running
    // The binary's own error names the fix; put it in the failure message, not only the log.
    const tail = await logTail(files.log);
    core.setFailed(tail ? `${message(e)}\nLast lines of the hoard log:\n${tail}` : message(e));
    await printLog(core, files.log);
  }
}

export interface PostDeps {
  core: ActionCore;
  env: NodeJS.ProcessEnv;
  getStats: typeof getStats;
  shutdownServer: typeof shutdownServer;
  kill(pid: number): void;
}

export function postDeps(): PostDeps {
  return { core: actionsCore, env: process.env, getStats, shutdownServer, kill: killServer };
}

function savedWarnings(raw: string): string[] {
  try {
    const parsed: unknown = JSON.parse(raw || "[]");
    return Array.isArray(parsed) ? parsed.map(String) : [];
  } catch {
    return [];
  }
}

/** post: outputs from stats → shutdown → job summary → log on problems. Never throws. */
export async function runPost(d: PostDeps = postDeps()): Promise<void> {
  const { core, env } = d;
  try {
    const unavailable = core.getState("unavailable");
    if (unavailable) {
      await core.summary.addRaw(`> ⚠️ ${unavailable}\n`).write();
      return;
    }
    const port = Number(core.getState("port"));
    const token = core.getState("token");
    if (!port || !token) return; // main did not start hoard; nothing to stop
    core.setSecret(token); // post is its own process: mask again before any output

    let problems = false;
    const warnings = savedWarnings(core.getState("warnings"));
    const warn = (w: string) => {
      problems = true;
      warnings.push(w);
      core.warning(w);
    };

    const stats = await d.getStats(port, token);
    core.setOutput("hits", String(stats?.hits ?? 0));
    core.setOutput("misses", String(stats?.misses ?? 0));
    core.setOutput("time-saved-ms", String(stats?.time_saved_ms ?? 0));
    const putErrors = stats?.put_errors ?? 0;
    if (putErrors > 0) {
      // The summary line says `Upload errors: N` too (§9); this makes it a visible annotation.
      problems = true;
      core.warning(`hoard: ${putErrors} uploads failed; see the hoard log`);
    }

    if (!(await d.shutdownServer(port, token))) {
      warn("hoard server was not running at shutdown; the summary may be incomplete");
      d.kill(Number(core.getState("pid"))); // in case it is alive but not answering
    } else if (!stats) {
      warn("could not read hoard stats; the hits, misses and time-saved-ms outputs are 0");
    }

    const summaryFile = core.getState("summary");
    const summary = summaryFile ? await readFile(summaryFile, "utf8").catch(() => "") : "";
    // Blank lines keep the table and each warning in their own Markdown block.
    const parts = [summary.trimEnd(), ...warnings.map((w) => `> ⚠️ ${w}`)].filter(Boolean);
    if (parts.length > 0) {
      try {
        await core.summary.addRaw(`${parts.join("\n\n")}\n`).write();
      } catch (e) {
        warn(`could not write the hoard job summary: ${message(e)}`);
      }
    }

    const logPath = core.getState("log");
    if (logPath && (problems || env.RUNNER_DEBUG === "1")) await printLog(core, logPath);
  } catch (e) {
    core.warning(`hoard post step failed: ${message(e)}`);
  }
}
