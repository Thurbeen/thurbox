//! Fleet's architecture rules, checked from source.
//!
//! The rules live in `crates/architecture.toml`: each crate's role, the
//! workspace crates it may depend on, the external crates a pure crate may
//! use, the paths a pure crate may not reach, which crates may name each
//! status-writing field manager, and which methods the CLI may not call.
//!
//! The checker reads two things and compares them with the rules:
//!
//! - **Manifests.** Every `[dependencies]`, `[build-dependencies]` and
//!   `[dev-dependencies]` table, target-specific ones included, with
//!   `package =` renames resolved. The declared graph and the actual graph
//!   must both be acyclic, and every actual edge must be declared.
//! - **Sources.** Every `src/**/*.rs` file, parsed with `syn`. Each path in
//!   the code is resolved the way the compiler would start to: through `use`
//!   aliases and renames, `extern crate .. as ..`, `crate::`/`self::`/`super::`,
//!   glob imports, and re-exports (`pub use`) inside this crate and across
//!   workspace crates, to a fixpoint. The resolved path is what is judged, so
//!   `use std::fs as f; f::read(..)` and a `pub use` chain that reaches a
//!   forbidden crate are both caught.
//!
//! What static resolution cannot see, stated plainly:
//!
//! - Code produced by macros: a `macro_rules!` body or a proc macro's output
//!   is not expanded. A macro's own path is resolved (`env!`, `println!`), and
//!   arguments that parse as comma-separated expressions are judged
//!   (`format!`, `vec![a, b]`, `assert!`); any other macro input is not.
//! - Method calls have no types here, so the CLI's forbidden methods are
//!   matched by name, on every receiver, and as paths whose segment before
//!   the method is a type or trait (`Store::update_status(..)`). The
//!   qualified-self form `<FileStore>::update_status(..)` is not caught.
//! - Glob imports are resolved conservatively: a name that could come from a
//!   forbidden glob is judged as if it did.
//! - `cfg` is ignored: code behind any `cfg` is judged too.
//! - Tests and examples (`tests/`, `benches/`, `#[cfg(test)]` modules inside
//!   `src/` excepted) are not under the source rules; their manifests are.
//!
//! The compiler and clippy cover parts of the same ground at build time: an
//! undeclared crate does not compile (the `compile_fail` probes in the pure
//! crates prove it for each forbidden edge), and each pure crate's
//! `clippy.toml` disallows the I/O types and functions on resolved paths.
//! This checker is what proves the rules hold across re-exports and is what
//! the negative fixtures exercise.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use syn::visit::{self, Visit};
use syn::{Item, UseTree};

/// The rules file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub crates: BTreeMap<String, CrateRule>,
    pub pure: PureRules,
    /// Field-manager path -> the crates whose sources may name it.
    #[serde(default)]
    pub status_writers: BTreeMap<String, Vec<String>>,
    /// Paths no crate may reach except the ones listed, whatever its role.
    #[serde(default)]
    pub restricted: Vec<Restricted>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Restricted {
    pub path: String,
    #[serde(default)]
    pub allowed: Vec<String>,
    pub why: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrateRule {
    pub role: Role,
    /// Workspace crates this crate may depend on (normal and build).
    pub deps: Vec<String>,
    /// Workspace crates allowed as dev-dependencies only.
    #[serde(default)]
    pub dev_deps: Vec<String>,
    /// For pure crates: the external crates allowed.
    #[serde(default)]
    pub external: Vec<String>,
    /// Methods this crate's sources may not call, by name.
    #[serde(default)]
    pub forbidden_methods: Vec<String>,
    /// Workspace crates this crate's `compile_fail` doc probes must name.
    #[serde(default)]
    pub probes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Pure,
    Adapter,
    Orchestration,
    Edge,
    Tooling,
    /// Another component of the repository (thurbox, a runtime): held to
    /// the crate graph from its manifest, since components meet only through
    /// processes, and never to Fleet's own source rules.
    Component,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PureRules {
    /// Path prefixes a pure crate may not reach, e.g. `std::fs`.
    pub forbidden_paths: Vec<String>,
    /// Types and functions each pure crate's clippy.toml must disallow.
    #[serde(default)]
    pub clippy_guards: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    pub code: &'static str,
    pub krate: String,
    pub location: String,
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} at {}: {}",
            self.code, self.krate, self.location, self.detail
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Build,
    Dev,
}

#[derive(Debug, Clone)]
pub struct Dep {
    /// The package depended on.
    pub package: String,
    /// The name code uses for it (the dependency key, `-` as `_`).
    pub alias: String,
    pub kind: DepKind,
}

#[derive(Debug, Clone)]
pub struct CrateInfo {
    pub name: String,
    pub lib: String,
    pub dir: PathBuf,
    pub deps: Vec<Dep>,
}

/// Every member crate of a workspace, with its manifest dependencies.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub crates: Vec<CrateInfo>,
}

