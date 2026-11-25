#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
OUT="${OUT:-$ROOT/media}"
NAME=herdr-fleet-vega
RUN=$$
SESSION="fleet-demo-$RUN"
PREFIX="herdrdemo$RUN"
SCREEN_W=1280
SCREEN_H=820
DISPLAY_NUM="${DISPLAY_NUM:-:97}"
WORK_TIMEOUT="${WORK_TIMEOUT:-1500}"
WORK_SECONDS="${WORK_SECONDS:-24}"
DEMO="$(mktemp -d /tmp/hfv.XXXX)"

for tool in Xvfb ffmpeg jq git hc herdr-app claude; do
  command -v "$tool" >/dev/null || { echo "record-herdr: $tool is not on PATH" >&2; exit 1; }
done

(cd "$ROOT" && cargo build --quiet -p fleet-cli --bin fleet && cargo build --quiet -p runtime-herdr -p workspace-git)
export PATH="$ROOT/target/debug:$PATH"
export HC_SESSION="$SESSION"
export HERDR_APP_HOME="$DEMO/app"
export DISABLE_AUTOUPDATER=1
export FLEET_CONFIG="$DEMO/fleet.toml"
export DISPLAY="$DISPLAY_NUM"
unset TMUX

reset_session() {
  local t
  for t in $(hc ls --all 2>/dev/null | jq -r '.terminals[].address'); do
    hc kill "$t" >/dev/null 2>&1 || true
  done
  sleep 2
  for t in $(hc ls --all 2>/dev/null | jq -r '.terminals[].address'); do
    hc rm "$t" >/dev/null 2>&1 || true
  done
}

pids=()
cleanup() {
  herdr-app "do" window.close >/dev/null 2>&1 || true
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  reset_session
  hc server stop >/dev/null 2>&1 || true
  rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/runtime-herdr/workspaces/local/$PREFIX."*
  find "${XDG_STATE_HOME:-$HOME/.local/state}/runtime-herdr" -name "$PREFIX.*.start" -delete 2>/dev/null || true
  if [ -z "${KEEP:-}" ]; then rm -rf "$DEMO"; else echo "kept $DEMO" >&2; fi
}
trap cleanup EXIT

step() { printf '[record-herdr] %s\n' "$*" >&2; }
pause() { sleep "$1"; }
app() { herdr-app "do" "$@" >/dev/null; }
key_of() { fleet status task "$1" | awk '/^session / {print $2}'; }
terminal_of() { hc ls --label "fleet.key=$1" | jq -r '[.terminals[] | .address][0] // empty'; }

step "scratch dir $DEMO"
mkdir -p "$DEMO/app" "$DEMO/src/greeter"
cat >"$FLEET_CONFIG" <<EOF
version = 1

[data]
dir = "$DEMO/store"

[runtimes.herdr]
program = "runtime-herdr"
keyPrefix = "$PREFIX"

[runtimes.herdr.places.local]
session = "$SESSION"
EOF

(
  cd "$DEMO/src/greeter"
  git init -q -b main
  printf 'import sys\n\n\ndef greet(name):\n    return f"Hello, {name}!"\n\n\nif __name__ == "__main__":\n    print(greet(sys.argv[1] if len(sys.argv) > 1 else "world"))\n' >greet.py
  printf '%s\n' '# greeter' '' 'python3 greet.py NAME prints a greeting.' >README.md
  git add . && git -c user.name=demo -c user.email=demo@example.invalid commit -qm "greeter"
)

VEGA_DIR="${VEGA_DIR:-$DEMO/vega}"
[ -d "$VEGA_DIR" ] || git clone -q https://github.com/Thurbeen/vega "$VEGA_DIR"

step "herdr-core session $SESSION, Xvfb $DISPLAY, herdr-app"
reset_session
Xvfb "$DISPLAY" -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -nolisten tcp -nocursor >/dev/null 2>&1 &
pids+=($!)
pause 2
printf '{"layout":"main","frame.welcomed":"skipped"}' >"$HERDR_APP_HOME/shared.json"
herdr-app >"$DEMO/app.log" 2>&1 &
pids+=($!)
for _ in $(seq 1 30); do herdr-app status >/dev/null 2>&1 && break; pause 1; done

step "fleet up"
fleet up >"$DEMO/fleet-up.log" 2>&1 &
pids+=($!)

trust_watch() {
  while :; do
    for t in $(hc ls | jq -r '.terminals[] | select(.agent == "claude-code" or .agent == "claude") | .address'); do
      screen="$(hc read "$t" 2>/dev/null | tail -12)"
      if printf '%s' "$screen" | grep -q "Yes, I trust this folder"; then
        printf '%s' "$screen" | grep -q "❯ Yes, I trust this folder" || hc keys "$t" down >/dev/null 2>&1 || true
        hc keys "$t" enter >/dev/null 2>&1 || true
      fi
    done
    sleep 2
  done
}
trust_watch &
pids+=($!)

step "install VEGA"
"$VEGA_DIR/install.sh" --config "$FLEET_CONFIG" --harness claude >"$DEMO/vega-install.log" 2>&1
for _ in $(seq 1 120); do
  vega_key="$(key_of vega || true)"
  [ -n "$vega_key" ] && [ -n "$(terminal_of "$vega_key")" ] && break
  pause 2
