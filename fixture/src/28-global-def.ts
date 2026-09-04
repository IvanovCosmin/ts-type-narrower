// Case 28: a module-scoped function shadowing a global name. Unbound calls to
// the global elsewhere must NOT escape it — module scope is unreachable from
// other modules without an import.
function isNaN(x: "a" | "b"): void {
  void x;
}
isNaN("a");
// expected unused: "b"

export {};
