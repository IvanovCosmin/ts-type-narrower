//! Per-file extraction: parse with oxc, convert everything the analysis needs
//! into the owned IR in `model`, drop the AST. Runs in parallel across files.
//!
//! Soundness invariant: any reference this pass cannot attribute precisely
//! must be recorded as a (possibly broad) escape or an opaque observation —
//! never silently dropped. Dropped calls manufacture false "never passed"
//! findings; broad escapes only cost coverage.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_ast_visit::{walk, Visit};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use oxc_syntax::scope::ScopeFlags;

use crate::model::*;
use crate::workspace::SpecifierMap;

const TYPE_DEPTH_LIMIT: usize = 200;
const STMT_DEPTH_LIMIT: usize = 500;

/// What a name in scope refers to, for conservative resolution.
#[derive(Debug, Clone)]
enum Binding {
    /// Top-level function or const-bound arrow/function expression (a call target).
    Fn,
    /// Imported binding (named or default); resolved through the module's
    /// import map at link time. Unresolvable imports still get this binding so
    /// their uses taint by name instead of vanishing.
    Import,
    /// `import * as ns from ...`.
    Namespace,
    Const { ann: Option<TypeExpr>, lit: Option<Observed> },
    Param { ann: Option<TypeExpr> },
    /// `const obj = { m() {} }` — methods are call targets.
    ObjectConst,
    /// `const x = new Cls()`.
    Instance(String),
    ClassDecl,
    EnumDecl,
    /// Anything else local (let, nested function, catch var, block shadow, …):
    /// shadows outer names; calls through it are unattributable.
    LocalOther,
}

pub fn extract_module(path: &Path, rel: String, source: &str, specmap: &SpecifierMap) -> ModuleInfo {
    let tsx = path.extension().is_some_and(|e| e == "tsx");
    let source_type = if tsx { SourceType::tsx() } else { SourceType::ts() };
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, source_type).parse();

    let line_starts = build_line_starts(source);
    let mut info = ModuleInfo {
        path: path.to_path_buf(),
        rel,
        parse_errors: ret.diagnostics.len(),
        ..Default::default()
    };

    let mut module_scope: HashMap<String, Binding> = HashMap::new();
    let mut exported: HashSet<String> = HashSet::new();
    let mut bodyless_fns: HashSet<String> = HashSet::new();

    // ---- Pre-pass: top-level declarations, bindings, type environment. ----
    for stmt in &ret.program.body {
        prepass_stmt(stmt, source, path, specmap, &line_starts, &mut info, &mut module_scope, &mut exported, &mut bodyless_fns);
    }

    // Overloads / merged declarations: any name with a bodyless signature or a
    // duplicate implementation is ineligible.
    let mut seen: HashMap<(Owner, String), usize> = HashMap::new();
    for d in &info.decls {
        *seen.entry((d.owner.clone(), d.name.clone())).or_default() += 1;
    }
    for d in &mut info.decls {
        if seen[&(d.owner.clone(), d.name.clone())] > 1 {
            d.skip = Some(SkipReason::Overload);
            d.eligible = false;
        }
        if d.owner == Owner::Free && bodyless_fns.contains(&d.name) {
            d.skip = Some(SkipReason::Overload);
            d.eligible = false;
        }
        // Export via modifier was set at declaration time; extend to names in
        // `export { ... }` / `export default`, including class/object owners.
        let owner_name = match &d.owner {
            Owner::Free => Some(&d.name),
            Owner::Class(c) | Owner::ObjectConst(c) => Some(c),
        };
        if let Some(n) = owner_name {
            if exported.contains(n) || info.default_export.as_deref() == Some(n) {
                d.exported = true;
            }
        }
    }

    // ---- Usage pass. ----
    let mut v = UsageVisitor {
        src: source,
        line_starts: &line_starts,
        scopes: vec![module_scope],
        type_shadows: vec![HashSet::new()],
        usages: Vec::new(),
        emitted_escapes: HashSet::new(),
    };
    v.visit_program(&ret.program);
    info.usages = v.usages;
    info
}

fn build_line_starts(src: &str) -> Vec<u32> {
    let mut v = vec![0u32];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            v.push(i as u32 + 1);
        }
    }
    v
}

fn line_of(line_starts: &[u32], offset: u32) -> u32 {
    match line_starts.binary_search(&offset) {
        Ok(i) => i as u32 + 1,
        Err(i) => i as u32,
    }
}

fn span_text<'a>(src: &'a str, span: oxc_span::Span) -> &'a str {
    &src[span.start as usize..span.end as usize]
}

fn canonical_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

// ---------------------------------------------------------------------------
// Import resolution
// ---------------------------------------------------------------------------

/// Resolve an import specifier to an on-disk module file. `None` means
/// unresolvable — the caller must record the import as unresolved (taint),
/// never drop it.
fn resolve_import(from: &Path, spec: &str, specmap: &SpecifierMap) -> Option<PathBuf> {
    if spec.starts_with('.') {
        let dir = from.parent()?;
        probe_candidates(&normalize(&dir.join(spec)))
    } else {
        specmap.resolve(spec).iter().find_map(|base| probe_candidates(&normalize(base)))
    }
}

