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
    // `boolean` narrows like the union it is.
    let normalized;
    let declared = if *declared == Ty::Bool {
        normalized = Ty::Union(vec![Ty::BoolLit(false), Ty::BoolLit(true)]);
        &normalized
    } else {
        declared
    };

    match declared {
        Ty::Union(constituents) => {
            let mut used = vec![false; constituents.len()];
            for obs in observed {
                for oc in split(obs) {
                    if matches!(oc, Ty::AnyLike) {
                        used.iter_mut().for_each(|u| *u = true);
                        continue;
                    }
                    let mut any = false;
                    for (i, c) in constituents.iter().enumerate() {
                        if assignable(oc, c) {
                            used[i] = true;
                            any = true;
                        }
                    }
                    if !any {
                        // The observed constituent maps to nothing we can
                        // identify (e.g. `string` into a literal union):
                        // we cannot prove anything — mark everything used.
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
                out.push(PathReport { path, declared: print_ty(declared), unused });
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

/// Structural, conservative assignability: `a ⊆ b`.
pub fn assignable(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::AnyLike, _) | (_, Ty::AnyLike) => true,
        (Ty::Union(parts), _) => parts.iter().all(|p| assignable(p, b)),
        (_, Ty::Union(parts)) => parts.iter().any(|p| assignable(a, p)),
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
            Some(have) => assignable(&have.ty, &need.ty),
            None => need.optional,
        }),
        _ => false,
    }
}
