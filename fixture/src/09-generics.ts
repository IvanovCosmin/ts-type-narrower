// Case 09: generic function -> skipped in the MVP.
type Channel = "email" | "sms" | "push" | "fax";

function send<T extends Channel>(channel: T): void {
  void channel;
}

send("email");
// expected: no finding (generics skipped)

export {};
