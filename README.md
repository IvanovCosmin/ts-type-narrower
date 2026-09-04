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
unions, nested object types, enums, aliases/interfaces across relative
imports) and bails to "no finding" on anything it cannot prove.

## Usage

```
cargo build --release
./target/release/overwide <dir>                    # analyze every .ts/.tsx under dir
./target/release/overwide <dir> --diff origin/main # only functions touched by the diff
./target/release/overwide <dir> --json
./target/release/overwide <dir> --respect-exports  # open-world: skip exported functions
```

`--diff <base>` runs `git diff -U0 <base>` and restricts *reporting* to
functions whose declaration overlaps a changed line; call-site discovery still
spans the whole project.

By default the analysis assumes a closed world (the whole project is visible,
exported functions included). `--respect-exports` switches to the open-world
assumption from the original design note: an exported function might be called
from outside, so it is skipped.

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
- anything unprovable (spread arguments, wide or opaque argument types,
  `any`/`unknown`) marks *all* constituents as used.

A function is skipped entirely when analysis would be unsound for it:

- any reference outside callee position (passed as callback, aliased, member
  of an escaped object/class value);
- overloads, generics, rest parameters, destructured parameters;
- method calls that cannot be attributed to a tracked object conservatively
  escape every same-named method.

Both failure directions are under-reporting by construction; the tool should
never claim a constituent is unused when some visible call passes it.

## Layout

- `src/` — the analyzer: `extract.rs` (oxc parse → owned IR, parallel),
  `link.rs` (cross-module linking + orchestration), `resolve.rs` (type
  resolution), `narrow.rs` (the narrowing core), `diff.rs`, `genproj.rs`
  (benchmark project generator).
- `fixture/` — a handcrafted TypeScript project with 18 cases and the ground
  truth in `fixture/expected.json`; `npm run check:fixture` typechecks it with
  real tsc. `cargo test` asserts the analyzer's output matches exactly.
- `overwide gen --out <dir> --files N --fns M` — deterministic synthetic
  project generator; it prints the finding count the analyzer must reproduce.

## Performance

Measured in this container (release build, cold file cache irrelevant —
dominated by parse):

| project | files | functions | size | wall time |
|---|---|---|---|---|
| fixture | 19 | 27 | ~30 KB | 3 ms |
| generated | 2,000 | 22,000 | 7.9 MB | 95 ms |
| generated | 10,000 | 110,000 | 40 MB | 532 ms |

Finding counts on the generated projects match the generator's expected count
exactly (14,106 and 70,180).
