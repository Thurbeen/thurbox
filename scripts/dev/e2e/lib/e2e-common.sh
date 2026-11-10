# shellcheck shell=bash
#
# Shared helpers for the session-backend e2e family (scripts/dev/e2e/*.sh):
#
#   linux-container.sh   ephemeral Podman/Linux SSH host
#   windows-vm.sh        ephemeral dockur/Windows psmux host
#   real-host.sh         a real Linux/Windows/WSL machine you own
#
# The three harnesses share one concept — "provision (or point at) a host, create
# a session on it over the backend, assert it landed there" — so they share this
# one library: colour logging, the PASS/FAIL result contract, a dependency-free
# JSON field extractor (no python3), the `[[hosts]]` TOML emitter, and the
# create -> get -> assert core. Each entrypoint keeps only its own provisioning.
#
# Source it (don't execute) AFTER setting REPO_ROOT. Sourced only by the bash
# e2e scripts, so bashisms (local, arrays) are fine.

# ---- colour logging (identical across all three harnesses) -----------------

log()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarn:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Per-item check lines (used by readiness probes). `bad` bumps a caller-owned
# FAILS counter — initialise `FAILS=0` before the first `bad`.
ok()   { printf '  \033[1;32mok\033[0m   %s\n' "$*"; }
bad()  { printf '  \033[1;31mmiss\033[0m %s\n' "$*"; FAILS=$((FAILS + 1)); }
info() { printf '  \033[2m--\033[0m   %s\n' "$*"; }

# ---- shared PASS/FAIL result contract --------------------------------------
# One format for every harness. `pass`/`fail` print the final verdict; a harness
# ends on `pass "…"` (exit 0) or `fail "…"; return 1`. Set E2E_JSON=1 to also
# emit a machine-readable result line for CI/automation.

pass() {
  printf '\033[1;32mPASS\033[0m %s\n' "$*"
  [ "${E2E_JSON:-0}" = "1" ] && printf '{"result":"pass","message":"%s"}\n' "$*"
  return 0
}

fail() {
  printf '\033[1;31mFAIL\033[0m %s\n' "$*"
  [ "${E2E_JSON:-0}" = "1" ] && printf '{"result":"fail","message":"%s"}\n' "$*"
  return 1
}

# ---- hosts.toml [[hosts]] emitter ------------------------------------------

# hosts_block NAME DESTINATION [SSH_OPTS_TOML] [MULTIPLEXER] — emit a `[[hosts]]`
# block. SSH_OPTS_TOML is the verbatim TOML value for `ssh_opts` (a `[...]`
# array); omit for none. MULTIPLEXER adds a `multiplexer = "…"` line (e.g. psmux
# for a Windows host).
hosts_block() {
  local name="$1" dest="$2" ssh_opts="${3:-}" mux="${4:-}"
  printf '[[hosts]]\n'
  printf 'name = "%s"\n' "$name"
  printf 'destination = "%s"\n' "$dest"
  [ -n "$ssh_opts" ] && printf 'ssh_opts = %s\n' "$ssh_opts"
  [ -n "$mux" ] && printf 'multiplexer = "%s"\n' "$mux"
  return 0
}
