#!/usr/bin/env bash
# Run omp once inside Docker with the study's network and capture setup.
# usage: run_omp.sh NAME "task text"
#   env: FIXTURE=<dir under fixtures/>  CONFIG=<file under scripts/ copied to ~/.omp/agent/config.yml>
#        OMP_ARGS="extra omp args"  TELEMETRY=1 (leave telemetry at defaults)  IDLE=1 (start container, run no omp; use POST)
#        POST=<cmd run in container after>  PRE=<cmd run in container before>  KEEP=1 (do not tear down)  GIT=1 (git init the workspace)
#        XENV="-e A=b -e C=d" (extra env for the omp process)
#        TIMEOUT=<seconds> (default 900)  CMD=<full command run instead of omp -p>
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
NAME=$1; TASK=${2:-}
RUN=$HERE/runs/$NAME
IMG=${IMG:-omp-study:dcafcef6}; NETIMG=omp-study-net
NET=omp-study-$NAME
BR=172.17.0.1; PORT=18090
[ -z "${RESUME:-}" ] && rm -rf "$RUN"
mkdir -p "$RUN/home/.omp/agent" "$RUN/ws"
if [ -n "${FIXTURE:-}" ]; then cp -a "$HERE/fixtures/$FIXTURE/." "$RUN/ws/"; [ -d "$RUN/ws/dot-claude" ] && mv "$RUN/ws/dot-claude" "$RUN/ws/.claude"; fi
cp "$HERE/scripts/models.yml" "$RUN/home/.omp/agent/models.yml"
[ -n "${CONFIG:-}" ] && cp "$HERE/scripts/$CONFIG" "$RUN/home/.omp/agent/config.yml"
if [ -n "${GIT:-}" ] && [ ! -d "$RUN/ws/.git" ]; then
  (cd "$RUN/ws" && git init -q && git add -A && git -c user.name=study -c user.email=study@example.invalid commit -qm init)
fi
cp "$HERE"/scripts/e*_*.sh "$RUN/" 2>/dev/null || true
chmod -R a+rwX "$RUN" 2>/dev/null || true
echo "$NAME" > "$HERE/runs/CURRENT"
python3 - <<PY || { echo "proxy not reachable on $BR:$PORT" >&2; exit 2; }
import socket; socket.create_connection(("$BR",$PORT),timeout=2)
PY
python3 -I - <<'PY'
import tomllib,time,os
d=tomllib.load(open(os.path.expanduser("~/.local/share/fontaine/credentials.toml"),"rb"))
left=d["accounts"]["go"]["oauth"]["expires_at"]-time.time()
print(f"go access token remaining: {left/3600:.1f} h")
if left < 1800: raise SystemExit("token nearly expired: run a plain fontaine -p first")
PY
cleanup() {
  [ -n "${KEEP:-}" ] && return
  docker rm -f "omp-$NAME" "cap-$NAME" "fwd-$NAME" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT
KEEP_SAVE=${KEEP:-}; KEEP=; cleanup; KEEP=$KEEP_SAVE
docker network create --internal "$NET" >/dev/null
docker create --name "fwd-$NAME" --network "$NET" --network-alias fwd --add-host=host.docker.internal:host-gateway \
  "$NETIMG" sh -c "socat TCP-LISTEN:$PORT,fork,reuseaddr TCP:host.docker.internal:$PORT & wait" >/dev/null
docker network connect bridge "fwd-$NAME"
docker start "fwd-$NAME" >/dev/null
ENVS=(-e HOME=/run/study/home -e OMP_STUDY_KEY=omp-dummy-key -e FORCE_COLOR=0 -e TERM=xterm-256color)
docker run -d --name "omp-$NAME" --network "$NET" --network-alias omp --user "$(id -u):$(id -g)" -v "$RUN:/run/study" \
  "${ENVS[@]}" -w /run/study/ws "$IMG" sleep infinity >/dev/null
docker run -d --name "cap-$NAME" --network "container:omp-$NAME" --cap-add NET_RAW -v "$RUN:/run/study" "$NETIMG" \
  tcpdump -i any -p -U -w /run/study/egress.pcap >/dev/null
sleep 1
[ -n "${PRE:-}" ] && docker exec "omp-$NAME" sh -c "$PRE" > "$RUN/pre.out" 2>&1 || true
set +e
if [ -z "${IDLE:-}" ]; then
  if [ -n "${CMD:-}" ]; then
    timeout "${TIMEOUT:-900}" docker exec ${XENV:-} "omp-$NAME" sh -c "$CMD" > "$RUN/omp.out" 2> "$RUN/omp.err"
  else
    timeout "${TIMEOUT:-900}" docker exec ${XENV:-} "omp-$NAME" omp -p --model go/deepseek-v4.1-flash --no-title ${OMP_ARGS:-} "$TASK" > "$RUN/omp.out" 2> "$RUN/omp.err"
  fi
  echo "exit=$?" | tee "$RUN/exit.txt"
fi
set -e
[ -n "${POST:-}" ] && docker exec "omp-$NAME" sh -c "$POST" > "$RUN/post.out" 2>&1 || true
sleep 2
docker stop -t 3 "cap-$NAME" >/dev/null 2>&1 || true
docker run --rm -v "$RUN:/run/study" "$NETIMG" chmod -R a+rwX /run/study >/dev/null 2>&1 || true
