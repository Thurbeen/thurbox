---@meta

---@alias thurbox.Color string A role's colour, `#rrggbb`, a name, or a 0-255 index.

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

---@class thurbox.Span
---@field text string|number|boolean
---@field style? thurbox.StyleSpec
---@field id? string
---@field role? string

---@alias thurbox.Line string|number|boolean|thurbox.Span|thurbox.Span[]

---@alias thurbox.Text thurbox.Line|thurbox.Line[]

---@class thurbox.Overlay
---@field top_left? thurbox.Text Rightward from the cell after the top-left corner.
---@field top_right? thurbox.Text Leftward from the cell before the top-right corner.
---@field bottom_left? thurbox.Text
---@field bottom_right? thurbox.Text
---@field right_column? thurbox.Text One run per inner row, down the right border column.

---@class thurbox.Frame
---@field title? thurbox.Text
---@field title_align? "left"|"center"|"centre"|"right"
---@field borders? "all"|"none"
---@field border_type? "rounded"|"square"|"plain"|"thick"
---@field border_style? thurbox.StyleSpec
---@field style? thurbox.StyleSpec
---@field padding? integer
---@field overlay? thurbox.Overlay

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

---@class (exact) thurbox.TextNode : thurbox.NodeCommon
---@field type? "text"|"paragraph"|"line"
---@field text thurbox.Text
---@field align? "left"|"center"|"centre"|"right"
---@field wrap? boolean
---@field scroll? integer
---@field style? thurbox.StyleSpec

---@class (exact) thurbox.BoxNode : thurbox.NodeCommon
---@field type? "box"|"vstack"|"hstack"|"column"|"row"|"stack"
---@field axis? "vertical"|"horizontal"|"column"|"row"|"v"|"h"
---@field gap? integer
---@field children? thurbox.Node[]

---@class (exact) thurbox.InputNode : thurbox.NodeCommon
---@field type? "input"|"field"
---@field value string
---@field cursor? integer
---@field placeholder? string
---@field focused? boolean
---@field style? thurbox.StyleSpec

---@class (exact) thurbox.SurfaceNode : thurbox.NodeCommon
---@field type? "surface"|"terminal"
---@field session? string
---@field program? string
---@field cells? thurbox.Line[]
---@field scroll? integer
---@field mark? integer A row to highlight, counted from the top of the surface — where a pane that scrolled a terminal to a line points at it.

---@alias thurbox.Node thurbox.TextNode|thurbox.BoxNode|thurbox.InputNode|thurbox.SurfaceNode

---@class (exact) thurbox.RootText : thurbox.TextNode
---@field float? thurbox.Float|boolean `true` takes the default size.
---@class (exact) thurbox.RootBox : thurbox.BoxNode
---@field float? thurbox.Float|boolean
---@class (exact) thurbox.RootInput : thurbox.InputNode
---@field float? thurbox.Float|boolean
---@class (exact) thurbox.RootSurface : thurbox.SurfaceNode
---@field float? thurbox.Float|boolean

---@alias thurbox.Root thurbox.RootText|thurbox.RootBox|thurbox.RootInput|thurbox.RootSurface

---@class (exact) thurbox.FloatAt
---@field x integer
---@field y integer

---@class (exact) thurbox.Float
---@field at? thurbox.FloatAt
---@field width? number
---@field height? number
---@field cols? integer
---@field rows? integer

---@class (exact) thurbox.Ctx
---@field width integer
---@field height integer
---@field focused boolean
---@field frame integer
---@field name string
---@field slot string
---@field elapsed number

---@class (exact) thurbox.DecorateCtx
---@field width integer
---@field height integer

---@class (exact) thurbox.LayoutCtx
---@field width integer
---@field height integer
---@field slots table<string, boolean>
---@field strips thurbox.Strip[] Slots named by panes that declared `strip = true`, once each, in load order.

---@class (exact) thurbox.Strip
---@field slot string
---@field len integer Its panes' declared `size.len` (1 when unsaid): summed in a stack slot, the tallest in a switch one.