/// Try the TypeScript file-candidate dance for a base path: extension
/// appending (never `with_extension`, which eats dotted names like
/// `foo.service`), `.js`-family suffix swaps, and index files.
fn probe_candidates(base: &Path) -> Option<PathBuf> {
    let s = base.to_string_lossy();
    let mut cands: Vec<PathBuf> = Vec::new();
    for (js, ts) in [(".js", ".ts"), (".js", ".tsx"), (".jsx", ".tsx"), (".mjs", ".mts"), (".cjs", ".cts")] {
        if let Some(stripped) = s.strip_suffix(js) {
            cands.push(PathBuf::from(format!("{stripped}{ts}")));
        }
    }
    cands.push(PathBuf::from(format!("{s}.ts")));
    cands.push(PathBuf::from(format!("{s}.tsx")));
    if base
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "ts" | "tsx" | "mts" | "cts"))
    {
        cands.push(base.to_path_buf());
    }
    for idx in ["index.ts", "index.tsx", "src/index.ts", "src/index.tsx"] {
        cands.push(base.join(idx));
    }
    cands.into_iter().find(|c| c.is_file())
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn export_name_to_string(n: &ModuleExportName) -> String {
    match n {
        ModuleExportName::IdentifierName(x) => x.name.to_string(),
        ModuleExportName::IdentifierReference(x) => x.name.to_string(),
        ModuleExportName::StringLiteral(x) => x.value.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Pre-pass
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn prepass_stmt(
    stmt: &Statement,
    src: &str,
    path: &Path,
    specmap: &SpecifierMap,
    line_starts: &[u32],
    info: &mut ModuleInfo,
    scope: &mut HashMap<String, Binding>,
    exported: &mut HashSet<String>,
    bodyless_fns: &mut HashSet<String>,
) {
    match stmt {
        Statement::ImportDeclaration(d) => {
            let Some(specs) = &d.specifiers else { return };
            let source_path = resolve_import(path, d.source.value.as_str(), specmap);
            for s in specs {
                match s {
                    ImportDeclarationSpecifier::ImportSpecifier(is) => {
                        let imported = export_name_to_string(&is.imported);
                        let local = is.local.name.to_string();
                        info.imports.insert(local.clone(), (source_path.clone(), imported));
                        scope.insert(local, Binding::Import);
                    }
                    ImportDeclarationSpecifier::ImportDefaultSpecifier(x) => {
                        let local = x.local.name.to_string();
                        info.imports.insert(local.clone(), (source_path.clone(), "default".to_string()));
                        scope.insert(local, Binding::Import);
                    }
                    ImportDeclarationSpecifier::ImportNamespaceSpecifier(x) => {
                        let local = x.local.name.to_string();
                        info.imports.insert(local.clone(), (source_path.clone(), "*".to_string()));
                        scope.insert(local, Binding::Namespace);
                    }
                }
            }
        }
        Statement::ExportNamedDeclaration(d) => {
            if let Some(source) = &d.source {
                // Re-export: `export { a as b } from "./m"`.
                let source_path = resolve_import(path, source.value.as_str(), specmap);
                for s in &d.specifiers {
                    let exported_name = export_name_to_string(&s.exported);
                    let source_name = export_name_to_string(&s.local);
                    info.reexports_named.insert(exported_name, (source_path.clone(), source_name));
                }
            } else {
                for s in &d.specifiers {
                    exported.insert(export_name_to_string(&s.local));
                }
            }
            if let Some(decl) = &d.declaration {
                prepass_declaration(decl, true, src, path, specmap, line_starts, info, scope);
            }
        }
        Statement::ExportAllDeclaration(d) => {
            // `export * as ns from ...` re-exports a namespace object; we model
            // it as an unresolvable star so misses taint instead of vanishing.
            if d.exported.is_some() {
                info.reexports_star.push(None);
            } else {
                info.reexports_star.push(resolve_import(path, d.source.value.as_str(), specmap));
            }
        }
        Statement::ExportDefaultDeclaration(d) => match &d.declaration {
            ExportDefaultDeclarationKind::Identifier(id) => {
                info.default_export = Some(id.name.to_string());
                exported.insert(id.name.to_string());
            }
            ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                prepass_function(f, true, src, line_starts, info, scope, bodyless_fns);
                if let Some(id) = &f.id {
                    info.default_export = Some(id.name.to_string());
                }
            }
            ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                prepass_class(c, true, src, line_starts, info, scope);
                if let Some(id) = &c.id {
                    info.default_export = Some(id.name.to_string());
                }
            }
            _ => {}
        },
        Statement::VariableDeclaration(v) => prepass_var(v, false, src, path, specmap, line_starts, info, scope),
        Statement::FunctionDeclaration(f) => prepass_function(f, false, src, line_starts, info, scope, bodyless_fns),
        Statement::ClassDeclaration(c) => prepass_class(c, false, src, line_starts, info, scope),
        Statement::TSTypeAliasDeclaration(t) => prepass_type_alias(t, src, info),
        Statement::TSInterfaceDeclaration(t) => prepass_interface(t, src, info),
        Statement::TSEnumDeclaration(t) => prepass_enum(t, info, scope),
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn prepass_declaration(
    decl: &Declaration,
    exported_ctx: bool,
    src: &str,
    path: &Path,
    specmap: &SpecifierMap,
    line_starts: &[u32],
    info: &mut ModuleInfo,
    scope: &mut HashMap<String, Binding>,
) {
    let mut bodyless = HashSet::new();
    match decl {
        Declaration::VariableDeclaration(v) => prepass_var(v, exported_ctx, src, path, specmap, line_starts, info, scope),
        Declaration::FunctionDeclaration(f) => {
            prepass_function(f, exported_ctx, src, line_starts, info, scope, &mut bodyless);
            // A bodyless `export function` signature: mark via decls pass by
            // inserting a duplicate-suppressor. Simplest: mark ineligible now.
            if !bodyless.is_empty() {
                for d in &mut info.decls {
                    if d.owner == Owner::Free && bodyless.contains(&d.name) {
                        d.skip = Some(SkipReason::Overload);
                        d.eligible = false;
                    }
                }
            }
        }
        Declaration::ClassDeclaration(c) => prepass_class(c, exported_ctx, src, line_starts, info, scope),
        Declaration::TSTypeAliasDeclaration(t) => prepass_type_alias(t, src, info),
        Declaration::TSInterfaceDeclaration(t) => prepass_interface(t, src, info),
        Declaration::TSEnumDeclaration(t) => prepass_enum(t, info, scope),
        _ => {}
    }
}

fn prepass_type_alias(t: &TSTypeAliasDeclaration, src: &str, info: &mut ModuleInfo) {
    let name = t.id.name.to_string();
    let expr = if t.type_parameters.is_some() || info.type_aliases.contains_key(&name) {
        // Generic aliases and merged/duplicate declarations are unmodeled.
        TypeExpr::Opaque(span_text(src, t.span).to_string())
    } else {
        ts_type_to_expr(&t.type_annotation, src, 0)
    };
    info.type_aliases.insert(name, expr);
}

fn prepass_interface(t: &TSInterfaceDeclaration, src: &str, info: &mut ModuleInfo) {
    let name = t.id.name.to_string();
    let expr = if t.type_parameters.is_some() || !t.extends.is_empty() || info.type_aliases.contains_key(&name) {
        // Declaration merging (a second interface of the same name) makes the
        // combined shape unknowable to us: degrade to Opaque.
        TypeExpr::Opaque(span_text(src, t.span).to_string())
    } else {
        signatures_to_object(&t.body.body, src, 0)
    };
    info.type_aliases.insert(name, expr);
}

fn prepass_enum(t: &TSEnumDeclaration, info: &mut ModuleInfo, scope: &mut HashMap<String, Binding>) {
    let name = t.id.name.to_string();
    let members: Vec<String> = t
        .body
        .members
        .iter()
        .filter_map(|m| match &m.id {
            TSEnumMemberName::Identifier(n) => Some(n.name.to_string()),
            TSEnumMemberName::String(s) => Some(s.value.to_string()),
            _ => None,
        })
        .collect();
    // Merged enum declarations: extend, don't overwrite.
    info.enums.entry(name.clone()).or_default().extend(members);
    scope.insert(name, Binding::EnumDecl);
}

fn prepass_function(
    f: &Function,
    exported_ctx: bool,
    src: &str,
    line_starts: &[u32],
    info: &mut ModuleInfo,
    scope: &mut HashMap<String, Binding>,
    bodyless_fns: &mut HashSet<String>,
) {
    let Some(id) = &f.id else { return };
    let name = id.name.to_string();
    scope.insert(name.clone(), Binding::Fn);
    if f.body.is_none() {
        bodyless_fns.insert(name);
        return;
    }
    let skip = skip_reason_of(f.type_parameters.is_some(), &f.params);
    info.decls.push(FnDecl {
        name,
        owner: Owner::Free,
        params: extract_params(&f.params, src),
        line: line_of(line_starts, f.span.start),
        end_line: line_of(line_starts, f.span.end),
        exported: exported_ctx,
        skip,
        eligible: skip.is_none(),
    });
}

fn prepass_class(
    c: &Class,
    exported_ctx: bool,
    src: &str,
    line_starts: &[u32],
    info: &mut ModuleInfo,
    scope: &mut HashMap<String, Binding>,
) {
    let Some(id) = &c.id else { return };
    let class_name = id.name.to_string();
    scope.insert(class_name.clone(), Binding::ClassDecl);
    // Inheritance is unmodeled: a subclass can expose this class's methods
    // under its own name (see link-time AnyMethodNamed handling); a class that
    // *extends* something also inherits methods we can't see. Mark methods of
    // deriving classes ineligible-safe by leaving them out when a superclass
    // exists? No: analyzing them is fine — calls on subclass instances that we
    // can't attribute already escape by method name. But methods of THIS class
    // may be called through subclass instances whose class we resolve —
    // handled at link time by falling back to AnyMethodNamed when the
    // (class, method) pair is missing.
    for el in &c.body.body {
        if let ClassElement::MethodDefinition(m) = el {
            if m.kind != MethodDefinitionKind::Method || m.r#static || m.computed {
                continue;
            }
            let PropertyKey::StaticIdentifier(key) = &m.key else { continue };
            let func = &m.value;
            if func.body.is_none() {
                continue;
            }
            let skip = skip_reason_of(func.type_parameters.is_some(), &func.params);
            info.decls.push(FnDecl {
                name: key.name.to_string(),
                owner: Owner::Class(class_name.clone()),
                params: extract_params(&func.params, src),
                line: line_of(line_starts, m.span.start),
                end_line: line_of(line_starts, m.span.end),
                exported: exported_ctx,
                skip,
                eligible: skip.is_none(),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn prepass_var(
    v: &VariableDeclaration,
    exported_ctx: bool,
    src: &str,
    path: &Path,
    specmap: &SpecifierMap,
    line_starts: &[u32],
    info: &mut ModuleInfo,
    scope: &mut HashMap<String, Binding>,
) {
    let is_const = v.kind == VariableDeclarationKind::Const;
    for d in &v.declarations {
        let BindingPattern::BindingIdentifier(id) = &d.id else { continue };
        let name = id.name.to_string();
        // `const m = await import("./x")` binds a namespace-like object.
        if let Some(spec) = dynamic_import_specifier(d.init.as_ref()) {
            let resolved = resolve_import(path, &spec, specmap);
            info.imports.insert(name.clone(), (resolved, "*".to_string()));
            scope.insert(name, Binding::Namespace);
            continue;
        }
        let binding = classify_declarator(d, is_const, name.as_str(), exported_ctx, src, line_starts, Some(info));
        scope.insert(name, binding);
    }
}

/// Shared between the pre-pass (top level, `info` present so methods/arrows
/// become targets) and the usage visitor (locals, `info` absent).
fn classify_declarator(
    d: &VariableDeclarator,
    is_const: bool,
    name: &str,
    exported_ctx: bool,
    src: &str,
    line_starts: &[u32],
    info: Option<&mut ModuleInfo>,
) -> Binding {
    let ann = d.type_annotation.as_ref().map(|t| ts_type_to_expr(&t.type_annotation, src, 0));
    match &d.init {
        Some(Expression::ArrowFunctionExpression(a)) => {
            if let Some(info) = info {
                let skip = skip_reason_of(a.type_parameters.is_some(), &a.params);
                info.decls.push(FnDecl {
                    name: name.to_string(),
                    owner: Owner::Free,
                    params: extract_params(&a.params, src),
                    line: line_of(line_starts, a.span.start),
                    end_line: line_of(line_starts, a.span.end),
                    exported: exported_ctx,
                    skip,
                    eligible: skip.is_none(),
                });
                Binding::Fn
            } else {
                Binding::LocalOther
            }
        }
        Some(Expression::FunctionExpression(f)) => {
            if let Some(info) = info {
                let skip = if f.body.is_none() {
                    Some(SkipReason::Overload)
                } else {
                    skip_reason_of(f.type_parameters.is_some(), &f.params)
                };
                info.decls.push(FnDecl {
                    name: name.to_string(),
                    owner: Owner::Free,
                    params: extract_params(&f.params, src),
                    line: line_of(line_starts, f.span.start),
                    end_line: line_of(line_starts, f.span.end),
                    exported: exported_ctx,
                    skip,
                    eligible: skip.is_none(),
                });
                Binding::Fn
            } else {
                Binding::LocalOther
            }
        }
        Some(Expression::ObjectExpression(o)) if is_const => {
            let mut has_methods = false;
            if let Some(info) = info {
                for p in &o.properties {
                    let ObjectPropertyKind::ObjectProperty(prop) = p else { continue };
                    if prop.computed {
                        continue;
                    }
                    let PropertyKey::StaticIdentifier(key) = &prop.key else { continue };
                    let (params, type_params, has_body, span) = match &prop.value {
                        Expression::FunctionExpression(f) => (&f.params, &f.type_parameters, f.body.is_some(), prop.span),
                        Expression::ArrowFunctionExpression(a) => (&a.params, &a.type_parameters, true, prop.span),
                        _ => continue,
                    };
                    if !has_body {
                        continue;
                    }
                    has_methods = true;
                    let skip = skip_reason_of(type_params.is_some(), params);
                    info.decls.push(FnDecl {
                        name: key.name.to_string(),
                        owner: Owner::ObjectConst(name.to_string()),
                        params: extract_params(params, src),
                        line: line_of(line_starts, span.start),
                        end_line: line_of(line_starts, span.end),
                        exported: exported_ctx,
                        skip,
                        eligible: skip.is_none(),
                    });
                }
            }
            if has_methods { Binding::ObjectConst } else { Binding::Const { ann, lit: None } }
        }
        Some(Expression::NewExpression(n)) if is_const => {
            if let Expression::Identifier(cls) = &n.callee {
                Binding::Instance(cls.name.to_string())
            } else {
                Binding::LocalOther
            }
        }
        Some(init) if is_const => {
            let lit = simple_literal(init);
            if ann.is_some() || lit.is_some() {
                Binding::Const { ann, lit }
            } else {
                Binding::LocalOther
            }
        }
        _ => {
            if ann.is_some() && is_const {
                Binding::Const { ann, lit: None }
            } else {
                Binding::LocalOther
            }
        }
    }
}

/// `import("./x")` or `await import("./x")` with a literal specifier.
fn dynamic_import_specifier(init: Option<&Expression>) -> Option<String> {
    let e = init?;
    let inner = match e {
        Expression::AwaitExpression(a) => &a.argument,
        other => other,
    };
    if let Expression::ImportExpression(imp) = inner {
        if let Expression::StringLiteral(s) = &imp.source {
            return Some(s.value.to_string());
        }
    }
    None
}

fn simple_literal(e: &Expression) -> Option<Observed> {
    match e {
        Expression::StringLiteral(s) => Some(Observed::StrLit(s.value.to_string())),
        Expression::NumericLiteral(n) => Some(Observed::NumLit(canonical_num(n.value))),
        Expression::BooleanLiteral(b) => Some(Observed::BoolLit(b.value)),
        Expression::NullLiteral(_) => Some(Observed::Null),
        _ => None,
    }
}

fn skip_reason_of(has_type_params: bool, params: &FormalParameters) -> Option<SkipReason> {
    if has_type_params {
        return Some(SkipReason::Generic);
    }
    if params.rest.is_some() {
        return Some(SkipReason::RestParam);
    }
    if params.items.is_empty() {
        return Some(SkipReason::NoParams);
    }
    None
}

#[allow(dead_code)]
fn params_eligible(params: &FormalParameters) -> bool {
    // Rest params shift nothing before them but complicate omitted-arg logic;
    // functions carrying one stay skipped. Destructured params are fine: an
    // object pattern analyzes against its annotation, an array pattern simply
    // yields no declared type for that position.
    !(params.rest.is_some() || params.items.is_empty())
}

/// Display name for a parameter position.
fn pattern_display(p: &BindingPattern) -> String {
    match p {
        BindingPattern::BindingIdentifier(id) => id.name.to_string(),
        BindingPattern::ObjectPattern(o) => {
            let mut names: Vec<String> = Vec::new();
            for prop in o.properties.iter().take(3) {
                if let PropertyKey::StaticIdentifier(k) = &prop.key {
                    names.push(k.name.to_string());
                }
            }
            let ellipsis = if o.properties.len() > 3 || o.rest.is_some() { ", …" } else { "" };
            format!("{{{}{}}}", names.join(", "), ellipsis)
        }
        BindingPattern::ArrayPattern(_) => "[…]".to_string(),
        BindingPattern::AssignmentPattern(ap) => pattern_display(&ap.left),
    }
}

fn extract_params(params: &FormalParameters, src: &str) -> Vec<ParamInfo> {
    params
        .items
        .iter()
        .map(|p| {
            let name = pattern_display(&p.pattern);
            // Array patterns have no analyzable declared shape (tuples are
            // opaque in our IR); object patterns and identifiers use the
            // annotation as-is.
            let analyzable = !matches!(p.pattern, BindingPattern::ArrayPattern(_));
            let (ty, ty_text) = match &p.type_annotation {
                Some(t) if analyzable => (
                    Some(ts_type_to_expr(&t.type_annotation, src, 0)),
                    Some(span_text(src, t.type_annotation.span()).to_string()),
                ),
                _ => (None, None),
            };
            ParamInfo {
                name,
                ty,
                ty_text,
                optional: p.optional,
                default: p.initializer.as_ref().map(|e| literal_observed(e, 0).unwrap_or(Observed::Opaque)),
            }
        })
        .collect()
}

/// Scope-free literal extraction, safe for parameter defaults: the default is
/// evaluated fresh at call entry, so a nested object literal is exactly the
/// value observed. (NOT safe for const bindings — a const *object* can be
/// mutated between its declaration and a later call.)
fn literal_observed(e: &Expression, depth: usize) -> Option<Observed> {
    if depth > TYPE_DEPTH_LIMIT {
        return None;
    }
    if let Some(l) = simple_literal(e) {
        return Some(l);
    }
    match e {
        Expression::Identifier(id) if id.name == "undefined" => Some(Observed::Undefined),
        Expression::ObjectExpression(o) => {
            let mut props = Vec::new();
            for p in &o.properties {
                let ObjectPropertyKind::ObjectProperty(prop) = p else { return None };
                if prop.computed {
                    return None;
                }
                let name = match &prop.key {
                    PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                    PropertyKey::StringLiteral(s) => s.value.to_string(),
                    _ => return None,
                };
                props.push((name, literal_observed(&prop.value, depth + 1)?));
            }
            Some(Observed::Object(props))
        }
        _ => None,
    }
}

fn signatures_to_object(members: &[TSSignature], src: &str, depth: usize) -> TypeExpr {
    if depth > TYPE_DEPTH_LIMIT {
        return TypeExpr::Opaque("<deep>".to_string());
    }
    let mut props = Vec::new();
    for m in members {
        match m {
            TSSignature::TSPropertySignature(ps) => {
                if ps.computed {
                    return TypeExpr::Opaque("<interface>".to_string());
                }
                let name = match &ps.key {
                    PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                    PropertyKey::StringLiteral(s) => s.value.to_string(),
                    _ => return TypeExpr::Opaque("<interface>".to_string()),
                };
                let ty = match &ps.type_annotation {
                    Some(t) => ts_type_to_expr(&t.type_annotation, src, depth + 1),
                    None => TypeExpr::Any,
                };
                props.push(ObjProp { name, ty, optional: ps.optional });
            }
            _ => return TypeExpr::Opaque("<interface>".to_string()),
        }
    }
    TypeExpr::ObjectLit(props)
}

pub fn ts_type_to_expr(t: &TSType, src: &str, depth: usize) -> TypeExpr {
    if depth > TYPE_DEPTH_LIMIT {
        return TypeExpr::Opaque("<deep>".to_string());
    }
    match t {
        TSType::TSUnionType(u) => TypeExpr::Union(u.types.iter().map(|x| ts_type_to_expr(x, src, depth + 1)).collect()),
        TSType::TSLiteralType(l) => match &l.literal {
            TSLiteral::StringLiteral(s) => TypeExpr::StrLit(s.value.to_string()),
            TSLiteral::NumericLiteral(n) => TypeExpr::NumLit(canonical_num(n.value)),
            TSLiteral::BooleanLiteral(b) => TypeExpr::BoolLit(b.value),
            _ => TypeExpr::Opaque(span_text(src, l.span).to_string()),
        },
        TSType::TSTypeReference(r) => {
            if r.type_arguments.is_some() {
                return TypeExpr::Opaque(span_text(src, r.span).to_string());
            }
            match &r.type_name {
                TSTypeName::IdentifierReference(id) => TypeExpr::Ref(id.name.to_string()),
                _ => TypeExpr::Opaque(span_text(src, r.span).to_string()),
            }
        }
        TSType::TSTypeLiteral(o) => signatures_to_object(&o.members, src, depth + 1),
        TSType::TSStringKeyword(_) => TypeExpr::Str,
        TSType::TSNumberKeyword(_) => TypeExpr::Num,
        TSType::TSBooleanKeyword(_) => TypeExpr::Bool,
        TSType::TSUndefinedKeyword(_) => TypeExpr::Undefined,
        TSType::TSNullKeyword(_) => TypeExpr::Null,
        TSType::TSAnyKeyword(_) => TypeExpr::Any,
        TSType::TSUnknownKeyword(_) => TypeExpr::Unknown,
        TSType::TSParenthesizedType(p) => ts_type_to_expr(&p.type_annotation, src, depth + 1),
        other => TypeExpr::Opaque(span_text(src, other.span()).to_string()),
    }
}

/// Collect every `Ref` name mentioned in a type expression.
fn collect_refs<'a>(te: &'a TypeExpr, out: &mut Vec<&'a str>) {
    match te {
        TypeExpr::Ref(n) => out.push(n),
        TypeExpr::Union(parts) => parts.iter().for_each(|p| collect_refs(p, out)),
        TypeExpr::ObjectLit(props) => props.iter().for_each(|p| collect_refs(&p.ty, out)),
        TypeExpr::Proj(base, _) => collect_refs(base, out),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Function-body pre-scan: every name *declared anywhere* in a function body
// (at any block depth, excluding nested function bodies) shadows outer scopes
// for the whole function. Registering them up-front as LocalOther means
// references before/inside blocks degrade to "unattributable" (escape/opaque)
// instead of wrongly binding to an outer declaration.
// ---------------------------------------------------------------------------

fn scan_stmts(stmts: &[Statement], values: &mut HashMap<String, Binding>, types: &mut HashSet<String>, depth: usize) {
    if depth > STMT_DEPTH_LIMIT {
        return;
    }
    for stmt in stmts {
        scan_stmt(stmt, values, types, depth);
    }
}

fn scan_stmt(stmt: &Statement, values: &mut HashMap<String, Binding>, types: &mut HashSet<String>, depth: usize) {
    if depth > STMT_DEPTH_LIMIT {
        return;
    }
    match stmt {
        Statement::VariableDeclaration(v) => {
            for d in &v.declarations {
                bind_pattern_names(&d.id, values);
            }
        }
        Statement::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                values.insert(id.name.to_string(), Binding::LocalOther);
            }
        }
        Statement::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                values.insert(id.name.to_string(), Binding::LocalOther);
            }
        }
        Statement::TSTypeAliasDeclaration(t) => {
            types.insert(t.id.name.to_string());
        }
        Statement::TSInterfaceDeclaration(t) => {
            types.insert(t.id.name.to_string());
        }
        Statement::TSEnumDeclaration(t) => {
            types.insert(t.id.name.to_string());
            values.insert(t.id.name.to_string(), Binding::LocalOther);
        }
        Statement::BlockStatement(b) => scan_stmts(&b.body, values, types, depth + 1),
        Statement::IfStatement(s) => {
            scan_stmt(&s.consequent, values, types, depth + 1);
            if let Some(alt) = &s.alternate {
                scan_stmt(alt, values, types, depth + 1);
            }
        }
        Statement::ForStatement(s) => {
            if let Some(ForStatementInit::VariableDeclaration(v)) = &s.init {
                for d in &v.declarations {
                    bind_pattern_names(&d.id, values);
                }
            }
            scan_stmt(&s.body, values, types, depth + 1);
        }
        Statement::ForInStatement(s) => {
            if let ForStatementLeft::VariableDeclaration(v) = &s.left {
                for d in &v.declarations {
                    bind_pattern_names(&d.id, values);
                }
            }
            scan_stmt(&s.body, values, types, depth + 1);
        }
        Statement::ForOfStatement(s) => {
            if let ForStatementLeft::VariableDeclaration(v) = &s.left {
                for d in &v.declarations {
                    bind_pattern_names(&d.id, values);
                }
            }
            scan_stmt(&s.body, values, types, depth + 1);
        }
        Statement::WhileStatement(s) => scan_stmt(&s.body, values, types, depth + 1),
        Statement::DoWhileStatement(s) => scan_stmt(&s.body, values, types, depth + 1),
        Statement::SwitchStatement(s) => {
            for case in &s.cases {
                scan_stmts(&case.consequent, values, types, depth + 1);
            }
        }
        Statement::TryStatement(s) => {
            scan_stmts(&s.block.body, values, types, depth + 1);
            if let Some(h) = &s.handler {
                if let Some(p) = &h.param {
                    bind_pattern_names(&p.pattern, values);
                }
                scan_stmts(&h.body.body, values, types, depth + 1);
            }
            if let Some(f) = &s.finalizer {
                scan_stmts(&f.body, values, types, depth + 1);
            }
        }
        Statement::LabeledStatement(s) => scan_stmt(&s.body, values, types, depth + 1),
        _ => {}
    }
}

/// Bind the names of a destructured object parameter to property projections
/// of `base`. Anything we can't project (computed keys, array patterns) falls
/// back to opaque local bindings.
fn bind_object_projections(o: &ObjectPattern, base: &TypeExpr, scope: &mut HashMap<String, Binding>) {
    for prop in &o.properties {
        let key = if prop.computed {
            None
        } else {
            match &prop.key {
                PropertyKey::StaticIdentifier(k) => Some(k.name.to_string()),
                PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
                _ => None,
            }
        };
        match key {
            Some(key) => {
                let proj = TypeExpr::Proj(Box::new(base.clone()), key);
                bind_pattern_projection(&prop.value, proj, scope);
            }
            None => bind_pattern_names(&prop.value, scope),
        }
    }
    if let Some(rest) = &o.rest {
        bind_pattern_names(&rest.argument, scope);
    }
}

fn bind_pattern_projection(p: &BindingPattern, ty: TypeExpr, scope: &mut HashMap<String, Binding>) {
    match p {
        BindingPattern::BindingIdentifier(id) => {
            scope.insert(id.name.to_string(), Binding::Param { ann: Some(ty) });
        }
        BindingPattern::ObjectPattern(o) => bind_object_projections(o, &ty, scope),
        // A default only removes undefined from the runtime value; the
        // projection (which may include undefined) is wider — safe for
        // observations.
        BindingPattern::AssignmentPattern(ap) => bind_pattern_projection(&ap.left, ty, scope),
        BindingPattern::ArrayPattern(_) => bind_pattern_names(p, scope),
    }
}

fn bind_pattern_names(p: &BindingPattern, scope: &mut HashMap<String, Binding>) {
    match p {
        BindingPattern::BindingIdentifier(id) => {
            scope.insert(id.name.to_string(), Binding::LocalOther);
        }
        BindingPattern::ObjectPattern(o) => {
            for prop in &o.properties {
                bind_pattern_names(&prop.value, scope);
            }
            if let Some(rest) = &o.rest {
                bind_pattern_names(&rest.argument, scope);
            }
        }
        BindingPattern::ArrayPattern(a) => {
            for el in a.elements.iter().flatten() {
                bind_pattern_names(el, scope);
            }
            if let Some(rest) = &a.rest {
                bind_pattern_names(&rest.argument, scope);
            }
        }
        BindingPattern::AssignmentPattern(ap) => bind_pattern_names(&ap.left, scope),
    }
}

// ---------------------------------------------------------------------------
// Usage visitor
// ---------------------------------------------------------------------------

struct UsageVisitor<'s> {
    src: &'s str,
    line_starts: &'s [u32],
    scopes: Vec<HashMap<String, Binding>>,
    /// Locally-declared type names (aliases, interfaces, enums, type params)
    /// per scope: any annotation mentioning one resolves to Opaque, since our
    /// resolver only knows module-level types.
    type_shadows: Vec<HashSet<String>>,
    usages: Vec<Usage>,
    emitted_escapes: HashSet<(u8, String)>,
}

impl UsageVisitor<'_> {
    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn scope_mut(&mut self) -> &mut HashMap<String, Binding> {
        self.scopes.last_mut().unwrap()
    }

    fn type_shadowed(&self, te: &TypeExpr) -> bool {
        let mut refs = Vec::new();
        collect_refs(te, &mut refs);
        refs.iter().any(|n| self.type_shadows.iter().any(|s| s.contains(*n)))
    }

    /// Strip a type annotation that mentions locally-shadowed type names.
    fn safe_ann(&self, ann: Option<TypeExpr>) -> Option<TypeExpr> {
        ann.filter(|t| !self.type_shadowed(t))
    }

    fn push_fn_scope(
        &mut self,
        params: &FormalParameters,
        type_parameters: Option<&TSTypeParameterDeclaration>,
        body_stmts: Option<&[Statement]>,
    ) {
        let mut types = HashSet::new();
        if let Some(tp) = type_parameters {
            for p in &tp.params {
                types.insert(p.name.name.to_string());
            }
        }
        let mut scope = HashMap::new();
        if let Some(stmts) = body_stmts {
            scan_stmts(stmts, &mut scope, &mut types, 0);
        }
        // Parameters are registered after the body pre-scan so a parameter
        // beats a same-named inner declaration until the declarator re-inserts.
        self.type_shadows.push(types);
        for p in &params.items {
            let ann = p.type_annotation.as_ref().map(|t| ts_type_to_expr(&t.type_annotation, self.src, 0));
            let ann = ann.filter(|t| {
                let mut refs = Vec::new();
                collect_refs(t, &mut refs);
                !refs
                    .iter()
                    .any(|n| self.type_shadows.iter().any(|s| s.contains(*n)))
            });
            match (&p.pattern, ann) {
                (BindingPattern::BindingIdentifier(id), ann) => {
                    scope.insert(id.name.to_string(), Binding::Param { ann });
                }
                // Destructured object parameter: each name binds to a property
                // projection of the annotation, so forwarding a prop into
                // another call narrows that callee too.
                (BindingPattern::ObjectPattern(o), Some(base)) => {
                    bind_object_projections(o, &base, &mut scope);
                }
                (other, _) => bind_pattern_names(other, &mut scope),
            }
        }
        self.scopes.push(scope);
    }

    fn pop_fn_scope(&mut self) {
        self.scopes.pop();
        self.type_shadows.pop();
    }

    fn expr_to_observed(&self, e: &Expression, depth: usize) -> Observed {
        if depth > TYPE_DEPTH_LIMIT {
            return Observed::Opaque;
        }
        match e {
            Expression::StringLiteral(s) => Observed::StrLit(s.value.to_string()),
            Expression::NumericLiteral(n) => Observed::NumLit(canonical_num(n.value)),
            Expression::BooleanLiteral(b) => Observed::BoolLit(b.value),
            Expression::NullLiteral(_) => Observed::Null,
            Expression::Identifier(id) => {
                if id.name == "undefined" {
                    return Observed::Undefined;
                }
                match self.lookup(id.name.as_str()) {
                    Some(Binding::Param { ann: Some(t) }) | Some(Binding::Const { ann: Some(t), .. }) => {
                        if self.type_shadowed(t) {
                            Observed::Opaque
                        } else {
                            Observed::Typed(t.clone())
                        }
                    }
                    Some(Binding::Const { ann: None, lit: Some(l) }) => l.clone(),
                    _ => Observed::Opaque,
                }
            }
            Expression::StaticMemberExpression(m) => {
                if let Expression::Identifier(obj) = &m.object {
                    if let Some(Binding::EnumDecl) | Some(Binding::Import) = self.lookup(obj.name.as_str()) {
                        return Observed::EnumMember {
                            enum_name: obj.name.to_string(),
                            member: m.property.name.to_string(),
                        };
                    }
                }
                Observed::Opaque
            }
            Expression::ObjectExpression(o) => {
                let mut props = Vec::new();
                for p in &o.properties {
                    match p {
                        ObjectPropertyKind::ObjectProperty(prop) => {
                            if prop.computed {
                                return Observed::Opaque;
                            }
                            let name = match &prop.key {
                                PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                                PropertyKey::StringLiteral(s) => s.value.to_string(),
                                _ => return Observed::Opaque,
                            };
                            props.push((name, self.expr_to_observed(&prop.value, depth + 1)));
                        }
                        ObjectPropertyKind::SpreadProperty(_) => return Observed::Opaque,
                    }
                }
                Observed::Object(props)
            }
            Expression::TSAsExpression(a) => {
                // `x as const` keeps the expression; anything else takes the asserted type.
                if let TSType::TSTypeReference(r) = &a.type_annotation {
                    if let TSTypeName::IdentifierReference(id) = &r.type_name {
                        if id.name == "const" {
                            return self.expr_to_observed(&a.expression, depth + 1);
                        }
                    }
                }
                let te = ts_type_to_expr(&a.type_annotation, self.src, 0);
                if self.type_shadowed(&te) { Observed::Opaque } else { Observed::Typed(te) }
            }
            Expression::TSSatisfiesExpression(s) => self.expr_to_observed(&s.expression, depth + 1),
            Expression::TSNonNullExpression(n) => self.expr_to_observed(&n.expression, depth + 1),
            Expression::ParenthesizedExpression(p) => self.expr_to_observed(&p.expression, depth + 1),
            _ => Observed::Opaque,
        }
    }

    fn record_call(&mut self, target: UsageTargetRef, call: &CallExpression) {
        let opaque = call.arguments.iter().any(|a| matches!(a, Argument::SpreadElement(_)));
        let args = if opaque {
            CallArgs::Opaque
        } else {
            CallArgs::Args(
                call.arguments
                    .iter()
                    .map(|a| a.as_expression().map(|e| self.expr_to_observed(e, 0)).unwrap_or(Observed::Opaque))
                    .collect(),
            )
        };
        let line = line_of(self.line_starts, call.span.start);
        self.usages.push(Usage { target, kind: UsageKind::Call(args), line });
    }

    fn escape(&mut self, target: UsageTargetRef) {
        // Broad escapes flood real codebases (every property access can emit
        // one); dedupe per module since escaping is idempotent.
        let key = match &target {
            UsageTargetRef::Local { owner, name } => (0u8, format!("{owner:?}|{name}")),
            UsageTargetRef::Imported { local } => (1, local.clone()),
            UsageTargetRef::NamespaceMember { ns_local, name } => (2, format!("{ns_local}|{name}")),
            UsageTargetRef::AnyMethodNamed(n) => (3, n.clone()),
            UsageTargetRef::AnyFreeNamed(n) => (4, n.clone()),
            UsageTargetRef::AllMembersOf(o) => (5, format!("{o:?}")),
            UsageTargetRef::AllExportsOfModule { ns_local } => (6, ns_local.clone()),
        };
        if self.emitted_escapes.insert(key) {
            self.usages.push(Usage { target, kind: UsageKind::Escape, line: 0 });
        }
    }

    /// Escape for an identifier used as a value in an unattributable position.
    fn escape_identifier_use(&mut self, name: &str) {
        match self.lookup(name) {
            Some(Binding::Fn) => self.escape(UsageTargetRef::Local { owner: Owner::Free, name: name.to_string() }),
            Some(Binding::Import) => self.escape(UsageTargetRef::Imported { local: name.to_string() }),
            Some(Binding::Namespace) => self.escape(UsageTargetRef::AllExportsOfModule { ns_local: name.to_string() }),
            Some(Binding::ObjectConst) => self.escape(UsageTargetRef::AllMembersOf(Owner::ObjectConst(name.to_string()))),
            Some(Binding::Instance(cls)) => {
                let cls = cls.clone();
                self.escape(UsageTargetRef::AllMembersOf(Owner::Class(cls)));
            }
            Some(Binding::ClassDecl) => self.escape(UsageTargetRef::AllMembersOf(Owner::Class(name.to_string()))),
            _ => {}
        }
    }
}

impl<'a> Visit<'a> for UsageVisitor<'_> {
    // Type positions never contain calls we care about; skipping them also
    // prevents type references from registering as escapes.
    fn visit_ts_type(&mut self, _it: &TSType<'a>) {}
    fn visit_ts_type_annotation(&mut self, _it: &TSTypeAnnotation<'a>) {}
    fn visit_ts_type_alias_declaration(&mut self, _it: &TSTypeAliasDeclaration<'a>) {}
    fn visit_ts_interface_declaration(&mut self, _it: &TSInterfaceDeclaration<'a>) {}
    fn visit_ts_enum_declaration(&mut self, _it: &TSEnumDeclaration<'a>) {}
    fn visit_import_declaration(&mut self, _it: &ImportDeclaration<'a>) {}
    fn visit_jsx_closing_element(&mut self, _it: &JSXClosingElement<'a>) {}

    fn visit_export_named_declaration(&mut self, it: &ExportNamedDeclaration<'a>) {
        // Export specifiers are neutral references; only walk the declaration.
        if let Some(d) = &it.declaration {
            self.visit_declaration(d);
        }
    }

    fn visit_export_default_declaration(&mut self, it: &ExportDefaultDeclaration<'a>) {
        // `export default someFn` is neutral (an export, not a value escape,
        // under the closed-world assumption).
        if let ExportDefaultDeclarationKind::Identifier(_) = &it.declaration {
            return;
        }
        walk::walk_export_default_declaration(self, it);
    }

    fn visit_ts_as_expression(&mut self, it: &TSAsExpression<'a>) {
        self.visit_expression(&it.expression);
    }

    fn visit_function(&mut self, it: &Function<'a>, _flags: ScopeFlags) {
        let is_decl = it.r#type == FunctionType::FunctionDeclaration;
        // A nested function declaration's name lives in the enclosing scope; a
        // named function expression's name is visible only inside itself.
        if is_decl && self.scopes.len() > 1 {
            if let Some(id) = &it.id {
                self.scope_mut().insert(id.name.to_string(), Binding::LocalOther);
            }
        }
        let stmts = it.body.as_deref().map(|b| &b.statements[..]);
        self.push_fn_scope(&it.params, it.type_parameters.as_deref(), stmts);
        if !is_decl {
            if let Some(id) = &it.id {
                self.scope_mut().insert(id.name.to_string(), Binding::LocalOther);
            }
        }
        for p in &it.params.items {
            if let Some(init) = &p.initializer {
                self.visit_expression(init);
            }
        }
        if let Some(b) = &it.body {
            self.visit_function_body(b);
        }
        self.pop_fn_scope();
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        self.push_fn_scope(&it.params, it.type_parameters.as_deref(), Some(&it.body.statements[..]));
        for p in &it.params.items {
            if let Some(init) = &p.initializer {
                self.visit_expression(init);
            }
        }
        self.visit_function_body(&it.body);
        self.pop_fn_scope();
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if let BindingPattern::BindingIdentifier(id) = &it.id {
            // Top level was pre-registered (module scope); locals get classified
            // here so shadowing works. `info: None` — locals never create targets.
            if self.scopes.len() > 1 {
                let is_const = it.kind == VariableDeclarationKind::Const;
                let b = classify_declarator(it, is_const, id.name.as_str(), false, self.src, &[0], None);
                // Strip annotations that reference locally-shadowed type names.
                let b = match b {
                    Binding::Const { ann, lit } => Binding::Const { ann: self.safe_ann(ann), lit },
                    other => other,
                };
                self.scope_mut().insert(id.name.to_string(), b);
            }
        } else {
            let mut names = HashMap::new();
            bind_pattern_names(&it.id, &mut names);
            self.scope_mut().extend(names);
        }
        if let Some(init) = &it.init {
            self.visit_expression(init);
        }
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        match &it.callee {
            Expression::Identifier(id) => match self.lookup(id.name.as_str()) {
                Some(Binding::Fn) => self.record_call(UsageTargetRef::Local { owner: Owner::Free, name: id.name.to_string() }, it),
                Some(Binding::Import) => self.record_call(UsageTargetRef::Imported { local: id.name.to_string() }, it),
                Some(Binding::Namespace) => {
                    // Calling the namespace object itself: not a member call we
                    // model; escape everything it exports.
                    self.escape(UsageTargetRef::AllExportsOfModule { ns_local: id.name.to_string() });
                }
                _ => {
                    // Unattributable identifier call (shadowed, unbound, or a
                    // plain value): could reach any same-named free function.
                    self.escape(UsageTargetRef::AnyFreeNamed(id.name.to_string()));
                }
            },
            Expression::StaticMemberExpression(m) => {
                let prop = m.property.name.to_string();
                if let Expression::Identifier(obj) = &m.object {
                    match self.lookup(obj.name.as_str()) {
                        Some(Binding::ObjectConst) => {
                            let owner = Owner::ObjectConst(obj.name.to_string());
                            self.record_call(UsageTargetRef::Local { owner, name: prop }, it);
                        }
                        Some(Binding::Instance(cls)) => {
                            let owner = Owner::Class(cls.clone());
                            self.record_call(UsageTargetRef::Local { owner, name: prop }, it);
                        }
                        Some(Binding::Namespace) => {
                            self.record_call(
                                UsageTargetRef::NamespaceMember { ns_local: obj.name.to_string(), name: prop },
                                it,
                            );
                        }
                        Some(Binding::EnumDecl) => {}
                        Some(Binding::Fn) | Some(Binding::Import) => {
                            // Function used as an object (`send.call(...)` etc.).
                            self.escape_identifier_use(obj.name.as_str());
                            self.escape(UsageTargetRef::AnyMethodNamed(prop));
                        }
                        _ => {
                            // Unattributable method call: could hit any method
                            // with this name, or a free function reached via an
                            // untracked namespace-like object (dynamic import,
                            // required module, …).
                            self.escape(UsageTargetRef::AnyMethodNamed(prop.clone()));
                            self.escape(UsageTargetRef::AnyFreeNamed(prop));
                        }
                    }
                } else {
                    self.escape(UsageTargetRef::AnyMethodNamed(prop.clone()));
                    self.escape(UsageTargetRef::AnyFreeNamed(prop));
                    self.visit_expression(&m.object);
                }
            }
            other => self.visit_expression(other),
        }
        for arg in &it.arguments {
            match arg {
                Argument::SpreadElement(s) => self.visit_expression(&s.argument),
                _ => {
                    if let Some(e) = arg.as_expression() {
                        self.visit_expression(e);
                    }
                }
            }
        }
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        // The class identifier in `new Cls()` is neutral.
        if !matches!(&it.callee, Expression::Identifier(_)) {
            self.visit_expression(&it.callee);
        }
        for arg in &it.arguments {
            match arg {
                Argument::SpreadElement(s) => self.visit_expression(&s.argument),
                _ => {
                    if let Some(e) = arg.as_expression() {
                        self.visit_expression(e);
                    }
                }
            }
        }
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        // Non-call member access (call positions are intercepted above).
        let prop = it.property.name.to_string();
        if let Expression::Identifier(obj) = &it.object {
            match self.lookup(obj.name.as_str()) {
                Some(Binding::Fn) | Some(Binding::Import) | Some(Binding::Namespace) => {
                    self.escape_identifier_use(obj.name.as_str());
                }
                Some(Binding::ObjectConst) => {
                    self.escape(UsageTargetRef::Local { owner: Owner::ObjectConst(obj.name.to_string()), name: prop });
                }
                Some(Binding::Instance(cls)) => {
                    let cls = cls.clone();
                    self.escape(UsageTargetRef::Local { owner: Owner::Class(cls), name: prop });
                }
                Some(Binding::EnumDecl) => {}
                _ => {
                    self.escape(UsageTargetRef::AnyMethodNamed(prop.clone()));
                    self.escape(UsageTargetRef::AnyFreeNamed(prop));
                }
            }
        } else {
            self.escape(UsageTargetRef::AnyMethodNamed(prop.clone()));
            self.escape(UsageTargetRef::AnyFreeNamed(prop));
            self.visit_expression(&it.object);
        }
    }

    fn visit_jsx_opening_element(&mut self, it: &JSXOpeningElement<'a>) {
        match &it.name {
            JSXElementName::IdentifierReference(id) => {
                let target = match self.lookup(id.name.as_str()) {
                    Some(Binding::Fn) => Some(UsageTargetRef::Local { owner: Owner::Free, name: id.name.to_string() }),
                    Some(Binding::Import) => Some(UsageTargetRef::Imported { local: id.name.to_string() }),
                    Some(Binding::ClassDecl) => {
                        // Class components are unmodeled; props flow somewhere
                        // we don't track.
                        self.escape(UsageTargetRef::AllMembersOf(Owner::Class(id.name.to_string())));
                        None
                    }
                    _ => None,
                };
                if let Some(target) = target {
                    let mut opaque = false;
                    let mut props: Vec<(String, Observed)> = Vec::new();
                    for attr in &it.attributes {
                        match attr {
                            JSXAttributeItem::SpreadAttribute(_) => opaque = true,
                            JSXAttributeItem::Attribute(a) => {
                                let JSXAttributeName::Identifier(name) = &a.name else {
                                    opaque = true;
                                    continue;
                                };
                                let obs = match &a.value {
                                    None => Observed::BoolLit(true),
                                    Some(JSXAttributeValue::StringLiteral(s)) => Observed::StrLit(s.value.to_string()),
                                    Some(JSXAttributeValue::ExpressionContainer(c)) => match c.expression.as_expression() {
                                        Some(e) => self.expr_to_observed(e, 0),
                                        None => Observed::Opaque,
                                    },
                                    Some(_) => Observed::Opaque,
                                };
                                props.push((name.name.to_string(), obs));
                            }
                        }
                    }
                    let args = if opaque { CallArgs::Opaque } else { CallArgs::Args(vec![Observed::Object(props)]) };
                    let line = line_of(self.line_starts, it.span.start);
                    self.usages.push(Usage { target, kind: UsageKind::Call(args), line });
                }
            }
            JSXElementName::MemberExpression(m) => {
                // <UI.Badge .../>: unattributable component reference — escape
                // by member name, and escape the base object if we track it.
                self.escape(UsageTargetRef::AnyMethodNamed(m.property.name.to_string()));
                let mut obj = &m.object;
                loop {
                    match obj {
                        JSXMemberExpressionObject::IdentifierReference(id) => {
                            let name = id.name.to_string();
                            self.escape_identifier_use(&name);
                            break;
                        }
                        JSXMemberExpressionObject::MemberExpression(inner) => {
                            self.escape(UsageTargetRef::AnyMethodNamed(inner.property.name.to_string()));
                            obj = &inner.object;
                        }
                        JSXMemberExpressionObject::ThisExpression(_) => break,
                    }
                }
            }
            _ => {}
        }
        // Walk attribute values for nested usages; the tag name is consumed.
        for attr in &it.attributes {
            match attr {
                JSXAttributeItem::Attribute(a) => {
                    if let Some(JSXAttributeValue::ExpressionContainer(c)) = &a.value {
                        if let Some(e) = c.expression.as_expression() {
                            self.visit_expression(e);
                        }
                    }
                }
                JSXAttributeItem::SpreadAttribute(s) => self.visit_expression(&s.argument),
            }
        }
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        // Any identifier that reaches the default visitor is in a non-call,
        // non-neutral position.
        self.escape_identifier_use(it.name.as_str());
    }
}
