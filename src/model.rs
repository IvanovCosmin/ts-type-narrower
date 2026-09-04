//! Owned IR extracted from each file's AST. Nothing here borrows from the
//! parser arena, so extraction can run in parallel and ASTs are dropped
//! immediately after their file is processed.

use std::collections::HashMap;
use std::path::PathBuf;

pub type ModuleId = usize;

/// A type annotation, structurally extracted but unresolved (references are by name).
#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    Ref(String),
    Union(Vec<TypeExpr>),
    StrLit(String),
    NumLit(String),
    BoolLit(bool),
    Str,
    Num,
    Bool,
    Undefined,
    Null,
    Any,
    Unknown,
    ObjectLit(Vec<ObjProp>),
    /// Anything we do not model (generics, arrays, functions, mapped types, …).
    /// Carries the source text for display. Never matches anything.
    Opaque(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjProp {
    pub name: String,
    pub ty: TypeExpr,
    pub optional: bool,
}

/// A conservatively-inferred argument expression.
#[derive(Debug, Clone)]
pub enum Observed {
    StrLit(String),
    NumLit(String),
    BoolLit(bool),
    Undefined,
    Null,
    /// The expression's type is the given annotation (const/param annotation, `as` cast).
    Typed(TypeExpr),
    /// Enum member access like `Priority.Low`.
    EnumMember { enum_name: String, member: String },
    Object(Vec<(String, Observed)>),
    /// Cannot determine — covers everything.
    Opaque,
}

#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub name: String,
    pub ty: Option<TypeExpr>,
    /// Annotation source text, for display.
    pub ty_text: Option<String>,
    pub optional: bool,
    pub default: Option<Observed>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Owner {
    /// Plain function or const-bound arrow/function expression.
    Free,
    Class(String),
    ObjectConst(String),
}

#[derive(Debug)]
pub struct FnDecl {
    pub name: String,
    pub owner: Owner,
    pub params: Vec<ParamInfo>,
    pub line: u32,
    pub end_line: u32,
    pub exported: bool,
    /// False for generics, rest params, destructured params, zero params, or no body.
    pub eligible: bool,
}

#[derive(Debug, Clone)]
pub enum CallArgs {
    /// Arguments in order; trailing omitted arguments are simply absent.
    Args(Vec<Observed>),
    /// Spread or otherwise uninterpretable argument list — covers everything.
    Opaque,
}

#[derive(Debug)]
pub enum UsageKind {
    Call(CallArgs),
    /// Reference outside callee position: analysis for the target is unsound.
    Escape,
}

/// How a usage names its target; resolved during linking.
#[derive(Debug)]
pub enum UsageTargetRef {
    Local { owner: Owner, name: String },
    Imported { local: String, name: String },
    /// `obj.m(...)` or `obj.m` where `obj` could not be resolved:
    /// conservatively hits every method named `m` in the project.
    AnyMethodNamed(String),
    /// A tracked object/class binding escaped as a value: every method of that
    /// owner must be considered escaped.
    AllMembersOf(Owner),
}

#[derive(Debug)]
pub struct Usage {
    pub target: UsageTargetRef,
    pub kind: UsageKind,
}

#[derive(Debug, Default)]
pub struct ModuleInfo {
    pub path: PathBuf,
    /// Path relative to the analyzed root, forward slashes.
    pub rel: String,
    pub decls: Vec<FnDecl>,
    pub type_aliases: HashMap<String, TypeExpr>,
    /// Enum name -> member names in declaration order.
    pub enums: HashMap<String, Vec<String>>,
    /// Local binding name -> (resolved absolute path of source module, imported name).
    pub imports: HashMap<String, (PathBuf, String)>,
    pub usages: Vec<Usage>,
    pub parse_errors: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Finding {
    pub file: String,
    pub line: u32,
    #[serde(rename = "function")]
    pub function_name: String,
    pub param: String,
    pub path: String,
    pub declared: String,
    pub unused: Vec<String>,
    #[serde(rename = "callCount")]
    pub call_count: usize,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub diff_base: Option<String>,
    pub json: bool,
    pub respect_exports: bool,
    pub max_depth: usize,
    pub timing: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { diff_base: None, json: false, respect_exports: false, max_depth: 6, timing: false }
    }
}
