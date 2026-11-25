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
mkdir -p "$DEMO/app" "$DEMO/src/backend" "$DEMO/src/frontend" "$DEMO/src/docs"
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

cat >"$DEMO/src/backend/server.py" <<'SERVER'
import json
from http.server import BaseHTTPRequestHandler, HTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/api/greeting":
            body = json.dumps({"greeting": "Hello, world!"}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)


if __name__ == "__main__":
    HTTPServer(("127.0.0.1", 8000), Handler).serve_forever()
SERVER
cat >"$DEMO/src/frontend/index.html" <<'HTML'
<!doctype html>
<title>Greeting</title>
<p id="greeting">Loading...</p>
<script src="app.js"></script>
HTML
cat >"$DEMO/src/frontend/app.js" <<'SCRIPT'
fetch("/api/greeting")
  .then((r) => r.json())
  .then((data) => {
    document.getElementById("greeting").textContent = data.greeting;
  });
SCRIPT
printf '%s\n' '# Greeting' '' 'A backend that serves a greeting, and a frontend that shows it.' >"$DEMO/src/docs/README.md"
for repo in backend frontend docs; do
  (cd "$DEMO/src/$repo" && git init -q -b main && git add . &&
    git -c user.name=demo -c user.email=demo@example.invalid commit -qm "$repo")
done

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

type_into() {
  local terminal="$1" text="$2" i c
  for ((i = 0; i < ${#text}; i++)); do
    c="${text:i:1}"
    case "$c" in
      " ") c=space ;;
      [A-Z]) c="shift+${c,,}" ;;
    esac
    hc keys "$terminal" "$c" >/dev/null
    sleep 0.02
  done
}

REQUEST="Dispatch three workers at once, one per repository in $DEMO/src. In backend, add a GET /api/health endpoint that answers ok, with a unittest. In frontend, show the backend health next to the greeting. In docs, document both endpoints in the README. Each worker commits on its own branch. Local only, no push and no pull request."

step "record"
ffmpeg -loglevel error -y -f x11grab -framerate 10 -video_size "${SCREEN_W}x${SCREEN_H}" -i "$DISPLAY" \
  -c:v libx264 -preset ultrafast -crf 18 -pix_fmt yuv420p "$DEMO/raw.mp4" &
recorder=$!
pids+=("$recorder")
started=$(date +%s)
mark() { echo "$1 $(($(date +%s) - started))" >>"$DEMO/marks"; }

vega_terminal="$(terminal_of "$vega_key")"
pause 2
app agent.focus --terminal "$vega_terminal"
pause 3
type_into "$vega_terminal" "$REQUEST"
pause 2
hc keys "$vega_terminal" enter >/dev/null
pause 4
mark work-start

workers=()
declare -A unsent_since=()
finished=
deadline=$(($(date +%s) + WORK_TIMEOUT))
while [ "$(date +%s)" -lt "$deadline" ]; do
  mapfile -t workers < <(fleet list task --json 2>/dev/null | jq -r '.[] | select(.metadata.name != "vega" and .spec.dispatch) | .metadata.name')
  shown=0
  for w in "${workers[@]}"; do
    k="$(key_of "$w" || true)"
    t="$( [ -n "$k" ] && terminal_of "$k" || true)"
    if [ -n "$t" ] && hc read "$t" 2>/dev/null | tail -6 | grep -q "❯ \[Pasted text"; then
      unsent_since[$w]="${unsent_since[$w]:-$(date +%s)}"
      if [ $(($(date +%s) - unsent_since[$w])) -gt 90 ]; then
        KEEP=1
        step "the brief of $w sat in its composer unsent for 90s: no clip written"
        exit 1
      fi
    else
      unset "unsent_since[$w]"
    fi
    if [ -n "$t" ] && ! grep -qx "$w" "$DEMO/shown" 2>/dev/null; then
      echo "$w" >>"$DEMO/shown"
      app agent.focus --terminal "$t"
      shown=1
      pause 6
    fi
  done
  if [ "${#workers[@]}" -ge 3 ]; then
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
app agent.focus --terminal "$vega_terminal"
pause 10
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
