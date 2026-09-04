//! Runs the analyzer over the fixture project and compares against the
//! hand-written ground truth in fixture/expected.json.

use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(serde::Deserialize)]
struct Expected {
    findings: Vec<ExpectedFinding>,
}

#[derive(serde::Deserialize)]
struct ExpectedFinding {
    file: String,
    #[serde(rename = "function")]
    function_name: String,
    param: String,
    path: String,
    unused: Vec<String>,
}

fn key(file: &str, func: &str, param: &str, path: &str, unused: &[String]) -> String {
    let mut u: Vec<&str> = unused.iter().map(|s| s.as_str()).collect();
    u.sort();
    format!("{file}|{func}|{param}|{path}|{}", u.join(","))
}

#[test]
fn fixture_matches_expected() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixture");
    let expected: Expected =
        serde_json::from_str(&std::fs::read_to_string(root.join("expected.json")).unwrap()).unwrap();

    let findings = overwide::analyze(&root, &overwide::Options::default()).unwrap();

    let got: BTreeSet<String> = findings
        .iter()
        .map(|f| key(&f.file, &f.function_name, &f.param, &f.path, &f.unused))
        .collect();
    let want: BTreeSet<String> = expected
        .findings
        .iter()
        .map(|f| key(&f.file, &f.function_name, &f.param, &f.path, &f.unused))
        .collect();

    let missing: Vec<_> = want.difference(&got).collect();
    let unexpected: Vec<_> = got.difference(&want).collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "missing findings:\n  {}\nunexpected findings:\n  {}",
        missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  "),
        unexpected.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  "),
    );
}

#[test]
fn respect_exports_drops_exported() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixture");
    let opts = overwide::Options { respect_exports: true, ..Default::default() };
    let findings = overwide::analyze(&root, &opts).unwrap();
    // 06-cross-file-def.ts `log` is exported and must disappear in open-world mode.
    assert!(
        !findings.iter().any(|f| f.file.contains("06-cross-file-def")),
        "exported function still reported under --respect-exports"
    );
}

#[test]
fn diff_parser_basics() {
    let diff = "\
--- a/src/a.ts
+++ b/src/a.ts
@@ -10,0 +11,3 @@
+x
+y
+z
@@ -20,2 +25,0 @@
";
    let root = PathBuf::from("/repo");
    let changed = overwide::diff::parse_unified_diff(diff, &root);
    let ranges = changed.get(&root.join("src/a.ts")).unwrap();
    assert_eq!(ranges, &vec![(11, 13), (25, 25)]);
    assert!(overwide::diff::intersects(ranges, 12, 40));
    assert!(!overwide::diff::intersects(ranges, 14, 24));
}
