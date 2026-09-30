//! The dependency-edge resolver behind `tests/architecture_rules.rs`.
//!
//! Reads a source tree the way the compiler names things, closely enough that
//! an edge is judged by what a path *resolves to* rather than by how it is
//! spelt:
//!
//! - every file and inline `mod x { … }` is a module path, so `super::`,
//!   `self::` and a bare child-module path (`backend::Session` inside
//!   `agent/mod.rs`) resolve relative to where they are written;
//! - `use` trees are parsed whole — nested brace groups, `self` inside a group,
//!   `as` renames and globs — and each leaf is one reference;
//! - a name imported by `use` resolves through that import where it is used;
//! - `pub use` re-exports and `type` aliases are chased, so a crossing made
//!   through `crate::agent::Name` or through an alias is attributed to the module
//!   the name really lives in (and the alias itself is an edge of the module that
//!   declares it);
//! - code under `#[cfg(test)]` (an item, an inline module, or a file declared by
//!   `#[cfg(test)] mod x;`) is marked, so production-only properties can leave it
//!   out.
//!
//! What stays invisible: `$crate::` inside macro bodies, method calls on a trait
//! object, and glob re-exports' members (`pub use x::*` resolves no further than
//! the module that re-exports them — every such glob in `src/` stays inside one
//! governed module). The trait objects are why the backend contract is kept
//! small and typed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub type ModPath = Vec<String>;

/// One resolved reference between two governed nodes.
#[derive(Clone, Debug)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// The first segment past the target node: the item referenced, or `""`
    /// for the module itself.
    pub item: String,
    pub file: PathBuf,
    pub line: usize,
    pub in_use: bool,
    pub test: bool,
}

impl Edge {
    pub fn target(&self) -> String {
        match self.item.as_str() {
            "" => self.to.clone(),
            item => format!("{}::{item}", self.to),
        }
    }

    pub fn site(&self, root: &Path) -> String {
        let file = self.file.strip_prefix(root).unwrap_or(&self.file);
        format!(
            "{}:{}",
            file.display().to_string().replace('\\', "/"),
            self.line
        )
    }
}

