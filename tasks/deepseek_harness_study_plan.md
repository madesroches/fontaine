# DeepSeek Harness Study Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/154

## Overview

Study DeepSeek's own agent harness, DeepSeek Harness (`dsh`), and produce a list of desirable
features for Fontaine. The list is led by the features that let the model iterate on a solution,
which dsh is reportedly good at. The work covers how it frames prompts, tool calls and reasoning for DeepSeek
models, how it manages context, its safety model, tasks, session log and the feedback it gives the
model. It also lists what Fontaine should support for the DeepSeek models themselves. Running DeepSeek models as the main agent through Fontaine's rig provider path is in scope too. Its fit as a
guardian or screen is assessed from source and the thinking-off history only. This is a research spike. The deliverables are
`research/research-deepseek-harness.md`, the experiment scripts and captured data under
`research/deepseek-harness/`, GitHub issues for anything worth adopting, and dated `## Decisions`
lines only where a finding argues for departing from Claude Code. No product crate changes.

## Current State

**The subject.** `github.com/deepseek-ai/deepseek-harness`, TypeScript, MIT. It is a
"developer preview" that warns of breaking changes. The repo was last pushed 2026-10-23 and has no
release tags. The study pins commit `22ddee3792402c3e70ec5f86a63c3e3e895a2f60` (2026-10-23), and
every source citation in the doc is `path@22ddee37`. Known shape, from a first read:

- It is built on Cordis ("everything is a plugin"). The agent loop, tool registry, session log and
  model adapter are each a replaceable plugin, and a profile (`web`, `headless`, `sdk`,
  `sdk-minimal`, `acp`) is a stack of bundles plus YAML patches (`docs/architecture.md`).
- The core packages are `core/session` (an append-only `SessionEvent` log), `core/system-prompt`
  (prompt sections and tool schemas), `core/tools` (the guarded execution pipeline),
  `core/agent-loop` and `llm/llm`.
- The DeepSeek adapter (`packages/llm/llm-deepseek`) speaks only DeepSeek's
  **Anthropic-compatible Messages** endpoint (`https://api.deepseek.com/anthropic`). It
  "serializes reasoning history back when required" and maps cache-hit metrics. `llm-pi-ai` routes
  every other provider.
- `docs/deepseek-llm-api-wire-extensions.md` documents non-model-visible request additions:
  `x-deepseek-harness-user-id` (a stable anonymous UUID), `x-deepseek-harness-session-id`,
  `x-deepseek-harness-compact`, and `dsh_*` body fields such as `dsh_plugin_packages` and
  `dsh_session_log`. `dsh_session_log` is on by default (`enabled: true`, `maxBytes` 8 MiB,
  `packages/session/session-log-deepseek/src/index.ts:52-53`) and mounted in the base bundle
  (`packages/bundle/base/cordis.patch.yml:43-44`), so headless has it. It is sent incrementally on
  every Messages request to whatever base URL is configured (`index.ts:188-193`). It carries the
  full canonical session log: cwd, system-prompt snapshots, user and assistant content,
  reasoning, tool args and results, and compaction summaries. API keys are excluded.
  `dsh_plugin_packages` is also on by default (`cordis.patch.yml:85-86`). So whole session
  content reaches the opencode gateway in every experiment.
- Candidate iteration mechanisms, the first reading of what lets the model iterate on a
  solution: `packages/goal/*` (a durable per-session goal, plus `goal-round-driver`, mounted by
  default in base, `packages/bundle/base/cordis.patch.yml:326-327`, which continues in rounds;
  feeds #17); `packages/guard/repeat-tool-reminder` (reminds the model
  when it repeats the exact same tool call); `packages/guard/timeout-policy` (a hung call returns
  a timed-out error to the model, but only for tools that declare `timeoutMs`: `web_fetch` at 30 s
  and `web_search` at 60 s (`cordis.patch.yml:482-486`); shipped bash/read/write/edit "are never cut off",
  `packages/guard/timeout-policy/README.md:42`); `packages/shell/tool-bash` (keeps its own
  `BASH_TIMEOUT` and, with `promoteOnTimeout: true` (the default), moves a hung command to a background job and
  returns `[still running after <ms>ms; moved to background job <id>]`,
  `packages/shell/tool-bash/README.md:51,66,217`); `packages/lsp/*` (navigation only); and the shapes of tool errors and test output. Negative
  findings from source: `packages/feedback/*` is human ratings that never reach the model; there is
  no LSP diagnostics feedback after edits (LSP is not mounted in base or headless, ships no
  servers per `packages/lsp/README.md`, and ignores diagnostics notifications at
  `packages/lsp/lsp-stdio/src/connection.ts:248`).
- Packages map onto the issue's questions: `compaction/*` (basic, tool-result pruner, image
  offload), `guard/*` (repeat-tool-reminder, timeout-policy), `sandbox/*` (local, policy,
  Windows ACL), `plan/plan-mode`, `todo/tool-todo`, `goal/*`, `subagent`, `feedback/*`, `lsp`,
  `session`, `session-query`, `spill`, `context`. The docs include `tool-execution-pipeline.md`,
  `agent-lifecycle.md`, `session-format-status.md`, `persistence-catalog.md` and
  `docs/subsystems/*.md`.
