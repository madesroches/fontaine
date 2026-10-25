# DeepSeek Harness study: experiments

Scripts, fixtures and captured excerpts behind `research/research-deepseek-harness.md` (issue #154).
Subject: `github.com/deepseek-ai/deepseek-harness` at commit `22ddee3792402c3e70ec5f86a63c3e3e895a2f60`
(package version 0.2.1-rc.3). Model: `deepseek-v4.1-flash` through the opencode Go gateway.

Layout:

- `scripts/` tooling (below), `fixtures/` the task repo, `captures/` the committed excerpts the doc cites.
- `runs/` is gitignored: full proxy captures, pcaps, dsh homes and session logs. Nothing in it is committed.

## Pinned inputs

- dsh checkout: the commit above (`git clone https://github.com/deepseek-ai/deepseek-harness && git checkout 22ddee37`).
- Image `dsh-study:22ddee37`, built from that checkout: `docker build -f scripts/dsh.Dockerfile -t dsh-study:22ddee37 <checkout>`.
  Base `node@sha256:6449e19d8184d84062525b6a7524def03146c2cb04a7f08b076680d0e8e4240a` (node:24-bookworm, Node 24.22.1).
  The Dockerfile installs `musl-tools` and runs `build:native` so the static `landlock-run` exists.
- Image `dsh-study-net`: `docker build -f scripts/net.Dockerfile -t dsh-study-net .`
  Base `debian@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8`; adds `socat` and `tcpdump`.
- Fontaine: `cargo build --release -p fontaine` from `rust/` (the study used the branch tip at the start of the work).

## Safety rules the tooling enforces

- The container only ever holds the dummy key `dsh-dummy-key`. The real `go` access token is read (never written, never
  refreshed) from Fontaine's `credentials.toml` by the host proxy, which injects it on the dsh listener.
- dsh runs on a `docker network --internal`; its only route out is a `socat` sidecar (`fwd`) to the host proxy. A `tcpdump`
  sidecar shares dsh's network namespace and records `runs/<run>/egress.pcap`.
- `scripts/scrub.py --check captures/` must report 0 hits before anything is committed.

## Reproducing

1. Start the proxy (stdlib only): `python3 -I scripts/record_proxy.py --runs "$PWD/runs"` (binds 172.17.0.1:18080 for dsh,
   :18081 for Fontaine; it writes into `runs/<name>/` where `<name>` is in `runs/CURRENT`, which the run scripts set).
2. dsh runs: `scripts/run_dsh.sh <name> "<task>"`, with `FIXTURE=repo-failing`, `PATCHES="e3.patch.yml ..."`, `STALL=1`,
   `TELEMETRY=1`, `DSH_ARGS=...`, `SESSION_ID=...`, `WEB=1` as needed. `scripts/dump_session.sh <name>` decompresses the session log.
3. Fontaine runs: `scripts/run_fontaine.sh <name> proxy.toml "<prompt>" [args]` (`proxy_messages.toml` for the Messages wire).
4. Experiments:

| Id | Command |
|---|---|
| E1 | `scripts/dump_config.sh`; `TELEMETRY=1 scripts/run_dsh.sh e1-bash "Run the shell command ..."`; `python3 -I scripts/pcap_endpoints.py runs/e1-bash/egress.pcap` |
| E2 | `FIXTURE=repo-failing scripts/run_dsh.sh e2-default "<read-only question>"` and again with `PATCHES=e2_nolog.patch.yml` |
| E3 | `scripts/e3_mechanisms.sh` (stall, promote, repeat, goal; the second-chance repeat and goal triggers are the last two lines) and `scripts/run_dsh.sh e3-fix ...` with `PATCHES=e3.patch.yml` |
| E4 | `scripts/e4_hostile.sh` (container only) |
| E5 | `PATCHES=e5b.patch.yml scripts/run_dsh.sh e5-compact2 ...` (`e5.patch.yml` is the first, too-high threshold) |
| E6 | headless todo and subagent runs; Web UI leg only booted (`WEB=1`) |
| E7 | copy a run's `dsh-home` and `ws`, then `SESSION_ID=<id> PATCHES="e3.patch.yml /dsh/packages/experimental/session-search/cordis.patch.yml" scripts/run_dsh.sh e7-resume ...` |
| E8 | `scripts/run_fontaine.sh`, `scripts/probe_chat.py` (hand-built Chat and Messages probes), `scripts/check_roundtrip.py` |
| E9 | `scripts/run_claude_baseline.sh <name> "<task>"` (manual only) |

`scripts/make_captures.py` writes `captures/` from `runs/`. Every capture in the doc comes from it or from `probe_chat.py`.
`probe_chat.py` needs a non-default `user-agent`: the gateway's edge answers urllib's default with a 403 (error 1010).
