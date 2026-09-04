// Case 05: unions buried inside a nested (non-union) object parameter.
// The analyzer must recurse into properties and report per-path.
type Config = {
  mode: "dev" | "prod" | "test";
  opts: {
    level: 1 | 2 | 3;
    log: {
      format: "json" | "pretty" | "syslog";
      color: boolean;
    };
  };
};

function configure(config: Config): void {
  void config;
}

configure({
  mode: "dev",
  opts: { level: 1, log: { format: "json", color: true } },
});
configure({
  mode: "prod",
  opts: { level: 2, log: { format: "pretty", color: true } },
});
// expected unused at .mode: "test"
// expected unused at .opts.level: 3
// expected unused at .opts.log.format: "syslog"
// expected unused at .opts.log.color: false

export {};
