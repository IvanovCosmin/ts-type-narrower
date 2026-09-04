//! Phase 2 + 3: link usages across modules onto declarations, then run the
//! narrowing analysis per target in parallel.
//!
//! Linking policy: a reference that cannot be resolved to a specific
//! declaration NEVER disappears — it degrades to a taint that escapes every
//! declaration it could plausibly denote. Silence is what manufactures false
//! positives.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rayon::prelude::*;

use crate::diff;
use crate::extract::extract_module;
use crate::model::*;
use crate::narrow::analyze_param;
use crate::resolve::{Resolver, Ty};
use crate::workspace;

struct TargetState {
    /// (caller module, call-site line, args) per observed direct call.
    calls: Vec<(ModuleId, u32, CallArgs)>,
    escaped: bool,
}

enum FreeLookup {
    Found(usize),
    /// The name definitely does not resolve to a free function we track.
    NotFound,
    /// Resolution left the analyzed universe — must taint, not drop.
    Unknown,
}

pub fn analyze(root: &Path, opts: &Options) -> Result<(Vec<Finding>, Stats), String> {
    let root = std::fs::canonicalize(root)
        .map_err(|e| format!("cannot open {}: {e}", root.display()))?;
    // Closed-world analysis is only sound over the whole repository: callers
    // outside the analysis root are invisible and would manufacture findings.
    if !opts.quiet {
        if let Ok(out) = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&root)
            .output()
        {
            if out.status.success() {
                let top = String::from_utf8_lossy(&out.stdout);
                if let Ok(top) = std::fs::canonicalize(top.trim()) {
                    if top != root {
                        eprintln!(
                            "overwide: WARNING: analyzing {} but the repository root is {} — calls outside the analyzed directory are invisible and findings may be wrong; prefer running at the repository root",
                            root.display(),
                            top.display()
                        );
                    }
                }
            }
        }
    }
    let t0 = Instant::now();
    let (files, rel_base) = collect_files(&root)?;

    let specmap = workspace::build(&rel_base);
    let t_walk = t0.elapsed();

    let t1 = Instant::now();
    let modules: Vec<ModuleInfo> = files
        .par_iter()
        .map(|p| {
            let rel = p.strip_prefix(&rel_base).unwrap_or(p).to_string_lossy().replace('\\', "/");
            match std::fs::read_to_string(p) {
                Ok(src) => extract_module(p, rel, &src, &specmap),
                Err(_) => ModuleInfo { path: p.clone(), rel, read_error: true, ..Default::default() },
            }
        })
        .collect();
    let t_extract = t1.elapsed();

    let t2 = Instant::now();
    let mut by_path: HashMap<PathBuf, ModuleId> = HashMap::new();
    for (i, m) in modules.iter().enumerate() {
        by_path.insert(m.path.clone(), i);
    }

    // ---- Declaration indexes. ----
    let mut decl_index: HashMap<(ModuleId, Owner, String), usize> = HashMap::new();
    let mut by_method_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_free_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_class_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_owner: HashMap<(ModuleId, Owner), Vec<usize>> = HashMap::new();
    let mut by_module: Vec<Vec<usize>> = vec![Vec::new(); modules.len()];
    let mut flat: Vec<(ModuleId, usize)> = Vec::new();
    for (mid, m) in modules.iter().enumerate() {
        for (di, d) in m.decls.iter().enumerate() {
            let ti = flat.len();
            flat.push((mid, di));
            by_module[mid].push(ti);
            decl_index.insert((mid, d.owner.clone(), d.name.clone()), ti);
            match &d.owner {
                Owner::Free => by_free_name.entry(d.name.clone()).or_default().push(ti),
                Owner::Class(c) => {
                    by_method_name.entry(d.name.clone()).or_default().push(ti);
                    by_class_name.entry(c.clone()).or_default().push(ti);
                    by_owner.entry((mid, d.owner.clone())).or_default().push(ti);
                }
                Owner::ObjectConst(_) => {
                    by_method_name.entry(d.name.clone()).or_default().push(ti);
                    by_owner.entry((mid, d.owner.clone())).or_default().push(ti);
                }
            }
        }
    }
    let mut states: Vec<TargetState> = flat.iter().map(|_| TargetState { calls: Vec::new(), escaped: false }).collect();

    // Any file we could not fully read/parse may hide call sites: taint every
    // name it could have called — we don't know them, so taint globally is the
    // only sound choice; we instead surface it loudly and keep analyzing,
    // since a rare parse error should not zero out a whole repo. This is a
    // documented soundness trade-off.
    let parse_error_files = modules.iter().filter(|m| m.parse_errors > 0).count();
    let read_error_files = modules.iter().filter(|m| m.read_error).count();

    // ---- Free-function resolution through re-export chains. ----
    fn resolve_free(
        mid: ModuleId,
        name: &str,
        modules: &[ModuleInfo],
        by_path: &HashMap<PathBuf, ModuleId>,
        decl_index: &HashMap<(ModuleId, Owner, String), usize>,
        visited: &mut HashSet<(ModuleId, String)>,
    ) -> FreeLookup {
        if !visited.insert((mid, name.to_string())) {
            return FreeLookup::NotFound;
        }
        let effective = if name == "default" {
            match &modules[mid].default_export {
                Some(n) => n.clone(),
                None => return FreeLookup::NotFound, // anonymous default: nothing we analyze
            }
        } else {
            name.to_string()
        };
        if let Some(&ti) = decl_index.get(&(mid, Owner::Free, effective.clone())) {
            return FreeLookup::Found(ti);
        }
        if let Some((path_opt, source_name)) = modules[mid].reexports_named.get(&effective) {
            return match path_opt {
                Some(p) => match by_path.get(p) {
                    Some(&m2) => resolve_free(m2, source_name, modules, by_path, decl_index, visited),
                    None => FreeLookup::Unknown,
                },
                None => FreeLookup::Unknown,
            };
        }
        let mut unknown = false;
        for star in &modules[mid].reexports_star {
            match star {
                Some(p) => match by_path.get(p) {
                    Some(&m2) => match resolve_free(m2, &effective, modules, by_path, decl_index, visited) {
                        FreeLookup::Found(ti) => return FreeLookup::Found(ti),
                        FreeLookup::Unknown => unknown = true,
                        FreeLookup::NotFound => {}
                    },
                    None => unknown = true,
                },
                None => unknown = true,
            }
        }
        if unknown { FreeLookup::Unknown } else { FreeLookup::NotFound }
    }

    // Resolve a class named `local` in module `mid` to its declaring module +
    // class name, following one import hop (incl. default imports).
    let resolve_class_home = |mid: ModuleId, local: &str| -> Option<(ModuleId, String)> {
        let is_local = modules[mid].decls.iter().any(|d| matches!(&d.owner, Owner::Class(c) if c == local));
        if is_local {
            return Some((mid, local.to_string()));
        }
        if let Some((Some(path), imported)) = modules[mid].imports.get(local) {
            if let Some(&mid2) = by_path.get(path) {
                let name = if imported == "default" {
                    modules[mid2].default_export.clone()?
                } else {
                    imported.clone()
                };
                return Some((mid2, name));
            }
        }
        None
    };

    // ---- Apply usages. ----
    enum Resolved {
        One(usize),
        Many(Vec<usize>),
        None,
    }

    for (mid, m) in modules.iter().enumerate() {
        for u in &m.usages {
            let resolved: Resolved = match &u.target {
                UsageTargetRef::Local { owner: Owner::Free, name } => {
                    // Fast path: same-module declaration (the overwhelmingly
                    // common case) needs no re-export chase.
                    if let Some(&ti) = decl_index.get(&(mid, Owner::Free, name.clone())) {
                        match &u.kind {
                            UsageKind::Call(args) => states[ti].calls.push((mid, u.line, args.clone())),
                            UsageKind::Escape => states[ti].escaped = true,
                        }
                        continue;
                    }
                    let mut visited = HashSet::new();
                    match resolve_free(mid, name, &modules, &by_path, &decl_index, &mut visited) {
                        FreeLookup::Found(ti) => Resolved::One(ti),
                        // A bodyless `declare function` or similar: calls to it
                        // are calls to something we don't analyze.
                        FreeLookup::NotFound => Resolved::None,
                        FreeLookup::Unknown => {
                            Resolved::Many(by_free_name.get(name).cloned().unwrap_or_default())
                        }
                    }
                }
                UsageTargetRef::Local { owner, name } => {
                    // Method on a tracked object/class.
                    match owner {
                        Owner::Class(cls) => match resolve_class_home(mid, cls) {
                            Some((mid2, real)) => {
                                match decl_index.get(&(mid2, Owner::Class(real), name.clone())) {
                                    Some(&ti) => Resolved::One(ti),
                                    // Inherited/unknown method (e.g. subclass
                                    // instance): taint by method name.
                                    None => Resolved::Many(by_method_name.get(name).cloned().unwrap_or_default()),
                                }
                            }
                            None => Resolved::Many(by_method_name.get(name).cloned().unwrap_or_default()),
                        },
                        _ => match decl_index.get(&(mid, owner.clone(), name.clone())) {
                            Some(&ti) => Resolved::One(ti),
                            None => Resolved::Many(by_method_name.get(name).cloned().unwrap_or_default()),
                        },
                    }
                }
                UsageTargetRef::Imported { local } => match modules[mid].imports.get(local) {
                    Some((Some(path), imported)) => match by_path.get(path) {
                        Some(&mid2) => {
                            if imported != "default" {
                                if let Some(&ti) = decl_index.get(&(mid2, Owner::Free, imported.clone())) {
                                    match &u.kind {
                                        UsageKind::Call(args) => states[ti].calls.push((mid, u.line, args.clone())),
                                        UsageKind::Escape => states[ti].escaped = true,
                                    }
                                    continue;
                                }
                            }
                            let mut visited = HashSet::new();
                            match resolve_free(mid2, imported, &modules, &by_path, &decl_index, &mut visited) {
                                FreeLookup::Found(ti) => Resolved::One(ti),
                                FreeLookup::NotFound => Resolved::None,
                                FreeLookup::Unknown => {
                                    Resolved::Many(by_free_name.get(imported).cloned().unwrap_or_default())
                                }
                            }
                        }
                        None => Resolved::Many(by_free_name.get(imported).cloned().unwrap_or_default()),
                    },
                    Some((None, imported)) => {
                        // Unresolvable module (external package, unknown alias):
                        // the call could reach any same-named exported function
                        // re-exported through it. Taint by name.
                        Resolved::Many(by_free_name.get(imported).cloned().unwrap_or_default())
                    }
                    None => Resolved::None,
                },
                UsageTargetRef::NamespaceMember { ns_local, name } => match modules[mid].imports.get(ns_local) {
                    Some((Some(path), _)) => match by_path.get(path) {
                        Some(&mid2) => {
                            let mut visited = HashSet::new();
                            match resolve_free(mid2, name, &modules, &by_path, &decl_index, &mut visited) {
                                FreeLookup::Found(ti) => Resolved::One(ti),
                                FreeLookup::NotFound => Resolved::None,
                                FreeLookup::Unknown => {
                                    Resolved::Many(by_free_name.get(name).cloned().unwrap_or_default())
                                }
                            }
                        }
                        None => Resolved::Many(by_free_name.get(name).cloned().unwrap_or_default()),
                    },
                    _ => Resolved::Many(by_free_name.get(name).cloned().unwrap_or_default()),
                },
                UsageTargetRef::AnyMethodNamed(name) => {
                    Resolved::Many(by_method_name.get(name).cloned().unwrap_or_default())
                }
                UsageTargetRef::AnyFreeNamed(name) => {
                    Resolved::Many(by_free_name.get(name).cloned().unwrap_or_default())
                }
                UsageTargetRef::AllMembersOf(owner) => match owner {
                    Owner::Class(cls) => match resolve_class_home(mid, cls) {
                        Some((mid2, real)) => match by_owner.get(&(mid2, Owner::Class(real.clone()))) {
                            Some(v) => Resolved::Many(v.clone()),
                            None => Resolved::Many(by_class_name.get(&real).cloned().unwrap_or_default()),
                        },
                        None => Resolved::Many(by_class_name.get(cls).cloned().unwrap_or_default()),
                    },
                    _ => match by_owner.get(&(mid, owner.clone())) {
                        Some(v) => Resolved::Many(v.clone()),
                        None => Resolved::None,
                    },
                },
                UsageTargetRef::AllExportsOfModule { ns_local } => match modules[mid].imports.get(ns_local) {
                    Some((Some(path), _)) => match by_path.get(path) {
                        Some(&mid2) => Resolved::Many(
                            by_module[mid2]
                                .iter()
                                .copied()
                                .filter(|&ti| {
                                    let (m2, di) = flat[ti];
                                    modules[m2].decls[di].exported
                                })
                                .collect(),
                        ),
                        None => Resolved::None, // external module: nothing analyzed to escape
                    },
                    _ => Resolved::None,
                },
            };
            match resolved {
                Resolved::None => {}
                Resolved::One(ti) => match &u.kind {
                    UsageKind::Call(args) => states[ti].calls.push((mid, u.line, args.clone())),
                    UsageKind::Escape => states[ti].escaped = true,
                },
                Resolved::Many(list) => {
                    // Broad resolution is only ever a taint: even for a Call we
                    // cannot know which target it hits, so all of them escape.
                    for ti in list {
                        states[ti].escaped = true;
                    }
                }
            }
        }
    }

    // ---- Diff scoping. ----
    let changed = match &opts.diff_base {
        Some(base) => Some(diff::git_changed_lines(base, &root)?),
        None => None,
    };

    let resolver = Resolver::new(&modules, &by_path);
    let t_link = t2.elapsed();

    // ---- Narrowing. ----
    let t3 = Instant::now();
    let findings: Vec<Finding> = flat
        .par_iter()
        .enumerate()
        .flat_map_iter(|(ti, &(mid, di))| {
            let m = &modules[mid];
            let d = &m.decls[di];
            let st = &states[ti];
            let mut out: Vec<Finding> = Vec::new();
            if !d.eligible || st.escaped || st.calls.is_empty() {
                return out;
            }
            if opts.respect_exports && d.exported {
                return out;
            }
            if let Some(changed) = &changed {
                match changed.get(&m.path) {
                    Some(ranges) if diff::intersects(ranges, d.line, d.end_line) => {}
                    _ => return out,
                }
            }
            let sites: Vec<String> = st
                .calls
                .iter()
                .take(3)
                .map(|(cm, line, _)| format!("{}:{}", modules[*cm].rel, line))
                .collect();
            for (pi, param) in d.params.iter().enumerate() {
                let Some(ty) = &param.ty else { continue };
                let declared = resolver.resolve_expr(ty, mid);
                let observed: Vec<Ty> = st
                    .calls
                    .iter()
                    .map(|(caller, _, args)| match args {
                        CallArgs::Opaque => Ty::AnyLike,
                        CallArgs::Args(list) => match list.get(pi) {
                            Some(obs) => resolver.resolve_observed(obs, *caller),
                            None => {
                                if let Some(default) = &param.default {
                                    resolver.resolve_observed(default, mid)
                                } else if param.optional {
                                    Ty::Undefined
                                } else {
                                    Ty::AnyLike
                                }
                            }
                        },
                    })
                    .collect();
                for report in analyze_param(&declared, &observed, param.optional, opts.max_depth) {
                    out.push(Finding {
                        file: m.rel.clone(),
                        line: d.line,
                        function_name: d.name.clone(),
                        param: param.name.clone(),
                        path: report.path,
                        declared: report.declared,
                        unused: report.unused,
                        call_count: st.calls.len(),
                        sites: sites.clone(),
                    });
                }
            }
            out
        })
        .collect();
    let t_narrow = t3.elapsed();

    let mut findings = findings;
    findings.sort_by(|a, b| {
        (&a.file, a.line, &a.function_name, &a.param, &a.path)
            .cmp(&(&b.file, b.line, &b.function_name, &b.param, &b.path))
    });

    let mut stats = Stats {
        files: modules.len(),
        decls: flat.len(),
        parse_error_files,
        read_error_files,
        ..Default::default()
    };
    for (ti, &(mid, di)) in flat.iter().enumerate() {
        let d = &modules[mid].decls[di];
        match d.skip {
            Some(SkipReason::Generic) => stats.skipped_generic += 1,
            Some(SkipReason::Overload) => stats.skipped_overload += 1,
            Some(SkipReason::RestParam) => stats.skipped_rest_param += 1,
            Some(SkipReason::NoParams) => stats.skipped_no_params += 1,
            None => {
                if states[ti].escaped {
                    stats.escaped += 1;
                } else if states[ti].calls.is_empty() {
                    stats.uncalled += 1;
                } else {
                    stats.analyzed += 1;
                }
            }
        }
    }

    if opts.timing {
        eprintln!(
            "timing: walk={t_walk:?} parse+extract={t_extract:?} link={t_link:?} narrow={t_narrow:?} files={} decls={} total={:?}",
            modules.len(),
            flat.len(),
            t0.elapsed()
        );
    }
    Ok((findings, stats))
}

/// Collect analyzable files. Accepts a directory or a single file. Symlinks
/// are skipped entirely (following them duplicates modules and can loop).
fn collect_files(root: &Path) -> Result<(Vec<PathBuf>, PathBuf), String> {
    if root.is_file() {
        let base = root.parent().unwrap_or(Path::new("/")).to_path_buf();
        return Ok((vec![root.to_path_buf()], base));
    }
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if name == "node_modules" || name.starts_with('.') || name == "dist" || name == "target" {
                    continue;
                }
                stack.push(path);
            } else if let Some(ext) = path.extension().and_then(|x| x.to_str()) {
                if matches!(ext, "ts" | "tsx" | "mts" | "cts") && !name.ends_with(".d.ts") {
                    out.push(path);
                }
            }
        }
    }
    if out.is_empty() {
        return Err(format!("no TypeScript files found under {}", root.display()));
    }
    out.sort();
    Ok((out, root.to_path_buf()))
}
