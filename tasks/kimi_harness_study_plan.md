# Kimi Harness Study Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/156

## Overview

Study Moonshot's agent harness, Kimi Code CLI (`MoonshotAI/kimi-code`, command `kimi`, MIT,
TypeScript), and record what Fontaine should adopt, adapt or deliberately skip. The deliverable is
`research/research-kimi-harness.md`, built around two lists: desirable features (led by those that
let the model iterate on a solution) and Kimi-model-specific support Fontaine should add. This is a
research spike in the pattern of the DeepSeek study (`completed/deepseek_harness_study_plan.md`,
#154) and the oh-my-pi study (`completed/oh_my_pi_harness_study_plan.md`, #213): read the pinned
source, run the harness in a disposable container with a recording proxy, run Fontaine on the same
tasks, record every finding including negative ones, and file follow-up issues. No product crate
changes, no benchmark or live scorecard.

## Current State

**The subject.** `github.com/MoonshotAI/kimi-code`, MIT, not archived, very active (default branch
`main` pushed 2026-11-05). The study pins commit `df90d90ec0714db23dcc830acd2db6232f5c7426`
(2026-11-05; npm package `@moonshot-ai/kimi-code` 2.5.1 is the latest stable tag, a 2.6.0 canary
exists). Every source citation in the doc is `path@df90d90e`. The older Python
`MoonshotAI/kimi-cli` is archived (last push 2026-09-22); it is consulted only where a difference
explains a design choice. First look at the layout, to be verified rather than trusted:

- `apps/kimi-code` (the CLI, `src/main.ts`, `src/built-in-catalog.ts`), `apps/vis`, `apps/vscode`,
  and `packages/`: `agent-core-v2` (the core), `acp-server`, `kap-server`, `klient`, `node-sdk`,
  `oauth`, `minidb`, `pi-tui`, `migration-legacy`.
- `packages/agent-core-v2/docs/en/llm.md` describes the single-request LLM layer: a protocol
  pipeline (`cacheKey → thinking → responseFormat → maxOutputTokens → sampling → tool → message →
  mergeTurns → annotate → finalize → flatten`) with legalize passes for media, tool-call ids,
  thinking history and model rules, and a Kimi protocol patch at
  `src/human/llm/builtin/provider/kimi/protocol.ts`. It states "no silent fallback", deferred tool
  disclosure (`SelectTools`, added/removed announcements via `loop.remind`), and `kimi-file://` /
  `ms://` media references with Kimi file uploads. This is the densest source for the
  model-specific questions.
- User docs under `docs/en/`: `configuration/{config-files,data-locations,env-vars,providers}.md`,
  `customization/{agents,hooks,mcp,plugins,skills,datasource}.md`,
  `guides/{sessions,interaction,tower,remote-control,...}.md`, `reference/tools.md`,
  `reference/slash-commands.md`. Data lives under `~/.kimi-code` (override `KIMI_CODE_HOME`):
  `config.toml`, `sessions/<workDirKey>/<sessionId>/`, `session_index.jsonl`, `credentials/`,
  `logs/`.
- Distribution is a single binary (`curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash`)
  or npm, Node >= 22.19. Login is Kimi Code OAuth or a Moonshot Open Platform API key.
- The repo ships `AGENTS.md`, `CLAUDE.md` and `todo.md`; no `SAFETY.md` is known. Treat it as
  unaudited code that runs shell commands.
- Mechanisms to look for first, per the issue's questions: loop guards, goal or continuation
  mechanisms, timeouts, tool-error shape, diagnostics feedback, hooks, plan mode, todo and subtask
  state (`reference/tools.md`, `customization/agents.md`), the permission and sandbox model,
  compaction, the session record (`agent-core-v2/src/wire/{journal,record,repair}.ts`), and
  telemetry (search the tree for exporters, `usage`, `telemetry`, update checks).

**Fontaine today.**

- Kimi reaches Fontaine two ways. (1) The `kimi` account in the local config is
  `kind = "openai-compatible"`, `base_url = "https://api.kimi.ai/coding/v1"`, with models
  `k3.1-pro`, `k3.1-pro-highspeed`, `k3`, `k3-256k`, `kimi-for-coding`,
  `kimi-for-coding-highspeed`, and the default main model is `kimi/k3.1-pro`. A `KimiQuota`
  source reads the subscription windows from `GET <base_url>/usages`
  (`rust/fontaine-core/src/providers/kimi_quota.rs`, wired at `openai_compatible.rs:61`).
  (2) `opencode-go` serves Kimi models on the Chat wire (`providers/opencode_go.rs`: `route()`
  sends `kimi-k3` to `Wire::Chat`, a `wires` override can force Messages).
  `kimi/kimi-for-coding` is the reference guardian scorecard model (control row, 89.7% catch /
  3.9% false deny / p95 16.1 s in `fontaine_harness_plan.md`), and the Jev screen studies replay
  its verdicts.
- Reasoning: rig-core's Chat decode stores `reasoning_content` as a native reasoning item and
  replays it, and adds an empty `reasoning_content` on tool-call turns for a moonshot or
  opencode-go/moonshot/openrouter catalog row (see the DeepSeek plan's Current State for the
  source lines; they are re-verified against the pinned rig version in step 5). Fontaine's main
  agent sends no thinking parameter (#53 open). The guardian turns thinking off for the models
  where it can (`opencode_go.rs` `thinking_off`).
- An account's `base_url` can be overridden in a `--settings` file (the DeepSeek study's
  `proxy.toml` pattern), so Fontaine's traffic can go through a recording proxy with no code
  change. The settings layer replaces a named account whole, so the override repeats `kind`,
  `models` and `wires`.
- Precedent and tooling to reuse: `research/deepseek-harness/scripts/` (`record_proxy.py`,
  `scrub.py`, `net.Dockerfile`, the `--internal` network + `socat` forwarder + `tcpdump` sidecar
  design, token-injecting proxy) and `research/oh-my-pi-harness/`. This plan copies and adapts
  those scripts rather than designing new ones. Host: Docker 29.8 present; `node` 22 and `pnpm`
  present on the host but the harness is built inside the container; `bwrap` absent.
- Related open issues that findings may feed rather than duplicate: #13 prompt caching, #53
  reasoning effort, #22 todo tools, #24 plan-mode exit, #10 session UX, #12 checkpoints/rewind,
  #17 /goal, #27 LSP, #59 ACP, #39 forked subagents, #21 images, #149 and #97 (local guardian),
  #152 (`autocompact_window`), #56 cache accounting, #132 (OpenRouter scoring). The DeepSeek and
  oh-my-pi studies' follow-up issues are checked first so nothing is filed twice.

## Design

### Method

Two tracks: read the pinned source and docs, and run the harness. Every claim in the doc carries
its evidence: a `path@df90d90e` citation, a captured request or log line, or a command and its
output. Negative results are findings ("X does not exist", "Y failed because Z"). Claims about
Claude Code are checked with `claude -p` where in doubt (E10), never from memory.

### Environment and safety

Copied from the DeepSeek study, with these Kimi-specific points:

- `kimi` runs **only in a Docker container** (Node 22+ base pinned by digest, or the official
  install script if the source build proves impractical; the choice and the resulting version are
  recorded), with a throwaway workspace volume, a throwaway `KIMI_CODE_HOME` under
  `runs/<run>/kimi-home/`, and no host mounts besides `research/kimi-harness/runs/<run>/`.
- **No real credential enters the container.** The harness gets a dummy key and a custom provider
  `base_url` pointing at `fwd:<port>` (provider configuration is in
  `docs/en/configuration/providers.md`; step 2 confirms an OpenAI-compatible Chat provider with a
  custom `base_url` and model id can be declared, since the built-in Kimi Code provider uses
  OAuth). The host recording proxy injects the real credential per request: for
  `kimi-for-coding` the Kimi account key Fontaine already holds, for `opencode-go` the current
  `go` access token read read-only from `credentials.toml`, never refreshed and never written
  (a pre-run `fontaine -p` refreshes; on mid-run expiry the proxy answers 503, the run is marked
  invalid and repeated). Hostile commands in the safety experiment can therefore find only the
  dummy key.
- Build and run are split: build with normal Docker network, run on an `--internal` network whose
  only exit is a `socat` forwarder to the host proxy. A `tcpdump` sidecar shares the harness's
  network namespace and records `egress.pcap` for the whole run, which is what answers the
  telemetry question (including update checks, plugin marketplace, remote-control and any
  analytics endpoint). Telemetry is **not** disabled for the baseline run; a second run sets
  whatever opt-out the docs name, and the difference is recorded.
- The harness's own sandbox, if any, is probed in E1 (what it selects in the container and its
  reported enforcement). No `--privileged`, seccomp or user-namespace relaxation.

