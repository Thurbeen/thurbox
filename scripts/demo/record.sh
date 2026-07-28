#!/usr/bin/env sh
# Regenerate ALL Thurbox demo media in one pass, using REAL coding-agent CLIs.
#
# This single script records every video pair under docs/media/:
#
#   * thurbox-demo.{gif,mp4}            (agents.tape          — the hero demo)
#   * thurbox-file-manager.{gif,mp4}    (file-manager.tape)
#   * thurbox-info-panel.{gif,mp4}      (info-panel.tape)
#   * thurbox-theme.{gif,mp4}           (theme.tape)
#   * thurbox-session-creation.{gif,mp4}(session-creation.tape)
#   * thurbox-fork.{gif,mp4}            (fork.tape)
#   * automations-demo.{gif,mp4}        (automations.tape)
#   * tasks-demo.{gif,mp4}              (tasks.tape)
#   * search-demo.{gif,mp4}             (search.tape)
#   * code-review-demo.{gif,mp4}        (code-review.tape)
#
# Every clip drives the actual `claude`, `opencode`, `codex` and `antigravity` CLIs —
# one per thurbox session — to showcase real multi-agent orchestration. No prompt
# is sent to any agent; they are launched and left on their start screens.
#
# Isolation (so this never touches your real thurbox, tmux, or agent accounts):
#   * HOME points at a throwaway dir  -> agents boot with NO chat history (no past
#     conversations leak into the video). To avoid login/trust dialogs on screen,
#     each CLI's auth *token* is copied into the throwaway HOME and every demo repo
#     is marked trusted (see "Seed agent credentials + pre-trust" below). Only the
#     token is copied, never history; auth files absent for a CLI you are not
#     logged into are simply skipped. No account email/handle is shown on screen:
#     codex surfaces no identity when logged in; antigravity (agy) and claude are
#     both featured LOGGED OUT on purpose, because each prints your account email
#     in its welcome box when signed in (agy fetches it from the server via its
#     keyring auth; claude prints the org name) — see their notes below.
#   * TMUX_TMPDIR points at a throwaway dir -> the `thurbox-dev` tmux server lives
#     in its own socket directory, so cleanup can't kill dev sessions you already
#     have running.
#   * XDG_{DATA,CONFIG,STATE,CACHE}_HOME point at a throwaway dir.
#
# Requirements: cargo, git, tmux, sqlite3, jq, vhs (+ ffmpeg + ttyd) and whichever agent CLIs
# you want to feature (claude / opencode / codex / antigravity). Missing agents are
# skipped with a warning.
#
# Usage:  scripts/demo/record.sh [tape-stem ...]
#
#   With no args, records every tape below. Pass one or more tape stems to
#   re-record only a subset, e.g. `record.sh theme automations`.

set -eu

# Tapes to record (stems of scripts/demo/<stem>.tape), hero first. `agents` is
# the combined hero demo (docs/media/thurbox-demo.*); the rest are per-feature
# clips (`automations` -> automations-demo.*, `tasks` -> tasks-demo.*, `search`
# -> search-demo.*, others -> thurbox-<stem>.*).
ALL_TAPES="agents file-manager info-panel theme session-creation fork automations tasks search code-review"
TAPES="${*:-$ALL_TAPES}"

# thurbox TUI theme every clip starts in (persisted string in metadata.active_theme,
# see src/session/theme_config.rs). The `theme` clip switches away from it to show
# the picker, so we re-apply this before EVERY tape to keep all videos on-brand.
DEMO_THEME="${DEMO_THEME:-doom}"

# --- Locate the repo root (this script lives in scripts/demo/) ---------------
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
cd "$REPO_ROOT"

# Validate requested tapes exist before doing any expensive setup.
for tape in $TAPES; do
    if [ ! -f "$SCRIPT_DIR/$tape.tape" ]; then
        echo "error: no such tape: $SCRIPT_DIR/$tape.tape" >&2
        echo "  available: $ALL_TAPES" >&2
        exit 1
    fi
done

# --- Preflight: required tools ----------------------------------------------
missing=
for tool in cargo git tmux vhs sqlite3 jq; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "error: missing required tool(s):$missing" >&2
    echo "  vhs:  https://github.com/charmbracelet/vhs (needs ffmpeg + ttyd)" >&2
    exit 1
fi

