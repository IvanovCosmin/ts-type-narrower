// Case 06: exported function, callers live in another file (06-cross-file-use.ts).
// Under the default closed-world assumption this is analyzed like any other function.
export type Verbosity = "silent" | "normal" | "debug" | "trace";

export function log(verbosity: Verbosity): void {
  void verbosity;
}
// expected unused (closed world): "trace"
