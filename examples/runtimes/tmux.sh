#!/bin/sh
# A runtime in one script: the runtime protocol (docs/runtime/README.md)
# over tmux, with jq for JSON. `tmux.sh <verb>` reads one JSON request on
# stdin and prints one answer: {"known": ...} or {"unknown": {"why": ...}}.
#
# Each process is a tmux window named after its key, in one session on a
# socket of its own (RUNTIME_TMUX_SH_SOCKET, default runtime-tmux-sh). A
# keeper window holds the server, and remain-on-exit keeps an exited
# process's pane, so `inspect` can report its code.
#
# Check it with: fleet runtime check -- examples/runtimes/tmux.sh

set -u

SOCKET=${RUNTIME_TMUX_SH_SOCKET:-runtime-tmux-sh}
SESSION=runtime
# Not a key: `!` is outside the characters a key may hold.
KEEPER='!keeper'

answer() {
	printf '%s\n' "$1"
	exit 0
}

unknown() {
	answer "$(jq -cn --arg why "$1" '{unknown: {why: $why}}')"
}

t() {
	tmux -L "$SOCKET" "$@"
}

verb=${1:-}
if [ "$verb" = manifest ]; then
	answer '{"known":{"protocol":1,"verbs":["manifest","start","inspect","stop","list"]}}'
fi
command -v jq >/dev/null 2>&1 || answer '{"unknown":{"why":"jq is not installed"}}'

req=$(cat)
printf '%s' "$req" | jq -e 'type == "object"' >/dev/null 2>&1 ||
	unknown "the request is not a JSON object"
[ "$(printf '%s' "$req" | jq -r '.protocol // empty')" = 1 ] ||
	unknown "this runtime speaks protocol 1"

field() {
	printf '%s' "$req" | jq -r "$1"
}

check_key() {
	key=$(field '.key // empty')
	case $key in
	'' | *[!A-Za-z0-9._-]*) unknown "key: use ASCII letters, digits, '.', '-' or '_'" ;;
	esac
	[ ${#key} -le 128 ] || unknown "key: at most 128 characters"
}

# Sets `pane` to "<pane id> <dead> <exit status>" for the key's window, or
# to nothing when there is none. Answers Unknown when tmux cannot say.
find_pane() {
	if ! out=$(t list-panes -s -t "$SESSION" \
		-F '#{window_name} #{pane_id} #{pane_dead} #{pane_dead_status}' 2>&1); then
		case $out in
		*"no server running"* | *"error connecting"* | *"can't find session"*)
			pane=
			return 0
			;;
		*) unknown "tmux: $out" ;;
		esac
	fi
	# Spaces apart: a key holds none, and tmux keeps a space where it may
	# rewrite a tab for a client it does not take for UTF-8.
	pane=$(printf '%s\n' "$out" | awk -v k="$key" '$1 == k { print $2, $3, $4 }')
	[ "$(printf '%s\n' "$pane" | grep -c .)" -le 1 ] ||
		unknown "several windows hold the key"
}

# Starts and stops run one at a time per server: a start looks the key up,
# then makes its window, and two at once would each make one. The lock is a
# symbolic link beside where tmux keeps the socket, naming its holder's pid:
# `ln -s` makes it with its holder in one step, or fails. A holder that died
# without removing it (SIGKILL runs no trap) no longer answers `kill -0`, and
# the one waiter that makes `<lock>.reap.<pid>` removes its lock; a reaper
# that died is replaced the same way.
alive() {
	[ -n "$1" ] && kill -0 "$1" 2>/dev/null
}

take_lock() {
	lock="${TMUX_TMPDIR:-/tmp}/runtime-tmux-sh-$(id -u)-$SOCKET.lock"
	tries=0
	until ln -sn "$$" "$lock" 2>/dev/null; do
		holder=$(readlink "$lock" 2>/dev/null)
		if [ -n "$holder" ] && ! alive "$holder"; then
			reap="$lock.reap.$holder"
			reaper=$(readlink "$reap" 2>/dev/null)
			[ -n "$reaper" ] && ! alive "$reaper" && rm -f "$reap"
			if ln -sn "$$" "$reap" 2>/dev/null; then
				[ "$(readlink "$lock" 2>/dev/null)" = "$holder" ] && rm -f "$lock"
				rm -f "$reap"
				continue
			fi
		fi
		tries=$((tries + 1))
		[ "$tries" -lt 200 ] || unknown "another start or stop holds $lock"
		sleep 0.05
	done
	trap 'rm -f "$lock"' EXIT
	trap 'exit 1' INT TERM HUP
}

case $verb in
start)
	check_key
	take_lock
	[ "$(field '.argv | length')" -gt 0 ] 2>/dev/null || unknown "argv names no program"
	command -v tmux >/dev/null 2>&1 ||
		answer '{"known":{"failed":{"why":"tmux is not installed"}}}'
	find_pane
	if [ -n "$pane" ]; then
		# shellcheck disable=SC2086 # three words, split on purpose
		set -- $pane
		[ "$2" = 0 ] &&
			answer "$(jq -cn --arg id "$1" '{known: {started: {id: $id, adopted: true}}}')"
		t kill-window -t "$1" 2>/dev/null
	fi
	if ! t has-session -t "$SESSION" 2>/dev/null; then
		t new-session -d -s "$SESSION" -n "$KEEPER" 'tail -f /dev/null' \; \
			set-option -g remain-on-exit on >/dev/null 2>&1 ||
			unknown "cannot start the tmux server"
	fi
	dir=$(field '.dir // empty')
	[ -n "$dir" ] || dir=$HOME
	# The environment as -e options, then the argv, each word quoted by jq.
	words=$(field '([.env // {} | to_entries[] | "-e", "\(.key)=\(.value)"] + ["--"] + .argv) | map(@sh) | join(" ")')
	eval "set -- $words"
	id=$(t new-window -d -P -F '#{pane_id}' -t "$SESSION:" -n "$key" -c "$dir" "$@" 2>&1) ||
		unknown "tmux new-window: $id"
	answer "$(jq -cn --arg id "$id" '{known: {started: {id: $id, adopted: false}}}')"
	;;
inspect)
	check_key
	find_pane
	[ -n "$pane" ] || answer '{"known":"none"}'
	# shellcheck disable=SC2086 # three words, split on purpose
	set -- $pane
	[ "$2" = 0 ] && answer "$(jq -cn --arg id "$1" '{known: {running: {id: $id}}}')"
	code=${3:-}
	case $code in '' | *[!0-9]*) code=null ;; esac
	answer "$(jq -cn --arg id "$1" --argjson code "$code" '{known: {exited: {id: $id, code: $code}}}')"
	;;
stop)
	check_key
	take_lock
	find_pane
	if [ -n "$pane" ]; then
		# shellcheck disable=SC2086 # three words, split on purpose
		set -- $pane
		err=$(t kill-window -t "$1" 2>&1) || unknown "tmux kill-window: $err"
	fi
	answer '{"known":"stopped"}'
	;;
list)
	if ! out=$(t list-windows -t "$SESSION" -F '#{window_name}' 2>&1); then
		case $out in
		*"no server running"* | *"error connecting"* | *"can't find session"*) out= ;;
		*) unknown "tmux: $out" ;;
		esac
	fi
	answer "$(printf '%s\n' "$out" | jq -cR -s --arg keeper "$KEEPER" \
		'{known: {keys: (split("\n") | map(select(length > 0 and . != $keeper)) | unique)}}')"
	;;
*)
	unknown "unknown verb: $verb"
	;;
esac