struct SourceFile {
    path: PathBuf,
    stripped: String,
    module: ModPath,
    /// Inline `mod x { … }` bodies: byte span and the module path inside.
    scopes: Vec<(usize, usize, ModPath)>,
    test_spans: Vec<(usize, usize)>,
    /// `mod x;` declarations: offset and name.
    declarations: Vec<(usize, String)>,
    uses: Vec<UseLeaf>,
    use_spans: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
struct UseLeaf {
    offset: usize,
    /// Segments as written, `super`/`self`/`crate` included.
    raw: Vec<String>,
    /// The name it binds in its scope; `None` for a glob.
    binding: Option<String>,
    public: bool,
}

/// A parsed source tree: the module set, the re-export/alias map and every
/// resolved edge between governed nodes.
pub struct Tree {
    pub root: PathBuf,
    files: Vec<SourceFile>,
    modules: HashSet<ModPath>,
    test_modules: Vec<ModPath>,
    /// `(module, name)` → the absolute paths `name` stands for there: a
    /// `pub use` leaf, or every crate path on a `type` alias's right-hand side.
    reexports: HashMap<(ModPath, String), Vec<ModPath>>,
    /// Per scope: name → absolute path, from its `use` statements.
    bindings: HashMap<ModPath, HashMap<String, ModPath>>,
    /// Scopes holding `use super::*`, which see their parent's bindings too.
    glob_parent: HashSet<ModPath>,
}

pub fn segments(node: &str) -> ModPath {
    node.split("::").map(str::to_string).collect()
}

pub fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl Tree {
    /// Parse every `.rs` file under `root` except `root/bin/` (separate crates).
    pub fn load(root: &Path) -> Tree {
        let mut files = Vec::new();
        for path in collect_rs(root) {
            let rel = path.strip_prefix(root).expect("file under root");
            let parts: Vec<String> = rel
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if parts.first().is_some_and(|p| p == "bin") {
                continue;
            }
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            files.push(SourceFile::parse(path.clone(), module_of(&parts), &content));
        }
        let mut tree = Tree {
            root: root.to_path_buf(),
            files,
            modules: HashSet::new(),
            test_modules: Vec::new(),
            reexports: HashMap::new(),
            bindings: HashMap::new(),
            glob_parent: HashSet::new(),
        };
        tree.index();
        tree
    }

    fn index(&mut self) {
        self.index_modules();
        self.bind_uses();
        self.index_aliases();
    }

    /// Every file and inline module, and which of them are test-only.
    fn index_modules(&mut self) {
        for file in &self.files {
            self.modules.insert(file.module.clone());
            for (_, _, scope) in &file.scopes {
                self.modules.insert(scope.clone());
            }
            for (offset, name) in &file.declarations {
                let mut child = file.scope_at(*offset);
                child.push(name.clone());
                self.modules.insert(child.clone());
                if file.in_test_span(*offset) {
                    self.test_modules.push(child);
                }
            }
        }
        for (_, _, scope) in self.files.iter().flat_map(|f| {
            f.scopes
                .iter()
                .filter(|(s, _, _)| f.in_test_span(*s))
                .collect::<Vec<_>>()
        }) {
            self.test_modules.push(scope.clone());
        }
    }

    /// Every `use` leaf's binding, and the public ones as re-exports.
    ///
    /// Before aliases, which may resolve through bindings. A `use` path may
    /// itself start at an imported name (`use x as y;` then `pub use
    /// y::Item;`), so leaves are bound in passes until a pass binds nothing
    /// new: each pass can only add a binding, so it terminates.
    fn bind_uses(&mut self) {
        let mut pending: Vec<(ModPath, UseLeaf)> = self
            .files
            .iter()
            .flat_map(|f| {
                f.uses
                    .iter()
                    .map(|leaf| (f.scope_at(leaf.offset), leaf.clone()))
            })
            .collect();
        let mut public = Vec::new();
        loop {
            let before = pending.len();
            let mut unresolved = Vec::new();
            for (scope, leaf) in pending {
                let Some(abs) = self.absolute_code(&scope, &leaf.raw) else {
                    unresolved.push((scope, leaf));
                    continue;
                };
                match &leaf.binding {
                    Some(name) if name != "_" => {
                        self.bindings
                            .entry(scope.clone())
                            .or_default()
                            .insert(name.clone(), abs.clone());
                        if leaf.public {
                            public.push(((scope, name.clone()), abs));
                        }
                    }
                    Some(_) => {}
                    None => {
                        if leaf.raw == ["super"] {
                            self.glob_parent.insert(scope);
                        }
                    }
                }
            }
            pending = unresolved;
            if pending.len() == before {
                break;
            }
        }
        for (key, abs) in public {
            self.reexports.entry(key).or_default().push(abs);
        }
    }

    /// Every `type` alias, as a re-export of each crate path on its right-hand
    /// side.
    fn index_aliases(&mut self) {
        let mut aliases = Vec::new();
        for file in &self.files {
            for (offset, name, rhs) in type_aliases(&file.stripped) {
                let scope = file.scope_at(offset);
                let targets: Vec<ModPath> = code_paths(&file.stripped[rhs.0..rhs.1])
                    .into_iter()
                    .filter_map(|(_, raw)| self.absolute_code(&scope, &raw))
                    .collect();
                if !targets.is_empty() {
                    aliases.push(((scope, name), targets));
                }
            }
        }
        for (key, targets) in aliases {
            self.reexports.entry(key).or_default().extend(targets);
        }
    }

    fn is_module(&self, path: &[String]) -> bool {
        self.modules.contains(path)
    }

    /// A `use` path made absolute: `crate`/`thurbox` (the library's name, the
    /// only spelling the binary's own modules have), `super`, `self`, or a
    /// child module of the scope. Anything else is another crate.
    fn absolute_use(&self, scope: &[String], raw: &[String]) -> Option<ModPath> {
        let (first, rest) = raw.split_first()?;
        let mut base: ModPath = match first.as_str() {
            "crate" | "thurbox" => Vec::new(),
            "self" => scope.to_vec(),
            "super" => {
                let mut base = scope.to_vec();
                base.pop()?;
                let mut rest = rest;
                while rest.first().is_some_and(|s| s == "super") {
                    base.pop()?;
                    rest = &rest[1..];
                }
                base.extend(rest.iter().cloned());
                return Some(base);
            }
            name => {
                let mut child = scope.to_vec();
                child.push(name.to_string());
                if !self.is_module(&child) {
                    return None;
                }
                scope.to_vec()
            }
        };
        if !matches!(first.as_str(), "crate" | "thurbox" | "self") {
            base.push(first.clone());
        }
        base.extend(rest.iter().cloned());
        Some(base)
    }

    /// A path made absolute: as for [`Self::absolute_use`], plus a name the
    /// scope imported (`use crate::agent::tmux;` then `tmux::x()`, or `pub use
    /// tmux::X;`).
    fn absolute_code(&self, scope: &[String], raw: &[String]) -> Option<ModPath> {
        if let Some(abs) = self.absolute_use(scope, raw) {
            return Some(abs);
        }
        let (first, rest) = raw.split_first()?;
        let mut at = scope.to_vec();
        loop {
            if let Some(bound) = self.bindings.get(&at).and_then(|b| b.get(first)) {
                let mut abs = bound.clone();
                abs.extend(rest.iter().cloned());
                return Some(abs);
            }
            if !self.glob_parent.contains(&at) || at.pop().is_none() {
                return None;
            }
        }
    }

    /// Where an absolute path lands: the deepest module it names, and the item
    /// after it — chased through re-exports and aliases.
    pub fn resolve(&self, path: &[String]) -> Vec<(ModPath, String)> {
        self.resolve_depth(path, 0)
    }

    fn resolve_depth(&self, path: &[String], depth: usize) -> Vec<(ModPath, String)> {
        let k = (0..=path.len())
            .rev()
            .find(|&k| self.is_module(&path[..k]))
            .unwrap_or(0);
        let module = path[..k].to_vec();
        let Some(item) = path.get(k) else {
            return vec![(module, String::new())];
        };
        if depth < 8 {
            if let Some(targets) = self.reexports.get(&(module.clone(), item.clone())) {
                // A binding naming itself (`pub use self::x;` of a child
                // module) resolves to what it already is.
                let chased: Vec<_> = targets
                    .iter()
                    .filter(|t| t.as_slice() != &path[..=k])
                    .flat_map(|t| {
                        let mut full = t.clone();
                        full.extend(path[k + 1..].iter().cloned());
                        self.resolve_depth(&full, depth + 1)
                    })
                    .collect();
                if !chased.is_empty() {
                    return chased;
                }
            }
        }
        vec![(module, item.clone())]
    }

    fn is_test_module(&self, module: &[String]) -> bool {
        self.test_modules.iter().any(|t| module.starts_with(t))
    }

    /// Every reference between two distinct governed nodes.
    pub fn edges(&self, nodes: &[&str]) -> Vec<Edge> {
        let governed: Vec<ModPath> = nodes.iter().map(|n| segments(n)).collect();
        let node_of = |path: &[String]| -> Option<String> {
            governed
                .iter()
                .filter(|g| path.starts_with(g))
                .max_by_key(|g| g.len())
                .map(|g| g.join("::"))
        };
        let mut edges = Vec::new();
        for file in &self.files {
            let file_test = self.is_test_module(&file.module);
            let mut refs: Vec<(usize, ModPath, bool)> = Vec::new();
            for leaf in &file.uses {
                let scope = file.scope_at(leaf.offset);
                if let Some(abs) = self.absolute_code(&scope, &leaf.raw) {
                    refs.push((leaf.offset, abs, true));
                }
            }
            for (offset, raw) in code_paths(&file.stripped) {
                if file
                    .use_spans
                    .iter()
                    .any(|&(s, e)| offset >= s && offset < e)
                {
                    continue;
                }
                let scope = file.scope_at(offset);
                if let Some(abs) = self.absolute_code(&scope, &raw) {
                    refs.push((offset, abs, false));
                }
            }
            for (offset, abs, in_use) in refs {
                let scope = file.scope_at(offset);
                let Some(from) = node_of(&scope) else {
                    continue;
                };
                let test = file_test || file.in_test_span(offset) || self.is_test_module(&scope);
                for (module, item) in self.resolve(&abs) {
                    let Some(to) = node_of(&module) else {
                        continue;
                    };
                    if to == from {
                        continue;
                    }
                    // The item named past the node, not past the deepest
                    // module: `backend::pane::x` from outside a governed
                    // `backend` is the item `pane`.
                    let depth = segments(&to).len();
                    let item = if module.len() > depth {
                        module[depth].clone()
                    } else {
                        item
                    };
                    edges.push(Edge {
                        from: from.clone(),
                        to,
                        item,
                        file: file.path.clone(),
                        line: file.stripped[..offset].matches('\n').count() + 1,
                        in_use,
                        test,
                    });
                }
            }
        }
        edges
    }

    /// Every module under `parent` that has a file of its own, at any depth,
    /// test modules left out. An inline `mod x { … }` is part of the file
    /// holding it: splitting it out means a new file, which this then names.
    pub fn file_descendants(&self, parent: &str) -> BTreeSet<String> {
        let parent = segments(parent);
        self.files
            .iter()
            .map(|f| &f.module)
            .filter(|m| m.len() > parent.len() && m.starts_with(&parent))
            .filter(|m| !self.is_test_module(m))
            .map(|m| m.join("::"))
            .collect()
    }

    pub fn has_module(&self, node: &str) -> bool {
        self.is_module(&segments(node))
    }
}

impl SourceFile {
    fn parse(path: PathBuf, module: ModPath, content: &str) -> SourceFile {
        let stripped = strip_comments_and_strings(content);
        let test_spans = test_spans(&stripped);
        let (inline, declarations) = mod_items(&stripped);
        let use_spans = use_spans(&stripped);
        let mut uses = Vec::new();
        for &(start, end) in &use_spans {
            let public = is_public(&stripped, start);
            for (raw, binding) in parse_use_tree(&stripped[start + 3..end]) {
                uses.push(UseLeaf {
                    offset: start,
                    raw,
                    binding,
                    public,
                });
            }
        }
        // Nest inline scopes: a scope's path is its innermost parent's plus
        // its own name.
        let mut scopes: Vec<(usize, usize, ModPath)> = Vec::new();
        let mut sorted = inline;
        sorted.sort_by_key(|(s, _, _)| *s);
        for (start, end, name) in sorted {
            let mut path = scopes
                .iter()
                .filter(|(s, e, _)| *s < start && end <= *e)
                .max_by_key(|(s, _, _)| *s)
                .map_or_else(|| module.clone(), |(_, _, p)| p.clone());
            path.push(name);
            scopes.push((start, end, path));
        }
        SourceFile {
            path,
            stripped,
            module,
            scopes,
            test_spans,
            declarations,
            uses,
            use_spans,
        }
    }

