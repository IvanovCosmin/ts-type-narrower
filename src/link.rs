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
    /// The name resolves to a re-exported namespace object of this module.
    FoundNamespace(ModuleId),
    /// The name definitely does not resolve to a free function we track.
    NotFound,
    /// Resolution left the analyzed universe — must taint, not drop.
    Unknown,
}

enum ClassLookup {
    Found(ModuleId, String),
    NotFound,
    Unknown,
}

/// Resolve an EXPORT of module `mid` named `name` to a class declaration,
/// following default-export indirection and re-export chains.
fn resolve_class_export(
    mid: ModuleId,
    name: &str,
    modules: &[crate::model::ModuleInfo],
    by_path: &HashMap<PathBuf, ModuleId>,
    visited: &mut HashSet<(ModuleId, String)>,
) -> ClassLookup {
    if !visited.insert((mid, name.to_string())) {
        return ClassLookup::NotFound;
    }
    let effective = if name == "default" {
        match &modules[mid].default_export {
            Some(n) => n.clone(),
            None => return ClassLookup::NotFound,
        }
    } else {
        name.to_string()
    };
    if modules[mid].classes_declared.contains(&effective) {
        return ClassLookup::Found(mid, effective);
    }
    if let Some((path_opt, source_name)) = modules[mid].reexports_named.get(&effective) {
        if source_name == "*" {
            return ClassLookup::NotFound;
        }
        return match path_opt {
            Some(p) => match by_path.get(p) {
                Some(&m2) => resolve_class_export(m2, source_name, modules, by_path, visited),
                None => ClassLookup::Unknown,
            },
            None => ClassLookup::Unknown,
        };
    }
    let mut unknown = false;
    for star in &modules[mid].reexports_star {
        match star {
            Some(p) => match by_path.get(p) {
                Some(&m2) => match resolve_class_export(m2, &effective, modules, by_path, visited) {
                    ClassLookup::Found(a, b) => return ClassLookup::Found(a, b),
                    ClassLookup::Unknown => unknown = true,
                    ClassLookup::NotFound => {}
                },
                None => unknown = true,
            },
            None => unknown = true,
        }
    }
    if unknown { ClassLookup::Unknown } else { ClassLookup::NotFound }
}

/// Resolve a class named `local` as visible in module `mid` (declaration or
/// import), re-export-chain aware.
fn resolve_class_via_local(
    mid: ModuleId,
    local: &str,
    modules: &[crate::model::ModuleInfo],
    by_path: &HashMap<PathBuf, ModuleId>,
) -> ClassLookup {
    if modules[mid].classes_declared.contains(local) {
        return ClassLookup::Found(mid, local.to_string());
    }
    match modules[mid].imports.get(local) {
        Some((Some(path), imported)) => match by_path.get(path) {
            Some(&m2) => {
                let mut visited = HashSet::new();
                resolve_class_export(m2, imported, modules, by_path, &mut visited)
            }
            None => ClassLookup::Unknown,
        },
        Some((None, _)) => ClassLookup::Unknown,
        None => ClassLookup::NotFound,
    }
}

