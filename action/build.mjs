// Bundles the action into dist/main.js and dist/post.js (spec §10: committed, < 200 KB total).
import { rm } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const dist = new URL("./dist/", import.meta.url);
await rm(dist, { recursive: true, force: true });

await build({
  entryPoints: ["src/main.ts", "src/post.ts"],
  outdir: "dist",
  bundle: true,
  platform: "node",
  target: "node24",
  format: "esm",
  minify: true,
  legalComments: "none",
  // Some bundled dependencies are CommonJS and require() node builtins; ESM has no `require`.
  banner: {
    js: 'import{createRequire as __hoardRequire}from"node:module";const require=__hoardRequire(import.meta.url);',
  },
  alias: { undici: "./src/undici-stub.ts" },
  logLevel: "warning",
  absWorkingDir: fileURLToPath(new URL(".", import.meta.url)),
});
