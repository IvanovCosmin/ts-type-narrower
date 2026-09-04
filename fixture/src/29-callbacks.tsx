// Case 29: functions passed as callbacks. Array higher-order methods with a
// provably-array receiver and JSX intrinsic event handlers become modeled
// calls; anything else (user methods, component props) stays an escape.

// 29a: annotated array receiver narrows the callback's element param.
type Chan = "a" | "b" | "c";
function handle(c: Chan): void {
  void c;
}
const pair: ("a" | "b")[] = ["a", "b"];
pair.forEach(handle);
// expected: handle unused "c"

// 29b: array-literal receiver (as const keeps the literal element types).
function handle2(c: Chan): void {
  void c;
}
(["a", "b"] as const).map(handle2);
// expected: handle2 unused "c"

// 29c: the index param is a plain number (atomic — no finding for it).
function withIdx(c: "x" | "y", i: number): void {
  void c;
  void i;
}
(["x"] as const).forEach(withIdx);
// expected: withIdx unused "y" (param c only)

// 29d: unknown receiver — a user-defined .map could store the callback and
// call it with anything later; must stay an escape.
declare const mystery: { map: (cb: (v: Chan) => void) => void };
function unsafeCb(c: Chan): void {
  void c;
}
mystery.map(unsafeCb);
unsafeCb("a");
// expected: NO finding for unsafeCb

// 29e: JSX intrinsic handler — the DOM contract passes exactly one event
// argument, so the trailing optional param is provably never provided.
function onPress(e: unknown, mode?: "fast" | "slow"): void {
  void e;
  void mode;
}
export const btn = <button onClick={onPress} />;
// expected: onPress param mode unused "fast", "slow"

// 29f: component-prop callback — the component decides how to call it;
// must stay an escape.
function CompCb(props: { onGo: (v: "p" | "q" | "r") => void }): null {
  void props;
  return null;
}
function goCb(v: "p" | "q" | "r"): void {
  void v;
}
goCb("p");
export const comp = <CompCb onGo={goCb} />;
// expected: NO finding for goCb
