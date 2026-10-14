import { strict as assert } from "node:assert";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, readdirSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { generateMonorepo } from "./generate-monorepo.mjs";

const cli = fileURLToPath(new URL("./generate-monorepo.mjs", import.meta.url));
const tmp = () => mkdtempSync(join(tmpdir(), "bench-gen-"));

test("writes the workspace root pinned like the turbo fixture", () => {
  const dir = tmp();
  try {
    generateMonorepo(dir, 3);
    const root = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
    assert.equal(root.packageManager, "pnpm@10.28.0");
    assert.equal(root.devDependencies.turbo, "2.11.8");
    assert.match(readFileSync(join(dir, "pnpm-workspace.yaml"), "utf8"), /packages\/\*/);
    const turbo = JSON.parse(readFileSync(join(dir, "turbo.json"), "utf8"));
    assert.deepEqual(turbo.tasks.build.outputs, ["dist/**"]);
    assert.equal(existsSync(join(dir, "pnpm-lock.yaml")), false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("writes N independent, zero-padded packages with a build script", () => {
  const dir = tmp();
  try {
    generateMonorepo(dir, 12);
    const names = readdirSync(join(dir, "packages")).sort();
    assert.equal(names.length, 12);
    assert.equal(names[0], "pkg-0001");
    assert.equal(names[11], "pkg-0012");
    const pkg = JSON.parse(readFileSync(join(dir, "packages/pkg-0007/package.json"), "utf8"));
    assert.equal(pkg.name, "@bench/pkg-0007");
    assert.equal(pkg.dependencies, undefined);
    assert.equal(pkg.devDependencies, undefined);
    assert.ok(pkg.scripts.build.startsWith("node "));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("output is deterministic across runs and directories", () => {
  const a = tmp();
  const b = tmp();
  try {
    generateMonorepo(a, 5);
    generateMonorepo(b, 5);
    for (const f of ["package.json", "turbo.json", "packages/pkg-0003/package.json", "packages/pkg-0003/build.mjs"]) {
      assert.equal(readFileSync(join(a, f), "utf8"), readFileSync(join(b, f), "utf8"), f);
    }
  } finally {
    rmSync(a, { recursive: true, force: true });
    rmSync(b, { recursive: true, force: true });
  }
});

test("the build script writes dist/out.txt with the name and a 1-4 KB payload", () => {
  const dir = tmp();
  try {
    generateMonorepo(dir, 2);
    const pkgDir = join(dir, "packages/pkg-0002");
    execFileSync("node", ["build.mjs"], { cwd: pkgDir });
    const out = readFileSync(join(pkgDir, "dist/out.txt"), "utf8");
    assert.ok(out.includes("@bench/pkg-0002"));
    assert.ok(out.length >= 1024 && out.length <= 4096, `size ${out.length}`);
    execFileSync("node", ["build.mjs"], { cwd: pkgDir });
    assert.equal(readFileSync(join(pkgDir, "dist/out.txt"), "utf8"), out);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("rejects package counts outside 1..5000", () => {
  const dir = tmp();
  try {
    for (const n of [0, -1, 5001, 1.5, Number.NaN]) {
      assert.throws(() => generateMonorepo(dir, n), RangeError, String(n));
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI validates its arguments and generates", () => {
  const dir = tmp();
  try {
    assert.throws(() => execFileSync("node", [cli, dir, "abc"], { stdio: "pipe" }));
    assert.throws(() => execFileSync("node", [cli], { stdio: "pipe" }));
    execFileSync("node", [cli, dir, "4"], { stdio: "pipe" });
    assert.equal(readdirSync(join(dir, "packages")).length, 4);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
