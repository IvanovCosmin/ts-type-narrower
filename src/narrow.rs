//! The narrowing core: given a declared type and the resolved types of every
//! observed argument, report union constituents no call can produce.
//!
//! Conservative by construction: a constituent is reported only when every
//! observed argument provably excludes it. Anything uncertain marks all
//! constituents as used.

use crate::resolve::{print_ty, Ty};

pub struct PathReport {
    pub path: String,
    pub declared: String,
    pub unused: Vec<String>,
}

pub fn analyze_param(declared: &Ty, observed: &[Ty], optional_param: bool, max_depth: usize) -> Vec<PathReport> {
    let declared = if optional_param {
        // Optionality adds `undefined` to the effective type; omitted calls
        // observe it, and it is never reported (pure noise).
        match declared {
            Ty::Union(parts) => {
                let mut p = parts.clone();
                if !p.contains(&Ty::Undefined) {
                    p.push(Ty::Undefined);
                }
                Ty::Union(p)
            }
            other => Ty::Union(vec![other.clone(), Ty::Undefined]),
        }
    } else {
        declared.clone()
    };
    let mut out = Vec::new();
    walk(&declared, observed, String::new(), 0, optional_param, max_depth, &mut out);
    out
}

fn walk(
    declared: &Ty,
    observed: &[Ty],
    path: String,
    depth: usize,
    suppress_undefined: bool,
    max_depth: usize,
    out: &mut Vec<PathReport>,
) {
    if depth > max_depth {
        return;
    }
    // `boolean` narrows like the union it is (display stays "boolean").
    let normalized;
    let was_bool = *declared == Ty::Bool;
    let declared = if was_bool {
        normalized = Ty::Union(vec![Ty::BoolLit(false), Ty::BoolLit(true)]);
        &normalized
    } else {
        declared
    };

    match declared {
        Ty::Union(constituents) => {
            let mut used = vec![false; constituents.len()];

            // Identical observations are common (the same const passed at many
            // call sites) and marking is idempotent: process each once.
            let mut seen = std::collections::HashSet::new();
            let unique_obs: Vec<&Ty> = observed.iter().filter(|o| seen.insert(*o)).collect();

            // Index literal constituents so literal observations mark in O(1)
            // instead of scanning the whole union (quadratic on large unions).
            let mut lit_index: std::collections::HashMap<&Ty, Vec<usize>> = std::collections::HashMap::new();
            let (mut str_lits, mut num_lits, mut bool_lits) = (Vec::new(), Vec::new(), Vec::new());
            let (mut prim_str, mut prim_num, mut prim_bool) = (Vec::new(), Vec::new(), Vec::new());
            let mut misc = Vec::new();
            for (i, c) in constituents.iter().enumerate() {
                match c {
                    Ty::StrLit(_) => {
                        lit_index.entry(c).or_default().push(i);
                        str_lits.push(i);
                    }
                    Ty::NumLit(_) => {
                        lit_index.entry(c).or_default().push(i);
                        num_lits.push(i);
                    }
                    Ty::BoolLit(_) => {
                        lit_index.entry(c).or_default().push(i);
                        bool_lits.push(i);
                    }
                    Ty::EnumLit { .. } | Ty::Undefined | Ty::Null => {
                        lit_index.entry(c).or_default().push(i);
                    }
                    Ty::Str => prim_str.push(i),
                    Ty::Num => prim_num.push(i),
                    Ty::Bool => prim_bool.push(i),
                    _ => misc.push(i),
                }
            }

            for obs in unique_obs {
                for oc in split(obs) {
                    if matches!(oc, Ty::AnyLike) {
                        used.iter_mut().for_each(|u| *u = true);
                        continue;
                    }
                    let mut any = false;
                    let mut mark = |idx: &[usize], used: &mut Vec<bool>, any: &mut bool| {
                        for &i in idx {
                            used[i] = true;
                            *any = true;
                        }
                    };
                    match oc {
                        // Literal observation: exact literal constituent plus
                        // the matching primitive constituent.
                        Ty::StrLit(_) | Ty::NumLit(_) | Ty::BoolLit(_) | Ty::EnumLit { .. } | Ty::Undefined | Ty::Null => {
                            if let Some(idx) = lit_index.get(oc) {
                                mark(idx, &mut used, &mut any);
                            }
                            match oc {
                                Ty::StrLit(_) => mark(&prim_str, &mut used, &mut any),
                                Ty::NumLit(_) => mark(&prim_num, &mut used, &mut any),
                                Ty::BoolLit(_) => mark(&prim_bool, &mut used, &mut any),
                                _ => {}
                            }
                        }
                        // Primitive observation: the primitive constituent and
                        // every literal it subsumes (an argument typed `string`
                        // against `"fast" | string` can carry "fast").
                        Ty::Str => {
                            mark(&prim_str, &mut used, &mut any);
                            mark(&str_lits, &mut used, &mut any);
                        }
                        Ty::Num => {
                            mark(&prim_num, &mut used, &mut any);
                            mark(&num_lits, &mut used, &mut any);
                        }
                        Ty::Bool => {
                            mark(&prim_bool, &mut used, &mut any);
                            mark(&bool_lits, &mut used, &mut any);
                        }
                        // Structural observation: full two-way scan.
                        _ => {
                            for (i, c) in constituents.iter().enumerate() {
                                if assignable(oc, c) || assignable(c, oc) {
                                    used[i] = true;
                                    any = true;
                                }
                            }
                        }
                    }
                    // A structural observation can also cover misc constituents
                    // handled above; a literal/primitive one cannot match misc
                    // (objects/opaque) — except via the two-way scan, which the
                    // structural arm already performs.
                    if !any {
                        // The observed constituent maps to nothing we can
                        // identify: we cannot prove anything — mark everything used.
                        used.iter_mut().for_each(|u| *u = true);
                    }
                }
            }
            let unused: Vec<String> = constituents
                .iter()
                .zip(&used)
                .filter(|(c, u)| !**u && !(suppress_undefined && **c == Ty::Undefined))
                .map(|(c, _)| print_ty(c))
                .collect();
            if !unused.is_empty() {
                let declared_str =
                    if was_bool { "boolean".to_string() } else { print_ty(declared) };
                out.push(PathReport { path, declared: declared_str, unused });
            }
            // Unions are terminal: constituent-internal narrowing would need
            // per-variant call grouping. (Future work.)
        }
        Ty::Object(props) => {
            // Recurse per property; every observed value must be a plain object
            // we can read, otherwise this subtree proves nothing.
            let mut obs_objects: Vec<&Vec<crate::resolve::RProp>> = Vec::with_capacity(observed.len());
            for obs in observed {
                match obs {
                    Ty::Object(p) => obs_objects.push(p),
                    _ => return,
                }
            }
            for prop in props {
                let mut child_obs: Vec<Ty> = Vec::with_capacity(obs_objects.len());
                for op in &obs_objects {
                    match op.iter().find(|x| x.name == prop.name) {
                        Some(x) => child_obs.push(x.ty.clone()),
                        None => child_obs.push(if prop.optional { Ty::Undefined } else { Ty::AnyLike }),
                    }
                }
                walk(
                    &prop.ty,
                    &child_obs,
                    format!("{path}.{}", prop.name),
                    depth + 1,
                    prop.optional,
                    max_depth,
                    out,
                );
            }
        }
        _ => {}
    }
}

