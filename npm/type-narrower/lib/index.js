"use strict";

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

/**
 * Map (platform, arch, libc) to the platform package that carries the binary.
 * Mirrors the optionalDependencies in package.json and the CI build matrix.
 */
function platformPackage() {
  const { platform, arch } = process;
  if (platform === "linux" && arch === "x64") {
    return isMusl() ? "type-narrower-linux-x64-musl" : "type-narrower-linux-x64";
  }
  if (platform === "linux" && arch === "arm64") return "type-narrower-linux-arm64";
  if (platform === "darwin" && arch === "x64") return "type-narrower-darwin-x64";
  if (platform === "darwin" && arch === "arm64") return "type-narrower-darwin-arm64";
  if (platform === "win32" && arch === "x64") return "type-narrower-win32-x64";
  return null;
}

function isMusl() {
  // glibcVersionRuntime is absent on musl-based distros (Alpine).
  try {
    const report = process.report.getReport();
    return !report.header.glibcVersionRuntime;
  } catch {
    return false;
  }
}

/**
 * Absolute path to the type-narrower binary for this platform.
 * Resolution order: TYPE_NARROWER_BINARY env var, then the installed platform
 * package. Throws with an actionable message when neither is available.
 */
function binaryPath() {
  const override = process.env.TYPE_NARROWER_BINARY;
  if (override) {
    if (!fs.existsSync(override)) {
      throw new Error(`TYPE_NARROWER_BINARY points to a missing file: ${override}`);
    }
    return override;
  }

  const pkg = platformPackage();
  if (!pkg) {
    throw new Error(
      `type-narrower: unsupported platform ${process.platform}-${process.arch}. ` +
        `Build from source (cargo build --release) and set TYPE_NARROWER_BINARY.`
    );
  }

  const exe = process.platform === "win32" ? "type-narrower.exe" : "type-narrower";
  try {
    return require.resolve(`${pkg}/bin/${exe}`);
  } catch {
    throw new Error(
      `type-narrower: platform package "${pkg}" is not installed. ` +
        `Reinstall dependencies (optionalDependencies must not be disabled), ` +
        `or set TYPE_NARROWER_BINARY to a locally built binary.`
    );
  }
}

/**
 * Run the analyzer over `root` and return the parsed JSON report:
 * { version, findings, stats, uncalled, warnings }.
 *
 * options:
 *   respectExports  boolean  skip exported functions (open-world mode)
 *   maxDepth        number   directory recursion limit
 *   diff            string   git range; restrict reporting to changed lines
 *   listUncalled    boolean  include never-called function candidates
 */
function analyze(root, options = {}) {
  const args = [path.resolve(root), "--json", "--quiet"];
  if (options.respectExports) args.push("--respect-exports");
  if (options.maxDepth != null) args.push("--max-depth", String(options.maxDepth));
  if (options.diff) args.push("--diff", options.diff);
  if (options.listUncalled) args.push("--list-uncalled");

  const bin = binaryPath();
  const res = spawnSync(bin, args, {
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
  });

  if (res.error) {
    throw new Error(`type-narrower: failed to spawn ${bin}: ${res.error.message}`);
  }
  // --quiet suppresses only the summary line; anything left on stderr is a
  // soundness warning (parse errors, unreadable files, sub-root analysis).
  if (res.stderr) process.stderr.write(res.stderr);
  if (res.status !== 0) {
    throw new Error(
      `type-narrower exited with code ${res.status}:\n${res.stderr || res.stdout}`
    );
  }
  return JSON.parse(res.stdout);
}

module.exports = { analyze, binaryPath, platformPackage };
