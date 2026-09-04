//! Owned IR extracted from each file's AST. Nothing here borrows from the
//! parser arena, so extraction can run in parallel and ASTs are dropped
//! immediately after their file is processed.

use std::collections::{HashMap, HashSet};
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
    /// Array type: `E[]`, `Array<E>`, `ReadonlyArray<E>`, `readonly E[]`.
    Arr(Box<TypeExpr>),
    /// Property projection: the type of `base[prop]`, used for destructured
    /// parameter bindings (`{ variant }: BadgeProps` binds `variant` to
    /// `Proj(BadgeProps, "variant")`). Only ever feeds OBSERVED types — its
    /// unresolvable fallback is AnyLike (wide), which is safe for observations
    /// and would be unsound for declared types.
    Proj(Box<TypeExpr>, String),
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
    /// The element type of an array-typed receiver: what an array-method
    /// callback (`arr.map(cb)`) observes as its first argument. Resolves wide
    /// (AnyLike) whenever the base isn't provably an array — observed-only,
    /// like Proj.
    ElemOf(Box<TypeExpr>),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    Generic,
    RestParam,
    NoParams,
    Overload,
}

#[derive(Debug)]
pub struct FnDecl {
    pub name: String,
    pub owner: Owner,
    pub params: Vec<ParamInfo>,
    pub line: u32,
    pub end_line: u32,
    pub exported: bool,
    /// private/protected class method: unreachable from outside the class
    /// hierarchy, so class-value escapes don't affect it (only `this.*` calls
    /// and name-based taints can).
    pub non_public: bool,
    /// Why this declaration cannot be analyzed, if it can't.
    pub skip: Option<SkipReason>,
    /// Convenience: `skip.is_none()`.
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
    /// A module-namespace VALUE was created in an untracked position
    /// (`import("./x")` / `require("./x")` flowing somewhere): everything that
    /// module exports escapes.
    AllExportsOfPath(PathBuf),
    /// Member access on an object we cannot attribute. Escapes same-named
    /// exported free functions, but ONLY when some module in the project
    /// created a namespace value we could not track (computed or unresolvable
    /// dynamic import) — otherwise every namespace value is tracked at its
    /// source and this taint is vacuous.
    MemberFreeNamed(String),
    /// `this.m(...)` inside a class body: a call to the method resolved
    /// through the inheritance chain, plus escapes for overrides in
    /// descendant classes (the receiver may be a subclass instance).
    ThisMethod { class: String, name: String },
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
    /// Enum name -> (member name, is_string_member) in declaration order.
    pub enums: HashMap<String, Vec<(String, bool)>>,
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
    /// True when the file contains any import/export syntax. Script files
    /// (false) share the global scope, so unbound identifier calls anywhere
    /// can reach their functions.
    pub is_module: bool,
    /// A namespace value escaped tracking here: computed `import(expr)`,
    /// or a dynamic import/require whose literal specifier didn't resolve.
    /// Activates the project-wide MemberFreeNamed taints.
    pub has_untracked_namespace: bool,
    /// Every class declared in this module (methodless ones included), so the
    /// inheritance graph has complete nodes.
    pub classes_declared: HashSet<String>,
    /// Names exported via source-less specifiers: (local, exported) pairs, so
    /// `import { x } from ...; export { x as y }` becomes a re-export edge.
    pub export_specifiers: Vec<(String, String)>,
    /// Class name -> local name of its `extends` base (identifier heritage
    /// only). Classes extending expressions are recorded with "" (unknown
    /// parent — treated as a potential descendant of anything).
    pub class_extends: HashMap<String, String>,
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

/// An eligible, un-escaped function with zero visible direct calls — a
/// dead-function candidate under the closed-world assumption.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UncalledFn {
    pub file: String,
    pub line: u32,
    #[serde(rename = "function")]
    pub function_name: String,
    pub exported: bool,
}

/// Complete result of a run: what --json emits (plus version).
#[derive(Debug, Clone)]
pub struct Analysis {
    pub findings: Vec<Finding>,
    pub stats: Stats,
    pub uncalled: Vec<UncalledFn>,
    /// Soundness-relevant warnings (parse errors, unreadable files, sub-root
    /// analysis). Printed even under --quiet.
    pub warnings: Vec<String>,
}

/// Run statistics surfaced to stderr so silent degradation is visible.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Stats {
    pub files: usize,
    pub decls: usize,
    /// Eligible, not escaped, and with at least one observed call.
    pub analyzed: usize,
    pub skipped_generic: usize,
    pub skipped_overload: usize,
    pub skipped_rest_param: usize,
    pub skipped_no_params: usize,
    /// Referenced outside callee position (or hit by a conservative taint).
    pub escaped: usize,
    /// Eligible and un-escaped but with zero visible direct calls —
    /// dead-function candidates under the closed-world assumption.
    pub uncalled: usize,
    pub parse_error_files: usize,
    pub read_error_files: usize,
}
