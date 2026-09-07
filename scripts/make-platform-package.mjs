#!/usr/bin/env node
// Generate one platform npm package (e.g. type-narrower-linux-x64) around a built
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
  "type-narrower-linux-x64": { os: "linux", cpu: "x64", libc: "glibc" },
  "type-narrower-linux-x64-musl": { os: "linux", cpu: "x64", libc: "musl" },
  "type-narrower-linux-arm64": { os: "linux", cpu: "arm64", libc: "glibc" },
  "type-narrower-darwin-x64": { os: "darwin", cpu: "x64" },
  "type-narrower-darwin-arm64": { os: "darwin", cpu: "arm64" },
  "type-narrower-win32-x64": { os: "win32", cpu: "x64" },
};

const meta = PLATFORMS[pkgName];
if (!meta) {
  console.error(`unknown platform package: ${pkgName}`);
  process.exit(2);
}

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const { version } = JSON.parse(
  fs.readFileSync(path.join(repoRoot, "npm/type-narrower/package.json"), "utf8")
);

const exe = meta.os === "win32" ? "type-narrower.exe" : "type-narrower";
const binDir = path.join(outDir, "bin");
fs.mkdirSync(binDir, { recursive: true });
fs.copyFileSync(binaryPath, path.join(binDir, exe));
if (meta.os !== "win32") fs.chmodSync(path.join(binDir, exe), 0o755);

const pkg = {
  name: pkgName,
  version,
  description: `type-narrower binary for ${meta.os}-${meta.cpu}${meta.libc === "musl" ? " (musl)" : ""}`,
  license: "MIT",
  repository: { type: "git", url: "git+https://github.com/IvanovCosmin/type-narrower.git" },
  os: [meta.os],
  cpu: [meta.cpu],
  files: ["bin"],
  ...(meta.libc ? { libc: [meta.libc] } : {}),
};
fs.writeFileSync(path.join(outDir, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
fs.writeFileSync(
  path.join(outDir, "README.md"),
  `# ${pkgName}\n\nPrebuilt \`type-narrower\` binary for ${meta.os}-${meta.cpu}. Install the [type-narrower](https://www.npmjs.com/package/type-narrower) package instead of this one.\n`
);
console.log(`wrote ${pkgName}@${version} to ${outDir}`);
