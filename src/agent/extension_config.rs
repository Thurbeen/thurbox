//! Loading of extension manifests from the discovery directory.
//!
//! Opt-in extensions (see `extensions/<name>/`) drop an `extension.toml`
//! manifest into `~/.config/thurbox/extensions/<name>.toml` from their own
//! installer. thurbox core reads any manifest there without knowing the
//! extension by name (ADR-20: extensions are data + scripts, never embedded).
//!
//! Unlike `agents.toml`/`hosts.toml` this file is **never seeded** — a fresh
//! install has no extensions, and the directory only appears once a user runs an
//! extension's installer. Read/parse errors degrade gracefully (a bad manifest
//! is skipped with a warning, never aborting startup).

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;

use crate::session::{AgentDef, AgentPatch, ExtensionDef};

/// Raw-content root of the thurbox repo (no git ref).
const OFFICIAL_REPO_RAW: &str = "https://raw.githubusercontent.com/Thurbeen/thurbox";

/// The running binary's version string (e.g. `0.113.0`, or `0.0.0-dev` for a
/// development build), injected at compile time by `build.rs`. The reference
/// point for extension staleness + compatibility checks.
pub fn binary_version() -> &'static str {
    env!("THURBOX_VERSION")
}

/// Whether this is an unstable development build (its version doesn't order
/// against release tags, so version checks are skipped for it).
pub fn is_dev_build() -> bool {
    cfg!(dev_build) || crate::session::extension_def::is_dev_version(binary_version())
}

/// The git ref to fetch official extensions from: the running binary's release
/// tag (`v0.7.0`), so a fetched extension matches the binary that reads it. Dev
/// builds track `main`.
pub fn official_ref() -> String {
    if is_dev_build() {
        "main".to_string()
    } else {
        format!("v{}", binary_version())
    }
}

/// Base URL for one of the repo's officially-distributed sets, pinned to this
/// binary's release tag.
///
/// Two sets use it: `extensions/` and `examples/panes/`. Pinning to the tag is the
/// property worth keeping in both cases — an extension calls `thurbox-cli` and a
/// pane reads `thurbox.*`, and both are contracts that move, so what the official
/// source hands over should match the binary asking for it.
pub fn official_set_base(folder: &str) -> String {
    official_set_base_at(folder, &official_ref())
}

/// [`official_set_base`] at an explicit git ref.
///
/// What a lockfile needs: an entry recorded at `v2.0.0` has to keep resolving to
/// `v2.0.0` after the binary moves on, or applying the same spec elsewhere would
/// deliver whatever is newest — which is the property a lock exists to provide.
pub fn official_set_base_at(folder: &str, git_ref: &str) -> String {
    format!("{OFFICIAL_REPO_RAW}/{git_ref}/{folder}")
}

/// Base URL for the official extensions shipped in the thurbox repo, pinned to
/// this binary's version. A bare `thurbox-cli extension install <name>` resolves
/// to `<official_base()>/<name>`.
pub fn official_base() -> String {
    official_set_base("extensions")
}

/// One officially-distributed extension, surfaced for discovery
/// (`thurbox-cli extension available`) and typo help on a failed bare-name
/// install. Each installs by its bare `name` against [`official_base`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfficialExtension {
    /// Bare install name (`thurbox-cli extension install <name>`).
    pub name: &'static str,
    /// One-line human summary, mirrored from the extension's own manifest.
    pub description: &'static str,
}

/// The official extensions shipped in `extensions/<name>/` of the thurbox repo.
///
/// **Source of truth for discovery + typo suggestions.** Keep in sync when an
/// extension is added/removed under `extensions/` (descriptions mirror each
/// `extension.toml`'s `description`). This is intentionally a small static list
/// rather than a remote index so `extension available` works offline and a typo
/// can be caught without a network round-trip.
pub const OFFICIAL_EXTENSIONS: &[OfficialExtension] = &[
    OfficialExtension {
        name: "flow",
        description: "Focus-protecting triage agent",
    },
    OfficialExtension {
        name: "forge",
        description: "Workflow analyst that proposes new automations",
    },
    OfficialExtension {
        name: "ci-shepherd",
        description: "Watches change requests (PR/MR) and dispatches CI/review fixers",
    },
    OfficialExtension {
        name: "renovate",
        description: "Keeps local repos on up-to-date dependencies via Renovate's local platform",
    },
];

/// Whether an install `target` is a **bare name** (not a URL or path) — i.e. it
/// resolves against the official source. Mirrors the branch in [`resolve_source`].
pub fn is_bare_name(target: &str) -> bool {
    let t = target.trim();
    !(t.starts_with("https://")
        || t.starts_with("http://")
        || t.contains('/')
        || t.starts_with('.')
        || t.starts_with('~'))
}

/// Suggest the closest official extension name to `name` (for a "did you mean?"
/// hint), within a small edit-distance budget so unrelated typos suggest nothing.
pub fn suggest_extension(name: &str) -> Option<&'static str> {
    let name = name.trim().to_lowercase();
    // Budget scales a little with length: tolerate a couple of edits for short
    // names (a transposition is 2), more for longer ones, but never so loose that
    // everything matches.
    let budget = (name.len() / 3).clamp(2, 3);
    OFFICIAL_EXTENSIONS
        .iter()
        .map(|e| (e.name, levenshtein(&name, e.name)))
        .filter(|(_, d)| *d <= budget)
        .min_by_key(|(_, d)| *d)
        .map(|(n, _)| n)
}

