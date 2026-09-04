// Case 17: several parameters analyzed independently in one function.
type Method = "GET" | "POST" | "PUT" | "DELETE";
type Encoding = "json" | "form" | "binary";

function request(method: Method, encoding: Encoding, retries: 0 | 1 | 3 | 5): void {
  void method;
  void encoding;
  void retries;
}

request("GET", "json", 0);
request("POST", "json", 3);
// expected unused at method: "PUT", "DELETE"
// expected unused at encoding: "form", "binary"
// expected unused at retries: 1, 5

export {};