done
[ -n "${vega_key:-}" ] || { echo "record-herdr: VEGA did not start" >&2; exit 1; }
for _ in $(seq 1 90); do
  [ "$(fleet get task vega | jq -r '.status.agentState // empty')" = idle ] && break
  pause 2
done

step "the shell the viewer types in"
you="$(hc run --bg --name you "$SHELL" | jq -r .address)"
hc label "$you" herdr.space=you herdr.tab=you >/dev/null
pause 2
hc send "$you" "export FLEET_CONFIG=$FLEET_CONFIG PATH=$PATH USER=you; cd $DEMO/src/greeter; clear" >/dev/null
pause 2

REQUEST="In git+file://$DEMO/src/greeter, dispatch two workers at once: one adds a --shout flag to greet.py that prints the greeting in capitals, the other adds tests for greet.py with unittest. Each one commits on its own branch and runs the tests. Local only: no push, no pull request."

step "record"
ffmpeg -loglevel error -y -f x11grab -framerate 10 -video_size "${SCREEN_W}x${SCREEN_H}" -i "$DISPLAY" \
  -c:v libx264 -preset ultrafast -crf 18 -pix_fmt yuv420p "$DEMO/raw.mp4" &
recorder=$!
pids+=("$recorder")
started=$(date +%s)
mark() { echo "$1 $(($(date +%s) - started))" >>"$DEMO/marks"; }

pause 2
app agent.focus --terminal "$(terminal_of "$vega_key")"
pause 4
app pane.focus --terminal "$you"
pause 2
hc send "$you" "fleet list task" >/dev/null
pause 4
hc send "$you" "fleet send vega \"$REQUEST\"" >/dev/null
pause 5
mark work-start
app agent.focus --terminal "$(terminal_of "$vega_key")"

workers=()
finished=
deadline=$(($(date +%s) + WORK_TIMEOUT))
while [ "$(date +%s)" -lt "$deadline" ]; do
  mapfile -t workers < <(fleet list task --json 2>/dev/null | jq -r '.[] | select(.metadata.name != "vega" and .spec.dispatch) | .metadata.name')
  shown=0
  for w in "${workers[@]}"; do
    k="$(key_of "$w" || true)"
    t="$( [ -n "$k" ] && terminal_of "$k" || true)"
    if [ -n "$t" ] && ! grep -qx "$w" "$DEMO/shown" 2>/dev/null; then
      echo "$w" >>"$DEMO/shown"
      app agent.focus --terminal "$t"
      shown=1
      pause 6
    fi
  done
  if [ "${#workers[@]}" -ge 2 ]; then
    done_count=$(fleet list task --json | jq '[.[] | select(.metadata.name != "vega" and .spec.dispatch and .status.result != null)] | length')
    if [ "$done_count" -ge "${#workers[@]}" ]; then finished=1; break; fi
  fi
  [ "$shown" = 1 ] || pause 5
done
if [ -z "$finished" ]; then
  KEEP=1
  step "the workers did not all report a result in ${WORK_TIMEOUT}s: no clip written"
  exit 1
fi
mark work-end
for w in "${workers[@]}"; do
  k="$(key_of "$w" || true)"; t="$( [ -n "$k" ] && terminal_of "$k" || true)"
  [ -n "$t" ] && { app agent.focus --terminal "$t"; pause 4; }
done
app agent.focus --terminal "$(terminal_of "$vega_key")"
pause 6
app pane.focus --terminal "$you"
hc send "$you" "clear; fleet list task" >/dev/null
pause 3
hc send "$you" "git branch --list" >/dev/null
pause 6
mark end
kill -INT "$recorder"
wait "$recorder" 2>/dev/null || true

step "cut"
read -r _ work_start < <(grep '^work-start' "$DEMO/marks")
read -r _ work_end < <(grep '^work-end' "$DEMO/marks")
read -r _ end < <(grep '^end' "$DEMO/marks")
span=$((work_end - work_start))
speed=$(awk -v s="$span" -v w="$WORK_SECONDS" 'BEGIN { f = s / w; print (f < 1 ? 1 : f) }')
mkdir -p "$OUT"
ffmpeg -loglevel error -y -i "$DEMO/raw.mp4" -filter_complex "\
[0:v]trim=0:${work_start},setpts=PTS-STARTPTS[a];\
[0:v]trim=${work_start}:${work_end},setpts=(PTS-STARTPTS)/${speed}[b];\
[0:v]trim=${work_end}:${end},setpts=PTS-STARTPTS[c];\
[a][b][c]concat=n=3:v=1:a=0,fps=15,scale=1280:-2:flags=lanczos[v]" \
  -map '[v]' -c:v libx264 -crf 22 -preset slow -pix_fmt yuv420p -movflags +faststart "$OUT/$NAME.mp4"
ffmpeg -loglevel error -y -i "$OUT/$NAME.mp4" -filter_complex \
  "fps=8,scale=960:-2:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=128[p];[s1][p]paletteuse=dither=bayer:bayer_scale=4" \
  "$OUT/$NAME.gif"
step "wrote $OUT/$NAME.mp4 and $OUT/$NAME.gif (work sped up ${speed}x over ${span}s)"
