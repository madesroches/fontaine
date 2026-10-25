# Research Findings

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Research Findings (prior art this design is grounded in)

### Claude Code auto mode (the feature being replicated, and its exact semantics)

From the official docs (code.claude.com/docs/en/permission-modes and /auto-mode-config):

- **Six permission modes**: `default` (manual, prompt on everything state-changing), `acceptEdits`
  (auto-approve edits in workdir), `plan` (read/explore only, then approve a plan), `auto`
  (classifier reviews actions), `dontAsk` (pre-approved tools only; anything that would prompt is
  denied — for CI), `bypassPermissions` (everything runs; containers only). Shift+Tab cycles modes.
- **Auto mode = a second model ("the classifier") reviews each action before it runs.** It is now
  the default mode for interactive sessions (v2.1.283+). Anthropic reports it caught **89% of
  dangerous commands vs 13.6% for human reviewers** at prompts.
- **What the classifier sees**: user messages, pending tool calls (not read-only lookups), and
  CLAUDE.md/project-instruction content. It does **not** see raw tool outputs — limiting what
  injected content can argue to the classifier directly. A separate server-side probe scans
  incoming tool *results* for suspicious content before the agent reads them.
- **Decision order**: explicit `deny` rules → explicit `ask` rules (always prompt, even in auto) →
  read-only skip → critical-path removals (`rm -rf /`, `~`) always prompt, never auto-approved → classifier.
- **Fail-closed**: if the classifier request fails, times out, or returns no parseable verdict, the
  action is **denied**, never run unreviewed.
- **Denial UX**: the agent receives the denial reason (e.g. `[Data Exfiltration]`, `[Production
  Deploy]`) and adapts; denials are recorded in a "Recently denied" list where the user can mark
  actions for retry; repeated denials prompt the user to teach the classifier about their
  environment.
- **Classifier config is prose, not regex**: four lists — `environment` (trusted infra: repos,
  buckets, internal domains, registries, sensitive locations), `hard_deny` (unconditional),
  `soft_deny` (destructive; clearable by explicit, specific user intent), `allow` (exceptions to
  soft denies). Precedence: hard_deny > soft_deny > allow > explicit user intent clears remaining
  soft blocks. General requests ("clean up the repo") do **not** count as intent; specific ones
  ("force-push this branch") do. User-level settings only — project-level files can't inject
  allow rules (a checked-in repo must not be able to loosen its own guardian).
- **The classifier model is separately configurable** (`CLAUDE_CODE_AUTO_MODE_MODEL`, default
  Sonnet 5) — the guardian can be a cheaper/faster model than the main agent.
- Default blocked behaviors worth seeding our guardian rules with: `curl | bash`, exfiltration to
  unrecognized endpoints, prod deploys/migrations, force push, `git reset --hard`/`clean -fd` of
  pre-session state, IAM grants, destroying files that existed before the session, secrets into
  commits/PRs/logs, tunnels/reverse shells, launching other unguarded agent loops
  (`--dangerously-skip-permissions`), disabling safety flags (`--insecure`), commenting out
  security-guarding tests, `git remote set-url` to unnamed third parties, merging unreviewed PRs /
  self-approving own PR / disabling CI.
- **Trust boundary**: only the working directory and git remotes configured at session start are
  trusted; remotes added mid-session are not.

### Codex CLI (codex-rs) — Rust architecture precedent

- Large Cargo workspace (60+ crates): `core` (agent loop, `ToolOrchestrator`), `tui` (ratatui),
  `exec` (headless), provider crates, sandbox crates.
- `ToolOrchestrator` pipeline: **approval check → sandbox selection → execute → retry-with-
  escalation on denial**. Config in `~/.codex/config.toml` with `approval_policy` ×
  `sandbox_mode` as orthogonal axes. Fontaine mirrors this shape but replaces the
  sandbox/approval-policy axes with **permission mode × guardian**.
- Confirms the pattern: headless and TUI are thin surfaces over a shared `core` crate.
- **Codex ships its own LLM adjudicator too**: codex-rs main source has `GuardianReviewContext` +
  `strict_auto_review` in `core/src/tools/orchestrator.rs`, plus `ext/guardian-reviewer` and
  `ext/guardian-v2` crates; the `--approve-for-me` flag (v0.147.0, Aug 2026) replaces manual
  approval with a separate reviewer agent at the sandbox boundary. The reviewer code and policy
  are open-source in-tree (`codex-rs/core/src/guardian/` — review.rs/prompt.rs/decision.rs +
  policy.md); what is closed is the reviewer **model** — OpenAI-hosted, never local, and it only
  runs on OpenAI models. PromptArmor demonstrated bypasses of the auto-review agent
  (approving malicious npm installs via indirect prompt injection) — the pattern is validated by
  both majors but breakable, which is why Fontaine's guardian ships with a public eval scorecard
  (see `testing.md` Testing Strategy).