/// Build a helpful error for a failed **bare-name** install: surface the real
/// fetch failure, list the known official extensions, and offer a "did you
/// mean?" suggestion plus a pointer at `extension available`.
pub fn unknown_extension_help(name: &str, cause: &str) -> String {
    let mut msg = format!("could not install extension '{name}': {cause}\n");
    if let Some(suggestion) = suggest_extension(name) {
        msg.push_str(&format!("\nDid you mean '{suggestion}'?\n"));
    }
    msg.push_str("\nKnown official extensions:\n");
    for ext in OFFICIAL_EXTENSIONS {
        msg.push_str(&format!("  {:<12} {}\n", ext.name, ext.description));
    }
    msg.push_str(
        "\nRun `thurbox-cli extension available` to list them, or pass a URL / local path.",
    );
    msg
}

/// Classic Levenshtein edit distance (two-row DP). Small inputs only.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The extension-manifest discovery directory:
/// `~/.config/thurbox/extensions/` (sibling of `config.toml`).
pub fn extensions_dir() -> Option<PathBuf> {
    crate::paths::config_file().map(|p| p.with_file_name("extensions"))
}

/// Path to a single extension's manifest: `<extensions_dir>/<name>.toml`.
pub fn manifest_path(name: &str) -> Option<PathBuf> {
    extensions_dir().map(|d| d.join(format!("{name}.toml")))
}

/// Default install home for an extension: `<extensions_dir>/<name>/` (a sibling
/// dir of its `<name>.toml` manifest, e.g. `~/.config/thurbox/extensions/flow`).
/// Used when neither `--home` nor a manifest `home` is given. Discovery
/// (`list_manifests_with_warnings`) only reads `*.toml`, so this dir is ignored.
pub fn default_home(name: &str) -> Option<PathBuf> {
    extensions_dir().map(|d| d.join(name))
}

/// Load one extension manifest by name. Returns `None` when the file is absent,
/// unreadable, or malformed (with a logged warning) — callers treat a missing
/// manifest as "this extension isn't installed".
pub fn load_manifest(name: &str) -> Option<ExtensionDef> {
    let (def, warnings) = load_manifest_with_warnings(name);
    for w in &warnings {
        tracing::warn!("{w}");
    }
    def
}

/// [`load_manifest`], also returning user-facing warnings (unknown fields,
/// parse errors) so the TUI/CLI can surface them.
pub fn load_manifest_with_warnings(name: &str) -> (Option<ExtensionDef>, Vec<String>) {
    let Some(path) = manifest_path(name) else {
        return (None, vec!["Could not resolve extensions directory".into()]);
    };
    if !path.exists() {
        return (None, Vec::new());
    }
    let label = format!("{name}.toml");
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            match super::agent_config::parse_toml_reporting_unknown::<ExtensionDef>(
                &contents, &label,
            ) {
                Ok((def, warnings)) => (Some(def), warnings),
                Err(e) => (
                    None,
                    vec![format!(
                        "{label}: {}; extension skipped",
                        super::agent_config::compact_toml_error(&e.to_string())
                    )],
                ),
            }
        }
        Err(e) => (None, vec![format!("Failed to read {label}: {e}")]),
    }
}

/// Load every installed extension manifest, in filename order. Malformed
/// manifests are skipped (their warnings logged), never aborting the scan.
pub fn list_manifests() -> Vec<ExtensionDef> {
    let (defs, warnings) = list_manifests_with_warnings();
    for w in &warnings {
        tracing::warn!("{w}");
    }
    defs
}

/// [`list_manifests`], also returning accumulated warnings.
pub fn list_manifests_with_warnings() -> (Vec<ExtensionDef>, Vec<String>) {
    let Some(dir) = extensions_dir() else {
        return (
            Vec::new(),
            vec!["Could not resolve extensions directory".into()],
        );
    };
    if !dir.exists() {
        return (Vec::new(), Vec::new());
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            return (
                Vec::new(),
                vec![format!("Failed to read extensions dir: {e}")],
            )
        }
    };
    // Collect + sort filenames so the scan order is deterministic across runs.
    let mut stems: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(OsStr::to_str) == Some("toml"))
        .filter_map(|p| p.file_stem().and_then(OsStr::to_str).map(String::from))
        .collect();
    stems.sort();

    let mut defs = Vec::new();
    let mut warnings = Vec::new();
    for stem in stems {
        let (def, mut warns) = load_manifest_with_warnings(&stem);
        warnings.append(&mut warns);
        if let Some(def) = def {
            defs.push(def);
        }
    }
    (defs, warnings)
}

// --- install: source resolution, fetching, agents.toml + manifest writes ----