    fn scope_at(&self, offset: usize) -> ModPath {
        self.scopes
            .iter()
            .filter(|(s, e, _)| *s <= offset && offset < *e)
            .max_by_key(|(s, _, _)| *s)
            .map_or_else(|| self.module.clone(), |(_, _, p)| p.clone())
    }

    fn in_test_span(&self, offset: usize) -> bool {
        self.test_spans
            .iter()
            .any(|&(s, e)| offset >= s && offset < e)
    }
}

/// `a/b/mod.rs` → `a::b`; `a/b.rs` → `a::b`; `lib.rs`/`main.rs` → the root.
fn module_of(parts: &[String]) -> ModPath {
    let mut module: ModPath = parts.to_vec();
    let last = module.pop().expect("a file name");
    let stem = last.trim_end_matches(".rs");
    let at_root = module.is_empty();
    if !(stem == "mod" || (at_root && (stem == "lib" || stem == "main"))) {
        module.push(stem.to_string());
    }
    module
}

fn collect_rs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            out.extend(collect_rs(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn read_ident(bytes: &[u8], i: &mut usize) -> String {
    let start = *i;
    while *i < bytes.len() && is_ident_char(bytes[*i]) {
        *i += 1;
    }
    String::from_utf8_lossy(&bytes[start..*i]).into_owned()
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// The index just past the brace/bracket/paren group opening at `open`.
fn matching_close(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// The end of the item starting at `from`: its first `;` outside any group, or
/// the end of its first `{ … }` body.
fn item_end(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b';' => return i + 1,
            b'{' => return matching_close(bytes, i),
            b'(' | b'[' => i = matching_close(bytes, i),
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Whether a `cfg(…)` predicate is only ever true under test: `test`, or an
/// `all(…)` with `test` among its operands. `any(windows, test)` is compiled
/// into a Windows release and `not(test)` into every release, so neither is.
fn cfg_is_test(predicate: &str) -> bool {
    let p: String = predicate.chars().filter(|c| !c.is_whitespace()).collect();
    if p == "test" {
        return true;
    }
    let Some(inner) = p.strip_prefix("all(").and_then(|r| r.strip_suffix(')')) else {
        return false;
    };
    let mut depth = 0;
    let mut start = 0;
    let mut operands = Vec::new();
    for (i, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                operands.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    operands.push(&inner[start..]);
    operands.contains(&"test")
}

/// Spans of the items a test-only `#[cfg(…)]` attribute governs.
fn test_spans(stripped: &str) -> Vec<(usize, usize)> {
    let bytes = stripped.as_bytes();
    let mut spans = Vec::new();
    let mut search = 0;
    while let Some(found) = stripped[search..].find("#[cfg(") {
        let at = search + found;
        let open = at + "#[cfg".len();
        let close = matching_close(bytes, open);
        search = close;
        if !cfg_is_test(&stripped[open + 1..close - 1]) {
            continue;
        }
        // Past this and any further attributes to the item itself.
        let mut i = skip_ws(bytes, close + 1);
        while bytes.get(i) == Some(&b'#') {
            let open = i + 1;
            i = skip_ws(bytes, matching_close(bytes, open));
        }
        spans.push((at, item_end(bytes, i)));
    }
    spans
}

fn keyword_at(bytes: &[u8], i: usize, word: &str) -> bool {
    let w = word.as_bytes();
    bytes.len() >= i + w.len()
        && &bytes[i..i + w.len()] == w
        && (i == 0 || !is_ident_char(bytes[i - 1]))
        && bytes.get(i + w.len()).is_some_and(u8::is_ascii_whitespace)
}

/// Inline `mod x { … }` spans (start, end, name) and `mod x;` declarations.
#[allow(clippy::type_complexity)]
fn mod_items(stripped: &str) -> (Vec<(usize, usize, String)>, Vec<(usize, String)>) {
    let bytes = stripped.as_bytes();
    let mut inline = Vec::new();
    let mut declared = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !keyword_at(bytes, i, "mod") {
            i += 1;
            continue;
        }
        let mut j = skip_ws(bytes, i + 3);
        let name = read_ident(bytes, &mut j);
        j = skip_ws(bytes, j);
        match bytes.get(j) {
            Some(b';') if !name.is_empty() => declared.push((i, name)),
            Some(b'{') if !name.is_empty() => inline.push((j, matching_close(bytes, j), name)),
            _ => {}
        }
        i += 3;
    }
    (inline, declared)
}

/// Byte spans of `use …;` statements.
pub fn use_spans(stripped: &str) -> Vec<(usize, usize)> {
    let bytes = stripped.as_bytes();
    let mut spans = Vec::new();
    let mut search = 0;
    while let Some(found) = stripped[search..].find("use") {
        let start = search + found;
        search = start + 3;
        if keyword_at(bytes, start, "use") {
            let end = stripped[start..]
                .find(';')
                .map_or(stripped.len(), |e| start + e + 1);
            spans.push((start, end));
            search = end;
        }
    }
    spans
}

/// Whether the `use` at `start` carries a visibility (`pub`, `pub(crate)`, …),
/// which makes each of its bindings nameable from elsewhere.
fn is_public(stripped: &str, start: usize) -> bool {
    let before = &stripped[..start];
    let from = before.rfind([';', '{', '}']).map_or(0, |i| i + 1);
    let prefix = before[from..].trim();
    prefix.ends_with("pub") || prefix.contains("pub(")
}

#[derive(Debug, PartialEq)]
enum Tok {
    Ident(String),
    Path,
    Open,
    Close,
    Comma,
    Star,
}

fn tokenize(text: &str) -> Vec<Tok> {
    let bytes = text.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b':' if bytes.get(i + 1) == Some(&b':') => {
                toks.push(Tok::Path);
                i += 2;
            }
            b'{' => {
                toks.push(Tok::Open);
                i += 1;
            }
            b'}' => {
                toks.push(Tok::Close);
                i += 1;
            }
            b',' => {
                toks.push(Tok::Comma);
                i += 1;
            }
            b'*' => {
                toks.push(Tok::Star);
                i += 1;
            }
            b if is_ident_char(b) => {
                let mut j = i;
                toks.push(Tok::Ident(read_ident(bytes, &mut j)));
                i = j;
            }
            _ => i += 1,
        }
    }
    toks
}

/// The leaves of a `use` tree: `(segments, binding)`, the binding `None` for a
/// glob. `a::{self, b::{c as d}}` yields `([a], a)` and `([a, b, c], d)`.
pub fn parse_use_tree(text: &str) -> Vec<(Vec<String>, Option<String>)> {
    let toks = tokenize(text);
    // A leading `::` is another crate by definition.
    if toks.first() == Some(&Tok::Path) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pos = 0;
    use_tree(&toks, &mut pos, Vec::new(), &mut out);
    out
}

fn use_tree(
    toks: &[Tok],
    pos: &mut usize,
    mut path: Vec<String>,
    out: &mut Vec<(Vec<String>, Option<String>)>,
) {
    loop {
        match toks.get(*pos) {
            Some(Tok::Ident(s)) if s != "as" => {
                path.push(s.clone());
                *pos += 1;
                if toks.get(*pos) == Some(&Tok::Path) {
                    *pos += 1;
                    continue;
                }
                break;
            }
            Some(Tok::Open) => {
                *pos += 1;
                loop {
                    match toks.get(*pos) {
                        None => return,
                        Some(Tok::Close) => {
                            *pos += 1;
                            return;
                        }
                        Some(Tok::Comma) => *pos += 1,
                        Some(_) => use_tree(toks, pos, path.clone(), out),
                    }
                }
            }
            Some(Tok::Star) => {
                *pos += 1;
                out.push((path, None));
                return;
            }
            _ => break,
        }
    }
    if path.last().is_some_and(|s| s == "self") {
        path.pop();
    }
    let mut binding = path.last().cloned();
    if toks.get(*pos) == Some(&Tok::Ident("as".to_string())) {
        *pos += 1;
        if let Some(Tok::Ident(name)) = toks.get(*pos) {
            binding = Some(name.clone());
            *pos += 1;
        }
    }
    if !path.is_empty() {
        out.push((path, binding));
    }
}

/// Every multi-segment path in code: `(offset, segments)`. A path starts at an
/// identifier not itself preceded by `::`, `$` or an identifier character, and
/// ends before a turbofish, a brace group or a glob. The path of a
/// `pub(in path)` visibility names who may see an item, not a dependency, and
/// is left out.
pub fn code_paths(stripped: &str) -> Vec<(usize, Vec<String>)> {
    let bytes = stripped.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_alphabetic() || b == b'_') || (i > 0 && is_ident_char(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let preceded = i >= 1 && (bytes[i - 1] == b'$' || (i >= 2 && &bytes[i - 2..i] == b"::"));
        let start = i;
        let mut segs = vec![read_ident(bytes, &mut i)];
        while bytes.get(i) == Some(&b':')
            && bytes.get(i + 1) == Some(&b':')
            && bytes
                .get(i + 2)
                .is_some_and(|&c| c.is_ascii_alphabetic() || c == b'_')
        {
            i += 2;
            segs.push(read_ident(bytes, &mut i));
        }
        if !preceded && segs.len() > 1 && !is_visibility_path(bytes, start) {
            out.push((start, segs));
        }
    }
    out
}

/// Whether the path at `start` is the `path` of `pub(in path)`.
fn is_visibility_path(bytes: &[u8], start: usize) -> bool {
    let before = String::from_utf8_lossy(&bytes[start.saturating_sub(16)..start]);
    let before: String = before.chars().filter(|c| !c.is_whitespace()).collect();
    before.ends_with("pub(in")
}

/// `type Name<…> = rhs;` declarations: `(offset, name, rhs span)`. An
/// associated type in an `impl` is caught too, and is harmless: nothing names
/// `module::Target` from outside.
fn type_aliases(stripped: &str) -> Vec<(usize, String, (usize, usize))> {
    let bytes = stripped.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !keyword_at(bytes, i, "type") {
            i += 1;
            continue;
        }
        let at = i;
        let mut j = skip_ws(bytes, i + 4);
        let name = read_ident(bytes, &mut j);
        i += 4;
        let Some(eq) = stripped[j..].find(['=', ';']).map(|e| j + e) else {
            continue;
        };
        if name.is_empty() || bytes[eq] != b'=' {
            continue;
        }
        let end = item_end(bytes, eq);
        out.push((at, name, (eq + 1, end)));
    }
    out
}

/// Strongly connected components with more than one member, by Tarjan's
/// algorithm, each sorted, in a stable order.
pub fn cycles(edges: &BTreeSet<(String, String)>) -> Vec<Vec<String>> {
    let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (a, b) in edges {
        adjacency.entry(a).or_default().push(b);
        adjacency.entry(b).or_default();
    }
    struct State<'a> {
        index: HashMap<&'a str, usize>,
        low: HashMap<&'a str, usize>,
        stack: Vec<&'a str>,
        on_stack: HashSet<&'a str>,
        next: usize,
        out: Vec<Vec<String>>,
    }
    fn visit<'a>(v: &'a str, adj: &BTreeMap<&'a str, Vec<&'a str>>, st: &mut State<'a>) {
        st.index.insert(v, st.next);
        st.low.insert(v, st.next);
        st.next += 1;
        st.stack.push(v);
        st.on_stack.insert(v);
        for &w in &adj[v] {
            if !st.index.contains_key(w) {
                visit(w, adj, st);
                let low = st.low[v].min(st.low[w]);
                st.low.insert(v, low);
            } else if st.on_stack.contains(w) {
                let low = st.low[v].min(st.index[w]);
                st.low.insert(v, low);
            }
        }
        if st.low[v] == st.index[v] {
            let mut component = Vec::new();
            loop {
                let w = st.stack.pop().expect("v is on the stack");
                st.on_stack.remove(w);
                component.push(w.to_string());
                if w == v {
                    break;
                }
            }
            if component.len() > 1 {
                component.sort();
                st.out.push(component);
            }
        }
    }
    let mut st = State {
        index: HashMap::new(),
        low: HashMap::new(),
        stack: Vec::new(),
        on_stack: HashSet::new(),
        next: 0,
        out: Vec::new(),
    };
    for &v in adjacency.keys() {
        if !st.index.contains_key(v) {
            visit(v, &adjacency, &mut st);
        }
    }
    st.out.sort();
    st.out
}

/// Strip comments and string/char-literal contents from Rust source,
/// preserving newlines so byte offsets still map to line numbers.
///
/// One `skip_*` helper per lexical form, each returning the index just past what
/// it consumed and pushing only the newlines it swallowed.
pub fn strip_comments_and_strings(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        i = match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => skip_line_comment(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => skip_block_comment(bytes, i, &mut out),
            b'"' => skip_string(bytes, i, &mut out),
            // Raw strings: r"…", r#"…"#, br#"…"# (the `b` is consumed as a
            // normal byte before we land on the `r`).
            b'r' if !(i > 0 && is_ident_char(bytes[i - 1]) && bytes[i - 1] != b'b') => {
                skip_raw_string(bytes, i, &mut out)
            }
            // Char literal vs lifetime: 'x' / '\n' are literals; 'a is a
            // lifetime (kept — it contains no path).
            b'\'' => skip_char_literal(bytes, i, &mut out),
            b => {
                out.push(b);
                i + 1
            }
        };
    }
    String::from_utf8(out).expect("stripped source remains valid UTF-8")
}