pub fn analyze(root: &Path, opts: &Options) -> Result<Analysis, String> {
    let root = std::fs::canonicalize(root)
        .map_err(|e| format!("cannot open {}: {e}", root.display()))?;
    let mut warnings: Vec<String> = Vec::new();
    // Closed-world analysis is only sound over the whole repository: callers
    // outside the analysis root are invisible and would manufacture findings.
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&root)
        .output()
    {
        if out.status.success() {
            let top = String::from_utf8_lossy(&out.stdout);
            if let Ok(top) = std::fs::canonicalize(top.trim()) {
                if top != root {
                    warnings.push(format!(
                        "analyzing {} but the repository root is {} — calls outside the analyzed directory are invisible and findings may be wrong; prefer running at the repository root",
                        root.display(),
                        top.display()
                    ));
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

    // ---- Class inheritance graph. ----
    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    struct ClassId(ModuleId, String);
    #[derive(Clone, Debug)]
    enum Parent {
        None,
        Known(ClassId),
        /// Chain leaves the analyzed universe cleanly (external base class).
        External,
        /// Unresolvable heritage (aliased import, expression): carries the
        /// local super name for name-based tainting.
        Unknown(String),
    }
    let mut class_set: HashSet<ClassId> = HashSet::new();
    for (mid, m) in modules.iter().enumerate() {
        for d in &m.decls {
            if let Owner::Class(c) = &d.owner {
                class_set.insert(ClassId(mid, c.clone()));
            }
        }
        for c in m.class_extends.keys() {
            class_set.insert(ClassId(mid, c.clone()));
        }
        for c in &m.classes_declared {
            class_set.insert(ClassId(mid, c.clone()));
        }
    }
    let mut parents: HashMap<ClassId, Parent> = HashMap::new();
    let mut children: HashMap<ClassId, Vec<ClassId>> = HashMap::new();
    let mut orphans: Vec<ClassId> = Vec::new();
    let mut classes_by_module: HashMap<ModuleId, Vec<ClassId>> = HashMap::new();
    for c in &class_set {
        classes_by_module.entry(c.0).or_default().push(c.clone());
    }
    for (mid, m) in modules.iter().enumerate() {
        let Some(mod_classes) = classes_by_module.get(&mid) else { continue };
        for c in mod_classes {
            let parent = match m.class_extends.get(&c.1) {
                None => Parent::None,
                Some(sup) if sup.is_empty() => Parent::Unknown(String::new()),
                Some(sup) => {
                    // Re-export-chain aware: `extends Base` where Base arrives
                    // through a barrel must still produce a Known edge, and an
                    // unresolvable chain must be Unknown (taint), never a
                    // silent External.
                    match resolve_class_via_local(mid, sup, &modules, &by_path) {
                        ClassLookup::Found(m2, name) => Parent::Known(ClassId(m2, name)),
                        ClassLookup::NotFound => {
                            if m.imports.contains_key(sup) {
                                Parent::External
                            } else {
                                Parent::Unknown(sup.clone())
                            }
                        }
                        ClassLookup::Unknown => Parent::Unknown(sup.clone()),
                    }
                }
            };
            if let Parent::Known(p) = &parent {
                children.entry(p.clone()).or_default().push(c.clone());
            }
            if matches!(parent, Parent::Unknown(_)) {
                orphans.push(c.clone());
            }
            parents.insert(c.clone(), parent);
        }
    }
    // Orphans (unresolvable heritage) could sit anywhere in any chain: their
    // closure joins every descendant query. Computed once — per-query cloning
    // was an O(#this-calls x #orphans) cliff on React-style codebases where
    // `extends React.Component<P>` makes every class component an orphan.
    let orphan_closure: HashSet<ClassId> = {
        let mut seen: HashSet<ClassId> = HashSet::new();
        let mut queue: Vec<ClassId> = orphans.clone();
        while let Some(c) = queue.pop() {
            if !seen.insert(c.clone()) {
                continue;
            }
            queue.extend(children.get(&c).cloned().unwrap_or_default());
        }
        seen
    };
    // (home class, method name) pairs where `this.m()` was attributed; the
    // override-escape post-pass below consumes them. Attribution counts let
    // the post-pass exempt a decl that every same-named this-site resolves to.
    let mut this_method_sites: HashSet<(ClassId, String)> = HashSet::new();
    let mut this_method_attributed: HashMap<usize, usize> = HashMap::new();
    let mut this_method_site_count: HashMap<String, usize> = HashMap::new();
    enum ChainHit {
        Found(usize),
        NotFound,
        Tainted,
    }
    let find_in_chain = |start: ClassId, method: &str| -> ChainHit {
        let mut cur = start;
        let mut hops = 0;
        loop {
            if hops > 64 {
                return ChainHit::Tainted;
            }
            hops += 1;
            if let Some(&ti) = decl_index.get(&(cur.0, Owner::Class(cur.1.clone()), method.to_string())) {
                return ChainHit::Found(ti);
            }
            match parents.get(&cur) {
                Some(Parent::Known(p)) => cur = p.clone(),
                Some(Parent::None) | Some(Parent::External) | None => return ChainHit::NotFound,
                Some(Parent::Unknown(_)) => return ChainHit::Tainted,
            }
        }
    };

    // Any file we could not fully read/parse may hide call sites: taint every
    // name it could have called — we don't know them, so taint globally is the
    // only sound choice; we instead surface it loudly and keep analyzing,
    // since a rare parse error should not zero out a whole repo. This is a
    // documented soundness trade-off.
    let parse_error_files = modules.iter().filter(|m| m.parse_errors > 0).count();
    let read_error_files = modules.iter().filter(|m| m.read_error).count();
    // When any module created a namespace value we could not track, member
    // accesses on unknown objects must taint same-named exported functions.
    let untracked_namespace_exists = modules.iter().any(|m| m.has_untracked_namespace);

    // ---- Free-function resolution through re-export chains. ----
    #[allow(clippy::too_many_arguments)]
    fn resolve_free(
        mid: ModuleId,
        name: &str,
        modules: &[ModuleInfo],
        by_path: &HashMap<PathBuf, ModuleId>,
        decl_index: &HashMap<(ModuleId, Owner, String), usize>,
        flat: &[(ModuleId, usize)],
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
        // A local declaration satisfies an import only when it is exported —
        // `export { x } from "./other"` does not bind a private local `x`.
        if let Some(&ti) = decl_index.get(&(mid, Owner::Free, effective.clone())) {
            let (m2, di) = flat[ti];
            if modules[m2].decls[di].exported {
                return FreeLookup::Found(ti);
            }
        }
        if let Some((path_opt, source_name)) = modules[mid].reexports_named.get(&effective) {
            if source_name == "*" {
                return match path_opt {
                    Some(p) => match by_path.get(p) {
                        Some(&m2) => FreeLookup::FoundNamespace(m2),
                        None => FreeLookup::Unknown,
                    },
                    None => FreeLookup::Unknown,
                };
            }
            return match path_opt {
                Some(p) => match by_path.get(p) {
                    Some(&m2) => resolve_free(m2, source_name, modules, by_path, decl_index, flat, visited),
                    None => FreeLookup::Unknown,
                },
                None => FreeLookup::Unknown,
            };
        }
        let mut unknown = false;
        for star in &modules[mid].reexports_star {
            match star {
                Some(p) => match by_path.get(p) {
                    Some(&m2) => match resolve_free(m2, &effective, modules, by_path, decl_index, flat, visited) {
                        FreeLookup::Found(ti) => return FreeLookup::Found(ti),
                        ns @ FreeLookup::FoundNamespace(_) => return ns,
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
    // class name, re-export-chain aware.
    let resolve_class_home = |mid: ModuleId, local: &str| -> Option<(ModuleId, String)> {
        match resolve_class_via_local(mid, local, &modules, &by_path) {
            ClassLookup::Found(m2, name) => Some((m2, name)),
            _ => None,
        }
    };

    // All exported declarations of a module (what a namespace value exposes).
    let exported_of = |m2: ModuleId| -> Vec<usize> {
        by_module[m2]
            .iter()
            .copied()
            .filter(|&ti| {
                let (mm, di) = flat[ti];
                let d = &modules[mm].decls[di];
                d.exported && !d.non_public
            })
            .collect()
    };

    // Everything an instance of this class exposes: its own methods plus the
    // inherited chain; an unresolvable chain taints by the super's name.
    let expand_class_members = |start: ClassId| -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        let mut cur = start;
        let mut hops = 0;
        loop {
            if let Some(v) = by_owner.get(&(cur.0, Owner::Class(cur.1.clone()))) {
                // private/protected methods are unreachable through an escaped
                // class/instance value; internal this.* calls are modeled.
                out.extend(v.iter().copied().filter(|&ti| {
                    let (mm, di) = flat[ti];
                    !modules[mm].decls[di].non_public
                }));
            }
            hops += 1;
            if hops > 64 {
                break;
            }
            match parents.get(&cur) {
                Some(Parent::Known(p)) => cur = p.clone(),
                Some(Parent::Unknown(sup)) => {
                    out.extend(by_class_name.get(sup).cloned().unwrap_or_default());
                    break;
                }
                _ => break,
            }
        }
        out
    };

    // ---- Apply usages. ----
    enum Resolved {
        One(usize),
        Many(Vec<usize>),
        None,
    }

    // Broad taints are idempotent; apply each once. Extraction dedupes per
    // module, but common names (get/map/render) recur across modules, and
    // each application cloned + filtered a full index vector.
    let mut done_method_taint: HashSet<String> = HashSet::new();
    let mut done_member_free_taint: HashSet<String> = HashSet::new();
    let mut done_free_taint: HashSet<(ModuleId, String)> = HashSet::new();
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
                    match resolve_free(mid, name, &modules, &by_path, &decl_index, &flat, &mut visited) {
                        FreeLookup::Found(ti) => Resolved::One(ti),
                        FreeLookup::FoundNamespace(m2) => Resolved::Many(exported_of(m2)),
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
                            // `new Cls()` binds the exact class: resolve the
                            // method through the inheritance chain.
                            Some((mid2, real)) => match find_in_chain(ClassId(mid2, real), name) {
                                ChainHit::Found(ti) => Resolved::One(ti),
                                _ => Resolved::Many(by_method_name.get(name).cloned().unwrap_or_default()),
                            },
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
                                    // Fast path only for exported declarations:
                                    // a private local must not satisfy an
                                    // import (re-exports take precedence).
                                    let (fm, fdi) = flat[ti];
                                    if modules[fm].decls[fdi].exported {
                                        match &u.kind {
                                            UsageKind::Call(args) => states[ti].calls.push((mid, u.line, args.clone())),
                                            UsageKind::Escape => states[ti].escaped = true,
                                        }
                                        continue;
                                    }
                                }
                            }
                            let mut visited = HashSet::new();
                            match resolve_free(mid2, imported, &modules, &by_path, &decl_index, &flat, &mut visited) {
                                FreeLookup::Found(ti) => Resolved::One(ti),
                                FreeLookup::FoundNamespace(m3) => Resolved::Many(exported_of(m3)),
                                FreeLookup::NotFound => {
                                    // Not a free function: an imported CLASS or
                                    // object-const used as a value must escape
                                    // its members — dropping it manufactured
                                    // false positives.
                                    let mut visited2 = HashSet::new();
                                    match resolve_class_export(mid2, imported, &modules, &by_path, &mut visited2) {
                                        ClassLookup::Found(cm, cn) => {
                                            Resolved::Many(expand_class_members(ClassId(cm, cn)))
                                        }
                                        _ => match by_owner.get(&(mid2, Owner::ObjectConst(imported.clone()))) {
                                            Some(v) => Resolved::Many(v.clone()),
                                            None => Resolved::None,
                                        },
                                    }
                                }
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
                            match resolve_free(mid2, name, &modules, &by_path, &decl_index, &flat, &mut visited) {
                                FreeLookup::Found(ti) => Resolved::One(ti),
                                FreeLookup::FoundNamespace(m3) => Resolved::Many(exported_of(m3)),
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
                    if done_method_taint.insert(name.clone()) {
                        Resolved::Many(by_method_name.get(name).cloned().unwrap_or_default())
                    } else {
                        Resolved::None
                    }
                }
                UsageTargetRef::AnyFreeNamed(name) => {
                    if !done_free_taint.insert((mid, name.clone())) {
                        continue;
                    }
                    // An unbound/shadowed identifier call can only reach a
                    // function in the same module (scope-model imprecision) or
                    // in a script file (shared global scope) — never a function
                    // module-scoped elsewhere.
                    Resolved::Many(
                        by_free_name
                            .get(name)
                            .map(|v| {
                                v.iter()
                                    .copied()
                                    .filter(|&ti| {
                                        let (m2, _) = flat[ti];
                                        m2 == mid || !modules[m2].is_module
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    )
                }
                UsageTargetRef::AllMembersOf(owner) => match owner {
                    Owner::Class(cls) => match resolve_class_home(mid, cls) {
                        Some((mid2, real)) => Resolved::Many(expand_class_members(ClassId(mid2, real))),
                        None => Resolved::Many(by_class_name.get(cls).cloned().unwrap_or_default()),
                    },
                    _ => match by_owner.get(&(mid, owner.clone())) {
                        Some(v) => Resolved::Many(v.clone()),
                        None => Resolved::None,
                    },
                },
                UsageTargetRef::ThisMethod { class, name } => {
                    // `this.m(...)` in `class`: attribute up the chain, and
                    // escape overrides in descendant classes (the receiver may
                    // be a subclass instance).
                    let home = ClassId(mid, class.clone());
                    match find_in_chain(home.clone(), name) {
                        ChainHit::Found(ti) => {
                            match &u.kind {
                                UsageKind::Call(args) => states[ti].calls.push((mid, u.line, args.clone())),
                                UsageKind::Escape => states[ti].escaped = true,
                            }
                            // Override escapes are applied in one inverted
                            // post-pass (per method decl, walk ancestors) —
                            // per-call descendant BFS was quadratic on deep
                            // chains and orphan-heavy React codebases.
                            if this_method_sites.insert((home, name.clone())) {
                                *this_method_attributed.entry(ti).or_default() += 1;
                                *this_method_site_count.entry(name.clone()).or_default() += 1;
                            }
                            continue;
                        }
                        _ => Resolved::Many(by_method_name.get(name).cloned().unwrap_or_default()),
                    }
                }
                UsageTargetRef::AllExportsOfPath(path) => match by_path.get(path) {
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
                    None => Resolved::None, // external module
                },
                UsageTargetRef::MemberFreeNamed(name) => {
                    if untracked_namespace_exists && done_member_free_taint.insert(name.clone()) {
                        Resolved::Many(
                            by_free_name
                                .get(name)
                                .map(|v| {
                                    v.iter()
                                        .copied()
                                        .filter(|&ti| {
                                            let (m2, di) = flat[ti];
                                            modules[m2].decls[di].exported
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        )
                    } else {
                        Resolved::None
                    }
                }
                UsageTargetRef::AllExportsOfModule { ns_local } => match modules[mid].imports.get(ns_local) {
                    Some((Some(path), _)) => match by_path.get(path) {
                        Some(&mid2) => Resolved::Many(exported_of(mid2)),
                        None => Resolved::None, // resolved path outside the tree: external
                    },
                    // Unresolvable module: its exports are unknowable. The
                    // value-escape site set the untracked-namespace flag, so
                    // member accesses on unknown objects taint same-named
                    // exported functions project-wide (MemberFreeNamed). Calls
                    // hidden inside ambient code remain the documented
                    // closed-world boundary.
                    Some((None, _)) => Resolved::None,
                    None => Resolved::None,
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

    // ---- ThisMethod override post-pass. ----
    // A `this.m()` in class H can dispatch to an override in any descendant of
    // H: for every method decl, walk its ancestor chain once and escape it if
    // some strict ancestor recorded a this-call of this name. Orphan-closure
    // classes may descend from anything.
    if !this_method_sites.is_empty() {
        let override_names: HashSet<&String> = this_method_sites.iter().map(|(_, n)| n).collect();
        for (ti, &(mid, di)) in flat.iter().enumerate() {
            let d = &modules[mid].decls[di];
            let Owner::Class(c) = &d.owner else { continue };
            if !override_names.contains(&d.name) {
                continue;
            }
            let home = ClassId(mid, c.clone());
            if orphan_closure.contains(&home) {
                // An orphan could descend from any chain — escape, UNLESS every
                // same-named this-site attributes to this very decl (then it is
                // the receiver, not an override of some other chain).
                let attributed = this_method_attributed.get(&ti).copied().unwrap_or(0);
                let total = this_method_site_count.get(&d.name).copied().unwrap_or(0);
                if attributed < total {
                    states[ti].escaped = true;
                }
                continue;
            }
            let mut cur = home;
            let mut hops = 0;
            loop {
                match parents.get(&cur) {
                    Some(Parent::Known(p)) => {
                        if this_method_sites.contains(&(p.clone(), d.name.clone())) {
                            states[ti].escaped = true;
                            break;
                        }
                        cur = p.clone();
                    }
                    _ => break,
                }
                hops += 1;
                if hops > 64 {
                    states[ti].escaped = true;
                    break;
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
                        declared: display_ty(&report.declared),
                        unused: report.unused.iter().map(|u| display_ty(u)).collect(),
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
    let mut uncalled: Vec<UncalledFn> = Vec::new();
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
                    uncalled.push(UncalledFn {
                        file: modules[mid].rel.clone(),
                        line: d.line,
                        function_name: d.name.clone(),
                        exported: d.exported,
                    });
                } else {
                    stats.analyzed += 1;
                }
            }
        }
    }
    uncalled.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    if stats.parse_error_files > 0 {
        warnings.push(format!(
            "{} file(s) had parse errors — their call sites may be missing and findings may be unreliable",
            stats.parse_error_files
        ));
    }
    if stats.read_error_files > 0 {
        warnings.push(format!(
            "{} file(s) could not be read (non-UTF8?) — treated as empty",
            stats.read_error_files
        ));
    }

    if opts.timing {
        eprintln!(
            "timing: walk={t_walk:?} parse+extract={t_extract:?} link={t_link:?} narrow={t_narrow:?} files={} decls={} total={:?}",
            modules.len(),
            flat.len(),
            t0.elapsed()
        );
    }
    Ok(Analysis { findings, stats, uncalled, warnings })
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

/// Collapse whitespace and cap length so a 30-line interface body or a
/// 3000-constituent union doesn't render a finding unreadable.
fn display_ty(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(160));
    let mut last_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_ws {
                out.push(' ');
            }
            last_ws = true;
        } else {
            out.push(ch);
            last_ws = false;
        }
        if out.len() > 156 {
            out.push('…');
            break;
        }
    }
    out
}
