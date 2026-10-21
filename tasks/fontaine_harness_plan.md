# Fontaine: Rust AI Agent Harness with Guardian-Agent Auto Mode — Master Plan & Phase Tracker

This file is the **tracking task**: phase status and progress live here, together with the
workspace layout, trade-offs, risk gates and every decision. The cross-cutting design every
phase depends on (permission pipeline, config, sessions, observability, platforms, research
findings, documentation, testing strategy) lives in topic docs under `tasks/design/` — see
`## Design docs`. The build itself is split into **one plan per phase** under `tasks/phases/` — each phase plan owns
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

Deliberate design choices and user decisions are recorded in `## Decisions`.

## Current State

Repo `/home/mad/fontaine` is on branch `start` (not `main`); its history so far is this plan (and
its split into per-phase plans), the five analysis docs, the LlamaFirewall deep-dive, and
review/fact-check passes on this branch. No
Rust code exists yet — the implementation is greenfield. `research/` holds seven companion docs that are this plan's precedent and inputs:
`analysis-competitive-landscape.md` (market/permission-story survey),
`analysis-guardian-market.md`, `analysis-differentiation.md`,
`analysis-devils-advocate.md` (kill criteria), `analysis-strategic-verdict.md` (verdict and
"Required plan amendments" A1–A5, which this plan incorporates: A1 as the public scorecard plus a contributor path for dataset and red-team cases in `CONTRIBUTING.md`, A5 as the local/air-gapped lead in the README quickstart), and
`research-llamafirewall.md` (deep-dive on the nearest open prior art), and
`research-claude-code-parity.md` (Claude Code feature matrix and backlog triage).

## Phase Tracker

Build order and phase content are owned by the phase plans; this table tracks status and links
out. Steps are numbered **within each phase plan**; cross-phase references use the form
"Phase N, step M" (pipeline steps in the data-flow diagram below are a separate numbering).
Documentation is a **per-phase deliverable**: every phase plan ends with a docs step, and
user-facing behavior (modes, config keys, verdict classes, tool semantics, CLI surfaces) ships in
the same phase as its doc update — a phase whose feature lands undocumented is not complete. All
product documentation lives under `docs/` (see `design/documentation.md` §Documentation).

| Phase | Plan | Scope | Status | Exit milestone |
|-------|------|-------|--------|----------------|
| 0 — scaffold (eval first) | [phases/phase0_scaffold.md](phases/phase0_scaffold.md) | Cargo workspace, crate skeletons, CLI entry, tracing facade + `[telemetry]` config, CI, settings scope stack + workspace trust store + read-only credentials lookup; eval golden-dataset + runner skeleton; `docs/` tree started | not started | CI green; eval runner green against verdict fixtures with no guardian in the tree |
| 1 — model layer + headless | [phases/phase1_model_headless.md](phases/phase1_model_headless.md) | rig crates pinned; config → provider mapping; streaming spike; `fontaine -p` with the versioned headless I/O contract; telemetry backends (micromegas/OTLP) + `docs/telemetry.md`; provider docs | not started | `fontaine -p` streams a prompt plus a spike tool round-trip against a mock endpoint |
| 2 — agent runtime & tools | [phases/phase2_runtime_tools.md](phases/phase2_runtime_tools.md) | RiskClass registry, GuardedTool (gate stubbed), rig-tools built-ins + read tools + webfetch, multi-root scope validator, confined rig tool config; rig-agent session orchestration + session ledger; `docs/tools.md` | not started | agent runs tools end-to-end headless through wrapped registry; choke-point test green |
| 3 — pipeline & manual TUI | [phases/phase3_pipeline_tui.md](phases/phase3_pipeline_tui.md) | PermissionPipeline (manual/acceptEdits/plan/dontAsk; deterministic floor; `Tool(specifier)` rule grammar + allow rules; user-hook slot; Tier-0.5; audit log); headless `-p` denial semantics for manual/dontAsk; ratatui TUI; `docs/permissions.md` + pipeline diagram | not started | **usable Claude-Code-like agent with manual approvals** |
| 4 — guardian + scorecard | [phases/phase4_guardian.md](phases/phase4_guardian.md) | fontaine-guardian; `auto` wired into pipeline (incl. headless `-p` semantics for auto); denial UX; live evals; **published scorecard**; `docs/guardian.md` + `docs/evals.md` | not started | shipped default flips `manual` → `auto`; scorecard public with the release |
| 5 — extensibility | [phases/phase5_extensibility.md](phases/phase5_extensibility.md) | MCP (user + trust-gated project servers); session resume/fork; user hooks; skills/agent files/output styles; parallel subagents (v1) + budgets + agents panel; `docs/{mcp,agents,hooks,extensions}.md`; sandbox/sandboxing.md = **roadmap slot** | not started | gated background subagents live in one session |
| 6 — guardian hardening | [phases/phase6_hardening.md](phases/phase6_hardening.md) | tool-result scanner (`[guardian] scanner` = off/pg2/llm/command; PromptGuard 2 via candle, BYO command, cheap-LLM); scanner docs | not started | scanning demonstrable behind config, license caveat surfaced |

