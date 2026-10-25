#!/usr/bin/env bash
# Run dsh once inside Docker with the study's network and capture setup.
# usage: run_dsh.sh NAME "task text" [ENV=...]
#   env: FIXTURE=<dir under fixtures/> PATCHES="e3.patch.yml ..." (under scripts/, or abs path inside image)
#        DSH_ARGS="extra dsh args" PROFILE=headless TELEMETRY=1 STALL=1 WEB=1 SESSION_ID=<id> POST=<cmd run in container after>
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
NAME=$1; TASK=${2:-}
RUN=$HERE/runs/$NAME
PROFILE=${PROFILE:-headless}
IMG=dsh-study:22ddee37; NETIMG=dsh-study-net
NET=dsh-study-$NAME
BR=172.17.0.1; PORT=18080
mkdir -p "$RUN/dsh-home" "$RUN/ws"
if [ -n "${FIXTURE:-}" ]; then cp -a "$HERE/fixtures/$FIXTURE/." "$RUN/ws/"; fi
cp "$HERE/scripts/"*.patch.yml "$RUN/" 2>/dev/null || true
chmod -R a+rwX "$RUN"

# proxy must already be running: python3 -I record_proxy.py --runs runs  (it follows runs/CURRENT)
echo "$NAME" > "$HERE/runs/CURRENT"
python3 - <<PY || { echo "proxy not reachable on $BR:$PORT" >&2; exit 2; }
import socket; socket.create_connection(("$BR",$PORT),timeout=2)
PY
# token freshness: report remaining lifetime without printing the token
python3 -I - <<'PY'
import tomllib,time,os
d=tomllib.load(open(os.path.expanduser("~/.local/share/fontaine/credentials.toml"),"rb"))
left=d["accounts"]["go"]["oauth"]["expires_at"]-time.time()
print(f"go access token remaining: {left/3600:.1f} h")
if left < 1800:
    raise SystemExit("token has < 30 min left: run a plain `fontaine -p 'Reply with OK.' --model go/deepseek-v4.1-flash` (not via the proxy), then rerun")
PY

cleanup() {
  docker rm -f "dsh-$NAME" "cap-$NAME" "fwd-$NAME" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup
docker network create --internal "$NET" >/dev/null
FWD_PUB=()
if [ -n "${WEB:-}" ]; then FWD_PUB=(-p 127.0.0.1:3080:3080); fi
docker create --name "fwd-$NAME" --network "$NET" --network-alias fwd --add-host=host.docker.internal:host-gateway \
  "${FWD_PUB[@]}" "$NETIMG" sh -c "socat TCP-LISTEN:$PORT,fork,reuseaddr TCP:host.docker.internal:$PORT & ${WEB:+socat TCP-LISTEN:3080,fork,reuseaddr TCP:dsh:3080 &} wait" >/dev/null
docker network connect bridge "fwd-$NAME"
docker start "fwd-$NAME" >/dev/null

ENVS=(-e DSH_HOME=/run/study/dsh-home -e HOME=/home/node -e DEEPSEEK_API_KEY=dsh-dummy-key
      -e DEEPSEEK_BASE_URL=http://fwd:$PORT/anthropic -e DEEPSEEK_SEARCH_BASE_URL=http://fwd:$PORT/anthropic/v1
      -e FORCE_COLOR=0 -e TSX_TSCONFIG_PATH=/dsh/tsconfig.base.json)
[ -z "${TELEMETRY:-}" ] && ENVS+=(-e DSH_TELEMETRY_DISABLED=1)
docker run -d --name "dsh-$NAME" --network "$NET" --network-alias dsh --user 1000:1000 -v "$RUN:/run/study" \
  "${ENVS[@]}" -w /run/study/ws "$IMG" sleep infinity >/dev/null
docker run -d --name "cap-$NAME" --network "container:dsh-$NAME" --cap-add NET_RAW -v "$RUN:/run/study" "$NETIMG" \
  tcpdump -i any -p -U -w /run/study/egress.pcap >/dev/null
sleep 1
if [ -n "${STALL:-}" ]; then
  cp "$HERE/fixtures/stall.mjs" "$RUN/stall.mjs"
  docker exec -d "dsh-$NAME" node /run/study/stall.mjs
fi

PATCH_ARGS=(--patch /run/study/model.patch.yml)
for p in ${PATCHES:-}; do case "$p" in /*) PATCH_ARGS+=(--patch "$p");; *) PATCH_ARGS+=(--patch "/run/study/${p#scripts/}");; esac; done
DSH=(node --import /dsh/node_modules/tsx/dist/esm/index.mjs /dsh/apps/cli/src/bin.ts)
SID=(); [ -n "${SESSION_ID:-}" ] && SID=(--session-id "$SESSION_ID")
set +e
if [ -n "${WEB:-}" ]; then
  docker exec "dsh-$NAME" sh -c 'exec "$@" --host "$(hostname -i | awk "{print \$1}")" --no-open' sh "${DSH[@]}" --profile web "${PATCH_ARGS[@]}" ${DSH_ARGS:-} > "$RUN/dsh.out" 2> "$RUN/dsh.err" &
  echo "web UI launched; see $RUN/dsh.out; waiting for ${WEB_SECONDS:-600}s or touch $RUN/stop"
  for i in $(seq 1 "${WEB_SECONDS:-600}"); do [ -f "$RUN/stop" ] && break; sleep 1; done
else
  docker exec "dsh-$NAME" "${DSH[@]}" --profile "$PROFILE" "${PATCH_ARGS[@]}" ${DSH_ARGS:-} "${SID[@]}" "$TASK" > "$RUN/dsh.out" 2> "$RUN/dsh.err"
  echo "exit=$?" | tee "$RUN/exit.txt"
fi
set -e
[ -n "${POST:-}" ] && docker exec "dsh-$NAME" sh -c "$POST" > "$RUN/post.out" 2>&1 || true
sleep 2
docker stop -t 3 "cap-$NAME" >/dev/null 2>&1 || true
