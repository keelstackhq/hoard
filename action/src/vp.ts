// Runs `hoard vp restore` (main) and `hoard vp save` (post) for Vite+ repositories (spec §10).
// Every problem is reported as a not-ok result, never thrown: the cache must not fail the build.
import { execFile } from "node:child_process";
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

/** Runs `<bin> vp <cmd> <args>` with `env`, killed after `timeoutMs`. */
export function runVp(
  bin: string,
  cmd: "restore" | "save",
  args: string[],
  env: NodeJS.ProcessEnv,
  timeoutMs: number,
): Promise<VpResult> {
  return new Promise((resolve) => {
    execFile(
      bin,
      ["vp", cmd, ...args],
      { env, timeout: timeoutMs, killSignal: "SIGKILL", windowsHide: true, maxBuffer: 1 << 20 },
      (error, stdout, stderr) => {
        const out = lastLine(String(stdout));
        if (!error) {
          // hoard exits 0 on storage errors and says so on its one line.
          const warning = out.startsWith("warning: ");
          resolve({ ok: !warning, line: warning ? out.slice("warning: ".length) : out });
          return;
        }
        const e = error as NodeJS.ErrnoException & { killed?: boolean; code?: unknown };
        if (e.killed) {
          resolve({ ok: false, line: `hoard vp ${cmd} timed out after ${seconds(timeoutMs)}` });
        } else if (typeof e.code === "number") {
          const why = lastLine(String(stderr));
          resolve({
            ok: false,
            line: `hoard vp ${cmd} exited with code ${e.code}${why ? `: ${why}` : ""}`,
          });
        } else {
          resolve({ ok: false, line: `hoard vp ${cmd} could not run: ${e.message}` });
        }
      },
    );
  });
}
