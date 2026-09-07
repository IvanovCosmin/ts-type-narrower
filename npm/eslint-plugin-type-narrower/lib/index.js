"use strict";

const rule = require("./rules/no-wide-parameters.js");
const { version } = require("../package.json");

const plugin = {
  meta: { name: "eslint-plugin-type-narrower", version },
  rules: {
    "no-wide-parameters": rule,
  },
  configs: {},
};

// Flat config (ESLint 9 / eslint.config.js).
plugin.configs.recommended = {
  name: "type-narrower/recommended",
  files: ["**/*.ts", "**/*.tsx", "**/*.mts", "**/*.cts"],
  plugins: { "type-narrower": plugin },
  rules: {
    "type-narrower/no-wide-parameters": "warn",
  },
};

// Legacy config (.eslintrc): { extends: ["plugin:type-narrower/recommended-legacy"] }.
plugin.configs["recommended-legacy"] = {
  plugins: ["type-narrower"],
  rules: {
    "type-narrower/no-wide-parameters": "warn",
  },
};

module.exports = plugin;
