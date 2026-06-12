# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code)
when working with code in this repository.

## Project

Thurbox is a multi-session coding-agent TUI orchestrator built
with Rust. It runs multiple coding-agent CLI instances (Claude
Code, Codex, Gemini CLI, opencode, aider, … — any CLI you
define) inside persistent tmux sessions, rendered as terminal
panels via ratatui + tui-term. Sessions survive crashes/restarts
because tmux keeps the processes alive.

Each session picks **which agent** to run from a declarative
registry (`~/.config/thurbox/agents.toml`). Thurbox is
agent-neutral: it knows nothing about any agent's model,
permissions, prompts, or tools — only how to launch the CLI with
the right `command + args`. Each agent uses its own default
config (bake a model or other flags into the agent's `args` if
you want them).

## Build & Development Commands

```bash
cargo check --all                    # Type check
cargo build                          # Debug build
cargo build --release                # Release build (LTO, stripped)
cargo run                            # Run in dev mode
```

## Testing

```bash
cargo nextest run --all              # Run all tests (preferred runner)
cargo nextest run -E 'test(name)'    # Run a single test by name
cargo nextest run --all --profile ci # Run with CI profile
cargo test test_name                 # Run single test via cargo test
bats scripts/install.bats            # Test install script (requires bats-core)
```

## Installation Script

**Location:** `scripts/install.sh`

One-liner installation for end users:

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Features:**

- Platform detection (Linux/macOS, x86_64/aarch64)
- Automatic version fetching with API rate limit fallback (scrapes releases page)
- SHA256 checksum verification
- Creates `~/.local/bin` if needed
- Post-install instructions
- Graceful error handling with helpful messages

**Environment variables:**

- `VERSION=v0.1.0` - Install specific version (default: latest from GitHub API)
- `INSTALL_DIR=/path` - Custom install directory (default: `~/.local/bin`)

**Testing:**

- Comprehensive test suite in `scripts/install.bats` using bats-core framework
- 25 tests covering platform detection, checksum verification, binary extraction, and error handling
- Run tests locally: `bats scripts/install.bats`
- CI runs tests automatically on every commit

**Implementation notes:**

- POSIX shell (`#!/usr/bin/env sh`) for maximum compatibility
- No external dependencies beyond standard tools (curl/wget, tar, sha256sum/shasum)
- Non-interactive for safe pipe-to-shell execution
- Proper error handling and cleanup via trap

## Linting & Formatting

```bash
cargo fmt --all                      # Format (rustfmt: 100 char max)
cargo clippy --all-targets --all-features -- -D warnings  # Lint
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features  # Docs
rumdl check .                        # Markdown lint (.rumdl.toml)
rumdl fmt .                          # Markdown auto-fix
```

## Website Linting

```bash
npm ci                               # Install deps (use lockfile)
npm run lint:website                 # Run all website linters
npm run fmt:website                  # Auto-fix formatting (Prettier)
```

## Architecture Enforcement

```bash
cargo test --test architecture_rules                      # Arch rules
cargo deny check advisories                               # Advisories
cargo deny check bans licenses sources                    # Dep policy
```

## Release Process

Releases are **fully automated** via GitHub Actions. No version commits
are created - version is determined by git tags only.

### How It Works

Every push to `main` automatically triggers the release workflow:

1. **Commit Analysis**: Analyzes all commits since last tag using cocogitto
2. **Release Decision**:
   - **If** commits include `feat`, `fix`, or `perf` → creates release
   - **If** only docs/chore/ci commits → no release (workflow exits)
3. **Automated Release** (if needed):
   - Determines semantic version (feat→minor, fix/perf→patch)
   - Creates lightweight git tag: `v{version}` (e.g., v0.1.0)
   - Pushes tag to origin
   - Builds binaries for 3 platforms (version passed via environment variable)
   - Generates changelog from commits
   - Publishes GitHub Release with binaries and release notes

### Version Management

- **Cargo.toml version**: Always `0.0.0-dev` (static development marker)
- **Real version**: Determined by release workflow (v0.1.0, v0.2.0, etc.)
- **Build-time injection**: `build.rs` uses `THURBOX_RELEASE_VERSION` environment
  variable (set by workflow) to inject version into binary
- **Development builds**: Show `0.0.0-dev` (when `THURBOX_RELEASE_VERSION` not set)
- **Release builds**: Show actual version (e.g., `0.1.0`) via env variable from workflow

### Release Artifacts

Each release includes:

- Binaries for 3 platforms:
  - `thurbox-v{ver}-x86_64-unknown-linux-gnu.tar.gz`
  - `thurbox-v{ver}-x86_64-unknown-linux-musl.tar.gz`
  - `thurbox-v{ver}-aarch64-apple-darwin.tar.gz`
- `thurbox-v{ver}-checksums.txt` (SHA256 sums for verification)
- Changelog with categorized commits

### Distribution Packages

After the GitHub Release is published, `cd.yml` also updates the downstream
package channels (each gated on its secret, skipped on forks):

- **Homebrew** (`publish-homebrew`): bumps `version`/`sha256` in
  `packaging/homebrew/Formula/thurbox.rb` (via `packaging/homebrew/bump-formula.py`,
  reading the release `checksums.txt`) and pushes it to the
  `Thurbeen/homebrew-thurbox` tap over SSH. Needs the `HOMEBREW_TAP_DEPLOY_KEY`
  secret (a write deploy key on the tap repo; the org blocks cross-repo PATs).
  Install: `brew install thurbeen/thurbox/thurbox`. Supports macOS arm64
  (`aarch64-apple-darwin`) + Linux x86_64 (`x86_64-unknown-linux-musl`).
- **AUR** (`publish-aur`): bumps + pushes `thurbox`/`thurbox-bin` PKGBUILDs.
  Needs `AUR_SSH_PRIVATE_KEY`.

See `packaging/README.md` for the full packaging overview.

### Commit Types and Versioning

