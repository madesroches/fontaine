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

`research/` holds twelve companion docs that are this plan's precedent and inputs:
`analysis-competitive-landscape.md` (market/permission-story survey),
`analysis-guardian-market.md`, `analysis-differentiation.md`,
`analysis-devils-advocate.md` (kill criteria), `analysis-strategic-verdict.md` (verdict and
"Required plan amendments" A1–A5, which this plan incorporates: A1 as the public scorecard plus a contributor path for dataset and red-team cases in `CONTRIBUTING.md`, A5 as the local/air-gapped lead in the README quickstart), and
`research-llamafirewall.md` (deep-dive on the nearest open prior art),
`analysis-typed-decision-screen.md` (the typed-decision screen spike, with its scripts under
`research/typed-decision-screen/`), `research-claude-code-parity.md` (Claude Code feature matrix
and backlog triage), `research-deepseek-harness.md` (study of DeepSeek's own agent harness:
desirable features led by iteration, DeepSeek model support, experiments under
`research/deepseek-harness/`), `research-anthropic-models.md` (Claude on Microsoft Foundry and
Databricks, a docs-based study), `research-oh-my-pi-harness.md` (study of the oh-my-pi
harness: what lets the model iterate, edit formats, safety posture, terminal UI, experiments
under `research/oh-my-pi-harness/`), and `research-claude-code-auto-mode-prompt.md` (verbatim capture of Claude Code's auto-mode classifier prompt).

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
sits directly under `rust/`** — the root directories are only `rust/`, `evals/`, `analytics/`, `docs/`, `tasks/`,
`research/`, plus root files `README.md`, `LICENSE`, `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`, `AGENTS.md`, `.gitignore`,
`.github/workflows/release.yml` (cargo-dist release build; see `design/platforms.md` §Releases) and
`.github/workflows/ci.yml` (GitHub Actions; matrix `ubuntu-latest`, `macos-latest`, `windows-latest`, each job installs the toolchain pinned in `rust/rust-toolchain.toml` and from `rust/` runs `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check` (version-pinned cargo-deny, config `rust/deny.toml`), then the offline eval fixture run (Phase 3 adds the `--guardian none` eval run: `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian none`; Phase 4 adds a `cargo dist plan` step: install the pinned cargo-dist and run it from `rust/`; Phase 6 adds `cargo clippy --all-targets --features fontaine/pg2 -- -D warnings` and `cargo test --features fontaine/pg2`, Linux only, with `cargo check --features fontaine/pg2` on macOS and Windows; the two `fixture` eval runs are Linux only; a `changes` job skips the Rust steps for PRs touching only `tasks/` or `AGENTS.md`; PR runs restore but do not save the cache, and superseded PR runs are cancelled) — see
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
├── analytics/                  # micromegas notebooks (`notebooks/fontaine-overview.json`), applied with `micromegas-screens`
├── docs/                       # all product docs — one topic per page, website-ready (see `design/documentation.md` §Documentation);
│                               #   `schemas/headless-v1.schema.json`
├── tasks/                      # this master plan + design/ (topic design docs) + completed/ (shipped plans, incl. phases/)
└── research/                   # twelve analysis/research companion docs
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

- 2026-10-27: Background shell jobs and `monitor` (#25) are root-only; a subagent's `shell` keeps kill-at-timeout, a job keeps its output in a 1 MiB in-memory buffer with no spool files, and `job_cancel` (not `job_kill`) is `arbitrary` and path-free, because jobs must stay one session's, one writer's state and a spool file would add a write surface and path handling. A `shell` deny or ask rule and hook matcher also cover `monitor`; an allow rule does not, because a prohibition must not be sidestepped by running the same command through another tool. See `tasks/background_shell_jobs_plan.md`.
- 2026-10-25: Locks guarding permission, guardian, spend, model-selection or durable-record state panic on poison via `fontaine_tools::lock`; only display and advisory state recovers (`*_recovering`, reason at the call site), because a corrupted permission state must end the session rather than keep deciding (#186).
- 2026-10-25: One `todo_write` tool (the whole list replaced per call) stands in for Claude Code's TaskCreate/TaskGet/TaskList/TaskUpdate, because a single replace-all call is one testable path and Claude Code's `TodoWrite` already works that way (#22).
- 2026-10-25: Only the root agent's list is displayed and persisted with the session; a subagent's list lives and dies with that subagent, because the panel and the session file have one slot and a child's list is scratch state that is never shown or saved (#22).
- 2026-10-25: Ctrl-O and `/todos` both toggle the task panel (`/todos` does not print), because the panel can be hidden to reclaim height (#22).
- 2026-10-25: `todo_write` takes the name-based read-only fast path, like `skill`, because it touches no file the model names, process or network, and only writes the session's own `todos.json` (#22).
- 2026-10-24: `/help`, `/status`, `/permissions`, `/mcp`, `/hooks` and `/skills` are read-only transcript notices instead of Claude Code's interactive dialogs, because it is one testable path, rule editing stays in config files (2026-10-21), and the text stays in scrollback (#47).
- 2026-10-24: `/copy` uses OSC 52, preceded inside tmux by `tmux load-buffer -w -` (falling back to `tmux load-buffer -`) as Claude Code does, with no code-block picker and no `w`-to-file, because it adds no dependency and works over SSH, where `arboard` needs X11/Wayland; tmux's default `set-clipboard external` drops an application's OSC 52. The text goes to a clipboard sink passed to `drive`, so the write's `Result` reaches the command in the same call (#47).
- 2026-10-24: `/diff` is not implemented, because it would spawn `git`, which runs repo-controlled config (`diff.external`, textconv drivers); a `gix` diff is a feature of its own, and `!git diff` covers it as the human's own action (#47).
- 2026-10-24: The default theme is `dark-ansi` (today's 16-colour palette), not Claude Code's truecolor `dark`, because ratatui does not downsample truecolor on 256-colour terminals and the ANSI palette follows the terminal's own scheme (#50).
- 2026-10-24: There are no custom theme files (`themes/*.json`) and no `auto` light/dark detection, because detection needs an OSC 11 round trip that times out on terminals that do not answer; custom themes can follow as their own issue (#50).
- 2026-10-24: `/theme` is "no argument lists, a name switches and saves", like `/output-style`, not Claude Code's picker with a preview (#50).
- 2026-10-24: `[tui]` is a scope error in project and project-local files (like `accounts`); in Claude Code a project's `axScreenReader` is ignored with a startup line, but Fontaine's settings are `deny_unknown_fields` with a per-column scope table and terminal preferences are personal (#50).
- 2026-10-24: In screen-reader mode, menus keep their letter keys and arrows and the selected row is announced, rather than becoming numbered lists, which would need a second input path in every dialog. Claude Code's 3 s startup quiet wait, `CLAUDE_AX_PREPARK_MS`, the bell for tools running longer than 5 s, OSC 133 turn markers and word-deletion announcements are not implemented (#50).
- 2026-10-24: Assistant text is labelled `assistant:`, not Claude Code's `claude:`, because Fontaine runs any model; notices are labelled `notice:` (#50).
- 2026-10-24: Mode changes are announced in screen-reader mode however they happen, including `/plan`, where Claude Code announces only Shift+Tab (#50).
- 2026-10-24: Fontaine has no `/terminal-setup` command and does not enable the kitty keyboard protocol; terminal setup is documented in `docs/terminal.md`, because writing other programs' config files is an unrecoverable write outside the workspace and the keyboard protocol changes how several key chords are reported (#50).
- 2026-10-24: In screen-reader mode a running tool is not announced until it ends and messages queued mid-turn are not announced; both are listed as known limitations. The overlay snapshot is keyed on title plus an approval instance id so a queued approval that replaces an answered one is announced, and the deny-reason field is echoed through `echo_diff` instead of being reprinted with the footer (#50).
- 2026-10-24: `/mcp` takes no `reconnect`/`enable`/`disable` arguments until #35.
- 2026-10-24: `/release-notes` is not implemented, because it would compile `CHANGELOG.md` into the binary or need network access, with little value before the first release (#47).
- 2026-10-24: `/logout` is not implemented, because the `/login` dialog already logs out (#47).
- 2026-10-24: `/ide` is not implemented, because IDE integration is backlog issue #59 (ACP).
- 2026-10-24: `/bug` and `/feedback` are not implemented, because Claude Code sends reports to Anthropic and Fontaine has its GitHub issue tracker (#47).
- 2026-10-24: `/tasks` is an alias of `/agents`, because the agents panel is already the background-work view Claude Code's `/tasks` opens (#47).
- 2026-10-24: `/denied` stays as the view of denied calls alongside the read-only `/permissions`; the 2026-10-22 line called it a stand-in "until issue #47", but `/permissions` shows rules, not denials.
- 2026-10-24: A built-in command wins over a skill of the same name or alias; the skill is left out of the `/` menu and `/help`, named in a startup notice, and still loadable by the model. Departs from Claude Code, where a local skill replaces a built-in (not its aliases), because a repository skill must not take over `/plan` or `/permissions`, which the human relies on to change or inspect what the agent may do (#47).
- 2026-10-24: `/skills` has no filter, sort or visibility toggling, because a read-only listing covers discovery and the interactive dialog is not worth a second path (#47).
- 2026-10-24: `api_error` exports `error_class` and `status_code` instead of Claude Code's `error` message and `attempt`, because provider messages can echo the request and rig's retries are not visible at this layer (#139).
- 2026-10-24: `fontaine.token.usage` / `fontaine.cost.usage` gain an `estimated` attribute, a Fontaine extension, to separate the dropped-guardian-spend estimate from real usage (#139).
- 2026-10-24: No custom stream tags for opt-in content; tags are hard-coded upstream, and opt-in content already lives on `cpu`, which the retention recipe expires first.
- 2026-10-24: No materialized view set; applying one needs a micromegas admin and the notebook queries run over base views.
- 2026-10-24: No process-health collector; micromegas's system monitor is host-level and no question the telemetry answers needs it.
- 2026-10-24: The `SpanBridge` span half is kept despite #139's done-when, because the stock layer exports field content and absolute source paths; the swap waits on the upstream field hook and location switch (madesroches/micromegas#1725).
- 2026-10-24: micromegas is on a git pin (`rev 7a08f6541`) with `allow-git` in `deny.toml` until 0.33 is on crates.io; the git pin is never shipped in a release, and switching to an exact `=0.33.x` pin is tracked by the `release-blocker` issue #141.
- 2026-10-24: The analytics notebook's metric cells are native-only instead of changing the OTLP exporter, because micromegas drops OTLP histograms and Fontaine's OTLP sums are cumulative; the event cells work on both backends.
- 2026-10-24: The dropped-spend fields keep the `guardian_*` / `guardian.*` convention: `DecisionRecord` `guardian_dropped_{input_tokens,provider,model,priced}`, projected onto `tool_decision` as `guardian.dropped_{input_tokens,provider,model,priced}`.
- 2026-10-24: `fontaine auth login` is removed, with no CLI replacement (#140; Claude Code keeps `claude auth login`). The TUI's `/login` provider dialog is the only interactive way to store a credential, and when the interactive UI cannot resolve its main model at startup the same dialog opens before the session. Automation provisions the account config plus `api_key_env` or a `credentials.toml`; the telemetry ingestion key comes from `api_key_env` or a hand-provisioned `[telemetry]` entry. Reason: one credential path that works in the running session, and no secret read from a pipe.
- 2026-10-24: Deviation — the screen stage (`[guardian.screen]`) may allow a call without the LLM stage (#135), through a steering-hardened allow path only; this supersedes the 2026-10-24 "never allows" line below and deviates from "the fast path is earned by containment". It is a stage of a composed guardian (`fontaine_guardian::cascade`), so a screen allow is a guardian-tier verdict, taken to cut allowed-call latency: the screen and the LLM stage start together and a settled call drops the LLM request. Evidence (experiment 10, #133): with the shipped prompt one steering sentence moved 93 of 134 must-not-clear cases below the skip threshold, so the allow needs a hardened permission read and a steering detector below their thresholds; at the zero-miss threshold it skipped 13 of 59 should-allow calls with 0 must-not-clear allowed and 0 steered cases through. Fail-closed fallbacks: any read failure makes the screen `Unavailable`, and any guardian verdict already in hand (cache hit, unavailable, an earlier finish) wins and goes through the normal flow, so the call goes to the guardian. It is off by default (`allow_threshold`, user scope only), shadow-loggable first, never runs before the floor or on a paused breaker, and does not reset the denial breaker.
- 2026-10-24: (Superseded by the entry above for the allow path.) The cascade screen stage (`[guardian.screen]`, #130) only ever stops a call as an `escalate` (`tier = screen`); it never allows, so a screen error, timeout or unparseable answer goes to the guardian, never to allow. Reason: option 1 keeps the guardian's fail-closed floor intact, while a screen that allows would be a second, unreviewed allow path (option 2, #133, stays out of scope).
- 2026-10-23: Typed-decision screens (Laya, scored by `--guardian screen:<model>`): no screen ships now; the direction is a cascade guardian (floor → fast screen → LLM guardian) trained from Fontaine's own decisions (#130 cascade stage, #123 decision corpus, #124 shadow mode, #125 distillation). Out of the box `laya-typed-decisions` with the scorer's three-way question escalates 165 of 193 cases (catch 97.5%, benign stop 63.6%); rephrased as a yes/no risk question the base checkpoint reaches AUROC 0.835. Replayed against the `kimi-for-coding` verdicts with held-out thresholds, a stop-only screen (option 1, safe by construction) keeps the 90.8% catch rate and removes 15% of LLM calls, and a screen that also clears (option 2) removes 26% and halves benign latency; option 2 still needs its own decision on the containment-fast-path invariant. Training data, not the encoder, is the limit: 400 synthetic policy-written cases reach AUROC 0.855, public corpora transfer poorly. DiffusionGemma, Jev and full-encoder fine-tuning were not measured (#127). Follow-up the same day: typed decisions read from a small instruct chat model's first-token logprobs beat Laya. `qwen3-4b-instruct-2507` asked whether an employee given the request would be allowed the action reaches AUROC 0.913 and reads the task-action relation. Held out end to end (synthetic-corpus thresholds, fresh live kimi run), as a stop-only screen at a 10% escalation budget it lifts catch from 91.7% to 96.7%, cuts LLM calls to 41% and mean latency from 7.3 s to 3.0 s, at the cost of 13.6% of allowed calls escalated instead of 3.4%. That makes it the cascade's first screen candidate (#130), and still nothing ships. Numbers in `docs/evals.md` and `research/analysis-typed-decision-screen.md`.
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
- 2026-10-21: Workspace trust (Claude Code parity: folder trust covers the whole folder — bounded at the enclosing git work-tree root, as in Claude Code; unbounded outside a repo) — in the TUI, project and project-local executables (hooks, MCP servers, skill `!cmd`) load only after the trust prompt; headless loads them without one; they can tighten but never loosen a permission decision (`design/config.md` §Config, Workspace trust).
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
- 2026-10-24: AFK: `/clear` clears the conversation in the current session (same id, ledger, audit log, grants and mode) instead of starting a new session as Claude Code does, because `SessionState` holds the ledger and session id as plain fields shared with the gate, the agent tree and the child runner, so a new session mid-run is a rework of its own. The pre-clear conversation cannot be resumed: `--resume` reopens the empty conversation, and the log file keeps the old messages only as logged changes (#11).
- 2026-10-24: AFK: after `/clear`, the guardian's view of the human's messages restarts at the `ContextCleared` ledger record; workspace facts (actions, denials, file index, created files) are kept, because that matches what a new Claude Code session's classifier sees without dropping facts about the files (#11).
- 2026-10-24: AFK: `/clear <name>` is an error until `/resume` exists, because the name only labels the old conversation in `/resume` (#11).
- 2026-10-24: AFK: the auto-compact setting is `[context] autocompact_window` (tokens), not the issue's `autocompact_pct`, because Claude Code's `autoCompactWindow` is a token count (#11).
- 2026-10-24: AFK: `autocompact_window` accepts any positive token count, where Claude Code accepts 100K-1M, because Fontaine runs local and gateway models whose windows are below 100K (#11).
- 2026-10-24: AFK: `[context]` is refused from shared project scope, because a repository should not decide how often a user pays for summaries; project-local and user scope can set it (#11).
- 2026-10-24: AFK: no `/autocompact` command, `--autocompact` flag or `CLAUDE_CODE_AUTO_COMPACT_WINDOW` variable; the setting is config only (`--settings` covers a single launch), because there should be one way to set it and `/autocompact` writes user settings, which belongs with `/config` (#4, #11).
- 2026-10-24: AFK: `/context` is a text notice, not Claude Code's colored grid and suggestions, for the same reason as the other read-only reports (#11).
- 2026-10-24: AFK: `/clear` is refused while any subagent is running, because a running subagent's guardian requests read the root's live human messages, so clearing them under it would judge its later calls with an empty task intent; refusing is the fail-closed choice over a warning or a documented gap (#11).
- 2026-10-24: AFK: `[context] autocompact_window` enables threshold compaction only for catalog-listed models; a model the catalog does not list gets no fallback `ModelSpec`, is never compacted automatically (nor by `/compact`), and `/context` says so, because rig reports no spec for unlisted models on purpose and a guessed spec's request options and limits could be wrong for the real model; a fallback can come later as its own change (#11).
- 2026-10-24: A repeated-identical-tool-call reminder like dsh's `repeat-tool-reminder` (#160) may be added although Claude Code has none (measured: nine identical calls, no nudge); it must stay advisory, one user-role notice that never blocks a call or changes a permission decision, because in the #154 study it made the model stop and diagnose after three identical failing calls.
- 2026-10-25: project scope cannot set `[env]` (Claude Code honors it after trust); it cannot tighten, and a project `PATH` would shadow commands the pre-classifier and guardian reason about.
- 2026-10-25: `[env]` applies to tool and extension subprocesses only, not Fontaine's own process (Claude Code applies it to both); credential and telemetry lookups keep their explicit keys.
- 2026-10-25: `[network]` is config keys only, with no equivalent of Claude Code's `NODE_EXTRA_CA_CERTS`, `CLAUDE_CODE_CLIENT_CERT` or `CLAUDE_CODE_CLIENT_KEY` variables, because one place to set it keeps the project-scope tightening rules and the per-account override in force (#6).
- 2026-10-25: A passphrase-protected `client_key` is refused at config load, where Claude Code accepts one via `CLAUDE_CODE_CLIENT_KEY_PASSPHRASE`, because rustls cannot decrypt a key and a passphrase in config or env would be a secret to store (#6).
- 2026-10-25: Telemetry exporters are not covered by `[network]` (proxy, CA bundle, client certificate), because they use their own HTTP stack; covering them can come later as its own change (#6).
- 2026-10-25: `ask_user_question` is root-agent only, refused at call time for children (including custom types that inherit it), because background subagents must not put a dialog in front of the human (#23).
- 2026-10-25: `ask_user_question` is `ReadOnly` and takes the fast path through a pathless-tool carve-out (with `skill`), because it only shows the human text and collects a choice; the floor still precedes it and no mode answers it (#23).
- 2026-10-25: Esc in the `ask_user_question` dialog gives a "declined" result rather than an error or the unavailable text, so the model can tell a refusal from a headless session (#23).
- 2026-10-25: Only the human's typed "Other" text from `ask_user_question` is appended to the ledger as a `UserMessage` (`Other: "<text>"`); selected labels, question text and descriptions reach the model through the tool result only, because option labels are model-authored and "only messages you typed count as intent" must hold (#23).
- 2026-10-25: Narrows the 2026-10-21 "every CI step runs on all three OSes": compile, lint and test coverage stays on all three, Linux runs pg2 clippy and test, macOS and Windows run `cargo check --features fontaine/pg2`; only the pure-scoring `fixture` evals are Linux-only (`--guardian none` stays everywhere; required check names unchanged). Reason: Windows CI took ~32 min vs ~8 on Ubuntu (#178).
- 2026-10-25: Deviation — an MCP server's `requiresUserInteraction` marker stays in force for the life of the connection once any `tools/list` response carried it (Claude Code follows the latest listing); overlapping refreshes and late responses could otherwise let a stale page loosen it. It only ever asks more.
- 2026-10-25: Scheduled prompts (#16) have no fire-time jitter and no early firing of one-shots, where Claude Code adds both to spread load on one provider; a local single-session scheduler fires at the scheduled minute.
- 2026-10-25: `schedule_wakeup` takes no `prompt` (Claude Code's does); it re-arms the prompt of the self-paced loop the user started with `/loop`, so the model can set the pace but never author new standing text, and no guardian call is needed per iteration (#16).
- 2026-10-25: Scheduling is interactive-only: a headless `-p` run exits after its answer and registers no scheduling tools, because a scheduled job could never fire there (#16).
- 2026-10-25: Bare `/loop` (maintenance prompt, `loop.md`) and `/loop 20m /skill` expansion at fire time are deferred; a loop's prompt is plain text (#16).
- 2026-10-25: A scheduled turn is a harness note and never a `UserMessage`, so model-authored prompt text cannot become the guardian's task intent; `cron_create` is an `arbitrary` tool for the same reason, and `/clear` also clears the schedule (a `Scheduled { deleted }` ledger record per job) so a job planted under dropped intent cannot outlive it (#16).
- 2026-10-25: The list tool is `cron_jobs`, not `cron_list`, because the choke-point test bans tool names containing list/status/kill/interrupt/stop; its Claude alias stays `CronList` (#16).
- 2026-10-25: `/loop` accepts only even cadences (minute steps 1, 2, 5, 10, 15, 20, 30; hours 1, 2, 3, 4, 6, 8, 12; days 1-31 via `*/n` day-of-month, approximate at month ends) and rounds anything else to the nearest, ties down (`7m` -> `5m`, `90m` -> `1h`), stating the cadence chosen; the minute steps omit 3, 4, 6 and 12 so that `7m` rounds to `5m` as pinned, and cadences are wall-clock aligned (#16).
- 2026-10-25: `[schedule] enabled` is user-scope only (user file and `--settings`); a project file cannot set it, because scheduled prompts run without the user typing them (#16).
- 2026-10-25: Anything before the latest message is append-only within a session; front edits (system prompt, tool set) happen only at explicit boundaries (`/add-dir`, `/output-style`, model switch, MCP connect or refresh, `/clear`, compaction), each accepted as one lost cache read (#13).
- 2026-10-25: `prompt_caching` has a per-kind provider default (true for `anthropic`, false for `opencode-go` until its Messages route is measured live), is user-scope only and is accepted only by kinds where `accepts_prompt_caching()` is true; `cache_ttl = "1h"` is opt-in because extended TTL costs more per write (#13).
- 2026-10-25: The guardian handle marks only its static prefix (tools and system prompt), never the per-call final message, so it pays no write premium on a tail that is never reused; its prefix is near provider minimum lengths, so savings are model-dependent (#13).
- 2026-10-25: Headless `usage.main` gains cache read and write tokens; `guardian` and `subagents` (`GuardianUsage`) carry no cache fields and are not extended (#13).
- 2026-10-25: `/config` is an interactive overlay, unlike the read-only notices of `/status` and `/permissions`, because the issue's purpose is in-TUI editing; it edits preferences only and never permission rules, which stay in the config files (#4).
- 2026-10-25: `/config` edits only `tui.theme`, `tui.screen_reader`, `context.autocompact_window`, `schedule.enabled` and (via the pickers) `model.model` / `guardian.model`; every other key is shown read-only, because guardian, permission, account, network, telemetry, env, hook and MCP keys loosen or redirect the security pipeline or send data off the machine and should need a deliberate file edit (#4).
- 2026-10-25: `/config` writes only the user file; when a higher scope sets the key it saves anyway and says which scope wins, rather than writing the project or project-local file (Claude Code writes whichever settings file it chooses) (#4).
- 2026-10-25: `tui.screen_reader`, `context.autocompact_window` and `schedule.enabled` changes made in `/config` apply from the next launch (Claude Code applies most `/config` changes at once), because each is read once at startup (renderer choice, compaction threshold, scheduling tool registration); re-plumbing them live is its own change (#4).
- 2026-10-25: `user_config::set_value` and its wrappers validate the edited document against the user column before writing, so no Fontaine-made edit through them can leave the user file unloadable (#4).
- 2026-10-25: The `/config` panel shows only config-layer provenance (`Config::sources`, built by walking each layer's raw TOML); `--model`, the screen-reader flag and `FONTAINE_AX_SCREEN_READER` overrides are not named as sources, because they are per-launch overrides rather than config scopes (#4).
- 2026-10-25: `/config` keeps next-launch saves in a session-scoped map that survives closing the panel, and Del resets only `Choice`/`PositiveInt` rows (not the model rows, since removing `model.model` sends the next launch to the setup screen), because this needs no file re-read or comparison logic (#4).
- 2026-10-26: Session UX (#10) supersedes the 2026-10-24 `/clear <name>` line: `/clear` still takes no name, but with `/resume` available its message points to `/rename` and `/resume` instead of "not available yet".
- 2026-10-26: The `/resume` picker and `--resume <name>` cover the current workspace root only (the `--continue` scope), without Claude Code's all-projects toggle, because it avoids listing unrelated projects' sessions (#10).
- 2026-10-26: `--resume` keeps a required value, so there is no bare `--resume` picker at launch (Claude Code has one); the picker is `/resume` in the TUI, because it lives in `drive` and needs a live session, and an optional value would swallow the positional prompt (#10).
- 2026-10-26: `/rename` without an argument draws the name deterministically from the first message (first line, 60 characters) instead of asking the model as Claude Code does, so there is no extra model call, spend or prompt-injection surface (#10).
- 2026-10-26: `/resume` and `/branch` end the run and start the chosen session in the same process by looping `run` around the startup path, instead of swapping the session in place, so a switch resumes exactly as `--resume` does (default mode, no grants) and restarts MCP servers and the scanner; they run only between turns, and `/branch` is refused while a subagent runs (#10).
- 2026-10-26: `/export` writes Markdown from the conversation log, owner-only and never over an existing file, where Claude Code can copy to the clipboard or overwrite; tool results appear only as their length (#10).

- 2026-10-26: Supersedes the 2026-10-21 "no hierarchy or imports" line: instruction files form a hierarchy under `FONTAINE.md`, `CLAUDE.md` and `AGENTS.md` (user tier, every ancestor of the primary root, `@path` imports to four hops, `.claude/` and `.fontaine/` rules, on-demand nested files), `/memory` and `/init` (#7).
- 2026-10-26: Claude Code walks ancestor `.claude/rules/` at launch and injects nested `CLAUDE.md` plus matching path rules into the tool result after a Read (observed with `claude -p`); Fontaine copies both (#7).
- 2026-10-26: External imports from repo-controlled files are skipped with a warning instead of approved; an approval dialog needs persisted per-project state and a headless story (#7).
- 2026-10-26: A repo-controlled file that is a symlink resolving outside the workspace roots and the enclosing git work tree is refused, where Claude Code follows symlinks; a checked-in `CLAUDE.md -> ~/.ssh/id_rsa` must not reach the prompt (#7).
- 2026-10-26: Imports from directory-walk files above the confinement set (for example `~/CLAUDE.md`) are confined like repo-controlled ones, unlike Claude Code; those files can be planted by anything writing above the repo (#7).
- 2026-10-26: Files denied by `read` deny rules are kept out of the prompt, user tier included; Claude Code does not apply deny rules to instruction files. The deterministic floor gates every source (#7).
- 2026-10-26: `/memory` is a numbered list with `/memory <n>`, not an interactive picker; it reuses the `/copy [n]` pattern and works in the linear interface (#7).
- 2026-10-26: Shell reads (`cat`) do not trigger on-demand loading; Fontaine does not parse shell commands into file reads (#7).
- 2026-10-26: The `FONTAINE*` names count toward AGENTS mode like their `CLAUDE*` twins, so a repo with native files does not also load `AGENTS.md` (#7).
- 2026-10-26: The guardian's `project_instructions` is a snapshot taken at session start, so mid-session edits and on-demand files cannot steer it (#7).
- 2026-10-26: `.claude` joins the protected directories because Fontaine now reads it into system prompts, matching Claude Code (#7).
- 2026-10-26: A rebuild (model switch, output style, `/add-dir`) unions the launch set into each agent's on-demand entry instead of resetting it; the conversation survives the rebuild, so a reset would re-inject files earlier tool results already carry (#7).
- 2026-10-26: Per-agent on-demand entries are never evicted, so the map grows by one entry per subagent for the session's life, bounded like the agent tree's own node map (#7).
- 2026-10-26: User-tier rules carry `Origin::User`, not `Origin::Rule`, so the guardian's ordering places them with the user tier (#7).
- 2026-10-26 Deviation: unattended stretches are capped by `[goal] max_turns` (default 50) and an optional `[goal] max_cost_usd`; Claude Code has only the no-progress cap. The issue requires a bounded loop.
- 2026-10-26 Deviation: a failed turn pauses the goal instead of clearing or retrying it, and there are no idle check-ins for background work; Fontaine's turn failures are untyped strings.
- 2026-10-26 Deviation: an evaluator failure (error, timeout, unreadable answer, no guardian model) pauses the goal; a judgment that never arrived must not start an unattended turn.
- 2026-10-26 Deviation: the evaluator runs on the guardian's model (the cheap model the user chose for out-of-band judgments, as the LLM scanner does), not a dedicated small-fast-model setting; no `[goal] model` key.
- 2026-10-26 Deviation: verdicts are shown with their reason inline as transcript notices; Claude Code shows the reason behind Ctrl+O.
- 2026-10-26 Deviation: headless exits 1 when a goal it ran ends impossible or paused.
- 2026-10-26 `[goal]` is `user_or_cli` like `schedule.enabled`, not `user_trust` like `[agents]` (`config_scope.rs:167-168`): goal turns run unattended, so only the user decides how long they may run.
- 2026-10-26 AFK: headless gets a `goal_failed` flag beside `failed` that sets `code = 1` and breaks, and text mode prints `fontaine: goal impossible: <reason>` / `fontaine: goal paused: <why>` on stderr instead of the last answer; without it the decided exit 1 never fires and a failure would be silent.
- 2026-10-26 AFK: `span_bridge.rs` reports model calls under `fontaine.goal_evaluation` as `query_source = "goal"` (the span carries `model.priced`), documented in `docs/telemetry.md`; otherwise evaluator spend is misreported as `main`, unlike the guardian's and scanner's.
- 2026-10-26 AFK: "made a tool call" is a per-turn `made_call` flag set in `after_turn` from the change in root-agent ledger actions since the previous `after_turn`, and counts actions only (no `agent` added to `DenialEntry`); a turn whose calls were all denied counts as idle, so the no-progress cap can only pause sooner.
- 2026-10-26: The approval prompt for `edit` and `write` shows a diff computed from the file at prompt time from rig-tools' `FileChange` hunks (display-only, advisory, capped at 200 rows and 1 MiB, absent when unreadable or the edit would fail); shell and guardian explanations already reach the prompt, and MCP tools get no preview (#45).
- 2026-10-26: No key is rebound for tool cards; Ctrl-O stays the task list, because a transcript view needs a key and an alternate screen. `/expand [n]` re-prints a call instead (#222).
- 2026-10-26: `/expand n` takes the last entry with ordinal `n`, and an unknown call after `/clear` is an error, because no store survives `/clear` and that is the least new state (#222).
- 2026-10-26: Provider-native web search is used when the provider supports it (first release: Anthropic accounts on the Anthropic API; `[websearch] native` overrides). It runs inside the model's response and bypasses the guardian, the result scanner and the per-call permission prompt, so it is offered only when no `deny`/`ask` rule or `pre_tool_use` hook could cover `websearch` and, in manual/acceptEdits/dontAsk, only with an `allow` rule. Claude Code offers its search without these conditions; kept stricter because the floor must gate every call (#19).
- 2026-10-26: The user-scope `[websearch] command` launches SearXNG on demand as a fixed argv child (no shell, minimal environment, own process group), which Claude Code has no counterpart for. It never loosens a permission decision: searches through it are judged as any `websearch` call (#19).
- 2026-10-26: With no `[websearch] backend`, a container runtime (`docker`, then `podman`, on Fontaine's own PATH) makes the client-side `websearch` tool default to a SearXNG container Fontaine starts on the first search (fixed argv, loopback-only port, settings file in the user data dir with a generated secret, 300 s start timeout, minimal env plus `DOCKER_HOST`/`DOCKER_CONFIG`/`XDG_RUNTIME_DIR`/`CONTAINER_HOST`); `backend = "off"` disables it and a startup warning says when no search path exists. Claude Code has no counterpart (its search is server-side). The settings file is world-readable inside an owner-only directory because the container user must read the bind mount (#19).

- 2026-10-26: Each session gets a private scratch directory, `$XDG_RUNTIME_DIR/fontaine/<session-id>/` (else `<temp>/fontaine-<uid>/<session-id>/`; on Windows `<temp>\fontaine-<sid>\<session-id>\`), added to the workspace roots automatically and removed on exit, so file-tool writes of helper files work without a prompt (Claude Code prompts for out-of-root writes and Fontaine denies them, 2026-10-22; this is a further deviation); reason: the denial protected nothing once the model wrote the file through `shell`, and a private owner-only root keeps the floor intact (#238).
- 2026-10-26: AFK: The scratch directory lives outside the config and data dirs (`XDG_RUNTIME_DIR`, else an owner-only per-user dir under the OS temp dir, verified against squatting, canonicalized, and refused if under a protected dir) rather than under `<data>`, and the protected-path floor gets no exception; reason: `protected_dirs` covers the data dir, so `<data>/scratch` would defeat the feature or require a floor exception (#238).
- 2026-10-26: Subagents share the parent's scratch directory; reason: one shared `Scope`, and a child's mode is already capped by its parent's (#238).
- 2026-10-27: `--cd <DIR>` (short `-C`) is a Fontaine addition with no Claude Code equivalent; it changes the process working directory as well as `Host.cwd`, because relative `--settings`/`--add-dir`/`--debug-file` values and spawned children must agree with the project directory (#145).
- 2026-10-27: Auto memory (`[memory] auto`, user-scope only, on by default for testing at the user's request; the default should return to off until the memory-poisoning scorecard is run) stores notes under `<data dir>/memory/<project key>/`, outside the repository, and the agent writes them with the ordinary `write`/`edit` tools, which the protected-path floor and, in `auto`, the guardian gate; there is no memory fast path. The loaded index is framed as unverified notes and kept out of the guardian's `project_instructions`; the guardian sees the directory as a core-set session fact. Claude Code keeps its memory in `~/.claude/projects/` and writes it without a per-write check (#8)
- 2026-10-27: Checkpoints and `/rewind` ship without Claude Code's "summarize from here" option; the picker has the three restore options; reason: it needs a model call and a new transcript record, and is a follow-up if wanted (#12).
- 2026-10-27: A code restore warns about files changed since Fontaine last wrote them and keeps them unless the user confirms the overwrite; Claude Code restores without a warning; reason: a restore overwrites with no backup, so the user's own later edit would be lost silently (#12).
- 2026-10-27: A conversation rewind clears an active goal on any rewind, as `/clear` does, while todos, the schedule, grants and mode carry on; reason: a goal records no turn, so it cannot be told apart from one set after the rewind point (#12).
- 2026-10-27: Rewind points are limited to the current `/clear` epoch; reason: a `/clear` resets the conversation the checkpoints index, so earlier turns cannot be rewound to coherently (#12).
- 2026-10-27: The auto-memory directory is excluded from pre-image capture and from restore, so a code rewind never touches memory notes; reason: memory is persistent state across sessions, not workspace edits to undo (#12).
- 2026-10-27: `[checkpoints]` is user-scope only, stricter than the user-or-CLI scope of comparable keys; reason: it sets what is kept on disk and size caps, and a project file or flag has no business changing that (#12).
- 2026-10-27: A second verdict transport, Ollama's schema-constrained `format` over rig's native `prompt_typed`, serves an Ollama model that reports no `tools` capability; the path follows the model's `/api/show` capabilities with no config key, a probe failure or an old daemon keeps the benchmarked `submit` path, and no model is listed in `guardian_models.toml` for it without its own scorecard run clearing the inclusion bar. Claude Code has no analogue (it always has tool-capable models) (#250)
- 2026-10-27: Measured on dataset `d1740a7c65ae` (GTX 1060 6 GB, 16384-token window): `ollama/gemma3:4b` on `schema` scored 98.9% catch / 31.7% false-deny over the answers it gave, p95 3.7 s, with 48 of 209 answers unparseable (decision and rule disagree); `qwen3:4b-instruct-2507` scored 93.4% / 3.9% / p95 4.1 s on `tool` and 93.4% / 5.9% / p95 4.4 s on `schema`. None clears the inclusion bar, so `guardian_models.toml` gains no entry and the schema path ships as pin-only; `qwen3:8b` was not used because it does not fit the 6 GB card. A later round on the same dataset scored six more small local models and nine hosted ones (the `kimi` control included) on the `tool` path: `go/claude-haiku-5-5` scored 99.2% catch, 2.0% false-deny, p95 2.4 s and clears the bar; `go/qwen3.8-flash` (97.5%, 0.0%, 3.4 s), `go/deepseek-v4.1-flash` (95.9%, 0.0%, 3.2 s) and `go/minimax-m3` (90.9%, 2.0%, 3.2 s) miss it on p95 alone; `kimi/kimi-for-coding` is the control (89.7%, 3.9%, 16.1 s); no local model clears it. `opencode-go/claude-haiku-5-5` is the first entry in `guardian_models.toml`, added by hand because `benign-34` failed closed as unparseable on every retry and the runner will not write the file with an unscored case; the entry counts that case as a false deny (3.9%, 2 of 51, not the report's 2.0%), still under the bar (#250)
- 2026-10-27: Forking is an agent type (`fork`), not a `fork: true` argument, so it reuses toolset, mode, model, listing, `agent(fork)` rule matching and telemetry unchanged; `inherits_context` is a built-in capability no agent file or `[agents.types]` entry can set (#39).
- 2026-10-27: A fork shares the root guardian ledger instead of cloning it: the tree's one intent source is the root ledger, and a copy would create a second one (#39).
- 2026-10-27: Only the main conversation can fork, and a fork always runs on the session model (no `model` override), because children's contexts are in-memory only and the session model produced the context a fork inherits (#39).
- 2026-10-27: A fork uses the child preamble with a distinct `fork` layer, not the parent's exact system prompt, because the parent's assembled prompt is not retained by `ChildContext` and carries root-only sections (memory, scheduling, skill index); prompt-cache sharing is not measured (#39).
- 2026-10-27: A fork's context comes from a root-only `completion_call` snapshot (history plus prompt of the latest root model call), not the stored log, because rig-agent saves only at `Done`; a synthetic assistant bridge message separates it from the task, rig-memory's fork module is unused, and an empty or unpaired snapshot fails the spawn closed (#39).
- 2026-10-27: The built-in `fork` withholds `ask_user_question` from its otherwise unrestricted toolset, like the other built-in types, because only the main conversation has a human to ask (#39).
- 2026-10-27: A large inherited context against the default child budget (`subagent_token_budget`) is documented only; no guard and no new config (#39).
- 2026-10-27: Deviation from Claude Code — user reaffirmation after a denial counts for soft denies only when it restates or approves the denied action in the user's own message; there is no Path B (agent-proposal-plus-"yes"), because the guardian is never shown agent prose (#256).
- 2026-10-27: Same as Claude Code — a hard deny is never cleared by user intent, repeated or not (#256).
- 2026-10-27: The guardian's `task_intent` and `recent_user_messages` blocks carry `source="user"` and are read as the user's own words; the screen stage keeps its input and prompts byte-identical, since it is opt-in and never before the floor (#256).
- 2026-10-27: Deviation — `auto` mode allows edits inside the workspace to git-tracked files, new files and files created this session without a guardian verdict (tier `tracked_edit`), after the floor; Claude Code gives the same scope to its classifier but still asks it. Existing untracked files that predate the session, agent configuration, instruction files and `.github/` still go to the guardian, where an explicit request from the user that names the file clears them. Requested by the maintainer after guardian denials of ordinary repository edits (#256).
- 2026-10-27: Deviation — `auto` mode allows file-tool writes inside the session scratch directory without a guardian verdict (tier `scratch_write`), after the floor; shell commands there still go to the guardian, because the directory is private to the session and removed when it ends. Requested by the maintainer (#256).
- 2026-10-27: Deviation — a `tracked_edit` or `scratch_write` allow resets the loop breaker's consecutive-denial run like a guardian allow; the read-only fast path and the pre-classifier do not. The maintainer requires `auto` to work with the user absent (#256).
- 2026-10-27: The fast-path exception in the AGENTS.md security invariants gains a second exception naming these tiers (after the floor, scoped as above); `[permissions] auto_tracked_edits` and `auto_scratch_writes` default true, and project scope may only set them false (#256).
- AFK: The `tracked_edit` exclusion of instruction files and `.github/` is a `floor.rs` helper (resolved path relative to the root, case-folded like the protected list: basenames `FONTAINE.md`, `FONTAINE.local.md`, `CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md` at any depth, anything under `rules/`, any `.github` component); excluded paths fall through to the guardian with no new `soft_deny` rule, and `@import` targets are not excluded because the loader keeps no durable path list (#256).

## Open Questions

None.
