//! Synthetic-project generator for benchmarking. Deterministic for a given
//! seed; prints the number of findings the analyzer is expected to produce.

use std::fmt::Write as _;
use std::path::Path;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

pub fn generate(out: &Path, files: usize, fns_per_file: usize, seed: u64) -> Result<(), String> {
    std::fs::create_dir_all(out.join("src")).map_err(|e| e.to_string())?;
    let mut rng = Rng(seed | 1);
    let mut expected_findings = 0usize;

    for fi in 0..files {
        let mut s = String::new();
        let variants = 4 + rng.below(5); // 4..8
        let union: Vec<String> = (0..variants).map(|v| format!("\"v{fi}_{v}\"")).collect();
        writeln!(s, "// generated file {fi}").unwrap();
        writeln!(s, "export type U{fi} = {};", union.join(" | ")).unwrap();
        writeln!(
            s,
            "export type Cfg{fi} = {{ mode: {}; opts: {{ level: 1 | 2 | 3; deep: {{ tag: \"x\" | \"y\" | \"z\" }} }} }};",
            union.join(" | ")
        )
        .unwrap();

        if fi > 0 {
            writeln!(s, "import {{ xf{}, type U{} }} from \"./file{}\";", fi - 1, fi - 1, fi - 1).unwrap();
        }
        // Cross-file traffic: exported, called once from the next file with a
        // full-union-typed argument (covers everything -> no finding).
        writeln!(s, "export function xf{fi}(x: U{fi}): void {{ void x; }}").unwrap();

        for k in 0..fns_per_file {
            let style = rng.below(10);
            match style {
                // Nested-object parameter, one unused leaf variant -> 2 findings.
                0 => {
                    writeln!(s, "export function f{fi}_{k}(cfg: Cfg{fi}): void {{ void cfg; }}").unwrap();
                    writeln!(
                        s,
                        "f{fi}_{k}({{ mode: {}, opts: {{ level: 1, deep: {{ tag: \"x\" }} }} }});",
                        union[0]
                    )
                    .unwrap();
                    writeln!(
                        s,
                        "f{fi}_{k}({{ mode: {}, opts: {{ level: 2, deep: {{ tag: \"y\" }} }} }});",
                        union[1]
                    )
                    .unwrap();
                    // .mode: variants-2 unused; .opts.level: 3 unused; .opts.deep.tag: "z" unused
                    expected_findings += if variants > 2 { 3 } else { 2 };
                }
                // Escaped function: unused variants exist but must NOT be reported.
                1 => {
                    writeln!(s, "export function f{fi}_{k}(x: U{fi}): void {{ void x; }}").unwrap();
                    writeln!(s, "f{fi}_{k}({});", union[0]).unwrap();
                    writeln!(s, "export const ref{fi}_{k} = f{fi}_{k};").unwrap();
                }
                // Wide argument: covers everything, no finding.
                2 => {
                    writeln!(s, "declare function get{fi}_{k}(): U{fi};").unwrap();
                    writeln!(s, "export function f{fi}_{k}(x: U{fi}): void {{ void x; }}").unwrap();
                    writeln!(s, "const w{fi}_{k}: U{fi} = get{fi}_{k}();").unwrap();
                    writeln!(s, "f{fi}_{k}(w{fi}_{k});").unwrap();
                }
                // All variants used, no finding.
                3 | 4 | 5 => {
                    writeln!(s, "export function f{fi}_{k}(x: U{fi}): void {{ void x; }}").unwrap();
                    for v in &union {
                        writeln!(s, "f{fi}_{k}({v});").unwrap();
                    }
                }
                // Literal calls leaving exactly one variant unused -> 1 finding.
                _ => {
                    writeln!(s, "export function f{fi}_{k}(x: U{fi}): void {{ void x; }}").unwrap();
                    for v in union.iter().take(variants - 1) {
                        writeln!(s, "f{fi}_{k}({v});").unwrap();
                    }
                    expected_findings += 1;
                }
            }
        }
        if fi > 0 {
            writeln!(s, "xf{}(\"v{}_0\" as U{});", fi - 1, fi - 1, fi - 1).unwrap();
        }
        std::fs::write(out.join("src").join(format!("file{fi}.ts")), s).map_err(|e| e.to_string())?;
    }

    println!(
        "generated {files} files x {fns_per_file} functions under {} (seed {seed})",
        out.display()
    );
    println!("note: cross-file calls pass the full union, so per-function expectations hold; ballpark expected findings: {expected_findings}");
    Ok(())
}