- **feat**: Minor version bump (0.x.0)
- **fix, perf**: Patch version bump (0.0.x)
- **docs, chore, ci, style, test**: No release (appear in next version)
- **BREAKING CHANGE**: Major version bump (x.0.0) - use cautiously for 0.x

## Conventional Commits

All commits must follow
[Conventional Commits](https://www.conventionalcommits.org/).
Enforced by cocogitto via pre-commit hooks.

- **Types**: feat, fix, perf, refactor, docs, style, test,
  chore, ci, build, revert
- **Scopes**: api, cli, ui, git, core, docs, deps, config, mcp
- Use `cog commit feat "message"`
  or `cog commit fix "message" scope`

## Agent Definitions

The set of launchable coding agents is declared **as data** in
`~/.config/thurbox/agents.toml`, seeded with built-ins
(`claude`, `codex`, `gemini`, `opencode`, `aider`, `vibe`) on first run.
Each `[[agents]]` entry is an `AgentDef`:

```toml
default = "claude"

[[agents]]
name = "claude"
command = "claude"
args = []                               # always passed; bake a model here if you want one
resume_args = ["--resume", "{id}"]      # emitted when resuming
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]  # emitted on a fresh spawn

[[agents]]
name = "codex"
command = "codex"
resume_args = ["resume", "--last"]      # id-less: resumes the last session in cwd
fork_args = ["fork", "--last"]
resume_latest = true
```

Each `*_args` group is appended only when its driving value is
present, with `{id}` substituted; `args` is always passed. No
model is ever passed — each agent uses its own default config
(put `["--model", "opus"]` in `args` if you want to pin one).
Agents that omit `resume_args` simply start fresh on restart (the
live tmux process is what carries state across TUI restarts). Add
your own `[[agents]]` entry to support any CLI — no recompile.

**Session id pinning vs. `resume_latest`.** thurbox generates the
`agent_session_id` (a UUID) and only `claude` accepts it at creation
(`--session-id {id}`), so only claude can resume/fork by that exact id.
The other built-ins (`codex`, `opencode`, `gemini`, `aider`) can't pin
or report their id, so they set `resume_latest = true` with **id-less**
resume/fork flags (no `{id}` token): the agent resolves "the last
session in *this* directory" itself (`codex resume --last`, `opencode
--continue`, `gemini --resume latest`, `aider --restore-chat-history`).
This works because restart reuses the session's cwd and a single-repo
fork reuses the parent's cwd. `resume_latest` only changes *when* the
resume group fires (see `session_ops::resume_trigger_for`): for these
agents restart always triggers resume; for claude it still defers to an
on-disk transcript check. Caveats: agents without `fork_args`
(`gemini`, `aider` — neither CLI forks) start fresh on `Ctrl+F`; and a
**multi-repo** fork of a cwd-scoped agent lands in a fresh symlink
workspace, so `--last`/`--continue` finds no parent session (multi-repo
*restart* still resumes, since it keeps the same workspace dir).

- **Data type**: `session::AgentDef` / `session::AgentRegistry`
  (`session/agent_def.rs`, pure data + substitution logic).
- **Loading**: `agent::agent_config::load_or_seed()` reads/seeds
  the TOML; `builtin_registry()` is the fallback.
- **Launching**: `agent::GenericProvider` wraps an `AgentDef` and
  implements the `AgentProvider` trait (`command()` +
  `build_args(&SessionConfig)`). `App::provider_for(&config)`
  picks the provider for the session's agent.

A session stores only its **agent name**; there are no
per-session model/permission/prompt/tool knobs.

### Multi-repo sessions (symlink workspace)

A session can span several repositories (the repo picker allows
multiple). Because agent CLIs differ wildly in how — or whether —
they accept extra directories, thurbox does **not** pass per-agent
`--add-dir`-style flags. Instead, when a session has more than one
member directory it is launched in a per-session **symlink
workspace**: `~/.local/share/thurbox/workspaces/<agent_session_id>/`
holds one symlink per repo (worktree checkout or plain dir), and the
agent process is started there (`cwd` = the workspace). Every agent
then sees each repo as a subdirectory — fully agent-neutral, no
`agents.toml` changes.

`SessionInfo.cwd` keeps the **primary** repo (for display / editor /
git context); the workspace is a spawn-time process-cwd detail,
derived idempotently on every launch from the persisted members and
never stored. `workspace::ensure_workspace` / `remove_workspace`
(`src/workspace.rs`) build and tear it down; the member set is the
single `App::session_member_dirs` list that also feeds the rendered
repo names, and `App::resolve_process_cwd` picks workspace-vs-primary.
Single-repo sessions are unchanged (`cwd` = the repo directly).

## Remote SSH Sessions

Sessions can run on a **remote host** over SSH while the TUI runs
locally. Remote hosts are declared as data in
`~/.config/thurbox/hosts.toml` (seeded commented-out on first run,
so a fresh install has zero remote hosts and behaves as before). The
seeded file documents every field inline; the schema:

```toml
[[hosts]]
name = "devbox"               # required — backend id "ssh:devbox"; what --host expects
destination = "me@devbox"     # required — ssh target ("user@host" or ~/.ssh/config alias)
ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m", "-o", "ServerAliveInterval=15"]
                              # optional (default []) — extra ssh flags; no ~ expansion, use abs paths
socket = "thurbox"            # optional (default "thurbox") — remote `tmux -L` socket
session = "thurbox"           # optional (default "thurbox") — remote tmux session name
worktrees_dir = "/home/me/.local/share/thurbox/worktrees"
                              # optional — abs remote worktrees dir
                              # (default $HOME/.local/share/thurbox/worktrees, resolved over ssh)
```

| Field | Req | Default | Purpose |
|-------|-----|---------|---------|
| `name` | yes | — | unique id; registers backend `ssh:<name>` |
| `destination` | yes | — | ssh target, resolved via `~/.ssh/config` |
| `ssh_opts` | no | `[]` | extra `ssh` flags (one token per element; no `~` expansion) |
| `socket` | no | `thurbox` | remote `tmux -L` socket name |
| `session` | no | `thurbox` | remote tmux session name |
| `worktrees_dir` | no | `$HOME/.local/share/thurbox/worktrees` | abs remote worktrees dir |

How it works: `TmuxBackend` is transport-neutral
(`agent::transport::TmuxTransport`). The local backend launches
`tmux -L thurbox …`; a remote backend launches
`ssh <dest> tmux -L thurbox …`. The tmux **control-mode** protocol
(`control_mode.rs`) is byte-identical over either transport — only
the one-time process launch differs. Each host registers a backend
named `ssh:<name>` (`TmuxBackend::from_host`, registered lazily in
`main.rs`: a down host must not block startup, so
`check_available`/`ensure_ready` are deferred to first use via
`App::backend_for`).

- **Data**: `session::HostDef`/`HostRegistry` (pure data, in
  `session/` so both `agent` and `git` can use it). **Loading**:
  `agent::host_config::load_or_seed()`.
- **Selection**: `SessionConfig.backend` (`ssh:<host>` or `None` =
  local). The TUI new-session flow shows a **host picker** first
  (skipped when no hosts are configured); the chosen host runs git
  worktree creation + branch listing on that host over SSH.
- **Worktrees**: `git::*_on(host, …)` variants run `git` over
  `ssh <dest> git -C <repo> …`. Remote worktrees live under the
  host's `worktrees_dir` (or `$HOME/.local/share/thurbox/worktrees`
  resolved + cached over ssh).
- **Persistence/restore**: `backend_type` already round-trips in
  SQLite; restore discovers windows **per backend** so remote
  sessions re-adopt against their own host.
- **Headless**: `thurbox-cli session create --host <name>` spawns
  remotely (see below).
- **Local e2e**: `scripts/dev/remote-ssh-test.sh up` spins a
  throwaway Podman container (sshd + tmux + git) and `… test` runs
  an isolated headless smoke test asserting a session lands on the
  `ssh:podman` backend (state under `target/`, never touches your
  real `~/.ssh`/`~/.config`).

## thurbox-cli

A second binary (`thurbox-cli`) drives the same SQLite-backed,
tmux-hosted sessions headlessly (no TUI). It shares the database
with the TUI; changes appear via `PRAGMA data_version` polling.

```bash
cargo build --bin thurbox-cli
thurbox-cli session create --name demo --repo-path /path \
    --agent codex --worktree-branch feat/x
# Spawn on a remote host from hosts.toml (worktree + tmux live remotely):
thurbox-cli session create --name demo --repo-path /srv/repo \
    --host devbox --worktree-branch feat/x
# Spawn a worker under a lead session (parent must exist):
thurbox-cli session create --name worker --repo-path /path \
    --parent <lead-uuid>
thurbox-cli session list | jq
thurbox-cli session list --parent <lead-uuid> | jq  # direct children only
```

Subcommands: `session` (create/list/get/delete/restore/restart/
send/capture), `automation` (alias `auto`:
create/list/show/edit/remove/run/runs/tick), `task` (alias `todo`:
create/list/show/edit/remove/run), `editor`, `config`
(validate/show — strict-parses every config file / prints the
effective resolved config; see `docs/CONFIG.md`). Pass
`--pretty` for indented JSON.

`session delete <uuid>` **soft-deletes** by default — only the DB
row is marked deleted (the TUI tears down the tmux window/worktree
on its next sync), and `session restore` revives it. Pass `--force`
(`session_ops::delete_session_headless`) to also kill the tmux
window, remove worktrees + the symlink workspace, and disable
`send` automations targeting the session — for headless cleanup
when no TUI is running. Teardown is best-effort (failures land in
the JSON report); the row is always soft-deleted last, so even a
forced delete stays restorable.

### Parent sessions (lead/worker)

Sessions carry an optional **`parent_session_id`** so orchestration
scripts can model lead → worker relationships. `session create
--parent <uuid>` sets it (the parent must be an existing active
session — validated before any side effects); `session list`/`get`
emit it in the JSON (`null` for top-level sessions) and `session
list --parent <uuid>` filters to direct children. The link is
**purely informational**: deleting a parent never cascades to
children (orphans simply render as top-level), and the parent is
only validated at creation. In the TUI, **`Ctrl+F` fork** records
the source session as the fork's parent; the session list nests
children under their parent **within the same repo group** (muted
`└` tree prefix; a child whose parent renders in another group
keeps its own position with a `↳` mark instead), and the info panel
(F2) shows a `Parent:` row. The nesting lives in
`ui::project_list::compute_session_order` (`SessionOrder::depths`),
so `Ctrl+J`/`Ctrl+K` navigation follows the tree automatically.
Storage: nullable `sessions.parent_session_id` column (schema v30;
v29 is reserved by an in-flight branch).

### Manual session ordering

The session list is **manually orderable**: `Shift+J`/`Shift+K`
(while the session list is focused; rebindable
`SessionListMoveDown`/`SessionListMoveUp` actions) move the selected
session one row down/up. Manual order **wins** — status changes only
recolor the dot, never move a row. A move swaps two adjacent
*blocks* (a row plus its nested children, so a parent drags its
subtree): root rows swap within their repo group, the **whole
group** swaps past a group edge, and nested children move among
their siblings only (`ui::project_list::move_in_order`, pure;
`App::move_active_session` applies it). On every move all sessions
are densely renumbered `0..n` and persisted, so the order survives
restarts and syncs across instances via the existing
`data_version` polling. Storage: nullable `sessions.display_order`
column (schema v31); `None` = never moved, renders after ordered
sessions in creation order (new sessions append to their group).

Automations fire even when the TUI is closed: a tmux heartbeat
keeper window (`automation-heartbeat`, armed on TUI startup and on
`automation create`) loops `automation tick` every 60 s and keeps
the tmux server alive. `packaging/` ships opt-in systemd/launchd
units for reboot-proof firing. Concurrent firers are de-duplicated
by `Database::claim_due_automation` (atomic CAS), so the TUI, the
keeper, and an OS timer never double-fire.

In the TUI, automations also get a dedicated **Automations pane**
beneath the session list (left column). It is always present
(showing `none` when empty) — unless disabled via `[features]
automations = false` in settings.toml, which hides the pane (the
session list takes the whole column and `j`/`k` wrap within it),
blocks `Ctrl+P`, stops the TUI firing schedules, and skips arming the
heartbeat (the CLI surface stays fully functional) — and is treated
as **part of the session pane**: it forms one continuous, **circular** vertical list with the
session list. `j` past the last session drops focus into the pane and
`k` at the top automation hands focus back to the last session; the
ends wrap too — `j` past the last automation loops to the **top** of
the session list, and `k` above the first session loops to the
**last** automation. It is **not** a separate stop in the
`Ctrl+H`/`Ctrl+L` cycle (which
treats it like the session list). Once focused, `j`/`k` select,
`Space`/`r`/`d` toggle/run/delete the selected automation, and `n`
creates one.

The pane mirrors the session list, with the **central pane** as its
terminal-equivalent: while the pane is focused the central pane
shows a **single editor** for the selected automation (a live
preview — there is no separate read-only "info" screen). Pressing
`Enter`/`Ctrl+L` (or `e`) focuses that editor to change fields,
exactly as `Enter`/`Ctrl+L` on a session focuses its terminal;
`Ctrl+H`/`Esc` returns to the list, `Enter` saves, `Esc` discards,
`Ctrl+E` toggles enabled. The scoped automation's run history
(`db::list_automation_runs`, cached in `App::cached_automation_runs`)
renders beneath the editor and is itself focusable
([`InputFocus::AutomationRunHistory`], one more `Ctrl+L` past the
editor): `j`/`k` select a run (`App::automation_run_index`), `r`
triggers a fresh run, and `Enter` opens the session that run touched
(`App::open_run_related_session` parses the session id out of the
run's `detail` and switches to its terminal when still open).
`Ctrl+L`/`Ctrl+H` cycle **within the current
context's ring** (`App::focus_ring`) — the automation ring
`Automations → editor → run history` wraps back to `Automations`
(never to a session; landing on the list discards edits like `Esc`),
the session ring is `SessionList → Terminal` (+ file viewer). Crossing
contexts is via `j`/`k`, not the cycle. Because the in-pane
editor/history would otherwise lose chords like `Ctrl+E` to global
keybindings, `handle_key` captures input for those two focuses
**before** the global lookup, letting only the focus-cycle/quit chords
pass through. Implemented via
the persistent `App::automation_editor` state (kept in sync by
`App::sync_automation_editor`) and
`ui::automation_editor_modal::render_automation_editor_into` +
`ui::automation_detail::render_run_history`. The
`Ctrl+P` list path opens the same editor as a centered overlay
(`Modal::AutomationEditor`); both share
`AutomationEditorModal::handle_key` + `App::save_automation`.

## Tasks (todo list)

Thurbox has a **task list**: todo items (title + markdown description +
status). The whole TUI surface is gated by `[features] tasks` in
settings.toml (disabled: F5/Ctrl+W toasts, no task search results; the
CLI stays functional). A task can be **acted on by a coding agent** via a **trigger-time
picker** (`r`): you choose *Send → a running session* or *Spawn new session…*
(the normal repo→agent flow) at the moment you act — the action is **not**
authored into the task. Either way the agent is seeded with a **full context
prompt**, not the bare title: `Task::agent_prompt()` builds an `id + # title +
markdown description` block plus self-service hints (`thurbox-cli task show
<id>` to read the full record, `thurbox-cli task edit <id> --status done` to
close it out). The TUI seeds it via `App::task_agent_prompt` (bracketed-paste
safe, so the multi-line body never submits early); the headless `task run` path
builds the same string. Triggering advances the task `Todo → InProgress` (TUI:
`App::advance_task_to_in_progress`; CLI: `mark_in_progress`).
(`Task.action: Option<AutomationAction>` still exists for the CLI / external
sync, but the TUI editor never sets it.)

- **Data** (`session/task.rs`): `Task` (`id`, `title`,
  `description: Option<String>` (free-form markdown notes, `None` when blank),
  `status: TaskStatus` {`Todo`/`InProgress`/`Done`},
  `action: Option<AutomationAction>`, plus `source`/`external_id`/
  `external_url` scaffolding for **deferred** external sync — Jira/
  GitHub Issues slot in later with no migration; local tasks use
  `source = "local"`).
- **Storage** (`storage/tasks.rs`, schema v26): `tasks` table mirroring
  the automation action columns (`action_kind` nullable) plus a nullable
  `description` column (added in the v26 migration), soft-delete via
  `deleted_at`, audited under `EntityType::Task`. CRUD: `create_task`,
  `get_task`, `list_tasks`, `update_task`, `set_task_status`,
  `soft_delete_task`.
- **UI** — tasks render in a **toggleable right-side column** that sits
  between the terminal and the file viewer, behaving exactly like the file
  viewer: **F5**/`Ctrl+W` (`Action::FocusTasks`) shows **and** focuses it
  (and hides it again), and `Ctrl+L`/`Ctrl+H` cycle in/out of it as part of
  the session ring (`SessionList → Terminal → TaskList → FileViewer`, each a
  cycle stop only while visible). Layout: `compute_layout`'s
  `show_tasks_panel` flag adds a 20% column (`PanelAreas::tasks_panel`)
  between `terminal` and `file_viewer` at width ≥ 120. Rendered by
  `ui/tasks_panel.rs` (checkbox glyphs ☐/◐/☑) with the shared
  `ui::focus_block` for the highlighted title + accent border, matching the
  session list / file viewer. `InputFocus::TaskList` is the panel focus. Rows
  whose task has an **open related session** get a trailing accent `⇄` marker
  (`TaskPaneEntry::linked`).
- **Full-screen preview / edit toggle** — the central pane is a clean toggle
  (`view::render_task_workspace`): while the tasks panel is focused
  (`InputFocus::TaskList`) it shows the selected task's **full-screen,
  scrollable** read-only **details + markdown preview** (`ui/task_detail`:
  agent linkage, **related session(s)**, status, source, created/updated, then
  the markdown-rendered description via `ui/markdown::render_markdown`);
  `PageUp`/`PageDown` scroll it
  (`App::task_preview_scroll`, reset on selection change). Entering the central
  pane (`Enter`/`e` → `InputFocus::TaskEditor`) swaps to the **full-screen
  editor** (`ui/task_editor_modal::render_task_editor_into`); `Esc` returns to
  the preview/panel. Helpers: `sync_task_editor`, `new_task_in_pane`,
  `enter_task_editor`, `refresh_task_view`, `build_task_editor`.
- **Editor fields** — a task is just **title + description + status**
  (`TaskField`); the agent action is chosen at trigger time, not here. The
  `description` is a **multi-line** `modals::TextArea` (newline +
  vertical-cursor, distinct from single-line `TextInput`): **`Enter` inserts a
  newline** and `Up`/`Down` move within the text (field nav is `Tab`);
  **`Ctrl+S` saves from any field**.
- **Keys** (focused panel): `j`/`k` select (live-preview), `PageUp`/`PageDown`
  scroll the preview, `n` new, `e`/`Enter` open the central-pane editor,
  `Space` cycle status, `r` open the **trigger-time action picker**, `o` **open
  the task's related session** (`App::open_task_related_session` — jumps to the
  spawned `task-<id>-<slug>` window or a Send target, else a status hint), `d`/`Ctrl+D`
  delete, `Esc` back to the session list. In the editor: field nav +
  `Enter`/`Ctrl+S` save (→ back to panel), `Esc` discard; the editor captures
  its keys before global bindings (so `e`/`d` edit text) via
  `handle_automation_pane_capture`.
- **Trigger-time action picker** (`r`) — `Modal::TaskActionPicker`
  (`App::open_task_action_picker`, rendered by `ui/task_action_picker_modal`,
  modeled on the theme picker): one **Send → <session>** per running session
  plus **Spawn new session…**. *Send* runs immediately
  (`App::send_task_to_session`); *Spawn* stashes `App::pending_task_prompt =
  (task_id, title)` and reuses the normal `open_repo_picker` →
  `do_spawn_session` flow, whose success tail delivers the title (after
  `AGENT_BOOT_DELAY_TICKS`) and advances the task. The pending prompt is
  cleared on a manual `Ctrl+N` so a cancelled task-spawn can't leak into it.
  Both paths call `App::advance_task_to_in_progress`.
- **CLI**: `thurbox-cli task` (alias `todo`) —
  `create`/`list`/`show`/`edit`/`remove`/`run`. `create`/`edit` take an
  optional `--description` (markdown; `edit --description ""` clears it), and
  `task_to_json` emits a `description` field. `create` with neither
  `--session` nor `--repo` is a plain local todo; `run` triggers the
  Send/Spawn action headlessly. Tasks do **not** participate in sync
  (`SharedState`) and have no run-history table (audited via `audit_log`).

## Extensions

`extensions/` holds opt-in, **agent-agnostic** add-ons that build on
`thurbox-cli` without touching the core binary. Each ships its own
curl-able `install.sh`.

- **`extensions/flow/`** *(experimental — new and under active
  testing)* — a focus-protecting triage agent: brain-dumps
  become thurbox tasks, dispatchable ones spawn worker sessions (on
  `flow/<slug>` worktree branches, agents `flow-worker` /
  `flow-worker-heavy` mapped in `agents.toml` to any CLI), a dedicated
  `flow` session monitors them via a `flow-tick` automation, and every
  reply ends with the single next thing to focus on. The behavior spec
  is `FLOW.md`, surfaced to whichever CLI runs it via context-file
  symlinks (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `FLOW.md`). See
  `extensions/flow/README.md`.

## Global search (`Ctrl+A`)

A **non-modal bottom strip** (`Ctrl+A`, rebindable) searches **every scope at once**:
**sessions** (name/agent/branch + live vt100 **buffer content**), **tasks**
(title + description, with a description snippet when only the description
matched), **automations** (name), and **files** (the active session's file
tree). `Enter` jumps to the selected result and focuses its pane —
switching to a session's terminal, the tasks panel, the automations pane,
or the file viewer (revealing the path). Gated by `[features]
global_search` in settings.toml; scopes whose feature is disabled
(tasks/automations/file viewer) contribute no results.

- **Live in-place highlighting**: instead of reprinting results in the
  strip, matched characters highlight **in the panels themselves** (session
  list, tasks, automations) — accent+bold+underline on matching rows, dim
  on the rest — via the shared `src/ui/highlight.rs` helper. The view feeds
  each panel renderer the global query through `App::global_search_query()`
  (`Some` only while the strip is open with a non-empty query). The strip
  shows a query line, per-scope match counts, the grouped scrollable result
  list (selected row marked `▸`/highlighted, content snippets dimmed), and
  key hints (rendered by `src/ui/global_search.rs`).
- **Live preview + cancel-restore**: moving the selection
  (`App::preview_global_search_result`, called from `move_global_search_selection`
  and on query change) moves the owning panel's cursor — `active_index` /
  `task_panel_index` / `automation_panel_index` — so the previewed row is
  visible while focus stays in the strip (files are *not* previewed; they
  open only on `Enter`). `global_search_preview_kind()` tells the view which
  panel owns the preview so it force-shows that row's selection
  (`TaskPaneState`/`AutomationsPaneState::preview_selected`). `open_global_search`
  captures a `SearchSnapshot` (focus + the three indices + `show_tasks_panel`/
  `show_file_viewer`); `Esc`/`close_global_search` restores it, while `Enter`/
  `activate_global_search_result` drops it (keeps the jump).
- **State** lives in `src/app/search.rs` (`GlobalSearchState`,
  `GlobalSearchResult`, `SearchTarget`/`SearchKind`); building results +
  dispatching a selection live on `App` (`build_global_search_results`,
  `session_content_match`, `activate_global_search_result`,
  `open/close_global_search`). `InputFocus::GlobalSearch` captures all
  input before the global keybinding lookup.
- **Debounce**: cheap metadata results recompute on every keystroke; the
  expensive per-session buffer-content scan runs only after the query is
  idle for ~150 ms (`Instant`-based, driven from `tick()`), capped at
  `MAX_PER_GROUP` results and `CONTENT_LINE_CAP` lines per session.
- **Keys**: type to filter; `Up`/`Down` or `Ctrl+P`/`Ctrl+N` move the
  selection (so plain `j`/`k` still type); `Enter` activates; `Esc` closes
  and restores the prior focus.
- **Layout**: `compute_layout`'s `show_global_search` carves a full-width
  `PanelAreas::global_search` strip above the footer (shrinking the content
  area like the side panels). Rendered by `src/ui/global_search.rs`.
- **Binding**: `Action::GlobalSearch` defaults to `Ctrl+A` ("search All"),
  which encodes reliably on every terminal and is fully rebindable from the
  F1 editor like any other action (there is no separate hardcoded opener).
  Global search is the **only** search: the per-pane local `/` filters
  (session list, tasks panel) were removed in favour of it. The file
  viewer's `/` (in-file text search) is unrelated and stays.

## Demo Video

The demo media is **generated**, not hand-recorded. A single
script drives the *real* TUI via
[VHS](https://github.com/charmbracelet/vhs) (needs `vhs` +
`ffmpeg` + `ttyd` + `tmux`) and writes GIF **and** MP4 straight
into `docs/media/`:

```bash
scripts/demo/record.sh                 # regenerate ALL demo videos
scripts/demo/record.sh theme automations   # re-record a subset
```

`record.sh` records every video pair in one pass: the combined
hero demo (`thurbox-demo.*` via `agents.tape`), one clip per
feature
(`thurbox-{file-manager,info-panel,theme,session-creation,fork}.*`),
and the automations/tasks/search demos (`automations-demo.*`,
`tasks-demo.*`, `search-demo.*`) — one VHS tape each
(`scripts/demo/<feature>.tape`). With no args it records all of
them; pass tape stems to re-record a subset (the `agents` stem is
the hero, `automations`/`tasks`/`search` map to `<stem>-demo.*`,
every other stem maps to `thurbox-<stem>.*`).

Every clip uses **real agent CLIs**: the script seeds one session
per installed CLI (`claude`, `opencode`, `codex`, `gemini`) in a
throwaway sample repo and launches them with no prompt. It
overrides `HOME`, so agents boot with fresh history/config (no
past conversations leak); CLIs that authenticate via the system
keyring stay logged in but show no account email on screen. The
tapes exercise the session list, info panel (`Ctrl+B`), file
viewer (`Ctrl+E`), theme picker, session-creation flow, and the
Automations pane over the seeded sessions and sample tree.

It runs fully isolated from your real environment — a dev build
(`0.0.0-dev` → `dev_build` cfg) uses the `thurbox-dev` socket and
XDG subdirs, and the script points `TMUX_TMPDIR` and
`XDG_{DATA,CONFIG,STATE,CACHE}_HOME` at a throwaway temp dir.
**`TMUX_TMPDIR` is essential**: the `thurbox-dev` socket *name* is
shared by every dev build, so without a private socket directory
the cleanup `kill-server` would tear down dev sessions you already
have running.

The deterministic recording path (a hidden `__demo-agent`
subcommand streaming canned scenarios) was retired in favor of the
single real-agents script and has been removed from the binary.

`.github/workflows/pages.yml` copies the mp4s into
`website/assets/` at deploy time and `README.md` embeds the gifs,
so regenerating these files propagates everywhere.

## Architecture (TEA Pattern)

The app follows **The Elm Architecture**:
`Event → Message → update(model, msg) → view(model) → Frame`

### Module Dependency Rules (enforced by tests/architecture_rules.rs)

```text
session  ← pure data types, no crate-internal references
agent    ← session (+ paths/shell utils; NEVER ui, git, app)
ui       ← session + app model/view state (+ fuzzy/paths;
           NEVER agent or git)
app      ← coordinator, imports all modules
```

Enforcement is an **allowlist**: every module under `src/` must
have a `ModuleRules` entry in `tests/architecture_rules.rs`
naming the crate modules it may reference — in *any* form (`use`,
`pub use`, brace groups, and fully-qualified `crate::…` paths) —
and a new module fails the test until its place in the
architecture is declared. `ui → app` is the TEA `view(model)`
coupling: ui renders state types owned by `app` (modal structs,
status messages) but never triggers side effects. `session_ops`
and `cli` may reach `crate::agent::…` (the narrow tmux helpers)
via fully-qualified paths only — never `use` — so the headless →
backend dependency stays visible at each call site.

### Module Responsibilities

- **`app/`** — Model (`App` struct) + Update
  (`AppMessage` enum + `handle_key/resize`) + View.
  Owns all state, coordinates side effects.
- **`agent/`** — Side-effect layer. `AgentProvider` trait
  abstracts CLI command + arg construction; `GenericProvider`
  implements it from a declarative `AgentDef` (loaded via
  `agent_config`). `Session` wraps a `SessionBackend`
  trait. `BackendRegistry` holds the backends, keyed by name.
  `TmuxBackend` runs tmux over a `TmuxTransport` (`transport.rs`):
  `Local` (`tmux -L thurbox`) for the default `local-tmux`
  backend, or `Ssh` (`ssh <dest> tmux …`) for each remote host
  in `hosts.toml` (registered as `ssh:<host>`, loaded via
  `host_config`). The control-mode protocol (`control_mode.rs`)
  is identical over either transport. Reads output into
  `Arc<Mutex<vt100::Parser>>`, writes input via mpsc channel.
  `input.rs` translates crossterm `KeyCode` → xterm ANSI bytes.
- **`session/`** — Plain data: `SessionId`, `SessionStatus`,
  `SessionInfo` (with `agent` name), `SessionConfig` (agent
  name, backend name, ids, cwd, env), `AgentDef`/`AgentRegistry`,
  `HostDef`/`HostRegistry` (remote SSH hosts).
  Mostly Display/Default impls plus the agent-arg
  substitution logic.
- **`ui/`** — Pure rendering functions. `layout.rs` computes
  panel areas (responsive: <80 = terminal only, >=80 = 2-panel,
  >=120 = optional 3-panel). Widgets: `project_list` (session
  list with repo/branch display; `compute_session_order` is the
  single comparator that orders sessions by manual order
  (`display_order`, never by status) and groups them by repo
  under headers — shared with `App`'s `Ctrl+J/K` navigation so
  the two never drift; `move_in_order` is the pure reorder step
  behind `Shift+J`/`Shift+K`),
  `terminal_view`, `info_panel`,
  `status_bar`, `repo_picker_modal` (repo selection with
  worktree toggle). `selection.rs` handles mouse-drag text
  selection, `links.rs` detects clickable URLs for Ctrl+Click.
  Mouse clicks are routed through a per-frame registry
  (`App::click_targets`, mirroring `scrollbar_hits`): list/modal
  renderers return `ui::RowHitbox`es, `App::view` records them as
  `ClickAction`s, and `handle_mouse_click` hit-tests them (rows
  select/confirm, panes focus, modals swallow everything else; the
  hovered row is underlined via mouse-move events). With a modal
  open, the wheel steps its selection and overflowing picker lists
  render a draggable scrollbar (`ScrollTarget::Modal`, drag replayed
  as Up/Down through the modal's key handler). All of it is gated by
  `[features] mouse` in settings.toml — disabled, mouse capture is
  never enabled and the terminal keeps native mouse behavior.
  `agent_picker_modal` drives the new-session flow.
- **`cli/`** — `thurbox-cli` subcommand dispatch (headless
  session ops + scheduling + editor command).

### Event Loop (main.rs)

```text
tokio::main → load AgentRegistry (agents.toml)
  → init BackendRegistry (local-tmux)
  → open SQLite DB → init terminal → spawn/restore sessions → loop {
    draw frame → poll crossterm events (10ms)
    → convert to AppMessage → app.update() → app.tick()
} → app.shutdown() (detach sessions) → restore terminal
```

- Logging goes to `~/.local/share/thurbox/thurbox.log`
  (file-based, since stdout is owned by the TUI)
- Panic hook restores terminal before printing

## Pre-commit Hooks

16 hooks run automatically via `prek` (Rust-based pre-commit
framework). Install with `prek install`. Stages:

- **commit-msg**: conventional commit validation (`cog verify`)
- **pre-commit**: fmt, clippy, check, nextest, architecture,
  deny, doc, bats, rumdl, prettier, htmlhint, stylelint,
  eslint
- **pre-push**: commit history check (`cog check`)

## Key Technical Details

- MSRV: 1.75, Edition 2021
- Async runtime: tokio (multi-threaded)
- Session backend: `TmuxBackend` over a `TmuxTransport`
  (local `tmux -L thurbox`, or `ssh <dest> tmux …` for
  `ssh:<host>` backends from `hosts.toml`)
- Output reader runs in `tokio::task::spawn_blocking`
  (blocking I/O), writer in `tokio::spawn` (async)
- Terminal state parsed by `vt100::Parser`,
  rendered by `tui_term::PseudoTerminal`
- Sessions persist across restarts (tmux keeps them alive)
- Session state in SQLite:
  `~/.local/share/thurbox/thurbox.db` (XDG_DATA_HOME respected);
  agent definitions in `~/.config/thurbox/agents.toml`;
  remote SSH hosts in `~/.config/thurbox/hosts.toml`
- Requires tmux >= 3.2

## Keybindings (Vim-Inspired)

Global keys use `Ctrl` + semantic Vim conventions:

| Key | Action | Mnemonic |
|-----|--------|----------|
| `Ctrl+Q` | Quit (detach sessions) | **Q**uit |
| `Ctrl+N` | New session (opens repo picker) | **N**ew |
| `Ctrl+C` | Copy selection / SIGINT (terminal) | **C**opy |
| `Ctrl+V` | Paste from clipboard | Paste |
| `Ctrl+P` | Automations (list/new/edit/toggle/run/delete) | **P**rogram |
| `Ctrl+W` / `F5` | Toggle tasks panel (todo list) | Work items |
| `Ctrl+A` | Global search (sessions/tasks/automations/files) | search **A**ll |
| `Ctrl+T` | Toggle shell pane | **T**erminal |
| `Ctrl+H` | Focus previous pane (cycle backward) | Vim: **h** = left |
| `Ctrl+J` | Select next session | Vim: **j** = down |
| `Ctrl+K` | Select previous session | Vim: **k** = up |
| `Ctrl+L` | Focus next pane (cycle forward) | Vim: **l** = right |
| `Ctrl+D` | Delete session | Vim: **d** = delete |
| `Ctrl+O` | Open active session's working dirs in editor | **O**pen |
| `Ctrl+R` | Restart active session | **R**estart |
| `Ctrl+F` | Fork active session | **F**ork |
| `Ctrl+S` | Sync worktrees with origin/main | **S**ync |
| `Ctrl+Z` | Undo session delete | **Z** = undo |
| `Ctrl+U` | Restore deleted sessions | **U**ndelete |
| `Ctrl+Y` / `F4` | Pick TUI theme | Color **Y**oke |
| `F1` / `Ctrl+G` | Keybindings help + interactive editor | Universal |
| `F2` | Toggle info panel (visible at width >= 120) | Next to F1 |
| `F3` | Toggle file viewer | Next to F2 |
| `F5` | Toggle tasks panel (visible at width >= 120) | Next to F4 |

List contexts use plain `j`/`k`/`Enter` for navigation.
In the focused session list, `Shift+J`/`Shift+K` move the selected
session down/up (manual reordering; whole groups move past a group
edge). Terminal forwards all non-Ctrl keys to the PTY.
`Shift+arrows/PageUp/PageDown` for scrollback; `Alt+PageUp/PageDown`
also page (fallback for terminals that claim `Shift+Page` for their
own scrollback, e.g. Terminal.app/iTerm2).

These defaults can be overridden two ways, both writing the same
`~/.config/thurbox/keybindings.json` (an `Action` name → one or more
chord strings, e.g. `{ "QuitApp": ["ctrl+x"] }`):

- **Interactively** from the F1 panel, which is a live editor rather
  than a read-only overlay. `j`/`k` select an action, `Enter`/`r`
  starts capture (the **next physical keypress** — including chords
  like `ctrl+q` — becomes that action's sole binding), `d` resets the
  selected action to its built-in default, and `Shift+D` resets **all**
  actions (via `App::reset_all_keybindings`, which deletes the override
  file so defaults stay authoritative). If the captured chord was already
  bound elsewhere it is reassigned (stolen from the other action) and a
  status toast reports the move. Each change is persisted immediately
  via `KeyBindings::{rebind,reset}` + `storage::keybindings::save_keybindings_json`
  and takes effect on the next keystroke — no restart. The editor lives
  in `Modal::Help(HelpModal { selected, capturing })`; capture input is
  routed through `App::handle_help_key` inside `handle_priority_key`
  (**before** the global `keybindings.lookup`, so capturing `ctrl+q`
  rebinds instead of quitting). Selection indices match
  `Action::rebindable_in_order()` — the flattened
  `keybindings::help_sections()`, the shared order used by
  `render_help_overlay`.
- **By hand-editing** the JSON file (e.g. via `$EDITOR`); reloaded live
  (mtime poll — see `docs/CONFIG.md`).

**Context-scoped bindings.** Each `Action` has a `KeyContext` (`Global`,
`SessionList`, `FileViewer`, `Terminal`). Global actions are active
everywhere; scoped actions fire only while their pane is focused, so a
single-letter key like `j` can drive both the file viewer and the session
list (and the terminal still forwards it to the PTY). `handle_key` resolves
keys via `KeyBindings::lookup_in(App::focus_key_context(), …)`, dispatched
through `dispatch_action`. Conflict detection (`KeyBindings::rebind`) only
steals a chord between actions whose scopes overlap (`contexts_overlap`) —
global-vs-anything, or same scope. Capital/shift-letter chords are
canonicalized via `KeyChord::normalized` (e.g. `Shift+N` → `{shift, n}`) so
capture, lookup, and the JSON round-trip agree regardless of how the
terminal encodes them. **Copy/Paste** are global rebindable actions handled
early in `handle_priority_key` (so Paste reaches modal text inputs).

A few stateful keys stay literal (the F1 panel lists them under
**Fixed (not rebindable)**): modal selectors (j/k/Enter/Esc), the
automations/tasks panes, the file-viewer **search sub-mode**, and the
terminal's catch-all PTY forwarding.

### macOS

Ctrl chords work unchanged in macOS terminals (raw mode bypasses
flow control; `Ctrl+Y`'s DSUSP quirk is why the `F4` alternate
exists). On top of that:

- **Cmd chords.** `main.rs` enables the kitty keyboard protocol
  (`PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`, gated
  on `supports_keyboard_enhancement()`, popped on shutdown and in
  the panic hook) so the Command key can be bound like any modifier:
  `cmd+j` in `keybindings.json` (`super`/`command`/`win` are parse
  aliases; `cmd` is the canonical display form) or captured live in
  the F1 editor. Delivered by iTerm2 3.5+, kitty, WezTerm, Ghostty;
  **not** Terminal.app (no kitty protocol — everything else still
  works there). The emulator's own Cmd shortcuts (`Cmd+Q/W/N/T/C/V`,
  `Cmd+K` clears, `Cmd+H` hides, `Cmd+digits` switch tabs) are
  consumed at the GUI level and can never reach the TUI — only bind
  what the terminal leaves free.
- **macOS-only default alternates** — appended after the Ctrl
  primaries via `Action::default_chords_for(macos)` (the
  `cfg!(target_os = "macos")` decision lives in `default_chords()`;
  Linux defaults are byte-identical): `Cmd+J`/`Cmd+Shift+J` =
  next/previous session, `Cmd+L`/`Cmd+Shift+L` = focus
  next/previous pane.
- **Unbound Cmd chords are swallowed**, never forwarded to the PTY
  (`agent::input::key_to_bytes` returns `None` for SUPER).
- **F-keys** (`F1`–`F5` alternates) need `Fn` on Mac laptops unless
  "Use F1, F2, etc. keys as standard function keys" is enabled;
  `Cmd+V` pastes via the terminal's native paste (bracketed paste),
  no binding needed.

## Themes

The TUI ships with nine palettes — five dark (**Default**, **Catppuccin
Mocha**, **Tokyo Night**, **Gruvbox Dark**, **Doom**) and four light
(**Catppuccin Latte**, **Tokyo Night Day**, **Gruvbox Light**,
**Solarized Light**). Users can add **custom themes** in
`~/.config/thurbox/themes.toml` (a built-in `base` plus per-colour
overrides — see `docs/CONFIG.md`); they appear in the picker after the
built-ins and persist by name exactly like a preset
(`session::theme_config::CustomThemeDef` → `ThemeEntry`, loaded by
`agent::themes_config::load_or_seed_with_warnings`, published via
`ui::theme::set_custom_themes`).
Pick one with `Ctrl+Y` (or `F4`,
which avoids terminals that intercept Ctrl+Y as DSUSP); the choice
is persisted in SQLite under `metadata.active_theme` and survives
restarts. Other thurbox processes pick up theme changes within one
tick via `PRAGMA data_version` polling.

## Design Documentation

For rationale behind decisions, see `docs/`:

- `docs/CONSTITUTION.md` — Core principles and non-negotiable rules
- `docs/ARCHITECTURE.md` — Architectural decisions with rationale
- `docs/FEATURES.md` — Feature-level design choices
- `docs/CONFIG.md` — Every config file/env var/DB setting in one place

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
