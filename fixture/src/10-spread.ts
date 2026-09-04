// Case 10: a spread argument means we cannot see what is passed -> that call
// covers everything, so no finding despite the literal calls.
type Channel = "email" | "sms" | "push" | "fax";

declare const mystery: [Channel];

function send(channel: Channel): void {
  void channel;
}

send("email");
send(...mystery);
// expected: no finding (spread call is opaque)

export {};
