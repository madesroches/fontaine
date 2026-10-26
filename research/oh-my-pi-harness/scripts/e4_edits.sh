#!/usr/bin/env bash
# E4: the same four edit tasks through omp (hashline forced with PI_STRICT_EDIT_MODE) and Fontaine.
# usage: e4_edits.sh omp|omp-default|fontaine
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
WHO=$1
declare -A T
T[multi]="In report.py, rename the parameter 'width' to 'column_width' in all three functions, including every use of it in their bodies. Use the edit tool; do not use sed."
T[tabs]="In Makefile, change the compiler flag -O2 to -O3 on the CFLAGS line, and add a new rule after 'clean' named 'test' whose recipe line runs './app --self-test'. Recipes must stay tab-indented. Use the edit tool; do not use sed."
T[stale]="First read cfg.py. Then, using the bash tool, append the line '# generated' to the end of cfg.py. Then, WITHOUT reading cfg.py again, use the edit tool to change DEFAULT_TIMEOUT from 10 to 30."
T[dup]="In dup.py, change only the find_group function so that it returns {} instead of None when nothing matches. Leave find_user and find_role unchanged. Use the edit tool; do not use sed."
for k in multi tabs stale dup; do
  case $WHO in
    omp) FIXTURE=edit-tasks GIT=1 XENV="-e PI_STRICT_EDIT_MODE=1" "$HERE/run_omp.sh" "e4-$k-omp-hashline" "${T[$k]}" ;;
    omp-default) FIXTURE=edit-tasks GIT=1 "$HERE/run_omp.sh" "e4-$k-omp-default" "${T[$k]}" ;;
    fontaine) FIXTURE=edit-tasks "$HERE/run_fontaine.sh" "e4-$k-fontaine" proxy.toml "${T[$k]}" --permission-mode acceptEdits --allowed-tools 'shell(cat *)' ;;
  esac
done
