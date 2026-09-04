---@meta
--
-- The plugin API, as types. Loaded by lua-language-server through
-- `.luarc.json`'s `workspace.library`; never loaded by the plugin VM.
--
-- It exists because every mistake the API allows is silent. `convert.rs` drops
-- a node key it does not know (that is how a plugin carries its own bookkeeping
-- on the node table), `command` reads a fixed list of option names and ignores
-- the rest, and `lib/theme.lua`'s `__index` answers nil for a role no palette
-- defines. Each of those renders something plausible and reports nothing, so
-- the only place a typo can be caught is before it runs.
--
-- Two shapes do the catching, because lua-language-server will not flag an
-- *extra* key in a table constructor:
--
--   * every kind and every verb declares the field it cannot work without, so
--     a misspelt one reads as that field missing (`missing-fields`);
--   * every field whose value is drawn from a fixed set is that set, so a
--     misspelt value is a type mismatch (`assign-type-mismatch`).
--
-- `tests/fixtures/lua_types` + `scripts/ci/check-lua-types.sh` hold this to it.
--
-- Keep in step with `src/kernel/node.rs` and `src/kernel/convert.rs` (nodes),
-- `src/kernel/host/load.rs` (the declaration), `src/kernel/host/publish.rs`
-- (`thurbox.*`), `src/kernel/command/mod.rs` (verbs) and `src/kernel/theme.rs`
-- (roles) — and with `thurbox.yml`, which is the same contract for selene.

---@alias thurbox.Color string A role's colour, `#rrggbb`, a name, or a 0-255 index.

--- A style: a bare colour is shorthand for a foreground.
---@class thurbox.Style
---@field fg? thurbox.Color
---@field bg? thurbox.Color
---@field bold? boolean
---@field dim? boolean
---@field italic? boolean
---@field underline? boolean
---@field reversed? boolean
---@field crossed_out? boolean

---@alias thurbox.StyleSpec thurbox.Color|thurbox.Style

--- One styled run within a line.
---@class thurbox.Span
---@field text string|number|boolean
---@field style? thurbox.StyleSpec

--- One line: a bare string, one span, or a list of spans.
---@alias thurbox.Line string|number|boolean|thurbox.Span|thurbox.Span[]

--- What a `text` node or a frame title accepts: one line, or a list of them.
---@alias thurbox.Text thurbox.Line|thurbox.Line[]

--- A frame drawn around a node. `borders` is all-or-none and the border type is
--- fixed; there is no `title_align`.
---@class thurbox.Frame
---@field title? thurbox.Text
---@field borders? "all"|"none"
---@field border_style? thurbox.StyleSpec
---@field style? thurbox.StyleSpec
---@field padding? integer

--- What every node may say about its space and its identity.
---
--- Sizing resolves exact → percentage → share, each clamped by `min`/`max`; a
--- node that asks for nothing takes an equal share of the remainder. `role`
--- carries the kernel's own click verbs (`action:`, `key:`, `focus:`, `url:`,
--- and the bare `drag`); any other role hands the click to the plugin.
---@class thurbox.NodeCommon
---@field len? integer
---@field pct? number
---@field fill? number
---@field min? integer
---@field max? integer
---@field id? string
---@field class? string|string[]
---@field role? string
---@field frame? thurbox.Frame|string|boolean
---@field block? thurbox.Frame|string|boolean The POC's spelling of `frame`.

--- Text. Carries no style of its own — style the spans.
---@class (exact) thurbox.TextNode : thurbox.NodeCommon
---@field type? "text"|"paragraph"|"line"
---@field text thurbox.Text
---@field align? "left"|"center"|"centre"|"right"
---@field wrap? boolean
---@field scroll? integer

--- A row or a column. Children live under `children`, or in the array part.
---@class (exact) thurbox.BoxNode : thurbox.NodeCommon
---@field type? "box"|"vstack"|"hstack"|"column"|"row"|"stack"
---@field axis? "vertical"|"horizontal"|"column"|"row"|"v"|"h"
---@field gap? integer
---@field children? thurbox.Node[]

--- A text field. `focused` claims the one caret, and `cursor` counts characters.
---@class (exact) thurbox.InputNode : thurbox.NodeCommon
---@field type? "input"|"field"
---@field value string
---@field cursor? integer
---@field placeholder? string
---@field focused? boolean
---@field style? thurbox.StyleSpec