---@class (exact) thurbox.Key
---@field key string
---@field char? string
---@field ctrl boolean
---@field alt boolean
---@field shift boolean
---@field cmd boolean

---@class (exact) thurbox.Hit
---@field id? string
---@field class string
---@field role? string
---@field x integer
---@field y integer
---@field w integer
---@field h integer
---@field screen_x integer The pressed cell on the screen, 0-based — what `float.at` takes.
---@field screen_y integer
---@field dragging boolean
---@field clicks integer 2 for the second press on the same node in quick succession, else 1.

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
---| "program.exited"
---| "interface.reloaded"
---| string A `user.<name>` a mod emits.

---@class (exact) thurbox.Binding
---@field key string
---@field action string
---@field desc? string
---@field scope? "global"|"mod"|"plugin" `plugin` is the deprecated spelling of `mod`.
---@field passthrough? boolean
---@field release? boolean Also call `on_action(action, { event = "release" })` when the key is let go; the press then arrives as `{ event = "press" }` and auto-repeat is swallowed — on a chord with `ctrl`, `alt` or `super`, since outside Windows a held text key (`v`, `space`) repeats as fresh presses. Needs a terminal reporting releases — see `thurbox.keyboard`.
---@field group? string

---@class (exact) thurbox.CommandDecl
---@field action string
---@field desc? string

---@class (exact) thurbox.ActionArgument
---@field name string
---@field kind "string"|"uuid"
---@field required? boolean

---@class (exact) thurbox.ActionDecl
---@field name string
---@field desc? string
---@field scope? "global"|"mod"|"plugin" `plugin` is the deprecated spelling of `mod`.
---@field effect? "read"|"ui-write"|"kernel-write"
---@field destructive? boolean
---@field args? thurbox.ActionArgument[]

---@class (exact) thurbox.Pill
---@field action string
---@field label string
---@field priority? integer

---@class (exact) thurbox.SettingDecl
---@field id string
---@field desc? string
---@field default boolean|number|string
---@field list? boolean A `;`-separated list of percent-escaped entries, shown as a count.
---@field choices? string[] The only values a string setting may take; the panel steps through them instead of offering a text field, and the default must be one of them.

---@class thurbox.Mod
---@field name? string Defaults to the filename, minus a numeric ordering prefix.
---@field slot? string Defaults to `"center"`.
---@field slot_mode? "stack"|"switch"
---@field order? number Defaults to 100.
---@field requires_api? integer The lowest `thurbox.api` this mod works with; a thurbox serving an older one refuses to load it and says why.
---@field focusable? boolean
---@field pure? boolean Cache the tree until an input changes.
---@field floats? boolean
---@field strip? boolean A full-width row above the bars, `size.len` high, placed without editing `layout.lua`.
---@field input? "session"
---@field size? thurbox.Size
---@field decorates? string A slot whose tree this mod transforms.
---@field keys? thurbox.Binding[]
---@field pills? thurbox.Pill[]
---@field settings? thurbox.SettingDecl[]
---@field commands? thurbox.CommandDecl[]
---@field actions? thurbox.ActionDecl[]
---@field events? thurbox.Event[]
---@field capabilities? ("run"|"program"|"sessions"|"files")[]
---@field render? fun(ctx: thurbox.Ctx): thurbox.Root
---@field decorate? fun(node: thurbox.Node, ctx: thurbox.DecorateCtx): thurbox.Node
---@field on_focus_cycle? fun(direction: "next"|"previous"): boolean A topmost open float: true consumes; false keeps the pane cycle.
---@field on_key? fun(key: thurbox.Key): boolean
---@field on_action? fun(action: string, args?: thurbox.ActionArgs): boolean
---@field on_click? fun(hit: thurbox.Hit): boolean
---@field on_context? fun(hit: thurbox.Hit): boolean A RIGHT press on the same node.
---@field on_outside? fun(hit: thurbox.Hit): boolean A float's: a press of either button that missed it while it held the pointer. `hit.id` is nil.
---@field on_scroll? fun(wheel: thurbox.Wheel): boolean
---@field on_event? fun(name: string, payload: table<string, any>)
---@field ui_state? fun(): table<string, string|number|boolean> Bounded public projection for `ui state`; omit private values.

