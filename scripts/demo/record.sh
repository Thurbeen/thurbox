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
# The code-review view toggles with F7 by default, which VHS also can't send, so
# it is rebound to Ctrl+X for the code-review tape. Both overrides are harmless
# for the other tapes.
printf '{\n  "GlobalSearch": ["ctrl+a"],\n  "ToggleReview": ["ctrl+x"]\n}\n' \
    > "$CFG_DIR/keybindings.json"

# --- A throwaway sample repo so the file viewer shows a realistic tree --------
DEMO_REPO="$DEMO_HOME/sample-project"
mkdir -p "$DEMO_REPO/src" "$DEMO_REPO/tests" "$DEMO_REPO/docs"
cat > "$DEMO_REPO/README.md" <<'EOF'
# sample-project

A tiny demo repository used to showcase the Thurbox file viewer.
EOF
cat > "$DEMO_REPO/src/main.rs" <<'EOF'
fn main() {
    println!("hello from sample-project");
}
EOF
cat > "$DEMO_REPO/src/lib.rs" <<'EOF'
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}
EOF
cat > "$DEMO_REPO/tests/basic.rs" <<'EOF'
#[test]
fn it_adds() {
    assert_eq!(sample_project::add(2, 2), 4);
}
EOF
cat > "$DEMO_REPO/docs/ARCHITECTURE.md" <<'EOF'
# Architecture

Sample document for the file-viewer demo.
EOF
git init -q "$DEMO_REPO"
git -C "$DEMO_REPO" -c user.email=demo@thurbox -c user.name=demo add -A
git -C "$DEMO_REPO" -c user.email=demo@thurbox -c user.name=demo \
    commit -q -m "init sample project"
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

# codex: auth token + one trusted [projects] table per demo dir
if [ -f "$REAL_HOME/.codex/auth.json" ]; then
    mkdir -p "$HOME/.codex"
    cp "$REAL_HOME/.codex/auth.json" "$HOME/.codex/auth.json"
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
echo "==> Seeding one session per agent:$AGENTS"
for a in $AGENTS; do
    "$CLI_BIN" session create --name "$a" --repo-path "$DEMO_REPO" --agent "$a" >/dev/null
done

# --- Code-review demo: a worktree session with a real committed diff ---------
# The review view diffs <base>..HEAD of a session's worktree, so the code-review
# tape needs a session whose branch actually has changes. Create it LAST so
# restore leaves it selected on launch (finish_adopted_session makes the
# last-restored session active), on a worktree off the sample repo, then commit a
# small multi-file change into that worktree so the Branch target shows a
# colourful diff. Gated on the code-review tape so other clips stay fast /
# unchanged.
# shellcheck disable=SC2086 # $TAPES is a space-separated list, split on purpose
if printf '%s ' $TAPES | grep -Eq '(^| )code-review( |$)'; then
    echo "==> Seeding a worktree review session with a committed diff"
    review_agent=$(printf '%s\n' $AGENTS | head -n1)
    "$CLI_BIN" session create --name "review" --repo-path "$DEMO_REPO" \
        --agent "$review_agent" --worktree-branch "review/demo" \
        --base-branch "$DEMO_BASE_BRANCH" >/dev/null
    # Resolve the worktree path thurbox created for branch review/demo.
    REVIEW_WT=$(git -C "$DEMO_REPO" worktree list --porcelain \
        | awk '/^worktree /{p=substr($0,10)} $0=="branch refs/heads/review/demo"{print p}')
    if [ -n "$REVIEW_WT" ]; then
        cat > "$REVIEW_WT/src/lib.rs" <<'EOF'
pub fn add(a: i64, b: i64) -> i64 {
    a.wrapping_add(b)
}

pub fn mul(a: i64, b: i64) -> i64 {
    a.wrapping_mul(b)
}
EOF
        cat > "$REVIEW_WT/src/greet.rs" <<'EOF'
pub fn greet(name: &str) -> String {
    format!("hello, {name}!")
}
EOF
        cat > "$REVIEW_WT/src/main.rs" <<'EOF'
mod greet;

fn main() {
    println!("{}", greet::greet("sample-project"));
}
EOF
        git -C "$REVIEW_WT" -c user.email=demo@thurbox -c user.name=demo add -A
        git -C "$REVIEW_WT" -c user.email=demo@thurbox -c user.name=demo \
            commit -q -m "feat: add greeting + checked arithmetic"
    fi
fi

# --- Pre-seed a few tasks + an automation -----------------------------------
# These give the `tasks` and `search` clips real content to render (the search
# strip searches across sessions, tasks AND automations at once). Only needed
# for those two tapes, but seeding is cheap and harmless for the others.
# shellcheck disable=SC2086 # $TAPES is a space-separated list, split on purpose
if printf '%s ' $TAPES | grep -Eq '(^| )(tasks|search)( |$)'; then
    echo "==> Seeding demo tasks + an automation"
    # A plain local todo plus one already in progress, so the checkbox glyphs
    # (todo/in-progress/done) all show in the list.
    "$CLI_BIN" task create --title "Write integration tests" >/dev/null 2>&1 || true
    "$CLI_BIN" task create --title "Triage failing CI" --status in_progress \
        >/dev/null 2>&1 || true
    # A rich markdown description so the full-screen preview shows headings,
    # bold and lists rendered (the headline of the tasks feature).
    "$CLI_BIN" task create --title "Document the search feature" \
        --description "$(printf '## Goal\n\nExplain the **global search** strip.\n\n- matches sessions, tasks & automations\n- fuzzy across *title* and *description*')" \
        >/dev/null 2>&1 || true
    # An automation (spawn action, inferred from --repo) so the search demo has
    # a matching automation result too.
    "$CLI_BIN" automation create --name "nightly-triage" --trigger daily \
        --time "09:00" --repo "$DEMO_REPO" \
        --prompt "Triage failing CI and summarize blockers" >/dev/null 2>&1 || true
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

for tape in $TAPES; do
    # Re-apply before every tape: the `theme` clip switches themes (and persists
    # the change), so without this any tape after it would start on the wrong one.
    set_theme "$DEMO_THEME"
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
