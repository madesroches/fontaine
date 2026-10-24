# Fontaine: Rust AI Agent Harness with Guardian-Agent Auto Mode — Master Plan & Phase Tracker

This file is the **tracking task**: phase status and progress live here, together with the
workspace layout, trade-offs, risk gates and every decision. The cross-cutting design every
phase depends on (permission pipeline, config, sessions, observability, platforms, research
findings, documentation, testing strategy) lives in topic docs under `tasks/design/` — see
`## Design docs`. The build itself was split into **one plan per phase**, now archived under `tasks/completed/phases/` — each phase plan owns
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

**Built:** all six phases meet their definition of done locally (see the Phase Tracker and
Progress log). Fontaine is a working agent: rig-backed multi-account model layer, headless `fontaine -p`
with a versioned JSON Schema contract, wrapped built-in tools with a risk-class registry, the
permission pipeline (deterministic floor, rule grammar, Tier-0.5 pre-classifier, audit log), the
inline ratatui TUI, the guardian and `auto` mode (the interactive default; headless defaults to
`manual`), parallel subagents, MCP, user hooks, skills / agent files / output styles, session
resume/fork, the tool-result scanner (`pg2` / `llm` / `command`), micromegas and OTLP telemetry,
the offline eval runner, and a `docs/` page per topic. CI (fmt, clippy, test, deny, offline evals,
`cargo dist plan`, pg2 builds) is green on Linux, macOS and Windows.