/// Where an extension's files come from during install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionSource {
    /// A base URL (no trailing slash); files fetched via `<base>/<rel>`.
    Remote(String),
    /// A local directory containing `extension.toml` + payload files.
    Local(PathBuf),
    /// A version-control repository, obtained as a working copy rather than
    /// fetched file by file.
    ///
    /// The only form that can deliver bytes: [`fetch_file`] returns a `String` and
    /// decodes remote output lossily, so a binary through it is corrupted rather
    /// than refused. A clone carries whatever the repository holds, preserves its
    /// layout, and identifies exactly what it delivered.
    Git(String),
}

/// Resolve an install target into a source:
/// - `http(s)://…` → that base URL,
/// - a path-like target (`/abs`, `./rel`, `~/x`, anything with a `/`) → local dir,
/// - a bare name (`flow`) → the official remote `<official_base()>/<name>`.
pub fn resolve_source(target: &str) -> ExtensionSource {
    resolve_source_in(target, "extensions")
}

/// [`resolve_source`], for a named set within the official repo.
///
/// Interface plugins resolve the same way against `examples/panes/`, so a person
/// learns one vocabulary — bare name, URL, path — and it covers both kinds of
/// thing they can install. Only where a *bare name* points differs.
pub fn resolve_source_in(target: &str, folder: &str) -> ExtensionSource {
    let t = target.trim();
    // A repository, recognised EXPLICITLY and never guessed. Three unambiguous
    // forms: a `git+` prefix, a `.git` suffix, or the scp-like `git@host:path`.
    //
    // A bare `https://github.com/owner/repo` deliberately does NOT clone. That
    // spelling already means "a base URL to fetch manifest-named files from", and
    // reinterpreting it by host would change the meaning of every existing install
    // for one forge while leaving it alone for others. `git+` is the escape hatch
    // for a repository URL with no `.git` suffix — cargo's spelling, for this
    // reason.
    if let Some(url) = git_url(t) {
        return ExtensionSource::Git(url);
    }
    if let Some(rest) = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
    {
        let scheme = if t.starts_with("https://") {
            "https://"
        } else {
            "http://"
        };
        return ExtensionSource::Remote(format!("{scheme}{}", rest.trim_end_matches('/')));
    }
    // Looks like a path (not a bare extension name)? Detect `/` and `.`/`~`
    // prefixes, plus Windows forms: a `\` separator or an absolute drive path
    // (`C:\…`), which carry neither a `/` nor a leading `.`.
    let looks_local = t.contains('/')
        || t.contains('\\')
        || t.starts_with('.')
        || t.starts_with('~')
        || std::path::Path::new(t).is_absolute();
    if looks_local {
        return ExtensionSource::Local(expand_tilde(t));
    }
    ExtensionSource::Remote(format!("{}/{t}", official_set_base(folder)))
}

/// The repository URL a target names, or `None` when it does not name one.
///
/// The `git+` prefix is stripped: it is thurbox's marker, not part of the URL git
/// is handed.
pub fn git_url(target: &str) -> Option<String> {
    let t = target.trim();
    if let Some(rest) = t.strip_prefix("git+") {
        return (!rest.is_empty()).then(|| rest.to_string());
    }
    if t.ends_with(".git") {
        return Some(t.to_string());
    }
    // scp-like: `git@host:path`. Required to have both a host and a path, so a bare
    // `git@host` is not mistaken for one.
    if let Some(rest) = t.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            if !host.is_empty() && !path.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Expand a leading `~/` (or bare `~`) to the user's home directory. Shared by
/// the source resolver and the installer so both agree on home expansion.
/// Delegates to [`crate::paths::expand_tilde`] — the single implementation —
/// which additionally honours `~\` on Windows.
pub fn expand_tilde(path: &str) -> PathBuf {
    crate::paths::expand_tilde(path)
}

/// Fetch one file's text content from a source (`<source>/<rel>`).
pub fn fetch_file(source: &ExtensionSource, rel: &str) -> Result<String, String> {
    match source {
        ExtensionSource::Local(dir) => {
            let p = dir.join(rel);
            std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))
        }
        ExtensionSource::Remote(base) => http_get(&format!("{base}/{rel}")),
        // A repository is obtained as a working copy, not read file by file — that
        // is the whole point of the form. Reaching here means a caller took the
        // fetch path for a git source, which is a bug at that call site.
        ExtensionSource::Git(url) => Err(format!(
            "{url} is a repository: it is cloned, not fetched file by file"
        )),
    }
}

/// Run the first of `attempts` (a `(binary, args)` list) that succeeds,
/// returning its raw stdout. On failure the aggregated cause is surfaced: a
/// missing tool is distinguished from a tool that ran but failed (e.g. HTTP 404
/// for a misspelled name), whose last stderr line is included. Shared by the
/// `curl`-then-`wget` downloaders below; the caller adds the URL context.
fn run_first_ok(attempts: &[(&str, Vec<&str>)]) -> Result<Vec<u8>, String> {
    let mut errors = Vec::new();
    for (bin, args) in attempts {
        match Command::new(bin).args(args).output() {
            Ok(out) if out.status.success() => return Ok(out.stdout),
            Ok(out) => {
                // The tool ran but failed (404, DNS, refused…) — keep its stderr.
                let stderr = String::from_utf8_lossy(&out.stderr);
                let detail = stderr.trim();
                let detail = if detail.is_empty() {
                    format!("exit {}", out.status)
                } else {
                    detail.lines().last().unwrap_or(detail).to_string()
                };
                errors.push(format!("{bin}: {detail}"));
            }
            Err(_) => errors.push(format!("{bin}: not found")),
        }
    }
    Err(errors.join("; "))
}

