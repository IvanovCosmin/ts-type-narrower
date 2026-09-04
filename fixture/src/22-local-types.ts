// Case 22: function-local type declarations shadow module-level ones; an
// annotation mentioning them must resolve to opaque, not the module type.
type Level = "low" | "high";
function lsend(l: Level): void {
  void l;
}
export function helper(): void {
  type Level = "high";
  const x: Level = "high";
  lsend(x); // must NOT be observed as the module-level "low" | "high"
}
lsend("low");
helper();
// expected: NO finding for lsend (local x is opaque; covers all)