/// `// …` to the end of the line. The newline itself is left for the caller.
fn skip_line_comment(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// `/* … */`, nested.
fn skip_block_comment(bytes: &[u8], mut i: usize, out: &mut Vec<u8>) -> usize {
    let mut depth = 1usize;
    i += 2;
    while i < bytes.len() && depth > 0 {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
        } else {
            if bytes[i] == b'\n' {
                out.push(b'\n');
            }
            i += 1;
        }
    }
    i
}

/// `"…"`, honouring backslash escapes — including a `\` line continuation,
/// whose newline is kept so a violation below it reports the right line.
fn skip_string(bytes: &[u8], mut i: usize, out: &mut Vec<u8>) -> usize {
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    out.push(b'\n');
                }
                i += 2;
            }
            b'"' => return i + 1,
            b'\n' => {
                out.push(b'\n');
                i += 1;
            }
            _ => i += 1,
        }
    }
    i
}

/// `r"…"` / `r#"…"#`. Not a raw string after all (a bare `r` identifier) → emit
/// the `r` and move on.
fn skip_raw_string(bytes: &[u8], i: usize, out: &mut Vec<u8>) -> usize {
    let mut j = i + 1;
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'"') {
        out.push(b'r');
        return i + 1;
    }
    let hashes = j - (i + 1);
    let mut close = vec![b'"'];
    close.extend(std::iter::repeat(b'#').take(hashes));
    let mut at = j + 1;
    while at < bytes.len() && bytes[at..].len() >= close.len() {
        if bytes[at..at + close.len()] == close[..] {
            return at + close.len();
        }
        if bytes[at] == b'\n' {
            out.push(b'\n');
        }
        at += 1;
    }
    at
}

/// `'x'` / `'\n'` are literals and are consumed; `'a` is a lifetime and the
/// quote is kept, since a lifetime contains no path.
fn skip_char_literal(bytes: &[u8], mut i: usize, out: &mut Vec<u8>) -> usize {
    if bytes.get(i + 1) == Some(&b'\\') {
        i += 3;
        while i < bytes.len() && bytes[i] != b'\'' {
            i += 1;
        }
        return i + 1;
    }
    if bytes.get(i + 2) == Some(&b'\'') && bytes.get(i + 1) != Some(&b'\'') {
        return i + 3;
    }
    out.push(b'\'');
    i + 1
}
