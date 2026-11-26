# shellcheck shell=bash

log()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarn:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

ok()   { printf '  \033[1;32mok\033[0m   %s\n' "$*"; }
bad()  { printf '  \033[1;31mmiss\033[0m %s\n' "$*"; FAILS=$((${FAILS:-0} + 1)); }
info() { printf '  \033[2m--\033[0m   %s\n' "$*"; }

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
