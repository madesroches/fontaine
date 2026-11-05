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
  or npm (`@moonshot-ai/kimi-code`, Node >= 22.19). A source build needs Node >= 24.15 (root
  `package.json` engines, `.nvmrc` 24.15.0, engine-strict) and pnpm 10.33.0 via corepack. Login is Kimi Code OAuth or a Moonshot Open Platform API key.
- The repo ships `AGENTS.md`, `CLAUDE.md` and `todo.md`; no `SAFETY.md` is known. Treat it as
  unaudited code that runs shell commands.
- Mechanisms to look for first, per the issue's questions: loop guards, goal or continuation
  mechanisms, timeouts, tool-error shape, diagnostics feedback, hooks, plan mode, todo and subtask
  state (`reference/tools.md`, `customization/agents.md`), the permission and sandbox model,
  compaction, the session record (`agent-core-v2/src/wire/{journal,record,repair}.ts`), and
  telemetry (`packages/telemetry`, `agent-core-v2/src/app/telemetry/`,
  `apps/kimi-code/src/cli/telemetry.ts`; search the tree for exporters, `usage`, update checks).

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
  agent sends no thinking parameter (#53 open). The guardian's thinking-off is sent only on the
  opencode-go path (`opencode_go.rs` `thinking_off`); the `kimi` openai-compatible account sends
  none (the trait default `thinking_off` is None).
- An account's `base_url` can be overridden in a `--settings` file (the DeepSeek study's
  `proxy.toml` pattern), so Fontaine's traffic can go through a recording proxy with no code
  change. The settings layer replaces a named account whole, so the `kimi` override repeats `kind`,
  `base_url` and `models` (not `wires`, which `check_account` rejects for non-opencode-go kinds);
  the `madesroches` override repeats `kind` and `base_url`, plus `wires` only in the Messages
  variant.
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

- `kimi` runs **only in a Docker container** (a Node >= 24.15 base pinned by digest with pnpm
  10.33.0 via corepack for a source build; a Node 22.19+ base serves only the npm install of
  `@moonshot-ai/kimi-code@2.5.1`, or the official install script if the source build proves
  impractical; the choice and the resulting version are recorded), with a throwaway workspace volume, a throwaway `KIMI_CODE_HOME` under
  `runs/<run>/kimi-home/`, and no host mounts besides `research/kimi-harness/runs/<run>/`.
- **No real credential enters the container.** The harness gets a dummy `api_key` and a custom
  provider `base_url` pointing at `fwd:<port>`. Per the pinned docs
  (`docs/en/configuration/providers.md@df90d90e`, `config-files.md`) a provider is
  `[providers.<name>]` with `type`, `base_url`, `api_key`, plus `[models."<alias>"]` with
  `provider`, `model` and a required `max_context_size`, selected by `-m <alias>`. Provider types
  are `kimi`, `openai`, `anthropic`, `openai_responses`, `google-genai`, `vertexai`; the managed
  OAuth provider uses `https://api.kimi.com/coding/v1`. The `kimi-for-coding` leg uses
  `type = "kimi"` (OpenAI-compatible with any `base_url`; it sends a User-Agent and `X-Msh-*`
  identity headers), the Go leg uses `type = "openai"` (takes a `base_url` and a per-model
  `reasoning_key`). Credentials are not read from the shell env except via `api_key_env` or
  `KIMI_MODEL_*`, so the dummy key goes in the config. The host recording proxy injects the real
  credential per request: for `kimi-for-coding` the `kimi` account sets no `api_key_env`, so the
  key is `credentials.toml` `[accounts.kimi].api_key`, which Fontaine sends as
  `Authorization: Bearer`; the harness listener reads it read-only and injects it. For
  `opencode-go` it is the current `madesroches` access token (`[accounts.madesroches.oauth]`)
  read read-only from `credentials.toml`, never refreshed and never written
  (a pre-run `fontaine -p` refreshes; on mid-run expiry the proxy answers 503, the run is marked
  invalid and repeated). Hostile commands in the safety experiment can therefore find only the
  dummy key.
- Build and run are split: build with normal Docker network, run on an `--internal` network whose
  only exit is a `socat` forwarder to the host proxy. A `tcpdump` sidecar shares the harness's
  network namespace and records `egress.pcap` for the whole run, which is what answers the
  telemetry question (including update checks, plugin marketplace, remote-control and any
  analytics endpoint). Telemetry is **not** disabled for the baseline run; a second run sets
  `KIMI_DISABLE_TELEMETRY=1` and `KIMI_CODE_NO_AUTO_UPDATE=1` (separate switches; also top-level
  `telemetry = false` in `config.toml`), and the difference is recorded.