fn lib_name(package: &str) -> String {
    package.replace('-', "_")
}

fn read_toml(path: &Path) -> Result<toml::Table, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.parse::<toml::Table>()
        .map_err(|e| format!("{}: {e}", path.display()))
}

impl Workspace {
    /// Load the workspace at `root`. `manifest` is `Cargo.toml` for the real
    /// workspace; fixtures use another name so cargo never sees them.
    pub fn load(root: &Path, manifest: &str) -> Result<Workspace, String> {
        let top = read_toml(&root.join(manifest))?;
        let ws = top
            .get("workspace")
            .and_then(toml::Value::as_table)
            .ok_or("no [workspace] table")?;
        let members = ws
            .get("members")
            .and_then(toml::Value::as_array)
            .ok_or("no workspace.members")?;
        let mut dirs = Vec::new();
        for m in members {
            let m = m.as_str().ok_or("a member is not a string")?;
            if let Some(parent) = m.strip_suffix("/*") {
                let base = root.join(parent);
                let mut found: Vec<PathBuf> = fs::read_dir(&base)
                    .map_err(|e| format!("{}: {e}", base.display()))?
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.join(manifest).is_file())
                    .collect();
                found.sort();
                dirs.extend(found);
            } else {
                dirs.push(root.join(m));
            }
        }
        let mut crates = Vec::new();
        for dir in dirs {
            let t = read_toml(&dir.join(manifest))?;
            let name = t
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(toml::Value::as_str)
                .ok_or_else(|| format!("{}: no package.name", dir.display()))?
                .to_string();
            let mut deps = Vec::new();
            collect_deps(&t, &mut deps);
            if let Some(targets) = t.get("target").and_then(toml::Value::as_table) {
                for (_, target) in targets {
                    if let Some(target) = target.as_table() {
                        collect_deps(target, &mut deps);
                    }
                }
            }
            crates.push(CrateInfo {
                lib: lib_name(&name),
                name,
                dir,
                deps,
            });
        }
        Ok(Workspace {
            root: root.to_path_buf(),
            crates,
        })
    }

    fn by_name(&self, name: &str) -> Option<&CrateInfo> {
        self.crates.iter().find(|c| c.name == name)
    }

    fn is_member(&self, package: &str) -> bool {
        self.by_name(package).is_some()
    }
}

fn collect_deps(table: &toml::Table, out: &mut Vec<Dep>) {
    for (key, kind) in [
        ("dependencies", DepKind::Normal),
        ("build-dependencies", DepKind::Build),
        ("dev-dependencies", DepKind::Dev),
    ] {
        let Some(deps) = table.get(key).and_then(toml::Value::as_table) else {
            continue;
        };
        for (alias, spec) in deps {
            let package = spec
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(alias)
                .to_string();
            out.push(Dep {
                package,
                alias: lib_name(alias),
                kind,
            });
        }
    }
}