---@class (exact) thurbox.Size
---@field len? integer
---@field pct? number
---@field fill? number
---@field min? integer
---@field max? integer

---@alias thurbox.ActionArgs table<string, string>|{ event: "press"|"release" }

-- BEGIN GENERATED by src/kernel/lua_api.rs: edit there, then `just lua-api`
---@alias thurbox.Role
---| "accent"
---| "accent_bright"
---| "app_bg"
---| "border_focused"
---| "border_unfocused"
---| "branch_name"
---| "danger"
---| "diff_added"
---| "diff_added_bg"
---| "diff_removed"
---| "diff_removed_bg"
---| "inverted_fg"
---| "keybind_hint"
---| "modal_bg"
---| "modal_border"
---| "modal_dim_bg"
---| "role_name"
---| "search_bar"
---| "selection_bg"
---| "selection_fg"
---| "status_background"
---| "status_blocked"
---| "status_done"
---| "status_error"
---| "status_idle"
---| "status_running"
---| "status_unknown"
---| "status_unreachable"
---| "status_working"
---| "text_muted"
---| "text_primary"
---| "text_secondary"
---| "tool_allowed"
---| "tool_disallowed"

--- A session's derived status, as `SessionState::as_str` spells it.
--- `lib/theme.lua` has a glyph and a role for `working`, `blocked`,
--- `background`, `done`, `idle`, `unreachable`, `running`, `uncovered`,
--- `unreported` and `failed`, and falls back to `idle` only for `stopped` —
--- which is at rest by definition. `failed` is a launch that died before its
--- agent reported anything (a failed agents.toml `setup`, or an early
--- exit). `background` is a turn at rest whose sub-work is still running,
--- so it is not finished. The three silences are drawn apart
--- from `idle` on purpose: `running` is "an agent holds the pane and said
--- nothing", `uncovered` is "wired to report nothing" and `unreported` is
--- "can report, has not yet". None of them is the agent saying it is at
--- rest.
---@alias thurbox.Status
---| "working"
---| "blocked"
---| "background"
---| "done"
---| "idle"
---| "unreachable"
---| "stopped"
---| "failed"
---| "running"
---| "uncovered"
---| "unreported"

--- Where a Fleet session's status came from: `hook` is the agent's own
--- report, `screen` a screen rule Fleet read it with, and `none` neither,
--- so the status is unknown.
---@alias thurbox.StatusSource
---| "hook"
---| "screen"
---| "none"

--- Uncommitted work in a session's tree. Absent until it has been measured,
--- which a mod must be able to tell apart from a clean tree.
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
---@field id string `<fleet>:<task>` for a Fleet session: two Fleets can each hold a Task of one name.
---@field name string
---@field fleet? string The Fleet the session's Task belongs to, as `settings.toml` names it.
---@field key? string The address a driver gave the session (`session create --key`); opaque, and unchanged by a rename.
---@field agent string What the row was created as.
---@field reports_as? string What a driver declared this session runs (`session reports-as`).
---@field detected_agent? string The registered agent observed in the pane, when it is not `agent`.
---@field status thurbox.Status
---@field status_source? thurbox.StatusSource Where a Fleet session's status came from; absent for a status Fleet's agent state did not decide.
---@field status_rule? string The screen rule, `<set>/<rule id>`, of a `screen` status.
---@field status_since_ms? integer When Fleet first saw this status from this source, in ms since the epoch.
---@field deleting_since_ms? integer When Fleet marked this session for deletion, in ms since the epoch. Present only on a deletion that has run past the time a teardown takes: one that has not is not listed.
---@field deleting_why? string Why Fleet has not finished that deletion: its `Released` condition.
---@field state_why? string Why a Fleet session is stopped, lost or gone, or stalled: `paused · idle`, `paused · on request`, `paused · resuming`, `stopped · outside Fleet`, `lost · window gone`, `lost · revive failed`, `lost · cannot resume`, `gone · session ended`, `stalled · <reason>`.
---@field backend string
---@field repo? string
---@field branch? string
---@field base_branch? string What a diff is taken against.
---@field host? string
---@field place? string The place the session's Task runs on, as its Fleet names it: `local` or `<runtime>/<place>`, the Task's own or its TaskConfig's. Nil until Fleet has resolved a Task that names none.
---@field parent? string
---@field cwd? string
---@field worktrees integer
---@field repos string[]
---@field activity? string What the agent said about itself.
---@field notification? string
---@field meta? table<string, string> The session's `ui.*` meta keys, by full key (`meta["ui.note"]`). Untrusted text anyone with the CLI can write: one line, control and invisible characters stripped, at most 256 characters, 32 keys. Absent when it has none.
---@field display_order integer
---@field git? thurbox.GitStats
---@field attach_error? string Why this session's terminal is not live.
---@field starting boolean Whether the session is starting: a start this interface sent, or a resume Fleet runs, still waiting for the session's terminal to attach.
---@field start_error? string Why the last start this interface sent did not bring the session's terminal back.

