import { describe, expect, test } from "vitest";
import { VERSION } from "../src/checksums.generated.js";
import { resolveSettings } from "../src/settings.js";

const R2 = {
  storage: "r2",
  "r2-account-id": "acct-123",
  "r2-access-key-id": "AKIDSECRETVALUE",
  "r2-secret-access-key": "s3cr3t-value-do-not-print",
  "r2-bucket": "b",
};

test("r2 without secrets falls back to github", () => {
  const s = resolveSettings({ storage: "r2", "r2-bucket": "b" }, {});
  expect(s.storage).toBe("github");
  expect(s.warnings[0]).toMatch(/R2 secrets are not available/);
});

test("ACTIONS_CACHE_MODE maps to mode", () => {
  expect(resolveSettings({}, { ACTIONS_CACHE_MODE: "read" }).mode).toBe("read");
  expect(resolveSettings({}, { ACTIONS_CACHE_MODE: "none" }).mode).toBe("off");
  expect(resolveSettings({}, { ACTIONS_CACHE_MODE: "write-only" })).toMatchObject({
    mode: "read-write",
    noReads: true,
  });
});

test("explicit mode input wins", () => {
  expect(resolveSettings({ mode: "read" }, { ACTIONS_CACHE_MODE: "write" }).mode).toBe("read");
  expect(
    resolveSettings({ mode: "read-write" }, { ACTIONS_CACHE_MODE: "write-only" }),
  ).toMatchObject({ mode: "read-write", noReads: false });
});

test("ACTIONS_CACHE_MODE write, unset or unknown → read-write", () => {
  for (const env of [{ ACTIONS_CACHE_MODE: "write" }, {}, { ACTIONS_CACHE_MODE: "" }]) {
    expect(resolveSettings({}, env)).toMatchObject({ mode: "read-write", noReads: false });
  }
});

test("invalid mode input throws with the allowed values", () => {
  expect(() => resolveSettings({ mode: "write" }, {})).toThrow(/read-write, read or off/);
});

test("defaults", () => {
  const s = resolveSettings({}, {});
  expect(s).toEqual({
    storage: "github",
    mode: "read-write",
    noReads: false,
    cacheVersion: "v1",
    r2Lifecycle: "auto",
    maxAgeDays: 7,
    maxSize: "5GB",
    version: VERSION,
    warnings: [],
  });
});

test("empty inputs count as unset", () => {
  const s = resolveSettings(
    { storage: "", mode: "", "cache-version": "", "max-age-days": "", version: "" },
    {},
  );
  expect(s).toMatchObject({ storage: "github", cacheVersion: "v1", maxAgeDays: 7 });
});

test("r2 with all secrets", () => {
  const s = resolveSettings({ ...R2, "r2-prefix": "team/" }, {});
  expect(s.storage).toBe("r2");
  expect(s.r2).toEqual({
    accountId: "acct-123",
    accessKeyId: "AKIDSECRETVALUE",
    secretAccessKey: "s3cr3t-value-do-not-print",
    bucket: "b",
    prefix: "team/",
    endpoint: "",
  });
  expect(s.warnings).toEqual([]);
});

test("r2 prefix defaults to cache/", () => {
  expect(resolveSettings(R2, {}).r2?.prefix).toBe("cache/");
});

test("r2 with endpoint instead of account id is OK", () => {
  const { "r2-account-id": _, ...noAccount } = R2;
  const s = resolveSettings({ ...noAccount, "r2-endpoint": "http://127.0.0.1:9000" }, {});
  expect(s.storage).toBe("r2");
  expect(s.r2).toMatchObject({ accountId: "", endpoint: "http://127.0.0.1:9000" });
});

test("R2 fallback warning lists missing input names but never a secret value", () => {
  const s = resolveSettings({ ...R2, "r2-secret-access-key": "", "r2-account-id": "" }, {});
  expect(s.storage).toBe("github");
  expect(s.r2).toBeUndefined();
  const w = s.warnings.join("\n");
  expect(w).toMatch(/^R2 secrets are not available/);
  expect(w).toContain("fork");
  const missing = /missing: ([^)]*)\)/.exec(w)?.[1];
  expect(missing).toBe("r2-secret-access-key, r2-account-id or r2-endpoint");
  for (const value of Object.values(R2)) {
    if (value !== "r2" && value !== "b") expect(w).not.toContain(value);
  }
});

test("github storage ignores r2 inputs and warns nothing", () => {
  const s = resolveSettings({ ...R2, storage: "github" }, {});
  expect(s.storage).toBe("github");
  expect(s.r2).toBeUndefined();
  expect(s.warnings).toEqual([]);
});

describe("validation", () => {
  test("storage", () => {
    expect(resolveSettings({ storage: "fs" }, {}).storage).toBe("fs");
    expect(() => resolveSettings({ storage: "s3" }, {})).toThrow(/github, r2 or fs/);
  });
  test("r2-lifecycle", () => {
    expect(resolveSettings({ "r2-lifecycle": "manual" }, {}).r2Lifecycle).toBe("manual");
    expect(() => resolveSettings({ "r2-lifecycle": "off" }, {})).toThrow(/auto, verify or manual/);
  });
  test("max-age-days", () => {
    expect(resolveSettings({ "max-age-days": "36500" }, {}).maxAgeDays).toBe(36500);
    expect(resolveSettings({ "max-age-days": "1" }, {}).maxAgeDays).toBe(1);
    for (const bad of ["0", "36501", "1.5", "-3", "7d", "abc"]) {
      expect(() => resolveSettings({ "max-age-days": bad }, {})).toThrow(
        /max-age-days .*1 and 36500/,
      );
    }
  });
  test("max-size and cache-version pass through", () => {
    const s = resolveSettings({ "max-size": "20GB", "cache-version": "v2" }, {});
    expect(s).toMatchObject({ maxSize: "20GB", cacheVersion: "v2" });
  });
});

describe("version", () => {
  test("equal to the baked version is OK", () => {
    expect(resolveSettings({ version: VERSION }, {}).version).toBe(VERSION);
  });
  test("a different version is refused", () => {
    expect(() => resolveSettings({ version: "9.9.9" }, {})).toThrow(
      /only .*verified.*change `uses:`/s,
    );
  });
});