/// Run every check. Violations are sorted; an empty list is a pass.
pub fn check(ws: &Workspace, rules: &Rules) -> Vec<Violation> {
    let mut v = Vec::new();
    check_governance(ws, rules, &mut v);
    check_graphs(ws, rules, &mut v);
    // A component meets the others only through processes, so its manifest
    // already shows every edge it has; its sources are not Fleet's to index,
    // and thurbox's alone would cost more than the rest of the workspace.
    let indexes: Vec<CrateIndex> = ws
        .crates
        .iter()
        .filter(|c| {
            rules
                .crates
                .get(&c.name)
                .is_none_or(|r| r.role != Role::Component)
        })
        .map(|c| CrateIndex::build(c, ws, &mut v))
        .collect();
    for idx in &indexes {
        if let Some(rule) = rules.crates.get(&idx.name) {
            check_sources(ws, rules, rule, idx, &indexes, &mut v);
            if rule.role == Role::Pure {
                check_pure_guards(ws, rules, rule, idx, &mut v);
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

pub fn load_rules(path: &Path) -> Result<Rules, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn violation(
    code: &'static str,
    krate: &str,
    location: impl Into<String>,
    detail: impl Into<String>,
) -> Violation {
    Violation {
        code,
        krate: krate.to_string(),
        location: location.into(),
        detail: detail.into(),
    }
}

fn check_governance(ws: &Workspace, rules: &Rules, v: &mut Vec<Violation>) {
    for c in &ws.crates {
        if !rules.crates.contains_key(&c.name) {
            v.push(violation(
                "ungoverned-crate",
                &c.name,
                "Cargo.toml",
                "every workspace crate needs a rule",
            ));
        }
    }
    for (name, rule) in &rules.crates {
        if !ws.is_member(name) {
            v.push(violation(
                "missing-crate",
                name,
                "architecture.toml",
                "a rule names a crate that is not a member",
            ));
        }
        for d in rule.deps.iter().chain(&rule.dev_deps).chain(&rule.probes) {
            if !ws.is_member(d) {
                v.push(violation(
                    "missing-crate",
                    name,
                    "architecture.toml",
                    format!("names {d}, not a member"),
                ));
            }
        }
    }
}

/// A cycle in `edges`, as the list of its nodes, if there is one.
pub fn find_cycle(edges: &BTreeMap<String, BTreeSet<String>>) -> Option<Vec<String>> {
    fn visit(
        n: &str,
        edges: &BTreeMap<String, BTreeSet<String>>,
        state: &mut BTreeMap<String, u8>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        match state.get(n) {
            Some(2) => return None,
            Some(1) => {
                let at = stack.iter().position(|s| s == n).unwrap_or(0);
                let mut cycle = stack[at..].to_vec();
                cycle.push(n.to_string());
                return Some(cycle);
            }
            _ => {}
        }
        state.insert(n.to_string(), 1);
        stack.push(n.to_string());
        for m in edges.get(n).into_iter().flatten() {
            if let Some(c) = visit(m, edges, state, stack) {
                return Some(c);
            }
        }
        stack.pop();
        state.insert(n.to_string(), 2);
        None
    }
    let mut state = BTreeMap::new();
    for n in edges.keys() {
        let mut stack = Vec::new();
        if let Some(c) = visit(n, edges, &mut state, &mut stack) {
            return Some(c);
        }
    }
    None
}

fn check_graphs(ws: &Workspace, rules: &Rules, v: &mut Vec<Violation>) {
    let declared: BTreeMap<String, BTreeSet<String>> = rules
        .crates
        .iter()
        .map(|(n, r)| (n.clone(), r.deps.iter().cloned().collect()))
        .collect();
    if let Some(c) = find_cycle(&declared) {
        v.push(violation(
            "cycle",
            &c[0],
            "architecture.toml",
            format!("declared graph has a cycle: {}", c.join(" -> ")),
        ));
    }
    let mut actual: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for c in &ws.crates {
        let entry = actual.entry(c.name.clone()).or_default();
        let rule = rules.crates.get(&c.name);
        for d in c.deps.iter().filter(|d| ws.is_member(&d.package)) {
            let allowed = match (d.kind, rule) {
                (_, None) => true,
                (DepKind::Dev, Some(r)) => {
                    r.deps.contains(&d.package) || r.dev_deps.contains(&d.package)
                }
                (_, Some(r)) => r.deps.contains(&d.package),
            };
            if d.kind != DepKind::Dev {
                entry.insert(d.package.clone());
            }
            if !allowed {
                let code = if d.kind == DepKind::Dev {
                    "undeclared-dev-edge"
                } else {
                    "undeclared-edge"
                };
                v.push(violation(
                    code,
                    &c.name,
                    "Cargo.toml",
                    format!(
                        "depends on {} ({:?}), not allowed by architecture.toml",
                        d.package, d.kind
                    ),
                ));
            }
        }
        if let Some(r) = rule
            && r.role == Role::Pure
        {
            for d in c
                .deps
                .iter()
                .filter(|d| !ws.is_member(&d.package) && d.kind != DepKind::Dev)
            {
                if !r.external.contains(&d.package) {
                    v.push(violation(
                        "pure-external",
                        &c.name,
                        "Cargo.toml",
                        format!("pure crate depends on {}", d.package),
                    ));
                }
            }
        }
    }
    if let Some(c) = find_cycle(&actual) {
        v.push(violation(
            "cycle",
            &c[0],
            "Cargo.toml",
            format!("actual graph has a cycle: {}", c.join(" -> ")),
        ));
    }
}

/// A reference found in source, before resolution.
#[derive(Debug, Clone)]
struct Ref {
    module: Vec<String>,
    segments: Vec<String>,
    global: bool,
    macro_call: bool,
    location: String,
}

#[derive(Debug, Clone, Default)]
struct Scope {
    /// Local name -> the use path it stands for, unresolved, in this module.
    aliases: BTreeMap<String, (Vec<String>, bool)>,
    /// Glob imports, unresolved.
    globs: Vec<(Vec<String>, bool)>,
}

/// One crate's sources, indexed for resolution.
#[derive(Debug)]
struct CrateIndex {
    name: String,
    lib: String,
    externs: BTreeSet<String>,
    modules: BTreeMap<Vec<String>, Scope>,
    refs: Vec<Ref>,
    methods: Vec<(String, String)>,
    lib_doc: String,
    /// Dependency keys that rename a package: alias -> the package's lib name.
    renames: BTreeMap<String, String>,
}

impl CrateIndex {
    fn build(c: &CrateInfo, ws: &Workspace, v: &mut Vec<Violation>) -> CrateIndex {
        let mut externs: BTreeSet<String> = ["std", "core", "alloc"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        externs.extend(
            c.deps
                .iter()
                .filter(|d| d.kind != DepKind::Dev)
                .map(|d| d.alias.clone()),
        );
        let mut idx = CrateIndex {
            name: c.name.clone(),
            lib: c.lib.clone(),
            externs,
            modules: BTreeMap::new(),
            refs: Vec::new(),
            methods: Vec::new(),
            lib_doc: String::new(),
            renames: c
                .deps
                .iter()
                .filter(|d| d.alias != lib_name(&d.package))
                .map(|d| (d.alias.clone(), lib_name(&d.package)))
                .collect(),
        };
        let src = c.dir.join("src");
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        files.sort();
        for file in files {
            let rel = file
                .strip_prefix(&ws.root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let text = match fs::read_to_string(&file) {
                Ok(t) => t,
                Err(e) => {
                    v.push(violation("unreadable", &c.name, rel, e.to_string()));
                    continue;
                }
            };
            let parsed = match syn::parse_file(&text) {
                Ok(f) => f,
                Err(e) => {
                    v.push(violation("unparsable", &c.name, rel, e.to_string()));
                    continue;
                }
            };
            let module = module_of(&src, &file);
            if module.is_empty() && file.file_name().is_some_and(|n| n == "lib.rs") {
                idx.lib_doc = doc_text(&parsed.attrs);
            }
            let mut walker = Walker {
                idx: &mut idx,
                module,
                file: rel,
            };
            walker.scope();
            walker.items(&parsed.items);
        }
        idx
    }
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// `src/lib.rs` and `src/main.rs` are the root; `src/a/b.rs` and
/// `src/a/b/mod.rs` are `a::b`. A file under `src/bin/` is its own root.
fn module_of(src: &Path, file: &Path) -> Vec<String> {
    let rel = file.strip_prefix(src).unwrap_or(file);
    let mut parts: Vec<String> = rel
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if parts.first().is_some_and(|p| p == "bin") {
        return Vec::new();
    }
    if let Some(last) = parts.pop() {
        let stem = last.trim_end_matches(".rs").to_string();
        if !(parts.is_empty() && (stem == "lib" || stem == "main")) && stem != "mod" {
            parts.push(stem);
        }
    }
    parts
}

fn doc_text(attrs: &[syn::Attribute]) -> String {
    let mut out = String::new();
    for a in attrs {
        if let syn::Meta::NameValue(nv) = &a.meta
            && nv.path.is_ident("doc")
            && let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
        {
            out.push_str(&s.value());
            out.push('\n');
        }
    }
    out
}

struct Walker<'a> {
    idx: &'a mut CrateIndex,
    module: Vec<String>,
    file: String,
}

impl Walker<'_> {
    fn location(&self, span: proc_macro2::Span) -> String {
        format!("{}:{}", self.file, span.start().line)
    }

    fn items(&mut self, items: &[Item]) {
        for item in items {
            self.visit_item(item);
        }
    }

    fn scope(&mut self) -> &mut Scope {
        self.idx.modules.entry(self.module.clone()).or_default()
    }

    /// Record a `use` tree: each leaf is both an import and a reference.
    fn use_tree(
        &mut self,
        tree: &UseTree,
        prefix: &mut Vec<String>,
        global: bool,
        span: proc_macro2::Span,
    ) {
        match tree {
            UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.use_tree(&p.tree, prefix, global, span);
                prefix.pop();
            }
            UseTree::Name(n) => {
                let name = n.ident.to_string();
                let (local, target) = if name == "self" {
                    (prefix.last().cloned().unwrap_or_default(), prefix.clone())
                } else {
                    let mut t = prefix.clone();
                    t.push(name.clone());
                    (name, t)
                };
                self.import(local, target, global, span);
            }
            UseTree::Rename(r) => {
                let mut t = prefix.clone();
                if r.ident != "self" {
                    t.push(r.ident.to_string());
                }
                self.import(r.rename.to_string(), t, global, span);
            }
            UseTree::Glob(_) => {
                let p = prefix.clone();
                let loc = self.location(span);
                self.scope().globs.push((p.clone(), global));
                self.idx.refs.push(Ref {
                    module: self.module.clone(),
                    segments: p,
                    global,
                    macro_call: false,
                    location: loc,
                });
            }
            UseTree::Group(g) => {
                for t in &g.items {
                    self.use_tree(t, prefix, global, span);
                }
            }
        }
    }

    fn import(
        &mut self,
        local: String,
        target: Vec<String>,
        global: bool,
        span: proc_macro2::Span,
    ) {
        let loc = self.location(span);
        self.idx.refs.push(Ref {
            module: self.module.clone(),
            segments: target.clone(),
            global,
            macro_call: false,
            location: loc,
        });
        if local != "_" {
            self.scope().aliases.insert(local, (target, global));
        }
    }
}

impl<'ast> Visit<'ast> for Walker<'_> {
    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        let span = u.use_token.span;
        self.use_tree(&u.tree, &mut Vec::new(), u.leading_colon.is_some(), span);
    }

    fn visit_item_extern_crate(&mut self, e: &'ast syn::ItemExternCrate) {
        let name = e.ident.to_string();
        let local = e
            .rename
            .as_ref()
            .map_or(name.clone(), |(_, r)| r.to_string());
        self.import(local, vec![name], true, e.ident.span());
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if let Some((_, items)) = &m.content {
            self.module.push(m.ident.to_string());
            self.scope();
            self.items(items);
            self.module.pop();
        }
    }

    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}

    fn visit_path(&mut self, p: &'ast syn::Path) {
        let segments: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
        if let Some(first) = p.segments.first() {
            let loc = self.location(first.ident.span());
            self.idx.refs.push(Ref {
                module: self.module.clone(),
                segments,
                global: p.leading_colon.is_some(),
                macro_call: false,
                location: loc,
            });
        }
        visit::visit_path(self, p);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let segments: Vec<String> = m
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        if let Some(first) = m.path.segments.first() {
            let loc = self.location(first.ident.span());
            self.idx.refs.push(Ref {
                module: self.module.clone(),
                segments,
                global: m.path.leading_colon.is_some(),
                macro_call: true,
                location: loc,
            });
        }
        // The arguments of an expression-like macro (`format!`, `vec!`,
        // `assert!`, ...) are code too: parse them as expressions and judge them.
        let args = m.parse_body_with(
            syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
        );
        if let Ok(args) = args {
            for a in &args {
                self.visit_expr(a);
            }
        }
    }

    fn visit_expr_method_call(&mut self, c: &'ast syn::ExprMethodCall) {
        let loc = self.location(c.method.span());
        self.idx.methods.push((c.method.to_string(), loc));
        visit::visit_expr_method_call(self, c);
    }
}

/// Resolves paths across every crate's index.
struct Resolver<'a> {
    indexes: &'a [CrateIndex],
    budget: std::cell::Cell<u32>,
    exhausted: std::cell::Cell<bool>,
}

