//! Per-file extraction: parse with oxc, convert everything the analysis needs
//! into the owned IR in `model`, drop the AST. Runs in parallel across files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_ast_visit::{walk, Visit};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use oxc_syntax::scope::ScopeFlags;

use crate::model::*;

/// What a name in scope refers to, for conservative resolution.
#[derive(Debug, Clone)]
enum Binding {
    /// Top-level function or const-bound arrow/function expression (a call target).
    Fn,
    /// Imported binding; resolved through the module's import map at link time.
    Import,
    Const { ann: Option<TypeExpr>, lit: Option<Observed> },
    Param { ann: Option<TypeExpr> },
    /// `const obj = { m() {} }` — methods are call targets.
    ObjectConst,
    /// `const x = new Cls()`.
    Instance(String),
    ClassDecl,
    EnumDecl,
    /// Anything else local (let, nested function, catch var, …): shadows outer
    /// names, resolves to nothing analyzable.
    LocalOther,
}

pub fn extract_module(path: &Path, rel: String, source: &str) -> ModuleInfo {
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
        prepass_stmt(stmt, false, source, path, &line_starts, &mut info, &mut module_scope, &mut exported, &mut bodyless_fns);
    }

    // Overloads / merged declarations: any name with a bodyless signature or a
    // duplicate implementation is ineligible.
    let mut seen: HashMap<(Owner, String), usize> = HashMap::new();
    for d in &info.decls {
        *seen.entry((d.owner.clone(), d.name.clone())).or_default() += 1;
    }
    for d in &mut info.decls {
        if seen[&(d.owner.clone(), d.name.clone())] > 1 {
            d.eligible = false;
        }
        if d.owner == Owner::Free && bodyless_fns.contains(&d.name) {
            d.eligible = false;
        }
        if exported.contains(&d.name) && d.owner == Owner::Free {
            d.exported = true;
        }
    }

    // ---- Usage pass. ----
    let mut v = UsageVisitor {
        src: source,
        scopes: vec![module_scope],
        usages: Vec::new(),
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

#[allow(clippy::too_many_arguments)]
fn prepass_stmt(
    stmt: &Statement,
    exported_ctx: bool,
    src: &str,
    path: &Path,
    line_starts: &[u32],
    info: &mut ModuleInfo,
    scope: &mut HashMap<String, Binding>,
    exported: &mut HashSet<String>,
    bodyless_fns: &mut HashSet<String>,
) {
    match stmt {
        Statement::ImportDeclaration(d) => {
            let Some(specs) = &d.specifiers else { return };
            let source_path = resolve_import(path, d.source.value.as_str());
            for s in specs {
                match s {
                    ImportDeclarationSpecifier::ImportSpecifier(is) => {
                        let imported = match &is.imported {
                            ModuleExportName::IdentifierName(n) => n.name.to_string(),
                            ModuleExportName::IdentifierReference(n) => n.name.to_string(),
                            ModuleExportName::StringLiteral(sl) => sl.value.to_string(),
                        };
                        let local = is.local.name.to_string();
                        if let Some(sp) = &source_path {
                            info.imports.insert(local.clone(), (sp.clone(), imported));
                            scope.insert(local, Binding::Import);
                        } else {
                            scope.insert(local, Binding::LocalOther);
                        }
                    }
                    _ => {
                        // Default / namespace imports: unsupported, shadow only.
                        let name = match s {
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(x) => x.local.name.to_string(),
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(x) => x.local.name.to_string(),
                            _ => unreachable!(),
                        };
                        scope.insert(name, Binding::LocalOther);
                    }
                }
            }
        }
        Statement::ExportNamedDeclaration(d) => {
            for s in &d.specifiers {
                if let ModuleExportName::IdentifierReference(n) = &s.local {
                    exported.insert(n.name.to_string());
                }
            }
            if let Some(decl) = &d.declaration {
                prepass_declaration(decl, true, src, path, line_starts, info, scope, exported, bodyless_fns);
            }
        }
        Statement::ExportDefaultDeclaration(d) => {
            match &d.declaration {
                ExportDefaultDeclarationKind::Identifier(id) => {
                    exported.insert(id.name.to_string());
                }
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    prepass_function(f, true, src, line_starts, info, scope, bodyless_fns);
                }
                _ => {}
            }
        }
        Statement::VariableDeclaration(v) => {
            prepass_var(v, exported_ctx, src, line_starts, info, scope);
        }
        Statement::FunctionDeclaration(f) => {
            prepass_function(f, exported_ctx, src, line_starts, info, scope, bodyless_fns);
        }
        Statement::ClassDeclaration(c) => {
            prepass_class(c, exported_ctx, src, line_starts, info, scope);
        }
        Statement::TSTypeAliasDeclaration(t) => {
            let expr = if t.type_parameters.is_some() {
                TypeExpr::Opaque(span_text(src, t.span).to_string())
            } else {
                ts_type_to_expr(&t.type_annotation, src)
            };
            info.type_aliases.insert(t.id.name.to_string(), expr);
        }
        Statement::TSInterfaceDeclaration(t) => {
            let expr = if t.type_parameters.is_some() || !t.extends.is_empty() {
                TypeExpr::Opaque(span_text(src, t.span).to_string())
            } else {
                signatures_to_object(&t.body.body, src)
            };
            info.type_aliases.insert(t.id.name.to_string(), expr);
        }
        Statement::TSEnumDeclaration(t) => {
            let members = t
                .body
                .members
                .iter()
                .filter_map(|m| match &m.id {
                    TSEnumMemberName::Identifier(n) => Some(n.name.to_string()),
                    TSEnumMemberName::String(s) => Some(s.value.to_string()),
                    _ => None,
                })
                .collect();
            info.enums.insert(t.id.name.to_string(), members);
            scope.insert(t.id.name.to_string(), Binding::EnumDecl);
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn prepass_declaration(
    decl: &Declaration,
    exported_ctx: bool,
    src: &str,
    path: &Path,
    line_starts: &[u32],
    info: &mut ModuleInfo,
    scope: &mut HashMap<String, Binding>,
    exported: &mut HashSet<String>,
    bodyless_fns: &mut HashSet<String>,
) {
    // Reuse the statement handler through a small shim: only declaration kinds
    // that can appear under `export` matter here.
    match decl {
        Declaration::VariableDeclaration(v) => prepass_var(v, exported_ctx, src, line_starts, info, scope),
        Declaration::FunctionDeclaration(f) => prepass_function(f, exported_ctx, src, line_starts, info, scope, bodyless_fns),
        Declaration::ClassDeclaration(c) => prepass_class(c, exported_ctx, src, line_starts, info, scope),
        Declaration::TSTypeAliasDeclaration(t) => {
            let expr = if t.type_parameters.is_some() {
                TypeExpr::Opaque(span_text(src, t.span).to_string())
            } else {
                ts_type_to_expr(&t.type_annotation, src)
            };
            info.type_aliases.insert(t.id.name.to_string(), expr);
        }
        Declaration::TSInterfaceDeclaration(t) => {
            let expr = if t.type_parameters.is_some() || !t.extends.is_empty() {
                TypeExpr::Opaque(span_text(src, t.span).to_string())
            } else {
                signatures_to_object(&t.body.body, src)
            };
            info.type_aliases.insert(t.id.name.to_string(), expr);
        }
        Declaration::TSEnumDeclaration(t) => {
            let members = t
                .body
                .members
                .iter()
                .filter_map(|m| match &m.id {
                    TSEnumMemberName::Identifier(n) => Some(n.name.to_string()),
                    TSEnumMemberName::String(s) => Some(s.value.to_string()),
                    _ => None,
                })
                .collect();
            info.enums.insert(t.id.name.to_string(), members);
            scope.insert(t.id.name.to_string(), Binding::EnumDecl);
        }
        _ => {}
    }
    let _ = (exported, path);
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
        // Overload signature or `declare function`.
        bodyless_fns.insert(name);
        return;
    }
    let eligible = f.type_parameters.is_none() && params_eligible(&f.params);
    info.decls.push(FnDecl {
        name,
        owner: Owner::Free,
        params: extract_params(&f.params, src),
        line: line_of(line_starts, f.span.start),
        end_line: line_of(line_starts, f.span.end),
        exported: exported_ctx,
        eligible,
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
            let eligible = func.type_parameters.is_none() && params_eligible(&func.params);
            info.decls.push(FnDecl {
                name: key.name.to_string(),
                owner: Owner::Class(class_name.clone()),
                params: extract_params(&func.params, src),
                line: line_of(line_starts, m.span.start),
                end_line: line_of(line_starts, m.span.end),
                exported: exported_ctx,
                eligible,
            });
        }
    }
}

fn prepass_var(
    v: &VariableDeclaration,
    exported_ctx: bool,
    src: &str,
    line_starts: &[u32],
    info: &mut ModuleInfo,
    scope: &mut HashMap<String, Binding>,
) {
    let is_const = v.kind == VariableDeclarationKind::Const;
    for d in &v.declarations {
        let BindingPattern::BindingIdentifier(id) = &d.id else { continue };
        let name = id.name.to_string();
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
    let ann = d.type_annotation.as_ref().map(|t| ts_type_to_expr(&t.type_annotation, src));
    match &d.init {
        Some(Expression::ArrowFunctionExpression(a)) => {
            if let Some(info) = info {
                let eligible = a.type_parameters.is_none() && params_eligible(&a.params);
                info.decls.push(FnDecl {
                    name: name.to_string(),
                    owner: Owner::Free,
                    params: extract_params(&a.params, src),
                    line: line_of(line_starts, a.span.start),
                    end_line: line_of(line_starts, a.span.end),
                    exported: exported_ctx,
                    eligible,
                });
                Binding::Fn
            } else {
                Binding::LocalOther
            }
        }
        Some(Expression::FunctionExpression(f)) => {
            if let Some(info) = info {
                let eligible = f.type_parameters.is_none() && params_eligible(&f.params) && f.body.is_some();
                info.decls.push(FnDecl {
                    name: name.to_string(),
                    owner: Owner::Free,
                    params: extract_params(&f.params, src),
                    line: line_of(line_starts, f.span.start),
                    end_line: line_of(line_starts, f.span.end),
                    exported: exported_ctx,
                    eligible,
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
                    let func = match &prop.value {
                        Expression::FunctionExpression(f) => {
                            if f.body.is_none() {
                                continue;
                            }
                            has_methods = true;
                            let eligible = f.type_parameters.is_none() && params_eligible(&f.params);
                            info.decls.push(FnDecl {
                                name: key.name.to_string(),
                                owner: Owner::ObjectConst(name.to_string()),
                                params: extract_params(&f.params, src),
                                line: line_of(line_starts, prop.span.start),
                                end_line: line_of(line_starts, prop.span.end),
                                exported: exported_ctx,
                                eligible,
                            });
                            continue;
                        }
                        Expression::ArrowFunctionExpression(a) => Some(a),
                        _ => None,
                    };
                    if let Some(a) = func {
                        has_methods = true;
                        let eligible = a.type_parameters.is_none() && params_eligible(&a.params);
                        info.decls.push(FnDecl {
                            name: key.name.to_string(),
                            owner: Owner::ObjectConst(name.to_string()),
                            params: extract_params(&a.params, src),
                            line: line_of(line_starts, prop.span.start),
                            end_line: line_of(line_starts, prop.span.end),
                            exported: exported_ctx,
                            eligible,
                        });
                    }
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

fn simple_literal(e: &Expression) -> Option<Observed> {
    match e {
        Expression::StringLiteral(s) => Some(Observed::StrLit(s.value.to_string())),
        Expression::NumericLiteral(n) => Some(Observed::NumLit(canonical_num(n.value))),
        Expression::BooleanLiteral(b) => Some(Observed::BoolLit(b.value)),
        Expression::NullLiteral(_) => Some(Observed::Null),
        _ => None,
    }
}

fn params_eligible(params: &FormalParameters) -> bool {
    if params.rest.is_some() || params.items.is_empty() {
        return false;
    }
    params.items.iter().all(|p| matches!(p.pattern, BindingPattern::BindingIdentifier(_)))
}

fn extract_params(params: &FormalParameters, src: &str) -> Vec<ParamInfo> {
    params
        .items
        .iter()
        .map(|p| {
            let name = match &p.pattern {
                BindingPattern::BindingIdentifier(id) => id.name.to_string(),
                _ => "<pattern>".to_string(),
            };
            let (ty, ty_text) = match &p.type_annotation {
                Some(t) => (
                    Some(ts_type_to_expr(&t.type_annotation, src)),
                    Some(span_text(src, t.type_annotation.span()).to_string()),
                ),
                None => (None, None),
            };
            ParamInfo {
                name,
                ty,
                ty_text,
                optional: p.optional,
                default: p.initializer.as_ref().map(|e| simple_literal(e).unwrap_or(Observed::Opaque)),
            }
        })
        .collect()
}

fn signatures_to_object(members: &[TSSignature], src: &str) -> TypeExpr {
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
                    Some(t) => ts_type_to_expr(&t.type_annotation, src),
                    None => TypeExpr::Any,
                };
                props.push(ObjProp { name, ty, optional: ps.optional });
            }
            _ => return TypeExpr::Opaque("<interface>".to_string()),
        }
    }
    TypeExpr::ObjectLit(props)
}

pub fn ts_type_to_expr(t: &TSType, src: &str) -> TypeExpr {
    match t {
        TSType::TSUnionType(u) => TypeExpr::Union(u.types.iter().map(|x| ts_type_to_expr(x, src)).collect()),
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
        TSType::TSTypeLiteral(o) => signatures_to_object(&o.members, src),
        TSType::TSStringKeyword(_) => TypeExpr::Str,
        TSType::TSNumberKeyword(_) => TypeExpr::Num,
        TSType::TSBooleanKeyword(_) => TypeExpr::Bool,
        TSType::TSUndefinedKeyword(_) => TypeExpr::Undefined,
        TSType::TSNullKeyword(_) => TypeExpr::Null,
        TSType::TSAnyKeyword(_) => TypeExpr::Any,
        TSType::TSUnknownKeyword(_) => TypeExpr::Unknown,
        TSType::TSParenthesizedType(p) => ts_type_to_expr(&p.type_annotation, src),
        other => TypeExpr::Opaque(span_text(src, other.span()).to_string()),
    }
}

fn resolve_import(from: &Path, spec: &str) -> Option<PathBuf> {
    if !spec.starts_with('.') {
        return None;
    }
    let dir = from.parent()?;
    let base = normalize(&dir.join(spec));
    let candidates = [
        base.with_extension("ts"),
        base.with_extension("tsx"),
        base.clone(),
        base.join("index.ts"),
        base.join("index.tsx"),
    ];
    for c in &candidates {
        if c.is_file() {
            return Some(c.clone());
        }
    }
    Some(base.with_extension("ts"))
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

// ---------------------------------------------------------------------------
// Usage visitor
// ---------------------------------------------------------------------------

struct UsageVisitor<'s> {
    src: &'s str,
    scopes: Vec<HashMap<String, Binding>>,
    usages: Vec<Usage>,
}

impl UsageVisitor<'_> {
    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn scope_mut(&mut self) -> &mut HashMap<String, Binding> {
        self.scopes.last_mut().unwrap()
    }

    fn push_fn_scope(&mut self, params: &FormalParameters) {
        let mut scope = HashMap::new();
        for p in &params.items {
            if let BindingPattern::BindingIdentifier(id) = &p.pattern {
                let ann = p.type_annotation.as_ref().map(|t| ts_type_to_expr(&t.type_annotation, self.src));
                scope.insert(id.name.to_string(), Binding::Param { ann });
            } else {
                bind_pattern_names(&p.pattern, &mut scope);
            }
        }
        self.scopes.push(scope);
    }

    fn expr_to_observed(&self, e: &Expression) -> Observed {
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
                        Observed::Typed(t.clone())
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
                            props.push((name, self.expr_to_observed(&prop.value)));
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
                            return self.expr_to_observed(&a.expression);
                        }
                    }
                }
                Observed::Typed(ts_type_to_expr(&a.type_annotation, self.src))
            }
            Expression::TSSatisfiesExpression(s) => self.expr_to_observed(&s.expression),
            Expression::TSNonNullExpression(n) => self.expr_to_observed(&n.expression),
            Expression::ParenthesizedExpression(p) => self.expr_to_observed(&p.expression),
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
                    .map(|a| a.as_expression().map(|e| self.expr_to_observed(e)).unwrap_or(Observed::Opaque))
                    .collect(),
            )
        };
        self.usages.push(Usage { target, kind: UsageKind::Call(args) });
    }

    fn escape(&mut self, target: UsageTargetRef) {
        self.usages.push(Usage { target, kind: UsageKind::Escape });
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
        // A nested (non-top-level) function declaration shadows its name in the
        // enclosing scope. Top-level ones are already registered as Fn.
        if let Some(id) = &it.id {
            if self.scopes.len() > 1 {
                self.scope_mut().insert(id.name.to_string(), Binding::LocalOther);
            }
        }
        self.push_fn_scope(&it.params);
        for p in &it.params.items {
            if let Some(init) = &p.initializer {
                self.visit_expression(init);
            }
        }
        if let Some(b) = &it.body {
            self.visit_function_body(b);
        }
        self.scopes.pop();
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        self.push_fn_scope(&it.params);
        for p in &it.params.items {
            if let Some(init) = &p.initializer {
                self.visit_expression(init);
            }
        }
        self.visit_function_body(&it.body);
        self.scopes.pop();
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if let BindingPattern::BindingIdentifier(id) = &it.id {
            // Top level was pre-registered (module scope); locals get classified
            // here so shadowing works. `info: None` — locals never create targets.
            if self.scopes.len() > 1 {
                let is_const = it.kind == VariableDeclarationKind::Const;
                let b = classify_declarator(it, is_const, id.name.as_str(), false, self.src, &[0], None);
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
                Some(Binding::Import) => self.record_call(UsageTargetRef::Imported { local: id.name.to_string(), name: String::new() }, it),
                _ => {}
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
                        Some(Binding::EnumDecl) => {}
                        Some(Binding::Fn) | Some(Binding::Import) => {
                            // Function used as an object (`send.call(...)` etc.).
                            self.escape(UsageTargetRef::Local { owner: Owner::Free, name: obj.name.to_string() });
                            self.escape(UsageTargetRef::AnyMethodNamed(prop));
                        }
                        _ => {
                            // Unattributable method call: every method with this
                            // name must be considered escaped.
                            self.escape(UsageTargetRef::AnyMethodNamed(prop));
                        }
                    }
                } else {
                    self.escape(UsageTargetRef::AnyMethodNamed(prop));
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
                Some(Binding::Fn) => self.escape(UsageTargetRef::Local { owner: Owner::Free, name: obj.name.to_string() }),
                Some(Binding::Import) => self.escape(UsageTargetRef::Imported { local: obj.name.to_string(), name: String::new() }),
                Some(Binding::ObjectConst) => {
                    self.escape(UsageTargetRef::Local { owner: Owner::ObjectConst(obj.name.to_string()), name: prop });
                }
                Some(Binding::Instance(cls)) => {
                    self.escape(UsageTargetRef::Local { owner: Owner::Class(cls.clone()), name: prop });
                }
                Some(Binding::EnumDecl) => {}
                _ => self.escape(UsageTargetRef::AnyMethodNamed(prop)),
            }
        } else {
            self.escape(UsageTargetRef::AnyMethodNamed(prop));
            self.visit_expression(&it.object);
        }
    }

    fn visit_jsx_opening_element(&mut self, it: &JSXOpeningElement<'a>) {
        if let JSXElementName::IdentifierReference(id) = &it.name {
            let target = match self.lookup(id.name.as_str()) {
                Some(Binding::Fn) => Some(UsageTargetRef::Local { owner: Owner::Free, name: id.name.to_string() }),
                Some(Binding::Import) => Some(UsageTargetRef::Imported { local: id.name.to_string(), name: String::new() }),
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
                                    Some(e) => self.expr_to_observed(e),
                                    None => Observed::Opaque,
                                },
                                Some(_) => Observed::Opaque,
                            };
                            props.push((name.name.to_string(), obs));
                        }
                    }
                }
                let args = if opaque { CallArgs::Opaque } else { CallArgs::Args(vec![Observed::Object(props)]) };
                self.usages.push(Usage { target, kind: UsageKind::Call(args) });
            }
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
        match self.lookup(it.name.as_str()) {
            Some(Binding::Fn) => self.escape(UsageTargetRef::Local { owner: Owner::Free, name: it.name.to_string() }),
            Some(Binding::Import) => self.escape(UsageTargetRef::Imported { local: it.name.to_string(), name: String::new() }),
            Some(Binding::ObjectConst) => self.escape(UsageTargetRef::AllMembersOf(Owner::ObjectConst(it.name.to_string()))),
            Some(Binding::Instance(cls)) => self.escape(UsageTargetRef::AllMembersOf(Owner::Class(cls.clone()))),
            Some(Binding::ClassDecl) => self.escape(UsageTargetRef::AllMembersOf(Owner::Class(it.name.to_string()))),
            _ => {}
        }
    }
}
