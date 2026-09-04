// Case 07: functions whose references escape callee position must be skipped,
// even though their direct calls never pass "fax".
type Channel = "email" | "sms" | "push" | "fax";

declare function invokeLater(fn: (c: Channel) => void): void;

// 07a: passed as a callback -> skip.
function sendCallback(channel: Channel): void {
  void channel;
}
sendCallback("email");
invokeLater(sendCallback);
// expected: no finding (function escapes)

// 07b: aliased to a variable -> skip.
function sendAliased(channel: Channel): void {
  void channel;
}
sendAliased("email");
const alias = sendAliased;
alias("sms");
// expected: no finding (function escapes)

export {};
