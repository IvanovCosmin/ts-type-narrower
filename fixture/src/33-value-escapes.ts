// Case 33: instances and methods reachable through untracked value positions
// must escape, never silently vanish.
class W33 {
  m(x: "a" | "b"): void {
    void x;
  }
}
declare function reg33(u: unknown): void;
reg33(new W33()); // instance escapes: unknown code may call m with anything
const w33 = new W33();
w33.m("a");
// expected: NO finding for W33.m

const obj33 = {
  go(x: "g1" | "g2"): void {
    void x;
  },
};
obj33.go("g1");
obj33["go"]("g2"); // computed member call: escapes obj33's members
// expected: NO finding for obj33.go