# Map a featured-agent display name to its actual CLI binary. They differ only
# for antigravity, whose binary is `agy` (the Gemini CLI successor); identity for
# everyone else.
agent_command() {
    case "$1" in
        antigravity) echo "agy" ;;
        *) echo "$1" ;;
    esac
}

# Which agent CLIs are available? Feature only the ones present.
AGENTS=
for a in claude opencode codex antigravity; do
    bin=$(agent_command "$a")
    if command -v "$bin" >/dev/null 2>&1; then
        AGENTS="$AGENTS $a"
    else
        echo "warning: '$bin' not found on PATH — skipping '$a' in the demo" >&2
    fi
done
if [ -z "$AGENTS" ]; then
    echo "error: none of claude/opencode/codex/antigravity (agy) are installed" >&2
    exit 1
fi

# --- Build the dev binaries (version 0.0.0-dev => dev_build cfg) -------------
# Build BEFORE the HOME override so cargo still finds ~/.cargo.
echo "==> Building thurbox (dev) ..."
cargo build --bin thurbox --bin thurbox-cli

THURBOX_BIN="$REPO_ROOT/target/debug/thurbox"
CLI_BIN="$REPO_ROOT/target/debug/thurbox-cli"
export THURBOX_BIN   # consumed by the tapes (they `exec "$THURBOX_BIN"`)

# --- Isolated environment (shared dev-sandbox helper) ------------------------
REAL_HOME="$HOME"                        # captured before the override below
# shellcheck source=scripts/dev/lib/sandbox-env.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/dev/lib/sandbox-env.sh"
tbx_sandbox_init_full fresh              # throwaway temp HOME/XDG/TMUX_TMPDIR
DEMO_HOME="$TBX_SANDBOX_ROOT"            # fresh agent auth (no real creds/history)
CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"   # dev_build subdir
DB_FILE="$XDG_DATA_HOME/thurbox-dev/thurbox.db"  # SQLite db (dev_build subdir)
mkdir -p "$CFG_DIR"

# Hide `wsl.exe` from the demo's PATH. WSL distros are AUTO-discovered (no config
# to isolate — `host_config::discover_wsl_hosts` shells out to `wsl.exe -l -q`
# whenever it resolves on PATH), so on a WSL box every discovered distro becomes a
# host and Ctrl+N opens the "Run On" host picker *before* the repo picker. That
# extra modal shifts every subsequent keystroke in the spawn tapes by one step, so
# they record the wrong flow while still exiting 0. Dropping the Windows interop
# dirs makes discovery return empty -> zero hosts -> Ctrl+N opens the repo picker
# directly, which is what the tapes are written against (and what a non-WSL
# recording box produces, so demo output no longer depends on the host OS).
# Drop the WSL interop dirs only (`/mnt/<drive>/...`) — that is where wsl.exe and
# every other Windows binary lives. Filtering on the word "windows" instead would
# also strip a legitimate unix path that merely contains it.
PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -v '^/mnt/[a-z]/' | paste -sd: -)
export PATH
if command -v wsl.exe >/dev/null 2>&1; then
    echo "warning: wsl.exe still on PATH — the host picker may shift spawn tapes" >&2
fi

cleanup() {
    # The isolated tmux server (in TMUX_TMPDIR) hosts every agent pane, so the
    # helper's single kill reaps all the real agent processes too — and cannot
    # reach any tmux server outside this throwaway directory — then wipes it.
    tbx_sandbox_teardown
}
trap cleanup EXIT INT TERM

# --- Agent registry: one entry per available CLI, launched with no args ------
{
    # shellcheck disable=SC2086 # $AGENTS is a space-separated list, split on purpose
    first=$(printf '%s\n' $AGENTS | head -n1)
    echo "default = \"$first\""
    for a in $AGENTS; do
        if [ "$a" = "antigravity" ]; then
            # Launch agy with the keyring/D-Bus cut off so it boots to its clean,
            # branded logged-out screen ("Welcome to the Antigravity CLI … select
            # login method") instead of printing the real signed-in Google account
            # email + name. agy authenticates via the system keyring (D-Bus secret
            # service), which survives the HOME/XDG isolation, and fetches the
            # account identity from the server — so cutting D-Bus is the only way
            # to keep that PII off screen. Mirrors the claude treatment: featured,
            # but identity-free on screen.
            printf '\n[[agents]]\nname = "antigravity"\ncommand = "env"\nargs = ["-u", "GNOME_KEYRING_CONTROL", "DBUS_SESSION_BUS_ADDRESS=/dev/null", "agy"]\n'
        else
            printf '\n[[agents]]\nname = "%s"\ncommand = "%s"\n' "$a" "$(agent_command "$a")"
        fi
    done
} > "$CFG_DIR/agents.toml"