/// HTTP GET to stdout via `curl` (falling back to `wget`) — same dependency-free
/// approach as the shell installer, both run with a timeout.
///
/// Note: the body is decoded as UTF-8 (lossy); extension payloads are expected
/// to be text files (specs, scripts, JSON), not binaries.
pub(crate) fn http_get(url: &str) -> Result<String, String> {
    let attempts: [(&str, Vec<&str>); 2] = [
        ("curl", vec!["-fsSL", "--max-time", "30", url]),
        ("wget", vec!["--timeout=30", "-O", "-", url]),
    ];
    run_first_ok(&attempts)
        .map(|stdout| String::from_utf8_lossy(&stdout).into_owned())
        .map_err(|e| format!("failed to fetch {url} ({e})"))
}

/// Download a URL's raw bytes to `dest` via `curl` (falling back to `wget`) —
/// the bytes sibling of [`http_get`], for binary artifacts (release tarballs)
/// that must not be UTF-8-decoded. Same approach, but a longer timeout (a
/// tarball is larger than a text file).
pub(crate) fn http_get_to_file(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| format!("non-UTF-8 destination path: {}", dest.display()))?;
    let attempts: [(&str, Vec<&str>); 2] = [
        (
            "curl",
            vec!["-fsSL", "--max-time", "120", "-o", dest_str, url],
        ),
        ("wget", vec!["--timeout=120", "-O", dest_str, url]),
    ];
    run_first_ok(&attempts)
        .map(|_| ())
        .map_err(|e| format!("failed to download {url} ({e})"))
}

/// Fetch + parse the `extension.toml` manifest from a source.
pub fn load_manifest_from_source(
    source: &ExtensionSource,
) -> Result<(ExtensionDef, Vec<String>), String> {
    let contents = fetch_file(source, "extension.toml")?;
    super::agent_config::parse_toml_reporting_unknown::<ExtensionDef>(&contents, "extension.toml")
        .map_err(|e| {
            format!(
                "extension.toml: {}",
                super::agent_config::compact_toml_error(&e.to_string())
            )
        })
}

/// Register agents in `agents.toml`, appending only the ones whose names aren't
/// already present (so user edits and existing agents are preserved — the file's
/// comments/formatting stay intact because we append text rather than rewrite).
/// Returns the names actually added.
pub fn ensure_agents_registered(agents: &[AgentDef]) -> Result<Vec<String>, String> {
    if agents.is_empty() {
        return Ok(Vec::new());
    }
    // load_or_seed writes the built-in file if absent, so the path then exists.
    let existing_reg = super::agent_config::load_or_seed();
    let Some(path) = super::agent_config::agents_config_path() else {
        return Err("could not resolve agents.toml path".into());
    };
    let mut text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;

    let missing: Vec<&AgentDef> = agents
        .iter()
        .filter(|a| existing_reg.get(&a.name).is_none())
        .collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }

    #[derive(Serialize)]
    struct AgentsDoc<'a> {
        agents: Vec<&'a AgentDef>,
    }
    let block = toml::to_string(&AgentsDoc {
        agents: missing.clone(),
    })
    .map_err(|e| format!("serialize agents: {e}"))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&block);
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;

    Ok(missing.iter().map(|a| a.name.clone()).collect())
}

/// Remove `[[agents]]` entries from `agents.toml` by name, preserving the rest
/// of the file (other agents, comments, top-level keys) by editing text rather
/// than re-serializing. The reverse of [`ensure_agents_registered`], used by
/// extension uninstall. Returns the names actually removed.
///
/// Caveat: removal is by name, so an agent a user re-pointed at a different CLI
/// but kept the name is still removed. Names are namespaced per extension
/// (`flow`, `flow-worker`, …) to make collisions unlikely.
pub fn remove_agents_from_toml(names: &[String]) -> Result<Vec<String>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let Some(path) = super::agent_config::agents_config_path() else {
        return Err("could not resolve agents.toml path".into());
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;

    let lines: Vec<&str> = text.lines().collect();
    let (out, removed) = strip_agent_blocks(&lines, names);

    if removed.is_empty() {
        return Ok(Vec::new());
    }
    let mut new_text = out.join("\n");
    if text.ends_with('\n') {
        new_text.push('\n');
    }
    std::fs::write(&path, new_text).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(removed)
}

/// Append each patch's `append_args` to the matching agents' `args` in
/// `agents.toml`, idempotently and preserving comments/formatting (toml_edit).
/// A patch matches the agent literally named `patch.name` **and** every custom
/// agent that declared `hook_schema = "<patch.name>"` — so a rebranded-claude
/// agent picks up the same `--settings` wiring as the built-in `claude`. Unlike
/// [`ensure_agents_registered`] (which only adds *new* agents), this extends
/// agents the extension does not own. Agents not present are skipped. Returns
/// the names actually patched (one entry per matched agent).
pub fn apply_agent_patches(patches: &[AgentPatch]) -> Result<Vec<String>, String> {
    edit_agent_patches(patches, true)
}

