#!/usr/bin/env node
"use strict";

const { spawnSync } = require("node:child_process");
const { binaryPath } = require("../lib/index.js");

let bin;
try {
  bin = binaryPath();
} catch (err) {
  console.error(err.message);
  process.exit(2);
}

const res = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (res.error) {
  console.error(`type-narrower: ${res.error.message}`);
  process.exit(2);
}
process.exit(res.status ?? 2);
