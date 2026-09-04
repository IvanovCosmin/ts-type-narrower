# overwide

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

## Usage

```
cargo build --release
./target/release/overwide <dir>                      # analyze every .ts/.tsx under dir
./target/release/overwide <dir> --diff 'origin/main...HEAD'
./target/release/overwide <dir> --json
./target/release/overwide <dir> --fail-on-findings   # exit 1 when findings exist
./target/release/overwide <dir> --respect-exports    # open-world: skip exported functions
```

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
a real call), `const m = await import("./x")`, re-export chains
(`export { x } from`, `export * from`, barrel files), tsconfig
`paths`/`baseUrl` mappings, and workspace package names (`package.json`
`name` fields found under the analysis root).

A function is skipped entirely when analysis would be unsound for it:
any reference outside callee position, overloads, generics, rest parameters,
destructured parameters. Scope handling is function-granular: every name
declared anywhere in a function body shadows outer scopes for the whole
function, so block-scoped shadowing degrades to escapes/opaque observations
rather than wrong bindings; annotations mentioning function-local type
declarations resolve to opaque.

Known limits (all degrade to under-reporting, never over-reporting): unions
are terminal (no per-variant recursion), arrays/tuples/generics/intersections
are opaque, class components and `this.method()` escape broadly, symlinked
directories are skipped, and files with parse errors or non-UTF8 content are
analyzed partially with a loud stderr warning (their missing call sites are
the one place the guarantee is knowingly best-effort).

## Layout

- `src/` — the analyzer: `extract.rs` (oxc parse → owned IR, parallel),
  `workspace.rs` (tsconfig paths + workspace package names), `link.rs`
  (cross-module linking + taints + orchestration), `resolve.rs` (memoized type
  resolution), `narrow.rs` (the narrowing core), `diff.rs`, `genproj.rs`.
- `fixture/` — a handcrafted TypeScript project with 25 cases (including
  regression cases for barrel files, default/namespace imports, shadowing,
  local type shadowing, subsumed constituents, dotted filenames, JSX member
  tags) and ground truth in `fixture/expected.json`; `npm run check:fixture`
  typechecks it with real tsc. `cargo test` asserts exact agreement.
- `overwide gen --out <dir> --files N --fns M` — deterministic synthetic
  project generator; it prints the finding count the analyzer must reproduce.

## Performance

Measured in this container (release build):

| project | files | functions | size | wall time |
|---|---|---|---|---|
| fixture | 29 | ~40 | ~35 KB | 4 ms |
| zod (real) | 505 | 1,318 | — | 86 ms |
| excalidraw (real) | 629 | 2,138 | — | 114 ms |
| generated | 2,000 | 22,000 | 7.9 MB | 154 ms |
| generated | 10,000 | 110,000 | 40 MB | ~600 ms |

Finding counts on generated projects match the generator's expected count
exactly. Pathological inputs that previously degraded — 2,000-constituent
unions (15 s → 90 ms via observed-set dedup + literal indexing) and
property-access escape floods (O(files²) → linear via per-module escape
dedup) — are covered by the link/narrow fast paths; deep alias chains and
nested types hit depth caps instead of overflowing the stack.
