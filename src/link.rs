//! Phase 2 + 3: link usages across modules onto declarations, then run the
//! narrowing analysis per target in parallel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rayon::prelude::*;

use crate::diff;
use crate::extract::extract_module;
use crate::model::*;
use crate::narrow::analyze_param;
use crate::resolve::{Resolver, Ty};

struct TargetState {
    /// (caller module, args) per observed direct call.
    calls: Vec<(ModuleId, CallArgs)>,
    escaped: bool,
}

pub fn analyze(root: &Path, opts: &Options) -> Result<Vec<Finding>, String> {
    let t0 = Instant::now();
    let files = collect_files(root);
    let t_walk = t0.elapsed();

    let t1 = Instant::now();
    let modules: Vec<ModuleInfo> = files
        .par_iter()
        .map(|p| {
            let src = std::fs::read_to_string(p).unwrap_or_default();
            let rel = p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/");
            extract_module(p, rel, &src)
        })
        .collect();
    let t_extract = t1.elapsed();

    let t2 = Instant::now();
    let mut by_path: HashMap<PathBuf, ModuleId> = HashMap::new();
    for (i, m) in modules.iter().enumerate() {
        by_path.insert(m.path.clone(), i);
    }

    // Index declarations.
    let mut decl_index: HashMap<(ModuleId, Owner, String), usize> = HashMap::new();
    let mut by_method_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_owner: HashMap<(ModuleId, Owner), Vec<usize>> = HashMap::new();
    let mut flat: Vec<(ModuleId, usize)> = Vec::new(); // target idx -> (module, decl idx)
    for (mid, m) in modules.iter().enumerate() {
        for (di, d) in m.decls.iter().enumerate() {
            let ti = flat.len();
            flat.push((mid, di));
            decl_index.insert((mid, d.owner.clone(), d.name.clone()), ti);
            if d.owner != Owner::Free {
                by_method_name.entry(d.name.clone()).or_default().push(ti);
                by_owner.entry((mid, d.owner.clone())).or_default().push(ti);
            }
        }
    }
    let mut states: Vec<TargetState> = flat.iter().map(|_| TargetState { calls: Vec::new(), escaped: false }).collect();

    // Resolve a usage-target reference to a target index.
    let resolve_ref = |mid: ModuleId, target: &UsageTargetRef| -> ResolvedRef {
        match target {
            UsageTargetRef::Local { owner, name } => {
                if let Some(&ti) = decl_index.get(&(mid, owner.clone(), name.clone())) {
                    return ResolvedRef::One(ti);
                }
                // A class named locally may be imported: retry in its module.
                if let Owner::Class(cls) = owner {
                    if let Some((path, imported)) = modules[mid].imports.get(cls) {
                        if let Some(&mid2) = by_path.get(path) {
                            if let Some(&ti) = decl_index.get(&(mid2, Owner::Class(imported.clone()), name.clone())) {
                                return ResolvedRef::One(ti);
                            }
                        }
                    }
                }
                ResolvedRef::None
            }
            UsageTargetRef::Imported { local, .. } => {
                if let Some((path, imported)) = modules[mid].imports.get(local) {
                    if let Some(&mid2) = by_path.get(path) {
                        if let Some(&ti) = decl_index.get(&(mid2, Owner::Free, imported.clone())) {
                            return ResolvedRef::One(ti);
                        }
                    }
                }
                ResolvedRef::None
            }
            UsageTargetRef::AnyMethodNamed(name) => match by_method_name.get(name) {
                Some(v) => ResolvedRef::Many(v.clone()),
                None => ResolvedRef::None,
            },
            UsageTargetRef::AllMembersOf(owner) => {
                if let Some(v) = by_owner.get(&(mid, owner.clone())) {
                    return ResolvedRef::Many(v.clone());
                }
                if let Owner::Class(cls) = owner {
                    if let Some((path, imported)) = modules[mid].imports.get(cls) {
                        if let Some(&mid2) = by_path.get(path) {
                            if let Some(v) = by_owner.get(&(mid2, Owner::Class(imported.clone()))) {
                                return ResolvedRef::Many(v.clone());
                            }
                        }
                    }
                }
                ResolvedRef::None
            }
        }
    };

    for (mid, m) in modules.iter().enumerate() {
        for u in &m.usages {
            let resolved = resolve_ref(mid, &u.target);
            let indices: Vec<usize> = match resolved {
                ResolvedRef::None => continue,
                ResolvedRef::One(i) => vec![i],
                ResolvedRef::Many(v) => v,
            };
            for ti in indices {
                match &u.kind {
                    UsageKind::Call(args) => states[ti].calls.push((mid, args.clone())),
                    UsageKind::Escape => states[ti].escaped = true,
                }
            }
        }
    }

    // Diff scoping: restrict which declarations are *reported*; the usage graph
    // above always spans the whole project.
    let changed = match &opts.diff_base {
        Some(base) => Some(diff::git_changed_lines(base, root)?),
        None => None,
    };

    let resolver = Resolver { modules: &modules, by_path: &by_path };
    let t_link = t2.elapsed();

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
            for (pi, param) in d.params.iter().enumerate() {
                let Some(ty) = &param.ty else { continue };
                let declared = resolver.resolve_expr(ty, mid);
                let observed: Vec<Ty> = st
                    .calls
                    .iter()
                    .map(|(caller, args)| match args {
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
                    });
                }
            }
            out
        })
        .collect();
    let t_narrow = t3.elapsed();

    let mut findings = findings;
    findings.sort_by(|a, b| {
        (&a.file, a.line, &a.param, &a.path).cmp(&(&b.file, b.line, &b.param, &b.path))
    });

    if opts.timing {
        eprintln!(
            "timing: walk={:?} parse+extract={:?} link={:?} narrow={:?} files={} decls={} total={:?}",
            t_walk,
            t_extract,
            t_link,
            t_narrow,
            modules.len(),
            flat.len(),
            t0.elapsed()
        );
    }
    Ok(findings)
}

enum ResolvedRef {
    None,
    One(usize),
    Many(Vec<usize>),
}

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
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
    out.sort();
    out
}
