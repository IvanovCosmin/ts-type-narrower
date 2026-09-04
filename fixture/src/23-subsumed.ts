// Case 23: a constituent subsumed by an observation is used. An argument
// typed `string` against "fast" | string can carry the value "fast".
type SMode = "fast" | string;
function ssend(m: SMode): void {
  void m;
}
declare function getStr(): string;
const sval: string = getStr();
ssend(sval);
// expected: NO finding for ssend

export {};
