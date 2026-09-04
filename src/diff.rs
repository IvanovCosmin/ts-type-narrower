//! `git diff -U0` parsing: which lines changed per file (new side), so the
//! analysis can be scoped to functions a diff touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub type ChangedLines = HashMap<PathBuf, Vec<(u32, u32)>>;

pub fn git_changed_lines(base: &str, cwd: &Path) -> Result<ChangedLines, String> {
    let root = run_git(cwd, &["rev-parse", "--show-toplevel"])?;
    let root = std::fs::canonicalize(root.trim()).map_err(|e| format!("git toplevel: {e}"))?;
    let out = run_git(
        cwd,
        &["diff", "-U0", "--no-color", base, "--", "*.ts", "*.tsx", "*.mts", "*.cts"],
    )?;
    Ok(parse_unified_diff(&out, &root))
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr)));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// Hunk headers on the new-file side. Pure deletions (`+n,0`) still produce a
/// one-line range so a function that only lost lines counts as modified.
///
/// A `+++ ` line is only a header when the previous line was a `--- ` header —
/// with `-U0`, an added source line like `++ x;` renders as `+++ x;` and must
/// not be mistaken for one. Git C-quotes non-ASCII paths (`"b/caf\303\251.ts"`);
/// those are unquoted.
pub fn parse_unified_diff(diff: &str, git_root: &Path) -> ChangedLines {
    let mut result: ChangedLines = HashMap::new();
    let mut current: Option<PathBuf> = None;
    let mut prev_was_minus_header = false;
    for line in diff.lines() {
        if prev_was_minus_header && line.starts_with("+++ ") {
            let p = unquote_git_path(line[4..].trim());
            current = if p == "/dev/null" {
                None
            } else {
                Some(git_root.join(p.strip_prefix("b/").unwrap_or(&p)))
            };
        } else if line.starts_with("@@") {
            if let Some(file) = &current {
                if let Some(range) = parse_hunk_new_side(&line[2..]) {
                    result.entry(file.clone()).or_default().push(range);
                }
            }
        }
        prev_was_minus_header = line.starts_with("--- ");
    }
    result
}

/// Undo git's C-style quoting: `"b/caf\303\251.ts"` -> `b/café.ts`.
fn unquote_git_path(p: &str) -> String {
    if !(p.starts_with('"') && p.ends_with('"') && p.len() >= 2) {
        return p.to_string();
    }
    let inner = &p[1..p.len() - 1];
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let c = bytes[i + 1];
            match c {
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'\\' | b'"' => {
                    out.push(c);
                    i += 2;
                }
                b'0'..=b'7' if i + 3 < bytes.len() && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b)) => {
                    let v = (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
                    out.push(v);
                    i += 4;
                }
                _ => {
                    out.push(c);
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_hunk_new_side(header: &str) -> Option<(u32, u32)> {
    let plus = header.split('+').nth(1)?;
    let spec: String = plus.chars().take_while(|c| c.is_ascii_digit() || *c == ',').collect();
    let mut it = spec.split(',');
    let start: u32 = it.next()?.parse().ok()?;
    let count: u32 = match it.next() {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    Some(if count == 0 { (start.max(1), start.max(1)) } else { (start, start + count - 1) })
}

pub fn intersects(ranges: &[(u32, u32)], start: u32, end: u32) -> bool {
    ranges.iter().any(|(s, e)| *s <= end && start <= *e)
}
