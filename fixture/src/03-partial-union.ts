// Case 03: a union-typed argument subtracts only its own constituents.
type Channel = "email" | "sms" | "push" | "fax";

declare function pick(): "email" | "sms";

function send(channel: Channel): void {
  void channel;
}

const c: "email" | "sms" = pick();
send(c);
// expected unused: "push", "fax"

export {};
