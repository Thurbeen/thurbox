#!/bin/sh

set -u

SOCKET=${RUNTIME_TMUX_SH_SOCKET:-runtime-tmux-sh}
SESSION=runtime
KEEPER='!keeper'

answer() {
	printf '%s\n' "$1"
	exit 0
}

unknown() {
	answer "$(jq -cn --arg why "$1" '{unknown: {why: $why}}')"
}

tmux_without_lock_fd() {
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

find_pane() {
	if ! out=$(tmux_without_lock_fd list-panes -s -t "$SESSION" \
		-F '#{window_name} #{pane_id} #{pane_dead} #{pane_dead_status}' 2>&1); then
		case $out in
		*"no server running"* | *"error connecting"* | *"can't find session"*)
			pane=
			return 0
			;;
		*) unknown "tmux: $out" ;;
		esac
	fi
	# tmux may rewrite a tab for a client it does not take for UTF-8, so fields are space-separated.
	pane=$(printf '%s\n' "$out" | awk -v k="$key" '$1 == k { print $2, $3, $4 }')
	[ "$(printf '%s\n' "$pane" | grep -c .)" -le 1 ] ||
		unknown "several windows hold the key"
}

refuse_dir_others_can_write() {
	private_dir=$1
	# shellcheck disable=SC2012,SC2046
	set -- $(ls -ldn "$private_dir")
	[ "$3" = "$(id -u)" ] || unknown "$private_dir is not owned by this user"
	case $1 in
	d????-??-*) ;;
	*) unknown "$private_dir is writable by others" ;;
	esac
}

# The kernel releases an fd-9 flock however the script dies; perl stands in for flock(1) on macOS.
lock_starts_and_stops_per_server() {
	dir="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)"
	[ -d "$dir" ] || (umask 077 && mkdir -p "$dir") || unknown "cannot make $dir"
	refuse_dir_others_can_write "$dir"
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
	lock_starts_and_stops_per_server
	[ "$(field '.argv | length')" -gt 0 ] 2>/dev/null || unknown "argv names no program"
	command -v tmux >/dev/null 2>&1 ||
		answer '{"known":{"failed":{"why":"tmux is not installed"}}}'
	find_pane
	if [ -n "$pane" ]; then
		# shellcheck disable=SC2086
		set -- $pane
		[ "$2" = 0 ] &&
			answer "$(jq -cn --arg id "$1" '{known: {started: {id: $id, adopted: true}}}')"
		tmux_without_lock_fd kill-window -t "$1" 2>/dev/null
	fi
	if ! tmux_without_lock_fd has-session -t "$SESSION" 2>/dev/null; then
		tmux_without_lock_fd new-session -d -s "$SESSION" -n "$KEEPER" 'tail -f /dev/null' \; \
			set-option -g remain-on-exit on >/dev/null 2>&1 ||
			unknown "cannot start the tmux server"
	fi
	dir=$(field '.dir // empty')
	[ -n "$dir" ] || dir=$HOME
	env_options_then_argv=$(field '([.env // {} | to_entries[] | "-e", "\(.key)=\(.value)"] + ["--"] + .argv) | map(@sh) | join(" ")')
	eval "set -- $env_options_then_argv"
	id=$(tmux_without_lock_fd new-window -d -P -F '#{pane_id}' -t "$SESSION:" -n "$key" -c "$dir" "$@" 2>&1) ||
		unknown "tmux new-window: $id"
	answer "$(jq -cn --arg id "$id" '{known: {started: {id: $id, adopted: false}}}')"
	;;
inspect)
	check_key
	find_pane
	[ -n "$pane" ] || answer '{"known":"none"}'
	# shellcheck disable=SC2086
	set -- $pane
	[ "$2" = 0 ] && answer "$(jq -cn --arg id "$1" '{known: {running: {id: $id}}}')"
	code=${3:-}
	case $code in '' | *[!0-9]*) code=null ;; esac
	answer "$(jq -cn --arg id "$1" --argjson code "$code" '{known: {exited: {id: $id, code: $code}}}')"
	;;
stop)
	check_key
	lock_starts_and_stops_per_server
	find_pane
	if [ -n "$pane" ]; then
		# shellcheck disable=SC2086
		set -- $pane
		err=$(tmux_without_lock_fd kill-window -t "$1" 2>&1) || unknown "tmux kill-window: $err"
	fi
	answer '{"known":"stopped"}'
	;;
attach)
	check_key
	find_pane
	[ -n "$pane" ] || answer '{"known":"none"}'
	# shellcheck disable=SC2086
	set -- $pane
	socket_path_independent_of_viewer_tmpdir="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)/$SOCKET"
	answer "$(jq -cn --arg s "$socket_path_independent_of_viewer_tmpdir" --arg p "$1" \
		'{known: {tty: {argv: ["tmux", "-S", $s, "attach-session", "-t", $p]}}}')"
	;;
list)
	if ! out=$(tmux_without_lock_fd list-windows -t "$SESSION" -F '#{window_name}' 2>&1); then
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