- The harness's own sandbox, if any, is probed in E1 (what it selects in the container and its
  reported enforcement). No `--privileged`, seccomp or user-namespace relaxation.

### Wire capture

Reuse `record_proxy.py` (stdlib only): reverse proxy to a configured upstream, SSE passed through,
one JSONL record per exchange, redaction of header names and of literal secret values (dummy key,
injected tokens, current `credentials.toml` tokens, any anonymous install id the harness writes).
One `record_proxy.py` instance per upstream, each with two listeners on the docker bridge
address: a harness port (injects auth: the Kimi API key on the Kimi upstream, the `madesroches`
access token on the Go upstream) and a Fontaine port (passes its own `authorization` through).
The copied `record_proxy.py` hardcodes the account name `go` (line ~26) and must be adapted to
`madesroches`. Upstreams:

- `https://api.kimi.ai/coding/v1` for `kimi-for-coding` (the Fontaine `kimi` account; the harness
  gets the same base URL via its provider config).
- `https://opencode.ai/inference/go` for the `opencode-go` Kimi models (Chat wire; Messages wire
  probed with a `wires` override as the DeepSeek study did).

Fontaine runs as `fontaine -p --permission-mode auto --settings <proxy.toml> --model
kimi/kimi-for-coding` (and the `madesroches/<model>` equivalent; headless starts in `manual`,
where would-ask outcomes are denied), so both harnesses hit the same models through the same recorder and the doc can
diff request framing side by side. `proxy.toml` also sets `[guardian] model` to the Kimi model under test and overrides
`[guardian.composition]` so guardian calls go through the recorder (otherwise the guardian wire
record is empty). Guardian exchanges (thinking-off, the guardian system prompt)
are separated from main-agent requests before any comparison and kept as the guardian wire record.

### Experiments

Each has a script under `scripts/`, a fixture repo under `fixtures/` where needed, and a results
section in the doc.