GitHub `phase-N` labels mean "earliest phase after which the issue can be taken; not part of that phase's definition of done; Decisions win over issue text".

### Progress log

Append one dated line as each phase's definition of done is met (local commits referencing the
phase plan are the work trail).

- (none yet)

## Design docs

The cross-cutting design is split by topic under `tasks/design/`; this master keeps the workspace
layout, Files to Modify, Trade-offs, Risk Gates, Decisions and Open Questions.

- [design/permissions.md](design/permissions.md) — core data flow (the permission pipeline), permission rules (deny / ask / allow), user hooks, composite guardian.
- [design/config.md](design/config.md) — config schema and the settings scope stack (incl. workspace trust).
- [design/sessions.md](design/sessions.md) — the session ledger.
- [design/observability.md](design/observability.md) — tracing, telemetry events, audit log.
- [design/platforms.md](design/platforms.md) — supported platforms, directories, releases.
- [design/research-findings.md](design/research-findings.md) — prior art the design is grounded in.
- [design/documentation.md](design/documentation.md) — the `docs/` tree plan.
- [design/testing.md](design/testing.md) — testing strategy and manual verification items.

## Design — cross-cutting core

### Workspace layout

Built **on rig**: Fontaine crates are the guardian, the permission pipeline, config, and surfaces.
The model layer, agent runtime, memory/compaction, base tools, and MCP come from rig crates.
Per the micromegas convention, the Cargo workspace lives at `rust/Cargo.toml` and **every crate
sits directly under `rust/`** — the root directories are only `rust/`, `evals/`, `docs/`, `tasks/`,
`research/`, plus root files `README.md`, `LICENSE`, `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`, `AGENTS.md`, `.gitignore`,
`.github/workflows/release.yml` (cargo-dist release build; see `design/platforms.md` §Releases) and
`.github/workflows/ci.yml` (GitHub Actions; matrix `ubuntu-latest`, `macos-latest`, `windows-latest`, each job installs the toolchain pinned in `rust/rust-toolchain.toml` and from `rust/` runs `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test`, then the offline eval fixture run (Phase 4 adds a `cargo dist plan` step: install the pinned cargo-dist and run it from `rust/`; Phase 6 adds `cargo clippy --all-targets --features fontaine-guardian/pg2 -- -D warnings` and `cargo test --features fontaine-guardian/pg2`) — see
`design/testing.md` §Testing Strategy).