--- Pre-rendered cells: a live session's terminal, a program this plugin asked
--- to run, or lines the plugin produced itself. Exactly one source.
---@class (exact) thurbox.SurfaceNode : thurbox.NodeCommon
---@field type? "surface"|"terminal"
---@field session? string
---@field program? string
---@field cells? thurbox.Line[]
---@field scroll? integer

---@alias thurbox.Node thurbox.TextNode|thurbox.BoxNode|thurbox.InputNode|thurbox.SurfaceNode

--- How big a floating pane asks to be: a share of the screen, or exact cells.
---@class (exact) thurbox.Float
---@field width? number
---@field height? number
---@field cols? integer
---@field rows? integer

--- What a render is told. `elapsed` is served through a metatable, so reading it
--- is what marks a `pure` pane as depending on the animation clock.
---@class (exact) thurbox.Ctx
---@field width integer
---@field height integer
---@field focused boolean
---@field frame integer
---@field name string
---@field slot string
---@field elapsed number

--- What a decorator is told. Smaller than a render's: it is handed the tree it
--- is transforming, not a slot of its own.
---@class (exact) thurbox.DecorateCtx
---@field width integer
---@field height integer

--- What `ui/layout.lua` is told. Smaller than a render's: an arrangement sees
--- the screen and which slots are occupied, and no plugin.
---@class (exact) thurbox.LayoutCtx
---@field width integer
---@field height integer
---@field slots table<string, boolean>

--- A key offered to `on_key`. Return true to consume it.
---@class (exact) thurbox.Key
---@field key string
---@field char? string
---@field ctrl boolean
---@field alt boolean
---@field shift boolean
---@field cmd boolean

--- A click on a node this plugin painted, with the rect it landed in.
--- Reached only for identity the kernel has no verb for.
---@class (exact) thurbox.Hit
---@field id? string
---@field class string
---@field role? string
---@field x integer
---@field y integer
---@field w integer
---@field h integer
---@field dragging boolean

--- A wheel tick over this plugin's pane. Declining puts it back on the key path.
---@class (exact) thurbox.Wheel
---@field up boolean
---@field x integer
---@field y integer

---@alias thurbox.Event
---| "session.created"
---| "session.deleted"
---| "session.status"
---| "session.changed"
---| "session.post_create"
---| "session.post_delete"
---| "session.post_restart"
---| "session.post_restore"
---| "focus.session"
---| "focus.pane"
---| "command.done"
---| "command.failed"
---| "interface.reloaded"
---| string A `user.<name>` a plugin emits.

--- A key a plugin declares. Declared as data so the registry can enumerate,
--- conflict-check and rebind it without calling the plugin.
---@class (exact) thurbox.Binding
---@field key string
---@field action string
---@field desc? string
---@field scope? "global"|"plugin"
---@field passthrough? boolean
---@field group? string

--- A palette row: an action reachable without a chord.
---@class (exact) thurbox.CommandDecl
---@field action string
---@field desc? string

--- An entry in the action band.
---@class (exact) thurbox.Pill
---@field action string
---@field label string
---@field priority? integer

--- A switch in the settings panel, owned by the declaring plugin.
---@class (exact) thurbox.SettingDecl
---@field id string
---@field desc? string
---@field default boolean|number|string

--- What a plugin file returns.
---
--- `render` is required unless the plugin `decorates` another, which draws
--- nothing of its own. A returned tree may carry `float` on its root.
---@class thurbox.Plugin
---@field name? string Defaults to the filename, minus a numeric ordering prefix.
---@field slot? string Defaults to `"center"`.
---@field slot_mode? "stack"|"switch"
---@field order? number Defaults to 100.
---@field focusable? boolean
---@field pure? boolean Cache the tree until an input changes.
---@field floats? boolean
---@field input? "session"
---@field size? thurbox.Size
---@field decorates? string A slot whose tree this plugin transforms.
---@field keys? thurbox.Binding[]
---@field pills? thurbox.Pill[]
---@field settings? thurbox.SettingDecl[]
---@field commands? thurbox.CommandDecl[]
---@field events? thurbox.Event[]
---@field capabilities? ("run"|"program")[]
---@field render? fun(ctx: thurbox.Ctx): thurbox.Node
---@field decorate? fun(node: thurbox.Node, ctx: thurbox.DecorateCtx): thurbox.Node
---@field on_key? fun(key: thurbox.Key): boolean
---@field on_action? fun(action: string): boolean
---@field on_click? fun(hit: thurbox.Hit): boolean
---@field on_scroll? fun(wheel: thurbox.Wheel): boolean
---@field on_event? fun(name: string, payload: table<string, any>)