# --- Keybindings: rebind global search to Ctrl+A for the demo ----------------
# The real default for Action::GlobalSearch is Ctrl+/ (plus the Ctrl+7/Ctrl+_
# raw-0x1F encodings), which VHS+ttyd do not deliver reliably across terminals.
# The search.tape opens the strip with Ctrl+A — an unambiguous chord every
# terminal sends — so seed a keybindings.json that maps it. Only GlobalSearch is
# overridden; every other action keeps its built-in default.
# The code-review view needs no override: its default primary chord is Ctrl+X
# (F7 alternate, which VHS can't send), so the agents/code-review tapes open it
# with a plain Ctrl+X. Because Ctrl+X is in terminal_passthrough (the emacs
# prefix key), those tapes open the review from the session list, not a focused
# terminal, where the chord would be forwarded to the agent instead.
printf '{\n  "GlobalSearch": ["ctrl+a"]\n}\n' \
    > "$CFG_DIR/keybindings.json"

# --- The demo repo: a vendored snapshot of thurbox's own tree ----------------
# The demo repo is a fixed subset of THIS repository, copied into the throwaway
# HOME and `git init`ed there.
#
# Why thurbox's own code rather than a synthetic "sample-project" (or a cloned
# third-party repo):
#   * It shows real work. The old stub was four toy files (`fn add(a, b)`), so
#     every clip was a UI tour — nothing on screen told a viewer WHY you would
#     run four agents at once. Real modules, a real test and real docs make the
#     file viewer, the search results and the review diff legible as actual work.
#   * It is already on the recording machine, so recordings stay hermetic and
#     offline (no clone step to slow down or break a re-record), which is the
#     same property the throwaway HOME/XDG isolation buys elsewhere.
#   * Licensing is a non-question: thurbox is MIT and we own it. A GPL engine or
#     any third-party tree would put a license notice into the release pipeline's
#     demo assets for no benefit.
#   * It is self-demonstrating — the tool built with the tool.
#
# COPIED, not symlinked, and never the live checkout: the recording must not be
# able to mutate your working tree (the review clip commits into a worktree of
# this repo), and a fixed file list keeps successive recordings visually stable
# even as the real repo moves on.
DEMO_REPO="$DEMO_HOME/thurbox"
mkdir -p "$DEMO_REPO"

# A curated file list: small enough to render legibly at the tapes' font size,
# varied enough that the file tree looks like a real project (nested src/,
# tests/, docs/). Paths are relative to the repo root and keep their layout.
DEMO_FILES="
src/shell.rs
src/workspace.rs
src/ui/highlight.rs
src/ui/syntax.rs
src/session/task.rs
tests/architecture_rules.rs
docs/ARCHITECTURE.md
docs/CONFIG.md
README.md
LICENSE
"
# shellcheck disable=SC2086 # $DEMO_FILES is a newline-separated list, split on purpose
for f in $DEMO_FILES; do
    [ -f "$REPO_ROOT/$f" ] || continue
    mkdir -p "$DEMO_REPO/$(dirname "$f")"
    cp "$REPO_ROOT/$f" "$DEMO_REPO/$f"
done
# A Cargo.toml so the tree reads as a buildable crate. Hand-written rather than
# copied: the real one carries the workspace/dependency detail that would only
# be noise on screen.
cat > "$DEMO_REPO/Cargo.toml" <<'EOF'
[package]
name = "thurbox"
version = "0.0.0-dev"
edition = "2021"
license = "MIT"

[dependencies]
ratatui = "0.30"
tui-term = "0.3"
vt100 = "0.16"
rusqlite = { version = "0.40", features = ["bundled"] }
tokio = { version = "1", features = ["full"] }
EOF

git init -q "$DEMO_REPO"
git -C "$DEMO_REPO" -c user.email=demo@thurbox -c user.name=demo add -A
git -C "$DEMO_REPO" -c user.email=demo@thurbox -c user.name=demo \
    commit -q -m "chore: import thurbox tree"
