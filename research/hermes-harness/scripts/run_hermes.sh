#!/usr/bin/env bash
# Study runner for the Hermes harness research (issue #253).
# Usage: run_hermes.sh <run-name> [hermes args...]
#   run_hermes.sh e1-smoke chat -q "hello"
#   run_hermes.sh e2-watcher gateway   (long-running; use docker stop hermes-<run-name>)
# State for the run lives in runs/<run-name>/home (HERMES_HOME), captures in
# captures/. The fake monitor and the host-side log proxy must be up first:
#   scripts/study_up.sh
set -euo pipefail
cd "$(dirname "$0")/.."
RUN="$1"; shift
IMAGE="hermes-study:c7fe850"
NET="hermes-study-net"
HOME_DIR="$PWD/runs/$RUN/home"
mkdir -p "$HOME_DIR"
# Seed config if absent: custom provider pointing at the host log proxy
# (which forwards to the local Ollama), qwen3:4b-64k as the model.
if [ ! -f "$HOME_DIR/config.yaml" ]; then
  cat > "$HOME_DIR/config.yaml" <<'YAML'
model:
  provider: "custom"
  base_url: "http://host.docker.internal:18090/v1"
  default: "qwen3:4b-64k"
YAML
fi
# Run as the host user: hermes refuses a HERMES_HOME owned by another uid
# ("unsafe_control_path"), so the bind-mounted state dir and the process
# must share an owner.
exec docker run --rm --init --name "hermes-$RUN" \
  --user "$(id -u):$(id -g)" \
  --network "$NET" --add-host host.docker.internal:host-gateway \
  -v "$HOME_DIR:/opt/data" \
  -e HERMES_HOME=/opt/data \
  "$IMAGE" "$@"