--- What a pane asks of its slot, in the same vocabulary a node uses.
---@class (exact) thurbox.Size
---@field len? integer
---@field pct? number
---@field fill? number
---@field min? integer
---@field max? integer

---@alias thurbox.Role
---| "accent"
---| "accent_bright"
---| "status_working"
---| "status_blocked"
---| "status_done"
---| "status_idle"
---| "status_error"
---| "status_unreachable"
---| "text_primary"
---| "text_secondary"
---| "text_muted"
---| "border_focused"
---| "border_unfocused"
---| "role_name"
---| "branch_name"
---| "search_bar"
---| "keybind_hint"
---| "tool_allowed"
---| "tool_disallowed"
---| "danger"
---| "selection_bg"
---| "selection_fg"
---| "modal_dim_bg"
---| "modal_bg"
---| "modal_border"
---| "inverted_fg"
---| "diff_added"
---| "diff_removed"
---| "diff_added_bg"
---| "diff_removed_bg"
---| "app_bg"

--- A session's derived status, as `SessionState::as_str` spells it.
--- `lib/theme.lua` has a glyph and a role for `working`, `blocked`, `done`,
--- `idle` and `unreachable`, and falls back to `idle` for the rest — which is
--- why `theme.status` accepts the whole vocabulary.
---@alias thurbox.Status
---| "working"
---| "blocked"
---| "done"
---| "idle"
---| "unreachable"
---| "stopped"
---| "running"
---| "uncovered"
---| "unreported"

--- Uncommitted work in a session's tree. Absent until it has been measured,
--- which a plugin must be able to tell apart from a clean tree.
---@class (exact) thurbox.GitStats
---@field files integer
---@field insertions integer
---@field deletions integer
---@field untracked integer
---@field dirty boolean
---@field ahead integer
---@field behind integer
---@field merged? boolean Absent when nobody could say.

---@class (exact) thurbox.Session
---@field id string
---@field name string
---@field agent string
---@field status thurbox.Status
---@field backend string
---@field repo? string
---@field branch? string
---@field base_branch? string What a diff is taken against.
---@field host? string
---@field parent? string
---@field cwd? string
---@field worktrees integer
---@field repos string[]
---@field activity? string What the agent said about itself.
---@field notification? string
---@field display_order integer
---@field git? thurbox.GitStats
---@field attach_error? string Why this session's terminal is not live.

---@class (exact) thurbox.DeletedSession
---@field id string
---@field name string
---@field agent string
---@field deleted_at integer Epoch millis, against `thurbox.taken_at_ms`.
---@field worktrees integer
---@field partial boolean Restoring recovers committed work only.
---@field restore_refusal? string Set when the restore would refuse, and why.

---@class (exact) thurbox.Repo
---@field path string
---@field name string

---@class (exact) thurbox.Agent
---@field name string
---@field command string

---@class (exact) thurbox.Host
---@field name string
---@field detail string
---@field backend string

---@class (exact) thurbox.Task
---@field id integer
---@field title string
---@field description? string
---@field status string
---@field source string
---@field url? string
---@field created_at integer Epoch seconds.
---@field updated_at integer

---@class (exact) thurbox.AutomationRun
---@field started_at integer
---@field status string
---@field detail string

---@class (exact) thurbox.Automation
---@field id integer
---@field name string
---@field schedule string
---@field action string
---@field enabled boolean
---@field last_outcome? string
---@field last_detail? string
---@field runs thurbox.AutomationRun[]

--- A command this interface accepted but has not finished.
---@class (exact) thurbox.InFlight
---@field id integer
---@field kind string
---@field session string
---@field phase string
---@field subject? string
---@field error? string

---@class (exact) thurbox.DiffFile
---@field path string
---@field added integer
---@field removed integer
---@field status string
---@field old_path? string

--- A session's diff. Branch on `state`.
---@class (exact) thurbox.Diff
---@field state "pending"|"ready"|"failed"
---@field error? string
---@field truncated? boolean
---@field raw_bytes? integer
---@field untracked_omitted? integer
---@field files? thurbox.DiffFile[]
---@field body? string[]

