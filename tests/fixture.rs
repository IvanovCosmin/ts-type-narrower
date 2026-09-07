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

    let findings = type_narrower::analyze(&root, &type_narrower::Options::default()).unwrap().findings;

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
    let opts = type_narrower::Options { respect_exports: true, ..Default::default() };
    let findings = type_narrower::analyze(&root, &opts).unwrap().findings;
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
    let changed = type_narrower::diff::parse_unified_diff(diff, &root);
    let ranges = changed.get(&root.join("src/a.ts")).unwrap();
    assert_eq!(ranges, &vec![(11, 13), (25, 25)]);
    assert!(type_narrower::diff::intersects(ranges, 12, 40));
    assert!(!type_narrower::diff::intersects(ranges, 14, 24));
}

#[test]
fn diff_parser_ignores_spoofed_headers() {
    // With -U0, an added source line `++ x;` renders as `+++ x;` and must not
    // be mistaken for a file header (only `+++ ` after `--- ` counts).
    let diff = "\
--- a/src/a.ts
+++ b/src/a.ts
@@ -1,0 +2,1 @@
+++ x;
@@ -9,0 +10,1 @@
+y
";
    let root = PathBuf::from("/repo");
    let changed = type_narrower::diff::parse_unified_diff(diff, &root);
    assert_eq!(changed.len(), 1, "spoofed header created a phantom file: {changed:?}");
    let ranges = changed.get(&root.join("src/a.ts")).unwrap();
    assert_eq!(ranges, &vec![(2, 2), (10, 10)]);
}

#[test]
fn diff_parser_unquotes_c_quoted_paths() {
    let diff = "\
--- \"a/src/caf\\303\\251.ts\"
+++ \"b/src/caf\\303\\251.ts\"
@@ -1,1 +1,1 @@
";
    let root = PathBuf::from("/repo");
    let changed = type_narrower::diff::parse_unified_diff(diff, &root);
    assert!(changed.contains_key(&root.join("src/café.ts")), "got: {changed:?}");
}