---@class (exact) thurbox.Repo
---@field path string
---@field name string

---@alias thurbox.Presence
---| "present"
---| "missing"
---| "unknown"

---@class (exact) thurbox.Agent
---@field name string A harness Fleet has a profile for.

--- A place a Fleet defines: where its sessions can run.
---@class (exact) thurbox.Host
---@field name string
---@field detail string
---@field backend string
---@field ready? boolean Whether the place's runtime can run a session now. Nil when it did not say.
---@field why? string Why the place is not ready.
---@field here boolean Whether the place is the machine its Fleet runs on: a runtime's `local` place.

--- A Fleet `settings.toml` lists: where sessions are managed.
---@class (exact) thurbox.Fleet
---@field name string
---@field transport string How it is reached: `local Fleet`, `ssh <destination>` or `wsl <distro>`.
---@field remote boolean It runs on another machine: its places are read there, through it.
---@field state "connecting"|"up"|"down"|"unreachable"|"unknown"|"incompatible" `up` while its manager holds the lease, `down` when none does.
---@field why? string Why it is not up.
---@field home? string A remote Fleet's home directory on its machine, once it has said. Nil for a local Fleet.
---@field agents? thurbox.Agent[] Nil until this Fleet has said which harnesses it has.
---@field places? thurbox.Host[] Every place this Fleet defines, `local` first. Nil until it answers.

--- A command this interface accepted but has not finished.
---@class (exact) thurbox.InFlight
---@field id integer
---@field kind string
---@field session string
---@field phase string
---@field subject? string A `create`'s repository name, or a `bookmark` write's path as issued.
---@field host? string The machine a creation will land on; nil for this one.
---@field error? string

--- One line a content search found, in a session's agent pane or its shell.
---@class (exact) thurbox.SearchHit
---@field session string
---@field shell boolean Found in the companion shell rather than the agent.
---@field text string The line, trimmed and windowed around the match.
---@field positions integer[] 1-based character indices of `text` that matched — what `lib.fuzzy.spans` lights.
---@field back integer Rows between the line and the bottom of its terminal.
---@field scroll integer The scrollback offset that puts the line on screen; 0 when it already is.
---@field row integer The screen row, from the top, the line is on once scrolled to `scroll`.
---@field exact boolean Every term matched as a substring, phrase or regex — none only as a subsequence.
---@field score integer Higher is better; hits arrive already ranked.

--- A finished content search (`kernel::search::Answer`).
---@class (exact) thurbox.SearchAnswer
---@field query string The `store.want_content` it answers.
---@field within? string The `store["want_content.sessions"]` it was limited to, if any.
---@field hits thurbox.SearchHit[] Best first, at most 200 (50 per session).
---@field total integer Matching lines found, before the cap.
---@field sessions integer Sessions whose terminals were read.
---@field lines integer Lines searched.
---@field ms number Time the worker took.
---@field error? string Why the query could not run — an invalid regex.

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