--- A link found in a session's terminal, at the cell it starts on.
---@class (exact) thurbox.Link
---@field url string
---@field row integer
---@field col integer

--- An answer to a program this plugin asked to run. Branch on `state`: a
--- program that has not answered is not one that answered with nothing.
---@class (exact) thurbox.Run
---@field state "pending"|"done"|"failed"
---@field error? string
---@field stdout? string
---@field stderr? string
---@field status? integer
---@field truncated? boolean
---@field timed_out? boolean
---@field ok? boolean

---@class (exact) thurbox.Features
---@field tasks boolean
---@field automations boolean
---@field file_viewer boolean
---@field global_search boolean
---@field info_panel boolean
---@field shell_pane boolean
---@field code_review boolean
---@field perf_hud boolean
---@field mouse boolean
---@field notifications boolean
---@field soft_delete boolean
---@field version_check boolean
---@field auto_update boolean

--- The settings in force. Read your own switch and decline to draw when it is
--- off — the kernel gates only what it owns.
---@class (exact) thurbox.Settings
---@field features thurbox.Features
---@field two_panel_min_cols integer
---@field three_panel_min_cols integer
---@field scrollback_lines integer

---@class (exact) thurbox.BookmarkRow
---@field path string
---@field name string
---@field parent? string
---@field label? string
---@field is_parent boolean
---@field offered boolean
---@field is_git? boolean

--- Remembered repositories, served only while `store.want_bookmarks` asks.
---@class (exact) thurbox.Bookmarks
---@field host string
---@field loading boolean
---@field rows thurbox.BookmarkRow[]

---@class (exact) thurbox.BrowseEntry
---@field name string
---@field is_git boolean

--- A directory listing, served only while `store.want_browse` asks.
---@class (exact) thurbox.Browse
---@field host string
---@field dir string
---@field loading boolean
---@field error? string
---@field entries thurbox.BrowseEntry[]

--- Base branches, served only while `store.want_branches` asks.
---@class (exact) thurbox.Branches
---@field host string
---@field repo string
---@field loading boolean
---@field error? string
---@field list string[]

---@class (exact) thurbox.WorktreeEntry
---@field path string
---@field branch string

--- The worktrees a repo already has, served while `store.want_worktrees` asks.
---@class (exact) thurbox.Worktrees
---@field host string
---@field repo string
---@field loading boolean
---@field error? string
---@field list thurbox.WorktreeEntry[]

---@class (exact) thurbox.SystemMetrics
---@field cpu_percent number
---@field memory_used integer
---@field memory_total integer

---@class thurbox.SessionMetrics
---@field cpu_percent? number
---@field memory_bytes? integer
---@field agent? table<string, any>
---@field usage? table<string, any>

---@class (exact) thurbox.Metrics
---@field system thurbox.SystemMetrics
---@field sessions table<string, thurbox.SessionMetrics>

--- The machine this is running on, from the values the binary was built for.
---@class (exact) thurbox.Platform
---@field os string
---@field arch string

--- What the pointer is over, as whichever of the two the affordance was marked
--- with. Empty when nothing is hovered.
---@class (exact) thurbox.Hover
---@field id? string
---@field role? string

---@class (exact) thurbox.ThemeChoice
---@field name string
---@field display_name string
---@field light boolean
---@field custom boolean

---@class (exact) thurbox.ThemeSnapshot
---@field name string
---@field roles table<thurbox.Role, thurbox.Color>
---@field choices thurbox.ThemeChoice[]

---@class (exact) thurbox.RegistryKey
---@field plugin string
---@field action string
---@field key string
---@field default_key string
---@field desc string
---@field scope "global"|"plugin"
---@field rebound boolean
---@field group string

---@class (exact) thurbox.RegistrySetting
---@field plugin string
---@field id string
---@field desc string
---@field type string
---@field value boolean|number|string
---@field default boolean|number|string

--- What every plugin declared, so help and settings render from it.
---@class (exact) thurbox.RegistrySnapshot
---@field keys thurbox.RegistryKey[]
---@field settings thurbox.RegistrySetting[]
---@field sections string[] The order help renders its sections in.

--- One of the interface's own files: where it came from, and whether it runs.
---@class (exact) thurbox.PluginRow
---@field path string
---@field name string
---@field kind string
---@field slot string
---@field source string
---@field state string
---@field error? string

