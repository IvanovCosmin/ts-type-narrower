# type-narrower

Static analysis for TypeScript that finds function parameters whose declared
union types are wider than anything the call sites actually pass.

```ts
type Channel = "email" | "sms" | "push" | "fax";
function send(channel: Channel) {}
send("email"); send("sms"); send("push");
// -> send(channel): never passed: "fax"
```

Written in Rust on top of the [oxc](https://oxc.rs) parser, with rayon for
per-file parallelism. There is no TypeScript type checker underneath: the tool
carries its own conservative resolver for the subset it analyzes (literal
unions, nested object types, enums, aliases/interfaces across imports) and
bails to "no finding" on anything it cannot prove.

## ESLint plugin

type-narrower ships as an npm package pair:
[`eslint-plugin-type-narrower`](https://www.npmjs.com/package/eslint-plugin-type-narrower)
(the plugin) and [`type-narrower`](https://www.npmjs.com/package/type-narrower) (the CLI
with prebuilt binaries per platform, pulled in automatically). The plugin runs
the whole-project analysis once per lint run and maps findings onto each
linted file:

```js
// eslint.config.js
import type-narrower from "eslint-plugin-type-narrower";
export default [
  // ...your typescript-eslint setup...
  type-narrower.configs.recommended,
];
```

See `npm/eslint-plugin-type-narrower/README.md` for rule options
(`projectRoot`, `respectExports`, `cacheMs`, ...) and the legacy
`.eslintrc` form.

Releasing: bump the version in `Cargo.toml`, `npm/type-narrower/package.json`
(including its optionalDependencies), and
`npm/eslint-plugin-type-narrower/package.json` (including its `type-narrower`
dependency), then push a `v<version>` tag. `.github/workflows/release.yml`
cross-builds the six platform binaries and publishes all eight packages to
npm; it needs an `NPM_TOKEN` repository secret (npm automation token).

## Usage

```
cargo build --release
type-narrower <dir|file>                    # analyze every .ts/.tsx/.mts/.cts under dir
type-narrower <dir> --diff 'origin/main...HEAD'
type-narrower <dir> --json                  # {version, findings, stats, uncalled*, warnings}
type-narrower <dir> --list-uncalled         # dead-function candidates (never called)
type-narrower <dir> --fail-on-findings      # exit 1 when findings exist
type-narrower <dir> --respect-exports       # open-world: skip exported functions
type-narrower <dir> --max-depth N --quiet --timing --version
```

Exit codes: 0 = ran (findings or not); 1 = findings with `--fail-on-findings`;
2 = usage/IO/git error. `--quiet` suppresses only the stderr summary line —
soundness warnings (parse errors, unreadable files, sub-root analysis) always
print and are included in the JSON report's `warnings`.

`--diff <base>` runs `git diff -U0 <base>` and restricts *reporting* to
functions whose declaration overlaps a changed line; call-site discovery still
spans the whole project. In CI, prefer the merge-base form
(`--diff 'origin/main...HEAD'`) so the base branch's own movement isn't blamed
on the PR, and fetch enough history for the merge base to exist.

Each finding includes up to three example call sites as evidence. A summary
line on stderr reports files, functions, how many were analyzed, and warnings
for parse errors or unreadable files (suppress with `--quiet`).

**Run at the repository root.** The closed-world assumption is only sound over
the whole repo; analyzing a sub-package hides its external callers, and the
tool warns when the analysis root is not the git top-level.

## The rule

For a function `f`, only direct `CallExpression`s (and JSX usages of function
components) whose callee resolves to `f` are considered. A union constituent is
reported only when **every** observed call provably excludes it:

- literal arguments count as their literal type;
- identifiers count as their `const`/parameter annotation, or their literal
  initializer for un-annotated `const`s;
- `x as T` counts as `T` (so `as any` covers everything);
- object literals are matched structurally against object/variant types, and
  the analysis recurses into nested (non-union) object properties, reporting
  per property path (`configure(config).opts.level: never passed: 3`);
- `enum` parameters narrow by member (`Priority.High` never passed);
- `boolean` narrows as `true | false`; `undefined` introduced by `?` or a
  default initializer is never reported;
- a constituent subsumed by an observation counts as used (an argument typed
  `string` against `"fast" | string` can carry `"fast"`);
- anything unprovable (spread arguments, wide or opaque argument types,
  `any`/`unknown`) marks *all* constituents as used.

## The soundness invariant

Every failure mode must degrade to *under-reporting*. The linker enforces one
rule everywhere: **a reference that cannot be resolved to a specific
declaration never disappears — it escapes every declaration it could plausibly
denote.** Concretely:

- calls through unresolvable imports (external packages, unknown aliases)
  escape every same-named exported function;
- unattributable identifier calls (shadowed or unbound callees) escape every
  same-named free function;
- unattributable method calls and property accesses escape every same-named
  method *and* free function (namespace-like objects can carry both);
- a tracked object/class/namespace binding used as a value escapes all of its
  members / the source module's exports;
- inherited or unknown methods on resolved classes fall back to name-based
  escapes.

Module resolution covers: relative imports (including dotted filenames like
`foo.service.ts` and `.js`/`.mjs`-suffixed NodeNext specifiers), named and
default exports/imports, namespace imports (`import * as ns` — `ns.f(...)` is
a real call), `const m = await import("./x")` and `require("./x")`, re-export
chains in every form (`export { x } from`, `export * from`,
`export * as ns from`, and import-then-export barrels), tsconfig
`paths`/`baseUrl` from every `tsconfig*.json` under the root (BOM-tolerant
JSONC), and workspace package names (`package.json` `name` fields). A private
local declaration never satisfies an import of the same name.

A function is skipped entirely when analysis would be unsound for it:
any reference outside callee position, overloads, generics, rest parameters.
Destructured object parameters are analyzed against their annotation
(`function Badge({ variant }: BadgeProps)` reports per-property paths), and
each destructured name binds to a property projection of the annotation, so a
forwarded prop narrows its callee too; array-pattern parameters are opaque. Scope handling is function-granular: every name
declared anywhere in a function body shadows outer scopes for the whole
function, so block-scoped shadowing degrades to escapes/opaque observations
rather than wrong bindings; annotations mentioning function-local type
declarations resolve to opaque.

Functions passed as callbacks are modeled instead of escaped in two cases
where the invocation contract is statically known: array higher-order methods
(`map`, `forEach`, `filter`, `find`, `some`, `every`, `flatMap`, `sort`, …)
when the receiver is provably an array (an `E[]`/`Array<E>` annotation, an
array literal, or an `as const`/`as E[]` cast) — the callback observes
(element, index, array); and JSX intrinsic-element handlers
(`<button onClick={h}/>`), which the DOM/JSX runtime invokes with exactly one
event argument, so trailing optional parameters are provably never provided.
Callbacks passed to user methods, component props, or `addEventListener`
still escape — those receivers can call with anything.

Known limits (all degrade to under-reporting, never over-reporting): unions
are terminal (no per-variant recursion), tuples/generics/intersections
are opaque (array types are modeled), class components and `this.method()` escape broadly, symlinked
directories are skipped, and files with parse errors or non-UTF8 content are
analyzed partially with a loud stderr warning (their missing call sites are
the one place the guarantee is knowingly best-effort).

## Layout

- `src/` — the analyzer: `extract.rs` (oxc parse → owned IR, parallel),
  `workspace.rs` (tsconfig paths + workspace package names), `link.rs`
  (cross-module linking + taints + orchestration), `resolve.rs` (memoized type
  resolution), `narrow.rs` (the narrowing core), `diff.rs`, `genproj.rs`.
- `fixture/` — a handcrafted TypeScript project with 35 cases (including
  regression cases for barrel files, default/namespace imports, shadowing,
  local type shadowing, subsumed constituents, dotted filenames, JSX member
  tags) and ground truth in `fixture/expected.json`; `npm run check:fixture`
  typechecks it with real tsc. `cargo test` asserts exact agreement.
- `type-narrower gen --out <dir> --files N --fns M` — deterministic synthetic
  project generator; it prints the finding count the analyzer must reproduce.

## Performance

Measured in this container (release build):

| project | files | functions | size | wall time |
|---|---|---|---|---|
| fixture | 46 | ~75 | ~25 KB | 6 ms |
| zod (real) | 505 | 1,318 | — | 86 ms |
| excalidraw (real) | 629 | 2,138 | — | 114 ms |
| bluesky social-app (real) | 1,802 | 3,991 | — | 141 ms |
| outline (real) | 2,178 | 3,313 | — | 180 ms |
| generated | 2,000 | 22,000 | 7.9 MB | 154 ms |
| generated | 10,000 | 110,000 | 40 MB | ~700 ms |

Finding counts on generated projects match the generator's expected count
exactly. Pathological inputs that previously degraded — 2,000-constituent
unions (15 s → 90 ms via observed-set dedup + literal indexing) and
property-access escape floods (O(files²) → linear via per-module escape
dedup) — are covered by the link/narrow fast paths; deep alias chains and
nested types hit depth caps instead of overflowing the stack.
