// Case 32: block scoping. A block-shadowed const must bind precisely inside
// the block and NOT leak past it — both directions previously manufactured
// false positives.
function f32(x: "a" | "b" | "c"): void {
  void x;
}
const v32: "a" = "a";
f32(v32);
{
  const v32: "c" = "c";
  f32(v32); // observes "c", not the outer "a"
}
f32("b");
// expected: NO finding for f32 (a, c, b all observed)

export function run32(): void {
  const w: "c" = "c";
  {
    const w: "a" = "a";
    g32(w);
  }
  g32(w); // outer w: really passes "c"
}
function g32(v: "a" | "b" | "c"): void {
  void v;
}
g32("b");
run32();
// expected: NO finding for g32 (a, c, b all observed)
