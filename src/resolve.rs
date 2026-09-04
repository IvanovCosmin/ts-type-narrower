//! Resolution of extracted `TypeExpr` / `Observed` values into semantic types
//! (`Ty`), following aliases, interfaces, enums, and imports across modules.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::model::*;

#[derive(Debug, Clone, PartialEq)]
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
    /// Unmodeled type; never matches anything.
    Opaque(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RProp {
    pub name: String,
    pub ty: Ty,
    pub optional: bool,
}

pub struct Resolver<'m> {
    pub modules: &'m [ModuleInfo],
    pub by_path: &'m HashMap<PathBuf, ModuleId>,
}

impl Resolver<'_> {
    pub fn resolve_expr(&self, te: &TypeExpr, mid: ModuleId) -> Ty {
        let mut stack = Vec::new();
        self.resolve_inner(te, mid, &mut stack)
    }

    fn resolve_inner(&self, te: &TypeExpr, mid: ModuleId, stack: &mut Vec<(ModuleId, String)>) -> Ty {
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
            TypeExpr::Opaque(s) => Ty::Opaque(s.clone()),
        }
    }

    fn resolve_name(&self, name: &str, mid: ModuleId, stack: &mut Vec<(ModuleId, String)>) -> Ty {
        let key = (mid, name.to_string());
        if stack.contains(&key) {
            return Ty::Opaque(name.to_string());
        }
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
            stack.push(key);
            let t = self.resolve_inner(alias, mid, stack);
            stack.pop();
            return t;
        }
        if let Some((path, imported)) = m.imports.get(name) {
            if let Some(&mid2) = self.by_path.get(path) {
                stack.push(key);
                let t = self.resolve_name(imported, mid2, stack);
                stack.pop();
                return t;
            }
        }
        Ty::Opaque(name.to_string())
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
                } else if let Some((path, imported)) = m.imports.get(enum_name) {
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

fn dedupe(v: &mut Vec<Ty>) {
    let mut out: Vec<Ty> = Vec::with_capacity(v.len());
    for t in v.drain(..) {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    *v = out;
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
        Ty::Opaque(s) => s.clone(),
    }
}
