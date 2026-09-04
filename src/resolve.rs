//! Resolution of extracted `TypeExpr` / `Observed` values into semantic types
//! (`Ty`), following aliases, interfaces, enums, and imports across modules.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::RwLock;

use crate::model::*;

const RESOLVE_DEPTH_LIMIT: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    StrLit(String),
    NumLit(String),
    BoolLit(bool),
    Str,
    Num,
    Bool,
    Undefined,
    Null,
    /// any / unknown / uninterpretable value: assignable both ways, covers all.
    AnyLike,
    EnumLit { enum_name: String, member: String },
    Union(Vec<Ty>),
    Object(Vec<RProp>),
    Arr(Box<Ty>),
    /// Unmodeled type; never matches anything.
    Opaque(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RProp {
    pub name: String,
    pub ty: Ty,
    pub optional: bool,
}

pub struct Resolver<'m> {
    pub modules: &'m [ModuleInfo],
    pub by_path: &'m HashMap<PathBuf, ModuleId>,
    /// Memoized top-level name resolutions; names are re-queried once per call
    /// argument, which is quadratic without this.
    cache: RwLock<HashMap<(ModuleId, String), Ty>>,
}

impl<'m> Resolver<'m> {
    pub fn new(modules: &'m [ModuleInfo], by_path: &'m HashMap<PathBuf, ModuleId>) -> Self {
        Self { modules, by_path, cache: RwLock::new(HashMap::new()) }
    }

    pub fn resolve_expr(&self, te: &TypeExpr, mid: ModuleId) -> Ty {
        let mut stack = Vec::new();
        self.resolve_inner(te, mid, &mut stack)
    }

    fn resolve_inner(&self, te: &TypeExpr, mid: ModuleId, stack: &mut Vec<(ModuleId, String)>) -> Ty {
        if stack.len() > RESOLVE_DEPTH_LIMIT {
            return Ty::Opaque("<deep>".to_string());
        }
        match te {
            TypeExpr::Ref(name) => self.resolve_name(name, mid, stack),
            TypeExpr::Union(parts) => {
                let mut out: Vec<Ty> = Vec::new();
                for p in parts {
                    match self.resolve_inner(p, mid, stack) {
                        Ty::Union(inner) => out.extend(inner),
                        t => out.push(t),
                    }
                }
                dedupe(&mut out);
                Ty::Union(out)
            }
            TypeExpr::ObjectLit(props) => Ty::Object(
                props
                    .iter()
                    .map(|p| RProp {
                        name: p.name.clone(),
                        ty: self.resolve_inner(&p.ty, mid, stack),
                        optional: p.optional,
                    })
                    .collect(),
            ),
            TypeExpr::StrLit(s) => Ty::StrLit(s.clone()),
            TypeExpr::NumLit(n) => Ty::NumLit(n.clone()),
            TypeExpr::BoolLit(b) => Ty::BoolLit(*b),
            TypeExpr::Str => Ty::Str,
            TypeExpr::Num => Ty::Num,
            TypeExpr::Bool => Ty::Bool,
            TypeExpr::Undefined => Ty::Undefined,
            TypeExpr::Null => Ty::Null,
            TypeExpr::Any | TypeExpr::Unknown => Ty::AnyLike,
            TypeExpr::Arr(e) => Ty::Arr(Box::new(self.resolve_inner(e, mid, stack))),
            TypeExpr::Proj(base, prop) => {
                let base = self.resolve_inner(base, mid, stack);
                project(&base, prop)
            }
            TypeExpr::Opaque(s) => Ty::Opaque(s.clone()),
        }
    }

