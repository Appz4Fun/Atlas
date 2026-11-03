#!/usr/bin/env bash
# Drive a local Atlas against a mock Usenet server. Run from the repo root.
#
#   driver.sh up            start mock NNTP + atlas menu (tmux "atlas"), start indexing, wait for releases
#   driver.sh keys K...     tmux send-keys to the menu, e.g. `keys 2 Enter`
#   driver.sh screen        print the menu's terminal (tmux capture-pane)
#   driver.sh api 'Q'       curl /api?Q with the apikey added, e.g. `api 't=search&q=movie'`
#   driver.sh shot OUT [PATH]  sign in to the web UI and screenshot PATH (default /) to OUT.png
#   driver.sh status        what's running, release count
#   driver.sh down          stop everything (the home folder stays for a look)
#
# Env: ATLAS_BIN (default target/release/atlas), ATLAS_HOME (default /tmp/atlas-run),
#      NNTP_PORT (1119), API_PORT (9099).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ATLAS_BIN="${ATLAS_BIN:-$PWD/target/release/atlas}"
export ATLAS_HOME="${ATLAS_HOME:-/tmp/atlas-run}"
export ATLAS_NO_KEYRING=1
NNTP_PORT="${NNTP_PORT:-1119}"
API_PORT="${API_PORT:-9099}"
KEY="driver-key"
BASE="http://127.0.0.1:$API_PORT"
SESSION=atlas
# absolute, because the menu is launched after a cd into ATLAS_HOME
mkdir -p "$ATLAS_HOME"
ATLAS_HOME="$(cd "$ATLAS_HOME" && pwd)"
case "$ATLAS_BIN" in /*) ;; *) ATLAS_BIN="$PWD/$ATLAS_BIN" ;; esac

count() { curl -s "$BASE/api?t=search&apikey=$KEY&limit=100" | { grep -o '<item>' || true; } | wc -l; }

case "${1:-}" in
up)
  [ -x "$ATLAS_BIN" ] || { echo "no $ATLAS_BIN; run: cargo build --release --locked" >&2; exit 1; }
  mkdir -p "$ATLAS_HOME"
  if ! (echo >/dev/tcp/127.0.0.1/"$NNTP_PORT") 2>/dev/null; then
    nohup python3 "$HERE/mock_nntp.py" --port "$NNTP_PORT" >"$ATLAS_HOME/mock_nntp.log" 2>&1 &
    echo $! >"$ATLAS_HOME/mock_nntp.pid"
    sleep 0.5
  fi
  if [ ! -f "$ATLAS_HOME/config.toml" ]; then
    cat >"$ATLAS_HOME/config.toml" <<EOF
group = "alt.binaries.test"
groups = ["alt.binaries.test"]
index_mode = "dynamic"
batch_size = 1000
request_size = 100
api_host = "127.0.0.1"
api_port = $API_PORT
api_key = "$KEY"

[[usenet_servers]]
host = "127.0.0.1"
username = "bob"
password = "secret"
port = $NNTP_PORT
ssl = false
compress = false
connections = 2
priority = 1

[postprocess]
settle_secs = 0
complete_settle_secs = 0
EOF
  fi
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  # wide window: the menus are ratatui panels and wrap badly under 120 cols
  tmux new-session -d -s "$SESSION" -x 160 -y 50 \
    "cd '$ATLAS_HOME' && ATLAS_HOME='$ATLAS_HOME' ATLAS_NO_KEYRING=1 '$ATLAS_BIN'; echo '[atlas exited]'; sleep 600"
  for _ in $(seq 60); do curl -sf "$BASE/api?t=caps" >/dev/null && break; sleep 0.5; done
  curl -sf "$BASE/api?t=caps" >/dev/null || { tmux capture-pane -pt "$SESSION"; echo "api never came up" >&2; exit 1; }
  # 1 = Start Indexing (spawns `atlas --bg-indexer`); only when it's not running yet
  if tmux capture-pane -pt "$SESSION" | grep -q 'Start Indexing'; then tmux send-keys -t "$SESSION" 1 Enter; fi
  # wait for releases, then until the count holds still for 3s (post-processing lands them a few at a time)
  last=-1; same=0
  for _ in $(seq 90); do
    n=$(count); if [ "$n" -gt 0 ] && [ "$n" = "$last" ]; then same=$((same + 1)); else same=0; fi
    last=$n; [ "$same" -ge 3 ] && break; sleep 1
  done
  echo "atlas up: web $BASE/  api key $KEY  home $ATLAS_HOME  releases $(count)"
  ;;
keys) shift; tmux send-keys -t "$SESSION" "$@" ;;
# the pane is 50 rows and menus draw at the top, so `screen | tail` would show
# only blank lines: drop trailing blank lines here
screen) tmux capture-pane -pt "$SESSION" | sed -e :a -e '/^[[:space:]]*$/{$d;N;ba' -e '}' ;;
api) shift; curl -s "$BASE/api?${1:-t=caps}&apikey=$KEY" ;;
shot) shift; node "$HERE/shot.cjs" "$BASE" "$KEY" "$@" ;;
status)
  # after Exit the tmux session stays up (sleep 600, so the last screen can be read),
  # so ask tmux whether atlas is still the pane's process
  if ! tmux has-session -t "$SESSION" 2>/dev/null; then echo "menu: stopped"
  elif tmux capture-pane -pt "$SESSION" | grep -q '\[atlas exited\]'; then echo "menu: exited (api down; run up again)"
  else echo "menu: running (tmux $SESSION)"; fi
  [ -f "$ATLAS_HOME/bg_indexer.pid" ] && kill -0 "$(cat "$ATLAS_HOME/bg_indexer.pid")" 2>/dev/null \
    && echo "indexer: running" || echo "indexer: stopped"
  (echo >/dev/tcp/127.0.0.1/"$NNTP_PORT") 2>/dev/null && echo "mock nntp: running :$NNTP_PORT" || echo "mock nntp: stopped"
  echo "releases: $(count)"
  ;;
down)
  # 0 (Exit) in the menu stops the api; the background indexer outlives the
  # menu on purpose, so stop it by its own pid file (SIGTERM, it saves and exits)
  tmux send-keys -t "$SESSION" 0 Enter 2>/dev/null || true
  sleep 1
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  if [ -f "$ATLAS_HOME/bg_indexer.pid" ]; then
    pid="$(cat "$ATLAS_HOME/bg_indexer.pid")"
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  fi
  # pid file, not `pkill -f mock_nntp.py`: that also matches (and kills) any shell whose command line names it
  [ -f "$ATLAS_HOME/mock_nntp.pid" ] && kill "$(cat "$ATLAS_HOME/mock_nntp.pid")" 2>/dev/null || true
  rm -f "$ATLAS_HOME/mock_nntp.pid"
  echo "stopped; data left in $ATLAS_HOME (rm -rf it for a fresh start)"
  ;;
*) sed -n '2,14p' "$0"; exit 1 ;;
esac
