#!/usr/bin/env node
// Generate one platform npm package (e.g. overwide-linux-x64) around a built
// binary. Used by the release workflow and by local testing.
//
//   node scripts/make-platform-package.mjs <pkg-name> <binary-path> <out-dir>

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const [pkgName, binaryPath, outDir] = process.argv.slice(2);
if (!pkgName || !binaryPath || !outDir) {
  console.error("usage: make-platform-package.mjs <pkg-name> <binary-path> <out-dir>");
  process.exit(2);
}

const PLATFORMS = {
  "overwide-linux-x64": { os: "linux", cpu: "x64", libc: "glibc" },
  "overwide-linux-x64-musl": { os: "linux", cpu: "x64", libc: "musl" },
  "overwide-linux-arm64": { os: "linux", cpu: "arm64", libc: "glibc" },
  "overwide-darwin-x64": { os: "darwin", cpu: "x64" },
  "overwide-darwin-arm64": { os: "darwin", cpu: "arm64" },
  "overwide-win32-x64": { os: "win32", cpu: "x64" },
};

const meta = PLATFORMS[pkgName];
if (!meta) {
  console.error(`unknown platform package: ${pkgName}`);
  process.exit(2);
}

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const { version } = JSON.parse(
  fs.readFileSync(path.join(repoRoot, "npm/overwide/package.json"), "utf8")
);

const exe = meta.os === "win32" ? "overwide.exe" : "overwide";
const binDir = path.join(outDir, "bin");
fs.mkdirSync(binDir, { recursive: true });
fs.copyFileSync(binaryPath, path.join(binDir, exe));
if (meta.os !== "win32") fs.chmodSync(path.join(binDir, exe), 0o755);

const pkg = {
  name: pkgName,
  version,
  description: `overwide binary for ${meta.os}-${meta.cpu}${meta.libc === "musl" ? " (musl)" : ""}`,
  license: "MIT",
  repository: { type: "git", url: "git+https://github.com/IvanovCosmin/overwide.git" },
  os: [meta.os],
  cpu: [meta.cpu],
  files: ["bin"],
  ...(meta.libc ? { libc: [meta.libc] } : {}),
};
fs.writeFileSync(path.join(outDir, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
fs.writeFileSync(
  path.join(outDir, "README.md"),
  `# ${pkgName}\n\nPrebuilt \`overwide\` binary for ${meta.os}-${meta.cpu}. Install the [overwide](https://www.npmjs.com/package/overwide) package instead of this one.\n`
);
console.log(`wrote ${pkgName}@${version} to ${outDir}`);
