#!/bin/sh
# Inside the container: start interactive omp under tmux with no prompt, wait, capture the pane, and dump config.
cd /run/study/ws
tmux new-session -d -s o -x 120 -y 40 "omp --model go/deepseek-v4.1-flash --no-session; sleep 5"
sleep ${WAIT:-25}
tmux capture-pane -p -t o > /run/study/pane_startup.txt
tmux send-keys -t o C-c; sleep 1; tmux send-keys -t o C-c; sleep 2
omp config --help > /run/study/config_help.txt 2>&1
omp config list > /run/study/config_list.txt 2>&1
