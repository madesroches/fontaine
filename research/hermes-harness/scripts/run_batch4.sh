#!/usr/bin/env bash
# Batch 4 for issue #253: the model-free watchdog path — the design Hermes
# actually intends for cheap scheduled monitoring.
#   A) --no-agent job: script stdout IS the job; empty stdout = silent.
#   B) --monitor-script job: exact-bytes hash suppresses the agent run when
#      the check output is unchanged; a change injects the diff into the
#      agent prompt (the only step here that calls the model).
# Manual fire path (no standing gateway): `cron run <name>` marks the job
# due, `cron tick` executes due jobs once and exits.
# Run DETACHED, exactly once, AFTER batch 3 finishes:
#   setsid nohup scripts/run_batch4.sh > runs/batch4.log 2>&1 &
set -u
cd "$(dirname "$0")/.."

MODEL="qwen3:4b-64k"
STATE=runs/monitor-state/monitor_state.json
RUN=e5-watchdog
HOME_DIR="runs/$RUN/home"

GRAFANA_URL="http://monitor:8080/api/v1/alerts"
TEAMCITY_URL="http://monitor:8080/app/rest/builds"

mode() { echo "{\"mode\": \"$1\"}" > "$STATE"; echo "--- monitor mode set to: $1"; }

run() { # run <timeout-secs> <hermes args...>
  local to="$1"; shift
  docker rm -f "hermes-$RUN" >/dev/null 2>&1
  echo
  echo "===== RUN $RUN: hermes $* ====="
  timeout "$to" scripts/run_hermes.sh "$RUN" "$@" 2>&1
  echo "===== END $RUN (exit $?) ====="
  docker rm -f "hermes-$RUN" >/dev/null 2>&1
}

scripts/study_up.sh captures/batch4

# --- seed home: config + the two watchdog scripts
mkdir -p "$HOME_DIR/scripts"
cat > "$HOME_DIR/config.yaml" <<YAML
model:
  provider: "custom"
  base_url: "http://host.docker.internal:18090/v1"
  default: "$MODEL"
YAML

# A) no-agent watchdog: prints only when something is wrong (empty = silent)
cat > "$HOME_DIR/scripts/watchdog.sh" <<'SH'
#!/usr/bin/env bash
set -u
alerts=$(curl -s -H 'Authorization: Bearer glsa_study_readonly_fake0001' http://monitor:8080/api/v1/alerts)
builds=$(curl -s -u watcher:tc-study-fake-pass http://monitor:8080/app/rest/builds)
python3 - "$alerts" "$builds" <<'PY'
import json, sys
alerts = json.loads(sys.argv[1]).get("data", [])
builds = json.loads(sys.argv[2]).get("build", [])
if alerts or builds:
    print(f"WATCHDOG: {len(alerts)} firing alert(s), {len(builds)} failed build(s)")
    for a in alerts:
        print(" alert:", a.get("labels", {}).get("alertname"), "-", a.get("annotations", {}).get("summary"))
    for b in builds:
        print(" build:", b.get("buildTypeId"), "#" + str(b.get("number")), b.get("status"))
PY
SH

# B) monitor-script: stable output (no timestamps), hash decides agent runs
cat > "$HOME_DIR/scripts/monitor_check.sh" <<'SH'
#!/usr/bin/env bash
curl -s -H 'Authorization: Bearer glsa_study_readonly_fake0001' http://monitor:8080/api/v1/alerts
curl -s -u watcher:tc-study-fake-pass http://monitor:8080/app/rest/builds
SH
chmod +x "$HOME_DIR/scripts/"*.sh

# --- create both jobs while green
mode green
run 120 cron create 5m --no-agent --script watchdog.sh --name tc-watchdog
run 120 cron create 5m --monitor-script monitor_check.sh --name grafana-teamcity-monitor \
    "A monitor change was detected. Summarize what changed in one line and say whether it needs a human." --reasoning-effort none
run 120 cron list

# --- A green: job fires, script finds nothing, delivery should be silent
run 120 cron run tc-watchdog
run 300 cron tick

# --- B green baseline: first tick establishes the hash
run 120 cron run grafana-teamcity-monitor
run 300 cron tick

# --- B green again: unchanged output must suppress the agent run entirely
run 120 cron run grafana-teamcity-monitor
run 300 cron tick

# --- firing: A should print the failure summary; B should trigger the agent
mode firing
run 120 cron run tc-watchdog
run 300 cron tick
run 120 cron run grafana-teamcity-monitor
run 420 cron tick

# --- history + observability
run 120 cron runs
run 120 cron list
run 120 cron status
mode green

echo
echo "===== BATCH 4 DONE ====="
