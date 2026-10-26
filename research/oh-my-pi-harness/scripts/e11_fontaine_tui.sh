#!/usr/bin/env bash
# E11 (fontaine): the same tmux drive as e11_tui.sh against the host fontaine binary (manual permission mode).
# usage: e11_fontaine_tui.sh NAME
set -uo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd); NAME=$1
RUN=$HERE/runs/$NAME; rm -rf "$RUN"; mkdir -p "$RUN/ws" "$RUN/frames"; cp -a "$HERE/fixtures/repo-failing/." "$RUN/ws/"
echo "$NAME" > "$HERE/runs/CURRENT"
FONTAINE=${FONTAINE:-$HERE/../../rust/target/release/fontaine}
OUT=$RUN/frames; S=fe11
cap() { tmux capture-pane -p -t $S > "$OUT/$1.txt"; }
tmux kill-session -t $S 2>/dev/null
tmux new-session -d -s $S -x 120 -y 40 -c "$RUN/ws" "$FONTAINE --settings $HERE/scripts/proxy.toml --model go/deepseek-v4.1-flash --permission-mode manual; sleep 3"
sleep 8; cap 00_trust_prompt
# decline (default, no side effects: it asks again next time) so the real trust.toml is not written
tmux send-keys -t $S n; sleep 3; cap 00b_startup
tmux send-keys -t $S "/"; sleep 2; cap 01_slash_menu
tmux send-keys -t $S C-u; sleep 1
tmux send-keys -t $S "/resume"; sleep 1; tmux send-keys -t $S Enter; sleep 3; cap 02_session_picker
tmux send-keys -t $S Escape; sleep 1
tmux send-keys -t $S "Use the todo tool to plan: investigate, fix, verify. Fix the failing unit test in this directory (run python3 -m unittest -q) and make sure it passes." Enter
i=3
while [ $i -lt 60 ]; do
  sleep 4; n=$(printf '%02d' $i); cap ${n}_run
  if tmux capture-pane -p -t $S | grep -qE "Allow [a-z_]+\?"; then
    cp "$OUT/${n}_run.txt" "$OUT/${n}_approval_prompt.txt"
    tmux send-keys -t $S Enter
  fi
  i=$((i+1))
  [ $i -gt 12 ] && ! tmux capture-pane -p -t $S | grep -qiE "esc to|interrupt|thinking|working|⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏" && break
done
sleep 3; cap 90_final
tmux resize-window -t $S -x 60 -y 30 2>/dev/null; sleep 3; cap 91_narrow_60
tmux resize-window -t $S -x 120 -y 40 2>/dev/null; sleep 2
tmux send-keys -t $S C-c; sleep 1; tmux send-keys -t $S C-c; sleep 2
tmux kill-session -t $S 2>/dev/null
