# type-narrower (npm distribution)

Prebuilt binaries for [type-narrower](https://github.com/IvanovCosmin/ts-type-narrower),
a static analyzer that finds TypeScript function parameters whose declared
union types are wider than anything the call sites actually pass.

```
npm install -D type-narrower
npx type-narrower .                 # analyze the repo
npx type-narrower . --json          # machine-readable report
```

The right binary for your platform (linux x64/arm64 glibc, linux x64 musl,
macOS x64/arm64, Windows x64) is installed through optionalDependencies. On
other platforms, build from source with `cargo build --release` and set
`TYPE_NARROWER_BINARY` to the binary path.

For ESLint integration use
[eslint-plugin-type-narrower](https://www.npmjs.com/package/eslint-plugin-type-narrower),
which depends on this package.

Node API:

```js
const { analyze, binaryPath } = require("type-narrower");
const report = analyze("/path/to/repo", { respectExports: false });
// { version, findings, stats, uncalled, warnings }
```

See the [project README](https://github.com/IvanovCosmin/ts-type-narrower) for the
rule definition, soundness contract, CLI flags, and performance numbers.