| # | Experiment | Answers |
|---|---|---|
| E1 | Build/install, boot headless (`kimi --prompt`, `-p`, `--output-format text\|stream-json`; locked to auto permission, cannot combine with --yolo/--auto/--plan), `GET /models` on both upstreams (pick each experiment's model), record config dump, tool list, egress endpoints from the pcap, and the sandbox it selects | identity, environment, telemetry baseline, safety model |
| E2 | One read-only question, wire captured | system-prompt sections, tool schemas, call/result format, headers and non-standard body fields, how tool results and reasoning are framed, prompt-cache hints (`cacheKey` stage), sampling parameters sent, `max_tokens`, deferred-tool behaviour |
| E3 | Fix a failing test in a fixture repo (edit, run tests, iterate), with triggers for each loop mechanism found in the source: a repeated identical tool call, a hung command, a tool error, a lint/diagnostic failure, a user correction mid-run | iteration: loop guards, timeouts, goal/continuation, error and diagnostics feedback |
| E4 | Risky actions (write outside the workspace, `rm -rf` of a parent, `curl \| sh`, read `~/.ssh`) under default and most-permissive permission modes (non-default modes go through `kimi acp` or the TUI, since `--prompt` is locked to auto; else Not tried), inside the container only | permission and safety model vs. the guardian and floor |
| E5 | A long session crossing the compaction threshold (threshold lowered by config if exposed) plus one very large tool result | compaction, truncation, caching under compaction, context-window sizing for Kimi models |
| E6 | Multi-step task with plan mode (via `kimi acp` or the TUI, else Not tried), the todo tool and one subagent; a goal/continuation command if the source has one | task representation, tracking and hand-off |
| E7 | Resume the E3 session, then inspect the on-disk session record (`sessions/<workDirKey>/<id>/`, `session_index.jsonl`) | session log format, durability, repair on resume, review |
| E8 | Reasoning: tool-call turns with thinking on, thinking effort variants the harness exposes, a model switch mid-session; inspect what is stored and replayed (`reasoning_content` round-trip, stripping, effort control) | Kimi reasoning handling; compare with Fontaine's replay |
| E9 | E2, E3 and E8 through `fontaine -p --permission-mode auto --settings proxy.toml` (which also points `[guardian] model` and `[guardian.composition]` at the recorder) on the same models; separate guardian traffic; confirm no thinking-off on the `kimi` account on the wire; resume a session built on another model with `--resume <id> --model kimi/kimi-for-coding` and with a `madesroches/` Kimi model | Fontaine provider-path risks: empty-reasoning fallback, tool-call-id and history legalization, sampling parameters ignored or clamped, cache-prefix stability, usage and cache accounting fields, `kimi-for-coding` quirks |
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
   sweeps; read cited files directly before quoting. Confirm the provider syntax and
   `type` choices recorded under Environment and safety against the source.
3. **Tooling.** Copy and adapt from `research/deepseek-harness/scripts/`: `record_proxy.py`,
   `scrub.py`, `net.Dockerfile`, a `kimi.Dockerfile` (or install-script image), `run_kimi.sh`
   (token freshness check, internal network, `fwd` and `cap` sidecars, harness run with dummy key,
   teardown), `run_fontaine.sh`, `proxy.toml` / `proxy_messages.toml` for the `kimi` and `madesroches`
   accounts (the `kimi` override repeats `kind`, `base_url`, `models`; the `madesroches` override
   repeats `kind` and `base_url`, plus `wires` only in the Messages variant), a harness config
   template, and the fixture repos. The Kimi key comes from `credentials.toml`
   `[accounts.kimi].api_key`; the `madesroches` token from `[accounts.madesroches.oauth]`.
4. **E1–E8** on the harness in order, recording each result in the doc as it lands. E4 runs only
   inside the container.
5. **E9** through Fontaine (`--permission-mode auto`), then the extra probes. Re-verify rig's reasoning-replay behaviour for
   Kimi rows against the pinned rig version. Reproduce any 400 or mangled-history bug minimally.
6. **E10** `claude -p` baselines for the doubtful rows.
7. **Write the doc**: summary, both lists, experiment log, Not tried.
8. **Follow-ups.** File and comment on issues, link them from the lists; add `## Decisions` lines
   only where the rule above applies.
9. **Index.** Add the doc to the research list in `tasks/fontaine_harness_plan.md` (add the four missing entries
   `analysis-jev-improvements.md`, `analysis-jev-openrouter.md`, `analysis-screener-models.md`,
   `research-hermes-harness.md` plus the Kimi doc, and set the count, now stale at "twelve", to
   "seventeen" in the research paragraph and in the workspace tree), and add a short "Kimi Code CLI" subsection to
   `tasks/design/research-findings.md` pointing to the deep-dive, and add a `#### Research`
   bullet to `CHANGELOG.md`.

## Files to Modify

- New: `research/research-kimi-harness.md`
- New: `research/kimi-harness/{README.md,.gitignore,scripts/*,fixtures/*,captures/*}`
- `tasks/fontaine_harness_plan.md`: research doc list and counts (twelve to seventeen); `## Decisions` only if needed
- `tasks/design/research-findings.md`: pointer subsection
- `CHANGELOG.md`: `#### Research` bullet

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

## Decisions

- 2026-11-05: AFK: the study pins `MoonshotAI/kimi-code` at commit `df90d90ec0714db23dcc830acd2db6232f5c7426` (HEAD at planning, package 2.5.1), not a release tag or the 2.6 canary, so every citation resolves to one tree.
- 2026-11-05: AFK: credentials follow the DeepSeek study's rule: the container holds only a dummy key and the host proxy injects the real Kimi account key or `madesroches` access token, read-only, never refreshing, because the container must not be able to leak a real credential.
- 2026-11-05: AFK: telemetry is measured with the harness's default settings first and with `KIMI_DISABLE_TELEMETRY=1` and `KIMI_CODE_NO_AUTO_UPDATE=1` second, because the issue asks what leaves the machine by default.
- 2026-11-05: AFK: Kimi as guardian/screen is assessed from source and the existing scorecard history, not re-measured, because the issue excludes benchmarks and live scorecards.

## Documentation

No `docs/` change: this is research and `docs/` describes shipped behaviour. The research doc and
the two index updates in step 9 are the documentation deliverable.

## Testing Strategy

No product code, so nothing is added to `cargo test`. The scripts are research tooling, in the
same tier as `research/deepseek-harness/scripts/`; each experiment's recorded output is its check.
One automated check is mandatory because a leak would fail silently: before commit, `scrub.py
--check research/kimi-harness/captures/` must report 0 hits (dummy key, current Kimi key and `madesroches`
tokens from `credentials.toml`, any anonymous install id), and an `rg` over the same directory for
credential-shaped patterns (`sk-[A-Za-z0-9_-]{20,}`) must
find nothing. Neither prints a matched value.