fn split(t: &Ty) -> Vec<&Ty> {
    match t {
        Ty::Union(parts) => parts.iter().collect(),
        other => vec![other],
    }
}

/// Structural, conservative assignability: `a ⊆ b`. Both call sites use the
/// result to mark constituents as used, so at the recursion cap we return
/// `true` — over-marking is the safe direction.
pub fn assignable(a: &Ty, b: &Ty) -> bool {
    assignable_at(a, b, 0)
}

fn assignable_at(a: &Ty, b: &Ty, depth: usize) -> bool {
    if depth > 64 {
        return true;
    }
    match (a, b) {
        (Ty::AnyLike, _) | (_, Ty::AnyLike) => true,
        (Ty::Union(parts), _) => parts.iter().all(|p| assignable_at(p, b, depth + 1)),
        (_, Ty::Union(parts)) => parts.iter().any(|p| assignable_at(a, p, depth + 1)),
        (Ty::StrLit(x), Ty::StrLit(y)) => x == y,
        (Ty::StrLit(_), Ty::Str) => true,
        (Ty::NumLit(x), Ty::NumLit(y)) => x == y,
        (Ty::NumLit(_), Ty::Num) => true,
        (Ty::BoolLit(x), Ty::BoolLit(y)) => x == y,
        (Ty::BoolLit(_), Ty::Bool) => true,
        (Ty::Str, Ty::Str) | (Ty::Num, Ty::Num) | (Ty::Bool, Ty::Bool) => true,
        (Ty::Undefined, Ty::Undefined) | (Ty::Null, Ty::Null) => true,
        (Ty::EnumLit { enum_name: e1, member: m1 }, Ty::EnumLit { enum_name: e2, member: m2 }) => e1 == e2 && m1 == m2,
        (Ty::Object(ap), Ty::Object(bp)) => bp.iter().all(|need| match ap.iter().find(|x| x.name == need.name) {
            Some(have) => assignable_at(&have.ty, &need.ty, depth + 1),
            None => need.optional,
        }),
        _ => false,
    }
}
