// Case 21: block-scoped shadowing and catch params. The scope model is
// function-granular, so shadowed names make calls unattributable — that must
// degrade to an escape/opaque observation, never to a dropped call.
type ShMode = "m1" | "m2" | "m3";
function shsend(mode: ShMode): void {
  void mode;
}
export function driver(flag: boolean): void {
  if (flag) {
    const shsend = (n: number): void => {
      void n;
    };
    shsend(1);
  }
  shsend("m3"); // outer shsend, really passes "m3"
}
shsend("m1");
driver(true);
driver(false);
// expected: NO finding for shsend (escaped via unattributable call)

function cfn(c: "c1" | "c2"): void {
  void c;
}
const e = "unused-outer";
void e;
export function catcher(): void {
  try {
    throw "c2";
  } catch (e: any) {
    cfn(e); // catch param, not the module const -> opaque, covers all
  }
}
cfn("c1");
catcher();
// expected: NO finding for cfn