--- An answer to a program this mod asked to run. Branch on `state`: a
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
---@field shell_pane boolean
---@field perf_hud boolean
---@field mouse boolean
---@field notifications boolean
---@field version_check boolean
---@field auto_update boolean

--- The settings in force. Read your own switch and decline to draw when it is
--- off — the kernel gates only what it owns.
---@class (exact) thurbox.Settings
---@field features thurbox.Features
---@field two_panel_min_cols integer
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
---@field home? string The home `host`'s runtime named when its `~` was listed: a typed `~` there is this. Nil until it has been listed.
---@field loading boolean
---@field error? string
---@field entries thurbox.BrowseEntry[]

--- A worktree a repository already has, which a session can open as it is.
---@class (exact) thurbox.ExistingWorktree
---@field path string
---@field branch? string Its branch; absent when its HEAD is detached.

--- A repository's branches on this machine, its `origin` fetched first, served only while `store.want_branches` names it.
---@class (exact) thurbox.RepoBranches
---@field repo string
---@field loading boolean
---@field is_git boolean False when the directory is no git repository.
---@field error? string Why a git repository's branches could not be read; its worktree choices stay offered.
---@field fetch_error? string Why fetching `origin` failed: the branches are the refs already there.
---@field default? string The branch a new worktree starts from by default: the upstream's, `origin/<default>`, when there is an `origin`.
---@field names string[] The local branches, then the `origin/*` ones.
---@field worktrees thurbox.ExistingWorktree[] The worktrees the repository already has besides its own checkout.

---@class (exact) thurbox.SystemMetrics
---@field cpu_percent number
---@field memory_used integer
---@field memory_total integer

--- What an agent reported about its own turn, from its statusline. Every field
--- is absent rather than zero when the agent did not report it, so a panel
--- renders the rows it has — `publish.rs`'s `agent_metrics_table` drops a nil.
--- The counts arrive as Lua numbers rather than integers: they cross as `f64`.
---@class (exact) thurbox.AgentMetrics
---@field model? string The display name.
---@field model_id? string
---@field cli_version? string
---@field cost_usd? number
---@field duration_ms? number
---@field api_duration_ms? number
---@field lines_added? number
---@field lines_removed? number
---@field input_tokens? number
---@field output_tokens? number
---@field context_window? number
---@field context_used_percent? number
---@field current_input_tokens? number
---@field current_output_tokens? number
---@field cache_creation_tokens? number
---@field cache_read_tokens? number

---@class thurbox.SessionMetrics
---@field cpu_percent? number
---@field memory_bytes? integer
---@field agent? thurbox.AgentMetrics

---@class (exact) thurbox.Metrics
---@field system thurbox.SystemMetrics
---@field sessions table<string, thurbox.SessionMetrics>

--- The machine this is running on, from the values the binary was built for.
---@class (exact) thurbox.Platform
---@field os string
---@field arch string

--- Whether a key's release reaches a binding declared `release = true`:
--- `unsupported` (no kitty keyboard protocol, not Windows — only presses
--- arrive), `negotiated` (asked for and accepted, none seen yet) or `reported`
--- (one has arrived this run).
---@class (exact) thurbox.Keyboard
---@field releases "unsupported"|"negotiated"|"reported"

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
---@field nerd_font boolean
---@field choices thurbox.ThemeChoice[]

---@class (exact) thurbox.RegistryKey
---@field mod string
---@field plugin string Deprecated: the same name as `mod`.
---@field action string
---@field key string
---@field default_key string
---@field desc string
---@field scope "global"|"plugin"
---@field rebound boolean
---@field group string

--- A chord-less action a mod declares in `commands`, for the palette.
---@class (exact) thurbox.RegistryCommand
---@field mod string
---@field plugin string Deprecated: the same name as `mod`.
---@field action string
---@field desc string

---@class (exact) thurbox.RegistrySetting
---@field mod string
---@field plugin string Deprecated: the same name as `mod`.
---@field id string
---@field desc string
---@field type string
---@field value boolean|number|string
---@field default boolean|number|string
---@field choices? string[] Present only when the setting declared them.

