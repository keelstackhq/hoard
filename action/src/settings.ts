// Resolves the action inputs and the runner environment into the settings for `hoard serve`
// (spec §4.2, §6.4, §8). Pure: no I/O, no process.env; callers pass both in.
import { VERSION } from "./checksums.generated.js";

export interface R2Settings {
  accountId: string;
  accessKeyId: string;
  secretAccessKey: string;
  bucket: string;
  prefix: string;
  /** Optional S3 endpoint override; empty when unset. */
  endpoint: string;
}

export interface Settings {
  storage: "github" | "r2" | "fs";
  mode: "read-write" | "read" | "off";
  noReads: boolean;
  cacheVersion: string;
  r2?: R2Settings;
  r2Lifecycle: "auto" | "verify" | "manual";
  maxAgeDays: number;
  maxSize: string;
  /** R2 only: refresh objects older than max-age-days / 2 when they are read (§7.3); default true. */
  refreshOnHit: boolean;
  /** `vp save` skips a Vite+ task cache larger than this (§4.2). */
  vpMaxSize: string;
  /** The hoard binary version; always the action's own release (the only verified one). */
  version: string;
  warnings: string[];
}

const STORAGES = ["github", "r2", "fs"] as const;
const MODES = ["read-write", "read", "off"] as const;
const LIFECYCLES = ["auto", "verify", "manual"] as const;

function oneOf<T extends string>(name: string, value: string, allowed: readonly T[]): T {
  if ((allowed as readonly string[]).includes(value)) return value as T;
  const list = `${allowed.slice(0, -1).join(", ")} or ${allowed[allowed.length - 1]}`;
  throw new Error(`invalid ${name} ${JSON.stringify(value)}: use ${list}`);
}

/** Mode from ACTIONS_CACHE_MODE (the workflow's cache-mode); anything else is read-write. */
function modeFromEnv(env: NodeJS.ProcessEnv): Pick<Settings, "mode" | "noReads"> {
  switch (env.ACTIONS_CACHE_MODE) {
    case "read":
      return { mode: "read", noReads: false };
    case "none":
      return { mode: "off", noReads: false };
    case "write-only":
      return { mode: "read-write", noReads: true };
    default:
      return { mode: "read-write", noReads: false };
  }
}

function maxAgeDays(value: string): number {
  const n = /^\d+$/.test(value) ? Number(value) : Number.NaN;
  if (!(n >= 1 && n <= 36500)) {
    throw new Error(
      `invalid max-age-days ${JSON.stringify(value)}: use a whole number of days between 1 and 36500`,
    );
  }
  return n;
}

/** A yes/no input: true, false, yes, no, on, off, 1 or 0 (any case). */
function bool(name: string, value: string): boolean {
  const v = value.toLowerCase();
  if (["true", "yes", "on", "1"].includes(v)) return true;
  if (["false", "no", "off", "0"].includes(v)) return false;
  throw new Error(`invalid ${name} ${JSON.stringify(value)}: use true or false`);
}

/** A size as the binary parses it: digits and an optional B, KB, MB, GB or TB (any case). */
function size(name: string, value: string): string {
  if (/^\d+(B|KB|MB|GB|TB)?$/i.test(value)) return value;
  throw new Error(
    `invalid ${name} ${JSON.stringify(value)}: use a number with an optional KB, MB, GB or TB suffix, e.g. 2GB`,
  );
}

export function resolveSettings(inputs: Record<string, string>, env: NodeJS.ProcessEnv): Settings {
  const input = (name: string, fallback = "") => inputs[name]?.trim() || fallback;
  const warnings: string[] = [];

  const version = input("version", VERSION);
  if (version !== VERSION) {
    throw new Error(
      `invalid version ${JSON.stringify(version)}: this action build only has the verified hoard ${VERSION} (its own release). ` +
        "Remove the version input, or change `uses:` to the hoard action release you want",
    );
  }

  const modeInput = input("mode");
  const { mode, noReads } = modeInput
    ? { mode: oneOf("mode", modeInput, MODES), noReads: false }
    : modeFromEnv(env);

  let storage = oneOf("storage", input("storage", "github"), STORAGES);
  let r2: R2Settings | undefined;
  if (storage === "r2") {
    const candidate: R2Settings = {
      accountId: input("r2-account-id"),
      accessKeyId: input("r2-access-key-id"),
      secretAccessKey: input("r2-secret-access-key"),
      bucket: input("r2-bucket"),
      prefix: input("r2-prefix", "cache/"),
      endpoint: input("r2-endpoint"),
    };
    // Names only, never values: these are secrets.
    const missing = [
      !candidate.accessKeyId && "r2-access-key-id",
      !candidate.secretAccessKey && "r2-secret-access-key",
      !candidate.bucket && "r2-bucket",
      !candidate.accountId && !candidate.endpoint && "r2-account-id or r2-endpoint",
    ].filter((name): name is string => Boolean(name));
    if (missing.length === 0) {
      r2 = candidate;
    } else {
      storage = "github";
      warnings.push(
        `R2 secrets are not available (missing: ${missing.join(", ")}), so hoard uses storage: github for this run. ` +
          "This is expected on pull requests from forks, which receive no secrets; otherwise set the missing inputs.",
      );
    }
  }

  return {
    storage,
    mode,
    noReads,
    cacheVersion: input("cache-version", "v1"),
    ...(r2 && { r2 }),
    r2Lifecycle: oneOf("r2-lifecycle", input("r2-lifecycle", "auto"), LIFECYCLES),
    maxAgeDays: maxAgeDays(input("max-age-days", "7")),
    maxSize: input("max-size", "5GB"),
    refreshOnHit: bool("refresh-on-hit", input("refresh-on-hit", "true")),
    vpMaxSize: size("vp-max-size", input("vp-max-size", "2GB")),
    version,
    warnings,
  };
}