/// Reverse of [`apply_agent_patches`]: remove each patch's `append_args`
/// subsequence from the named agent's `args`. Returns the names actually changed.
pub fn remove_agent_patches(patches: &[AgentPatch]) -> Result<Vec<String>, String> {
    edit_agent_patches(patches, false)
}

fn edit_agent_patches(patches: &[AgentPatch], add: bool) -> Result<Vec<String>, String> {
    use toml_edit::DocumentMut;

    if patches.is_empty() {
        return Ok(Vec::new());
    }
    // Ensure the file exists (seeds built-ins if absent).
    super::agent_config::load_or_seed();
    let Some(path) = super::agent_config::agents_config_path() else {
        return Err("could not resolve agents.toml path".into());
    };
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    let Some(agents) = doc
        .get_mut("agents")
        .and_then(|i| i.as_array_of_tables_mut())
    else {
        return Ok(Vec::new());
    };

    let mut changed = Vec::new();
    for patch in patches.iter().filter(|p| !p.append_args.is_empty()) {
        // A patch targets its named built-in agent AND every custom agent that
        // declared `hook_schema = "<patch.name>"` — so a rebranded-claude agent
        // gets the same `--settings` wiring. No `break`: fan out to all matches.
        for table in agents.iter_mut() {
            if let Some(name) = patch_one_agent(table, patch, add) {
                changed.push(name);
            }
        }
    }

    if !changed.is_empty() {
        std::fs::write(&path, doc.to_string())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(changed)
}

/// Add or remove one patch's `append_args` on one `[[agents]]` table.
///
/// `Some(name)` when the table was actually changed — a patch already applied (or
/// already absent) is a no-op, which is what makes install/uninstall idempotent.
fn patch_one_agent(table: &mut toml_edit::Table, patch: &AgentPatch, add: bool) -> Option<String> {
    use toml_edit::Array;

    // Snapshot the identity fields into owned strings first: the args mutation
    // below reborrows `table` mutably.
    let agent_name = table
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let hook_schema = table
        .get("hook_schema")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let targeted = agent_name.as_deref() == Some(patch.name.as_str())
        || hook_schema.as_deref() == Some(patch.name.as_str());
    // A hook_schema match with no `name` can't be patched — skip it.
    let agent_name = agent_name.filter(|_| targeted)?;

    if table.get("args").is_none() {
        table["args"] = toml_edit::value(Array::new());
    }
    let args = table.get_mut("args").and_then(|i| i.as_array_mut())?;
    let current: Vec<String> = args
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let present = contains_subsequence(&current, &patch.append_args);
    if add && !present {
        for a in &patch.append_args {
            args.push(a.as_str());
        }
        return Some(agent_name);
    }
    if !add && present {
        let mut arr = Array::new();
        for a in remove_subsequence(&current, &patch.append_args) {
            arr.push(a.as_str());
        }
        table["args"] = toml_edit::value(arr);
        return Some(agent_name);
    }
    None
}

/// Whether `hay` contains `needle` as a contiguous subsequence.
fn contains_subsequence(hay: &[String], needle: &[String]) -> bool {
    if needle.is_empty() {
        return true;
    }
    needle.len() <= hay.len() && hay.windows(needle.len()).any(|w| w == needle)
}

/// `hay` with the first contiguous occurrence of `needle` removed.
fn remove_subsequence(hay: &[String], needle: &[String]) -> Vec<String> {
    if needle.is_empty() {
        return hay.to_vec();
    }
    if let Some(pos) = hay.windows(needle.len()).position(|w| w == needle) {
        let mut out = hay[..pos].to_vec();
        out.extend_from_slice(&hay[pos + needle.len()..]);
        out
    } else {
        hay.to_vec()
    }
}

/// Walk `lines`, dropping every `[[agents]]` block whose `name` is in `names`
/// (with a single trailing blank separator). Returns the kept lines and the
/// names actually removed.
fn strip_agent_blocks<'a>(lines: &[&'a str], names: &[String]) -> (Vec<&'a str>, Vec<String>) {
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut removed = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].trim_start().starts_with("[[agents]]") {
            out.push(lines[i]);
            i += 1;
            continue;
        }
        i = take_agent_block(lines, i, names, &mut out, &mut removed);
    }
    (out, removed)
}

/// Handle a single `[[agents]]` block starting at `start`: either keep it
/// (append to `out`) or drop it (record the name in `removed`). Returns the
/// index to resume scanning from.
fn take_agent_block<'a>(
    lines: &[&'a str],
    start: usize,
    names: &[String],
    out: &mut Vec<&'a str>,
    removed: &mut Vec<String>,
) -> usize {
    // Collect this block: from the header to just before the next
    // top-level table header (`[` at the start of a trimmed line) or EOF.
    let end = agent_block_end(lines, start);
    let block = &lines[start..end];
    let target = block
        .iter()
        .find_map(|l| parse_agent_name(l))
        .filter(|name| names.iter().any(|n| n == name));
    let Some(name) = target else {
        out.extend_from_slice(block);
        return end;
    };
    removed.push(name);
    // Drop the block and a single trailing blank separator line.
    if end < lines.len() && lines[end].trim().is_empty() {
        end + 1
    } else {
        end
    }
}

