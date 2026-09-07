# eslint-plugin-type-narrower

Surfaces [type-narrower](https://github.com/IvanovCosmin/type-narrower) findings in
ESLint: function parameters whose declared union types contain constituents no
call site ever passes.

```ts
type Channel = "email" | "sms" | "push" | "fax";
function send(channel: Channel) {}
send("email"); send("sms"); send("push");
// warning  Parameter 'channel' of 'send' is declared as
// "email" | "sms" | "push" | "fax" but its 3 call site(s) never pass: "fax"
```

The analyzer is a Rust binary (installed automatically via the `type-narrower`
package's prebuilt platform binaries). It analyzes the **whole project once
per lint run** — closed-world, cross-module — and this plugin maps its
findings onto each linted file. It is not a per-file AST rule; the analysis
root defaults to the enclosing git top-level, which is the only root where
the closed-world assumption is sound.

## Setup

```
npm install -D eslint-plugin-type-narrower
```

Requires a TypeScript-capable parser (you almost certainly already use
[typescript-eslint](https://typescript-eslint.io)). Flat config:

```js
// eslint.config.js
import type-narrower from "eslint-plugin-type-narrower";

export default [
  // ...your typescript-eslint setup...
  type-narrower.configs.recommended, // warns on **/*.ts,tsx,mts,cts
];
```

Legacy `.eslintrc`:

```json
{ "extends": ["plugin:type-narrower/recommended-legacy"] }
```

## Rule: `type-narrower/no-wide-parameters`

Options:

```js
"type-narrower/no-wide-parameters": ["warn", {
  // Analysis root. Default: the enclosing git top-level.
  projectRoot: undefined,
  // Open-world mode: skip exported functions (their callers may live
  // outside the repo). Default false.
  respectExports: false,
  // Directory recursion limit passed to the analyzer.
  maxDepth: undefined,
  // How long a project analysis is reused before re-running (ms).
  // Matters only for long-lived processes such as editor integrations;
  // a CLI run reuses one analysis for all files. 0 = never expire.
  cacheMs: 10000,
}]
```

Findings are reported at the function declaration's line. Because the
analysis is project-wide, a warning in file A can be caused by call sites in
file B; each analyzer finding carries up to three example call sites, which
you can inspect by running the `type-narrower` CLI directly.

## Soundness

Everything the analyzer cannot prove degrades to *not reporting*: spread
arguments, `as any`, unresolvable imports, overloads, generics, rest
parameters all suppress findings rather than fabricate them. See the
[project README](https://github.com/IvanovCosmin/type-narrower#the-soundness-invariant)
for the full contract and known limits.
