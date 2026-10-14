// The five release targets, in spec order (§10). scripts/write-checksums.mjs keeps a copy of
// this list; action/test/write-checksums.test.ts asserts the two are equal.
export const TARGETS = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
] as const;

export type Target = (typeof TARGETS)[number];
