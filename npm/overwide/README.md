# overwide (npm distribution)

Prebuilt binaries for [overwide](https://github.com/IvanovCosmin/overwide),
a static analyzer that finds TypeScript function parameters whose declared
union types are wider than anything the call sites actually pass.

```
npm install -D overwide
npx overwide .                 # analyze the repo
npx overwide . --json          # machine-readable report
```

The right binary for your platform (linux x64/arm64 glibc, linux x64 musl,
macOS x64/arm64, Windows x64) is installed through optionalDependencies. On
other platforms, build from source with `cargo build --release` and set
`OVERWIDE_BINARY` to the binary path.

For ESLint integration use
[eslint-plugin-overwide](https://www.npmjs.com/package/eslint-plugin-overwide),
which depends on this package.

Node API:

```js
const { analyze, binaryPath } = require("overwide");
const report = analyze("/path/to/repo", { respectExports: false });
// { version, findings, stats, uncalled, warnings }
```

See the [project README](https://github.com/IvanovCosmin/overwide) for the
rule definition, soundness contract, CLI flags, and performance numbers.
