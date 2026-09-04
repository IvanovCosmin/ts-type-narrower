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

/// How a usage names its target; resolved during linking. Every variant that
/// cannot be resolved MUST degrade to escapes of every plausible target —
/// never to silence — or the soundness guarantee breaks.
#[derive(Debug)]
pub enum UsageTargetRef {
    Local { owner: Owner, name: String },
    /// Through a named or default import; `local` is the binding name in the
    /// using module, resolved via `ModuleInfo::imports`.
    Imported { local: String },
    /// `ns.f(...)` through `import * as ns`.
    NamespaceMember { ns_local: String, name: String },
    /// `obj.m(...)` or `obj.m` where `obj` could not be resolved:
    /// conservatively hits every method named `m` in the project.
    AnyMethodNamed(String),
    /// An identifier call whose callee binding could not be attributed
    /// (shadowed, unbound, or a plain value): conservatively escapes every
    /// same-named free function that this call could reach.
    AnyFreeNamed(String),
    /// A tracked object/class binding escaped as a value: every method of that
    /// owner must be considered escaped.
    AllMembersOf(Owner),
    /// A namespace-import binding escaped as a value: everything the source
    /// module exports must be considered escaped.
    AllExportsOfModule { ns_local: String },
}

#[derive(Debug)]
pub struct Usage {
    pub target: UsageTargetRef,
    pub kind: UsageKind,
    /// 1-based line of the call site (0 for escapes).
    pub line: u32,
}

/// Where an import specifier points. `path: None` means the module could not
/// be resolved (external package, unknown alias) — users of the binding must
/// then taint by name, never be dropped.
pub type ImportEntry = (Option<PathBuf>, String);

#[derive(Debug, Default)]
pub struct ModuleInfo {
    pub path: PathBuf,
    /// Path relative to the analyzed root, forward slashes.
    pub rel: String,
    pub decls: Vec<FnDecl>,
    pub type_aliases: HashMap<String, TypeExpr>,
    /// Enum name -> member names in declaration order.
    pub enums: HashMap<String, Vec<String>>,
    /// Local binding name -> (source module if resolved, imported name).
    /// Namespace imports use "*" as the imported name; default imports use "default".
    pub imports: HashMap<String, ImportEntry>,
    /// `export { x as y } from "./m"`: exported name -> (source module, source name).
    pub reexports_named: HashMap<String, ImportEntry>,
    /// `export * from "./m"`: None entries mean an unresolvable source.
    pub reexports_star: Vec<Option<PathBuf>>,
    /// Name of the declaration exported as `export default`, if identifiable.
    pub default_export: Option<String>,
    pub usages: Vec<Usage>,
    pub parse_errors: usize,
    /// File could not be read as UTF-8 (treated as empty — a soundness hazard
    /// that must at least be surfaced).
    pub read_error: bool,
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
    /// Up to three example call sites ("file:line") as evidence.
    pub sites: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub diff_base: Option<String>,
    pub json: bool,
    pub respect_exports: bool,
    pub max_depth: usize,
    pub timing: bool,
    pub fail_on_findings: bool,
    /// Suppress the stderr summary line.
    pub quiet: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            diff_base: None,
            json: false,
            respect_exports: false,
            max_depth: 6,
            timing: false,
            fail_on_findings: false,
            quiet: false,
        }
    }
}

/// Run statistics surfaced to stderr so silent degradation is visible.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Stats {
    pub files: usize,
    pub decls: usize,
    pub analyzed: usize,
    pub parse_error_files: usize,
    pub read_error_files: usize,
}