# The branch the initial commit landed on (master or main, per the host's git
# config) — used as the code-review demo's worktree base.
DEMO_BASE_BRANCH=$(git -C "$DEMO_REPO" symbolic-ref --short HEAD)

# --- A parent folder of several repos, for the "import as parent" demo --------
# Lives under $HOME so the session-creation tape can type `~/projects` and have
# the picker's tilde-expansion resolve it during recording. The picker imports
# the folder as a parent and lists these git sub-dirs (by basename) beneath it.
PROJECTS_DIR="$HOME/projects"
for r in api-server shared-lib web-app; do
    repo="$PROJECTS_DIR/$r"
    mkdir -p "$repo"
    printf '# %s\n' "$r" > "$repo/README.md"
    git init -q "$repo"
    git -C "$repo" -c user.email=demo@thurbox -c user.name=demo add -A
    git -C "$repo" -c user.email=demo@thurbox -c user.name=demo \
        commit -q -m "init $r"
done

# --- Seed agent credentials + pre-trust the demo folders ---------------------
# Agent CLIs (a) authenticate via files under $HOME and (b) prompt "do you trust
# this folder?" on first launch in an unknown dir. The throwaway $HOME wipes both,
# so without this the recordings show login/trust dialogs instead of the ready
# chat UI. Seed each CLI's auth token (NOT its chat history) and mark every demo
# repo trusted. opencode needs neither (it boots straight into a ready UI). The
# auth files are only copied when present, so this is a no-op for any CLI you are
# not logged into. Per-CLI on-disk formats:
#   codex  -> ~/.codex/{auth.json, config.toml: [projects."<p>"] trust_level}
#   antigravity (agy) -> featured logged-OUT (keyring auth can't be seeded into a
#                        throwaway HOME and leaks the account email); we only seed
#                        ~/.gemini/{settings.json, trustedFolders.json} +
#                        ~/.gemini/antigravity-cli/{cache/onboarding.json,
#                        bin/webm_encoder} to keep its logged-out screen tidy
#   claude -> ~/.claude/.credentials.json + ~/.claude.json projects."<p>"
#             .hasTrustDialogAccepted (+ a binary symlink so its self-install
#             check stays quiet under the throwaway HOME)
# The trusted dirs: the sample repo plus the parent-folder repos the
# session-creation tape browses.
set -- "$DEMO_REPO" "$PROJECTS_DIR" "$PROJECTS_DIR/api-server" \
    "$PROJECTS_DIR/shared-lib" "$PROJECTS_DIR/web-app"

# Suppress every agent's "a new version is available" first-run prompt. These are
# MODAL in some CLIs (opencode renders a centered Update Available box that
# swallows arrow keys), so a tape's navigation never reaches thurbox and the
# following keystrokes are typed into the agent instead — a broken clip that still
# exits 0. They also date the recording. Env vars are set here rather than in
# agents.toml so they cover every launch path; opencode's `autoupdate` config key
# is honoured only at the global path (and was ignored outright in some releases),
# hence the env var as well.
export OPENCODE_DISABLE_AUTOUPDATE=true
export CODEX_DISABLE_UPDATE_CHECK=1
export npm_config_update_notifier=false      # npm-distributed CLIs (update-notifier)
export NO_UPDATE_NOTIFIER=1

# opencode: disable the update prompt via config too (belt and braces with the
# env var above), at the global path the setting is actually read from.
mkdir -p "$HOME/.config/opencode"
printf '{\n  "autoupdate": false\n}\n' > "$HOME/.config/opencode/opencode.json"

# codex: auth token + one trusted [projects] table per demo dir
if [ -f "$REAL_HOME/.codex/auth.json" ]; then
    mkdir -p "$HOME/.codex"
    cp "$REAL_HOME/.codex/auth.json" "$HOME/.codex/auth.json"
    # Silence codex's release-notes banner ("Update available! x -> y").
    printf 'hide_update_notice = true\n\n' > "$HOME/.codex/config.toml"
    for p in "$@"; do
        printf '[projects."%s"]\ntrust_level = "trusted"\n\n' "$p" \
            >> "$HOME/.codex/config.toml"
    done
fi

