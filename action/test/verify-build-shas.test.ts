import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "vitest";

const script = new URL("../../scripts/verify-build-shas.sh", import.meta.url).pathname;
const sha = (s: string) => createHash("sha256").update(s).digest("hex");

function run(files: Record<string, string>, outputs: Record<string, string>) {
  const dir = mkdtempSync(join(tmpdir(), "vbs-"));
  for (const [n, c] of Object.entries(files)) writeFileSync(join(dir, n), c);
  const r = spawnSync("bash", [script, dir], {
    encoding: "utf8",
    env: { PATH: process.env.PATH, BUILD_OUTPUTS: JSON.stringify(outputs) },
  });
  return { code: r.status, out: r.stdout + r.stderr };
}

test("matching sha256 of every tarball passes", () => {
  const r = run(
    { "hoard-x86_64-unknown-linux-musl.tar.gz": "a", "hoard-aarch64-apple-darwin.tar.gz": "b" },
    { sha_x86_64_unknown_linux_musl: sha("a"), sha_aarch64_apple_darwin: sha("b") },
  );
  expect(r.code).toBe(0);
});
test("a tarball that differs from its build output fails", () => {
  const r = run(
    { "hoard-x86_64-unknown-linux-musl.tar.gz": "a" },
    { sha_x86_64_unknown_linux_musl: sha("evil") },
  );
  expect(r.code).toBe(1);
  expect(r.out).toContain("hoard-x86_64-unknown-linux-musl.tar.gz");
});
test("a tarball with no build output (or an empty one) fails", () => {
  expect(run({ "hoard-x86_64-unknown-linux-musl.tar.gz": "a" }, {}).code).toBe(1);
  expect(
    run({ "hoard-x86_64-unknown-linux-musl.tar.gz": "a" }, { sha_x86_64_unknown_linux_musl: "" })
      .code,
  ).toBe(1);
});
test("no tarballs at all fails", () => {
  expect(run({}, {}).code).toBe(1);
});
