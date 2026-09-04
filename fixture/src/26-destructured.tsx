// Case 26: destructured object parameters — the dominant React pattern.
// The parameter analyzes against its annotation, and destructured names bind
// to property projections so forwarded props narrow callees too.
type ChipProps = {
  tone: "t1" | "t2" | "t3";
  size: "s" | "m" | "l";
};

function Chip({ tone, size }: ChipProps): null {
  void tone;
  void size;
  return null;
}

export const c1 = <Chip tone="t1" size="s" />;
export const c2 = <Chip tone="t2" size="s" />;
// expected unused at .tone: "t3"
// expected unused at .size: "m", "l"

// 26b: projection forwarding — `tone` here is typed "p1" | "p2" via the
// destructuring, which must narrow pinner's parameter.
function pinner(t: "p1" | "p2" | "p3"): void {
  void t;
}
function pouter({ tone }: { tone: "p1" | "p2" }): void {
  pinner(tone);
}
pouter({ tone: "p1" });
pouter({ tone: "p2" });
// expected for pinner: unused "p3"
// expected for pouter at .tone: nothing (both passed)

// 26c: renamed + nested destructuring forwards precisely as well.
function qinner(lv: 1 | 2 | 9): void {
  void lv;
}
function qouter({ opts: { level: lv } }: { opts: { level: 1 | 2 } }): void {
  qinner(lv);
}
qouter({ opts: { level: 1 } });
qouter({ opts: { level: 2 } });
// expected for qinner: unused 9
// expected for qouter at .opts.level: nothing

// 26d: default in the destructuring widens the projection (includes
// undefined) — must not create a false finding on rinner.
function rinner(f: boolean): void {
  void f;
}
function router({ flag = false }: { flag?: boolean }): void {
  rinner(flag);
}
router({});
router({ flag: true });
router({ flag: false });
// expected for rinner: nothing (projection is opaque-wide about undefined)
// expected for router at .flag: nothing (true, false, and omitted all seen)