# antigravity (agy): featured logged-OUT, like claude — NO auth token is seeded.
# agy authenticates via the system keyring (D-Bus secret service), which survives
# the HOME/XDG isolation, and prints the signed-in Google account's email + full
# name in its welcome box (fetched from the server). The only way to keep that PII
# off screen is to launch it with the keyring cut off (the agents.toml entry above
# wraps `agy` in `env … DBUS_SESSION_BUS_ADDRESS=/dev/null`), so it boots to its
# clean, branded "not signed in / select login method" screen. We still seed
# ~/.gemini so that screen is tidy: onboarding marked complete (skips the
# first-run intro), the demo folders pre-trusted, the oauth-personal auth type
# selected, and webm_encoder copied in (avoids a ~17 MB on-camera download). This
# runs unconditionally — agy is logged out, so it needs nothing from your real
# ~/.gemini except the (optional) cached webm_encoder.
mkdir -p "$HOME/.gemini/antigravity-cli/cache" "$HOME/.gemini/antigravity-cli/bin"
printf '{"security":{"auth":{"selectedType":"oauth-personal"}}}\n' \
    > "$HOME/.gemini/settings.json"
jq -n '$ARGS.positional | map({(.): "TRUST_FOLDER"}) | add' --args "$@" \
    > "$HOME/.gemini/trustedFolders.json"
printf '{"consumerOnboardingComplete":true,"enterpriseOnboardingComplete":false,"onboardingComplete":true}\n' \
    > "$HOME/.gemini/antigravity-cli/cache/onboarding.json"
[ -f "$REAL_HOME/.gemini/antigravity-cli/bin/webm_encoder" ] && \
    cp "$REAL_HOME/.gemini/antigravity-cli/bin/webm_encoder" \
        "$HOME/.gemini/antigravity-cli/bin/webm_encoder"

# claude: trust + onboarding flags only — deliberately NOT logged in. claude's
# welcome box renders the account's organizationName, and a personal org is
# auto-named after your email; worse, claude force-syncs that field from the
# server (overwriting any seeded override, even via a read-only file, since it
# writes through a temp-file rename), so a logged-in claude would print your email
# in the recording. We therefore leave it logged out: trust is pre-accepted (no
# trust dialog) and it shows a clean "Welcome back!" with no account identity. The
# binary symlink keeps its self-install check ("claude command missing") quiet.
mkdir -p "$HOME/.local/bin"
jq -n '{hasCompletedOnboarding: true,
        projects: ($ARGS.positional
                   | map({(.): {hasTrustDialogAccepted: true}}) | add)}' \
    --args "$@" > "$HOME/.claude.json"
claude_bin=$(command -v claude 2>/dev/null || true)
[ -n "$claude_bin" ] && ln -sf "$(readlink -f "$claude_bin")" "$HOME/.local/bin/claude"

# --- Pre-seed one session per agent so the TUI opens populated ---------------
# Sessions are named after the WORK, not the agent running it. The session list
# is the demo's headline shot, and `claude`/`codex`/`opencode`/`antigravity` told
# a viewer nothing except which CLIs are installed — it never answered "why would
# I run four of these at once?". Task-shaped names make the list read as one
# backlog with four branches in flight, which is the actual use case. The agent is
# still visible per session (info panel, tab title), so nothing is lost.
#
# Each name is a real item from thurbox's own history, matching the vendored tree.
demo_session_name() {
    case "$1" in
        claude)      echo "fix-osc52-tmux" ;;
        codex)       echo "add-wsl-host-tests" ;;
        opencode)    echo "perf-session-order-cache" ;;
        antigravity) echo "docs-remote-hooks" ;;
        *)           echo "$1" ;;
    esac
}

# Opt out of the built-in hooks extension for the recording. It is auto-activated
# by default and injects a `--settings` patch into claude, which then asks
# "Hooks need review / 4 hooks are new or changed" on first launch in the
# throwaway HOME — a modal that both hides the agent's real UI and can swallow a
# tape's keystrokes. The status hooks it wires drive the session-list dots, which
# no tape asserts on (agents are launched with no prompt, so nothing is working),
# so dropping it costs the demo nothing. Must run BEFORE the sessions spawn.
"$CLI_BIN" extension deactivate hooks >/dev/null 2>&1 || true

echo "==> Seeding one session per agent:$AGENTS"
for a in $AGENTS; do
    "$CLI_BIN" session create --name "$(demo_session_name "$a")" \
        --repo-path "$DEMO_REPO" --agent "$a" >/dev/null
done