### Wire capture

Reuse `record_proxy.py` (stdlib only): reverse proxy to a configured upstream, SSE passed through,
one JSONL record per exchange, redaction of header names and of literal secret values (dummy key,
injected tokens, current `credentials.toml` tokens, any anonymous install id the harness writes).
Two listeners on the docker bridge address: one for the harness (injects auth), one for Fontaine
(passes its own `authorization` through). Upstreams:

- `https://api.kimi.ai/coding/v1` for `kimi-for-coding` (the Fontaine `kimi` account; the harness
  gets the same base URL via its provider config).
- `https://opencode.ai/inference/go` for the `opencode-go` Kimi models (Chat wire; Messages wire
  probed with a `wires` override as the DeepSeek study did).

Fontaine runs as `fontaine -p --settings <proxy.toml> --model kimi/kimi-for-coding` (and the
`go/` equivalent), so both harnesses hit the same models through the same recorder and the doc can
diff request framing side by side. Guardian exchanges (thinking-off, the guardian system prompt)
are separated from main-agent requests before any comparison and kept as the guardian wire record.

### Experiments

Each has a script under `scripts/`, a fixture repo under `fixtures/` where needed, and a results
section in the doc.

| # | Experiment | Answers |
|---|---|---|
| E1 | Build/install, boot headless (`kimi` print or non-interactive mode, whichever `reference/kimi-command.md` documents), record config dump, tool list, egress endpoints from the pcap, and the sandbox it selects | identity, environment, telemetry baseline, safety model |
| E2 | One read-only question, wire captured | system-prompt sections, tool schemas, call/result format, headers and non-standard body fields, how tool results and reasoning are framed, prompt-cache hints (`cacheKey` stage), sampling parameters sent, `max_tokens`, deferred-tool behaviour |
| E3 | Fix a failing test in a fixture repo (edit, run tests, iterate), with triggers for each loop mechanism found in the source: a repeated identical tool call, a hung command, a tool error, a lint/diagnostic failure, a user correction mid-run | iteration: loop guards, timeouts, goal/continuation, error and diagnostics feedback |
| E4 | Risky actions (write outside the workspace, `rm -rf` of a parent, `curl \| sh`, read `~/.ssh`) under default and most-permissive permission modes, inside the container only | permission and safety model vs. the guardian and floor |
| E5 | A long session crossing the compaction threshold (threshold lowered by config if exposed) plus one very large tool result | compaction, truncation, caching under compaction, context-window sizing for Kimi models |
| E6 | Multi-step task with plan mode, the todo tool and one subagent; a goal/continuation command if the source has one | task representation, tracking and hand-off |
| E7 | Resume the E3 session, then inspect the on-disk session record (`sessions/<workDirKey>/<id>/`, `session_index.jsonl`) | session log format, durability, repair on resume, review |
| E8 | Reasoning: tool-call turns with thinking on, thinking effort variants the harness exposes, a model switch mid-session; inspect what is stored and replayed (`reasoning_content` round-trip, stripping, effort control) | Kimi reasoning handling; compare with Fontaine's replay |
| E9 | E2, E3 and E8 through `fontaine -p --settings proxy.toml` on the same models; separate guardian traffic; resume a session built on another model with `--resume <id> --model kimi/kimi-for-coding` and with a `go/` Kimi model | Fontaine provider-path risks: empty-reasoning fallback, tool-call-id and history legalization, sampling parameters ignored or clamped, cache-prefix stability, usage and cache accounting fields, `kimi-for-coding` quirks |
| E10 | `claude -p` baselines on the same fixture for each row where Claude Code's behaviour is in doubt | the "Claude Code does it?" column |