--- What the arrangement needs to know about the bands, and no more.
---@class (exact) thurbox.Chrome
---@field status_rows integer

--- Capabilities THIS plugin has been granted. A boolean about a decision the
--- user already made; it grants nothing.
---@class (exact) thurbox.Granted
---@field run? boolean
---@field program? boolean

--- Everything readable. Rebuilt each publish; a group whose inputs did not move
--- is handed back as the same table, which is what `lib/theme.lua` memoizes on.
---@class (exact) thurbox.Api
---@field sessions thurbox.Session[]
---@field deleted thurbox.DeletedSession[]
---@field repos thurbox.Repo[]
---@field agents thurbox.Agent[]
---@field agent_default string What a bare launch would use.
---@field settings thurbox.Settings
---@field bookmarks thurbox.Bookmarks
---@field browse thurbox.Browse
---@field branches thurbox.Branches
---@field worktrees thurbox.Worktrees
---@field hosts thurbox.Host[]
---@field tasks thurbox.Task[]
---@field automations thurbox.Automation[]
---@field commands thurbox.InFlight[]
---@field diffs table<string, thurbox.Diff>
---@field links table<string, thurbox.Link[]>
---@field content table<string, string> Served while `store.want_content` asks.
---@field runs table<string, thurbox.Run> Answers to THIS plugin's runs.
---@field granted thurbox.Granted
---@field metrics thurbox.Metrics
---@field platform thurbox.Platform
---@field version string
---@field reloads integer
---@field can_open_links boolean
---@field taken_at_ms integer When these rows were read.
---@field error? string
---@field focus string Which pane holds focus, by name.
---@field hover thurbox.Hover
---@field plugins thurbox.PluginRow[]
---@field ui_dir string
---@field chrome thurbox.Chrome
---@field theme thurbox.ThemeSnapshot
---@field registry thurbox.RegistrySnapshot
thurbox = {}

--- Persistent, shared by every plugin — the bus between them. Survives a
--- reload, not a restart.
---@type table<string, any>
store = {}

--- Persistent and private to the declaring plugin. Same lifetime as `store`.
---@type table<string, any>
state = {}

---@class (exact) thurbox.FileEntry
---@field name string
---@field dir boolean

--- Directory entries and file text, rooted at a session's working directory.
--- Not a filesystem: a path outside the root is refused.
---@class thurbox.Files
files = {}

---@param session string
---@param path? string Relative to the session's root; defaults to the root.
---@return thurbox.FileEntry[]
function files.list(session, path) end

---@param session string
---@param path string
---@return string
function files.read(session, path) end

---@class (exact) thurbox.RunOpts
---@field session? string Run in this session's directory.
---@field ttl? number Seconds an answer stays fresh.
---@field timeout? number Seconds before the program is given up on.
---@field refresh? boolean Ask again even if a fresh answer is held.

--- Ask for a program and read the answer later, from `thurbox.runs[key]`.
---
--- Queued, never executed here — a plugin cannot call anything that waits.
--- Absent unless this plugin declares `capabilities = { "run" }` AND the user
--- has trusted it, so `if not run then` is the honest check.
---@param key string
---@param program string
---@param opts? thurbox.RunOpts
function run(key, program, opts) end

--- Loads any `.lua` under the interface directory, and nothing outside it.
---@param name string
---@return any
function require(name) end

--- A user event. Not exact: every other scalar on the table travels as the
--- event's payload, which is the one place an unknown key is meaningful.
---@class thurbox.cmd.Emit
---@field text string The event's name; subscribers see `user.<name>`.

---@class (exact) thurbox.cmd.Plugin
---@field file string A path within the interface directory.
---@field action "restore"|"remove"

---@class (exact) thurbox.cmd.Set
---@field text string A `plugin.setting` key.
---@field flag? boolean
---@field number? number
---@field reset? boolean Put the setting back to its default.

---@class (exact) thurbox.cmd.Task
---@field text? string A title: creates when there is no `number`.
---@field number? integer An existing task's id.
---@field status? string
---@field reset? boolean Delete it.

---@class (exact) thurbox.cmd.Dispatch
---@field number integer The task to dispatch.
---@field session? string

---@class (exact) thurbox.cmd.Automation
---@field number integer
---@field flag? boolean Enable or disable it.
---@field force? boolean Run it now.
---@field reset? boolean Delete it.