/// Index just past the `[[agents]]` block starting at `start` — the next line
/// whose trimmed form begins a top-level table header (`[`), or EOF.
fn agent_block_end(lines: &[&str], start: usize) -> usize {
    let mut j = start + 1;
    while j < lines.len() && !lines[j].trim_start().starts_with('[') {
        j += 1;
    }
    j
}

/// Parse `name = "x"` out of a TOML line, returning the value.
fn parse_agent_name(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("name")?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let inner = rest.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

/// Write a manifest to the discovery dir (`<extensions_dir>/<name>.toml`),
/// creating the directory if needed. Returns the path written.
pub fn write_manifest(def: &ExtensionDef) -> Result<PathBuf, String> {
    let Some(path) = manifest_path(&def.name) else {
        return Err("could not resolve extensions directory".into());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let text = toml::to_string(def).map_err(|e| format!("serialize manifest: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Remove an extension's manifest from the discovery dir. Returns whether a file
/// was actually removed.
pub fn remove_manifest_file(name: &str) -> Result<bool, String> {
    let Some(path) = manifest_path(name) else {
        return Err("could not resolve extensions directory".into());
    };
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare-name install resolves against the official source, so a name in
    /// [`OFFICIAL_EXTENSIONS`] with no `extensions/<name>/` behind it advertises
    /// something `extension available` can list and `extension install` cannot
    /// fetch.
    #[test]
    fn every_official_extension_has_a_shipped_manifest() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions");
        for ext in OFFICIAL_EXTENSIONS {
            assert!(
                is_bare_name(ext.name),
                "'{}' must install by bare name",
                ext.name
            );
            assert!(
                root.join(ext.name).join("extension.toml").is_file(),
                "official extension '{}' has no extensions/{}/extension.toml",
                ext.name,
                ext.name
            );
        }
    }

    fn write_manifest_file(name: &str, contents: &str) {
        let path = manifest_path(name).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
    }

    #[test]
    fn missing_manifest_is_none_without_warning() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let (def, warnings) = load_manifest_with_warnings("flow");
        assert!(def.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn loads_a_valid_manifest() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        write_manifest_file(
            "flow",
            "name = \"flow\"\n[[sessions]]\nname = \"flow\"\nagent = \"flow\"\nrepo_path = \"/home/me/flow\"\n",
        );
        let def = load_manifest("flow").unwrap();
        assert_eq!(def.name, "flow");
        assert_eq!(def.sessions.len(), 1);
    }

    #[test]
    fn malformed_manifest_is_skipped_with_warning() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        write_manifest_file("flow", "this is not = valid toml {{{");
        let (def, warnings) = load_manifest_with_warnings("flow");
        assert!(def.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("flow.toml"));
    }

    #[test]
    fn unknown_field_warns_but_keeps_manifest() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        write_manifest_file("flow", "name = \"flow\"\nbogus = 1\n");
        let (def, warnings) = load_manifest_with_warnings("flow");
        assert_eq!(def.unwrap().name, "flow");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("bogus"));
    }

    #[test]
    fn lists_manifests_in_sorted_order() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        write_manifest_file("zed", "name = \"zed\"\n");
        write_manifest_file("flow", "name = \"flow\"\n");
        let names: Vec<String> = list_manifests().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["flow", "zed"]);
    }

    #[test]
    fn lists_nothing_when_dir_absent() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        assert!(list_manifests().is_empty());
    }

    #[test]
    fn is_bare_name_only_for_plain_names() {
        assert!(is_bare_name("flow"));
        assert!(is_bare_name("ci-shepherd"));
        assert!(!is_bare_name("https://example.com/flow"));
        assert!(!is_bare_name("./flow"));
        assert!(!is_bare_name("~/flow"));
        assert!(!is_bare_name("/abs/flow"));
        assert!(!is_bare_name("a/b"));
    }

    #[test]
    fn suggest_extension_catches_typos_but_not_noise() {
        assert_eq!(suggest_extension("flwo"), Some("flow"));
        assert_eq!(suggest_extension("forge"), Some("forge"));
        assert_eq!(suggest_extension("renovat"), Some("renovate"));
        assert_eq!(suggest_extension("ci-shepard"), Some("ci-shepherd"));
        // Unrelated input shouldn't map to anything.
        assert_eq!(suggest_extension("zzzzzzzzzz"), None);
    }

    #[test]
    fn unknown_extension_help_lists_known_and_suggests() {
        let msg = unknown_extension_help("flwo", "curl: HTTP 404");
        assert!(msg.contains("could not install extension 'flwo'"));
        assert!(msg.contains("curl: HTTP 404"));
        assert!(msg.contains("Did you mean 'flow'?"));
        // Every official extension is listed for discovery.
        for ext in OFFICIAL_EXTENSIONS {
            assert!(msg.contains(ext.name), "missing {}", ext.name);
        }
        assert!(msg.contains("extension available"));
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("flow", "flow"), 0);
        assert_eq!(levenshtein("flwo", "flow"), 2);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    /// The three repository forms, and — the load-bearing half — that nothing else
    /// became one.
    ///
    /// A bare `https://…` URL silently reinterpreted as a clone would change the
    /// meaning of every install that works today, which is why recognition is
    /// explicit rather than guessed from the host.
    #[test]
    fn a_repository_source_is_recognised_only_in_its_explicit_forms() {
        for (target, expected) in [
            // `git+` prefix, stripped from what git is handed.
            (
                "git+https://github.com/you/thurbox-widget",
                "https://github.com/you/thurbox-widget",
            ),
            // A `.git` suffix speaks for itself.
            (
                "https://github.com/you/thurbox-widget.git",
                "https://github.com/you/thurbox-widget.git",
            ),
            // scp-like.
            (
                "git@github.com:you/thurbox-widget.git",
                "git@github.com:you/thurbox-widget.git",
            ),
        ] {
            match resolve_source_in(target, "examples/panes") {
                ExtensionSource::Git(url) => assert_eq!(url, expected, "{target}"),
                other => panic!("{target} should be a repository, got {other:?}"),
            }
        }

        // And these must keep the meanings they already have.
        assert!(matches!(
            resolve_source_in("https://github.com/you/thurbox-widget", "examples/panes"),
            ExtensionSource::Remote(_)
        ));
        assert!(matches!(
            resolve_source_in("./local/pane", "examples/panes"),
            ExtensionSource::Local(_)
        ));
        assert!(matches!(
            resolve_source_in("tasks", "examples/panes"),
            ExtensionSource::Remote(_)
        ));
    }

    #[test]
    fn a_near_miss_is_not_a_repository() {
        // `git@host` with no path, and a bare `git+`, are not sources.
        assert_eq!(git_url("git@github.com"), None);
        assert_eq!(git_url("git+"), None);
        // A directory merely named `.github` is not a `.git` suffix.
        assert_eq!(git_url("./.github"), None);
        // A path that happens to contain the word.
        assert_eq!(git_url("./my-git-panes"), None);
    }

    /// A repository must never be read through the text fetch path: that is where a
    /// binary is corrupted rather than refused, so the mistake is made loud.
    #[test]
    fn fetching_a_file_from_a_repository_is_refused() {
        let error = fetch_file(
            &ExtensionSource::Git("https://example.com/x.git".into()),
            "plugin.toml",
        )
        .expect_err("should refuse");
        assert!(error.contains("cloned"), "{error}");
    }

    #[test]
    fn resolve_source_distinguishes_name_url_and_path() {
        assert_eq!(
            resolve_source("flow"),
            ExtensionSource::Remote(format!("{}/flow", official_base()))
        );
        // Official base is pinned to a concrete ref (a tag or main), never bare.
        assert!(official_base().starts_with(OFFICIAL_REPO_RAW));
        assert!(official_base().ends_with("/extensions"));
        assert_eq!(
            resolve_source("https://example.com/ext/foo/"),
            ExtensionSource::Remote("https://example.com/ext/foo".into())
        );
        assert_eq!(
            resolve_source("./extensions/flow"),
            ExtensionSource::Local(PathBuf::from("./extensions/flow"))
        );
        assert_eq!(
            resolve_source("/abs/flow"),
            ExtensionSource::Local(PathBuf::from("/abs/flow"))
        );
    }

    #[test]
    fn fetch_and_load_manifest_from_local_source() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("extension.toml"),
            "name = \"flow\"\nhome = \"~/flow\"\n[[files]]\npath = \"FLOW.md\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("FLOW.md"), "spec body").unwrap();

        let src = ExtensionSource::Local(dir.path().to_path_buf());
        assert_eq!(fetch_file(&src, "FLOW.md").unwrap(), "spec body");
        let (def, warnings) = load_manifest_from_source(&src).unwrap();
        assert_eq!(def.name, "flow");
        assert_eq!(def.home.as_deref(), Some("~/flow"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn ensure_agents_registered_appends_only_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let flow = AgentDef {
            name: "flow".into(),
            command: "claude".into(),
            args: vec!["--model".into(), "haiku".into()],
            resume_args: vec![],
            fork_args: vec![],
            new_session_args: vec![],
            resume_latest: false,
            hook_schema: None,
        };
        // claude already exists in the seeded built-ins → not re-added.
        let claude = AgentDef {
            name: "claude".into(),
            command: "claude".into(),
            ..flow.clone()
        };

        let added = ensure_agents_registered(&[flow.clone(), claude]).unwrap();
        assert_eq!(added, ["flow"]);

        let reg = super::super::agent_config::load_or_seed();
        assert_eq!(reg.get("flow").unwrap().command, "claude");
        assert_eq!(reg.get("flow").unwrap().args, ["--model", "haiku"]);

        // Idempotent: a second call adds nothing.
        assert!(ensure_agents_registered(&[flow]).unwrap().is_empty());
    }

    #[test]
    fn remove_agents_preserves_other_entries_and_comments() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let flow = AgentDef {
            name: "flow".into(),
            command: "claude".into(),
            args: vec![],
            resume_args: vec![],
            fork_args: vec![],
            new_session_args: vec![],
            resume_latest: false,
            hook_schema: None,
        };
        ensure_agents_registered(&[flow]).unwrap();
        // Sanity: flow + the seeded built-ins are present.
        assert!(super::super::agent_config::load_or_seed()
            .get("flow")
            .is_some());

        let removed = remove_agents_from_toml(&["flow".to_string()]).unwrap();
        assert_eq!(removed, ["flow"]);

        let reg = super::super::agent_config::load_or_seed();
        assert!(reg.get("flow").is_none(), "flow removed");
        // Built-ins (and the seed file's header comment) survive.
        assert!(reg.get("claude").is_some(), "other agents preserved");
        let text =
            std::fs::read_to_string(super::super::agent_config::agents_config_path().unwrap())
                .unwrap();
        assert!(text.contains('#'), "header comments preserved");

        // Removing a non-existent agent is a no-op.
        assert!(remove_agents_from_toml(&["ghost".to_string()])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn agent_patches_apply_idempotently_and_reverse() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let patch = AgentPatch {
            name: "claude".into(),
            append_args: vec!["--settings".into(), "/x/hooks.json".into()],
        };

        // Applies to the seeded built-in `claude`.
        let patched = apply_agent_patches(std::slice::from_ref(&patch)).unwrap();
        assert_eq!(patched, ["claude"]);
        let reg = super::super::agent_config::load_or_seed();
        let args = &reg.get("claude").unwrap().args;
        assert!(
            args.windows(2)
                .any(|w| w == ["--settings".to_string(), "/x/hooks.json".to_string()]),
            "args carry the injected flag: {args:?}"
        );

        // Idempotent: re-applying changes nothing.
        assert!(apply_agent_patches(std::slice::from_ref(&patch))
            .unwrap()
            .is_empty());

        // Reverse removes exactly the injected subsequence.
        let removed = remove_agent_patches(std::slice::from_ref(&patch)).unwrap();
        assert_eq!(removed, ["claude"]);
        let reg = super::super::agent_config::load_or_seed();
        assert!(!reg
            .get("claude")
            .unwrap()
            .args
            .contains(&"--settings".to_string()));

        // Patching an absent agent is a silent no-op.
        let ghost = AgentPatch {
            name: "ghost".into(),
            append_args: vec!["--x".into()],
        };
        assert!(apply_agent_patches(&[ghost]).unwrap().is_empty());
    }

    #[test]
    fn agent_patch_fans_out_to_hook_schema_family() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        // A custom rebranded-claude agent declaring it speaks the claude hook
        // family. It should pick up the claude `--settings` patch too.
        let fleet = AgentDef {
            name: "fleet".into(),
            command: "fleet".into(),
            args: vec![],
            resume_args: vec![],
            fork_args: vec![],
            new_session_args: vec![],
            resume_latest: false,
            hook_schema: Some("claude".into()),
        };
        ensure_agents_registered(&[fleet]).unwrap();

        let patch = AgentPatch {
            name: "claude".into(),
            append_args: vec!["--settings".into(), "/x/hooks.json".into()],
        };
        let mut patched = apply_agent_patches(std::slice::from_ref(&patch)).unwrap();
        patched.sort();
        // The built-in `claude` and the family member `fleet` both get it.
        assert_eq!(patched, ["claude", "fleet"]);

        let reg = super::super::agent_config::load_or_seed();
        for name in ["claude", "fleet"] {
            let args = &reg.get(name).unwrap().args;
            assert!(
                args.windows(2)
                    .any(|w| w == ["--settings".to_string(), "/x/hooks.json".to_string()]),
                "{name} carries the injected flag: {args:?}"
            );
        }

        // Idempotent across the family.
        assert!(apply_agent_patches(std::slice::from_ref(&patch))
            .unwrap()
            .is_empty());

        // Reverse strips it from every family member.
        let mut removed = remove_agent_patches(std::slice::from_ref(&patch)).unwrap();
        removed.sort();
        assert_eq!(removed, ["claude", "fleet"]);
        let reg = super::super::agent_config::load_or_seed();
        for name in ["claude", "fleet"] {
            assert!(!reg
                .get(name)
                .unwrap()
                .args
                .contains(&"--settings".to_string()));
        }
    }

    #[test]
    fn parse_agent_name_extracts_value() {
        assert_eq!(parse_agent_name("name = \"flow\"").as_deref(), Some("flow"));
        assert_eq!(
            parse_agent_name("  name=\"flow-worker\"  ").as_deref(),
            Some("flow-worker")
        );
        assert_eq!(parse_agent_name("command = \"claude\""), None);
    }

    #[test]
    fn write_manifest_round_trips_to_discovery_dir() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());

        let def = ExtensionDef {
            name: "flow".into(),
            ..Default::default()
        };
        let path = write_manifest(&def).unwrap();
        assert!(path.exists());
        assert_eq!(load_manifest("flow").unwrap().name, "flow");
    }
}
