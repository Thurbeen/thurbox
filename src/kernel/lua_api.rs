//! The plugin API's published shape, declared once (issue #1476).
//!
//! What a plugin can read off `thurbox` used to be written out by hand in four
//! places — the publish code, `ui/lib/thurbox.d.lua`, `thurbox.yml` and the
//! `tests/fixtures/lua_std` probe — and nothing compared them, so a field could
//! be published undeclared (#1133) with CI green. This module is the one
//! statement of that shape. The three files carry a generated region built from
//! it, and `tests/lua_api.rs` holds them to it in both directions: each file
//! must equal what this module renders, and every key a real publish produces
//! must be declared here.
//!
//! `just lua-api` rewrites the generated regions after an edit here.
//!
//! [`API`] versions the contract. A field is added freely; removing one, or
//! changing what it means, bumps the number. A plugin that needs a newer
//! contract says so with `requires_api` in its declaration table, and is
//! refused at load with a reason rather than rendering against a field that is
//! not there.

/// The plugin API contract number, published as `thurbox.api`.
///
/// Bumped when the plugin contract changes incompatibly (ADR-40).
/// API 2 makes `files.read` asynchronous: `nil` means pending.
/// Additions never bump it: a plugin reads a newer field defensively, as it
/// reads any optional one.
pub const API: u32 = 2;

/// The prefix of the session meta keys published as `session.meta`.
///
/// Fixed, so a key a driver keeps for itself (`fm.lease`) or thurbox keeps for
/// itself (`thurbox.codex_conversation_id`) never reaches a plugin.
pub const META_PREFIX: &str = crate::session::PUBLISHED_META_PREFIX;

/// The longest published meta value, in characters. A row note is one line.
pub const META_VALUE_CHARS: usize = 256;

/// The longest published meta key, in characters; a longer key is skipped.
pub const META_KEY_CHARS: usize = 64;

/// The most `ui.*` keys published per session, in key order.
///
/// Bounds the per-row table the `sessions` group rebuilds whenever the snapshot
/// moves, whatever a driver writes.
pub const META_KEYS: usize = 32;

/// The published form of one meta value: control characters (newlines
/// included) and invisible characters removed, and the length capped.
/// Anyone with the CLI writes these, agents included, so they reach a pane as
/// untrusted plain text.
pub fn clean_meta_value(value: &str) -> String {
    value
        .chars()
        .filter(|c| !is_hidden(*c))
        .take(META_VALUE_CHARS)
        .collect()
}

/// The characters that are not drawn yet still act: the format characters
/// that change how the text around them is drawn (the soft hyphen, the Arabic
/// letter mark, the Mongolian vowel separator, zero-width spaces and joiners,
/// bidi marks, embeddings, overrides and isolates, the byte-order mark,
/// interlinear annotation, the tag characters, the shorthand and musical
/// format controls), and the Hangul fillers, letters that draw as blank space.
/// Left in, a note could reorder the row it is drawn on or carry text nobody
/// sees.
const INVISIBLE_META_RANGES: &[(char, char)] = &[
    ('\u{00ad}', '\u{00ad}'),
    ('\u{061c}', '\u{061c}'),
    ('\u{180e}', '\u{180e}'),
    ('\u{200b}', '\u{200f}'),
    ('\u{202a}', '\u{202e}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{2069}'),
    ('\u{feff}', '\u{feff}'),
    ('\u{fff9}', '\u{fffb}'),
    ('\u{e0001}', '\u{e0001}'),
    ('\u{e0020}', '\u{e007f}'),
    ('\u{115f}', '\u{1160}'),
    ('\u{3164}', '\u{3164}'),
    ('\u{ffa0}', '\u{ffa0}'),
    ('\u{1bca0}', '\u{1bca3}'),
    ('\u{1d173}', '\u{1d17a}'),
];

/// Extra characters the bounded metadata query must exclude before counting
/// keys. Shares the publication filter's ranges so rejected keys cost no slot.
pub fn invisible_meta_key_chars() -> String {
    INVISIBLE_META_RANGES
        .iter()
        .flat_map(|&(start, end)| (start as u32..=end as u32).filter_map(char::from_u32))
        .collect()
}

/// A character a published meta key or value may not carry: one that acts on
/// the terminal, or one that is not drawn yet changes or hides text.
fn is_hidden(c: char) -> bool {
    c.is_control()
        || INVISIBLE_META_RANGES
            .iter()
            .any(|&(start, end)| (start..=end).contains(&c))
}

/// Whether a stored meta key is one a plugin may see. A pane may draw the key
/// too, so one carrying a hidden character is skipped, not cleaned: cleaning
/// could fold it onto another key.
pub fn is_published_meta_key(key: &str) -> bool {
    key.len() > META_PREFIX.len()
        && key.starts_with(META_PREFIX)
        && key.chars().count() <= META_KEY_CHARS
        && !key.chars().any(is_hidden)
}

/// One published table's shape, rendered as a `---@class`.
pub struct Class {
    pub name: &'static str,
    /// The `---` lines above the class.
    pub doc: &'static [&'static str],
    /// `(exact)`: luals reports a field read that is not declared.
    pub exact: bool,
    pub fields: &'static [Field],
}

pub struct Field {
    pub name: &'static str,
    pub ty: Ty,
    pub optional: bool,
    pub doc: &'static str,
}

