#!/bin/sh
# A runtime in one script: the runtime protocol (docs/runtime/README.md)
# over tmux, with jq for JSON and flock(1) or perl for a lock. `tmux.sh <verb>` reads one JSON request on
# stdin and prints one answer: {"known": ...} or {"unknown": {"why": ...}}.
#
# `attach` answers a raw tmux client (`tty`), which `fleet attach` bridges.
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
	tmux -L "$SOCKET" "$@" 9>&-
}

verb=${1:-}
if [ "$verb" = manifest ]; then
	answer '{"known":{"protocol":1,"verbs":["manifest","start","inspect","stop","list","attach"]}}'
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
# then makes its window, and two at once would each make one. The lock is
# flock(2) on a file in the private directory tmux keeps its socket in,
# held on fd 9 until the script ends, so the kernel releases it however the
# script dies: flock(1) takes it where util-linux is installed, perl (as on
# macOS) elsewhere. `t` closes fd 9, so the tmux server never holds it.
take_lock() {
	dir="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)"
	[ -d "$dir" ] || (umask 077 && mkdir -p "$dir") || unknown "cannot make $dir"
	# Another user able to write there could plant a link at the lock's
	# name: tmux refuses such a directory for its socket, and so does this.
	# shellcheck disable=SC2012,SC2046 # ls -ldn is the portable owner and mode, split on purpose
	set -- $(ls -ldn "$dir")
	[ "$3" = "$(id -u)" ] || unknown "$dir is not owned by this user"
	case $1 in
	d????-??-*) ;;
	*) unknown "$dir is writable by others" ;;
	esac
	# Appending never truncates what the name points at.
	exec 9>>"$dir/runtime-tmux-sh-$SOCKET.lock" || unknown "cannot open the lock file"
	if command -v flock >/dev/null 2>&1; then
		flock -w 10 9
	else
		perl -MFcntl=:flock -e 'open(my $f, ">&=9") or exit 2;
			for (1 .. 200) { exit 0 if flock($f, LOCK_EX | LOCK_NB); select(undef, undef, undef, 0.05) }
			exit 1'
	fi || unknown "another start or stop holds the lock"
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
attach)
	# A raw tmux client on the key's pane: Fleet bridges it into a stream.
	# By socket path, as the view may run without this TMUX_TMPDIR.
	check_key
	find_pane
	[ -n "$pane" ] || answer '{"known":"none"}'
	# shellcheck disable=SC2086 # three words, split on purpose
	set -- $pane
	sock="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)/$SOCKET"
	answer "$(jq -cn --arg s "$sock" --arg p "$1" \
		'{known: {tty: {argv: ["tmux", "-S", $s, "attach-session", "-t", $p]}}}')"
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
