// Case 35a: `T | any` is `any` — no "never passed: any" nonsense.
function h35(x: "a" | any): void {
  void x;
}
h35("a");
// expected: NO finding for h35

// Case 35b: enum members are number/string subtypes; a number observation
// subsumes numeric members.
enum N35 {
  A,
  B,
}
function en35(x: N35 | number): void {
  void x;
}
declare const num35: number;
en35(N35.A);
en35(num35);
// expected: NO finding for en35

// Case 35c: a bare (un-awaited) import() is a Promise — the namespace escapes
// at the source, so calls through .then() can't produce false findings.
const p35 = import("./34-ns-def");
void p35.then((m) => m.nsr("n2"));
// (nsr already covered by case 34; this must simply not crash or mis-bind)
export {};