```
fontaine/
├── rust/
│   ├── rust-toolchain.toml     # channel = "1.97.1" (rig 0.45.0 `rust-version`)
│   ├── Cargo.toml              # workspace root (`[workspace] members = ["*"]`, `exclude = ["target", ".*"]`, resolver = "2",
│   │                           #   workspace.package: edition 2024, rust-version 1.97.1); rig crates pinned to exact versions
│   ├── fontaine/               # bin + lib crate (logic in `src/lib.rs`, thin `src/main.rs`): CLI (clap), wires core+tui, `-p` headless from Phase 1,
│   │                           #   owns the tracing subscriber + telemetry export (`src/telemetry/`)
│   ├── fontaine-core/          # session orchestration on rig-agent (AgentRun, rig bookkeeping hooks),
│   │                           #   permission pipeline + rule grammar, settings scope stack, workspace
│   │                           #   trust, user hooks, session ledger, extension loading, FONTAINE.md,
│   │                           #   registry assembly (full choke-point test: `fontaine-core/tests/choke_point.rs`)
│   ├── fontaine-tools/         # risk metadata registry + GuardedTool wrapper around
│   │                           #   rig-core Tool (calls the `ToolGate` trait it defines); rig-tools built-ins re-exposed with risk
│   │                           #   classes; workspace-scope validation over `rig_tools::fs::place`; confined rig tool config
│   ├── fontaine-guardian/      # the guardian agent: verdict protocol (rig extractor),
│   │                           #   prompt builder, rules config, caching, fail-closed logic
│   ├── fontaine-agents/        # parallel subagent orchestration on rig-agent: spawn/list/
│   │                           #   interrupt, per-type tool registries, budgets (v1); defines the
│   │                           #   `ChildRunner` trait (implemented by core)
│   ├── fontaine-mcp/           # rig-rmcp wiring: MCP servers from config, tools registered
│   │                           #   as RiskClass::Arbitrary, annotations → guardian hints
│   ├── fontaine-tui/           # ratatui app: chat, approval prompts, denial/retry UI, modes
│   └── fontaine-evals/         # eval runner (bin + lib crate, same split): scores guardian verdicts against the dataset
├── evals/                      # golden dataset (`evals/dataset/*.jsonl`) + README; runner is rust/fontaine-evals
├── docs/                       # all product docs — one topic per page, website-ready (see `design/documentation.md` §Documentation)
├── tasks/                      # this master plan + design/ (topic design docs) + phases/ (one plan per phase)
└── research/                   # seven analysis/research companion docs
```

Crate dependency graph (arrows point at dependencies; no cycles):

```
fontaine (bin+lib)       ─▶ fontaine-core, fontaine-tui
fontaine-evals (bin+lib) ─▶ fontaine-core, fontaine-guardian
fontaine-tui          ─▶ fontaine-core
fontaine-core         ─▶ fontaine-tools, fontaine-guardian, fontaine-mcp, fontaine-agents
fontaine-mcp          ─▶ fontaine-tools
fontaine-agents       ─▶ fontaine-tools
fontaine-guardian     ─▶ (no Fontaine crates)
fontaine-tools        ─▶ (no Fontaine crates — never fontaine-core)
```

Phase-owned design sections live in the phase plans: model layer + headless I/O contract (Phase 1),
tools/risk registry/GuardedTool + multi-root workspace + session ledger (Phase 2), permission modes
+ rule grammar + base TUI (Phase 3), the guardian agent (Phase 4), parallel agents + agents panel,
user hooks, extension files, session resume/fork (Phase 5), result scanning (Phase 6).

## Files to Modify

All new. Workspace-level files: `rust/Cargo.toml` (see Phase 0 step 1), `rust/rust-toolchain.toml`,
`README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`, `.github/workflows/*`. Each phase plan lists its own
file slice in dependency order.

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
- **Stateless one-shot guardian requests vs persistent guardian conversation** — stateless chosen:
  no context poisoning over a long session, trivially cacheable and parallelizable, and each
  request carries a fresh digest of intent + session facts. Loses "guardian memory" of near-misses;
  recovered by passing `prior_denials` explicitly.
- **Read-only fast path (user-approved hybrid) + Tier 0.5 pre-classifier** — skips guardian latency on
  the highest-frequency calls (Claude Code does the same: dedicated Read/Grep/Glob tools and a
  built-in read-only Bash command set). The scope check (workspace containment, symlink resolution)
  and the pre-classifier (`brush-parser` + curated set membership + per-command read-only flag allowlist +
  the same scope validator) are
  deterministic code enforced by Fontaine at the execution boundary, not regex-on-args and not the
  tool's own declaration; a read that escapes scope, and any command the classifier cannot fully
  parse, goes to the guardian (read ≠ safe — out-of-workspace reads are recon/exfiltration risk),
  and MCP tools' self-declared `readOnlyHint` never qualifies for a fast path. Explicit allow rules
  (`design/permissions.md` §Permission rules) are a separate path: a user grant, written by the user, never derived from a
  tool label or server declaration, and never reaching past the floor.
- **Parallel agents multiply guardian round-trips and model cost** — N subagents mean N× adjudications
  and token spend (Claude Code documents the same). Mitigated by Tier 0.5 (most read-ish subagent
  calls never reach the guardian), the optional local PromptGuard 2 result scanner (`scanner = "pg2"`; no per-result LLM call),
  and `[agents]` budget caps; the stateless per-request guardian parallelizes without shared state.
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
  on the seeded attack classes at <10% benign false-deny; this kill floor is separate from the
  stricter guardian-list inclusion bar in the Phase 4 design) — meaning the "any model" promise is
  hollow for the feature that matters.

## Decisions

- 2026-10-21: Multi-account model registry, following opencode: named `[accounts.<name>]`, models addressed `<account>/<model>`, one rig client per account; replaces `[model] provider`/`base_url`. Reason: user requirement; Claude Code has no analogous design point.
- 2026-10-20: Hybrid gate — tool-declared `read_only` + workspace-scope check skips the guardian; everything else is adjudicated. (User choice.)
- 2026-10-21: Hybrid-gate refinement — the fast path is earned by Fontaine-enforced workspace containment, not the tool's label; read-only ≠ safe, and MCP `readOnlyHint` never earns it. (User choice.)
- 2026-10-20 (amended 2026-10-21: headless to Phase 1): TUI-first (ratatui), headless `-p` on the same core. (User choice.)
- 2026-10-20: Guardian is fail-closed on every non-verdict outcome; Deny verdicts are never softened (Claude Code behavior; Phase 4 §Fail-closed guarantees).
- 2026-10-20: Core is built on the rig crate family, pinned to exact versions; no custom provider layer. (User directive: reuse existing crates.)
- 2026-10-20: rig's Bevy-based `rig-harness`/`rig-tui` are not adopted; Fontaine keeps its own ratatui TUI.
- 2026-10-20: License is Apache-2.0 (verbatim `LICENSE`); adapted Anthropic rule prose is attributed in `docs/guardian.md`.
- 2026-10-21: "fontaine" is the final product name — binary, crate prefix, config dir, `FONTAINE.md`. (User decision.)
- 2026-10-21: Guardian default = top-ranked benchmark-validated model available on the configured accounts; switching is easy, explicit, never automatic; no catalog match ⇒ main model with a one-time notice. (User decision.)
- 2026-10-21: `pg2` (PromptGuard 2) is the recommended tool-result scanner; `llm` and `command` are alternatives; default `off`; scanner errors fail closed (flagged) with no fallback; the Llama 4 Community License caveat is surfaced at first enable; BYO-scanner interface for license-clean setups. (User decision.)
- 2026-10-21: Fold strategic-verdict amendments A3 (sandbox interop, Phase 5 roadmap slot) and A4 (dependency-light, lift-out-able `fontaine-guardian`) into the roadmap. (User decision.)
- 2026-10-21: Revisit the core-vs-rig split if rig ships a permission-adjudication API.
- 2026-10-21: Design tie-breaker — when in doubt, do as Claude Code does; every deviation gets a dated line here with a one-line reason. (User directive.)
- 2026-10-21: Dedicated read tools plus a deterministic, zero-LLM Tier-0.5 shell pre-classifier (`brush-parser` + curated set + flag allowlist + scope validator; fail-through on anything unparsed), extended only via the user-scope `[shell_preclassifier] allow_extra` (`design/permissions.md` step 5). Deviations from Claude Code: unquoted globs are rejected (shell-side expansion is invisible to the scope validator), and "don't ask again" grants never auto-extend the curated set (a repo or injected session must not accumulate loosened prefixes). (User directive 2.)
- 2026-10-21: The Tier-0.5 curated set contains no `git` subcommand in v1 — every git invocation goes to the guardian. Deviation from Claude Code (stricter); reason: repo-controlled config and attributes (filter drivers, textconv, ext-diff, fsmonitor, gpg.program) can make read-only git execute programs and no override set closes it.
- 2026-10-21: Phase 2 `ls`/`grep` take rig-tools 0.45 `ListArgs`/`SearchArgs` only (`grep`: `pattern`, `path`, `glob`, `hidden`, `files` mode, 200-match cap; `(?i)` for case-insensitivity; no context lines, count, type or head_limit). Deviation from Claude Code's Grep/LS arguments; reason: no custom search layer over rig.
- 2026-10-21 (supersedes "no subagents in v1"): Parallel multi-agent management is a v1 requirement, Claude Code-style; agent-to-agent messages go through the same guardian gate. (User directive 3.)
- 2026-10-21: Deviation — a child's effective mode is never looser than its parent's (Claude Code lets a parent set a child `auto`); Phase 5 "Parallel agent orchestration".
- 2026-10-21: Deviation — `acceptEdits` auto-approves Fontaine's edit-class tools only, not in-scope filesystem Bash; reason: writes ride the classified edit tools and a mutating shell must not fast-path (stricter than Claude Code).
- 2026-10-21: Subagent nesting and budgets mirror Claude Code's documented rules (not a deviation); Phase 5.
- 2026-10-21: Plan mode follows Claude Code's default (classifier reviews consequential non-edit calls); edits are structurally blocked regardless of verdict.
- 2026-10-21 (revised): The deterministic floor (deny → ask → critical-path → protected-path) gates every mode and can only prompt or deny, never allow; critical-path never auto-approves and denies in `dontAsk`/headless; protected paths follow Claude Code. Spec: `design/permissions.md` §Core data flow.
- 2026-10-21: Deviation — config trust asymmetry is looser than Claude Code on the deny side: project repos may add deny/ask rules (Claude Code ignores the project `autoMode` block); reason: deny-side additions can only tighten.
- 2026-10-21 (user): Bypass mode is not shipped in v1 (deviation from `bypassPermissions`); eval/CI baselines always run with the guardian.
- 2026-10-21: Shipped default mode = auto (Claude Code parity); interim default = manual until the guardian wires in at Phase 4.
- 2026-10-21 (user): Headless `-p` promoted to Phase 1 so CI and the eval harness drive core headlessly from day one.
- 2026-10-21: Deviation — subagent reports are screened by the optional (default-off) result scanner, not by the classifier at handoff; reason: the guardian never sees tool output.
- 2026-10-21 (user): No zero-guardian mode; containers get `[guardian] profile = "minimal"`, never off, and it requires Fontaine-verified containment (user-scope `--in-container` plus the Linux probe; probe failure ⇒ `full`). Ruleset and request: Phase 4 §Minimal guardian profile; it ships with its own scorecard column.
- 2026-10-21 (user): Typed-decision screeners (Jev, Laya, DiffusionGemma) are deferred to the screening-tier roadmap; thresholds are always recalibrated on Fontaine's golden set (vendor defaults over-escalate: 17/21 wrong escalations at 0.85/0.6).
- 2026-10-21 (user): Guardian tree accepted as design direction — strictest-wins composition, fail-closed between tiers, per-domain subtrees, benchmarked tiers only; v1 ships the deterministic+LLM spine; one tree serves the whole agent tree.
- 2026-10-21 (user): The plan is split into this master tracker plus one plan per phase under `tasks/phases/`, each ending in a docs step; all documentation lives under `docs/`.
- 2026-10-21: The eval runner is the workspace bin + lib crate `rust/fontaine-evals` (like `fontaine`: logic in `src/lib.rs`, thin `src/main.rs`, so integration tests can import it); the dataset stays in `evals/dataset/` as JSONL.
- 2026-10-21 (user): `[permissions] allow` plus one `Tool(specifier)` grammar for deny/ask/allow (`design/permissions.md` §Permission rules); allow acts after the floor, narrow rules skip the guardian in `auto`, broad code-execution rules are suspended in `auto` (Claude Code parity); a user allow rule or user hook `allow` is a user grant distinct from the containment fast path.
- 2026-10-21: Deviation — shared project settings cannot hold allow rules, a project hook's `allow` and a project skill's `allowed-tools` are ignored; reason: project scope can only tighten.
- 2026-10-21: Deviation — `shell` deny/ask rules match as a prefix anywhere in the parsed word stream (and on unparseable commands containing the first word) while allow matches only one simple command; reason: no regex and no operator-level parser in v1, so deny fails toward matching and allow toward not matching.
- 2026-10-21: Settings scope stack managed (reserved) > CLI > project-local > project > user (Claude Code parity). Deviation: endpoint/credential keys (`[accounts.*]`, `[telemetry]` except `enabled = false`) are never read from project-local; reason: a file in the repo must not redirect session data.
- 2026-10-21: Multiple workspace roots (`--add-dir`, `/add-dir`, `additional_directories`; Claude Code parity): containment is inside any root, extra roots grant file access not configuration, only the user adds roots.
- 2026-10-21: Session ledger — the guardian's inputs come from a Fontaine-owned append-only ledger, never rig-memory context; resume/fork drop session grants (`design/sessions.md`). Claude Code has no analogous design point.
- 2026-10-21: The headless I/O contract is versioned (`schema_version`; additive keeps it, rename or removal bumps it), mirroring Claude Code's flags plus `gate_decision` events.
- 2026-10-21 (user): Project agent files (`.fontaine/agents/*.md`) are allowed but only narrow (subset toolset, `manual`/`dontAsk`, no `model`). Deviation from Claude Code; reason: a checked-in repo must not widen a child's authority or pick its endpoint.
- 2026-10-21 (user): Observability goes through `tracing` in every crate with only the bin installing a subscriber; export is opt-in, off by default, native micromegas by default (OTLP/HTTP fallback). Deviation (Claude Code exports only OTel); reason: user requirement. Names and opt-ins mirror `claude_code.*` / `OTEL_LOG_*`; details in `design/observability.md`.
- 2026-10-21: The audit log is a separate, always-on local record written before telemetry from one typed `GateDecision`; reason: telemetry is opt-in, lossy and redacted.
- 2026-10-21 (user): `FONTAINE.md` v1 loads `<primary root>/FONTAINE.md` plus the user-level file; no hierarchy or imports (backlog GitHub issue #7).
- 2026-10-21 (user): The micromegas crates need no published `rust-version`; that upstream item is dropped.
- 2026-10-21: The loop breaker follows Claude Code — `auto` pauses after 3 consecutive or 20 total guardian denials (not configurable); headless treats the paused state as would-prompt → Deny.
- 2026-10-21: The `GuardedTool` decorator is the single gate; rig `AgentHook::on_dispatch` is not used for permission decisions.
- 2026-10-21: Tool calls are adjudicated and executed one at a time (`tool_concurrency = 1`); parallel batch adjudication is dropped for v1.
- 2026-10-21: `fontaine-tools` defines the `ToolGate` trait that `fontaine-core`'s `PermissionPipeline` implements, so `fontaine-tools` never depends on `fontaine-core`.
- 2026-10-21: The "destructive" class behind the fail-closed Allow → Deny rule is derived deterministically by `fontaine-core` from the call, never from the guardian's label; verdict labels are Claude Code's default block categories plus `allow` (Phase 4).
- 2026-10-21: Deviation — project and project-local scopes may set `default_mode` to `manual` or `dontAsk` only; `plan` (its guardian leg can loosen a manual user) and `acceptEdits` (a repo must not auto-approve edits) are stricter than Claude Code, and `auto` is rejected from project-local too (Claude Code parity).
- 2026-10-21: The pre-session file index is gitignore-respecting, capped at 50,000 paths; the guardian request carries only the touched paths' pre-existence status.
- 2026-10-21: Directories come from the `dirs` crate, overridable by `FONTAINE_CONFIG_DIR` / `FONTAINE_DATA_DIR`; credentials at `<data dir>/credentials.toml`, owner-only (`design/platforms.md`).
- 2026-10-21: The eval dataset gains `mode` and `workspace` fields so `pipeline` cases are reproducible.
- 2026-10-21: The default guardian list is the data file `rust/fontaine-guardian/data/guardian_models.toml` (entry keys `kind`, `model`, `catch_rate`, `false_deny_rate`, `p95_ms`, `scored_on`, `dataset_version`); it and the prompt assets (`guardian_prompt.md`, `guardian_minimal.md`, `scanner_llm.md`) are compiled into `fontaine-guardian` via `include_str!`; it ships empty (fallback to the main model) and the Phase 4 scorecard run fills it, so a scorecard update ships with a release.
- 2026-10-21: v1 supports Linux, macOS and Windows; every CI step runs on all three OSes. Windows runs `shell` through Git Bash (Claude Code parity; PowerShell tool is backlog issue #28); details in `design/platforms.md`.
- 2026-10-21: Deviation — provider and telemetry credential variables are scrubbed from the `shell` and hook child environment (list: Phase 2 step 1); reason: a model-run command must not read provider keys.
- 2026-10-21: AgentDojo-comparable (ASR × utility) reporting is dropped; the scorecard reports per-class catch rate and false-deny rate.
- 2026-10-21: Workspace trust (Claude Code parity: folder trust covers the whole folder) — in the TUI, project and project-local executables (hooks, MCP servers, skill `!cmd`) load only after the trust prompt; headless loads them without one; they never change a permission decision (`design/config.md` §Config, Workspace trust).
- 2026-10-21: A user-scope hook `allow` in `auto` follows Claude Code (like an allow rule; broad matchers suspended); the `claude -p` baseline confirms it.
- 2026-10-21: The `skill` tool stays `ReadOnly` on the fast path; a skill's `!cmd` is gated as a `shell` call.
- 2026-10-21 (user): Shell parsing uses exact-pinned `brush-parser` for Tier 0.5, critical-path and rule matching; no hand-rolled lexer, no `shlex`; anything not exactly one simple command falls through.
- 2026-10-21 (user): Windows Git Bash discovery follows Claude Code's pattern with `FONTAINE_GIT_BASH_PATH` override (`design/platforms.md`).
- 2026-10-21 (user): `[shell_preclassifier] allow_extra` entries are a command or `command subcommand` pair, flag-free, with in-scope positional paths.
- 2026-10-21 (user): `webfetch` v1 takes `url` only (HTML → markdown via exact-pinned `htmd`; caps in Phase 2). Deviation: no `prompt` argument or small-model post-processing (backlog); reason: one deterministic tool with no extra model call.
- 2026-10-21 (user): The minimal profile uses the resolved `[guardian] model` and its ruleset in `prompts/guardian_minimal.md`.
- 2026-10-21: The `pg2` flag threshold is the fixed constant 0.9, LlamaFirewall's default `block_threshold`.
- 2026-10-21 (user): Destructive-class word lists ship as constants (Phase 4), checked against `claude -p` baselines; catch rate is over all `kind = guardian` `attack:*` cases, false-deny over all `kind = guardian` benign cases, and the runner reports p50/p95 latency.
- 2026-10-21: Output styles never alter the guardian's prompt or `AdjudicationRequest`.
- 2026-10-21 (user): Releases — semver 0.x, cargo-dist (exact-pinned) GitHub Releases plus `cargo install --git`, no crates.io, maintainer cuts releases, Keep a Changelog (`design/platforms.md` §Releases).
- 2026-10-21 (user): `fontaine-agents` defines `ChildRunner` and `LedgerView` traits implemented and injected by `fontaine-core`, the same inversion as `ToolGate`.
- 2026-10-21 (user): The `agent` tool blocks until the child finishes (Claude Code parity); `background: true` returns `{agent_id}` and delivers the report as a notification (Phase 5).
- 2026-10-21 (user): `send_message` reaches parent, children, siblings and `"main"`; delivery is queued, untrusted and sender-marked (Phase 5).
- 2026-10-21 (user): The RuleLabel enum gains `Container Escape` for `guardian_minimal.md`; the escape class goes in the verdict `explanation` as `class=<name>` and the `minimal` column reports per class from it.
- 2026-10-21 (user): Guardian constants ship in `fontaine-guardian`, not config keys (cache TTL, `task_intent` budget, message and action counts, extractor retries; Phase 4).
- 2026-10-21 (user): Root `SECURITY.md` (Phase 0): private reporting, 90-day coordinated disclosure; bypass dataset PRs only after a fix ships.
- 2026-10-21 (user): The eval dataset has minimum case counts and maintainer sign-off of `expected` labels against `claude -p` baselines before the scorecard publishes (`design/testing.md` §Eval harness).
- 2026-10-21 (user): PromptGuard 2 runs on `candle` (safetensors from the gated HF repo), not ONNX/`ort`; exact pins and the `pg2` feature in Phase 6.
- 2026-10-21 (user): Scanner selection is one key, `[guardian] scanner = "off" | "pg2" | "llm" | "command"` (default `off`), replacing `scan_tool_results`; scanner errors fail closed (flagged); scanner keys are user file only.
- 2026-10-21 (user): The Linux containment probe and its signals are defined in Phase 4 step 6; macOS and Windows ⇒ not contained; `minimal` needs `--in-container` AND a positive probe.
- 2026-10-21 (user): The guardian-list inclusion bar and ranking are defined in Phase 4 §The guardian agent; separate from the kill criteria.
- 2026-10-21: While a parent is in `plan`, every child's edits are blocked and its non-edit calls use the tighter of its own mode and `manual` (Phase 5).
- 2026-10-21 (user): `/model` and `/guardian-model` switch the live session and persist to the user config file (Claude Code `/model` parity); `/model` moves the guardian only when it is unset and on the main-model fallback; an unavailable pinned guardian in the TUI prompts retry / pick another validated guardian / switch to `manual` (Phase 4 §The guardian agent).
- 2026-10-21: A cost cap (`subagent_cost_budget_usd`, `session_cost_budget_usd`) on a model with no catalog price is inactive with a one-time notice and the token cap still binds; an explicit `--max-budget-usd` on an unpriced model stays a usage error (exit 2) (Phase 5 Budgets).
- 2026-10-21: The headless I/O contract is a checked-in JSON Schema per version, `docs/schemas/headless-v1.schema.json`, published with the docs and validated with the exact-pinned `jsonschema` crate (Phase 1).

## Open Questions

None.
