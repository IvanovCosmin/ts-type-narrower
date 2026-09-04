// Case 24: dotted module filenames (NestJS-style *.service.ts) must resolve;
// with_extension()-style resolution ate the ".service" segment.
export function dsend(c: "s1" | "s2" | "s3"): void {
  void c;
}
dsend("s1");
