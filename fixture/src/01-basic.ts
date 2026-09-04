// Case 01: the canonical example. All calls are literal; "fax" is never passed.
type Channel = "email" | "sms" | "push" | "fax";

function send(channel: Channel): void {
  void channel;
}

send("email");
send("sms");
send("push");

export {};
