#!/usr/bin/env bash
# Run kimi once inside Docker with the study's network and capture setup.
# usage: run_kimi.sh NAME "task text"
#   env: FIXTURE=<dir under fixtures/>  MODEL=<alias in config.template.toml> (default kc)  KIMI_ARGS="extra args"
#        EXTRA_CONFIG=<file appended to config.toml>  TELEMETRY_OFF=1  STALL=1  NOPROMPT=1 (run KIMI_ARGS only)
#        DEEPSEEK-style hooks: PRE=<cmd run in container before>  POST=<cmd run in container after>
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
NAME=$1; TASK=${2:-}
RUN=$HERE/runs/$NAME
IMG=kimi-study:2.5.1; NETIMG=kimi-study-net
NET=kimi-study-$NAME
BR=172.17.0.1
mkdir -p "$RUN/kimi-home" "$RUN/ws"
if [ -n "${FIXTURE:-}" ]; then cp -a "$HERE/fixtures/$FIXTURE/." "$RUN/ws/"; fi
if [ ! -f "$RUN/kimi-home/config.toml" ]; then
  cp "$HERE/scripts/config.template.toml" "$RUN/kimi-home/config.toml"
  [ -n "${EXTRA_CONFIG:-}" ] && cat "$EXTRA_CONFIG" >> "$RUN/kimi-home/config.toml"
fi
cp "$HERE/scripts/"*.mjs "$RUN/" 2>/dev/null || true
chmod -R a+rwX "$RUN"

echo "$NAME" > "$HERE/runs/CURRENT"
python3 - <<PY || { echo "proxies not reachable on $BR:18080/18082 (scripts/start_proxies.sh)" >&2; exit 2; }
import socket
for p in (18080, 18082): socket.create_connection(("$BR",p),timeout=2)
PY
# madesroches token freshness: report remaining lifetime without printing the token
python3 -I - <<'PY'
import tomllib,time,os
d=tomllib.load(open(os.path.expanduser("~/.local/share/fontaine/credentials.toml"),"rb"))
left=d["accounts"]["madesroches"]["oauth"]["expires_at"]-time.time()
print(f"madesroches access token remaining: {left/3600:.1f} h")
if left < 1800:
    raise SystemExit("token has < 30 min left: run a plain `fontaine -p 'Reply with OK.' --model madesroches/kimi-k3` (not via the proxy), then rerun")
PY

cleanup() {
  docker rm -f "kimi-$NAME" "cap-$NAME" "fwd-$NAME" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup
docker network create --internal "$NET" >/dev/null
docker create --name "fwd-$NAME" --network "$NET" --network-alias fwd --add-host=host.docker.internal:host-gateway \
  "$NETIMG" sh -c "socat TCP-LISTEN:18080,fork,reuseaddr TCP:host.docker.internal:18080 & socat TCP-LISTEN:18082,fork,reuseaddr TCP:host.docker.internal:18082 & wait" >/dev/null
docker network connect bridge "fwd-$NAME"
docker start "fwd-$NAME" >/dev/null

ENVS=(-e KIMI_CODE_HOME=/run/study/kimi-home -e HOME=/home/node -e FORCE_COLOR=0 -e NO_COLOR=1)
[ -n "${TELEMETRY_OFF:-}" ] && ENVS+=(-e KIMI_DISABLE_TELEMETRY=1 -e KIMI_CODE_NO_AUTO_UPDATE=1)
docker run -d --name "kimi-$NAME" --network "$NET" --network-alias kimi --user 1000:1000 -v "$RUN:/run/study" \
  "${ENVS[@]}" -w /run/study/ws "$IMG" sleep infinity >/dev/null
docker run -d --name "cap-$NAME" --network "container:kimi-$NAME" --cap-add NET_RAW -v "$RUN:/run/study" "$NETIMG" \
  tcpdump -i any -p -U -w /run/study/egress.pcap >/dev/null
sleep 1
{ echo "tz=$(docker exec kimi-$NAME date +%Z) lang=$(docker exec kimi-$NAME sh -c 'echo ${LANG:-unset}')"; } > "$RUN/locale.txt"
if [ -n "${STALL:-}" ]; then
  cp "$HERE/fixtures/stall.mjs" "$RUN/stall.mjs"
  docker exec -d "kimi-$NAME" node /run/study/stall.mjs
fi
[ -n "${PRE:-}" ] && docker exec "kimi-$NAME" sh -c "$PRE" > "$RUN/pre.out" 2>&1 || true
set +e
if [ -n "${NOPROMPT:-}" ]; then
  docker exec "kimi-$NAME" kimi ${KIMI_ARGS:-} > "$RUN/kimi.out" 2> "$RUN/kimi.err"
else
  docker exec "kimi-$NAME" kimi -m "${MODEL:-kc}" ${KIMI_ARGS:-} -p "$TASK" > "$RUN/kimi.out" 2> "$RUN/kimi.err"
fi
echo "exit=$?" | tee "$RUN/exit.txt"
set -e
[ -n "${POST:-}" ] && docker exec "kimi-$NAME" sh -c "$POST" > "$RUN/post.out" 2>&1 || true
sleep 2
docker stop -t 3 "cap-$NAME" >/dev/null 2>&1 || true