### Goose (Block / AAIF) — model-agnostic Rust precedent

- Rust workspace, 15+ LLM providers behind its own provider trait (not a third-party framework),
  MCP extensions via the Rust SDK, `reqwest` + `rustls`, `goose serve` server + CLI + desktop surfaces.
- Confirms: the older production harnesses (codex-rs, goose) each hand-rolled a
  thin provider layer over `reqwest` — but they predate rig's 0.4x decomposition into harness-grade
  crates (agent runtime with hooks, memory/compaction, coding tools, MCP bridge, cassette replay).
  Fontaine reuses rig for that layer instead of hand-rolling it (see master Trade-offs); goose remains the
  reference for config UX and multi-surface shape.

### Guardian-agent research

- **GuardAgent** (Xiang et al., arXiv:2406.09187, ICML 2025): first "guard agent" paper — an LLM
  guards a target agent by checking actions against safety guard requests; 98%/83% guardrail
  accuracy on two benchmarks. Validates the core concept.
- **Dual-LLM pattern** (Willison 2023) / **CaMeL** (DeepMind, arXiv:2503.18813): privileged LLM
  plans; quarantined LLM touches untrusted data; controller enforces that tainted content never
  flows back unfiltered. Key insight adopted here: **the guardian is the quarantined-side
  component** — its exposure to untrusted material is the tool args echoing file contents (tool
  *results* go to the separate result scanner and are never guardian input), so it must have **no
  tools of its own**, must emit **structured output only** (a fixed
  verdict schema can't carry an injection forward), and its verdicts are validated by the
  controller (regular code) before acting.
- **AgentDojo** (Debenedetti, Zhang, Balunović, Beurer-Kellner, Fischer & Tramèr, ETH Zürich;
  arXiv:2406.13352, NeurIPS 2024 Datasets & Benchmarks) and follow-ups: prompt-injection detectors transfer poorly between
  benchmarks; specialized small detectors (e.g. Prompt Guard 2) catch ~0–2% of realistic agent
  injections **beyond explicit jailbreak phrasing** — a cheap first layer, not a judge; on
  AgentDojo's suite PG2 blocks the majority of ASR (see below). Conclusion: a **general reasoning model as guardian with task context** is currently
  the strongest practical approach — and the guardian itself must be treated as an attack surface
  (hence: no tools, structured output, content clearly delimited as untrusted data, fail-closed).
- **OPA for agent tool calling**: policy-as-data approach exists but is rule-based; orthogonal —
  our prose `hard_deny`/`allow` lists fill that niche inside the guardian prompt rather than as an
  external policy engine.
- **Typed decision models as guardrail judges (2026)**: JEV as a judge for agent-trace security
  (arXiv:2609.34862, Wang & Gao, Sep 28 2026) vs four generative judges over 5,219 trajectories —
  benchmark-averaged positive-class F1 77.8 vs 74.1 (best generative: GLM-5.2); leads ATBench500 /
  MCPHunt, trails R-Judge / TraceSafe; median latency 0.99 s, ~$0.000195/valid judgment; authors'
  own framing: "an economical screening signal, with trade-offs in precision and recall" — and
  Jev-Guard safety classification + routing over an editable policy tree (arXiv:2610.18326, Hu et
  al., HKUST/CUHK-Shenzhen et al., Oct 13 2026): over 7,033 cases 93.72% safety accuracy at 58 ms
  mean latency, 93.10% hierarchical routing accuracy, full pipeline ~240 ms; integrated into the
  Hermes harness, its permission gate cut mean review time per task 4.80 s → 1.33 s vs an SFT-LLM
  judge, zero policy violations for either guard. Fast calibrated bounded verdicts beat generative
  judges on average F1 and 4–6× on review latency; the framing that fits our architecture is
  **decision-model SCREEN tier + LLM adjudicator, not replacement**. Caveats cited honestly: Jev is
  hosted and closed-weights; arXiv:2610.04985 ("Hidden Risks of Jev") demonstrates membership /
  internal-knowledge inference against its outputs — a typed decision model is still an attack
  surface, which supports fail-closed + audit + BYO-local posture.

### LlamaFirewall (Meta) — nearest open prior art (full deep-dive: `research-llamafirewall.md`)

- **Positioning**: LlamaFirewall (arXiv:2505.03574, May 2025) is an open (MIT), in-process Python
  guardrail *framework*: a policy engine orchestrating scanners over agent messages/traces —
  PromptGuard 2 (injection classifier), AlignmentCheck (LLM chain-of-thought auditor), CodeShield
  (static analysis), regex/custom scanners. It is the strongest open prior art and the benchmark
  baseline we will be compared against — but it is a *detector library for agent builders*, not a
  *permission engine inside a harness*. Commit activity since ~March 2026 is predominantly bot
  chores (`research-llamafirewall.md` §6 — last functional change Jan 2026, last commit 2026-08-18);
  Meta has **not** declared the project in maintenance mode (no such statement exists) — the
  slow-moving read on classifier updates rests on that commit history, not a Meta declaration.
- **Monitoring vs authorization**: AlignmentCheck answers "has this agent drifted from the user's
  goal?" — a trace **drift monitor** (labeled experimental; defaults to a Together-hosted model).
  Fontaine's guardian answers "may this specific action run, under these rules, in this
  environment?" — a pre-execution **authorization gate** with Allow/Deny/Escalate verdicts, prose
  intent rules, and deny-reason feedback. Different seat; the public will conflate them, so the
  distinction must stay crisp in docs.
- **PromptGuard 2 as the tool-result scanner** (recommended, `scanner = "pg2"` — user decision 2026-10-21; the default is `off`): adopt as the
  recommended scanner for the result-scanning slot (recall @ 1% FPR **97.5% (English)** per both the HF model
  card and the paper's §4.1 table; EN AUC differs — .998 on the card vs .98 in the paper's table;
  0.995 multilingual AUC; 19–92ms) via `candle` (DeBERTa-v2
  sequence classification over the repo's safetensors weights), with the cheap-LLM pass (`llm`) and bring-your-own (`command`) as alternatives; a scanner error fails closed (flagged) with no fallback. Caveat: the
  PromptGuard 2 HF repos are **gated downloads** (`gated: "manual"` — license acceptance required)
  and the weights carry the **Llama 4 Community License**, which **permits commercial use**
  (royalty-free, conditional; extra restrictions only above 700M monthly active users; AUP +
  "Built with Llama" notice). Llama Guard 4 12B is under the same license (not Apache-2.0) and
  too heavy for hot-path scanning anyway. Community mirrors and ONNX ports exist but do not
  change the license; the air-gapped / license-clean story is the BYO-scanner interface.
- **Attribution**: the LlamaFirewall code and its examples are MIT-licensed (examples carry MIT
  headers; no separate license file in `Examples/`; the enclosing PurpleLlama repo carries the
  Llama 3.2 Community License for its model weights, and GitHub reports the repo license as
  NOASSERTION/"Other"); their AlignmentCheck prompts (paper Appendix
  C.6, CC BY 4.0) are reusable, attributed prior art for `guardian_prompt.md`. Cite their
  anti-proprietary-guardrails argument — provider-embedded safety offers "limited visibility,
  auditability, and customizability" (the paper's Introduction §1; §2.4, "Open-Source vs.
  Proprietary Approaches", says "limited visibility and extensibility") — it is our pitch, from
  Meta. Do **not** take a runtime
  dependency on the `llamafirewall` Python package; depend on its models and numbers, not its
  framework.

### DeepSeek Harness (dsh) — DeepSeek's own agent harness (full study: `research-deepseek-harness.md`)

- **What it is**: TypeScript, MIT, developer preview, pinned at `22ddee37`. A Cordis plugin tree
  (agent loop, tools, session log, model adapter are all plugins) that speaks DeepSeek's
  Anthropic-compatible Messages endpoint. It was built, run in Docker against the opencode Go
  gateway, and captured on the wire; Fontaine ran the same tasks on its Chat wire.
- **What lets the model iterate** (the centrepiece): durable goals continued in rounds by a driver
  (`create_goal`, 256-round cap, `blocked` gated behind three consecutive rounds); a repeated-
  identical-call reminder (Claude Code has none, measured); bash results that end in `[exit code: N]`
  with non-zero exits not treated as tool errors; promote-on-timeout to a background job (Claude
  Code does the same, measured); read-before-edit with a version check and remedy text in errors;
  spill of oversized results to a file the model can read back.
- **Not copied**: the default upload of the whole session log (`dsh_session_log`) to the model
  endpoint, a sandbox that labels any "permission denied" as a policy denial, and prompt-only plan
  mode.
- **DeepSeek model support**: the documented 400 for a missing `reasoning_content` did not
  reproduce through the Go gateway on either wire; cache behaviour and the usage fields were
  measured; the rest needs a direct key (#164).

### Ecosystem / dependencies — reuse-first (user requirement)

- **rig** (MIT, v0.45.0, Oct 2026, 0xPlaygrounds) — the Rust LLM framework, now split into exactly
  the crates a harness needs; this is the core reuse target:
  - `rig-core`: provider-neutral messages/completion/tool contracts; **20+ providers** (OpenAI,
    Anthropic, Gemini, Ollama, Bedrock, Vertex, OpenAI-compatible custom endpoints…) behind one
    interface; streaming; a `structured_output` module implementing the **synthetic output tool**
    pattern (works on every tool-capable model — exactly the guardian-verdict fallback we need);
    `catalog` of model facts (limits, pricing, caching); `redact` (secret finding/redaction in
    text the harness logs or displays — reuse for transcripts and guardian inputs); `test_utils`
    for deterministic completion-model tests.
  - `rig-agent`: the agent runtime — `AgentBuilder`, `AgentHook`/`HookContext` (typed hooks around
    agent turns — bookkeeping only; not the gate), `AgentRun` (serializable, sans-I/O state
    machine for model turns + tool recovery + history — session persistence for free), `extractor`
    (typed extraction through a submit-output tool with configurable retries — reuse verbatim for
    parsing guardian `Verdict`s), tool registry with ordered registrations.
  - `rig-memory`: context windows, **compaction**, saving/resuming and time-travel
    (`as_of`/restore).
  - `rig-tools`: ready coding tools (read, write, edit, search, list, shell, delete, move) any rig agent can use; file tools take `confine: bool`, `shell` has `confine` (default false), `max_timeout` and an output `cut`, and `fs::place(base, confine, path)` resolves paths with symlink and `..` canonicalization.
  - `rig-rmcp`: MCP integration for rig agents (native), bridging rmcp into the rig tool registry. `McpClientHandler::new(config, sink)` registers raw `DynamicTool`s into whatever sink it is given, again on every list-changed refresh, so Fontaine supplies a wrapping sink (Phase 5); it enables only rmcp's `client` feature, so transports need a direct, pinned `rmcp` dependency.
  - `rig-cassette`: effect recording/replay and checkpoints — deterministic integration tests of
    full agent runs without live APIs.
  - Caveat: rig is pre-1.0 with breaking changes each release; pin exact versions workspace-wide
    (`rig-core = "=0.45.0"` style) and upgrade deliberately. The bare `rig` crate is a facade that
    is `no_run` in the workspace (never compiled or CI-tested) — depend on `rig-core` directly.
    rig also ships its own Bevy-plugin coding
    agent (`rig-harness`, `rig-tui`) — we do **not** build on that; Fontaine keeps its own ratatui
    TUI and guardian-first architecture.
- **rmcp** — official MCP Rust SDK, v3.6.0 (Oct 2026), tokio-based, spec 2026-07-28; reached
  through `rig-rmcp` so third-party tools plug into the same registry and guardian gate.
- **ratatui + crossterm** — the standard Rust TUI stack (used by codex-rs; also demonstrated with
  rig in JetBrains' "Rust AI in Practice" session — a coding agent built on rig + ratatui).
- **tokio, serde, clap, tracing, toml** — glue. **wiremock** (dev-dependency) — used by the
  per-provider smoke tests and the fail-closed 500 test (see `testing.md` Testing Strategy).
- **Observability:**
  - `tracing =0.1.44` / `tracing-subscriber =0.3.23` (env-filter, fmt).
  - micromegas backend: `micromegas-tracing` (`tokio` feature, for
    `TracingRuntimeExt`) + `micromegas-telemetry-sink`, on a git revision (see the caveats) until an
    exact crates.io `=0.33.x` pin replaces it.
  - OTLP backend: `opentelemetry =0.33.1`, `opentelemetry_sdk =0.33.1`,
    `opentelemetry-otlp =0.33.1` (`default-features = false`, `http-proto`, `reqwest-blocking-client`;
    shares rig's reqwest 0.13), `tracing-opentelemetry =0.34.0` and
    `opentelemetry-appender-tracing =0.33.1`.
  - Both backends are compiled in by default, behind the bin crate's features
    `telemetry-micromegas` and `telemetry-otlp`; pinned exactly, like rig, because they are pre-1.0.
  - Caveats: the pins are a git revision of micromegas `main` (`7a08f6541`, 0.33.0, unreleased)
    until 0.33 is on crates.io (the `release-blocker` issue #141). That revision has the builder knob
    for span/thread streams (`with_cpu_tracing_enabled`, which replaced the
    `MICROMEGAS_ENABLE_CPU_TRACING` environment variable), reqwest 0.13 and `tracing` span capture
    with parentage in `TracingCaptureLayer`. The bridge's span half stays until that capture has a
    field hook and a location switch (madesroches/micromegas#1725).
