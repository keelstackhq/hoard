import { expect, test } from "vitest";
import { targetFor } from "../src/platform.js";

test("targets", () => {
  expect(targetFor("linux", "x64")).toBe("x86_64-unknown-linux-musl");
  expect(targetFor("darwin", "arm64")).toBe("aarch64-apple-darwin");
  expect(targetFor("win32", "x64")).toBe("x86_64-pc-windows-msvc");
  expect(() => targetFor("linux", "s390x")).toThrow("unsupported platform linux/s390x");
});

test("all five targets are reachable", () => {
  expect(targetFor("linux", "arm64")).toBe("aarch64-unknown-linux-musl");
  expect(targetFor("darwin", "x64")).toBe("x86_64-apple-darwin");
});

test.each([
  ["win32", "arm64"],
  ["freebsd", "x64"],
  ["linux", "ia32"],
] as const)("%s/%s is unsupported", (platform, arch) => {
  expect(() => targetFor(platform, arch)).toThrow(`unsupported platform ${platform}/${arch}`);
});
