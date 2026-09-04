// Case 19: calls arriving through barrel re-exports (named and star) must be
// linked; before the fix they were dropped, manufacturing a false "r1"/"r2".
export function relay(c: "r1" | "r2" | "r3"): void {
  void c;
}
