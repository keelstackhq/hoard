// Which task runners a repository uses (spec §10 main 1). Pure file checks at the root.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

export interface Tools {
  /** `turbo.json` (or `turbo.jsonc`) at the root. */
  turbo: boolean;
  /** `vite-plus` in the root `package.json` dependencies or devDependencies. */
  vp: boolean;
  /** `Cargo.toml` at the root (spec §14: hoard does not cache cargo builds). */
  cargo: boolean;
}

const DEPENDENCY_FIELDS = ["dependencies", "devDependencies", "optionalDependencies"] as const;

function hasVitePlus(root: string): boolean {
  try {
    const pkg: unknown = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
    if (typeof pkg !== "object" || pkg === null) return false;
    return DEPENDENCY_FIELDS.some((field) => {
      const deps = (pkg as Record<string, unknown>)[field];
      return typeof deps === "object" && deps !== null && "vite-plus" in deps;
    });
  } catch {
    return false; // no or unreadable package.json: not a Vite+ repo
  }
}

export function detectTools(root: string): Tools {
  return {
    turbo: ["turbo.json", "turbo.jsonc"].some((f) => existsSync(join(root, f))),
    vp: hasVitePlus(root),
    cargo: existsSync(join(root, "Cargo.toml")),
  };
}
