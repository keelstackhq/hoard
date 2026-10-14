// Runs `hoard vp restore` (main) and `hoard vp save` (post) for Vite+ repositories (spec §10).
// Every problem is reported as a not-ok result, never thrown: the cache must not fail the build.
import { spawn } from "node:child_process";
import type { serverFiles } from "./server.js";
import { storageArgs } from "./server.js";
import type { Settings } from "./settings.js";

/** Upper bound for `hoard vp restore` in main. */
export const VP_RESTORE_TIMEOUT_MS = 120_000;
/** Upper bound for `hoard vp save` in post (uploads can be large). */
export const VP_SAVE_TIMEOUT_MS = 300_000;

/** argv after `vp <cmd>`: the same storage as the server, the workspace and the size limit. */
export function vpArgs(
  s: Settings,
  files: ReturnType<typeof serverFiles>,
  workspace: string,
): string[] {
  return [...storageArgs(s, files), "--workspace", workspace, "--vp-max-size", s.vpMaxSize];
}

/** `ok` is false when the cache could not be restored or saved; `line` says what happened. */
export interface VpResult {
  ok: boolean;
  line: string;
}

const lastLine = (text: string) =>
  text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean)
    .pop() ?? "";

function seconds(ms: number): string {
  return `${ms / 1000}s`;
}

/** Time between the SIGTERM at the timeout and the SIGKILL. */
export const KILL_GRACE_MS = 5_000;

/**
 * Runs `<bin> vp <cmd> <args>` with `env`. At `timeoutMs` it sends SIGTERM (hoard then drops
 * its spooled archive or half-extracted directory and exits), and SIGKILL `KILL_GRACE_MS` later.
 */
export function runVp(
  bin: string,
  cmd: "restore" | "save",
  args: string[],
  env: NodeJS.ProcessEnv,
  timeoutMs: number,
): Promise<VpResult> {
  return new Promise((resolve) => {
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    let done = false;
    const timers: NodeJS.Timeout[] = [];
    const finish = (r: VpResult) => {
      if (done) return;
      done = true;
      for (const t of timers) clearTimeout(t);
      resolve(r);
    };
    const child = spawn(bin, ["vp", cmd, ...args], {
      env,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    child.stdout.on("data", (b: Buffer) => {
      stdout = (stdout + b.toString()).slice(-(1 << 16));
    });
    child.stderr.on("data", (b: Buffer) => {
      stderr = (stderr + b.toString()).slice(-(1 << 16));
    });
    child.once("error", (e) =>
      finish({ ok: false, line: `hoard vp ${cmd} could not run: ${e.message}` }),
    );
    timers.push(
      setTimeout(() => {
        timedOut = true;
        child.kill("SIGTERM");
        timers.push(setTimeout(() => child.kill("SIGKILL"), KILL_GRACE_MS));
      }, timeoutMs),
    );
    // `exit`, not `close`: a grandchild holding the pipes open must not keep the step waiting.
    child.once("exit", (code) => {
      if (timedOut) {
        finish({ ok: false, line: `hoard vp ${cmd} timed out after ${seconds(timeoutMs)}` });
        return;
      }
      // Let the pipes drain what was written before the exit.
      setImmediate(() => {
        const out = lastLine(stdout);
        if (code === 0) {
          // hoard exits 0 on storage errors and says so on its one line.
          const warning = out.startsWith("warning: ");
          finish({ ok: !warning, line: warning ? out.slice("warning: ".length) : out });
        } else {
          const why = lastLine(stderr);
          const how = code === null ? "was killed" : `exited with code ${code}`;
          finish({ ok: false, line: `hoard vp ${cmd} ${how}${why ? `: ${why}` : ""}` });
        }
      });
    });
  });
}
