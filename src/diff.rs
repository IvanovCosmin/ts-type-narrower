//! `git diff -U0` parsing: which lines changed per file (new side), so the
//! analysis can be scoped to functions a diff touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub type ChangedLines = HashMap<PathBuf, Vec<(u32, u32)>>;

pub fn git_changed_lines(base: &str, cwd: &Path) -> Result<ChangedLines, String> {
    let root = run_git(cwd, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root.trim());
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
pub fn parse_unified_diff(diff: &str, git_root: &Path) -> ChangedLines {
    let mut result: ChangedLines = HashMap::new();
    let mut current: Option<PathBuf> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let p = rest.trim();
            current = if p == "/dev/null" {
                None
            } else {
                Some(git_root.join(p.strip_prefix("b/").unwrap_or(p)))
            };
        } else if let Some(file) = &current {
            if let Some(rest) = line.strip_prefix("@@") {
                if let Some(range) = parse_hunk_new_side(rest) {
                    result.entry(file.clone()).or_default().push(range);
                }
            }
        }
    }
    result
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
