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
Rust code exists yet — the implementation is greenfield. `research/` holds six companion docs that are this plan's precedent and inputs:
`analysis-competitive-landscape.md` (market/permission-story survey),
`analysis-guardian-market.md`, `analysis-differentiation.md`,
`analysis-devils-advocate.md` (kill criteria), `analysis-strategic-verdict.md` (verdict and
"Required plan amendments" A1–A5, which this plan incorporates: A1 as the public scorecard plus a contributor path for dataset and red-team cases in `CONTRIBUTING.md`, A5 as the local/air-gapped lead in the README quickstart), and
`research-llamafirewall.md` (deep-dive on the nearest open prior art).

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
| 0 — scaffold (eval first) | [phases/phase0_scaffold.md](phases/phase0_scaffold.md) | Cargo workspace, crate skeletons, CLI entry, tracing facade + `[telemetry]` config, CI, settings scope stack + workspace trust store; eval golden-dataset + runner skeleton; `docs/` tree started | not started | CI green; eval runner green against verdict fixtures with no guardian in the tree |
| 1 — model layer + headless | [phases/phase1_model_headless.md](phases/phase1_model_headless.md) | rig crates pinned; config → provider mapping; streaming spike; `fontaine -p` with the versioned headless I/O contract; telemetry backends (micromegas/OTLP) + `docs/telemetry.md`; provider docs | not started | `fontaine -p` streams a prompt plus a spike tool round-trip against a mock endpoint |
| 2 — agent runtime & tools | [phases/phase2_runtime_tools.md](phases/phase2_runtime_tools.md) | RiskClass registry, GuardedTool (gate stubbed), rig-tools built-ins + read tools + webfetch, multi-root scope validator, confined rig tool config; rig-agent session orchestration + session ledger; `docs/tools.md` | not started | agent runs tools end-to-end headless through wrapped registry; choke-point test green |
| 3 — pipeline & manual TUI | [phases/phase3_pipeline_tui.md](phases/phase3_pipeline_tui.md) | PermissionPipeline (manual/acceptEdits/plan/dontAsk; deterministic floor; `Tool(specifier)` rule grammar + allow rules; user-hook slot; Tier-0.5; audit log); headless `-p` denial semantics for manual/dontAsk; ratatui TUI; `docs/permissions.md` + pipeline diagram | not started | **usable Claude-Code-like agent with manual approvals** |
| 4 — guardian + scorecard | [phases/phase4_guardian.md](phases/phase4_guardian.md) | fontaine-guardian; `auto` wired into pipeline (incl. headless `-p` semantics for auto); denial UX; live evals; **published scorecard**; `docs/guardian.md` + `docs/evals.md` | not started | shipped default flips `manual` → `auto`; scorecard public with the release |
| 5 — extensibility | [phases/phase5_extensibility.md](phases/phase5_extensibility.md) | MCP (user + trust-gated project servers); session resume/fork; user hooks; skills/agent files/output styles; parallel subagents (v1) + budgets + agents panel; `docs/{mcp,agents,hooks,extensions}.md`; sandbox/sandboxing.md = **roadmap slot** | not started | gated background subagents live in one session |
| 6 — guardian hardening | [phases/phase6_hardening.md](phases/phase6_hardening.md) | tool-result scanner (PromptGuard 2 ONNX default, BYO interface, cheap-LLM fallback); scanner docs | not started | scanning demonstrable behind config, license caveat surfaced |

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
`research/`, plus root files `README.md`, `LICENSE`, `CHANGELOG.md`, `CONTRIBUTING.md`, `AGENTS.md`,
`.github/workflows/release.yml` (cargo-dist release build; see `design/platforms.md` §Releases) and
`.github/workflows/ci.yml` (GitHub Actions; matrix `ubuntu-latest`, `macos-latest`, `windows-latest`, each job installs the toolchain pinned in `rust/rust-toolchain.toml` and from `rust/` runs `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test`, then the offline eval fixture run — see
`design/testing.md` §Testing Strategy).