Further probes inside E2/E9 (cheap, scripted against the proxy, no harness needed): context window
and `max_tokens` limits per model from the models endpoint; whether `temperature`/`top_p` are
ignored or clamped when thinking is on; image and video input shape and size limits through the
gateway; Kimi as guardian/screen (from the `kimi-for-coding` scorecard history and the thinking-off
path, not re-measured: no scorecard in this study). Anything that cannot be run goes to "Not
tried" with the reason, so the user can supply what is missing.

### The research doc

`research/research-kimi-harness.md`, same shape as the DeepSeek doc:

1. Executive summary and bottom line for Fontaine.
2. Identity: repo, license, pinned commit, maturity, how it runs, kimi-cli vs. kimi-code where it
   explains a choice.
3. Architecture and the turn flow.
4. One section per issue question (iteration; model-specific support; framing; context
   management; safety; tasks; session log; telemetry and privacy; Fontaine's provider path). Each
   has "What kimi does", "Claude Code", "Fontaine today", evidence, verdict.
5. **Desirable features**, led by those that let the model iterate. Each entry: what the harness
   does; evidence (citation or observed run); the Fontaine issue it feeds or a new one filed;
   whether Claude Code does it (if not, adopting needs a Decisions line).
6. **Kimi-model-specific support**: what Fontaine should add for Kimi models, each marked
   correctness or optimisation, with evidence, affected wire and code path, and issue.
7. Experiment log (E1–E10) with commands, results, failures.
8. Not tried, and why.

### Follow-ups and decisions

- Per desirable feature or model-support item to adopt: `gh issue create` (labels matching the
  area) or a comment on the existing issue it feeds, linked from the doc. A Fontaine bug found in
  E9 is filed as `bug` with a minimal reproduction. One issue for a measured guardian scorecard of
  any newly relevant Kimi model, only if the study surfaces one.
- A finding that argues for departing from Claude Code semantics gets a dated line in the master
  plan's `## Decisions`, at the end of its subject's subsection. Adopting what Claude Code also
  does needs none.
- The doc and PR body both carry the full findings list, negatives included.

## Implementation Steps

1. **Scaffold** `research/kimi-harness/` with `scripts/`, `fixtures/`, `captures/`, a `.gitignore`
   for `runs/`, and a `README.md` (pinned commit, image digests, how to reproduce each
   experiment, the safety rules the tooling enforces).
2. **Source read.** Clone the pinned commit (shallow, outside the repo) and take notes per issue
   question with `path@df90d90e` citations; start from `packages/agent-core-v2/docs/en/llm.md`, the
   Kimi protocol patch, `docs/en/reference/tools.md`, `docs/en/configuration/providers.md`, the
   permission, hooks, compaction, wire/journal and telemetry code. Use Explore subagents for broad
   sweeps; read cited files directly before quoting. Confirm how a custom OpenAI-compatible
   provider with a base URL and key is declared, and which non-interactive mode drives scripted
   runs.
3. **Tooling.** Copy and adapt from `research/deepseek-harness/scripts/`: `record_proxy.py`,
   `scrub.py`, `net.Dockerfile`, a `kimi.Dockerfile` (or install-script image), `run_kimi.sh`
   (token freshness check, internal network, `fwd` and `cap` sidecars, harness run with dummy key,
   teardown), `run_fontaine.sh`, `proxy.toml` / `proxy_messages.toml` for the `kimi` and `go`
   accounts, a harness config template, and the fixture repos. Resolve where the proxy gets the
   Kimi account key from (same source Fontaine reads) in this step.
4. **E1–E8** on the harness in order, recording each result in the doc as it lands. E4 runs only
   inside the container.
5. **E9** through Fontaine, then the extra probes. Re-verify rig's reasoning-replay behaviour for
   Kimi rows against the pinned rig version. Reproduce any 400 or mangled-history bug minimally.
6. **E10** `claude -p` baselines for the doubtful rows.
7. **Write the doc**: summary, both lists, experiment log, Not tried.
8. **Follow-ups.** File and comment on issues, link them from the lists; add `## Decisions` lines
   only where the rule above applies.
9. **Index.** Add the doc to the research list in `tasks/fontaine_harness_plan.md` (and update the
   document count there and in the workspace tree), and add a short "Kimi Code CLI" subsection to
   `tasks/design/research-findings.md` pointing to the deep-dive.

## Files to Modify

- New: `research/research-kimi-harness.md`
- New: `research/kimi-harness/{README.md,.gitignore,scripts/*,fixtures/*,captures/*}`
- `tasks/fontaine_harness_plan.md`: research doc list and counts; `## Decisions` only if needed
- `tasks/design/research-findings.md`: pointer subsection

## Trade-offs

- **Container, not host install.** The harness executes shell commands and ships an installer
  that pipes to bash; isolation and an egress log matter more than convenience. The cost is a
  build step and dummy-credential plumbing, which the DeepSeek study already solved.
- **Recording proxy in front of the real Kimi endpoints, not a mock.** Wire framing,
  reasoning and cache behaviour are the questions; only the live models answer them. The cost is
  that experiments spend a little quota on the Kimi subscription and the Go gateway.
- **Both `kimi-for-coding` and an `opencode-go` Kimi model.** They are the two paths Fontaine
  users take and may differ (gateway-specific caching, wires). Doubling E2/E3/E8/E9 for both is
  more spend; E4–E7 are harness-only and run on one model.
- **Pinned commit rather than the 2.6 canary or latest tag.** Citations stay checkable; the doc
  records the version and notes drift.

## Decisions

- 2026-11-05: AFK: the study pins `MoonshotAI/kimi-code` at commit `df90d90ec0714db23dcc830acd2db6232f5c7426` (HEAD at planning, package 2.5.1), not a release tag or the 2.6 canary, so every citation resolves to one tree.
- 2026-11-05: AFK: credentials follow the DeepSeek study's rule: the container holds only a dummy key and the host proxy injects the real Kimi account key or `go` access token, read-only, never refreshing, because the container must not be able to leak a real credential.
- 2026-11-05: AFK: telemetry is measured with the harness's default settings first and with its documented opt-out second, because the issue asks what leaves the machine by default.
- 2026-11-05: AFK: Kimi as guardian/screen is assessed from source and the existing scorecard history, not re-measured, because the issue excludes benchmarks and live scorecards.

## Documentation

No `docs/` change: this is research and `docs/` describes shipped behaviour. The research doc and
the two index updates in step 9 are the documentation deliverable.

## Testing Strategy

No product code, so nothing is added to `cargo test`. The scripts are research tooling, in the
same tier as `research/deepseek-harness/scripts/`; each experiment's recorded output is its check.
One automated check is mandatory because a leak would fail silently: before commit, `scrub.py
--check research/kimi-harness/captures/` must report 0 hits (dummy key, current Kimi key and `go`
tokens from `credentials.toml`, any anonymous install id), and an `rg` over the same directory for
credential-shaped patterns (for example `Bearer [A-Za-z0-9._-]{20,}`, `sk-[A-Za-z0-9]{20,}`) must
find nothing. Neither prints a matched value.

## Open Questions

- Can kimi-code be pointed at a custom OpenAI-compatible Chat endpoint with a static key (so the
  proxy can sit in front of `kimi-for-coding` and the `go` gateway)? Step 2 answers it from
  `docs/en/configuration/providers.md`. If only the built-in OAuth Kimi Code provider is usable,
  the harness legs run with the dummy-key proxy injecting the account key on that provider's
  base URL, or are recorded in Not tried and the source-reading track carries the answers.
- Which models does the Kimi account's key actually serve right now (`k3.1-pro`, `kimi-for-coding`
  and the highspeed variants)? E1's models probe settles which model each experiment uses.
