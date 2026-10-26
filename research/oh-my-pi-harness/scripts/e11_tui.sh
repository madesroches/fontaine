#!/bin/sh
# E11 (omp): drive the interactive TUI under tmux at 120x40 and capture panes as text.
# Frames: f_<n>_<label>.txt. Approval prompts are answered with Enter on the first choice.
cd /run/study/ws
OUT=/run/study/frames; mkdir -p $OUT
cap() { tmux capture-pane -p -t o > "$OUT/$1.txt"; }
tmux new-session -d -s o -x 120 -y 40 "omp --model go/deepseek-v4.1-flash --approval-mode write --no-session; sleep 3"
sleep 15; cap 00_startup
tmux send-keys -t o "/"; sleep 2; cap 01_slash_menu
tmux send-keys -t o C-u; sleep 1
tmux send-keys -t o "/resume"; sleep 1; tmux send-keys -t o Enter; sleep 3; cap 02_session_picker
tmux send-keys -t o Escape; sleep 1
tmux send-keys -t o "Use the todo tool to plan: investigate, fix, verify. Fix the failing unit test in this directory (run python3 -m unittest -q) and make sure it passes." Enter
i=3; approved=0
while [ $i -lt 60 ]; do
  sleep 4; n=$(printf '%02d' $i); cap ${n}_run
  if tmux capture-pane -p -t o | grep -qiE "approve|requires approval|Deny"; then
    cp "$OUT/${n}_run.txt" "$OUT/${n}_approval_prompt.txt"
    tmux send-keys -t o Enter; approved=$((approved+1))
  fi
  if tmux capture-pane -p -t o | grep -qE "Working|Thinking|⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏|esc to interrupt"; then :; else
    [ $i -gt 8 ] && break
  fi
  i=$((i+1))
done
sleep 3; cap 90_final
tmux resize-window -t o -x 60 -y 30 2>/dev/null; sleep 3; cap 91_narrow_60
tmux resize-window -t o -x 120 -y 40 2>/dev/null; sleep 2
tmux send-keys -t o "/" ; sleep 1; tmux send-keys -t o "h" "e" "l" "p" Enter; sleep 3; cap 92_help
tmux send-keys -t o Escape; sleep 1
tmux send-keys -t o C-c; sleep 1; tmux send-keys -t o C-c; sleep 2
