use std::path::PathBuf;
use std::process::ExitCode;

use overwide::{analyze, Options};

const USAGE: &str = "\
overwide — find function parameters declared wider than any call site uses

USAGE:
  overwide <dir> [--diff <base>] [--json] [--respect-exports] [--max-depth N] [--timing]
  overwide gen --out <dir> [--files N] [--fns N] [--seed N]

OPTIONS:
  --diff <base>       Only report functions touched by `git diff <base>`
  --json              JSON output
  --respect-exports   Skip exported functions (open-world; default is closed-world)
  --max-depth N       Property recursion depth (default 6)
  --timing            Phase timings on stderr
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    if args[0] == "gen" {
        return match overwide_gen(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let root = PathBuf::from(&args[0]);
    let mut opts = Options::default();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--diff" => {
                i += 1;
                opts.diff_base = Some(args.get(i).cloned().unwrap_or_default());
            }
            "--json" => opts.json = true,
            "--respect-exports" => opts.respect_exports = true,
            "--max-depth" => {
                i += 1;
                opts.max_depth = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(6);
            }
            "--timing" => opts.timing = true,
            other => {
                eprintln!("unknown option: {other}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    match analyze(&root, &opts) {
        Ok(findings) => {
            if opts.json {
                println!("{}", serde_json::to_string_pretty(&findings).unwrap());
            } else if findings.is_empty() {
                println!("no over-wide parameters found");
            } else {
                for f in &findings {
                    let loc = format!("{}:{}", f.file, f.line);
                    let subject = if f.path.is_empty() {
                        format!("{}({})", f.function_name, f.param)
                    } else {
                        format!("{}({}){}", f.function_name, f.param, f.path)
                    };
                    println!(
                        "{loc}  {subject}: declared {}, never passed: {}  [{} call{}]",
                        f.declared,
                        f.unused.join(", "),
                        f.call_count,
                        if f.call_count == 1 { "" } else { "s" }
                    );
                }
                println!("\n{} finding(s)", findings.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `overwide gen`: emit a synthetic project for benchmarking.
fn overwide_gen(args: &[String]) -> Result<(), String> {
    let mut out: Option<PathBuf> = None;
    let mut files = 1000usize;
    let mut fns = 10usize;
    let mut seed = 42u64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out = args.get(i).map(PathBuf::from);
            }
            "--files" => {
                i += 1;
                files = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(files);
            }
            "--fns" => {
                i += 1;
                fns = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(fns);
            }
            "--seed" => {
                i += 1;
                seed = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(seed);
            }
            other => return Err(format!("unknown gen option: {other}")),
        }
        i += 1;
    }
    let out = out.ok_or("gen requires --out <dir>")?;
    overwide::genproj::generate(&out, files, fns, seed)
}
