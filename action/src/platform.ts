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

const BY_PLATFORM: Partial<Record<string, Partial<Record<string, Target>>>> = {
  linux: { x64: "x86_64-unknown-linux-musl", arm64: "aarch64-unknown-linux-musl" },
  darwin: { x64: "x86_64-apple-darwin", arm64: "aarch64-apple-darwin" },
  win32: { x64: "x86_64-pc-windows-msvc" },
};

/** The release target for a Node `process.platform` / `process.arch` pair. */
export function targetFor(platform: NodeJS.Platform, arch: string): Target {
  const target = BY_PLATFORM[platform]?.[arch];
  if (!target) throw new Error(`unsupported platform ${platform}/${arch}`);
  return target;
}