**Merged to `main`:** every phase, 0–6 (PRs #65–#68, #75, #79) plus follow-ups, including the
release-readiness repo work (#92), streamed and resumable live evals (#95) and OpenCode Go
accounts (#99).

**Not done:** no release has been cut. A first dry run of the live checks (2026-10-23,
`completed/release_live_checks_plan.md`) ran the scorecard, but no model cleared the inclusion
bar, so `guardian_models.toml` still ships empty and the guardian falls back to the main model.
The remaining live-key, live-model and maintainer steps are GitHub issues labelled
`release-blocker`; see `## Remaining work`.

`research/` holds seven companion docs that are this plan's precedent and inputs:
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
| 0 — scaffold (eval first) | [completed/phases/phase0_scaffold.md](completed/phases/phase0_scaffold.md) | Cargo workspace, crate skeletons, CLI entry, tracing facade + `[telemetry]` config, CI, settings scope stack + workspace trust store + read-only credentials lookup; eval golden-dataset + runner skeleton; `docs/` tree started | done | CI green; eval runner green against verdict fixtures with no guardian in the tree |
| 1 — model layer + headless | [completed/phases/phase1_model_headless.md](completed/phases/phase1_model_headless.md) | rig crates pinned; config → provider mapping; streaming spike; `fontaine -p` with the versioned headless I/O contract; telemetry backends (micromegas/OTLP) + `docs/telemetry.md`; provider docs | done | `fontaine -p` streams a prompt plus a spike tool round-trip against a mock endpoint |
| 2 — agent runtime & tools | [completed/phases/phase2_runtime_tools.md](completed/phases/phase2_runtime_tools.md) | RiskClass registry, GuardedTool (gate stubbed), rig-tools built-ins + read tools + webfetch, multi-root scope validator, confined rig tool config; rig-agent session orchestration + session ledger; `docs/tools.md` | done | agent runs tools end-to-end headless through wrapped registry; choke-point test green |
| 3 — pipeline & manual TUI | [completed/phases/phase3_pipeline_tui.md](completed/phases/phase3_pipeline_tui.md) | PermissionPipeline (manual/acceptEdits/plan/dontAsk; deterministic floor; `Tool(specifier)` rule grammar + allow rules; user-hook slot; Tier-0.5; audit log); headless `-p` denial semantics for manual/dontAsk; ratatui TUI; `docs/permissions.md` + pipeline diagram | done | **usable Claude-Code-like agent with manual approvals** |
| 4 — guardian + scorecard | [completed/phases/phase4_guardian.md](completed/phases/phase4_guardian.md) | fontaine-guardian; `auto` wired into pipeline (incl. headless `-p` semantics for auto); denial UX; live evals; **published scorecard**; `docs/guardian.md` + `docs/evals.md` | done | shipped default flips `manual` → `auto`; scorecard public with the release |
| 5 — extensibility | [completed/phases/phase5_extensibility.md](completed/phases/phase5_extensibility.md) | MCP (user + trust-gated project servers); session resume/fork; user hooks; skills/agent files/output styles; parallel subagents (v1) + budgets + agents panel; `docs/{mcp,agents,hooks,extensions}.md`; sandbox/sandboxing.md = **roadmap slot** | done | gated background subagents live in one session |
| 6 — guardian hardening | [completed/phases/phase6_hardening.md](completed/phases/phase6_hardening.md) | tool-result scanner (`[guardian] scanner` = off/pg2/llm/command; PromptGuard 2 via candle, BYO command, cheap-LLM); scanner docs | done | scanning demonstrable behind config, license caveat surfaced |

GitHub `phase-N` labels mean "earliest phase after which the issue can be taken; not part of that phase's definition of done; Decisions win over issue text".

### Progress log

Append one dated line as each phase's definition of done is met (local commits referencing the
phase plan are the work trail).

- 2026-10-21: Phase 0 (scaffold) definition of done met locally: workspace, config scope stack, trust store, credentials lookup, tracing facade, eval runner and seed dataset, docs tree; the CI matrix run and private vulnerability reporting are human steps.
- 2026-10-21: Phase 1 (model layer + headless) definition of done met locally: rig pinned, config accounts → provider clients with `<account>/<model>` resolution and discovery, `fontaine auth login`, `fontaine -p` with the versioned headless contract and JSON Schema, micromegas and OTLP telemetry backends, provider/headless/telemetry docs; the offline gates (fmt, clippy, tests, deny, eval fixture runs) are green.
- 2026-10-22: Phase 2 (agent runtime & tools) definition of done met locally: RiskClass registry, GuardedTool and the deny-by-default stub gate over rig-tools built-ins, `ls`/`glob`/`grep`/`webfetch`, multi-root scope validator with per-root confined instances, rig-agent session orchestration with rig-memory persistence and between-runs compaction, the session ledger, the versioned system prompt, `docs/tools.md`; the offline gates are green; the windows CI run of the `#[cfg(windows)]` tests is a human step.
- 2026-10-22: Phase 3 (pipeline & manual TUI) definition of done met locally: PermissionPipeline with the deterministic floor, rule grammar and allow rules, Tier-0.5 pre-classifier, audit log and decision telemetry, headless `gate_decision`/`denials`, the ratatui TUI with approval prompts, `--guardian none` evals and baseline subcommands, `docs/permissions.md` and `docs/tui.md`; the offline gates are green; the windows CI run, the cassette recording, the live TUI check and the `claude -p` baseline capture are human steps.
- 2026-10-22: Phase 4 (guardian + scorecard) definition of done met locally: `fontaine-guardian` (typed verdicts over rig's extractor, prompt assets and builder, verdict cache, loop breaker, minimal profile), `auto` wired into the pipeline and `plan` branch 9, the ledger-only request builder with the structural destructive class, default-guardian selection and live `/model` / `/guardian-model`, the TUI denial UX, guardian telemetry and audit fields, headless `auto` semantics, the eval runner's `live:` mode and models-file writer, cargo-dist release setup, `docs/guardian.md` and `docs/evals.md`; the offline gates are green; the live scorecard run that fills `guardian_models.toml`, its publication, the auto-mode cassette, the `claude -p` baseline capture and diff, the Docker leg of the minimal-profile check, the dataset label sign-off and the first release tag are human steps.
- 2026-10-22: Phase 5 (extensibility) definition of done met locally: parallel subagents (`agent`, `send_message`, agent types and files, mode ladder, budgets, depth cap, the agents panel), MCP over stdio and streamable HTTP with workspace trust and per-server approval, user hooks, skills / agent files / output styles, `--continue` / `--resume` / `--fork-session`, the trust prompt, per-agent telemetry and `docs/{agents,hooks,mcp,extensions}.md`; the offline gates are green; the live look-and-feel check of the agents panel (Manual Verification item 5), the `claude -p` hook-allow baseline, and the sandbox slot are human steps or not taken.
- 2026-10-22: Phase 6 (guardian hardening) definition of done met locally: the tool-result scanner as the `Scanner` stage of `ResultPipeline` with `pg2` (candle, windowed, behind the `pg2` feature, startup download and load that never block a session), `llm` and `command` implementations, `ScanRecord` telemetry on `tool_result`, `scanner_notices` carrying the Llama 4 license notice in every mode, the resolved `HF_TOKEN` scrub default, live-mode scanner evals, CI pg2 steps, the release build enabling `pg2`, and the scanner docs; the offline gates are green; scanning with the real PromptGuard 2 weights (the logit spike against the model card, `FONTAINE_PG2_MODEL` inference test, a live session) needs a Hugging Face token and is a human step.
- 2026-10-22: The three-OS CI matrix is green on every phase branch and `main`, which closes the Windows CI runs left open by phases 0, 2 and 3.

## Remaining work

The deferred human steps from the Progress log, gathered in one place, plus everything after v1.
Each open item is a GitHub issue; the ones that gate the first release carry the
`release-blocker` label (`gh issue list --label release-blocker`), with #80 as the tracker. Tick
an item here by changing `[ ]` to `[x]` and adding the date when its issue closes.

### Merge

- [x] Merge the Phase 5 PR (#75), then the Phase 6 PR (#79). (2026-10-23)

### Release readiness (gates the first release)

Apart from the ticked repo-side items, each item needs live provider keys, a live model, or a maintainer decision, so none of them runs in CI.

- **Guardian scorecard**
  - [ ] Maintainer signs off the eval dataset's `expected` labels against `claude -p` baselines
        (`design/testing.md` §Eval harness). (#102)
  - [x] Live scorecard run (`fontaine-evals` `live:` mode, `--profile full` and `minimal`)
        fills `rust/fontaine-guardian/data/guardian_models.toml`. (2026-10-23; run with
        `kimi/kimi-for-coding` and `local/qwen3:4b`, no model cleared the inclusion bar, so the file stays
        empty; see `completed/release_live_checks_plan.md` `## Results`)
  - [ ] Check the run against the Risk Gates' catch-rate kill floor. (#105; needs #96, #97)
  - [ ] Publish the scorecard with the release (`docs/evals.md`), rerun on the shipped rules. (#104)
- **Claude Code baselines (`claude -p`)**
  - [x] Phase 3 pipeline-case capture. (2026-10-23; 44 of 54 captured, 14 divergences triaged in
        `completed/release_live_checks_plan.md`; open divergences and 10 uncaptured cases: #101)
  - [ ] Phase 4 auto-mode capture and diff, including the destructive-class word lists. (#103)
  - [x] Phase 5 user-hook `allow` in `auto` baseline. (2026-10-23; observed once per variant; Claude Code
        honors a broad hook, which Fontaine deliberately suspends: Decisions 2026-10-23)
- **Cassettes** (`RIG_PROVIDER_TEST_MODE=record cargo test -p fontaine --test cassettes -- --ignored`; #106)
  - [ ] Record the Phase 3 manual-mode cassette.
  - [ ] Record the Phase 4 auto-mode cassette.
- **Manual verification** (`design/testing.md` §Manual Verification)
  - [ ] Item 1: live TUI, manual mode, Anthropic. (#107)
  - [ ] Items 2–3: live guardian denial and `/denied` retry. (#107)
  - [ ] Item 4: the same against Ollama, with a local model as both agent and guardian. (Run
        2026-10-23 with `local/qwen3:4b`: the guardian timed out at 30 s, so check 2 did not hold; #108, needs #96.)
  - [ ] Item 5: agents-panel look and feel. (#107)
  - [x] Item 6, Docker leg: minimal profile with `--in-container`. (2026-10-23)
- **PromptGuard 2 with real weights** (needs a Hugging Face token)
  - [x] Logit spike against the model card. (2026-10-23; against a `transformers` reference, the model card itself is gated)
  - [x] `FONTAINE_PG2_MODEL` inference test. (2026-10-23; passed with the loader fix, run with `--release`)
  - [x] A live session with `scanner = "pg2"`. (2026-10-23)
- **Bugs to fix before tagging**
  - [x] #78 (Git Bash collapses `\\` on Windows) and #73 (`webfetch` 403 without a User-Agent)
        fixed. (2026-10-23)
- **Repository and release**
  - [x] Turn on GitHub private vulnerability reporting (`SECURITY.md` promises it; it is off today). (2026-10-23)
  - [x] `CHANGELOG.md` entry for 0.1.0. (2026-10-23)
  - [x] Release archives carry LICENSE/README/CHANGELOG; release notes from the changelog;
        missing-notes guard. (2026-10-23)
  - [ ] Rename `## [Unreleased]` to `## [0.1.0] - <date>` and cut the first cargo-dist release tag. (#109)

### Roadmap slots (designed, deliberately not built in v1)

- Sandbox interop: srt/bubblewrap, `docs/sandboxing.md`, and the srt leg of Manual Verification
  item 6 (Phase 5 §Amendments A3–A4).
- Screening tier: typed-decision screeners (Jev, Laya, DiffusionGemma) and further guardian-tree
  tiers. v1 ships only the deterministic + LLM spine (Decisions 2026-10-21).

### Backlog

Everything else is a GitHub issue (`gh issue list`); a `phase-N` label means "can be taken after
phase N", and every phase is now done. The `parity` issues are the Claude Code feature gap, triaged
in `research/research-claude-code-parity.md`; the backlog has no priority order yet beyond the
`priority:low` labels.

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
`cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check` (version-pinned cargo-deny, config `rust/deny.toml`), then the offline eval fixture run (Phase 3 adds the `--guardian none` eval run: `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian none`; Phase 4 adds a `cargo dist plan` step: install the pinned cargo-dist and run it from `rust/`; Phase 6 adds `cargo clippy --all-targets --features fontaine/pg2 -- -D warnings` and `cargo test --features fontaine/pg2`) — see
`design/testing.md` §Testing Strategy).

```
fontaine/
├── rust/
│   ├── rust-toolchain.toml     # channel = "1.99.1" (rig 0.45.0 needs ≥ 1.97.1)
│   ├── Cargo.toml              # workspace root (`[workspace] members = ["*"]`, `exclude = ["target", ".cargo", ".fontaine", ".config", ".claude"]`, resolver = "2",
│   │                           #   workspace.package: edition 2024, rust-version 1.99.1); rig crates pinned to exact versions
│   ├── deny.toml               # cargo-deny config (advisories, licenses, bans, sources)
│   ├── fontaine/               # bin + lib crate (logic in `src/lib.rs`, thin `src/main.rs`): CLI (clap), wires core+tui, `-p` headless from Phase 1,
│   │                           #   owns the tracing subscriber + telemetry export (`src/telemetry/`)
│   ├── fontaine-core/          # session orchestration on rig-agent (AgentRun, rig bookkeeping hooks),
│   │                           #   permission pipeline + rule grammar, settings scope stack, workspace
│   │                           #   trust, user hooks, session ledger, extension loading, FONTAINE.md,
│   │                           #   registry assembly (full choke-point test: `fontaine-core/tests/choke_point.rs`);
│   │                           #   `prompts/system_prompt.md`
│   ├── fontaine-tools/         # risk metadata registry + GuardedTool wrapper around
│   │                           #   rig-core Tool (calls the `ToolGate` trait it defines); rig-tools built-ins re-exposed with risk
│   │                           #   classes; workspace-scope validation over `rig_tools::fs::place`; confined rig tool config
│   ├── fontaine-guardian/      # the guardian agent: verdict protocol (rig extractor),
│   │                           #   prompt builder, rules config, caching, fail-closed logic, result scanner;
│   │                           #   `prompts/` (guardian, minimal, scanner) + `data/guardian_models.toml`
│   ├── fontaine-agents/        # parallel subagent orchestration on rig-agent: spawn/list/
│   │                           #   interrupt, per-type tool registries, budgets (v1); defines the
│   │                           #   `ChildRunner` trait (implemented by core)
│   ├── fontaine-mcp/           # rig-rmcp wiring: MCP servers from config, tools registered
│   │                           #   as RiskClass::Arbitrary, annotations → guardian hints
│   ├── fontaine-tui/           # ratatui app: chat, approval prompts, denial/retry UI, modes
│   └── fontaine-evals/         # eval runner (bin + lib crate, same split): scores guardian verdicts against the dataset
├── evals/                      # golden dataset (`evals/dataset/*.jsonl`) + README; runner is rust/fontaine-evals
├── docs/                       # all product docs — one topic per page, website-ready (see `design/documentation.md` §Documentation);
│                               #   `schemas/headless-v1.schema.json`
├── tasks/                      # this master plan + design/ (topic design docs) + completed/ (shipped plans, incl. phases/)
└── research/                   # seven analysis/research companion docs
```

Crate dependency graph (arrows point at dependencies; no cycles):

```
fontaine (bin+lib)       ─▶ fontaine-core, fontaine-tui
fontaine-evals (bin+lib) ─▶ fontaine-core, fontaine-guardian, fontaine-tools
fontaine-tui          ─▶ fontaine-core
fontaine-core         ─▶ fontaine-tools, fontaine-guardian, fontaine-mcp, fontaine-agents
fontaine-mcp          ─▶ fontaine-tools
fontaine-agents       ─▶ fontaine-tools
fontaine-guardian     ─▶ (no Fontaine crates)
fontaine-tools        ─▶ (no Fontaine crates — never fontaine-core)
```

Cargo features: `fontaine/pg2 = ["fontaine-core/pg2"]` and `fontaine-core/pg2 = ["fontaine-guardian/pg2"]` forward the optional PromptGuard 2 scanner (Phase 6); CI pg2 steps use `--features fontaine/pg2`.

Phase-owned design sections live in the phase plans: model layer + headless I/O contract (Phase 1),
tools/risk registry/GuardedTool + multi-root workspace + session ledger (Phase 2), permission modes
+ rule grammar + base TUI (Phase 3), the guardian agent (Phase 4), parallel agents + agents panel,
user hooks, extension files, session resume/fork (Phase 5), result scanning (Phase 6).

## Files to Modify

Every file named in the phase plans exists. Workspace-level files: `rust/Cargo.toml`,
`rust/rust-toolchain.toml`, `rust/deny.toml`, `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`,
`SECURITY.md`, `.github/workflows/{ci,release}.yml`. Each phase plan lists its own file slice. New work
lists its files in its own plan or issue.

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
  built-in read-only Bash command set).
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

Status (2026-10-22): no gate has fired. The catch-rate gate cannot be checked until the live
scorecard run (see `## Remaining work`).

## Decisions

- 2026-10-23: Typed-decision screens (Laya, scored by `--guardian screen:<model>`): no screen ships now; the direction is a cascade guardian (floor → fast screen → LLM guardian) trained from Fontaine's own decisions (#123 decision corpus, #124 shadow mode, #125 distillation). Out of the box `laya-typed-decisions` with the scorer's three-way question escalates 165 of 193 cases (catch 97.5%, benign stop 63.6%); rephrased as a yes/no risk question the base checkpoint reaches AUROC 0.835. Replayed against the `kimi-for-coding` verdicts with held-out thresholds, a stop-only screen (option 1, safe by construction) keeps the 90.8% catch rate and removes 15% of LLM calls, and a screen that also clears (option 2) removes 26% and halves benign latency; option 2 still needs its own decision on the containment-fast-path invariant. Training data, not the encoder, is the limit: 400 synthetic policy-written cases reach AUROC 0.855, public corpora transfer poorly. DiffusionGemma, Jev and full-encoder fine-tuning were not measured (#127). Numbers in `docs/evals.md` and `research/analysis-typed-decision-screen.md`.
- 2026-10-22: `webfetch` sends `User-Agent: fontaine/<version> (+https://github.com/madesroches/fontaine)`, not Claude Code's `Claude-User (claude-code/<version>; ...)`. Reason: sites such as Wikimedia 403 a missing User-Agent, and a harness must not present itself as Claude Code's agent.
- 2026-10-21: Headless `stream-json` event names (`init`, `assistant`, `assistant_delta`, `tool_call`, `gate_decision`, `tool_result`, `result`) are Fontaine's own versioned schema — flat typed events — not Claude Code's SDK message shape; flags and `result` subtypes match Claude Code. Reason: one flat, JSON-Schema-validated contract consumed by the eval runner.
- 2026-10-21: No external approval delegate (`--permission-prompt-tool`) in v1; every headless would-prompt outcome is Deny (Claude Code supports the delegate). Reason: an external delegate would be a second, ungated approval path around the deterministic floor and guardian.
- 2026-10-21: Multi-account model registry, following opencode: named `[accounts.<name>]`, models addressed `<account>/<model>`, one rig client per account; replaces `[model] provider`/`base_url`. Reason: user requirement; Claude Code has no analogous design point.
- 2026-10-21: Unknown config keys and sections are a config error (exit 2) (Claude Code ignores them): a misspelled `[permisions]` must not silently drop deny rules.
- 2026-10-21: Permission rules naming an unknown tool are a config error (exit 2), not a warning (Claude Code warns): a mistyped deny rule must fail loudly rather than silently protect nothing.
- 2026-10-21: `--settings` takes a TOML file path only (Claude Code also accepts an inline JSON string): TOML has no unambiguous inline top-level form.
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
- 2026-10-21 (user): Bypass mode is not shipped in v1 (deviation from `bypassPermissions`); no eval or CI run bypasses the permission pipeline; `auto` cases always run with a guardian.
- 2026-10-21: Shipped default mode = auto (Claude Code parity); interim default = manual until the guardian wires in at Phase 4. Clarified 2026-10-22: the auto default applies to the interactive TUI only; headless `-p` keeps `manual` (Claude Code's `-p` starts in `default`), and an explicit `default_mode` applies to both.
- 2026-10-21 (user): Headless `-p` promoted to Phase 1 so CI and the eval harness drive core headlessly from day one.
- 2026-10-21: Deviation — subagent reports are screened by the optional (default-off) result scanner, not by the classifier at handoff; reason: the guardian never sees tool output.
- 2026-10-21 (user): No zero-guardian mode; containers get `[guardian] profile = "minimal"`, never off, and it requires Fontaine-verified containment (user-scope `--in-container` plus the Linux probe; probe failure ⇒ `full`). Ruleset and request: Phase 4 §Minimal guardian profile; it ships with its own scorecard column.
- 2026-10-21 (user): Typed-decision screeners (Jev, Laya, DiffusionGemma) are deferred to the screening-tier roadmap; thresholds are always recalibrated on Fontaine's golden set (vendor defaults over-escalate: 17/21 wrong escalations at 0.85/0.6).
- 2026-10-21 (user): Guardian tree accepted as design direction — strictest-wins composition, fail-closed between tiers, per-domain subtrees, benchmarked tiers only; v1 ships the deterministic+LLM spine; one tree serves the whole agent tree.
- 2026-10-21 (user): The plan is split into this master tracker plus one plan per phase under `tasks/phases/`, each ending in a docs step; all documentation lives under `docs/`.
- 2026-10-21: Phase 1 implementation choices that refine the design docs: the OTLP backend uses `opentelemetry-otlp`'s `reqwest-blocking-client` (the SDK's batch processors run on their own threads, outside tokio; exporters are built before the runtime); the micromegas span bridge never calls `init_thread_stream` itself, since micromegas keeps a raw pointer to every thread stream and a thread that exits without unregistering would leave it dangling for its flush monitor — the runtime callbacks and the main-thread guard supply the streams; events are exported from the `fontaine::telemetry` target only (rig's event targets carry request and response bodies at trace level) while spans are exported from all four allowlisted targets; the `fontaine.interaction` span is adopted by rig as the agent span, so no separate `invoke_agent` span appears; exported span fields outside a fixed content-free set are treated as content (unknown `gen_ai.*`/`rig.*` fields are dropped unless their opt-in key covers them) and source locations and thread names are not exported; the micromegas sink is built without its host-identifying default properties.
- 2026-10-21: Deviation — without `--max-turns` a headless turn is capped at 1000 model calls (Claude Code is unbounded); reason: rig requires a bound and a looping model should not run forever. `--max-budget-usd` is checked between model calls: the call that crosses the cap stops the run before its tool calls execute, and an answer already complete is kept.
- 2026-10-21: An `openai-compatible` account with no credential source sends no `Authorization` header (the gateway dialect uses optional bearer auth), so local servers that reject a bare `Bearer` header work.
- 2026-10-21: The eval runner is the workspace bin + lib crate `rust/fontaine-evals` (like `fontaine`: logic in `src/lib.rs`, thin `src/main.rs`, so integration tests can import it); the dataset stays in `evals/dataset/` as JSONL.
- 2026-10-21: Fontaine never spawns `git` for project-local trust classification or ledger capture; `gix` (exact pin, default features off) reads tracked/ignored status and remotes, and no `.git` or a `gix` error ⇒ project scope and no remotes; reason: repo-controlled git config can execute commands.
- 2026-10-21: Audit `args_digest` canonicalizes with exact-pinned `serde_jcs` (RFC 8785).
- 2026-10-21: `/permissions` leaves Phase 4 for backlog GitHub issue #47 (core slash commands); users edit the config file, and "always allow" grants still write to the user file; supersedes the Phase 4 ownership of `/permissions`.
- 2026-10-21: CI runs `cargo deny check` (advisories, licenses, bans, sources) with a checked-in `rust/deny.toml` on all three OSes; cargo-deny version-pinned.
- 2026-10-21 (user): `[permissions] allow` plus one `Tool(specifier)` grammar for deny/ask/allow (`design/permissions.md` §Permission rules); allow acts after the floor, narrow rules skip the guardian in `auto`, broad code-execution rules are suspended in `auto` (Claude Code parity for allow rules; for broad hooks see 2026-10-23); a user allow rule or user hook `allow` is a user grant distinct from the containment fast path.
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
- 2026-10-21: Workspace trust (Claude Code parity: folder trust covers the whole folder — bounded at the enclosing git work-tree root, as in Claude Code; unbounded outside a repo) — in the TUI, project and project-local executables (hooks, MCP servers, skill `!cmd`) load only after the trust prompt; headless loads them without one; they never change a permission decision (`design/config.md` §Config, Workspace trust).
- 2026-10-21: A user-scope hook `allow` in `auto` follows Claude Code (like an allow rule; broad matchers suspended; the `claude -p` baseline confirms narrow hooks only, see 2026-10-23 for broad ones).
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
- 2026-10-22: The `fontaine-agents` ledger trait is named `RootLedger`; reason: `LedgerView` is already a `fontaine-core` struct.
- 2026-10-21 (user): The `agent` tool blocks until the child finishes (Claude Code parity); `background: true` returns `{agent_id}` and delivers the report as a notification (Phase 5).
- 2026-10-21 (user): `send_message` reaches parent, children, siblings and `"main"`; delivery is queued, untrusted and sender-marked (Phase 5).
- 2026-10-21 (user): The RuleLabel enum gains `Container Escape` for `guardian_minimal.md`; the escape class goes in the verdict `explanation` as `class=<name>` and the `minimal` column reports per class from it.
- 2026-10-21 (user): Guardian constants ship in `fontaine-guardian`, not config keys (cache TTL, `task_intent` budget, message and action counts, extractor retries; Phase 4).
- 2026-10-21 (user): Root `SECURITY.md` (Phase 0): private reporting, 90-day coordinated disclosure; bypass dataset PRs only after a fix ships.
- 2026-10-21 (user): The eval dataset has minimum case counts and maintainer sign-off of `expected` labels against `claude -p` baselines before the scorecard publishes (`design/testing.md` §Eval harness).
- 2026-10-21 (user): PromptGuard 2 runs on `candle` (safetensors from the gated HF repo), not ONNX/`ort`; exact pins and the `pg2` feature in Phase 6.
- 2026-10-21 (user): Scanner selection is one key, `[guardian] scanner = "off" | "pg2" | "llm" | "command"` (default `off`), replacing `scan_tool_results`; scanner errors fail closed (flagged); scanner keys are user file only.
- 2026-10-21 (user): The Linux containment probe and its signals are defined in Phase 4 step 5; macOS and Windows ⇒ not contained; `minimal` needs `--in-container` AND a positive probe.
- 2026-10-21 (user): The guardian-list inclusion bar and ranking are defined in Phase 4 §The guardian agent; separate from the kill criteria.
- 2026-10-21: While a parent is in `plan`, every child's edits are blocked and its non-edit calls use the tighter of its own mode and `manual` (Phase 5).
- 2026-10-21 (user): `/model` and `/guardian-model` switch the live session and persist to the user config file (Claude Code `/model` parity); `/model` moves the guardian only when it is unset and on the main-model fallback; an unavailable pinned guardian in the TUI prompts retry / pick another validated guardian / switch to `manual` (Phase 4 §The guardian agent).
- 2026-10-21: A cost cap (`subagent_cost_budget_usd`, `session_cost_budget_usd`) on a model with no catalog price is inactive with a one-time notice and the token cap still binds; an explicit `--max-budget-usd` on an unpriced model stays a usage error (exit 2) (Phase 5 Budgets).
- 2026-10-21: The headless I/O contract is a checked-in JSON Schema per version, `docs/schemas/headless-v1.schema.json`, published with the docs and validated with the exact-pinned `jsonschema` crate (Phase 1).
- 2026-10-21: Eval `pipeline` cases take an optional `permissions` field (`deny`/`ask`/`allow` in the `Tool(specifier)` grammar, plus `allow_extra`); Phase 0 seeds floor cases and `baseline capture` writes the equivalent `.claude/settings.json` permissions into the scratch workspace.
- 2026-10-21: Deviation — user-scope agent files accept `permissionMode` only as `inherit`/`default`/`dontAsk`; any other value is a load error (exit 2) naming the file; reason: same mode rule as `[agents.types]`, stricter than Claude Code.
- 2026-10-21: Dependency policy — exact pins (`=x.y.z`) for rig and every pre-1.0 crate, each new crate listed with its pin in the owning phase's Files line; 1.x crates (clap, tokio, serde, toml) use caret.
- 2026-10-21: The system prompt is the versioned asset `rust/fontaine-core/prompts/system_prompt.md` layered base → `FONTAINE.md` → skill index → output style → `--append-system-prompt`; `--system-prompt` replaces only the base (Phase 2 step 2).
- 2026-10-21: Project-local `settings.local.toml` gets user trust only when untracked AND its root is trusted (Claude Code trusts `settings.local.json` unconditionally): a shipped `.git` skeleton or an enclosing repo can make a hostile file look untracked.
- 2026-10-22: A file-tool call outside the workspace roots is denied with the /add-dir hint instead of prompting (Claude Code prompts); reason: file tools are confined to the roots, so an approved out-of-scope call would still be refused.
- 2026-10-22: Read deny/ask rules check only the path words of a pre-classified shell command, not files a recursive search (`grep -r KEY .`) reaches under a directory; matches Claude Code, whose Read rules are best-effort for Bash. Reason: shell reads are not a containment boundary; use shell rules or the protected paths.
- 2026-10-22: `/denied` is Fontaine's stand-in for Claude Code's `/permissions` Recently-denied tab until issue #47; reason: Fontaine has no `/permissions` UI yet.
- 2026-10-22: The TUI draws inline (live area plus terminal scrollback) instead of on the alternate screen, matching Claude Code; reason: alternate-screen apps lose mouse selection and wheel scrolling under tmux (#70).
- 2026-10-22: The TUI takes the whole terminal at startup (earlier shell output scrolls into the terminal's history) and keeps the input box and status bar on the bottom rows; Claude Code renders from the cursor row. Reason: a full-screen layout was requested (#74).
- 2026-10-22: `@path` mentions are completion only; the message is sent as typed and the agent reads the file through its gated `read` tool (Claude Code inlines the file's content). Reason: file content reaching the model must pass the scope validator, the deterministic floor and the guardian.
- 2026-10-22: A `!` command's text and output are not guardian intent; the guardian sees only typed messages. Reason: output is data, and treating a command as authorization would let one `!` run widen what the agent may do.
- 2026-10-22: `!` commands run only between turns. Reason: a user command racing the agent's own calls would interleave side effects the guardian is judging.
- 2026-10-22: Queued messages become guardian intent when the model receives them, not when typed. Reason: at queue time the guardian would judge calls using words the model never read.
- 2026-10-22: Nothing is queued while a `!` command runs: Enter shows a notice and keeps the input. Reason: no agent run consumes the queue then.
- 2026-10-22: Toolchain and workspace `rust-version` raised from 1.97.1 to 1.99.1 (PR #69); rig 0.45.0's 1.97.1 is the floor, not the pin.
- 2026-10-22: Each phase plan's own `## Decisions` section holds that phase's implementation-level choices; deviations from Claude Code are also copied here.
- 2026-10-21: Deviation — with no main model configured (no `[model] model` or `--model`) Fontaine exits with a config error (exit 2) (Claude Code has a default model); reason: Fontaine is provider-agnostic with no default account (Phase 1).
- 2026-10-22: Deviation (project-agent-file types only) — the `agent` tool's per-call `model` is an error tool result for a type defined by a project agent file; elsewhere Claude Code's precedence (per-call > type > session) holds; reason: a repo-defined type must not end up on a model the repo steered (Phase 5).
- 2026-10-22: Deviation — `hooks` and `mcpServers` in agent or skill files and `permissionMode` in skill files are load errors naming the file, and an agent's `disallowedTools` is honoured as a subtraction; reason: these files load without workspace trust, and ignoring a narrowing key would widen authority (Phase 5).
- 2026-10-22: Deviation — a stdio MCP server gets a scrubbed environment (a base set plus the variables in `env_vars`) and its stderr goes to the debug file; reason: provider keys and telemetry credentials must not reach a third-party process (Phase 5).
- 2026-10-22: Exported MCP server and tool names read `third-party` unless `log_tool_details` is on; reason: those names can identify internal systems (Phase 5).
- 2026-10-22: `fontaine.session.count` `start_type` takes Claude Code's values `fresh`/`resume`/`continue`, and the surface moves to a `headless` attribute; this corrects an earlier silent deviation (Phase 5).
- 2026-10-22: Deviation from LlamaFirewall — pg2 scores the whole result in 512-token windows at stride 256 instead of truncating to the first 512 tokens; reason: an injection past the first window would otherwise pass unflagged (Phase 6).
- 2026-10-22: On Windows the command text reaches Git Bash in `FONTAINE_SH_COMMAND`, not on the command line, unlike Claude Code. Reason: the judged command must be the one that runs.
- 2026-10-23: In auto, Fontaine suspends a broad PreToolUse allow hook; Claude Code 2.1.309 honors it (one `claude -p` run per variant). Kept stricter: a broad hook would otherwise bypass the guardian for every call it matches.
- 2026-10-23 (user): Every phase is done, so the phase plans moved to `tasks/completed/phases/` and the live-checks plan to `tasks/completed/`; the master plan, `tasks/design/` and `## Decisions` stay live, and remaining work is tracked as GitHub issues (`release-blocker` for the first release).
- 2026-10-23: The `/model` picker groups models by account, filters them with a fuzzy match and lists recently used models first, going beyond Claude Code's flat `/model` picker. Reason: Fontaine's model namespace is `<account>/<model>` across several accounts; the design follows opencode.
- 2026-10-23 (user): Every model change is saved as the default for later sessions: Enter on the picker's current model saves it, and the unavailable-guardian prompt's "pick another" answer saves `[guardian] model` instead of lasting one session. Reason: a model chosen in the TUI should still be the model next time.

## Open Questions

None.