    fn resolve_name(&self, name: &str, mid: ModuleId, stack: &mut Vec<(ModuleId, String)>) -> Ty {
        let key = (mid, name.to_string());
        if stack.contains(&key) || stack.len() > RESOLVE_DEPTH_LIMIT {
            return Ty::Opaque(name.to_string());
        }
        // Only cache resolutions that started outside any alias chain, so
        // cycle-truncated intermediates never poison the cache.
        let cacheable = stack.is_empty();
        if cacheable {
            if let Some(t) = self.cache.read().unwrap().get(&key) {
                return t.clone();
            }
        }
        let t = self.resolve_name_uncached(name, mid, stack, &key);
        if cacheable {
            self.cache.write().unwrap().insert(key, t.clone());
        }
        t
    }

    fn resolve_name_uncached(
        &self,
        name: &str,
        mid: ModuleId,
        stack: &mut Vec<(ModuleId, String)>,
        key: &(ModuleId, String),
    ) -> Ty {
        let m = &self.modules[mid];
        if let Some(members) = m.enums.get(name) {
            return Ty::Union(
                members
                    .iter()
                    .map(|mem| Ty::EnumLit { enum_name: name.to_string(), member: mem.clone() })
                    .collect(),
            );
        }
        if let Some(alias) = m.type_aliases.get(name) {
            stack.push(key.clone());
            let t = self.resolve_inner(alias, mid, stack);
            stack.pop();
            return t;
        }
        if let Some((Some(path), imported)) = m.imports.get(name) {
            if let Some(&mid2) = self.by_path.get(path) {
                stack.push(key.clone());
                let t = self.resolve_name(imported, mid2, stack);
                stack.pop();
                return t;
            }
        }
        // Types can also arrive through re-export chains.
        if let Some(t) = self.resolve_reexported(name, mid, stack) {
            return t;
        }
        Ty::Opaque(name.to_string())
    }

    fn resolve_reexported(&self, name: &str, mid: ModuleId, stack: &mut Vec<(ModuleId, String)>) -> Option<Ty> {
        let m = &self.modules[mid];
        if let Some((Some(path), source_name)) = m.reexports_named.get(name) {
            if let Some(&mid2) = self.by_path.get(path) {
                let key = (mid, format!("reexport:{name}"));
                if stack.contains(&key) || stack.len() > RESOLVE_DEPTH_LIMIT {
                    return Some(Ty::Opaque(name.to_string()));
                }
                stack.push(key);
                let t = self.resolve_name(source_name, mid2, stack);
                stack.pop();
                return Some(t);
            }
        }
        for star in &m.reexports_star {
            if let Some(path) = star {
                if let Some(&mid2) = self.by_path.get(path) {
                    let m2 = &self.modules[mid2];
                    if m2.type_aliases.contains_key(name) || m2.enums.contains_key(name) {
                        let key = (mid, format!("star:{name}"));
                        if stack.contains(&key) || stack.len() > RESOLVE_DEPTH_LIMIT {
                            return Some(Ty::Opaque(name.to_string()));
                        }
                        stack.push(key);
                        let t = self.resolve_name(name, mid2, stack);
                        stack.pop();
                        return Some(t);
                    }
                }
            }
        }
        None
    }

    /// Resolve an observed argument in the module where the call appears.
    pub fn resolve_observed(&self, obs: &Observed, mid: ModuleId) -> Ty {
        match obs {
            Observed::StrLit(s) => Ty::StrLit(s.clone()),
            Observed::NumLit(n) => Ty::NumLit(n.clone()),
            Observed::BoolLit(b) => Ty::BoolLit(*b),
            Observed::Undefined => Ty::Undefined,
            Observed::Null => Ty::Null,
            Observed::Typed(te) => self.resolve_expr(te, mid),
            Observed::EnumMember { enum_name, member } => {
                let m = &self.modules[mid];
                if let Some(members) = m.enums.get(enum_name) {
                    if members.contains(member) {
                        return Ty::EnumLit { enum_name: enum_name.clone(), member: member.clone() };
                    }
                } else if let Some((Some(path), imported)) = m.imports.get(enum_name) {
                    if let Some(&mid2) = self.by_path.get(path) {
                        if let Some(members) = self.modules[mid2].enums.get(imported) {
                            if members.contains(member) {
                                return Ty::EnumLit { enum_name: imported.clone(), member: member.clone() };
                            }
                        }
                    }
                }
                Ty::AnyLike
            }
            Observed::ElemOf(base) => {
                let base = self.resolve_expr(base, mid);
                project_elem(&base)
            }
            Observed::Object(props) => Ty::Object(
                props
                    .iter()
                    .map(|(n, o)| RProp { name: n.clone(), ty: self.resolve_observed(o, mid), optional: false })
                    .collect(),
            ),
            Observed::Opaque => Ty::AnyLike,
        }
    }
}