- `SAFETY.md`: no security audit. Sandboxing and approvals "reduce risk but do not guarantee
  isolation". Run it in a disposable environment.

**Fontaine today.**

- DeepSeek reaches Fontaine only through the `opencode-go` account kind
  (`rust/fontaine-core/src/providers/opencode_go.rs`). `deepseek-v4.1-flash` is routed to the
  **Chat** wire (`wire()`, line 154), and the guardian turns thinking off with
  `{"thinking": {"type": "disabled"}}` (`thinking_off`, line 176; history in
  `tasks/completed/guardian_thinking_off_plan.md`). The local config has a `go` account and a
  `kimi` account, and no direct DeepSeek API key.
- DeepSeek's thinking-mode guide says that after a tool call, `reasoning_content` must be sent back
  in later turns, or the API returns 400. The rig-core 0.45.0 source answers the first half: Chat
  decode stores `reasoning_content` as a native reasoning item (`wire/chat.rs:1046,1343-1345`), and
  `assistant()` with `replay()` (`chat.rs:384-428`, `completion/history.rs:407-433`) sends it back
  under the same field. Fontaine never touches reasoning blocks, and `GO_DIALECT`
  (`opencode_go.rs:43`) sets no `reasoning_field`. The gap is a tool-call turn with no reasoning.
  rig adds an empty `reasoning_content: ""` to such turns (`chat.rs:177-199,470-474`) only for a
  deepseek.com base URL, a dialect quirk, or an opencode-go/moonshot/openrouter catalog row. None
  matches `go/deepseek-v4.1-flash`. Whether the gateway or DeepSeek returns 400 for that turn is
  untested. This is the most direct Fontaine risk the study can find.
