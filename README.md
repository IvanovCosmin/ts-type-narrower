# type-narrower

Static analysis for TypeScript that finds function parameters whose declared
union types are wider than anything the call sites actually pass.

```ts
type Channel = "email" | "sms" | "push" | "fax";
function send(channel: Channel) {}
send("email"); send("sms"); send("push");
```

```
src/notify.ts:4  send(channel): declared "email" | "sms" | "push" | "fax",
                 never passed: "fax"  [3 calls: src/notify.ts:8, src/notify.ts:9, src/notify.ts:10]
```

`Channel` should have been three constituents. `"fax"` is either dead code the
compiler will never flag, or a case someone forgot to wire up.

Written in Rust on top of the [oxc](https://oxc.rs) parser, with rayon for
per-file parallelism. There is no TypeScript type checker underneath: the tool
carries its own conservative resolver for the subset it analyzes (literal
unions, nested object types, enums, aliases/interfaces across imports) and
bails to "no finding" on anything it cannot prove.

## What it finds

Each case below is taken from `fixture/`, with the tool's own output beneath
it (long call-site lists elided).

**Object properties, per path.** Nested non-union objects are walked, and each
leaf union is reported separately:

```ts
type Config = {
  mode: "dev" | "prod" | "test";
  opts: { level: 1 | 2 | 3; log: { format: "json" | "pretty" | "syslog"; color: boolean } };
};
function configure(config: Config) {}

configure({ mode: "dev",  opts: { level: 1, log: { format: "json",   color: true } } });
configure({ mode: "prod", opts: { level: 2, log: { format: "pretty", color: true } } });
```

```
src/05-nested.ts:14  configure(config).mode: declared "dev" | "prod" | "test", never passed: "test"
src/05-nested.ts:14  configure(config).opts.level: declared 1 | 2 | 3, never passed: 3
src/05-nested.ts:14  configure(config).opts.log.color: declared boolean, never passed: false
src/05-nested.ts:14  configure(config).opts.log.format: declared "json" | "pretty" | "syslog", never passed: "syslog"
```

`boolean` narrows as `true | false`, so a flag that is only ever passed `true`
is a finding too.

**Discriminated unions.** Object literals are matched structurally against
variants, so a variant nobody constructs is reported whole:

```ts
type Request =
  | { type: "user"; id: number }
  | { type: "post"; slug: string }
  | { type: "comment"; postId: number; index: number }
  | { type: "admin"; token: string };
function process(request: Request) {}

process({ type: "user", id: 1 });
process({ type: "post", slug: "hello" });
process({ type: "comment", postId: 1, index: 0 });
```

```
src/04-discriminated.ts:8  process(request): declared { type: "user"; id: number; } | { type: "post"; slug: string; }
                           | { type: "comment"; postId: number; index: number; } | { type: "admin"; token: string; },
                           never passed: { type: "admin"; token: string; }  [3 calls: …]
```

**Enums**, by member:

```ts
enum Priority { Low, Medium, High, Critical }
function schedule(priority: Priority) {}
schedule(Priority.Low);
schedule(Priority.Medium);
```

```
src/13-enums.ts:9  schedule(priority): declared Priority.Low | Priority.Medium | Priority.High | Priority.Critical,
                   never passed: Priority.High, Priority.Critical  [2 calls: …]
```

**React props.** JSX usage of a function component is a call, and the
attributes object is the argument. Destructured parameters analyze against
their annotation:

```tsx
type ChipProps = { tone: "t1" | "t2" | "t3"; size: "s" | "m" | "l" };
function Chip({ tone, size }: ChipProps) { return null; }

export const c1 = <Chip tone="t1" size="s" />;
export const c2 = <Chip tone="t2" size="s" />;
```

```
src/26-destructured.tsx:9  Chip({tone, size}).size: declared "s" | "m" | "l", never passed: "m", "l"
src/26-destructured.tsx:9  Chip({tone, size}).tone: declared "t1" | "t2" | "t3", never passed: "t3"
```

**Forwarded values narrow transitively.** Each destructured name binds to a
property projection, so a prop handed to another function carries its narrowed
type with it — `pinner` is reported even though nothing calls it with a literal:

```ts
function pinner(t: "p1" | "p2" | "p3") {}
function pouter({ tone }: { tone: "p1" | "p2" }) { pinner(tone); }
pouter({ tone: "p1" });
pouter({ tone: "p2" });
```

```
src/26-destructured.tsx:22  pinner(t): declared "p1" | "p2" | "p3", never passed: "p3"  [1 call: …]
```

Call sites in other files count the same way — declaration and evidence
routinely live in different modules, which is why the analysis is
whole-project rather than per-file.

Under ESLint the same finding reads:

```
warning  Parameter 'channel' of 'send' is declared as "email" | "sms" | "push" | "fax"
         but its 3 call site(s) never pass: "fax".
```

## ESLint plugin

type-narrower ships as an npm package pair:
[`eslint-plugin-type-narrower`](https://www.npmjs.com/package/eslint-plugin-type-narrower)
(the plugin) and [`type-narrower`](https://www.npmjs.com/package/type-narrower) (the CLI
with prebuilt binaries per platform, pulled in automatically). The plugin runs
the whole-project analysis once per lint run and maps findings onto each
linted file:

```js
// eslint.config.js
import typeNarrower from "eslint-plugin-type-narrower";
export default [
  // ...your typescript-eslint setup...
  typeNarrower.configs.recommended,
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

## What is excepted from analysis

Everything the tool cannot prove degrades to silence, never to a false
finding. A function is excluded outright, and will never be reported, when it:

- is generic, is overloaded, takes a rest parameter, or takes no parameters;
- is referenced anywhere outside callee position — passed as a value, stored
  in an object, exported as a value — since any holder can call it;
- is reachable through a call the linker cannot attribute to one declaration
  (a call through an unresolvable import, a shadowed or unbound callee, a
  method on an unknown receiver): every same-named function escapes;
- is a class method reached via `this.method()`, via a class or namespace
  binding used as a value, or by inheritance from an unknown base;
- is passed as a callback, except to array higher-order methods on a provably
  array receiver (`map`, `filter`, `sort`, …) and to JSX intrinsic handlers
  (`<button onClick={h}/>`), where the invocation contract is statically known.

Individual parameters stay wide when their type is a tuple, an intersection, a
generic instantiation, or an array-destructuring pattern; unions are terminal,
so there is no recursion into a union's variants. A single unprovable argument
at any call site — a spread, an `as any`, an `any`/`unknown`-typed value —
marks every constituent of that parameter as used.

Module resolution covers relative imports (dotted filenames, `.js`/`.mjs`
NodeNext specifiers), named/default/namespace imports, dynamic `import()` and
`require()`, re-export chains and barrels, tsconfig `paths`/`baseUrl`, and
workspace package names. Scope handling is function-granular: a name declared
anywhere in a body shadows outer scopes for the whole function, so
block-scoped shadowing degrades to opaque observations rather than wrong
bindings.

The one place the guarantee is knowingly best-effort: files with parse errors
or non-UTF8 content are analyzed partially, and their missing call sites can
produce a wrong finding. Those files are named in a loud stderr warning and in
the JSON report's `warnings`.

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
