// Case 12: arrow function assigned to a const.
type Channel = "email" | "sms" | "push" | "fax";

const send = (channel: Channel): void => {
  void channel;
};

send("email");
send("fax");
// expected unused: "sms", "push"

export {};