--- What every mod declared, so help and settings render from it.
---@class (exact) thurbox.RegistrySnapshot
---@field keys thurbox.RegistryKey[]
---@field commands thurbox.RegistryCommand[]
---@field settings thurbox.RegistrySetting[]
---@field sections string[] The order help renders its sections in.

--- One of the interface's own files: where it came from, and whether it runs.
---@class (exact) thurbox.ModRow
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

--- Capabilities THIS mod has been granted. A boolean about a decision the
--- user already made; it grants nothing. Absent, like `thurbox.runs`, for a
--- mod that declares no capability.
---@class (exact) thurbox.Granted
---@field run? boolean
---@field program? boolean
---@field sessions? boolean Verbs that drive an agent or lose something for good are applied (docs/thurbox/mods.md).
---@field files? boolean `files` is present.

--- Everything readable. Rebuilt each publish; a group whose inputs did not move
--- is handed back as the same table, which is what `lib/theme.lua` memoizes on.
---@class (exact) thurbox.Api
---@field api integer The mod API contract number. It moves only when a field is removed or changes meaning; declare `requires_api` to need a newer one.
---@field sessions thurbox.Session[]
---@field repos thurbox.Repo[]
---@field agents? thurbox.Agent[] Nil until Fleet has said which harnesses it has.
---@field settings thurbox.Settings
---@field bookmarks thurbox.Bookmarks
---@field browse thurbox.Browse
---@field repo_branches thurbox.RepoBranches
---@field hosts? thurbox.Host[] Every place the Fleet in force defines, `local` first (its `backend` is empty) when Fleet defines it. Nil until Fleet answers.
---@field fleets thurbox.Fleet[] Every Fleet `settings.toml` lists, by name, each with its own agents and places.
---@field commands thurbox.InFlight[]
---@field diffs table<string, thurbox.Diff>
---@field links table<string, thurbox.Link[]> Keyed by SURFACE, not by session: a session's companion shell is `<id>#shell` and has links of its own.
---@field search? thurbox.SearchAnswer The content search's answer, while `store.want_content` asks. Nil until one lands.
---@field printing table<string, boolean> Sessions whose pane is producing output right now, keyed by id. The evidence `running` animates on — see `ui.status`.
---@field runs table<string, thurbox.Run> Answers to THIS mod's runs.
---@field granted thurbox.Granted
---@field metrics thurbox.Metrics
---@field platform thurbox.Platform
---@field keyboard thurbox.Keyboard
---@field version string
---@field reloads integer
---@field can_open_links boolean
---@field taken_at_ms integer When these rows were read.
---@field error? string
---@field focus string Which pane holds focus, by name.
---@field selection string The mouse text selection, empty when nothing is selected.
---@field hover thurbox.Hover
---@field mods thurbox.ModRow[]
---@field plugins thurbox.ModRow[] Deprecated: the same list as `mods`.
---@field ui_dir string
---@field chrome thurbox.Chrome
---@field theme thurbox.ThemeSnapshot
---@field registry thurbox.RegistrySnapshot
thurbox = {}
-- END GENERATED

---@type table<string, any>
store = {}

---@type table<string, any>
state = {}

---@param key string
---@return string?
function store_writer(key) end

---@class (exact) thurbox.FileEntry
---@field name string
---@field dir boolean True for a directory, and for a symlink to one inside the session.

---@class thurbox.Files
files = {}

---@param session string
---@param path? string Relative to the session's root; defaults to the root.
---@return thurbox.FileEntry[]
function files.list(session, path) end

---@param session string
---@param path string Relative to the session's root.
---@return string? text
function files.read(session, path) end

---@class (exact) thurbox.TruncateOpts
---@field ellipsis? string What marks the cut. `""` cuts without a mark.
---@field side? "right"|"left"|"middle" Which end is eaten. Defaults to `"right"`.

---@class thurbox.TextApi
text = {}

---@param str string
---@return integer
function text.width(str) end

---@param str string
---@param cols integer
---@param opts? string|thurbox.TruncateOpts
---@return string
function text.truncate(str, cols, opts) end

