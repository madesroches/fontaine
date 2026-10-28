#!/usr/bin/env bash
# Bring up the study side infrastructure (issue #253):
#   - docker network hermes-study-net
#   - host-side logging proxy on 0.0.0.0:18090 -> 127.0.0.1:11434 (Ollama)
#   - fake Grafana/TeamCity container "monitor" on hermes-study-net:8080
# Usage: study_up.sh <capture-subdir>   (e.g. captures/e1)
set -euo pipefail
cd "$(dirname "$0")/.."
CAP="$PWD/${1:-captures/misc}"
mkdir -p "$CAP" runs/monitor-state captures
docker network inspect hermes-study-net >/dev/null 2>&1 || docker network create hermes-study-net >/dev/null
# Fake monitor (rebuilt cheaply each time; python:3.14-slim base is shared).
docker rm -f monitor >/dev/null 2>&1 || true
cat > "$PWD/runs/monitor-state/Dockerfile" <<'EOF'
FROM python:3.14-slim-trixie
COPY fake_monitor.py /fake_monitor.py
ENTRYPOINT ["python3", "/fake_monitor.py"]
EOF
cp fixtures/fake_monitor.py "$PWD/runs/monitor-state/"
docker build -q -t hermes-study-monitor "$PWD/runs/monitor-state" >/dev/null
[ -f runs/monitor-state/monitor_state.json ] || echo '{"mode": "green"}' > runs/monitor-state/monitor_state.json
docker run -d --rm --name monitor --network hermes-study-net --network-alias monitor \
  -v "$PWD/runs/monitor-state/monitor_state.json:/state/monitor_state.json" \
  -v "$PWD/captures:/captures" \
  hermes-study-monitor >/dev/null
# Log proxy on the host (restarts it if already running for another capture dir).
if [ -f runs/log_proxy.pid ] && kill -0 "$(cat runs/log_proxy.pid)" 2>/dev/null; then
  kill "$(cat runs/log_proxy.pid)" 2>/dev/null || true
  sleep 0.3
fi
nohup python3 scripts/log_proxy.py "$CAP" 18090 > runs/log_proxy.out 2>&1 &
echo $! > runs/log_proxy.pid
echo "monitor: http://monitor:8080 (state: runs/monitor-state/monitor_state.json)"
echo "proxy:   http://host.docker.internal:18090 -> ollama 127.0.0.1:11434 (captures: $CAP)"
