// Case 31: forwarding an optional parameter observes `T | undefined`, so the
// callee never gets a false "never passed: undefined".
function inner31(x?: "p" | "q"): void {
  void x;
}
export function outer31(y?: "p" | "q"): void {
  inner31(y);
}
outer31("p");
// expected: NO finding for inner31 (y covers p, q, AND undefined)
// expected for outer31: unused "q"