/// Resolution steps one reference may take before it is reported as
/// unresolvable instead of judged.
const RESOLVE_BUDGET: u32 = 4_096;

impl Resolver<'_> {
    fn index(&self, lib: &str) -> Option<&CrateIndex> {
        self.indexes.iter().find(|i| i.lib == lib)
    }

    /// Every absolute path a reference may stand for, after aliases and
    /// re-exports. More than one only through globs.
    /// `globs`: whether an unresolved name may come through the module's glob
    /// imports. A glob's own base is resolved without them, so a glob can
    /// never expand through itself.
    #[allow(clippy::too_many_arguments)]
    fn resolve(
        &self,
        idx: &CrateIndex,
        module: &[String],
        segments: &[String],
        global: bool,
        macro_call: bool,
        globs: bool,
        depth: u32,
    ) -> Vec<Vec<String>> {
        if segments.is_empty() || depth > 24 || !self.spend() {
            return Vec::new();
        }
        let first = segments[0].as_str();
        let rest = &segments[1..];
        let join = |base: Vec<String>| -> Vec<String> {
            base.into_iter().chain(rest.iter().cloned()).collect()
        };
        let starts: Vec<Vec<String>> = if global {
            vec![segments.to_vec()]
        } else {
            match first {
                "crate" => vec![join(vec![idx.lib.clone()])],
                "self" => vec![join(
                    std::iter::once(idx.lib.clone())
                        .chain(module.iter().cloned())
                        .collect(),
                )],
                "super" => {
                    let mut m = module.to_vec();
                    let mut segs = segments;
                    while segs.first().is_some_and(|s| s == "super") {
                        m.pop();
                        segs = &segs[1..];
                    }
                    vec![
                        std::iter::once(idx.lib.clone())
                            .chain(m)
                            .chain(segs.iter().cloned())
                            .collect(),
                    ]
                }
                "Self" => Vec::new(),
                _ => {
                    let scope = idx.modules.get(module);
                    if let Some((target, g)) = scope.and_then(|s| s.aliases.get(first)) {
                        if target.len() == 1 && target[0] == first && !g {
                            // `use x;` of an extern crate: it names itself.
                            vec![segments.to_vec()]
                        } else {
                            let base =
                                self.resolve(idx, module, target, *g, false, globs, depth + 1);
                            base.into_iter().map(join).collect()
                        }
                    } else if idx.externs.contains(first) || self.index(first).is_some() {
                        vec![segments.to_vec()]
                    } else {
                        let mut out = vec![
                            std::iter::once(idx.lib.clone())
                                .chain(module.iter().cloned())
                                .chain(segments.iter().cloned())
                                .collect(),
                        ];
                        let module_globs = if globs {
                            scope.map(|s| s.globs.clone()).unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        for (g, gg) in module_globs {
                            for base in self.resolve(idx, module, &g, gg, false, false, depth + 1) {
                                out.push(
                                    base.into_iter().chain(segments.iter().cloned()).collect(),
                                );
                            }
                        }
                        if macro_call && segments.len() == 1 {
                            out.push(vec!["std".to_string(), first.to_string()]);
                        }
                        out
                    }
                }
            }
        };
        let mut out = Vec::new();
        for mut s in starts {
            // A renamed dependency (`policy = { package = "x" }`) is crate `x`.
            if let Some(real) = s.first().and_then(|f| idx.renames.get(f)) {
                s[0] = real.clone();
            }
            out.extend(self.through_exports(s, depth + 1));
        }
        out
    }

    /// Follow re-exports: if `a::b::c` names an item that module `a::b` of
    /// crate `a` imports (or glob-imports), replace that prefix by what it
    /// imports, until nothing changes.
    fn through_exports(&self, path: Vec<String>, depth: u32) -> Vec<Vec<String>> {
        if depth > 24 || path.len() < 2 {
            return vec![path];
        }
        let Some(idx) = self.index(&path[0]) else {
            return vec![path];
        };
        for k in (2..=path.len()).rev() {
            let module: Vec<String> = path[1..k - 1].to_vec();
            let name = &path[k - 1];
            // The longest prefix that is a real module is where the name
            // lives: a shorter prefix would re-read a module path segment as
            // an import, which is how a glob would expand through itself.
            let Some(scope) = idx.modules.get(&module) else {
                continue;
            };
            let tail = &path[k..];
            if !self.spend() {
                return vec![path];
            }
            if let Some((target, g)) = scope.aliases.get(name) {
                let mut out = Vec::new();
                for base in self.resolve(idx, &module, target, *g, false, true, depth + 1) {
                    out.push(base.into_iter().chain(tail.iter().cloned()).collect());
                }
                if !out.is_empty() {
                    return out;
                }
            }
            if !scope.globs.is_empty() {
                // The name may be the module's own item or come through any
                // of its globs: judge every possibility.
                let mut out = vec![path.clone()];
                for (g, gg) in &scope.globs {
                    for base in self.resolve(idx, &module, g, *gg, false, false, depth + 1) {
                        let through: Vec<String> = base
                            .into_iter()
                            .chain(std::iter::once(name.clone()))
                            .chain(tail.iter().cloned())
                            .collect();
                        out.extend(self.through_exports(through, depth + 1));
                    }
                }
                return out;
            }
            return vec![path];
        }
        vec![path]
    }

    /// One unit of the per-reference resolution budget. Past it, the
    /// reference is reported as unresolvable rather than left to grow.
    fn spend(&self) -> bool {
        let left = self.budget.get();
        if left == 0 {
            self.exhausted.set(true);
            return false;
        }
        self.budget.set(left - 1);
        true
    }
}

