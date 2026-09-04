// Case 13: enum-typed parameter behaves like a union of member literal types.
enum Priority {
  Low,
  Medium,
  High,
  Critical,
}

function schedule(priority: Priority): void {
  void priority;
}

schedule(Priority.Low);
schedule(Priority.Medium);
// expected unused: Priority.High, Priority.Critical

export {};
