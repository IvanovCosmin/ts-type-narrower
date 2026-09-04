//! Non-relative import resolution: tsconfig `paths`/`baseUrl` mappings and
//! workspace package names (monorepo `package.json` `name` fields).
//!
//! This is deliberately best-effort — anything it cannot resolve is reported
//! as unresolved and the linker taints by name instead of dropping calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct SpecifierMap {
    /// Exact tsconfig `paths` keys (no `*`) -> target bases.
    exact: HashMap<String, Vec<PathBuf>>,
    /// Wildcard `paths` entries: (key prefix, key suffix, target bases with one `*`).
    patterns: Vec<(String, String, Vec<(PathBuf, String)>)>,
    /// Workspace package name -> package directory.
    packages: HashMap<String, PathBuf>,
}

impl SpecifierMap {
    /// Candidate base paths (before extension/index probing) for a
    /// non-relative specifier. Empty vec = we know nothing about it.
    pub fn resolve(&self, spec: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(bases) = self.exact.get(spec) {
            out.extend(bases.iter().cloned());
        }
        for (prefix, suffix, targets) in &self.patterns {
            if spec.len() >= prefix.len() + suffix.len() && spec.starts_with(prefix) && spec.ends_with(suffix) {
                let star = &spec[prefix.len()..spec.len() - suffix.len()];
                for (dir, template) in targets {
                    out.push(dir.join(template.replace('*', star)));
                }
            }
        }
        // Workspace package: exact name, or name/subpath.
        if let Some(dir) = self.packages.get(spec) {
            out.push(dir.clone());
        } else if let Some((name, sub)) = split_scoped_subpath(spec) {
            if let Some(dir) = self.packages.get(name) {
                out.push(dir.join(sub));
                out.push(dir.join("src").join(sub));
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.patterns.is_empty() && self.packages.is_empty()
    }
}

/// `@scope/pkg/sub/path` -> ("@scope/pkg", "sub/path"); `pkg/sub` -> ("pkg", "sub").
fn split_scoped_subpath(spec: &str) -> Option<(&str, &str)> {
    let boundary = if spec.starts_with('@') {
        let first = spec.find('/')?;
        spec[first + 1..].find('/').map(|i| first + 1 + i)?
    } else {
        spec.find('/')?
    };
    Some((&spec[..boundary], &spec[boundary + 1..]))
}

/// Scan the analysis root for package.json / tsconfig*.json and build the map.
pub fn build(root: &Path) -> SpecifierMap {
    let mut map = SpecifierMap::default();
    let mut stack = vec![root.to_path_buf()];
    let mut configs: Vec<PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if name == "node_modules" || name.starts_with('.') || name == "dist" || name == "target" {
                    continue;
                }
                stack.push(path);
            } else if name == "package.json" {
                if let Some(pkg_name) = read_package_name(&path) {
                    map.packages.entry(pkg_name).or_insert_with(|| dir.clone());
                }
            } else if name == "tsconfig.json" || name == "tsconfig.base.json" {
                configs.push(path);
            }
        }
    }
    for cfg in configs {
        read_tsconfig_paths(&cfg, &mut map);
    }
    map
}

fn read_package_name(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("name")?.as_str().map(|s| s.to_string())
}

fn read_tsconfig_paths(path: &Path, map: &mut SpecifierMap) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text)) else { return };
    let dir = path.parent().unwrap_or(Path::new("."));
    let co = v.get("compilerOptions");
    let base_url = co
        .and_then(|c| c.get("baseUrl"))
        .and_then(|b| b.as_str())
        .map(|b| dir.join(b))
        .unwrap_or_else(|| dir.to_path_buf());
    let Some(paths) = co.and_then(|c| c.get("paths")).and_then(|p| p.as_object()) else { return };
    for (key, targets) in paths {
        let targets: Vec<String> = targets
            .as_array()
            .map(|a| a.iter().filter_map(|t| t.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        if let Some(star) = key.find('*') {
            let (prefix, suffix) = (key[..star].to_string(), key[star + 1..].to_string());
            let mapped: Vec<(PathBuf, String)> = targets.iter().map(|t| (base_url.clone(), t.clone())).collect();
            map.patterns.push((prefix, suffix, mapped));
        } else {
            let bases: Vec<PathBuf> = targets.iter().map(|t| base_url.join(t)).collect();
            map.exact.entry(key.clone()).or_default().extend(bases);
        }
    }
}

/// Good-enough JSONC -> JSON: strips // and /* */ comments outside strings
/// and trailing commas before } or ].
fn strip_jsonc(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_str = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            out.push(b);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            i += 1;
        } else if b == b'"' {
            in_str = true;
            out.push(b);
            i += 1;
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(b);
            i += 1;
        }
    }
    // Remove trailing commas: ",   }" or ",   ]"
    let s = String::from_utf8_lossy(&out).into_owned();
    let mut cleaned = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    for (idx, &c) in chars.iter().enumerate() {
        if c == ',' {
            let mut j = idx + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                continue;
            }
        }
        cleaned.push(c);
    }
    cleaned
}