fn starts_with(path: &[String], prefix: &str) -> bool {
    let p: Vec<&str> = prefix.split("::").collect();
    path.len() >= p.len() && path.iter().zip(&p).all(|(a, b)| a == b)
}

fn check_sources(
    ws: &Workspace,
    rules: &Rules,
    rule: &CrateRule,
    idx: &CrateIndex,
    all: &[CrateIndex],
    v: &mut Vec<Violation>,
) {
    let resolver = Resolver {
        indexes: all,
        budget: std::cell::Cell::new(0),
        exhausted: std::cell::Cell::new(false),
    };
    let allowed_libs: BTreeSet<String> = rule.deps.iter().map(|d| lib_name(d)).collect();
    let workspace_libs: BTreeSet<String> = ws.crates.iter().map(|c| c.lib.clone()).collect();
    for r in &idx.refs {
        resolver.budget.set(RESOLVE_BUDGET);
        resolver.exhausted.set(false);
        let paths = resolver.resolve(idx, &r.module, &r.segments, r.global, r.macro_call, true, 0);
        if resolver.exhausted.get() {
            v.push(violation(
                "unresolvable",
                &idx.name,
                &r.location,
                format!("{} exceeds the resolution budget", r.segments.join("::")),
            ));
            continue;
        }
        for path in paths {
            let shown = path.join("::");
            let root = path[0].as_str();
            if root != idx.lib && workspace_libs.contains(root) && !allowed_libs.contains(root) {
                // Reached directly (it is in this crate's manifest, under any
                // name) or only through another crate's re-export.
                let direct = idx.externs.contains(root) || idx.renames.values().any(|l| l == root);
                let code = if direct {
                    "undeclared-edge"
                } else {
                    "reexport-edge"
                };
                v.push(violation(
                    code,
                    &idx.name,
                    &r.location,
                    format!("reaches {shown}, and {root} is not an allowed dependency"),
                ));
            }
            if rule.role == Role::Pure {
                if let Some(f) = rules
                    .pure
                    .forbidden_paths
                    .iter()
                    .find(|f| starts_with(&path, f))
                {
                    v.push(violation(
                        "pure-io",
                        &idx.name,
                        &r.location,
                        format!("reaches {shown} (forbidden: {f})"),
                    ));
                }
                let std_like = ["std", "core", "alloc"].contains(&root);
                if root != idx.lib
                    && !std_like
                    && !workspace_libs.contains(root)
                    && !rule.external.iter().any(|e| lib_name(e) == root)
                {
                    v.push(violation(
                        "pure-external",
                        &idx.name,
                        &r.location,
                        format!("reaches {shown} of external crate {root}"),
                    ));
                }
            }
            // The same forbidden methods called as paths: `Store::update_status(&s, ..)`
            // or `FileStore::update_status(..)`. A type or trait segment before
            // the method is what tells it from a free function of the same name.
            if let [.., owner, method] = path.as_slice()
                && owner.starts_with(|c: char| c.is_ascii_uppercase())
                && rule.forbidden_methods.contains(method)
            {
                v.push(violation(
                    "cli-method",
                    &idx.name,
                    &r.location,
                    format!("calls {shown}: a write the manager owns"),
                ));
            }
            for x in rules
                .restricted
                .iter()
                .filter(|x| !x.allowed.contains(&idx.name))
            {
                if starts_with(&path, &x.path) {
                    v.push(violation(
                        "restricted",
                        &idx.name,
                        &r.location,
                        format!("reaches {shown}: {}", x.why),
                    ));
                }
            }
            for (writer, crates) in &rules.status_writers {
                if starts_with(&path, writer) && !crates.contains(&idx.name) {
                    v.push(violation(
                        "status-writer",
                        &idx.name,
                        &r.location,
                        format!("names {writer}, which only {} may", crates.join(", ")),
                    ));
                }
            }
        }
    }
    for (method, loc) in &idx.methods {
        if rule.forbidden_methods.contains(method) {
            v.push(violation(
                "cli-method",
                &idx.name,
                loc,
                format!("calls .{method}(): a write the manager owns"),
            ));
        }
    }
}

