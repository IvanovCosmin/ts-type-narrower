// Case 11: class methods and object-literal methods.
type Level = "info" | "warn" | "error" | "fatal";

class Logger {
  write(level: Level): void {
    void level;
  }
}

const logger = new Logger();
logger.write("info");
logger.write("warn");
// expected unused for Logger.write: "error", "fatal"

const sink = {
  emit(level: Level): void {
    void level;
  },
};

sink.emit("error");
// expected unused for sink.emit: "info", "warn", "fatal"

export {};