```
fontaine/
├── rust/
│   ├── rust-toolchain.toml     # channel ≥ 1.97.1 (rig 0.45.0 `rust-version`)
│   ├── Cargo.toml              # workspace root (`[workspace] members = ["*"]`, `exclude = ["target", ".*"]`, resolver = "2",
│   │                           #   workspace.package: edition 2024, rust-version 1.97.1); rig crates pinned to exact versions
│   ├── fontaine/               # thin bin crate: CLI (clap), wires core+tui, `-p` headless from Phase 1,
│   │                           #   owns the tracing subscriber + telemetry export (`src/telemetry/`)
│   ├── fontaine-core/          # session orchestration on rig-agent (AgentRun, rig bookkeeping hooks),
│   │                           #   permission pipeline + rule grammar, settings scope stack, workspace
│   │                           #   trust, user hooks, session ledger, extension loading, FONTAINE.md
│   ├── fontaine-tools/         # risk metadata registry + GuardedTool wrapper around
│   │                           #   rig-core Tool (calls the `ToolGate` trait it defines); rig-tools built-ins re-exposed with risk
│   │                           #   classes; workspace-scope validation over `rig_tools::fs::place`; confined rig tool config
│   ├── fontaine-guardian/      # the guardian agent: verdict protocol (rig extractor),
│   │                           #   prompt builder, rules config, caching, fail-closed logic
│   ├── fontaine-agents/        # parallel subagent orchestration on rig-agent: spawn/list/
│   │                           #   interrupt, per-type tool registries, budgets (v1)
│   ├── fontaine-mcp/           # rig-rmcp wiring: MCP servers from config, tools registered
│   │                           #   as RiskClass::Arbitrary, annotations → guardian hints
│   ├── fontaine-tui/           # ratatui app: chat, approval prompts, denial/retry UI, modes
│   └── fontaine-evals/         # eval runner (bin crate): scores guardian verdicts against the dataset
├── evals/                      # golden dataset (`evals/dataset/*.jsonl`) + README; runner is rust/fontaine-evals
├── docs/                       # all product docs — one topic per page, website-ready (see `design/documentation.md` §Documentation)
├── tasks/                      # this master plan + design/ (topic design docs) + phases/ (one plan per phase)
└── research/                   # six analysis/research companion docs
```

Crate dependency graph (arrows point at dependencies; no cycles):

```
fontaine (bin)        ─▶ fontaine-core, fontaine-tui
fontaine-evals (bin)  ─▶ fontaine-core, fontaine-guardian
fontaine-tui          ─▶ fontaine-core
fontaine-core         ─▶ fontaine-tools, fontaine-guardian, fontaine-mcp, fontaine-agents
fontaine-mcp          ─▶ fontaine-tools
fontaine-agents       ─▶ fontaine-tools
fontaine-guardian     ─▶ (no Fontaine crates)
fontaine-tools        ─▶ (no Fontaine crates — never fontaine-core)
```

`fontaine-tools` defines a `ToolGate` trait (one async check per call, returning allow or a denial
for the tool_result); `GuardedTool` calls it. `fontaine-core`'s `PermissionPipeline` implements
`ToolGate` and is injected into every wrapper at registration, so the gate logic lives in core
without tools depending on it.

`fontaine-llm` from the first draft is **gone** — rig-core's provider layer (20+ providers,
streaming, OpenAI-compatible custom endpoints for OpenRouter/LM Studio/vLLM/LiteLLM) replaces it
entirely.

Phase-owned design sections live in the phase plans: model layer + headless I/O contract (Phase 1),
tools/risk registry/GuardedTool + multi-root workspace + session ledger (Phase 2), permission modes
+ rule grammar + base TUI (Phase 3), the guardian agent (Phase 4), parallel agents + agents panel,
user hooks, extension files, session resume/fork (Phase 5), result scanning (Phase 6).

## Files to Modify

All new. Workspace-level files: `rust/Cargo.toml` (members = ["*"], exclude = ["target", ".*"], resolver 2,
workspace.package block with edition 2024 and rust-version 1.97.1; rig crates pinned), `rust/rust-toolchain.toml`,
`README.md`, `LICENSE`, `CHANGELOG.md`, `CONTRIBUTING.md`, `.github/workflows/*`. Each phase plan lists its own
file slice in dependency order. No `fontaine-llm` crate — the model layer is rig.

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
- **GuardedTool decorator vs rig AgentHook as the gate** — the decorator is the single gate:
  registry-level wrapping covers built-ins, per-root instances and MCP tools in one place and each
  wrapper is unit-testable without a run. rig-agent 0.45 `AgentHook::on_dispatch` is not used for
  permission decisions; hooks stay bookkeeping-only.
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
  calls never reach the guardian), the local PromptGuard 2 result scanner (no per-result LLM call),
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
  on the seeded attack classes at <10% benign false-deny) — meaning the "any model" promise is
  hollow for the feature that matters.

## Decisions

- 2026-10-21: Multi-account model registry, following opencode: named `[accounts.<name>]`, each
  providing models addressed `<account>/<model>`, one rig client per account; replaces
  `[model] provider`/`base_url`. Reason: user requirement; Claude Code has no analogous design point.
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
  output; an Allow below the confidence threshold on a destructive class → Deny; Deny verdicts are
  never softened). Adopted from Claude Code's documented behavior.
- 2026-10-20: Core is built on the rig crate family (rig-core/rig-agent/rig-memory/rig-tools/
  rig-rmcp/rig-cassette), pinned to exact versions; no custom provider layer. (User directive:
  reuse existing crates for the core wherever possible.)
- 2026-10-20: The guardian gate lives in the `GuardedTool` decorator (registry-level choke point),
  not in rig `AgentHook`s; hooks are for bookkeeping only. Settled (see 2026-10-21 below).
- 2026-10-20: rig's Bevy-based harness/TUI (`rig-harness`, `rig-tui`) is not adopted; Fontaine
  keeps its own ratatui TUI.
- 2026-10-20: License is Apache-2.0 — the repo's `LICENSE` is verbatim Apache-2.0 text; rule prose
  adapted from Anthropic's published documentation gets an attribution note in docs/guardian.md.
- 2026-10-21: "fontaine" is the final product name — binary, crate prefix,
  config dir `fontaine/` under the platform config dir, `FONTAINE.md` convention. (User decision.)
- 2026-10-21: guardian default = top-ranked model on Fontaine's benchmark-validated guardian list
  that is available on the configured accounts; switching easy, explicit, never automatic; unset +
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
  `glob`/`grep`) plus a deterministic, zero-LLM shell pre-classifier (structural: `brush-parser` parsing
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
  curated set — they land only as explicit user-scope config rules (`[permissions] allow`) (reason: a checked-in repo or
  an injected session must not accumulate loosened prefixes; auto-extension is loosening).
  (User directive 2.)
- 2026-10-21 (supersedes 2026-10-20 "no subagents in v1"): parallel multi-agent management is a
  v1 requirement, Claude Code-style (`fontaine-agents`; spawn/list/stream/interrupt, per-agent
  context + model + restricted toolset, budgets); agent-to-agent messages go through the same
  guardian gate — the 2026-10-20 line stands, now in v1. (User directive 3.)
- 2026-10-21: deliberate Claude-Code deviation — a child's effective mode is never looser than its parent's (Claude Code lets a parent set a child `auto`); mechanism in Phase 5 "Parallel agent orchestration".
- 2026-10-21: deliberate Claude-Code deviation — `acceptEdits` auto-approves Fontaine's edit-class
  tools only; Claude Code's acceptEdits also auto-approves in-scope filesystem Bash (`mkdir`,
  `touch`, `rm`, `rmdir`, `mv`, `cp`, `sed`), while Fontaine keeps those `Arbitrary`-class shell
  calls behind a prompt (stricter than Claude Code; reason: writes ride the classified edit tools,
  the pre-classifier set is read-only, and a mutating shell must not fast-path).
- 2026-10-21: subagent nesting and budgets mirror Claude Code's documented rules (not a deviation); details in Phase 5 "Parallel agent orchestration".
- 2026-10-21: plan mode follows Claude Code's default (classifier reviews consequential non-edit
  calls; `useAutoModeDuringPlan` is on by default) — edits structurally blocked regardless of
  verdict.
- 2026-10-21 (revised): deterministic `[permissions] deny`/`ask` rules and the critical-path and
  protected-path checks gate every mode and can only prompt or deny, never allow. Critical-path
  (`rm`/`rmdir` targets: filesystem root, top-level dirs, home, cwd and its parents) forces a human
  prompt — never auto-approved by any mode or verdict — and denies in `dontAsk` and headless;
  the protected-path list (enumerated in `design/permissions.md` §Core data flow; applies to `edit`/`write`; plan: edits
  already blocked) is a separate write check (prompt in manual/acceptEdits,
  guardian in `auto`, deny in `dontAsk`), matching Claude Code; independent of any bypass mode
  (structured tool/path matching, not arg regex). Floor order: deny → ask → critical-path →
  protected-path.
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
  adjudicate; requires Fontaine-verified containment (user-scope `--in-container` + Linux runtime probe in
  Phase 4; srt/bubblewrap launch detection joins with the Phase 5 sandbox slot; probe failure ⇒ fall back to `full`); user-scope only; ships
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
- 2026-10-21: the eval runner is the workspace bin crate `rust/fontaine-evals` (keeps the "every
  crate sits under `rust/`" rule); the dataset stays in `evals/dataset/` as JSONL.
- 2026-10-21 (user): `[permissions] allow` + one `Tool(specifier)` grammar for deny/ask/allow
  (`design/permissions.md` §Permission rules). Allow rules act after the deterministic floor and never reach past it; narrow
  allow rules skip the guardian in `auto` and broad code-execution allow rules are suspended in `auto`
  (Claude Code parity). A user-authored allow rule — or a user hook's `allow` — counts as a user
  grant, as in Claude Code; it is distinct from the containment fast path, which stays the only
  guardian skip earned without a user grant, and no tool label or server declaration produces one.
  Resolves the former allow-key and rule-grammar gaps.
- 2026-10-21: deliberate Claude-Code deviation — shared project settings cannot hold allow rules, a
  project hook's `allow` decision is ignored, and a project skill's `allowed-tools` is ignored; Claude
  Code honors project allow rules once the folder is trusted. Reason: project scope can only tighten.
- 2026-10-21: deliberate Claude-Code deviation — `shell` rule matching: deny/ask rules always match
  as a prefix and anywhere in the parsed word stream, and anything unparseable that contains the rule's
  first word matches; an allow rule matches only one simple command the Tier-0.5 parser accepts, so a
  compound command never matches an allow rule (Claude Code splits compound commands and requires
  every part to match). Reason: no regex and no operator-level parser in v1, so deny/ask fail toward
  matching and allow fails toward not matching.
- 2026-10-21: settings scope stack managed (reserved, empty in v1) > CLI > project-local > project >
  user, list keys merged, project-local trusted only while untracked (Claude Code parity). Project
  settings may set `default_mode` to `manual`/`dontAsk` only (see the dated lines at the end of this
  section). Deliberate Claude-Code deviation:
  endpoint/credential keys (`[accounts.*]`, `[telemetry]` except `enabled = false`) are never read
  from project-local either — reason: a file inside the repo directory must not redirect session data.
- 2026-10-21: multiple workspace roots (`--add-dir`, `/add-dir`, `additional_directories`; Claude
  Code parity): containment = inside any root; extra roots grant file access, not configuration; only
  the user adds roots.
- 2026-10-21: session ledger — the guardian's inputs come from a Fontaine-owned append-only ledger,
  never from rig-memory context that compaction rewrites; resume returns to the resolved default mode
  and drops session grants; fork copies both stores without session grants; rewind never removes
  ledger history. Claude Code has no documented analogous design point.
- 2026-10-21: the headless I/O contract (`--output-format text|json|stream-json`,
  `--input-format stream-json`) is versioned (`schema_version`; additive changes keep the version,
  any rename or removal bumps it), mirroring Claude Code's flags plus Fontaine `gate_decision` events.
- 2026-10-21 (user; amends the `[agents.types]` user-only rule): project-defined agent files
  (`.fontaine/agents/*.md`) are allowed but only narrow — toolset a subset of the parent's, `mode`
  `manual` (default) or `dontAsk`, no `model` key. Deliberate Claude-Code deviation (Claude Code's
  project agents may set any tools, model and `permissionMode`); reason: a checked-in repo must not
  widen a child's authority or pick its endpoint.
- 2026-10-21 (user): observability goes through the `tracing` facade in every crate, and only the bin
  crate installs a subscriber. Export is opt-in and off by default, like Claude Code; enabling it
  requires an ingestion URL and an ingestion key (env-var name or stored credential; no default
  endpoint). The default backend is native micromegas, exporting span trees through Fontaine's
  `SpanBridge` (micromegas async span events with parent ids) until micromegas captures `tracing`
  spans upstream; OTLP/HTTP is the fallback. Deliberate Claude-Code deviation (Claude Code exports only
  OTel); reason: user requirement (micromegas out of the box); the `otlp` backend keeps a Claude
  Code-compatible collector story. Metric and event names mirror `claude_code.*` under a `fontaine.`
  prefix; content opt-ins mirror the `OTEL_LOG_*` flags; endpoint, credentials and opt-ins are
  user-scope only, and a project may only set `enabled = false` (Claude Code parity).
- 2026-10-21: the audit log is a separate, always-on, durable local record, not derived from
  telemetry. Both are written from one typed `GateDecision`, the audit log first. Reason: telemetry
  is opt-in, lossy and redacted, while the audit log is the security record. Claude Code has no
  analogous design point.
- 2026-10-21 (user): `FONTAINE.md` v1 loads `<primary root>/FONTAINE.md` plus the user-level file in the
  user config dir; no parent-directory hierarchy or imports (backlog GitHub issue #7).
- 2026-10-21 (user): the micromegas crates need no published `rust-version` (Fontaine pins its own
  toolchain); that upstream item is dropped.
- 2026-10-21: the guardian loop breaker follows Claude Code — `auto` pauses after 3 consecutive or 20
  total guardian denials per session (any tool, not configurable); prompting resumes, approving resumes
  `auto`; headless treats the paused state as would-prompt → Deny.

- 2026-10-21: gate location settled — the `GuardedTool` decorator is the single gate (registry-level
  wrapping covers built-ins, per-root instances and MCP; unit-testable); rig `AgentHook::on_dispatch`
  is not used for permission decisions. Resolves the former gate-location open question.
- 2026-10-21: tool calls are adjudicated and executed one at a time (rig-agent `tool_concurrency =
  1`); parallel batch adjudication is dropped for v1. Reason: simpler ordering and per-call audit
  attribution; subagents still run concurrently.
- 2026-10-21: crate cycle resolved — `fontaine-tools` defines the `ToolGate` trait that `GuardedTool`
  calls; `fontaine-core`'s `PermissionPipeline` implements it and is injected at registration;
  `fontaine-tools` never depends on `fontaine-core` (graph in §Workspace layout).
- 2026-10-21: the "destructive" class behind the fail-closed Allow → Deny rule is derived
  deterministically by `fontaine-core` from the tool call (risk class plus floor-relevant features),
  never from the guardian's self-chosen label; verdict labels are Claude Code's default auto-mode
  block categories plus `allow` (enum in the Phase 4 plan).
- 2026-10-21: deliberate Claude-Code deviation — project scope cannot set `default_mode = "plan"`
  (reason: plan's guardian leg can loosen a manual user); project and project-local scopes may set
  `manual` or `dontAsk` only.
- 2026-10-21: project `default_mode = "acceptEdits"` is rejected (deliberate deviation, stricter
  than Claude Code; reason: a checked-in repo must not auto-approve edits); `auto` is rejected from
  project-local `settings.local.toml` as well as project (Claude Code parity).
- 2026-10-21: the pre-session file index is gitignore-respecting and capped at 50,000 paths
  (overflow recorded as truncated); the guardian request carries only the pre-existence status of
  the paths the current call touches, never the index.
- 2026-10-21: directories come from the `dirs` crate (config dir `dirs::config_dir()/fontaine`, data
  dir `dirs::data_dir()/fontaine`), overridable by `FONTAINE_CONFIG_DIR` / `FONTAINE_DATA_DIR`, which
  all tests use; credentials at `<data dir>/credentials.toml`, owner-only (0600 on Unix,
  current-user-only ACL on Windows).
- 2026-10-21: the eval dataset gains `mode` (default `manual`) and `workspace` (files, symlinks and
  `cwd` materialized in a temp root) fields so `pipeline` cases are reproducible.
- 2026-10-21: the default guardian list is the data file `rust/fontaine-guardian/data/guardian_models.toml`
  (ranked entries: provider kind, model id, scorecard metrics); it ships empty — empty falls back to
  the main model with the one-time notice — and the Phase 4 scorecard run fills it.
- 2026-10-21: v1 supports Linux, macOS and Windows; CI matrix `ubuntu-latest`, `macos-latest`,
  `windows-latest` on all four CI steps. Windows (Claude Code parity): `shell` runs through Git
  Bash (Git for Windows is a prerequisite), so the shell parser, Tier 0.5 and critical-path apply
  unchanged; a PowerShell tool is backlog (GitHub issue #28) and would never pre-classify; owner-only
  files use current-user-only ACLs; the containment probe reports "not contained".
- 2026-10-21: deliberate Claude-Code deviation — every configured `[accounts.*] api_key_env` name,
  `scanner_hf_token_env` and the telemetry credential names are removed from the `shell` and hook
  child environment. Reason: a model-run command must not read provider keys (stricter than Claude
  Code).
- 2026-10-21: AgentDojo-comparable (ASR × utility) reporting is dropped; the scorecard reports
  per-class catch rate and false-deny rate.
- 2026-10-21: workspace trust (Claude Code parity) — project executables (project MCP servers, hooks,
  skill `!cmd`) load after the TUI trust prompt and without one in headless `-p`; in both they can
  never change a permission decision (a project hook's `allow` is ignored; project MCP tools are
  gated like any MCP tool). Stated once, in `design/config.md` §Config Workspace trust.
- 2026-10-21: a user-scope hook `allow` in `auto` follows Claude Code — equivalent to an allow rule,
  skips the guardian for a narrow matcher, broad matchers suspended; the `claude -p` baseline capture
  confirms it.
- 2026-10-21: the `skill` tool stays `ReadOnly` and on the fast path (it only loads skill text from
  the start-time index into context); a skill's `!cmd` is gated as a `shell` call.
- 2026-10-21 (user): shell parsing uses `brush-parser` (exact-pinned; verified 0.4.0) for Tier 0.5, the
  critical-path check and `shell` rule matching — no hand-rolled lexer, and the `shlex` crate is not
  used. Soundness unchanged: anything not exactly one simple command falls through to the guardian;
  for the floor, any `rm`/`rmdir` word in an unparseable or compound command is critical.
- 2026-10-21 (user): Windows Git Bash discovery — `git.exe` on PATH or standard install locations,
  `<Git>\usr\bin` + `<Git>\bin` prepended to the launcher PATH, `FONTAINE_GIT_BASH_PATH` override
  (parity with `CLAUDE_CODE_GIT_BASH_PATH`), not found ⇒ exit 2 at startup; hooks share it.
- 2026-10-21 (user): `[shell_preclassifier] allow_extra` entries are a command or `command subcommand`
  pair, flag-free (any flag falls through) with positional args validated as in-scope paths.
- 2026-10-21 (user): `webfetch` v1 takes `url` only; HTML → markdown via `htmd` (exact-pinned; verified
  0.5.6); 10 MB fetch cap, 100,000-character output cap (truncation noted), 30 s timeout. Deliberate
  Claude-Code deviation: Claude Code's `prompt` argument with small-model post-processing is not in v1
  (backlog) — reason: keeps `webfetch` a single deterministic tool with no extra model call.
- 2026-10-21 (user): the minimal guardian profile uses the resolved `[guardian] model` (no separate
  model key); its reduced ruleset is `rust/fontaine-guardian/prompts/guardian_minimal.md`, versioned
  alongside `guardian_prompt.md`.
- 2026-10-21 (user): releases — semver 0.x, tags `vX.Y.Z`; GitHub Releases with prebuilt Linux, macOS
  and Windows binaries built by cargo-dist (exact-pinned; verified 0.32.0) plus `cargo install` from
  git/crates.io; the maintainer cuts releases (tagging and pushing are human steps); CHANGELOG follows
  Keep a Changelog. Workflow set up in Phase 4, the first phase that produces a release.

## Open Questions

None.