---@param str string
---@param cols integer
---@param align? "left"|"right"|"center"|"centre"
---@return string
function text.pad(str, cols, align) end

---@class (exact) thurbox.RunOpts
---@field session? string Required nonempty ID for machine="session"; run in this session's directory, on its machine.
---@field machine? string "session" (default), "local", or "host:<exact-configured-name>"; local/host need no session and run in the target home.
---@field ttl? number Seconds an answer stays fresh.
---@field timeout? number Seconds before the program is given up on.
---@field refresh? boolean Ask again even if a fresh answer is held.

---@param key string
---@param program string
---@param opts thurbox.RunOpts
function run(key, program, opts) end

---@param key string Nonempty key, implicitly scoped to this mod.
function run_forget(key) end

---@param name string
---@return any
function require(name) end

---@class thurbox.cmd.Emit
---@field text string The event's name; subscribers see `user.<name>`.

---@class (exact) thurbox.cmd.Mod
---@field file string A path within the interface directory.
---@field action "restore"|"remove"

---@class (exact) thurbox.cmd.Set
---@field text string A `plugin.setting` key.
---@field flag? boolean
---@field number? number
---@field value? string A text setting's value.
---@field reset? boolean Put the setting back to its default.

---@class (exact) thurbox.cmd.Task
---@field text? string A title: creates when there is no `number`.
---@field number? integer An existing task's id.
---@field status? string `todo`, `in_progress` or `done`.
---@field reset? boolean Delete it.

---@class (exact) thurbox.cmd.Dispatch
---@field number integer The task to dispatch.
---@field session? string A session to type it into instead of the task's own action.

---@class (exact) thurbox.cmd.CreateExtra
---@field path string An absolute path on the session's host (`~` only on this machine).
---@field worktree? boolean A worktree of its own on the session's branch, made from this repository's default branch; else the directory as it is.

---@class (exact) thurbox.cmd.Create
---@field repo string An absolute path on the session's host (`~` only on this machine).
---@field text? string The session's name.
---@field worktree? boolean A worktree of its own, or the directory as it is. Nil: a worktree for a git repository on this machine, the directory otherwise.
---@field branch? string The worktree's branch, existing or new. Nil: a new branch named after the session.
---@field base? string What a new branch starts from. Nil: the repository's default branch.
---@field extras? thurbox.cmd.CreateExtra[] The other repositories, in order. Each worktree is on `branch`.
---@field agent? string
---@field host? string The place, as `thurbox.fleets[i].places[j].backend` names it. Nil: the Fleet's `local`.
---@field fleet? string The Fleet the session is a Task of, by name. Nil: the first listed.

---@class (exact) thurbox.cmd.Bookmark
---@field repo string The path to remember or forget.
---@field action "add"|"remove"|"parent"|"create"|"init"|"clone" `create`/`init`/`clone` make the directory first (it must not exist, or be empty), then remember it.
---@field text? string The URL to clone, for `clone`.
---@field host? string

---@class (exact) thurbox.cmd.Focus
---@field text string The mod to focus.
---@field toggle? boolean Return to the previous pane when it already has focus.

---@class (exact) thurbox.cmd.Open
---@field text string The url. `url` is not read.

---@class (exact) thurbox.cmd.Theme
---@field text string The theme's name.

---@class (exact) thurbox.cmd.Order
---@field list string[] Every session id, in the order wanted.

---@class (exact) thurbox.cmd.Program
---@field text string This mod's name for the pane.
---@field repo? string The program to run; required unless closing or typing.
---@field args? string[]
---@field action? "close"|"stop"
---@field keys? string Type this into the program already running in the pane.

---@class (exact) thurbox.cmd.Delete
---@field session string
---@field force? boolean Skip the undo window.
---@field remember? thurbox.cmd.Remember Store an undo target only when the soft delete is issued.

---@class (exact) thurbox.cmd.Remember
---@field key string
---@field value string

---@class (exact) thurbox.cmd.Restore
---@field session string
---@field force? boolean Restore what can be restored.

