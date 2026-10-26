# oh-my-pi harness study: experiments

Scripts, fixtures and captured excerpts behind `research/research-oh-my-pi-harness.md` (issue #213).
Subject: `github.com/can1357/oh-my-pi` (command `omp`) at commit `dcafcef6422c6a260649b879d75a5aabb1ea778e`
(package version 18.13.0). Model: `deepseek-v4.1-flash` through the opencode Go gateway's OpenAI-compatible Chat route.

Layout:

- `scripts/` tooling (below), `fixtures/` the task repos, `captures/` the committed excerpts the doc cites.
- `runs/` is gitignored: full proxy captures, pcaps, omp homes and session logs, the copied checkout. Nothing in it is committed.

## Pinned inputs

- omp checkout: `git clone https://github.com/can1357/oh-my-pi && git checkout dcafcef6`, copied (without `.git`) to
  `runs/omp-src` so the build never runs from the clone.
- Base image `debian@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8` (bookworm-slim), Bun 1.4.4 (the version the
  repo's own Dockerfile pins).
- Native addon. The last published prebuilt addon (`@oh-my-pi/pi-natives-linux-x64@18.12.6`) is stale for this checkout: omp
  stops with ``@oh-my-pi/pi-natives export `editErrorKinds` is missing``. The addon is therefore built from the checkout with
  the repo's own Dockerfile stage (nightly-2026-10-06, 12 min 17 s here, one 116 MB `pi_natives.linux-x64-modern.node`):
  `docker build -f runs/omp-src/Dockerfile --target natives-builder -t omp-natives-builder:dcafcef6 runs/omp-src`, then copy
  `/out/*.node` to `runs/natives-out/` and `runs/omp-src/natives-out/` (and delete the `**/*.node` line of
  `runs/omp-src/.dockerignore`, which otherwise keeps it out of the build context).
- Image `omp-study:dcafcef6`: `docker build -f scripts/omp.Dockerfile -t omp-study:dcafcef6 runs/omp-src`.
- Image `omp-study-net` (`socat`, `tcpdump`): `docker build -f scripts/net.Dockerfile -t omp-study-net scripts`.
- Image `omp-study-lsp:dcafcef6` (adds Debian's `python3-pylsp`): `docker build -f scripts/omp-lsp.Dockerfile -t omp-study-lsp:dcafcef6 scripts`.
- Fontaine: `cargo build --release -p fontaine` from `rust/` (the branch tip at the start of the work).
- Claude Code 2.1.310 for the one `claude -p` baseline.

## Safety rules the tooling enforces

- The container only ever holds the dummy key `omp-dummy-key`. The real `go` access token is read (never written, never refreshed)
  from Fontaine's `credentials.toml` by the host proxy, which injects it for the omp listener only when the incoming bearer is the dummy.
- omp runs on a `docker network --internal`; its only route out is a `socat` sidecar (`fwd`) to the host proxy on 172.17.0.1:18090.
  A `tcpdump` sidecar shares omp's network namespace and records `runs/<run>/egress.pcap`, so every DNS name omp tries is logged
  even though nothing but the proxy answers.
- Hostile-command experiments (E5, E9) run only inside the container. The planted secret is fake.
- `scripts/scrub.py --check captures/` must report 0 hits, and `rg 'Bearer [A-Za-z0-9._-]{20,}'` must find nothing, before commit.

## Reproducing

1. Start the proxy (stdlib only): `python3 -I scripts/record_proxy.py --runs "$PWD/runs"` (ports 18090 omp, 18091 Fontaine; it writes
   `runs/<name>/exchanges.jsonl` where `<name>` is in `runs/CURRENT`, which the run scripts set; run one experiment at a time).
2. omp runs: `scripts/run_omp.sh <name> "<task>"` with `FIXTURE=<dir>`, `GIT=1`, `CONFIG=<file>` (copied to
   `~/.omp/agent/config.yml`), `OMP_ARGS`, `XENV="-e A=b"`, `IMG`, `CMD`, `POST`, `RESUME=1`, `TIMEOUT`. `scripts/models.yml` points the
   `go` provider at the proxy and adds the `x-opencode-session` header the gateway requires.
3. Fontaine runs: `scripts/run_fontaine.sh <name> proxy.toml "<prompt>" [args]`.
4. `scripts/trace_run.py runs/<name> [--full] [--listener omp|fontaine]` prints one block per model request;
   `scripts/summarize_runs.py runs/<a> runs/<b>` prints request, tool-call and token counts.

| Id | Command |
|---|---|
| E1 | `FIXTURE=repo-failing GIT=1 CMD='sh /run/study/e1_idle.sh' scripts/run_omp.sh e1-idle ""`; `scripts/run_omp.sh e1-ok "Reply with the single word OK."`; `python3 -I scripts/pcap_endpoints.py runs/e1-idle/egress.pcap` |
| E2 | `scripts/run_omp.sh e2-readonly "<question>"` and `scripts/run_fontaine.sh e2-readonly-fontaine proxy.toml "<question>" --permission-mode manual` |
| E3 | `run_omp.sh e3-fix` (plain); `CONFIG=e3_loop.yml` for `e3-repeat`, `e3-repeat2`, `e3-hang`; `CONFIG=e3_repair.yml XENV="-e PI_STRICT_EDIT_MODE=1"` for `e3-repair`; `FIXTURE=ttsr` for `e3-ttsr`; `"/goal ..."` for `e3-goal-print` |
| E4 | `scripts/e4_edits.sh omp\|omp-default\|fontaine`; LSP: `IMG=omp-study-lsp:dcafcef6 FIXTURE=lsp-py CONFIG=e4_lsp.yml scripts/run_omp.sh e4-lsp4 "..."` |
| E5 | `scripts/e5_risky.sh`; `scripts/e5_patterns.sh`; `CONFIG=e5_interceptor.yml` run |
| E6 | `python3 scripts/gen_long_fixture.py fixtures/long`, then `FIXTURE=long GIT=1 CONFIG=e6_compact.yml scripts/run_omp.sh e6-compact "..."` |
| E7 | `e7-todo-task` (plain), `e7-child-mode` and `e7-child-mode2` (`CONFIG=e7_child.yml`), `e7-plan` (`OMP_ARGS=--plan-yolo`) |
| E8 | copy `runs/e3-fix/{home,ws}` to `runs/e8-resume`, then `RESUME=1 CMD='omp -p --continue ...; omp -p --fork <id> ...; omp --export ...' scripts/run_omp.sh e8-resume ""` |
| E9 | `FIXTURE=planted-project GIT=1 CONFIG=e9_user.yml POST='ls -l /tmp/probe_*' scripts/run_omp.sh e9-planted "..."` |
| E10 | `scripts/run_claude_baseline.sh e10-claude-planted planted-claude "<task>"` (manual only) |
| E11 | `FIXTURE=repo-failing GIT=1 CONFIG=e11_tui.yml CMD='sh /run/study/e11_tui.sh' scripts/run_omp.sh e11-omp-tui ""`; `scripts/e11_fontaine_tui.sh e11-fontaine-tui` |

Fixtures keep a `dot-claude/` directory that the run scripts rename to `.claude/` (a local git exclude can hide `.claude/` directories from commits).

`scripts/make_captures.py` writes `captures/` from `runs/`; run `scrub.py captures/` and `scrub.py --check captures/` after it.
