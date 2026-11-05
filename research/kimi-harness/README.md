# Kimi Code CLI study: experiments

Scripts, fixtures and captured excerpts behind `research/research-kimi-harness.md` (issue #156).
Subject: `github.com/MoonshotAI/kimi-code`, source read at commit `df90d90ec0714db23dcc830acd2db6232f5c7426`
(package `@moonshot-ai/kimi-code` 2.5.1). The harness itself ran from the published npm package 2.5.1, not from a source
build: the pinned commit's `apps/kimi-code/package.json` carries the same version, but the npm build was not diffed against it.
Models: `kimi-for-coding` through the Kimi coding endpoint (`https://api.kimi.ai/coding/v1`); the opencode Go gateway's `kimi-k3`
was reached but its monthly usage limit was exhausted (every request answered 429), so no Go run produced a model reply.

Layout:

- `scripts/` tooling (below), `fixtures/` the task repos, `captures/` the committed excerpts the doc cites.
- `runs/` is gitignored: full proxy captures, pcaps, kimi homes and session logs. Nothing in it is committed.

## Pinned inputs

- Source: `git clone https://github.com/MoonshotAI/kimi-code && git checkout df90d90ec0714db23dcc830acd2db6232f5c7426` (read only, outside this repo).
- Image `kimi-study:2.5.1`: `docker build -f scripts/kimi.Dockerfile -t kimi-study:2.5.1 scripts`.
  Base `node@sha256:994ed664b2644b1b186516daaa1fb7aa93a1e717da2a0ff17094c765d5dbfde1` (node:24-bookworm, Node 24.22.2);
  adds git, python3 and ripgrep, then `npm install -g @moonshot-ai/kimi-code@2.5.1`.
- Image `kimi-study-net`: `docker build -f scripts/net.Dockerfile -t kimi-study-net scripts`.
  Base `debian@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8`; adds `socat` and `tcpdump`.
- Fontaine: `cargo build --release -p fontaine` from `rust/` (the study used the branch tip at the start of the work).

## Safety rules the tooling enforces

- The container only ever holds the dummy key `kimi-dummy-key` (in the throwaway `config.toml`). The real credentials stay on
  the host: the `kimi` account's `api_key` and the `madesroches` OAuth access token are read (never written, never refreshed) from
  Fontaine's `credentials.toml` by `record_proxy.py`, which injects them only when the incoming `Authorization` is the dummy key.
  The `madesroches` instance answers 503 near token expiry; `run_kimi.sh` refuses to start with under 30 minutes left.
- kimi runs on a `docker network --internal`; its only route out is a `socat` sidecar (`fwd`) to the two host proxies. A `tcpdump`
  sidecar shares kimi's network namespace and records `runs/<run>/egress.pcap`, which is how telemetry and update checks are seen
  (they fail inside the container, which is the point: the attempted destinations are logged).
- `KIMI_CODE_HOME` is `runs/<run>/kimi-home`; the only host mount is `runs/<run>/`. No `--privileged`, no relaxed seccomp.
- Hostile commands (`e4_hostile.sh`) run only inside the container, against decoys created in the run directory.
- `scripts/scrub.py --check captures/` must report 0 hits, and `rg 'sk-[A-Za-z0-9_-]{20,}' captures/` must find nothing, before commit.
  Neither prints a matched value. `make_captures.py` scrubs literal device ids as its last step.

## Reproducing

1. Start the proxies (stdlib only): `scripts/start_proxies.sh`. Instance 1 forwards to the Kimi coding endpoint (harness port
   18080, Fontaine port 18081, key from `[accounts.kimi].api_key`); instance 2 to `https://opencode.ai/inference/go` (18082 and
   18083, token from `[accounts.madesroches.oauth]`). Each writes `runs/<name>/exchanges-<tag>.jsonl`, `<name>` being in
   `runs/CURRENT`, which the run scripts set. A client that sends no `User-Agent` stays UA-less through the proxy (the edge answers
   urllib's default with 403, error 1010).
2. kimi runs: `scripts/run_kimi.sh <name> "<task>"` with `FIXTURE=repo-failing|empty`, `MODEL=kc|k3|go-k3|kc-small`,
   `KIMI_ARGS="-S <session>"`, `EXTRA_CONFIG=<file>`, `TELEMETRY_OFF=1`, `PRE=<cmd>`. The Go provider needs the
   `x-opencode-session` header, set through `custom_headers` in `config.template.toml`; without it the gateway answers 400
   `MissingSessionID` (`captures/e2_go_nosession_400.json`).
3. Fontaine runs: `scripts/run_fontaine.sh <name> proxy.toml "<prompt>" [args]` (`proxy_messages.toml` forces the Messages wire for
   the Go Kimi model); `MODEL=madesroches/kimi-k3` for the Go leg.
4. Experiments:

| Id | Command |
|---|---|
| E1 | `scripts/run_kimi.sh e1-boot "Run the shell command: uname -s; id -u; echo \$HOME ..."`, then again with `TELEMETRY_OFF=1` as `e1-off`; `python3 -I scripts/pcap_endpoints.py runs/e1-boot/egress.pcap`; models endpoints via `curl -H 'Authorization: Bearer kimi-dummy-key' http://172.17.0.1:18080/models` |
| E2 | `FIXTURE=repo-failing scripts/run_kimi.sh e2-kc "<read-only question>"`, then `MODEL=go-k3` as `e2-go` |
| E3 | `e3-fix` (fix the failing test), `e3-repeat` and `e3-repeat2` (repeated identical command), `e3-hung` (`sleep 75` in the foreground) |
| E4 | `scripts/e4_hostile.sh` (container only) |
| E5 | `EXTRA_CONFIG=scripts/e5.extra.toml MODEL=kc-small scripts/run_kimi.sh e5-compact ...` (24k-token window alias) |
| E6 | `e6-goal` (`/goal ...` through `-p`) and `e6-todo` (TodoList plus the explore subagent) |
| E7 | `cp -a runs/e3-fix runs/e7-resume`, then `KIMI_ARGS="-S <id>" scripts/run_kimi.sh e7-resume ...`; `e7-repair` appends a torn line to `wire.jsonl` first |
| E8 | `python3 -I scripts/probe_kimi.py e8-probe` (thinking, effort, sampling and token-limit variants) |
| E9 | `scripts/run_fontaine.sh e9-e2 proxy.toml ...`, `e9-e3`, `e9-effort` (`--effort low`), `e9-resume` and `e9-resume-same` (`--continue` in the `e9-e3` workspace), `e9-go-chat` / `e9-go-messages`; `probe_history.py`, `probe_dynamic_tools.py` |
| E10 | `scripts/run_claude_baseline.sh <name> "<task>"` (manual only) |

`scripts/make_captures.py` writes `captures/` from `runs/`; `summarize_run.py` prints a per-request view of any run.
