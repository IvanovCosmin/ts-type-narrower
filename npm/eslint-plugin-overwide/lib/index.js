"use strict";

const rule = require("./rules/no-overwide-parameters.js");
const { version } = require("../package.json");

const plugin = {
  meta: { name: "eslint-plugin-overwide", version },
  rules: {
    "no-overwide-parameters": rule,
  },
  configs: {},
};

// Flat config (ESLint 9 / eslint.config.js).
plugin.configs.recommended = {
  name: "overwide/recommended",
  files: ["**/*.ts", "**/*.tsx", "**/*.mts", "**/*.cts"],
  plugins: { overwide: plugin },
  rules: {
    "overwide/no-overwide-parameters": "warn",
  },
};

// Legacy config (.eslintrc): { extends: ["plugin:overwide/recommended-legacy"] }.
plugin.configs["recommended-legacy"] = {
  plugins: ["overwide"],
  rules: {
    "overwide/no-overwide-parameters": "warn",
  },
};

module.exports = plugin;