/// A field's value. Everything a walk or a dotted path can descend into is a
/// class of its own; a value with no fields to declare is a [`Ty::Leaf`].
pub enum Ty {
    /// A luals type with nothing below it to declare: a scalar, a literal
    /// union, an alias, a list of scalars or a map nobody reads by name.
    Leaf(&'static str),
    Record(&'static Class),
    List(&'static Class),
    /// Keyed by a session or surface id — reached through a variable, so a
    /// dotted path stops at the map.
    Map(&'static Class),
    /// Keyed by surface, each entry a list.
    MapOfLists(&'static Class),
    /// Keyed by the key THIS plugin passed to `run`, so a literal key is a
    /// dotted path a plugin really writes (`thurbox.runs.cpu.state`).
    OwnKeys(&'static Class),
    /// The palette, one key per role of [`roles`].
    Roles,
}

/// A string alias drawn from a fixed set, rendered as `---@alias`.
pub struct Alias {
    pub name: &'static str,
    pub doc: &'static [&'static str],
    /// `None` for `thurbox.Role`, whose values are [`roles`].
    pub values: Option<&'static [&'static str]>,
}

pub enum Item {
    Alias(&'static Alias),
    Class(&'static Class),
}

const fn field(name: &'static str, ty: Ty) -> Field {
    Field {
        name,
        ty,
        optional: false,
        doc: "",
    }
}

const fn leaf(name: &'static str, ty: &'static str) -> Field {
    field(name, Ty::Leaf(ty))
}

impl Field {
    const fn opt(mut self) -> Self {
        self.optional = true;
        self
    }

    const fn doc(mut self, doc: &'static str) -> Self {
        self.doc = doc;
        self
    }
}

/// Every theme role, named as a plugin names it — `theme::palette_roles` is the
/// field set, so a role added there is declared here without a second edit.
pub fn roles() -> Vec<&'static str> {
    let palette = crate::session::theme_config::ThemePreset::all()[0].palette();
    crate::kernel::theme::palette_roles(&palette)
        .into_keys()
        .collect()
}

pub static ROLE: Alias = Alias {
    name: "thurbox.Role",
    doc: &[],
    values: None,
};

pub static STATUS: Alias = Alias {
    name: "thurbox.Status",
    doc: &[
        "A session's derived status, as `SessionState::as_str` spells it.",
        "`lib/theme.lua` has a glyph and a role for `working`, `blocked`,",
        "`background`, `done`, `idle`, `unreachable`, `running`, `uncovered`,",
        "`unreported` and `failed`, and falls back to `idle` only for `stopped` —",
        "which is at rest by definition. `failed` is a launch that died before its",
        "agent reported anything (a failed agents.toml `setup`, or an early",
        "exit). `background` is a turn at rest whose sub-work is still running,",
        "so it is not finished. The three silences are drawn apart",
        "from `idle` on purpose: `running` is \"an agent holds the pane and said",
        "nothing\", `uncovered` is \"wired to report nothing\" and `unreported` is",
        "\"can report, has not yet\". None of them is the agent saying it is at",
        "rest.",
    ],
    values: Some(&[
        "working",
        "blocked",
        "background",
        "done",
        "idle",
        "unreachable",
        "stopped",
        "failed",
        "running",
        "uncovered",
        "unreported",
    ]),
};

pub static GIT_STATS: Class = Class {
    name: "thurbox.GitStats",
    doc: &[
        "Uncommitted work in a session's tree. Absent until it has been measured,",
        "which a plugin must be able to tell apart from a clean tree.",
    ],
    exact: true,
    fields: &[
        leaf("files", "integer"),
        leaf("insertions", "integer"),
        leaf("deletions", "integer"),
        leaf("untracked", "integer"),
        leaf("dirty", "boolean"),
        leaf("ahead", "integer"),
        leaf("behind", "integer"),
        leaf("merged", "boolean")
            .opt()
            .doc("Absent when nobody could say."),
    ],
};

pub static SESSION: Class = Class {
    name: "thurbox.Session",
    doc: &[],
    exact: true,
    fields: &[
        leaf("id", "string"),
        leaf("name", "string"),
        leaf("key", "string").opt().doc(
            "The address a driver gave the session (`session create --key`); opaque, and unchanged by a rename.",
        ),
        leaf("agent", "string").doc("What the row was created as."),
        leaf("reports_as", "string")
            .opt()
            .doc("What a driver declared this session runs (`session reports-as`)."),
        leaf("detected_agent", "string")
            .opt()
            .doc("The registered agent observed in the pane, when it is not `agent`."),
        leaf("status", "thurbox.Status"),
        leaf("backend", "string"),
        leaf("repo", "string").opt(),
        leaf("branch", "string").opt(),
        leaf("base_branch", "string")
            .opt()
            .doc("What a diff is taken against."),
        leaf("host", "string").opt(),
        leaf("parent", "string").opt(),
        leaf("cwd", "string").opt(),
        leaf("worktrees", "integer"),
        leaf("repos", "string[]"),
        leaf("activity", "string")
            .opt()
            .doc("What the agent said about itself."),
        leaf("notification", "string").opt(),
        leaf("meta", "table<string, string>").opt().doc(
            "The session's `ui.*` meta keys, by full key (`meta[\"ui.note\"]`). Untrusted text anyone with the CLI can write: one line, control and invisible characters stripped, at most 256 characters, 32 keys. Absent when it has none.",
        ),
        leaf("display_order", "integer"),
        field("git", Ty::Record(&GIT_STATS)).opt(),
        leaf("attach_error", "string")
            .opt()
            .doc("Why this session's terminal is not live."),
    ],
};

pub static DELETED_SESSION: Class = Class {
    name: "thurbox.DeletedSession",
    doc: &[],
    exact: true,
    fields: &[
        leaf("id", "string"),
        leaf("name", "string"),
        leaf("agent", "string"),
        leaf("deleted_at", "integer").doc("Epoch millis, against `thurbox.taken_at_ms`."),
        leaf("worktrees", "integer"),
        leaf("partial", "boolean").doc("Restoring recovers committed work only."),
        leaf("restore_refusal", "string")
            .opt()
            .doc("Set when the restore would refuse, and why."),
    ],
};

pub static REPO: Class = Class {
    name: "thurbox.Repo",
    doc: &[],
    exact: true,
    fields: &[leaf("path", "string"), leaf("name", "string")],
};

pub static PRESENCE: Alias = Alias {
    name: "thurbox.Presence",
    doc: &[],
    values: Some(&["present", "missing", "unknown"]),
};

pub static AGENT: Class = Class {
    name: "thurbox.Agent",
    doc: &[],
    exact: true,
    fields: &[
        leaf("name", "string"),
        leaf("command", "string"),
        leaf("presence", "thurbox.Presence").doc(
            "Whether `command` resolves to something runnable. `unknown` is not `missing`: it means nothing was looked at.",
        ),
    ],
};

pub static HOST: Class = Class {
    name: "thurbox.Host",
    doc: &[],
    exact: true,
    fields: &[
        leaf("name", "string"),
        leaf("detail", "string"),
        leaf("backend", "string"),
        leaf("platform", "\"posix\"|\"windows\""),
        leaf("multiplexer", "string").opt(),
        leaf("available_multiplexers", "string[]").doc(
            "Installed there and native to its platform. While `probing`, only its platform default and `multiplexer`.",
        ),
        leaf("probing", "boolean").doc("The host has not yet said which multiplexers it has."),
    ],
};

pub static MUX: Class = Class {
    name: "thurbox.Mux",
    doc: &["The local multiplexer every session's window is created in."],
    exact: true,
    fields: &[
        leaf("binary", "string").doc(
            "What a create here runs when nothing names one: the configured multiplexer, else the platform default when installed, else the one that is.",
        ),
        leaf("configured", "string").opt(),
        leaf("available", "string[]"),
        leaf("presence", "thurbox.Presence"),
        leaf("advice", "string").doc("What to do about it; empty when there is nothing to do."),
    ],
};

pub static PREFLIGHT: Class = Class {
    name: "thurbox.Preflight",
    doc: &[
        "Whether the binaries a session needs are installed, as the kernel last",
        "looked. Probed on the kernel's schedule behind a TTL — reading it is a table",
        "lookup, never a `which`.",
    ],
    exact: true,
    fields: &[field("mux", Ty::Record(&MUX))],
};

pub static TASK: Class = Class {
    name: "thurbox.Task",
    doc: &[],
    exact: true,
    fields: &[
        leaf("id", "integer"),
        leaf("title", "string"),
        leaf("description", "string").opt(),
        leaf("status", "string"),
        leaf("source", "string"),
        leaf("url", "string").opt(),
        leaf("created_at", "integer").doc("Epoch seconds."),
        leaf("updated_at", "integer"),
    ],
};

pub static AUTOMATION_RUN: Class = Class {
    name: "thurbox.AutomationRun",
    doc: &[],
    exact: true,
    fields: &[
        leaf("id", "integer"),
        leaf("started_at", "integer").doc("Unix millis."),
        leaf(
            "status",
            "\"running\"|\"success\"|\"error\"|\"skipped\"|\"timed_out\"",
        ),
        leaf("detail", "string").doc(
            "What the run reported: the error, the skip reason or an exec's output tail. The only log a run keeps.",
        ),
        leaf("session", "string")
            .opt()
            .doc("The session it sent to or spawned, when it recorded one."),
    ],
};

pub static AUTOMATION: Class = Class {
    name: "thurbox.Automation",
    doc: &[],
    exact: true,
    fields: &[
        leaf("id", "integer"),
        leaf("name", "string"),
        leaf("schedule", "string").doc("The cron expression, or `once`."),
        leaf("action", "\"send\"|\"spawn\"|\"exec\""),
        leaf("enabled", "boolean"),
        leaf("last_outcome", "string").opt(),
        leaf("last_detail", "string").opt(),
        field("runs", Ty::List(&AUTOMATION_RUN)).doc("The ten most recent, newest first."),
        leaf("trigger", "string").doc(
            "`cron:<expr>` or `at:<unix_millis>`: what an edit's `trigger` takes back unchanged.",
        ),
        leaf("timezone", "string")
            .opt()
            .doc("IANA name; nil is the system's own."),
        leaf("prompt", "string")
            .doc("What a send or spawn types; an exec automation runs `command` and ignores it."),
        leaf("session", "string")
            .opt()
            .doc("A send's target session."),
        leaf("repo", "string").opt().doc("A spawn's repository."),
        leaf("branch", "string")
            .opt()
            .doc("A spawn's worktree branch."),
        leaf("base", "string").opt(),
        leaf("agent", "string").opt(),
        leaf("extra_repos", "integer").doc("Further repositories a multi-repo spawn spans."),
        leaf("command", "string")
            .opt()
            .doc("An exec automation's command line."),
        leaf("created_at", "integer").doc("Unix millis."),
        leaf("updated_at", "integer"),
        leaf("last_run_at", "integer").opt(),
        leaf("next_run_at", "integer")
            .opt()
            .doc("Nil while disabled, and once a schedule has no future occurrence."),
    ],
};

pub static IN_FLIGHT: Class = Class {
    name: "thurbox.InFlight",
    doc: &["A command this interface accepted but has not finished."],
    exact: true,
    fields: &[
        leaf("id", "integer"),
        leaf("kind", "string"),
        leaf("session", "string"),
        leaf("phase", "string"),
        leaf("subject", "string").opt().doc(
            "A `create`'s repository name, a `bookmark` write's path as issued, or an `automation`'s `#<id>` (a create's name).",
        ),
        leaf("host", "string")
            .opt()
            .doc("The machine a creation will land on; nil for this one."),
        leaf("error", "string").opt(),
    ],
};

pub static SEARCH_HIT: Class = Class {
    name: "thurbox.SearchHit",
    doc: &["One line a content search found, in a session's agent pane or its shell."],
    exact: true,
    fields: &[
        leaf("session", "string"),
        leaf("shell", "boolean").doc("Found in the companion shell rather than the agent."),
        leaf("text", "string").doc("The line, trimmed and windowed around the match."),
        leaf("positions", "integer[]").doc(
            "1-based character indices of `text` that matched — what `lib.fuzzy.spans` lights.",
        ),
        leaf("back", "integer").doc("Rows between the line and the bottom of its terminal."),
        leaf("scroll", "integer")
            .doc("The scrollback offset that puts the line on screen; 0 when it already is."),
        leaf("row", "integer")
            .doc("The screen row, from the top, the line is on once scrolled to `scroll`."),
        leaf("exact", "boolean").doc(
            "Every term matched as a substring, phrase or regex — none only as a subsequence.",
        ),
        leaf("score", "integer").doc("Higher is better; hits arrive already ranked."),
    ],
};

pub static SEARCH_ANSWER: Class = Class {
    name: "thurbox.SearchAnswer",
    doc: &["A finished content search (`kernel::search::Answer`)."],
    exact: true,
    fields: &[
        leaf("query", "string").doc("The `store.want_content` it answers."),
        leaf("within", "string")
            .opt()
            .doc("The `store[\"want_content.sessions\"]` it was limited to, if any."),
        field("hits", Ty::List(&SEARCH_HIT)).doc("Best first, at most 200 (50 per session)."),
        leaf("total", "integer").doc("Matching lines found, before the cap."),
        leaf("sessions", "integer").doc("Sessions whose terminals were read."),
        leaf("lines", "integer").doc("Lines searched."),
        leaf("ms", "number").doc("Time the worker took."),
        leaf("error", "string")
            .opt()
            .doc("Why the query could not run — an invalid regex."),
    ],
};

pub static DIFF_FILE: Class = Class {
    name: "thurbox.DiffFile",
    doc: &[],
    exact: true,
    fields: &[
        leaf("path", "string"),
        leaf("added", "integer"),
        leaf("removed", "integer"),
        leaf("status", "string"),
        leaf("old_path", "string").opt(),
    ],
};

pub static DIFF: Class = Class {
    name: "thurbox.Diff",
    doc: &["A session's diff. Branch on `state`."],
    exact: true,
    fields: &[
        leaf("state", "\"pending\"|\"ready\"|\"failed\""),
        leaf("error", "string").opt(),
        leaf("truncated", "boolean").opt(),
        leaf("raw_bytes", "integer").opt(),
        leaf("untracked_omitted", "integer").opt(),
        field("files", Ty::List(&DIFF_FILE)).opt(),
        leaf("body", "string[]").opt(),
    ],
};

pub static LINK: Class = Class {
    name: "thurbox.Link",
    doc: &["A link found in a session's terminal, at the cell it starts on."],
    exact: true,
    fields: &[
        leaf("url", "string"),
        leaf("row", "integer"),
        leaf("col", "integer"),
    ],
};

pub static RUN: Class = Class {
    name: "thurbox.Run",
    doc: &[
        "An answer to a program this plugin asked to run. Branch on `state`: a",
        "program that has not answered is not one that answered with nothing.",
    ],
    exact: true,
    fields: &[
        leaf("state", "\"pending\"|\"done\"|\"failed\""),
        leaf("error", "string").opt(),
        leaf("stdout", "string").opt(),
        leaf("stderr", "string").opt(),
        leaf("status", "integer").opt(),
        leaf("truncated", "boolean").opt(),
        leaf("timed_out", "boolean").opt(),
        leaf("ok", "boolean").opt(),
    ],
};

pub static FEATURES: Class = Class {
    name: "thurbox.Features",
    doc: &[],
    exact: true,
    fields: &[
        leaf("tasks", "boolean"),
        leaf("automations", "boolean"),
        leaf("file_viewer", "boolean"),
        leaf("global_search", "boolean"),
        leaf("info_panel", "boolean"),
        leaf("shell_pane", "boolean"),
        leaf("code_review", "boolean"),
        leaf("perf_hud", "boolean"),
        leaf("mouse", "boolean"),
        leaf("notifications", "boolean"),
        leaf("soft_delete", "boolean"),
        leaf("version_check", "boolean"),
        leaf("auto_update", "boolean"),
    ],
};

pub static SETTINGS: Class = Class {
    name: "thurbox.Settings",
    doc: &[
        "The settings in force. Read your own switch and decline to draw when it is",
        "off — the kernel gates only what it owns.",
    ],
    exact: true,
    fields: &[
        field("features", Ty::Record(&FEATURES)),
        leaf("two_panel_min_cols", "integer"),
        leaf("three_panel_min_cols", "integer"),
        leaf("scrollback_lines", "integer"),
    ],
};

pub static BOOKMARK_ROW: Class = Class {
    name: "thurbox.BookmarkRow",
    doc: &[],
    exact: true,
    fields: &[
        leaf("path", "string"),
        leaf("name", "string"),
        leaf("parent", "string").opt(),
        leaf("label", "string").opt(),
        leaf("is_parent", "boolean"),
        leaf("offered", "boolean"),
        leaf("is_git", "boolean").opt(),
    ],
};

pub static BOOKMARKS: Class = Class {
    name: "thurbox.Bookmarks",
    doc: &["Remembered repositories, served only while `store.want_bookmarks` asks."],
    exact: true,
    fields: &[
        leaf("host", "string"),
        leaf("loading", "boolean"),
        field("rows", Ty::List(&BOOKMARK_ROW)),
    ],
};

pub static BROWSE_ENTRY: Class = Class {
    name: "thurbox.BrowseEntry",
    doc: &[],
    exact: true,
    fields: &[leaf("name", "string"), leaf("is_git", "boolean")],
};

pub static BROWSE: Class = Class {
    name: "thurbox.Browse",
    doc: &["A directory listing, served only while `store.want_browse` asks."],
    exact: true,
    fields: &[
        leaf("host", "string"),
        leaf("dir", "string"),
        leaf("loading", "boolean"),
        leaf("error", "string").opt(),
        field("entries", Ty::List(&BROWSE_ENTRY)),
    ],
};

pub static BRANCHES: Class = Class {
    name: "thurbox.Branches",
    doc: &["Base branches, served only while `store.want_branches` asks."],
    exact: true,
    fields: &[
        leaf("host", "string"),
        leaf("repo", "string"),
        leaf("loading", "boolean"),
        leaf("error", "string").opt(),
        leaf("list", "string[]"),
    ],
};

pub static WORKTREE_ENTRY: Class = Class {
    name: "thurbox.WorktreeEntry",
    doc: &[],
    exact: true,
    fields: &[leaf("path", "string"), leaf("branch", "string")],
};

pub static WORKTREES: Class = Class {
    name: "thurbox.Worktrees",
    doc: &["The worktrees a repo already has, served while `store.want_worktrees` asks."],
    exact: true,
    fields: &[
        leaf("host", "string"),
        leaf("repo", "string"),
        leaf("loading", "boolean"),
        leaf("error", "string").opt(),
        field("list", Ty::List(&WORKTREE_ENTRY)),
    ],
};

pub static SYSTEM_METRICS: Class = Class {
    name: "thurbox.SystemMetrics",
    doc: &[],
    exact: true,
    fields: &[
        leaf("cpu_percent", "number"),
        leaf("memory_used", "integer"),
        leaf("memory_total", "integer"),
    ],
};

pub static AGENT_METRICS: Class = Class {
    name: "thurbox.AgentMetrics",
    doc: &[
        "What an agent reported about its own turn, from its statusline. Every field",
        "is absent rather than zero when the agent did not report it, so a panel",
        "renders the rows it has — `publish.rs`'s `agent_metrics_table` drops a nil.",
        "The counts arrive as Lua numbers rather than integers: they cross as `f64`.",
    ],
    exact: true,
    fields: &[
        leaf("model", "string").opt().doc("The display name."),
        leaf("model_id", "string").opt(),
        leaf("cli_version", "string").opt(),
        leaf("cost_usd", "number").opt(),
        leaf("duration_ms", "number").opt(),
        leaf("api_duration_ms", "number").opt(),
        leaf("lines_added", "number").opt(),
        leaf("lines_removed", "number").opt(),
        leaf("input_tokens", "number").opt(),
        leaf("output_tokens", "number").opt(),
        leaf("context_window", "number").opt(),
        leaf("context_used_percent", "number").opt(),
        leaf("current_input_tokens", "number").opt(),
        leaf("current_output_tokens", "number").opt(),
        leaf("cache_creation_tokens", "number").opt(),
        leaf("cache_read_tokens", "number").opt(),
    ],
};

pub static USAGE_WINDOW: Class = Class {
    name: "thurbox.UsageWindow",
    doc: &["One account rate-limit window."],
    exact: true,
    fields: &[
        leaf("label", "string"),
        leaf("used_percent", "number"),
        leaf("resets_at", "integer")
            .opt()
            .doc("Epoch seconds; absent when the account did not say."),
    ],
};

pub static USAGE: Class = Class {
    name: "thurbox.Usage",
    doc: &["The account's rate-limit windows, shared by every session on that agent."],
    exact: true,
    fields: &[
        field("windows", Ty::List(&USAGE_WINDOW)),
        leaf("plan", "string").opt(),
        leaf("note", "string").opt(),
    ],
};

pub static SESSION_METRICS: Class = Class {
    name: "thurbox.SessionMetrics",
    doc: &[],
    exact: false,
    fields: &[
        leaf("cpu_percent", "number").opt(),
        leaf("memory_bytes", "integer").opt(),
        field("agent", Ty::Record(&AGENT_METRICS)).opt(),
        field("usage", Ty::Record(&USAGE)).opt(),
    ],
};

pub static METRICS: Class = Class {
    name: "thurbox.Metrics",
    doc: &[],
    exact: true,
    fields: &[
        field("system", Ty::Record(&SYSTEM_METRICS)),
        field("sessions", Ty::Map(&SESSION_METRICS)),
    ],
};

pub static PLATFORM: Class = Class {
    name: "thurbox.Platform",
    doc: &["The machine this is running on, from the values the binary was built for."],
    exact: true,
    fields: &[leaf("os", "string"), leaf("arch", "string")],
};

pub static KEYBOARD: Class = Class {
    name: "thurbox.Keyboard",
    doc: &[
        "Whether a key's release reaches a binding declared `release = true`:",
        "`unsupported` (no kitty keyboard protocol, not Windows — only presses",
        "arrive), `negotiated` (asked for and accepted, none seen yet) or `reported`",
        "(one has arrived this run).",
    ],
    exact: true,
    fields: &[leaf(
        "releases",
        "\"unsupported\"|\"negotiated\"|\"reported\"",
    )],
};

pub static HOVER: Class = Class {
    name: "thurbox.Hover",
    doc: &[
        "What the pointer is over, as whichever of the two the affordance was marked",
        "with. Empty when nothing is hovered.",
    ],
    exact: true,
    fields: &[leaf("id", "string").opt(), leaf("role", "string").opt()],
};

pub static THEME_CHOICE: Class = Class {
    name: "thurbox.ThemeChoice",
    doc: &[],
    exact: true,
    fields: &[
        leaf("name", "string"),
        leaf("display_name", "string"),
        leaf("light", "boolean"),
        leaf("custom", "boolean"),
    ],
};

pub static THEME_SNAPSHOT: Class = Class {
    name: "thurbox.ThemeSnapshot",
    doc: &[],
    exact: true,
    fields: &[
        leaf("name", "string"),
        field("roles", Ty::Roles),
        leaf("nerd_font", "boolean"),
        field("choices", Ty::List(&THEME_CHOICE)),
    ],
};

pub static REGISTRY_KEY: Class = Class {
    name: "thurbox.RegistryKey",
    doc: &[],
    exact: true,
    fields: &[
        leaf("plugin", "string"),
        leaf("action", "string"),
        leaf("key", "string"),
        leaf("default_key", "string"),
        leaf("desc", "string"),
        leaf("scope", "\"global\"|\"plugin\""),
        leaf("rebound", "boolean"),
        leaf("group", "string"),
    ],
};

pub static REGISTRY_COMMAND: Class = Class {
    name: "thurbox.RegistryCommand",
    doc: &["A chord-less action a plugin declares in `commands`, for the palette."],
    exact: true,
    fields: &[
        leaf("plugin", "string"),
        leaf("action", "string"),
        leaf("desc", "string"),
    ],
};

pub static REGISTRY_SETTING: Class = Class {
    name: "thurbox.RegistrySetting",
    doc: &[],
    exact: true,
    fields: &[
        leaf("plugin", "string"),
        leaf("id", "string"),
        leaf("desc", "string"),
        leaf("type", "string"),
        leaf("value", "boolean|number|string"),
        leaf("default", "boolean|number|string"),
        leaf("choices", "string[]")
            .opt()
            .doc("Present only when the setting declared them."),
    ],
};

pub static REGISTRY_SNAPSHOT: Class = Class {
    name: "thurbox.RegistrySnapshot",
    doc: &["What every plugin declared, so help and settings render from it."],
    exact: true,
    fields: &[
        field("keys", Ty::List(&REGISTRY_KEY)),
        field("commands", Ty::List(&REGISTRY_COMMAND)),
        field("settings", Ty::List(&REGISTRY_SETTING)),
        leaf("sections", "string[]").doc("The order help renders its sections in."),
    ],
};

pub static PLUGIN_ROW: Class = Class {
    name: "thurbox.PluginRow",
    doc: &["One of the interface's own files: where it came from, and whether it runs."],
    exact: true,
    fields: &[
        leaf("path", "string"),
        leaf("name", "string"),
        leaf("kind", "string"),
        leaf("slot", "string"),
        leaf("source", "string"),
        leaf("state", "string"),
        leaf("error", "string").opt(),
    ],
};

pub static CHROME: Class = Class {
    name: "thurbox.Chrome",
    doc: &["What the arrangement needs to know about the bands, and no more."],
    exact: true,
    fields: &[leaf("status_rows", "integer")],
};

/// One field per `Capability::ALL` entry, which `tests/lua_api.rs` checks.
pub static GRANTED: Class = Class {
    name: "thurbox.Granted",
    doc: &[
        "Capabilities THIS plugin has been granted. A boolean about a decision the",
        "user already made; it grants nothing. Absent, like `thurbox.runs`, for a",
        "plugin that declares no capability.",
    ],
    exact: true,
    fields: &[
        leaf("run", "boolean").opt(),
        leaf("program", "boolean").opt(),
        leaf("sessions", "boolean").opt().doc(
            "Verbs that drive an agent or lose something for good are applied (docs/PLUGINS.md).",
        ),
        leaf("files", "boolean").opt().doc("`files` is present."),
    ],
};

/// The root. Two publish paths fill it: `LuaHost::publish` for everything, and
/// `LuaHost::enter`, which sets `runs` and `granted` per plugin.
pub static API_ROOT: Class = Class {
    name: "thurbox.Api",
    doc: &[
        "Everything readable. Rebuilt each publish; a group whose inputs did not move",
        "is handed back as the same table, which is what `lib/theme.lua` memoizes on.",
    ],
    exact: true,
    fields: &[
        leaf("api", "integer").doc(
            "The plugin API contract number. It moves only when a field is removed or changes meaning; declare `requires_api` to need a newer one.",
        ),
        field("sessions", Ty::List(&SESSION)),
        field("deleted", Ty::List(&DELETED_SESSION)),
        field("repos", Ty::List(&REPO)),
        field("agents", Ty::List(&AGENT)),
        leaf("agent_default", "string").doc("What a bare launch would use."),
        field("settings", Ty::Record(&SETTINGS)),
        field("bookmarks", Ty::Record(&BOOKMARKS)),
        field("browse", Ty::Record(&BROWSE)),
        field("branches", Ty::Record(&BRANCHES)),
        field("worktrees", Ty::Record(&WORKTREES)),
        field("hosts", Ty::List(&HOST)),
        field("preflight", Ty::Record(&PREFLIGHT)),
        field("tasks", Ty::List(&TASK)),
        field("automations", Ty::List(&AUTOMATION)),
        field("commands", Ty::List(&IN_FLIGHT)),
        field("diffs", Ty::Map(&DIFF)),
        field("links", Ty::MapOfLists(&LINK)).doc(
            "Keyed by SURFACE, not by session: a session's companion shell is `<id>#shell` and has links of its own.",
        ),
        field("search", Ty::Record(&SEARCH_ANSWER)).opt().doc(
            "The content search's answer, while `store.want_content` asks. Nil until one lands.",
        ),
        leaf("printing", "table<string, boolean>").doc(
            "Sessions whose pane is producing output right now, keyed by id. The evidence `running` animates on — see `ui.status`.",
        ),
        field("runs", Ty::OwnKeys(&RUN)).doc("Answers to THIS plugin's runs."),
        field("granted", Ty::Record(&GRANTED)),
        field("metrics", Ty::Record(&METRICS)),
        field("platform", Ty::Record(&PLATFORM)),
        field("keyboard", Ty::Record(&KEYBOARD)),
        leaf("version", "string"),
        leaf("reloads", "integer"),
        leaf("can_open_links", "boolean"),
        leaf("taken_at_ms", "integer").doc("When these rows were read."),
        leaf("error", "string").opt(),
        leaf("focus", "string").doc("Which pane holds focus, by name."),
        leaf("selection", "string")
            .doc("The mouse text selection, empty when nothing is selected."),
        field("hover", Ty::Record(&HOVER)),
        field("plugins", Ty::List(&PLUGIN_ROW)),
        leaf("ui_dir", "string"),
        field("chrome", Ty::Record(&CHROME)),
        field("theme", Ty::Record(&THEME_SNAPSHOT)),
        field("registry", Ty::Record(&REGISTRY_SNAPSHOT)),
    ],
};

/// Everything the generated region of `thurbox.d.lua` declares, in order.
pub static ITEMS: &[Item] = &[
    Item::Alias(&ROLE),
    Item::Alias(&STATUS),
    Item::Class(&GIT_STATS),
    Item::Class(&SESSION),
    Item::Class(&DELETED_SESSION),
    Item::Class(&REPO),
    Item::Alias(&PRESENCE),
    Item::Class(&AGENT),
    Item::Class(&HOST),
    Item::Class(&MUX),
    Item::Class(&PREFLIGHT),
    Item::Class(&TASK),
    Item::Class(&AUTOMATION_RUN),
    Item::Class(&AUTOMATION),
    Item::Class(&IN_FLIGHT),
    Item::Class(&SEARCH_HIT),
    Item::Class(&SEARCH_ANSWER),
    Item::Class(&DIFF_FILE),
    Item::Class(&DIFF),
    Item::Class(&LINK),
    Item::Class(&RUN),
    Item::Class(&FEATURES),
    Item::Class(&SETTINGS),
    Item::Class(&BOOKMARK_ROW),
    Item::Class(&BOOKMARKS),
    Item::Class(&BROWSE_ENTRY),
    Item::Class(&BROWSE),
    Item::Class(&BRANCHES),
    Item::Class(&WORKTREE_ENTRY),
    Item::Class(&WORKTREES),
    Item::Class(&SYSTEM_METRICS),
    Item::Class(&AGENT_METRICS),
    Item::Class(&USAGE_WINDOW),
    Item::Class(&USAGE),
    Item::Class(&SESSION_METRICS),
    Item::Class(&METRICS),
    Item::Class(&PLATFORM),
    Item::Class(&KEYBOARD),
    Item::Class(&HOVER),
    Item::Class(&THEME_CHOICE),
    Item::Class(&THEME_SNAPSHOT),
    Item::Class(&REGISTRY_KEY),
    Item::Class(&REGISTRY_COMMAND),
    Item::Class(&REGISTRY_SETTING),
    Item::Class(&REGISTRY_SNAPSHOT),
    Item::Class(&PLUGIN_ROW),
    Item::Class(&CHROME),
    Item::Class(&GRANTED),
    Item::Class(&API_ROOT),
];

/// The first and last line of every generated region.
pub const BEGIN: &str = "BEGIN GENERATED by src/kernel/lua_api.rs: edit there, then `just lua-api`";
pub const END: &str = "END GENERATED";

impl Ty {
    fn luals(&self) -> String {
        match self {
            Ty::Leaf(ty) => (*ty).to_string(),
            Ty::Record(class) => class.name.to_string(),
            Ty::List(class) => format!("{}[]", class.name),
            Ty::Map(class) | Ty::OwnKeys(class) => format!("table<string, {}>", class.name),
            Ty::MapOfLists(class) => format!("table<string, {}[]>", class.name),
            Ty::Roles => "table<thurbox.Role, thurbox.Color>".to_string(),
        }
    }
}

fn doc_lines(out: &mut String, doc: &[&str]) {
    for line in doc {
        out.push_str("--- ");
        out.push_str(line);
        out.push('\n');
    }
}

/// The generated region of `ui/lib/thurbox.d.lua`, markers excluded.
pub fn declarations() -> String {
    let mut out = String::new();
    for (index, item) in ITEMS.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        match item {
            Item::Alias(alias) => {
                doc_lines(&mut out, alias.doc);
                out.push_str(&format!("---@alias {}\n", alias.name));
                let values = alias.values.map(<[_]>::to_vec).unwrap_or_else(roles);
                for value in values {
                    out.push_str(&format!("---| \"{value}\"\n"));
                }
            }
            Item::Class(class) => {
                doc_lines(&mut out, class.doc);
                let exact = if class.exact { "(exact) " } else { "" };
                out.push_str(&format!("---@class {exact}{}\n", class.name));
                for field in class.fields {
                    let optional = if field.optional { "?" } else { "" };
                    let doc = if field.doc.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", field.doc)
                    };
                    out.push_str(&format!(
                        "---@field {}{optional} {}{doc}\n",
                        field.name,
                        field.ty.luals()
                    ));
                }
            }
        }
    }
    out.push_str("thurbox = {}\n");
    out
}

/// Every dotted path selene checks, in [`API_ROOT`] order.
///
/// selene checks a path one segment at a time and stops at the first computed
/// index, so a record is written out field by field while a list or an
/// id-keyed map stops at itself: its rows are reached through `[i]` or a
/// variable. Declaring a field implies its parents, so only leaves appear.
/// `runs` is the one map a plugin indexes with a literal of its own choosing,
/// so it takes `*` twice instead.
pub fn selene_paths() -> Vec<String> {
    let mut paths = Vec::new();
    walk_paths("thurbox", &API_ROOT, &mut paths, true);
    paths
}

/// The same paths as a plugin really writes them — `runs.*` as a key a plugin
/// would pick — for the probe that must lint clean.
fn probe_paths() -> Vec<String> {
    let mut paths = Vec::new();
    walk_paths("thurbox", &API_ROOT, &mut paths, false);
    paths
}

fn walk_paths(prefix: &str, class: &Class, out: &mut Vec<String>, wildcard: bool) {
    for field in class.fields {
        let path = format!("{prefix}.{}", field.name);
        match &field.ty {
            Ty::Record(inner) => walk_paths(&path, inner, out, wildcard),
            Ty::Roles => out.extend(roles().into_iter().map(|role| format!("{path}.{role}"))),
            Ty::OwnKeys(_) if wildcard => {
                let (one, two) = (format!("{path}.*"), format!("{path}.*.*"));
                out.extend([path, one, two]);
            }
            Ty::OwnKeys(inner) => {
                out.push(format!("{path}.cpu"));
                out.push(format!("{path}.cpu.{}", inner.fields[0].name));
            }
            Ty::Leaf(_) | Ty::List(_) | Ty::Map(_) | Ty::MapOfLists(_) => out.push(path),
        }
    }
}

/// The generated region of `thurbox.yml`'s `globals:`, markers excluded.
pub fn selene_globals() -> String {
    let mut out = String::new();
    for path in selene_paths() {
        out.push_str(&format!("  {path}:\n    property: read-only\n"));
    }
    out
}

/// `tests/fixtures/lua_std/reads.lua`: a pane reading every declared path, which
/// must lint clean against `thurbox.yml`.
pub fn std_probe() -> String {
    let mut out = String::new();
    out.push_str(&format!("-- {BEGIN}\n"));
    out.push_str(
        "--\n\
         -- A pane that reads EVERY path `thurbox.yml` declares on `thurbox`, each as a\n\
         -- plain dotted path, and must lint CLEAN: a table declared as a bare property\n\
         -- rejects exactly the expression `docs/PLUGINS.md` tells an author to write,\n\
         -- and nothing in `ui/` would notice (issue #1133). `typos/` holds the other\n\
         -- direction. It ends with the stdlib names no bundled pane happens to name.\n\
         return {\n  name = \"std_reads\",\n  render = function()\n    local read = {\n",
    );
    for path in probe_paths() {
        out.push_str(&format!("      tostring({path}),\n"));
    }
    out.push_str(
        "      _VERSION,\n\
         \x20     tostring(math.acos(0)),\n\
         \x20     tostring(math.asin(1)),\n\
         \x20     tostring(math.atan(1, 1)),\n\
         \x20     tostring(math.deg(1)),\n\
         \x20     tostring(math.rad(90)),\n\
         \x20   }\n\
         \x20   return { text = table.concat(read, \" \") }\n\
         \x20 end,\n\
         }\n",
    );
    out.push_str(&format!("-- {END}\n"));
    out
}

/// Replace the region between the `BEGIN`/`END` marker lines of `text` (each
/// marker a whole line of `comment` + marker) with `body`. `None` when the
/// markers are missing or out of order.
///
/// The answer always has `\n` line ends: the files are committed that way,
/// and a Windows checkout with `core.autocrlf` reads them back with `\r\n`.
pub fn splice(text: &str, comment: &str, body: &str) -> Option<String> {
    let text = &text.replace("\r\n", "\n");
    let begin = format!("{comment}{BEGIN}\n");
    let end = format!("{comment}{END}\n");
    let start = text.find(&begin)? + begin.len();
    let stop = start + text[start..].find(&end)?;
    Some(format!("{}{body}{}", &text[..start], &text[stop..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meta_value_is_one_capped_line() {
        assert_eq!(
            clean_meta_value("waiting\non CI\u{1b}[31m"),
            "waitingon CI[31m"
        );
        // Invisible format characters: a right-to-left override, zero-width
        // space and joiner, a bidi isolate, a byte-order mark.
        assert_eq!(
            clean_meta_value("ab\u{202e}c\u{200b}d\u{200d}e\u{2066}f\u{feff}g"),
            "abcdefg"
        );
        // An Arabic letter mark, a soft hyphen, the Mongolian vowel separator,
        // interlinear annotation and tag characters — hidden text — and a
        // Hangul filler and a shorthand format control, which draw as nothing.
        assert_eq!(
            clean_meta_value("a\u{061c}b\u{00ad}c\u{180e}d\u{fff9}e\u{e0001}f\u{e0041}g"),
            "abcdefg"
        );
        assert_eq!(clean_meta_value("a\u{3164}b\u{1bca0}c"), "abc");
        let long = "é".repeat(META_VALUE_CHARS + 10);
        assert_eq!(clean_meta_value(&long).chars().count(), META_VALUE_CHARS);
    }

    #[test]
    fn only_ui_keys_are_published() {
        assert!(is_published_meta_key("ui.note"));
        assert!(!is_published_meta_key("ui."));
        assert!(!is_published_meta_key("fm.lease"));
        assert!(!is_published_meta_key("thurbox.codex_conversation_id"));
        assert!(!is_published_meta_key("ui.\nnote"));
        assert!(
            !is_published_meta_key("ui.\u{202e}eton"),
            "a key is drawn too"
        );
        assert!(!is_published_meta_key(&format!(
            "ui.{}",
            "k".repeat(META_KEY_CHARS)
        )));
    }

    #[test]
    fn splice_replaces_only_the_region() {
        let text = format!("a\n-- {BEGIN}\nold\n-- {END}\nb\n");
        assert_eq!(
            splice(&text, "-- ", "new\n").unwrap(),
            format!("a\n-- {BEGIN}\nnew\n-- {END}\nb\n")
        );
        assert!(splice("no markers\n", "-- ", "x").is_none());
    }

    #[test]
    fn splice_finds_the_region_in_a_crlf_checkout() {
        // What a Windows checkout with `core.autocrlf` hands the drift test.
        let text = format!("a\r\n-- {BEGIN}\r\nold\r\n-- {END}\r\nb\r\n");
        assert_eq!(
            splice(&text, "-- ", "new\n").as_deref(),
            Some(format!("a\n-- {BEGIN}\nnew\n-- {END}\nb\n").as_str())
        );
    }
}