# --- Code-review demo: a worktree session with a real committed diff ---------
# The review view diffs <base>..HEAD of a session's worktree, so any tape that
# opens the code-review view needs a session whose branch actually has changes.
# Both the dedicated `code-review` clip and the hero `agents` demo (which now
# showcases the review feature) use it. Create it LAST so restore leaves it
# selected on launch (finish_adopted_session makes the last-restored session
# active), on a worktree off the sample repo, then commit a small multi-file
# change into that worktree so the Branch target shows a colourful diff. Gated on
# those two tapes so the remaining clips stay fast / unchanged.
# shellcheck disable=SC2086 # $TAPES is a space-separated list, split on purpose
if printf '%s ' $TAPES | grep -Eq '(^| )(code-review|agents)( |$)'; then
    echo "==> Seeding a worktree review session with a committed diff"
    review_agent=$(printf '%s\n' $AGENTS | head -n1)
    "$CLI_BIN" session create --name "harden-posix-quote" \
        --repo-path "$DEMO_REPO" \
        --agent "$review_agent" --worktree-branch "fix/posix-quote-newline" \
        --base-branch "$DEMO_BASE_BRANCH" >/dev/null
    # Resolve the worktree path thurbox created for the review branch.
    REVIEW_WT=$(git -C "$DEMO_REPO" worktree list --porcelain \
        | awk '/^worktree /{p=substr($0,10)}
               $0=="branch refs/heads/fix/posix-quote-newline"{print p}')
    if [ -n "$REVIEW_WT" ] && [ -f "$REVIEW_WT/src/shell.rs" ]; then
        # A REAL, self-explanatory change: `posix_quote`'s own doc comment says it
        # does not strip newlines and pushes that onto callers, so making it reject
        # them (plus a test) reads as a genuine follow-up fix rather than demo
        # filler. It also produces a diff that demonstrates the review view well —
        # a modified function body, a new doc line, and a new test block, so the
        # unified/side-by-side layouts and the syntax highlighting all have
        # something to show. Applied with `patch`-free python so the edit is exact
        # and fails loudly if the vendored file ever drifts.
        # Raw strings (r""") so the Rust source's own backslashes need no
        # re-escaping here: the heredoc is quoted, so python sees these bytes
        # exactly as written.
        python3 - "$REVIEW_WT/src/shell.rs" <<'PY'
import sys
path = sys.argv[1]
src = open(path).read()
anchor = r"""    if !s.is_empty() && s.chars().all(is_safe_shell_char) {"""
guard = r"""    debug_assert!(
        !s.contains('\n'),
        "posix_quote received a newline; quote line-delimited input first"
    );
"""
helper = r"""
/// Whether `s` is safe to hand to [`posix_quote`] as a single shell word.
///
/// A newline would be re-split by a line-delimited protocol before the remote
/// shell ever sees the quoting, so callers must reject it up front.
pub fn is_quotable_word(s: &str) -> bool {
    !s.contains('\n')
}
"""
if anchor not in src:
    sys.exit("posix_quote body not found - vendored src/shell.rs drifted")
# Insert the guard at the top of posix_quote's body...
src = src.replace(anchor, guard + anchor, 1)
# ...and the new helper right after that function's closing brace.
end = src.index(anchor)
close = src.index("\n}\n", end) + len("\n}\n")
src = src[:close] + helper + src[close:]
open(path, "w").write(src)
PY
        cat >> "$REVIEW_WT/tests/architecture_rules.rs" <<'EOF'

#[test]
fn posix_quote_rejects_newlines() {
    // A newline survives quoting and would be re-split by tmux control mode,
    // so callers must screen it out before quoting.
    assert!(thurbox::shell::is_quotable_word("feat/add-panel"));
    assert!(!thurbox::shell::is_quotable_word("two\nlines"));
}
EOF
        git -C "$REVIEW_WT" -c user.email=demo@thurbox -c user.name=demo add -A
        git -C "$REVIEW_WT" -c user.email=demo@thurbox -c user.name=demo \
            commit -q -m "fix(shell): reject newlines in posix_quote"
    fi
fi

# --- Pre-seed a few tasks + an automation -----------------------------------
# These give the `tasks` and `search` clips real content to render (the search
# strip searches across sessions, tasks AND automations at once). Only needed
# for those two tapes, but seeding is cheap and harmless for the others.
# shellcheck disable=SC2086 # $TAPES is a space-separated list, split on purpose
if printf '%s ' $TAPES | grep -Eq '(^| )(tasks|search)( |$)'; then
    echo "==> Seeding demo tasks + an automation"
    # Real backlog items from thurbox's own tracker, matching the vendored tree
    # and the session names — so the tasks panel reads as the same sprint the
    # sessions are working, not as generic filler.
    #
    # NOTE: `search.tape` types the literal queries `tri`, `quote` and `test`, so
    # at least one task/automation must match each. Keep them in sync when editing.
    #
    # A plain local todo plus one already in progress, so the checkbox glyphs
    # (todo/in-progress/done) all show in the list.
    "$CLI_BIN" task create --title "Add psmux control-mode tests" >/dev/null 2>&1 || true
    "$CLI_BIN" task create --title "Triage flaky WSL host discovery" \
        --status in_progress >/dev/null 2>&1 || true
    # A rich markdown description so the full-screen preview shows headings,
    # bold and lists rendered (the headline of the tasks feature).
    "$CLI_BIN" task create --title "Harden posix_quote against newlines" \
        --description "$(printf '## Goal\n\nA newline survives `posix_quote` and is re-split by tmux **control mode**.\n\n- reject newlines at the quoting boundary\n- add a regression test\n- audit callers that build *remote* git commands')" \
        >/dev/null 2>&1 || true
    # An automation (spawn action, inferred from --repo) so the search demo has
    # a matching automation result too.
    "$CLI_BIN" automation create --name "nightly-clippy-triage" --trigger daily \
        --time "09:00" --repo "$DEMO_REPO" \
        --prompt "Run cargo clippy --all-targets and triage any new warnings" \
        >/dev/null 2>&1 || true
fi

# Give the real CLIs a moment to boot before VHS starts capturing. Each tape
# relaunches the TUI against the same seeded sessions, so they only need to be
# warm once.
sleep 6

# --- Record -----------------------------------------------------------------
# Each tape declares its own Output paths, so one VHS run == one output pair.
# Loop over the requested tapes, rendering each into docs/media/.
# Persist the TUI theme into the seeded db so the next launched TUI starts in it.
# The db + metadata table already exist (the `session create` calls above opened
# them). No TUI is running between vhs invocations, so this write is conflict-free.
set_theme() {
    sqlite3 "$DB_FILE" \
        "INSERT INTO metadata (key, value) VALUES ('active_theme', '$1') \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value"
}

# Pin a deterministic session order so the left column is byte-stable across
# runs. Tapes that walk the list with a FIXED number of Down presses (the
# `automations` clip steps past the last session to flow into the Automations
# pane) are only correct if the walk starts on a known row: the combined
# session+automations column is circular, so one press too many wraps back onto
# the list and the following keystrokes are forwarded to the focused agent's PTY
# instead — a broken clip that still exits 0. `display_order` is the authoritative
# ordering (a NULL sorts after ordered rows, in creation order), so stamping it
# 0..n-1 by creation time fixes both the order and which row is selected first.
set_session_order() {
    sqlite3 "$DB_FILE" <<'SQL'
UPDATE sessions
   SET display_order = (
       SELECT COUNT(*) FROM sessions AS earlier
        WHERE earlier.deleted_at IS NULL
          AND earlier.created_at < sessions.created_at
   )
 WHERE deleted_at IS NULL;
SQL
}

for tape in $TAPES; do
    # Re-apply before every tape: the `theme` clip switches themes (and persists
    # the change), so without this any tape after it would start on the wrong one.
    set_theme "$DEMO_THEME"
    set_session_order
    echo "==> Recording $tape.tape (theme: $DEMO_THEME) ..."
    vhs "$SCRIPT_DIR/$tape.tape"
done

echo "==> Done. Updated docs/media/ for tape(s):$([ "$TAPES" = "$ALL_TAPES" ] && echo " all" || echo " $TAPES")"
for tape in $TAPES; do
    case "$tape" in
        agents)      echo "    thurbox-demo.{gif,mp4}" ;;
        automations) echo "    automations-demo.{gif,mp4}" ;;
        tasks)       echo "    tasks-demo.{gif,mp4}" ;;
        search)      echo "    search-demo.{gif,mp4}" ;;
        code-review) echo "    code-review-demo.{gif,mp4}" ;;
        *)           echo "    thurbox-$tape.{gif,mp4}" ;;
    esac
done