- DeepSeek model support, seed candidates for the research doc's second list:
  1. Reasoning passback: with `tools` present, DeepSeek requires `reasoning_content` from all
     previous assistant turns, including turns without a tool call, or it returns 400
     (thinking-mode guide, https://api-docs.deepseek.com/guides/thinking_mode/). Fontaine always
     sends tools. Ties to the E8 empty-reasoning check.
  2. Reasoning effort `off|low|high|max`: Chat `reasoning_effort` (+ `thinking` in body), Messages
     `output_config.effort`, Responses `reasoning.effort`; dsh `reasoningEffort`
     (`packages/llm/llm-deepseek/README.md:45,58-59,103`). Feeds #53.
  3. KV-cache prefix stability: caching is automatic and needs a full match of a persisted prefix
     unit (https://api-docs.deepseek.com/guides/kv_cache). dsh's `systemPromptUpdate: in-history`
     appends a changed system prompt after the cached history. `toolUpdate: addition-only` sends
     `tool_addition`/`tool_removal` blocks with `defer_loading` under the
     `mid-conversation-tool-changes-2026-07-01` beta (`llm-deepseek/README.md:53,85,181`;
     `src/messages-api.ts`). Feeds #13 and #34.
  4. Cache accounting: `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`. Feeds #56.
  5. 1,048,576-token context window (`llm-deepseek/src/models.ts`) and compaction sizing. Ties to
     `autocompact_window` (#152).
  6. Sampling parameters ignored in thinking mode (`temperature`, presence/frequency penalties;
     `top_p` clamped per model), per the thinking-mode guide.
  7. Images via the Files API, sized to the vision token grid (14px patches, at most 1024
     tokens per image; `llm-deepseek/README.md:95-97`). Feeds #21.
  8. Malformed historical tool arguments replayed as `{}` (`llm-deepseek/README.md:173`).
- An account's `base_url` can be overridden (`providers/mod.rs:183`), so Fontaine's traffic can be
  routed through a recording proxy without code changes.
- Precedent: `research/research-llamafirewall.md` (format of a deep-dive) and the #98 spike
  (`research/analysis-typed-decision-screen.md` with `research/typed-decision-screen/scripts/`,
  for the layout of experiments and data). The master plan lists the research docs at
  `tasks/fontaine_harness_plan.md:53-60`, and `tasks/design/research-findings.md` has one
  subsection per piece of prior art.
- Related open issues that findings may feed rather than duplicate: #13 prompt caching, #53
  reasoning effort, #22 todo tools, #24 plan-mode exit, #10 session UX, #12 checkpoints/rewind,
  #17 /goal, #27 LSP, #59 ACP, #39 forked subagents, #149 and #97 (local guardian), #132
  (OpenRouter scoring).
- Tooling here: Docker is available. Node, npm and pnpm are not installed on the host. bwrap is
  installed.

## Design

### Method

There are two tracks: read the pinned source, and run the harness. The doc states every claim with
its evidence, which is a source citation, a captured request or log line, or a command and its
output. Negative results are recorded as findings ("X does not exist", "Y failed because Z").

### Environment and safety

- dsh runs **only in a Docker container** (`node:24-bookworm`, pinned by digest in the script;
  dsh needs Node >=24.9, see the root `package.json` engines and `docs/development.md:11`)
  with a throwaway workspace volume, a throwaway `DSH_HOME` at `runs/<run>/dsh-home/`, and no
  host mounts besides `research/deepseek-harness/runs/<run>/`, as `SAFETY.md` asks.
- **No real credential enters the container.** dsh gets `DEEPSEEK_API_KEY=dsh-dummy-key`.
  The `go` account has no static key: it is an OAuth sign-in with short-lived access tokens and
  rotating refresh tokens (`opencode_go.rs:61-105,227-249`). So the host proxy injects auth on
  dsh's listener. Per request it reads the `go` access token from Fontaine's `credentials.toml`
  (in the data dir, `FONTAINE_DATA_DIR` or the default; writes are atomic, so a plain read is
  safe). It replaces the `x-api-key` header only when its value is the dummy key. It never
  refreshes and never writes the file, so it cannot race Fontaine over a rotated refresh token.
  A fresh token comes from Fontaine itself: if less than 30 minutes remain, `run_dsh.sh` waits
  until the token is inside Fontaine's 5-minute refresh margin (`token_source.rs:19`), then
  runs `fontaine -p "Reply with OK." --model go/deepseek-v4.1-flash` directly (not through the
  proxy), which refreshes and stores the new pair. On mid-run expiry the proxy does not forward;
  it answers 503 with a JSON error and logs it. That run is marked invalid and repeated after a
  refresh. E1 records the token lifetime. E4's hostile commands (`/proc/<pid>/environ`,
  `~/.ssh`) can therefore find only the dummy key.
- **Build and run are split.** `scripts/dsh.Dockerfile` builds image `dsh-study:22ddee37` from
  the pinned checkout as build context, with Docker's normal build network (Corepack, `pnpm
  install` and its postinstall, and the native build need the internet). Runs use that image
  with no internet. A second image, `dsh-study-net` (`debian:bookworm-slim` by digest, plus
  `socat` and `tcpdump`), serves two sidecars.
- **Run network.** dsh runs on a user-defined `docker network create --internal dsh-study`.
  It has no default route, and an internal network cannot reach `host-gateway`. So a `fwd`
  sidecar joins both `dsh-study` and the default bridge, with
  `--add-host=host.docker.internal:host-gateway`. It runs `socat` from `fwd:<port>` to the
  host proxy's dsh listener, and nothing else. dsh's base URLs name `http://fwd:<port>`.
  `HTTP(S)_PROXY` and `ALL_PROXY` are left unset. The host firewall is not changed.
- **Egress log.** A `cap` sidecar shares dsh's network namespace
  (`--network container:<dsh>`, `--cap-add NET_RAW`; the dsh container gets no extra caps). It
  runs `tcpdump -i any -p -w runs/<run>/egress.pcap` for the whole run. It sees every DNS query
  to Docker's resolver and every packet sent. A connect to a literal public IP fails with no
  route and sends no packet, so it shows only in dsh's own error output. E1 lists the
  endpoints from the pcap and those errors.
- **dsh's sandbox.** Base mounts `sandbox-local` in `workspace-write`
  (`packages/bundle/base/cordis.patch.yml:240-253`). On Linux it tries bwrap, then Landlock,
  and otherwise fails closed (`packages/sandbox/sandbox-local/README.md:12,57,73`). The image
  installs `musl-tools` and runs `pnpm --dir native/system run build:native` after
  `build:artifacts`, which builds the static `landlock-run` (`native/system/scripts/build.ts`);
  `build:artifacts` alone builds only the flock addon. bwrap is not installed in the image, so
  Landlock is the only candidate. The host kernel has Landlock in its LSM list, and Docker's
  default seccomp profile allows the Landlock syscalls. No seccomp, user-namespace or
  `--privileged` change is made. If the Landlock probe still fails, that is recorded, and bash
  legs run and record `SANDBOX_UNAVAILABLE`.
- It is built from the pinned checkout, not with `npx @deepseek-ai/dsh@latest`, so the binary
  matches the cited source. pnpm comes from Corepack (`docs/development.md:13`), so the
  Dockerfile runs `corepack enable` before `pnpm install && pnpm run build:artifacts` (skips
  typecheck and desktop; `docs/development.md:96`). If the build fails, fall back to
  `@deepseek-ai/dsh@0.2.1-rc.3`, whose exact commit is unknown (the repo has no tags), and
  record the mismatch.
- The `headless` profile is the driver (`pnpm dsh --profile headless "<task>"`,
  `docs/development.md:173-178`), so experiments are scripted and repeatable. The Web UI
  is opened once, to check that headless behaviour matches the default surface, and for E6's
  plan-mode leg. Otherwise it is dropped. The web listener defaults to 127.0.0.1:3080 and rejects
  wildcard binds (`packages/bundle/web-app/cordis.patch.yml:175-176`), so `run_dsh.sh` has a Web UI
  launch that binds to the container IP and passes `--trusted-host` if needed. An internal
  network publishes no ports, so `fwd` also forwards its port 3080 to dsh's and publishes it on
  `127.0.0.1:3080` only. The script opens the tokenized URL (`packages/bundle/web-app/README.md:16,65`). The web profile runs
  behind presets with the credentials store on, unlike headless (`web-app/README.md:101`).

### Wire capture

`research/deepseek-harness/scripts/record_proxy.py` is a small stdlib-only (`http.server` plus
`urllib`) reverse proxy. It forwards to a configured upstream, streams SSE through unchanged, and
writes each exchange as JSONL (request headers, request body, response status, and the response
chunks joined into one body). Before writing, it **redacts** an exact list of names:
`authorization`, `x-api-key`, `api_key`, `apiKey`. It also replaces literal secret values
everywhere in what it writes, so gitignored `runs/` holds no raw token either: the dummy key,
every access token it injected (kept in memory), the current `go` access and refresh tokens from
`credentials.toml`, and the anonymous user id from `runs/<run>/dsh-home/.anonymous-user-id`
(`packages/identity/anonymous-user-id/src/index.ts`), which is also sent as
`x-deepseek-harness-user-id`. It has two listeners, both bound to the docker bridge address.
The dsh listener injects auth as described under Environment and safety. The Fontaine listener
passes Fontaine's own `authorization` through unchanged. It does not match substrings, which would wipe the usage and cache fields the study measures
(`max_tokens`, `input_tokens`, `cache_read_input_tokens`, `prompt_cache_hit_tokens`). Both
harnesses point their base URL at it. Its upstream is `https://opencode.ai/inference/go`, with
the request path passed through unchanged:

- dsh: `DEEPSEEK_BASE_URL=http://fwd:<port>/anthropic`. dsh appends
  `/v1/messages` to its base (`packages/llm/llm-deepseek/src/messages-api.ts:14-17`,
  `adapter.ts:138`). Its default model is `deepseek-flash`
  (`packages/bundle/base/cordis.patch.yml:88-94`, `llm-deepseek/src/models.ts:10`), not the
  gateway id, so a dsh patch replaces the `models` catalog with only `deepseek-v4.1-flash` and sets
  `agent-default-model` to it. Subagent spawn accepts a model argument
  (`packages/subagent/subagent/src/child-agent.ts:87-97`), so a stock `deepseek-v4-pro` entry
  would be selectable and unserved by the gateway. Per-model behaviour is keyed per catalog entry
  (`model-info.ts:66-80`, `serialize.ts:64`), so the patch copies every field of the
  `deepseek-flash` entry (`llm-deepseek/src/models.ts:9-16`: `contextWindow` 1_048_576, text+image
  input, `systemPromptUpdate` `in-history`, `toolUpdate` `addition-only`) onto the new id. dsh applies
  patches in layers: bundles, the profile's `cordis.patch.yml`, `$DSH_HOME/cordis.patch.yml`, then
  `--patch` (`apps/cli/README.md:41-44`). `run_dsh.sh` passes `--patch <file>` from
  `research/deepseek-harness/scripts/`, with per-experiment overlays (E3, E5, E7), and E1's
  `--dump-config` confirms they land. dsh keys reasoning replay on the requested model id
  (`replay.ts:25-30,54,63`); the capture records whether the thinking signature survives across
  turns on the gateway. dsh may send the beta `mid-conversation-tool-changes-2026-07-01` blocks
  (`messages-api.ts:7`); a gateway rejection of the beta header or blocks is recorded as a
  finding. If the Go gateway does not serve DeepSeek on its Messages path, fall back to an
  `llm-pi-ai` OpenAI-compatible route to the Go Chat path, and record that the native adapter
  was not exercised.
- Fontaine: `fontaine -p --settings <proxy.toml> --model go/deepseek-v4.1-flash`. The settings
  layer redefines `[accounts.go]` with `base_url` set to the proxy
  (`rust/fontaine/src/cli.rs:9-11,44`; `rust/fontaine-core/src/config.rs:667-676,1220-1226`). The
  layer replaces a named account whole (`config.rs:1213,1220-1226`), so `proxy.toml` repeats
  `kind = "opencode-go"` (plus any `wires`/`models` the real account sets) beside `base_url`. The
  credential is still found by account name, so the same model runs on Fontaine's normal Chat path.
  Token refresh does not go through the proxy. The proxy forwards `{base}/openai/v1`
  (`opencode_go.rs:126-140`) to the same upstream. `--settings` takes one path, no CLI flag sets
  `wires`, and `wire()` checks overrides first, so one file cannot serve both wires. There are
  two: `proxy.toml` sets no `wires`, so the model takes its default Chat route; E8 uses it.
  `proxy_messages.toml` is the same plus `wires = { "deepseek-v4.1-flash" = "messages" }`
  under `[accounts.go]` (`Account.wires`, `rust/fontaine-core/src/config.rs:317-318`;
  `opencode_go.rs:84-86,154-157`), for the Messages probe and the Messages-wire comparison.
  The Messages client goes to `{base}/anthropic` (`opencode_go.rs:135`), which the
  path-passthrough proxy forwards.
  Guardian exchanges go through the proxy too. The validated guardian table is empty
  (`rust/fontaine-guardian/data/guardian_models.toml`) and the config sets no `[guardian] model`,
  so the guardian falls back to the main model (`rust/fontaine-core/src/models.rs`, MainFallback)
  on the same account. Its requests carry `thinking: disabled` and the guardian's system prompt,
  which tells them apart from main-agent requests. They, and scanner requests if the scanner is enabled, are filtered out before the E8
  round-trip check and the framing comparison, and may be kept separately as the guardian's own
  wire record.

The model-URL proxy cannot show what else dsh sends. The base bundle
mounts `session-telemetry-otel`, exporting to `https://dsh-otel-collector.deepseeksvc.com/v1/logs`
(`cordis.patch.yml:205-231`; default mode `FEEDBACK_ONLY`; `DSH_TELEMETRY_OTLP_URL` and
`DSH_TELEMETRY_MODE` are read at `cordis.patch.yml:221,225`, and `DSH_TELEMETRY_DISABLED` at
`packages/boot/app-boot/src/profile-context.ts:52-54`). Web search defaults to
`https://api.deepseek.com/anthropic/v1` (`packages/web/web-search-deepseek/src/provider.ts:34`;
env override at `provider.ts:31`) and uses its own model `deepseek-v4-flash`
(`provider.ts:37`), which needs the same model override as the main agent. `web-deepseek-api`
defaults to `https://api.deepseek.com/v1/tools` (`packages/web/web-deepseek-api/src/search.ts:16`),
has no env override, and is unselected by default (`fetchProvider: http`,
`cordis.patch.yml:466-467`). Both send `DEEPSEEK_API_KEY` (`cordis.patch.yml:463-477`). `web_fetch`
can reach any public URL (`cordis.patch.yml:479-486`), and the session-title model request is
another endpoint (`cordis.patch.yml:67-77`). So the container's egress is blocked and logged:
its only route out is `fwd` to the host proxy, and the `cap` sidecar records every DNS query and
packet (Environment and safety). Telemetry is turned off with
`DSH_TELEMETRY_DISABLED=1`. `DEEPSEEK_SEARCH_BASE_URL` replaces the whole base including `/v1`, so
it points at the proxy and ends in `/anthropic/v1`. No credential reaches `api.deepseek.com`.

Before any capture is committed, a scrubber pass
(`scripts/scrub.py`) removes keys and the anonymous user id. It takes its literal values from the
same sources as the proxy: the dummy key, the current `go` access and refresh tokens in
`credentials.toml`, and every `runs/*/dsh-home/.anonymous-user-id`. It never prints them. Bulky full captures stay gitignored
under `runs/`. The specific excerpts the doc cites are committed under
`research/deepseek-harness/captures/`.

### Experiments

Each experiment has a script under `scripts/`, a task fixture under `fixtures/` where it needs one (a tiny repo),
and a results section in the doc.
| # | Experiment | Answers |
|---|---|---|
| E1 | Build, boot `headless`, run `--dump-config`, and record the default plugin tree and the container's egress endpoints (from `egress.pcap` and dsh's errors). Run one bash call and record the sandbox runner `sandbox-local` selected and its reported `enforcement` (`full` or `partial`). Record the `go` access-token lifetime. | architecture; which defaults are on (telemetry, `dsh_session_log`); every endpoint dsh contacts; the sandbox runner in effect |
| E2 | One read-only question with the wire captured, once with default `session-log-deepseek` and once with `enabled: false` | system-prompt sections, tool schemas, tool-call and result format, headers, `dsh_*` fields; what `dsh_session_log` puts on the wire. The capture also checks DeepSeek model-support items 1-4 where the gateway allows: reasoning passback on every assistant turn, the effort field sent, and whether a system-prompt or tool change keeps the earlier prefix hit, as reported by cache-hit tokens |
| E3 | Fix a failing test in a fixture repo (edit → run tests → iterate). Each mechanism gets a trigger: a `web_fetch` against `http://127.0.0.1:<port>/` served inside the container by `fixtures/stall.mjs`, which `run_dsh.sh` starts before dsh (`web-fetch-http` does not restrict loopback, `packages/web/web-fetch-http/README.md:12`) (timeout-policy; the E3 patch sets `tool-web` `fetchTimeoutMs` well below 30 s, e.g. 5000, so timeout-policy's result fires before web-fetch-http's own 30 s deadline); a test command that hangs past bash-local's 120 s foreground default (cap 600 s, `packages/shell/bash-local/src/index.ts:102-103`), or a task that passes a smaller `timeoutMs` (bash promote-to-background, recorded as its own mechanism); a check that keeps failing identically, to invite 3 identical consecutive calls (repeat-tool-reminder, `cordis.patch.yml:444-448`); task wording that asks the model to call `create_goal` (goal; headless sends a plain user message and has no `/goal`), plus `goal-round-driver`, which the base bundle mounts by default (`packages/bundle/base/cordis.patch.yml:326-327`). Authority is settled by source: headless sends the task as a `source.kind: 'user'` message to the root agent (`packages/bundle/headless/src/index.ts:420-423`), which `requireDirectHuman` accepts (`packages/goal/tool-goal/src/authority.ts:79-82,99-101`), so `create_goal` is authorized. Source says headless exits before a round fires: it exits at the first idle with no children (`headless/src/index.ts:425-443`), while `goal-round-driver` awaits `sessions.flush` and `waitForChildren` before its follow-up (`packages/goal/goal-round-driver/src/index.ts:150-153,179,208`), and unload cancels the round. E3 confirms this by run; goal rounds themselves are observed in the Web UI, alongside E6's Web UI leg. A mechanism that does not fire is recorded as "did not fire" with the trigger used, distinct from "source-only". | the iteration loop: tool errors, the test-output feedback, repeat-tool-reminder, timeout-policy, bash promote-to-background, goal rounds, multi-turn reasoning round-trip |
| E4 | Risky actions in the fixture (write outside the workspace, `rm -rf` of a parent, `curl … \| sh`, reading `~/.ssh` in the container), run under the default and under `--approval-policy never` (accepted values `ask`, `never`; `--permission-mode` takes a configured preset name), recording what the model gets back in each. Headless has no live approval-response channel (`packages/bundle/headless/README.md:98`). The default policy is `ask`: an unanswered request resolves `unavailable` and fails closed, while `never` resolves `rejected` with different text. The default sandbox is `workspace-write` (`packages/bundle/headless/cordis.patch.yml:29-33`), so some actions stop before approval. E4 compares the `unavailable` and `rejected` denial outcomes plus sandbox blocks. | sandbox and approval policy, defaults, what a denial says to the model; compared with Fontaine's floor and guardian |
| E5 | A long session that crosses the compaction threshold plus a large tool result. Compaction triggers at floor(min(W×thresholdRatio, W−O−65536)) (`packages/compaction/compaction-basic/README.md:64-80`), about 840k tokens with the copied 1,048,576 `contextWindow`. The E5 overlay sets a small `thresholdRatio` through `compaction-basic` `modelPolicies` for `deepseek-v4.1-flash` (README:46-56,68), or lowers `contextWindow` while staying above O+65536. Record the computed trigger. | compaction trigger, summary prompt, tool-result pruning and spill, cache-hit accounting across compaction |
| E6 | A multi-step task in plan mode (through the Web UI: exit needs a review, `cordis.patch.yml:346`, and `/plan` is not available in headless), then with the todo tool, then with one subagent (both in headless). In the same Web UI session, set a goal and watch whether `goal-round-driver` starts a follow-up round | task representation, todo state, plan handoff, subagent context, goal rounds |
| E7 | Resume the E3 session with `--session-id <id>` (`packages/bundle/headless/src/startup.ts:59`; `--add-dir` and the policy flags must be repeated, `headless/README.md:90`), then inspect the on-disk log and `session-query` | session log format, durability, resume and review. Session-query search is off by default (`session-query-sqlite` with `path: ':memory:'`, `openAt: never`, `cordis.patch.yml:150-161`, giving `SESSION_QUERY_SEARCH_DISABLED`; `tool-session-query` is experimental and unmounted). A patch setting only `openAt: first-search` would expose nothing in headless, because only the unmounted `tool-session-query` calls search (`packages/experimental/tool-session-query/src/operations.ts:95,151`). So E7 adds `--patch packages/experimental/session-search/cordis.patch.yml`, which mounts `session-query-sqlite` with `openAt: first-search` plus `tool-session-query`, and asks the model to search the earlier session. |
| E8 | Run E2 and E3 through `fontaine -p --settings proxy.toml` (Chat wire) on the same model, through the proxy. It relies on the model's default thinking: Fontaine's main agent sends no thinking parameter (`thinking_off` applies only to guardian and scanner; #53 is open). Check the capture shows a `reasoning_content` response before judging the round-trip. Filter out guardian requests first (`thinking: disabled`, guardian system prompt); they carry no reasoning by design. The captures also check DeepSeek model-support items 1-4 where the gateway allows: reasoning passback on every assistant turn, the effort field sent, and whether a system-prompt or tool change keeps the earlier prefix hit, as reported by cache-hit tokens. Also provoke a tool-call turn with no reasoning: build a session with tool calls on any Go model other than `deepseek-v4.1-flash` (rig turns foreign reasoning into plain text on resume), then `fontaine -p --settings proxy.toml --resume <id> --model go/deepseek-v4.1-flash` through the proxy (`rust/fontaine/src/cli.rs:83-89`; `--model` overrides on resume, `rust/fontaine/src/lib.rs:347-372,464`), and record whether the request gets a 400. | whether reasoning-bearing turns are round-tripped as `reasoning_content` on the Chat wire (source says yes); whether a tool-call turn with no reasoning gets a 400; a side-by-side diff of framing against dsh |
| E9 | Where dsh's behaviour differs from what Fontaine copies, capture the Claude Code baseline with `claude -p` on the same fixture task | the "different from Claude Code" column |

Anything that cannot be run goes into a "Not tried" list with the reason (for example, no direct
DeepSeek API key, so `api.deepseek.com` Files and context caching are reached only through the
gateway, or the build fails). The user can then supply what is missing.

### The research doc

`research/research-deepseek-harness.md` follows the shape of `research-llamafirewall.md`:

1. Executive summary and bottom line for Fontaine.
2. Identity: repo, license, pinned commit, maturity, how it runs.
3. Architecture (Cordis, profiles, the turn flow).
4. One section per issue question: prompt and tool framing; reasoning output; context management
   (caching, compaction, truncation); permission and safety model; DeepSeek through rig, including
   as a guardian or screen (from source and the thinking-off history, not measured); tasks; session log; model feedback. Each section has "What dsh does",
   "Claude Code", "Fontaine today", the evidence, and a verdict.
5. Telemetry and privacy: what leaves the machine by default (headers, `dsh_*` fields, the
   session log upload, the telemetry exporter, every endpoint the container contacts).
6. **Desirable features**, the centrepiece, led by the features that let the model iterate on a
   solution. Each entry gives: what dsh does; the evidence (source citation and/or observed
   run); how Fontaine would get it (an existing issue it feeds, a new issue, or skip with the
   reason); and whether Claude Code does it (if not, adopting needs a Decisions line).
7. **DeepSeek model support**, the second centrepiece: what Fontaine should support to get the
   most out of the DeepSeek models, seeded from the Current State candidates. Each entry gives:
   what it is; the evidence (DeepSeek API docs, dsh adapter source, or a wire capture); which
   Fontaine wire(s), code path and existing issue it affects; and whether it is needed for
   correctness or is an optimisation.
8. Experiment log (E1–E9), with commands, results and failures.
9. Not tried, and why.

### Follow-ups and decisions

- For each desirable feature to adopt, file an issue (`gh issue create`, with labels matching the
  area) or comment on the existing issue it feeds, linking the doc section. A bug found in
  Fontaine (E8 is the likely candidate) is filed as a `bug` with its reproduction. File one more
  issue for a measured guardian scorecard of DeepSeek on the live eval harness. Each
  DeepSeek model-support correctness item becomes a `bug` issue. Each optimisation feeds its
  existing issue or a new one.
- A finding that argues for departing from Claude Code semantics gets a dated line in the master
  plan's `## Decisions`. Adopting something Claude Code also does needs no Decisions line.
- The doc and the PR body both carry the full findings list, including the negative ones.

## Implementation Steps

1. **Scaffold** `research/deepseek-harness/` with `scripts/`, `fixtures/`, `captures/` and a
   `.gitignore` for `runs/`. Add `README.md` there, covering how to reproduce each experiment and
   the pinned commit and image digest.
2. **Source read.** Read the pinned checkout's docs and packages listed in Current State and take
   notes per question, with `path@22ddee37` citations. Use Explore subagents for the broad
   package sweep, and read the cited files directly before quoting them.
3. **Tooling.** Write, all under `research/deepseek-harness/scripts/`: `proxy.toml` (no
   `wires`, Chat) and `proxy_messages.toml` (with the Messages `wires` override), the dsh
   overlays `*.patch.yml` (E3, E5; E7 uses the shipped
   `packages/experimental/session-search/cordis.patch.yml`), `record_proxy.py` and `scrub.py`
   (stdlib only, run with `python3 -I`), `dsh.Dockerfile` (`corepack enable`, `musl-tools`,
   `pnpm install`, `pnpm run build:artifacts`, `pnpm --dir native/system run build:native`) and
   `net.Dockerfile` (`socat`, `tcpdump`), `run_dsh.sh` (token freshness check, create the
   `--internal` network, start `fwd` with `--add-host=host.docker.internal:host-gateway` and
   `cap`, start `fixtures/stall.mjs` where E3 needs it, run
   `pnpm dsh --profile headless "<task>"` with the dummy key and `fwd` base URLs, a run
   directory, then tear the sidecars and network down; the proxy binds to the docker bridge
   address, not 0.0.0.0), and `run_fontaine.sh`
   (`fontaine -p --settings <proxy.toml|proxy_messages.toml> --model go/deepseek-v4.1-flash`).
4. **E1–E7** on dsh, in order. Record each result in the doc as it lands. E4 runs only inside the
   container.
5. **E8** through Fontaine with `proxy.toml` (Chat). Separate guardian exchanges from main-agent requests in the capture
   before checking anything. Confirm reasoning-bearing turns are round-tripped, and test a tool-call
   turn with no reasoning by resuming a session built on another Go model with
   `--resume <id> --model go/deepseek-v4.1-flash`. If the 400 reproduces, reproduce it minimally and file a bug about the
   missing empty-reasoning fallback for the Go DeepSeek route. Note whether the Messages wire
   avoids it by repeating the resume with `--settings proxy_messages.toml`.
6. **E9** `claude -p` baselines, only for the rows where the Claude Code behaviour is in doubt.
7. **Write the doc.** Fill in the summary, the Desirable features list, the experiment log and
   the Not tried list.
8. **Follow-ups.** File and comment on issues, including the guardian scorecard issue, then link
   them from the list. Add a `## Decisions` line only where the rule above applies.
9. **Index.** Add the doc to the research list in `tasks/fontaine_harness_plan.md:53-60`, add the
   missing `analysis-typed-decision-screen.md` entry there, and update the count in that list and
   in the workspace tree (~line 225) from "seven" to nine. Add a short "DeepSeek Harness"
   subsection to `tasks/design/research-findings.md` pointing to the deep-dive.

## Files to Modify

- New: `research/research-deepseek-harness.md`
- New: `research/deepseek-harness/{README.md,.gitignore,scripts/*,fixtures/*,captures/*}`
- `tasks/fontaine_harness_plan.md`: the research doc list (add the missing
  `analysis-typed-decision-screen.md` entry and the new doc, count nine there and in the
  workspace tree), and `## Decisions` only if needed
- `tasks/design/research-findings.md`: a pointer subsection

## Trade-offs

- **Container rather than bwrap.** bwrap would need Node on the host anyway.
- **Run through the opencode-go gateway rather than a direct DeepSeek key.** It is the only
  DeepSeek access available, and it is the path Fontaine users take. The cost is that
  gateway-specific behaviour (caching, Files API, wire coverage) may differ from
  `api.deepseek.com`. Each such gap is recorded in Not tried.
- **A stdlib recording proxy rather than mitmproxy.** Both harnesses accept a plain-HTTP base
  URL, so TLS interception is not needed and there are no new dependencies.

## Decisions

- 2026-10-24: The deliverable is a list of desirable features, led by those that let the model iterate; no benchmark or live scorecard in this study (user direction).
- 2026-10-24: The research doc also lists DeepSeek-model-specific support Fontaine should add, marked correctness or optimisation (user direction).
- 2026-10-24: AFK: dsh holds only a dummy `DEEPSEEK_API_KEY`; the host proxy injects the current `go` access token, read-only from `credentials.toml`, never refreshing (a pre-run `fontaine -p` refreshes; mid-run expiry gets a 503 and a rerun), and the proxy, `scrub.py` and the commit check redact the dummy key, the stored tokens and the `.anonymous-user-id` values, because E4's hostile commands must find no real credential and only Fontaine may rotate the refresh token.
- 2026-10-24: AFK: dsh is built by `docker build` with normal network and run on an `--internal` network whose only exit is a `socat` sidecar to the host proxy, with a `tcpdump` sidecar in dsh's network namespace logging DNS and packets and the E3 stall server inside the container, because this blocks direct egress and records it without changing the host firewall.
- 2026-10-24: AFK: the image builds the static musl `landlock-run` (`musl-tools` plus `build:native`) so `sandbox-local` selects Landlock under Docker's default seccomp, with no userns, seccomp or `--privileged` relaxation, and E1 records the runner and its enforcement, because that keeps the container's own isolation intact.
- 2026-10-24: AFK: Fontaine uses two settings files, `proxy.toml` (no `wires`, Chat) for E8 and `proxy_messages.toml` (Messages override) for the probe and comparison, because `--settings` takes one path and a `wires` override would put every E8 leg on Messages.
- 2026-10-24: AFK: E3 treats `create_goal` authority as settled by source and goal rounds in headless as "source says headless exits first; confirm by run", with rounds observed in the Web UI during E6, because the headless and goal-round-driver source answer both unknowns.
- 2026-10-24: AFK: E7 applies the shipped `packages/experimental/session-search/cordis.patch.yml` overlay in headless rather than using Web UI search, because only the unmounted `tool-session-query` calls search and the overlay keeps E7 scripted.

## Documentation

No `docs/` change. This is research, and `docs/` describes shipped behaviour. The research doc and
the two index updates in Step 9 are the documentation deliverable.

## Testing Strategy

There is no product code, so nothing is added to `cargo test`. The scripts are research tooling,
in the same tier as `research/typed-decision-screen/scripts/`. They are checked by running them:
each experiment's recorded output is the check. One exception: `scrub.py` runs over every
committed capture, and before commit two checks over `research/deepseek-harness/captures/` only
must find nothing, because a leaked key would fail silently. An `rg` uses credential-shaped
patterns (for example `Bearer [A-Za-z0-9._-]{20,}`). `scrub.py --check` searches for the literal
values: the dummy key, the current `go` access and refresh tokens from `credentials.toml`, and
every `runs/*/dsh-home/.anonymous-user-id`. It reports only file and line, never the value.
Access tokens rotated away before commit are covered by the proxy's header-name and in-memory
redaction at write time and by the `Bearer` pattern.

## Open Questions

- Does the Go gateway serve `deepseek-v4.1-flash` on its `/anthropic` (Messages) path? This
  decides whether dsh's native adapter can be exercised at all. A Fontaine probe on the Messages
  wire (`--settings proxy_messages.toml`) answers it, including whether the gateway accepts `x-api-key`,
  dsh's only auth header, before E2.
