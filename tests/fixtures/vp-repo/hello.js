import { mkdirSync, writeFileSync } from "node:fs";

mkdirSync("dist", { recursive: true });
writeFileSync("dist/hello.txt", "hello from the hoard vp fixture\n");
console.log("wrote dist/hello.txt");
