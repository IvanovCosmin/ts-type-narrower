// Case 30: import-then-export barrels (`import {x}; export {x}`) — the form
// that ISN'T `export ... from` — must link calls, or they vanish (FP factory).
export function relay2(c: "x" | "y" | "z"): void {
  void c;
}
relay2("x");