/// Each pure crate keeps its compile-time guards: `compile_fail` doc probes
/// naming every forbidden workspace crate, and a clippy.toml disallowing
/// the I/O types and functions.
fn check_pure_guards(
    ws: &Workspace,
    rules: &Rules,
    rule: &CrateRule,
    idx: &CrateIndex,
    v: &mut Vec<Violation>,
) {
    let probes = compile_fail_uses(&idx.lib_doc);
    for p in &rule.probes {
        if !probes.contains(&lib_name(p)) {
            v.push(violation(
                "missing-probe",
                &idx.name,
                "src/lib.rs",
                format!("no compile_fail probe uses {}", lib_name(p)),
            ));
        }
    }
    let Some(c) = ws.by_name(&idx.name) else {
        return;
    };
    let clippy = c.dir.join("clippy.toml");
    let text = fs::read_to_string(clippy).unwrap_or_default();
    let table: toml::Table = text.parse().unwrap_or_default();
    let mut guarded = BTreeSet::new();
    for key in ["disallowed-types", "disallowed-methods"] {
        for entry in table
            .get(key)
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            let path = entry
                .get("path")
                .and_then(toml::Value::as_str)
                .or_else(|| entry.as_str());
            guarded.extend(path.map(str::to_string));
        }
    }
    for g in &rules.pure.clippy_guards {
        if !guarded.contains(g) {
            v.push(violation(
                "missing-clippy-guard",
                &idx.name,
                "clippy.toml",
                format!("{g} is not disallowed"),
            ));
        }
    }
}

/// The crate names `use`d inside ```` ```compile_fail ```` blocks of a doc.
fn compile_fail_uses(doc: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_probe = false;
    for line in doc.lines() {
        let t = line.trim();
        if let Some(info) = t.strip_prefix("```") {
            in_probe = !in_probe && info.split(',').any(|w| w.trim() == "compile_fail");
            continue;
        }
        if in_probe
            && let Some(rest) = t.strip_prefix("use ")
            && let Some(root) = rest.split("::").next()
        {
            out.insert(root.trim().to_string());
        }
    }
    out
}
