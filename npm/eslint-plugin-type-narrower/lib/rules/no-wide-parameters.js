"use strict";

const path = require("node:path");
const { getReport, resolveRoot } = require("../analysis-cache.js");

module.exports = {
  meta: {
    type: "suggestion",
    docs: {
      description:
        "Report function parameters whose declared union types contain constituents no call site ever passes",
      url: "https://github.com/IvanovCosmin/type-narrower#the-rule",
    },
    schema: [
      {
        type: "object",
        properties: {
          projectRoot: {
            type: "string",
            description:
              "Analysis root. Defaults to the enclosing git top-level; the closed-world assumption is only sound over the whole repository.",
          },
          respectExports: {
            type: "boolean",
            description: "Open-world mode: skip exported functions.",
          },
          maxDepth: { type: "integer", minimum: 1 },
          cacheMs: {
            type: "integer",
            minimum: 0,
            description:
              "How long a project analysis is reused before re-running (ms). 0 disables expiry. Default 10000.",
          },
        },
        additionalProperties: false,
      },
    ],
    messages: {
      neverPassed:
        "Parameter '{{name}}' of '{{function}}' is declared as {{declared}} but its {{callCount}} call site(s) never pass: {{unused}}.",
      analysisFailed: "type-narrower analysis failed: {{message}}",
    },
  },

  create(context) {
    const opts = context.options[0] || {};
    const cacheMs = opts.cacheMs ?? 10_000;
    const analyzeOpts = {};
    if (opts.respectExports) analyzeOpts.respectExports = true;
    if (opts.maxDepth != null) analyzeOpts.maxDepth = opts.maxDepth;

    const filename = context.filename ?? context.getFilename();
    // Skip virtual filenames (stdin, processor-generated blocks).
    if (!path.isAbsolute(filename)) return {};

    return {
      Program(node) {
        const root = resolveRoot(context.cwd ?? process.cwd(), opts.projectRoot);
        const { report, error } = getReport(root, analyzeOpts, cacheMs);

        if (error) {
          context.report({ node, messageId: "analysisFailed", data: { message: error.message } });
          return;
        }

        const here = path.resolve(filename);
        const lineCount = context.sourceCode.lines.length;
        for (const f of report.findings) {
          if (path.resolve(root, f.file) !== here) continue;
          // The report can be a few seconds stale in editors; clamp so a
          // moved declaration never produces an out-of-range location.
          const line = Math.max(1, Math.min(f.line, lineCount));
          context.report({
            loc: { start: { line, column: 0 }, end: { line, column: 0 } },
            messageId: "neverPassed",
            data: {
              name: f.param + f.path,
              function: f.function,
              declared: f.declared,
              callCount: String(f.callCount),
              unused: f.unused.join(", "),
            },
          });
        }
      },
    };
  },
};