/// The element type of an array. Observed-only: unresolvable cases fall back
/// to AnyLike (covers everything).
fn project_elem(base: &Ty) -> Ty {
    match base {
        Ty::Arr(e) => (**e).clone(),
        Ty::Union(parts) => {
            let mut out: Vec<Ty> = Vec::new();
            for part in parts {
                match project_elem(part) {
                    Ty::AnyLike => return Ty::AnyLike,
                    Ty::Union(inner) => out.extend(inner),
                    t => out.push(t),
                }
            }
            dedupe(&mut out);
            Ty::Union(out)
        }
        Ty::AnyLike => Ty::AnyLike,
        _ => Ty::AnyLike,
    }
}

/// The type of `base[prop]`. This feeds observed types only, so every
/// unresolvable case falls back to AnyLike (covers everything — safe for
/// observations, which can only mark constituents as used).
fn project(base: &Ty, prop: &str) -> Ty {
    match base {
        Ty::Object(props) => match props.iter().find(|p| p.name == prop) {
            Some(p) if p.optional => match &p.ty {
                Ty::Union(parts) => {
                    let mut parts = parts.clone();
                    if !parts.contains(&Ty::Undefined) {
                        parts.push(Ty::Undefined);
                    }
                    Ty::Union(parts)
                }
                other => Ty::Union(vec![other.clone(), Ty::Undefined]),
            },
            Some(p) => p.ty.clone(),
            None => Ty::AnyLike,
        },
        Ty::Union(parts) => {
            let mut out: Vec<Ty> = Vec::new();
            for part in parts {
                match project(part, prop) {
                    Ty::AnyLike => return Ty::AnyLike,
                    Ty::Union(inner) => out.extend(inner),
                    t => out.push(t),
                }
            }
            dedupe(&mut out);
            Ty::Union(out)
        }
        _ => Ty::AnyLike,
    }
}

fn dedupe(v: &mut Vec<Ty>) {
    let mut seen: HashSet<Ty> = HashSet::with_capacity(v.len());
    v.retain(|t| seen.insert(t.clone()));
}

pub fn print_ty(t: &Ty) -> String {
    match t {
        Ty::StrLit(s) => format!("\"{s}\""),
        Ty::NumLit(n) => n.clone(),
        Ty::BoolLit(b) => b.to_string(),
        Ty::Str => "string".to_string(),
        Ty::Num => "number".to_string(),
        Ty::Bool => "boolean".to_string(),
        Ty::Undefined => "undefined".to_string(),
        Ty::Null => "null".to_string(),
        Ty::AnyLike => "any".to_string(),
        Ty::EnumLit { enum_name, member } => format!("{enum_name}.{member}"),
        Ty::Union(parts) => parts.iter().map(print_ty).collect::<Vec<_>>().join(" | "),
        Ty::Object(props) => {
            let mut s = String::from("{ ");
            for p in props {
                s.push_str(&p.name);
                if p.optional {
                    s.push('?');
                }
                s.push_str(": ");
                s.push_str(&print_ty(&p.ty));
                s.push_str("; ");
            }
            s.push('}');
            s
        }
        Ty::Arr(e) => match &**e {
            Ty::Union(_) => format!("({})[]", print_ty(e)),
            _ => format!("{}[]", print_ty(e)),
        },
        Ty::Opaque(s) => s.clone(),
    }
}
