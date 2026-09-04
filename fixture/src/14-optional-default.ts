// Case 14: optional and defaulted parameters.
type Mode = "fast" | "slow" | "adaptive";

// 14a: optional param. Omission counts as undefined; the undefined constituent
// that optionality itself introduces is never reported (it would be pure noise).
function run(mode?: Mode): void {
  void mode;
}
run();
run("fast");
// expected unused: "slow", "adaptive"   (not: undefined)

// 14b: defaulted param. Omission counts as the default initializer's type.
function walk(mode: Mode = "slow"): void {
  void mode;
}
walk();
walk("fast");
// expected unused: "adaptive"

// 14c: boolean narrowing: only true is ever passed.
function toggle(enabled: boolean): void {
  void enabled;
}
toggle(true);
toggle(true);
// expected unused: false

export {};