---@class (exact) thurbox.cmd.ExtraMember
---@field path string
---@field worktree? boolean

---@class (exact) thurbox.cmd.Create
---@field repo string
---@field text? string The session's name.
---@field branch? string
---@field base? string
---@field worktree_path? string An existing worktree to open rather than make.
---@field agent? string
---@field host? string
---@field extras? thurbox.cmd.ExtraMember[] Further repositories to span.

---@class (exact) thurbox.cmd.Bookmark
---@field repo string The path to remember or forget.
---@field action "add"|"remove"|"parent"
---@field host? string

---@class (exact) thurbox.cmd.Focus
---@field text string The plugin to focus.
---@field toggle? boolean Return to the previous pane when it already has focus.

---@class (exact) thurbox.cmd.Open
---@field text string The url. `url` is not read.

---@class (exact) thurbox.cmd.Theme
---@field text string The theme's name.

---@class (exact) thurbox.cmd.Order
---@field list string[] Every session id, in the order wanted.

---@class (exact) thurbox.cmd.Program
---@field text string This plugin's name for the pane.
---@field repo? string The program to run; required unless closing.
---@field args? string[]
---@field action? "close"|"stop"

---@class (exact) thurbox.cmd.Delete
---@field session string
---@field force? boolean Skip the undo window.

---@class (exact) thurbox.cmd.Restore
---@field session string
---@field force? boolean Restore what can be restored.

---@class (exact) thurbox.cmd.Session
---@field session string

---@class (exact) thurbox.cmd.Send
---@field session string
---@field text string

---@class (exact) thurbox.cmd.Reorder
---@field session string
---@field delta integer Non-zero.

---@class (exact) thurbox.cmd.Fork
---@field session string
---@field text? string The new session's name.

---@alias thurbox.Verb
---| "emit" | "plugin" | "set" | "task" | "dispatch" | "automation"
---| "create" | "bookmark" | "focus" | "open" | "theme" | "order" | "program"
---| "delete" | "restore" | "restart" | "send" | "reorder" | "fork"
---| "sync" | "copy" | "diff" | "shell" | "editor"

--- The only way a plugin changes anything. Enqueues and returns; it never runs
--- the operation, which is why a plugin cannot stall the loop.
---
--- An option name no verb reads is collected and ignored, so the overloads
--- below name what each verb actually requires.
---@overload fun(verb: "emit", opts: thurbox.cmd.Emit)
---@overload fun(verb: "plugin", opts: thurbox.cmd.Plugin)
---@overload fun(verb: "set", opts: thurbox.cmd.Set)
---@overload fun(verb: "task", opts: thurbox.cmd.Task)
---@overload fun(verb: "dispatch", opts: thurbox.cmd.Dispatch)
---@overload fun(verb: "automation", opts: thurbox.cmd.Automation)
---@overload fun(verb: "create", opts: thurbox.cmd.Create)
---@overload fun(verb: "bookmark", opts: thurbox.cmd.Bookmark)
---@overload fun(verb: "focus", opts: thurbox.cmd.Focus)
---@overload fun(verb: "open", opts: thurbox.cmd.Open)
---@overload fun(verb: "theme", opts: thurbox.cmd.Theme)
---@overload fun(verb: "order", opts: thurbox.cmd.Order)
---@overload fun(verb: "program", opts: thurbox.cmd.Program)
---@overload fun(verb: "delete", opts: thurbox.cmd.Delete)
---@overload fun(verb: "restore", opts: thurbox.cmd.Restore)
---@overload fun(verb: "restart", opts: thurbox.cmd.Session)
---@overload fun(verb: "send", opts: thurbox.cmd.Send)
---@overload fun(verb: "reorder", opts: thurbox.cmd.Reorder)
---@overload fun(verb: "fork", opts: thurbox.cmd.Fork)
---@overload fun(verb: "sync", opts: thurbox.cmd.Session)
---@overload fun(verb: "copy", opts: thurbox.cmd.Session)
---@overload fun(verb: "diff", opts: thurbox.cmd.Session)
---@overload fun(verb: "shell", opts: thurbox.cmd.Session)
---@overload fun(verb: "editor", opts: thurbox.cmd.Session)
---@param verb thurbox.Verb
---@param opts? table
function command(verb, opts) end