---@class (exact) thurbox.cmd.Restart
---@field session string
---@field fresh? boolean Start a new conversation instead of resuming this one.

---@class (exact) thurbox.cmd.Session
---@field session string A session id, or for `copy` a surface: `<id>#shell` reads the companion shell's screen rather than the agent's.

---@class (exact) thurbox.cmd.Send
---@field session string
---@field text string

---@class (exact) thurbox.cmd.Reorder
---@field session string
---@field delta integer Non-zero.

---@class (exact) thurbox.cmd.Fork
---@field session string
---@field text? string The new session's name.

---@class (exact) thurbox.cmd.Rename
---@field session string
---@field text string The name to give it.

---@class (exact) thurbox.cmd.Action
---@field text string The action id. `action` is not read.
---@field session? string Pass this row as the action's `session_id` argument.
---@field target? string Pass this row as the action's `target` argument.

---@class (exact) thurbox.cmd.Message
---@field text string
---@field level? "info"|"success"|"error" Defaults to `info`; anything else is refused.

---@alias thurbox.Verb
---| "emit" | "mod" | "plugin" | "set" | "task" | "dispatch"
---| "create" | "bookmark" | "focus" | "open" | "theme" | "order" | "program"
---| "delete" | "restore" | "restart" | "start" | "pause" | "send" | "reorder" | "fork"
---| "sync" | "rename" | "copy" | "diff" | "shell" | "editor" | "action" | "message"

---@overload fun(verb: "emit", opts: thurbox.cmd.Emit)
---@overload fun(verb: "mod", opts: thurbox.cmd.Mod)
---@overload fun(verb: "plugin", opts: thurbox.cmd.Mod)
---@overload fun(verb: "set", opts: thurbox.cmd.Set)
---@overload fun(verb: "task", opts: thurbox.cmd.Task)
---@overload fun(verb: "dispatch", opts: thurbox.cmd.Dispatch)
---@overload fun(verb: "create", opts: thurbox.cmd.Create)
---@overload fun(verb: "bookmark", opts: thurbox.cmd.Bookmark)
---@overload fun(verb: "focus", opts: thurbox.cmd.Focus)
---@overload fun(verb: "open", opts: thurbox.cmd.Open)
---@overload fun(verb: "theme", opts: thurbox.cmd.Theme)
---@overload fun(verb: "order", opts: thurbox.cmd.Order)
---@overload fun(verb: "program", opts: thurbox.cmd.Program)
---@overload fun(verb: "delete", opts: thurbox.cmd.Delete)
---@overload fun(verb: "restore", opts: thurbox.cmd.Restore)
---@overload fun(verb: "restart", opts: thurbox.cmd.Restart)
---@overload fun(verb: "start", opts: thurbox.cmd.Session)
---@overload fun(verb: "pause", opts: thurbox.cmd.Session)
---@overload fun(verb: "send", opts: thurbox.cmd.Send)
---@overload fun(verb: "reorder", opts: thurbox.cmd.Reorder)
---@overload fun(verb: "fork", opts: thurbox.cmd.Fork)
---@overload fun(verb: "sync", opts: thurbox.cmd.Session)
---@overload fun(verb: "rename", opts: thurbox.cmd.Rename)
---@overload fun(verb: "copy", opts: thurbox.cmd.Session)
---@overload fun(verb: "diff", opts: thurbox.cmd.Session)
---@overload fun(verb: "shell", opts: thurbox.cmd.Session)
---@overload fun(verb: "editor", opts: thurbox.cmd.Session)
---@overload fun(verb: "action", opts: thurbox.cmd.Action)
---@overload fun(verb: "message", opts: thurbox.cmd.Message)
---@param verb thurbox.Verb
---@param opts? table
function command(verb, opts) end

---@deprecated Use `thurbox.Mod`.
---@alias thurbox.Plugin thurbox.Mod

---@deprecated Use `thurbox.ModRow`.
---@alias thurbox.PluginRow thurbox.ModRow

---@deprecated Use `thurbox.cmd.Mod`.
---@alias thurbox.cmd.Plugin thurbox.cmd.Mod
