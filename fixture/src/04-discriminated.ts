// Case 04: discriminated union of object variants. The "admin" variant is never constructed.
type Request =
  | { type: "user"; id: number }
  | { type: "post"; slug: string }
  | { type: "comment"; postId: number; index: number }
  | { type: "admin"; token: string };

function process(request: Request): void {
  void request;
}

process({ type: "user", id: 1 });
process({ type: "post", slug: "hello" });
process({ type: "comment", postId: 1, index: 0 });
// expected unused: { type: "admin"; token: string }

export {};
