// Case 18: arguments whose types are wider than the parameter's union, or
// opaque (any/unknown-ish). The analyzer must NOT report false positives here.
type Channel = "email" | "sms" | "push" | "fax";

// 18a: `as any` argument -> covers everything.
function sendAny(channel: Channel): void {
  void channel;
}
sendAny("email");
sendAny(0 as any);
// expected: no finding

// 18b: mutable variable widened to string, passed with assertion -> the
// observed type "string" maps to no single constituent; treat as covering all.
function sendWide(channel: Channel): void {
  void channel;
}
let s = "email";
sendWide(s as Channel);
// expected: no finding

// 18c: all constituents actually used -> no finding, obviously.
function sendAll(channel: Channel): void {
  void channel;
}
sendAll("email");
sendAll("sms");
sendAll("push");
sendAll("fax");
// expected: no finding

export {};
