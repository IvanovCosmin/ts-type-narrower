// Case 08: overloaded function -> skipped in the MVP.
type Channel = "email" | "sms" | "push" | "fax";

function send(channel: "email" | "sms"): void;
function send(channel: "push"): void;
function send(channel: Channel): void {
  void channel;
}

send("email");
// expected: no finding (overloads skipped)

export {};
