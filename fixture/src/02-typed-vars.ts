// Case 02: arguments that are variables, not literals.
type Channel = "email" | "sms" | "push" | "fax";

declare function getChannel(): Channel;

// 02a: variable annotated with a narrow literal type -> counts as that literal.
function sendA(channel: Channel): void {
  void channel;
}
const narrow: "sms" = "sms";
sendA("email");
sendA(narrow);
// expected unused: "push", "fax"

// 02b: variable typed as the full union -> covers everything, no finding.
function sendB(channel: Channel): void {
  void channel;
}
const wide: Channel = getChannel();
sendB(wide);
// expected: no finding

// 02c: forwarding a parameter typed as the full union -> covers everything.
function sendC(channel: Channel): void {
  void channel;
}
function forward(c: Channel): void {
  sendC(c);
}
forward(getChannel());
// expected: no finding for sendC.
// forward itself: its only call passes Channel (return of getChannel) -> no finding either.

export {};
