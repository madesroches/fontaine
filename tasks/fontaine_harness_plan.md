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

`research/` holds seventeen companion docs that are this plan's precedent and inputs:
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
under `research/oh-my-pi-harness/`), `research-claude-code-auto-mode-prompt.md` (verbatim capture of Claude Code's auto-mode classifier prompt),
`research-hermes-harness.md` (study of the Hermes harness's cron, watcher and gateway orchestration, with
scripts under `research/hermes-harness/`), `research-kimi-harness.md` (study of Moonshot's Kimi Code CLI:
what lets the model iterate, Kimi-model-specific support, telemetry, experiments under
`research/kimi-harness/`), and the Jev screen studies `analysis-jev-improvements.md`,
`analysis-jev-openrouter.md` and `analysis-screener-models.md`.

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
  - [ ] Release signing (#425): every archive on the `release` and `dev` channels signed with a
        Fontaine-owned minisign key, verified before publication; the maintainer bootstraps the keys
        and the `release-signing` / `dev-signing` environments (`CONTRIBUTING.md` §Releasing). Blocks #175, #176 and #109.
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
`.github/workflows/release.yml` (cargo-dist release build, signed and draft-verified before publication; see `design/platforms.md` §Releases),
`.github/workflows/dev.yml` (the signed dev channel), `.github/workflows/docs.yml` (builds and link-checks the mdBook site from `docs/`, deploys it to GitHub Pages; `.github/scripts/docs-site.sh`), `.github/scripts/` (minisign install, sign, verify and dev-installer rewrite scripts), `rust/fontaine/keys/` (the channels' public keys) and
`.github/workflows/ci.yml` (GitHub Actions; matrix `ubuntu-latest`, `macos-latest`, `windows-latest`, each job installs the toolchain pinned in `rust/rust-toolchain.toml` and from `rust/` runs `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo test --doc`, `cargo doc --no-deps --workspace` with `RUSTDOCFLAGS=-D warnings` (plus a Linux-only `--features fontaine/pg2` run), `cargo deny check` (version-pinned cargo-deny, config `rust/deny.toml`), then the offline eval fixture run (Phase 3 adds the `--guardian none` eval run: `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian none`; the release plan (`dist plan`) runs once per PR, in the Release workflow's `plan` job, not in `ci.yml`; Phase 6 adds `cargo clippy --all-targets --features fontaine/pg2 -- -D warnings` and `cargo test --features fontaine/pg2`, Linux only, with `cargo check --features fontaine/pg2` on macOS and Windows; the two `fixture` eval runs and the "Release signing" step (real minisign CLI and scripts against disposable keys, then the ignored `release_signature` interop test) are Linux only; a `changes` job skips the Rust steps for PRs touching only `tasks/` or `AGENTS.md`; PR runs restore but do not save the cache, and superseded PR runs are cancelled) — see
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
│   │                           #   classes; workspace-scope validation over `rig_tools::fs::place`; confined rig tool config;
│   │                           #   MCP servers (`mcp`): rig-rmcp wiring, tools registered as `RiskClass::Arbitrary`
│   ├── fontaine-guardian/      # the guardian agent: verdict protocol (rig extractor),
│   │                           #   prompt builder, rules config, caching, fail-closed logic, result scanner;
│   │                           #   `prompts/` (guardian, minimal, scanner) + `data/guardian_models.toml`
│   ├── fontaine-agents/        # parallel subagent orchestration on rig-agent: spawn/list/
│   │                           #   interrupt, per-type tool registries, budgets (v1); defines the
│   │                           #   `ChildRunner` trait (implemented by core)
│   ├── fontaine-tui/           # ratatui app: chat, approval prompts, denial/retry UI, modes
│   ├── fontaine-evals/         # eval runner (bin + lib crate, same split): scores guardian verdicts against the dataset
│   └── fontaine-testkit/       # dev-only test helper crate: builds/locates Cargo example binaries
│                               #   (`example_bin`) for integration tests
├── evals/                      # golden dataset (`evals/dataset/*.jsonl`) + README; runner is rust/fontaine-evals
├── analytics/                  # micromegas notebooks (`notebooks/fontaine-overview.json`), applied with `micromegas-screens`
├── docs/                       # all product docs — one topic per page, website-ready (see `design/documentation.md` §Documentation);
│                               #   `schemas/headless-v1.schema.json`
├── tasks/                      # this master plan + design/ (topic design docs) + completed/ (shipped plans, incl. phases/)
└── research/                   # seventeen analysis/research companion docs
```

Crate dependency graph (arrows point at dependencies; no cycles):

```
fontaine (bin+lib)       ─▶ fontaine-core, fontaine-tui
fontaine-evals (bin+lib) ─▶ fontaine-core, fontaine-guardian, fontaine-tools
fontaine-tui          ─▶ fontaine-core
fontaine-core         ─▶ fontaine-tools, fontaine-guardian, fontaine-agents
fontaine-agents       ─▶ fontaine-tools
fontaine-guardian     ─▶ (no Fontaine crates)
fontaine-tools        ─▶ (no Fontaine crates — never fontaine-core)
fontaine-testkit      ─▶ (no Fontaine crates; dev-dependency of fontaine, fontaine-core, fontaine-tools, fontaine-tui)
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

Grouped by subject under the `area:` label set (`Agents`, `Config`, `Guardian`, `Permissions`,
`Tools`, `TUI`, …), plus `Build and release`, `Evals` and `Project` for the rest. A new line goes
at the **end** of its subject's subsection, so branches on different subjects never edit the same
spot; add a subsection, in alphabetical order, when no existing one fits.

### Agents

- 2026-10-21 (supersedes "no subagents in v1"): Parallel multi-agent management is a v1 requirement, Claude Code-style; agent-to-agent messages go through the same guardian gate. (User directive 3.)
- 2026-10-21: Deviation — a child's effective mode is never looser than its parent's (Claude Code lets a parent set a child `auto`); Phase 5 "Parallel agent orchestration".
- 2026-10-21: Subagent nesting and budgets mirror Claude Code's documented rules (not a deviation); Phase 5.
- 2026-10-21 (user): Project agent files (`.fontaine/agents/*.md`) are allowed but only narrow (subset toolset, `manual`/`dontAsk`, no `model`). Deviation from Claude Code; reason: a checked-in repo must not widen a child's authority or pick its endpoint.
- 2026-10-21 (user): `fontaine-agents` defines `ChildRunner` and `LedgerView` traits implemented and injected by `fontaine-core`, the same inversion as `ToolGate`.
- 2026-10-22: The `fontaine-agents` ledger trait is named `RootLedger`; reason: `LedgerView` is already a `fontaine-core` struct.
- 2026-10-21 (user): The `agent` tool blocks until the child finishes (Claude Code parity); `background: true` returns `{agent_id}` and delivers the report as a notification (Phase 5).
- 2026-10-21 (user): `send_message` reaches parent, children, siblings and `"main"`; delivery is queued, untrusted and sender-marked (Phase 5).
- 2026-10-21: While a parent is in `plan`, every child's edits are blocked and its non-edit calls use the tighter of its own mode and `manual` (Phase 5).
- 2026-10-21: A cost cap (`subagent_cost_budget_usd`, `session_cost_budget_usd`) on a model with no catalog price is inactive with a one-time notice and the token cap still binds; an explicit `--max-budget-usd` on an unpriced model stays a usage error (exit 2) (Phase 5 Budgets).
- 2026-10-21: Deviation — user-scope agent files accept `permissionMode` only as `inherit`/`default`/`dontAsk`; any other value is a load error (exit 2) naming the file; reason: same mode rule as `[agents.types]`, stricter than Claude Code.
- 2026-10-22: Deviation (project-agent-file types only) — the `agent` tool's per-call `model` is an error tool result for a type defined by a project agent file; elsewhere Claude Code's precedence (per-call > type > session) holds; reason: a repo-defined type must not end up on a model the repo steered (Phase 5).
- 2026-10-26 Deviation: unattended stretches are capped by `[goal] max_turns` (default 50) and an optional `[goal] max_cost_usd`; Claude Code has only the no-progress cap. The issue requires a bounded loop.
- 2026-10-26 Deviation: a failed turn pauses the goal instead of clearing or retrying it, and there are no idle check-ins for background work; Fontaine's turn failures are untyped strings.
- 2026-10-26 Deviation: an evaluator failure (error, timeout, unreadable answer, no guardian model) pauses the goal; a judgment that never arrived must not start an unattended turn.
- 2026-10-26 Deviation: the evaluator runs on the guardian's model (the cheap model the user chose for out-of-band judgments, as the LLM scanner does), not a dedicated small-fast-model setting; no `[goal] model` key.
- 2026-10-26 Deviation: verdicts are shown with their reason inline as transcript notices; Claude Code shows the reason behind Ctrl+O. Since 2026-11-04 Fontaine's Ctrl-O transcript view also shows the complete reason (#233).
- 2026-10-26 Deviation: headless exits 1 when a goal it ran ends impossible or paused.
- 2026-10-26 `[goal]` is `user_or_cli` like `schedule.enabled`, not `user_trust` like `[agents]` (`config_scope.rs:167-168`): goal turns run unattended, so only the user decides how long they may run.
- 2026-10-26 AFK: headless gets a `goal_failed` flag beside `failed` that sets `code = 1` and breaks, and text mode prints `fontaine: goal impossible: <reason>` / `fontaine: goal paused: <why>` on stderr instead of the last answer; without it the decided exit 1 never fires and a failure would be silent.
- 2026-10-26 AFK: `span_bridge.rs` reports model calls under `fontaine.goal_evaluation` as `query_source = "goal"` (the span carries `model.priced`), documented in `docs/telemetry.md`; otherwise evaluator spend is misreported as `main`, unlike the guardian's and scanner's.
- 2026-10-26 AFK: "made a tool call" is a per-turn `made_call` flag set in `after_turn` from the change in root-agent ledger actions since the previous `after_turn`, and counts actions only (no `agent` added to `DenialEntry`); a turn whose calls were all denied counts as idle, so the no-progress cap can only pause sooner.
- 2026-10-27: Forking is an agent type (`fork`), not a `fork: true` argument, so it reuses toolset, mode, model, listing, `agent(fork)` rule matching and telemetry unchanged; `inherits_context` is a built-in capability no agent file or `[agents.types]` entry can set (#39).
- 2026-10-27: A fork shares the root guardian ledger instead of cloning it: the tree's one intent source is the root ledger, and a copy would create a second one (#39).
- 2026-10-27: Only the main conversation can fork, and a fork always runs on the session model (no `model` override), because children's contexts are in-memory only and the session model produced the context a fork inherits (#39).
- 2026-10-27: A fork uses the child preamble with a distinct `fork` layer, not the parent's exact system prompt, because the parent's assembled prompt is not retained by `ChildContext` and carries root-only sections (memory, scheduling, skill index); prompt-cache sharing is not measured (#39).
- 2026-10-27: A fork's context comes from a root-only `completion_call` snapshot (history plus prompt of the latest root model call), not the stored log, because rig-agent saves only at `Done`; a synthetic assistant bridge message separates it from the task, rig-memory's fork module is unused, and an empty or unpaired snapshot fails the spawn closed (#39).
- 2026-10-27: The built-in `fork` withholds `ask_user_question` from its otherwise unrestricted toolset, like the other built-in types, because only the main conversation has a human to ask (#39).
- 2026-10-27: A large inherited context against the default child budget (`subagent_token_budget`) is documented only; no guard and no new config (#39).
- 2026-10-28: No default per-subagent token cap at all — not a raised one; `subagent_token_budget` is opt-in only, for Claude Code parity (#285).
- 2026-10-28: `subagent_cost_budget_usd` is likewise opt-in only — no default `$2.0`; no per-subagent budget binds unless the user sets one (#285).
- 2026-10-30: Deviation from Claude Code: Fontaine gives the model a blocking wait on background subagents, where Claude Code 2.1.314 delivers their results only as notifications, because a Fontaine subagent can have background children and cannot end its turn to wait (finishing kills its descendants), while Claude Code's subagents cannot nest (#301).
- 2026-10-30: The wait is `job_output`, extended to `agent-*` ids, rather than a new tool, mirroring the unified `TaskOutput` Claude Code had before removing it; `job_output` takes the legacy aliases `BashOutputTool`, `AgentOutput` and `AgentOutputTool` (#301).
- 2026-10-30: `job_output`'s `wait_secs` has no ceiling, because the model chooses the timeout; an omitted value means "until it ends" for a subagent and "now" for a job, and any wait returns early on a message or a user interjection. Claude Code's removed `TaskOutput` capped waits at 600 s (#301).
- 2026-10-30: A subagent may call `job_output` only with the id of a subagent it started; jobs and the list stay root-only. This amends the 2026-10-27 decision that background shell jobs and `monitor` are root-only (#301).
- 2026-10-30: Deviation from Claude Code (#321) — a killed or budget-exhausted agent cannot be resumed by typing into its view (Claude Code lets the user resume a stopped row within 30 s); reason: the human's kill and the budget cap stay final.
- 2026-10-30: `message_agent`'s resume branch gets the handle through `AgentTree::handle_of(id)`, with no id-to-`ChildRun` map in `CoreChildRunner`; reason: the tree already owns the handle (#321).
- 2026-10-30: The agent sink carries `(String, AgentViewEvent)` with `AgentViewEvent { Prompt(String), Event(SessionEvent) }` and no new `SessionEvent` variant is added; with no sink installed (headless, evals) child decisions and previews stay on the root feed as before (#321).
- 2026-10-30: Human messages left in an agent's queue after a run are drained through `ChildHandle::take_missed` after `conclude`, and a refused automatic resume warns with the undelivered text; reason: a late `finish` then cannot end the new run, and the human is told what was not delivered (#321).
- 2026-10-30: `message_agent` re-checks `is_running` after `queue_for` and takes its message back to resume if the run ended, and `AgentTree::resume` refuses an already-running node atomically (`Refusal::AlreadyRunning`); reason: the tree and steering locks are separate, so a run can end between them (#321).
- 2026-10-30 (user, supersedes "2026-10-21 (user): The `agent` tool blocks until the child finishes"): Deviation from Claude Code — every `agent` spawn returns `{agent_id}` at once and the report arrives as a notification or through `job_output`; there is no blocking spawn and no `background` argument; reason: subagents must run concurrently with their parent and each other, and waiting is the parent's explicit choice (#326).
- 2026-10-30 (user): A subagent that answers while its own subagents run joins them before it ends: its run waits for their reports (or a message, or the human's words) and takes a further turn on each batch, as a headless root does; only a killed, failed or budget-exhausted subagent's descendants are killed. This amends the 2026-10-30 #301 reasoning that "finishing kills its descendants" (#326).
- 2026-10-30: Interrupting a turn no longer kills any subagent, since no call waits on one; the issue's "interrupting the turn kills every in-flight child" is superseded by the always-async spawn, and the agents panel stays the way to kill one (#326).
- 2026-10-30: Tool calls of a turn keep running one at a time (`tool_concurrency(1)`); concurrency comes from the always-returning spawn, so the gate's in-order judgment needs no pre-pass (#326).
- 2026-10-30: A subagent's join rounds are bounded only by its budget and the kill switch, like the headless root's `settle`; no separate round cap (#326).
- 2026-10-30 AFK: Keep the dedicated "Concurrency (acceptance 1)" integration test rather than folding it into `children_run_at_once_up_to_max_parallel_and_the_next_spawn_is_refused`; reason: that test's children are single-request routes, so a "B's first request before A's last" assertion would mean reworking its routes, and keeping the cap test and the order test separate leaves the settled acceptance coverage unchanged (#326).
- 2026-10-31: A `job_output` wait on a shell job returns only when the job ends, the wait expires, or a message arrives, never on new output, so a chatty job cannot cut a long wait short; line-by-line delivery stays with `monitor`. Matches Claude Code's `TaskOutput` `block: true` (#336).
- 2026-10-31: A killed subagent persists its partial transcript: graceful settle (KillHook at rig's `on_completion_call`/`on_dispatch` boundaries) plus a bounded `KILL_GRACE` with the instantaneous drive-drop kept only as the grace-expiry backstop, a kill-select at the `gated` choke point (`KillSwitches` in `fontaine-tools`) for in-flight tool calls and permission waits, and the exit path's `tree.settled` waits raised to `KILL_GRACE` + 5 s; always on, no config gate (#348).
- 2026-11-01: A subagent's shell starts at the primary root and never inherits the parent's tracked
  directory; its own `cd` is tracked per agent id and persists across its own calls, never moving the
  parent's (#26). Claude Code starts a subagent in the main conversation's current directory and does
  not persist its `cd`. Starting at the primary root keeps the child's start independent of the
  parent's mutable state; a `fork` subagent follows the same rule, so its inherited history may show
  a parent `cd` that does not apply to it.
- 2026-11-01: Deviation from Claude Code (#290) — a taken subagent name is refused and names are never
  reused in a session; Claude Code lets the latest holder win. Reason: a late message cannot reach the
  wrong agent.
- 2026-11-01: Deviation from Claude Code (#290) — the `agent` tool takes an optional `name`; Claude
  Code's Agent tool has none. Reason: the model gets addressable roles without a naming step in the
  harness.
- 2026-11-01: Deviation from Claude Code (#290) — an unnamed subagent gets a generated `<type>-<k>`
  name; Claude Code addresses unnamed agents only by id. Reason: every agent is readable and
  addressable.
- 2026-11-01: Deviation from Claude Code (#290) — a name is 1-32 characters of `[a-z0-9-]`, starting
  with a letter, and is not `main`, `root`, `job-*` or `agent-<n>`. Reason: safe to embed as untrusted
  text and free of collisions with agent ids and job ids.
- 2026-11-02: A per-type `effort` (`[agents.types.<name>]` and agent-file frontmatter) inherits the session's level when unset and may differ from the parent's, since effort is not a permission; frontmatter `effort` is allowed in user-scope agent files only (a repository or plugin file may write `inherit`), like `model`.
- 2026-11-02: Agent teams (#40) are on by default and `[agents] teams = false` turns them off (reason: the feature should work without setup). Like the other `[agents]` keys, shared project settings cannot set it. The lead is always `main`, there is one team per session, and only `main` starts a teammate. A teammate is an ordinary subagent with an idle state, not an independent session as in Claude Code (deviation; reason: one permission pipeline and one budget tree). A teammate's finished turn leaves it `idle`, which holds its name and tasks but no `max_parallel` slot.
- 2026-11-02: The team's shared task list is called the board (`board_show`, `board_update`) to keep it apart from `todo_write` and the `/tasks` alias of `/agents`. `board_show` is read-only; `board_update` is `Arbitrary` and `Destructive::Always` like `send_message`, guardian-judged in `auto` and `plan`, with no new tier and no fast path. Task text is untrusted agent-authored data and never guardian intent.
- 2026-11-02: What wakes an idle teammate (a peer message, the idle hand-off note, a `teammate_idle` hook's reason) is delivered without recording a user message, so it never becomes guardian intent; only the human typing in its view is recorded. The hand-off and hook-block resumes are capped at 3 in a row with no board change (a constant, not a config key); reason: the loops sustain themselves.
- 2026-11-02: The `task_completed` and `teammate_idle` hooks can only block, never allow, take no matcher, and a hook that fails or times out warns and blocks nothing; a repository's `teammate_idle` hook is ignored with a warning (it forces spend), while its `task_completed` hook loads and can only keep a task open. Reason: quality gates add work, they must not wedge the team or loosen anything.
- 2026-11-02: No tmux split-pane display and no per-teammate model beyond what `agent` already has; both are Claude Code display or ergonomics features outside #40.
- 2026-11-02 (user): Deviation — once the team board has tasks, all completed, and no teammate is running, idle teammates are retired (ended `done`, off the agent list) instead of staying idle until killed; reason: a finished team should not linger in the list, and a later task spawns a fresh teammate.
- 2026-11-03: A subagent's in-flight messages go to a separate `<id>.inflight` log, emptied at settle and shown by `/export`, because rig's final save refuses a log that moved under it.
- 2026-11-03: A journal kept by an `Error` settle (the agent's log lacks the run's messages) is carried through the next run rather than discarded; the journal is cut back at settle to the whole-line byte length recorded at run start (`replace_log_async` takes raw bytes, so the prefix is exact), so nothing already journaled is lost.
- 2026-11-03: A kill loses the last assistant turn whose tools are still running, with that batch's results; accepted, no provisional record of uncommitted messages.
- 2026-11-05: A command, skill or `!` line the user queues while the root is parked in a `job_output` wait ends the wait and is named in its answer (first line only, once), but is not delivered to the model; it still runs after the turn in typed order, and a plain message queued behind it neither wakes nor is named. Reason: delivering it would reorder the user's input and make `/afk`-style text model-visible intent (#434).
- 2026-11-05 (supersedes the 2026-11-02 line "one team per session … only `main` starts a teammate … a teammate is an ordinary subagent with an idle state"): every subagent whose turn ends `done` or `failed` goes idle and keeps its conversation. Only the "one team / only `main` starts a teammate / teammate" clauses are superseded; teams on by default, `teams = false` turning off boards, the user-scope-only key, subagents not being independent sessions and idle holding no slot stay. A reachable agent's message, a report, the owner reminder or the human wakes it, and there is no `teammate` argument. This is Claude Code parity: its SendMessage resumes a completed agent from its transcript (#435).
- 2026-11-05 (supersedes the 2026-11-02 (user) board-retirement line): an idle agent retires only when an ancestor ends for good, on `/clear`, on a conversation rewind, or on an AFK stop (which also kills the running ones). There is no board-coupled retirement, timeout or cap, and the agent list hides idle rows that own no task (#435).
- 2026-11-05: `/clear` and a conversation rewind retire idle agents instead of refusing (#435).
- 2026-11-05 (user): Deviation from Claude Code, whose teammates claim their own next task. No idle agent is handed new board work. A claimable task goes to a fresh agent that the board's owner spawns after a deduplicated harness reminder. Reason: a fresh agent has a clean context, and reusing one is the owner's explicit choice through `send_message` (#435).
- 2026-11-05 (user): Deviation from Claude Code's one team per session with a fixed lead. Any agent holding the `agent` tool owns a board once it creates a task, and its children work it; an agent without it creates on its parent's board. An agent sees only its own board and its parent's. Only `main`'s board is persisted. Reason: any agent can decompose its work and spawn agents for it (#435).
- 2026-11-05: `teammate_idle` fires only for an agent going idle with an in-progress task on its parent's board, as a gate on unfinished work (#435).
- 2026-11-05 (user): A `--resume`d session rebuilds only the agents whose last end was `done` as idle, from the ledger and its per-agent log; failed agents are not rebuilt. Its handle is built lazily on the first wake, its tools and mode are re-derived (never looser than its parent's), and its per-agent spend starts over. Claude Code parity: an agent resumes from its transcript (#435).
- 2026-11-05 (amends the 2026-11-02 wake line): the idle hand-off note is no longer a wake, and the cap of 3 covers only the `teammate_idle` resume. The owner reminder is deduplicated, not capped. A wake is still never recorded as a user message (#435).
- 2026-11-06: Deviation from the 2026-10-30 "the agents panel stays the way to kill one" line: `job_cancel` also stops a subagent the caller started (any caller, own children only), matching Claude Code's `TaskStop`; a kill stays final and the human's panel kill is unchanged (#463).
- 2026-11-08: (#515) Subagent facts notices for a derived model are deduplicated per model reference through a session-wide set on the agent tree, and skip the current main and live guardian references, because a single flag would silence a second derived model and a `$main` or fallback child would re-announce the main model.
- 2026-11-08: Project and plugin agent files may name a model role or `$main` as `model`, loosening the 2026-10-21 "no `model`" rule toward Claude Code. A role names no endpoint, because the user defines every role. An undefined role runs the type on the session model, with a warning; the per-call `model` on such a type stays refused (#324).
- 2026-11-08: A skill's `model` accepts a role name and stays unused. Claude Code's per-skill model override is not adopted (#324).
- 2026-11-11: (#115) Waiting for a usage window spends no tokens or cost, so agent budgets are untouched; a node's displayed run time and wall-clock accounting subtract the pause time that fell inside its run.
- 2026-11-12: (#592) Final interactive process exit aborts nested agent tasks and their report watchers (`AgentTree::abort_all`, `ChildHandle::abort`) at once, without `settled(KILL_SETTLE)` and without a fabricated killed report, so a child that never reaches a rig hook boundary cannot hold the exit; supersedes the 2026-10-31 exit-wait choice for final interactive exit only. Ordinary kills, headless runs and `/resume`/`/branch` switches keep the graceful transcript semantics (`KILL_GRACE`, `KILL_SETTLE`). Reservation and direct resume refuse atomically once the tree is shutting down.

### Build and release

- 2026-10-21: CI runs `cargo deny check` (advisories, licenses, bans, sources) with a checked-in `rust/deny.toml` on all three OSes; cargo-deny version-pinned.
- 2026-10-21: v1 supports Linux, macOS and Windows; every CI step runs on all three OSes. Windows runs `shell` through Git Bash (Claude Code parity); details in `design/platforms.md`.
- 2026-10-21 (user): Releases — semver 0.x, cargo-dist (exact-pinned) GitHub Releases plus `cargo install --git`, no crates.io, maintainer cuts releases, Keep a Changelog (`design/platforms.md` §Releases).
- 2026-10-21: Dependency policy — exact pins (`=x.y.z`) for rig and every pre-1.0 crate, each new crate listed with its pin in the owning phase's Files line; 1.x crates (clap, tokio, serde, toml) use caret.
- 2026-10-22: Toolchain and workspace `rust-version` raised from 1.97.1 to 1.99.1 (PR #69); rig 0.45.0's 1.97.1 is the floor, not the pin.
- 2026-10-25: Narrows the 2026-10-21 "every CI step runs on all three OSes": compile, lint and test coverage stays on all three, Linux runs pg2 clippy and test, macOS and Windows run `cargo check --features fontaine/pg2`; only the pure-scoring `fixture` evals are Linux-only (`--guardian none` stays everywhere; required check names unchanged). Reason: Windows CI took ~32 min vs ~8 on Ubuntu (#178).
- 2026-11-04: CI builds with `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0`, the Linux job frees the runner's preinstalled .NET, Android, GHC and CodeQL trees first, and runs `cargo clean` before the pg2 tests. Reason: the ~140 integration-test binaries are ~250 MB each with debug info (~147 MB without) and the free GitHub runner has ~14 GB of disk, so the Linux job ran out of space linking them; the default linker (rust-lld) was not the cause and is unchanged.

- 2026-11-05: Release signing — separate Fontaine-owned minisign keys for `release` and `dev`, public keys checked in at `rust/fontaine/keys/` and compiled in, private keys only in the `release-signing` (tags, required reviewer) and `dev-signing` (main) GitHub Environments. Every archive is signed, published as a draft, re-downloaded and verified, then published. Reason: update trust anchored to Fontaine's keys (#175/#176), with a dev-key compromise unable to reach `release`.
- 2026-11-05: The dev channel publishes each verified `main` build as prerelease `dev-<run_number>`, gated on the repository variable `FONTAINE_DEV_CHANNEL`. Reason: the same draft-verify-publish gate as `release`, and no red `main` before the dev key exists.
- 2026-11-07 (#477): The cargo-dist `shell` installer ships on the `release` channel unsigned and verifies the archive's sha256 only (when `sha256sum` is present), not the minisign signature; reason: it carries no trust beyond the HTTPS fetch that delivered it, and there is no minisign on the user's machine. `release-verify.sh` accepts it on `release` only. Signing superseded by the 2026-11-07 (#487) release installer line below; the installer still does not verify itself.
- 2026-11-07 (#477): No installer on the dev channel; reason: dist bakes a `v<version>` download URL and dev tags are `dev-<n>`, so the script would 404. `dev.yml` removes it before signing and hosting. Superseded by the 2026-11-07 (#487) stable dev installer line below.
- 2026-11-07 (#477): Linux arm64 is not claimed as supported, so `aarch64-unknown-linux-gnu` is not added as a target; reason: it needs a CI runner, test coverage and a pg2 decision, which the installer issue does not cover.
- 2026-11-07 (#487): The dev channel has one stable installer address, `releases/download/dev/fontaine-installer.sh`, an asset of the long-lived prerelease `dev`; supersedes the 2026-11-07 (#477) "No installer on the dev channel" line. `dev-<n>` stays immutable and installer-free; `sign` rewrites dist's installer (`.github/scripts/dev-installer.sh`: download URL, `APP_VERSION`, receipt `version` and usage text, each matched exactly or the run fails) to fetch from its own `dev-<n>`, and `stable-installer` replaces it after `publish`.
- 2026-11-07 (#487): The `dev` tag is created once and never moved; only the release's assets change. Reason: moving a tag needs a force-push the project forbids.
- 2026-11-07 (#487): The dev installer is signed with the dev key (trusted comment `file=fontaine-installer.sh`) and uploaded beside it as `.minisig`; the installer does not verify itself (the key and sha would arrive in the same fetch), and the release installer was left unsigned (superseded by the release installer line below).
- 2026-11-07 (#487): The dev installer writes `<base>-dev.<n>+<sha7>` as the receipt version; channel detection is left to #175.
- 2026-11-07 (#487): `stable-installer` uploads only when its `dev-<n>` is the highest published dev release, so out-of-order runs and re-runs of older runs cannot regress the installer; `gh release upload --clobber` is kept and its brief 404 window, or a failed upload after the delete, is accepted, detected by the post-upload byte-compare and healed by the next run.
- 2026-11-07 (#487): Documentation shows only the dev install command until the first tagged release.
- 2026-11-07 (#487): The release installer is signed with the release key like the archives (trusted comment `channel=release file=fontaine-installer.sh`), `.minisig` uploaded by `host`, and `release-verify.sh` requires a valid one on `release` (an unsigned or mis-signed installer in the draft fails `verify-draft`); supersedes the "installer ships unsigned" part of the 2026-11-07 (#477) line. dist bakes the final `v<version>` URL, so the shell installer is signed unmodified (the PowerShell installer is patched first, see the #478 lines below); the installer still does not verify itself.
- 2026-11-07 (#478): The cargo-dist `powershell` installer ships for Windows x86_64 on both channels, and `sign` patches it with an sha256 check of the archive (`.github/scripts/windows-installer.sh`) because dist's stock script has none; reason: the installer must verify what the updater verifies. The patch uses exact-count anchors, so a dist upgrade that changes the text fails `sign`, not a user. dist's updater stays off (`install-updater` default); `fontaine update` (#175) replaces it.
- 2026-11-07 (#478): The `.ps1` is signed after it is modified (and, on `dev`, rewritten), so the signature covers exactly the uploaded bytes; this supersedes "signed unmodified" in the 2026-11-07 (#487) release installer line for the PowerShell installer only. The release `.ps1` reaches the draft through a `windows-installer` artifact that `host` copies over dist's stock one.
- 2026-11-07 (#478): The long-lived `dev` release holds both installers, each with its `.minisig`; the `dev-installer` signing kind requires both and `stable-installer` uploads all four. One rewrite script, `dev-installer.sh`, handles both installers, chosen by file extension. Supersedes the "holds only the installer" wording of the (#487) lines above.
- 2026-11-08 (#508): `fontaine --version`, `doctor`, `/status`, `/bug` and telemetry `service.version` show `<version>-dev+<sha12>[.dirty]` unless `v<version>` is among the tags at `HEAD` and tracked files are clean (or there is no git), computed in `build.rs`; Claude Code documents no dev-build format. Protocol identity strings and the statusline `version` keep the plain semver, the release signature `version=` stays tag-derived, and `.dirty` is computed at build time only.
- 2026-11-08 (#510): Supersedes the 2026-10-21 "Windows runs `shell` through Git Bash" line: Git for Windows is preferred, not a prerequisite. Without it Fontaine starts with PowerShell as the command shell (Claude Code parity); `design/platforms.md` has the table.
- 2026-11-08 (#509): `fontaine update` updates only on a matching install receipt and derives the channel from the receipt version (not config: no new user-scope key, and project config cannot flip it); it never switches channels. Windows swaps via rename-aside (`fontaine.exe` to `fontaine.exe.old`, deleted by the next update) rather than #175's separate updater process; narrower than #175, which keeps the background check, staged swap and TUI notice.
- 2026-11-08 (#509, AFK): Archive extraction uses pure-Rust `tar` + `lzma-rs` (xz) and `zip` =8.6.0 (already in `Cargo.lock`, `deflate-flate2-zlib-rs`), in-process only and exact-pinned; the most conservative choice because it needs no C toolchain and passes the deny license list.
- 2026-11-08 (#509, AFK): Update failures exit with distinct codes `EXIT_UPDATE_NOT_SELF_UPDATABLE` = 3, `EXIT_UPDATE_NETWORK` = 4, `EXIT_UPDATE_VERIFY` = 5 (config errors keep `EXIT_CONFIG` = 2); each is a kind scripts dispatch on.
- 2026-11-08 (#527): The installers use `install-path = "~/.fontaine/bin"`, not Claude Code's `~/.local/bin` or cargo-dist's `$CARGO_HOME/bin`; reason: the directory is Fontaine's alone, shared with no other tool, and trivial to remove. No automatic migration of older `$CARGO_HOME/bin` installs.

### Config

- 2026-10-21: Unknown config keys and sections are a config error (exit 2) (Claude Code ignores them): a misspelled `[permisions]` must not silently drop deny rules.
- 2026-10-21: `--settings` takes a TOML file path only (Claude Code also accepts an inline JSON string): TOML has no unambiguous inline top-level form.
- 2026-10-21: Deviation — config trust asymmetry is looser than Claude Code on the deny side: project repos may add deny/ask rules (Claude Code ignores the project `autoMode` block); reason: deny-side additions can only tighten.
- 2026-10-21: Deviation — shared project settings cannot hold allow rules, a project hook's `allow` and a project skill's `allowed-tools` are ignored; reason: project scope can only tighten. Amended 2026-11-03 (#379): a project skill can never be trusted either; a record for one in `skill-trust.toml` is ignored with a warning.
- 2026-10-21: Settings scope stack managed (reserved) > CLI > project-local > project > user (Claude Code parity). Deviation: endpoint/credential keys (`[accounts.*]`, `[telemetry]` except `enabled = false`) are never read from project-local; reason: a file in the repo must not redirect session data.
- 2026-10-21: Deviation — project and project-local scopes may set `default_mode` to `manual` or `dontAsk` only; `plan` (its guardian leg can loosen a manual user) and `acceptEdits` (a repo must not auto-approve edits) are stricter than Claude Code, and `auto` is rejected from project-local too (Claude Code parity).
- 2026-10-21: Directories come from the `dirs` crate, overridable by `FONTAINE_CONFIG_DIR` / `FONTAINE_DATA_DIR`; credentials at `<data dir>/credentials.toml`, owner-only (`design/platforms.md`).
- 2026-10-21: Workspace trust (Claude Code parity: folder trust covers the whole folder — bounded at the enclosing git work-tree root, as in Claude Code; unbounded outside a repo) — in the TUI, project and project-local executables (hooks, MCP servers, skill `!cmd`) load only after the trust prompt; headless loads them without one; they can tighten but never loosen a permission decision (`design/config.md` §Config, Workspace trust).
- 2026-10-21: Project-local `settings.local.toml` gets user trust only when untracked AND its root is trusted (Claude Code trusts `settings.local.json` unconditionally): a shipped `.git` skeleton or an enclosing repo can make a hostile file look untracked.
- 2026-10-21: Deviation — with no main model configured (no `[model] model` or `--model`) Fontaine exits with a config error (exit 2) (Claude Code has a default model); reason: Fontaine is provider-agnostic with no default account (Phase 1).
- 2026-10-25: project scope cannot set `[env]` (Claude Code honors it after trust); it cannot tighten, and a project `PATH` would shadow commands the pre-classifier and guardian reason about.
- 2026-10-25: `[env]` applies to tool and extension subprocesses only, not Fontaine's own process (Claude Code applies it to both); credential and telemetry lookups keep their explicit keys.
- 2026-11-02: `[model] effort` is `off|low|medium|high|max` (`off` is beyond Claude Code's set; `xhigh` and `minimal` are not exposed), user scope only. Unset sends nothing, so the model's own default applies, unlike a default level. `max` is passed through rig's provider mapping; Claude Code's exact `max` behavior was not re-verified. A level the model's catalog entry or its wire refuses (a generic `openai-compatible` gateway has no rig option mapping) is dropped with a one-time warning rather than failing the turn.
- 2026-11-03: `fontaine doctor` is a CLI subcommand only (Claude Code's `/doctor` is an in-session command); the TUI `/doctor` is deferred. It is read-only and exits 1 when a finding is `fail`.
- 2026-11-03: `config::load` stays fail-fast and fail-closed for sessions; only `config::diagnose`, used by `fontaine doctor`, reads every layer independently and reports each one's first error.
- 2026-11-03: `fontaine doctor` starts MCP servers only as a session's `mcp_plan` would without a prompt: repository servers are skipped in an untrusted workspace and until approved, so it never runs repository code a session would not (`--connect=no` skips the probe).
- 2026-11-03: `fontaine doctor` finding levels: only conditions that stop a session working as configured (unavailable guardian, `models::resolve` error, unreadable `trust.toml`, config, plugin, skill or memory load failures) are `fail`; an untrusted workspace, a failed MCP probe and model notices are `warn`. It adds no reachability probe of its own beyond `models::resolve`.
- 2026-11-08: `[models.roles]` is user file or `--settings` only. `[model] model` and `[models.roles] main` in one layer are a config error; across layers the higher-precedence one wins, so a project-local `[model] model` still overrides. Unknown accounts, unknown roles and cycles are load errors (#324).
- 2026-11-11: (#115) `[headless] max_usage_wait_secs` is user scope or CLI only (a repository must not make an unattended run wait longer), and `[headless] on_usage_limit` may be set to `exit` from project scopes, which only ends a run sooner. The same cap bounds a pause in the terminal UI while AFK is on, because nobody can cancel there.
- 2026-11-12: (#228) Amends the 2026-10-21 workspace-trust line: headless still loads project executables by default (Claude Code parity), and the opt-in user-scope `[headless] require_trust = true` (user file or `--settings`) makes it skip them in an untrusted workspace, with one stderr line. The headless `init` event and the ledger's `session_start` record `project_executables` (and `headless`) so the exposure is countable.

### Core

- 2026-10-25: Locks guarding permission, guardian, spend, model-selection or durable-record state panic on poison via `fontaine_tools::lock`; only display and advisory state recovers (`*_recovering`, reason at the call site), because a corrupted permission state must end the session rather than keep deciding (#186).
- 2026-10-20: Core is built on the rig crate family, pinned to exact versions; no custom provider layer. (User directive: reuse existing crates.)
- 2026-10-20: rig's Bevy-based `rig-harness`/`rig-tui` are not adopted; Fontaine keeps its own ratatui TUI.
- 2026-10-21: Revisit the core-vs-rig split if rig ships a permission-adjudication API.
- 2026-10-21: Fontaine never spawns `git` for project-local trust classification or ledger capture; `gix` (exact pin, default features off) reads tracked/ignored status and remotes, and no `.git` or a `gix` error ⇒ project scope and no remotes; reason: repo-controlled git config can execute commands.
- 2026-10-21 (user): `FONTAINE.md` v1 loads `<primary root>/FONTAINE.md` plus the user-level file; no hierarchy or imports (backlog GitHub issue #7).
- 2026-10-21: The system prompt is the versioned asset `rust/fontaine-core/prompts/system_prompt.md` layered base → `FONTAINE.md` → skill index → output style → `--append-system-prompt`; `--system-prompt` replaces only the base (Phase 2 step 2).
- 2026-10-24: A repeated-identical-tool-call reminder like dsh's `repeat-tool-reminder` (#160) may be added although Claude Code has none (measured: nine identical calls, no nudge); it must stay advisory, one user-role notice that never blocks a call or changes a permission decision, because in the #154 study it made the model stop and diagnose after three identical failing calls.
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
- 2026-10-27: `--cd <DIR>` (short `-C`) is a Fontaine addition with no Claude Code equivalent; it changes the process working directory as well as `Host.cwd`, because relative `--settings`/`--add-dir`/`--debug-file` values and spawned children must agree with the project directory (#145).
- 2026-10-27: Auto memory (`[memory] auto`, user-scope only, on by default for testing at the user's request; the default should return to off until the memory-poisoning scorecard is run) stores notes under `<data dir>/memory/<project key>/`, outside the repository, and the agent writes them with the ordinary `write`/`edit` tools, which the protected-path floor and, in `auto`, the guardian gate; there is no memory fast path. The loaded index is framed as unverified notes and kept out of the guardian's `project_instructions`; the guardian sees the directory as a core-set session fact. Claude Code keeps its memory in `~/.claude/projects/` and writes it without a per-write check (#8)

### Evals

- 2026-10-21: The eval runner is the workspace bin + lib crate `rust/fontaine-evals` (like `fontaine`: logic in `src/lib.rs`, thin `src/main.rs`, so integration tests can import it); the dataset stays in `evals/dataset/` as JSONL.
- 2026-10-21: The eval dataset gains `mode` and `workspace` fields so `pipeline` cases are reproducible.
- 2026-10-21: AgentDojo-comparable (ASR × utility) reporting is dropped; the scorecard reports per-class catch rate and false-deny rate.
- 2026-10-21 (user): The eval dataset has minimum case counts and maintainer sign-off of `expected` labels against `claude -p` baselines before the scorecard publishes (`design/testing.md` §Eval harness).
- 2026-10-21: Eval `pipeline` cases take an optional `permissions` field (`deny`/`ask`/`allow` in the `Tool(specifier)` grammar, plus `allow_extra`); Phase 0 seeds floor cases and `baseline capture` writes the equivalent `.claude/settings.json` permissions into the scratch workspace.
- 2026-11-04: Sequence-shaped eval cases (AFK `temperature` and `evasion`) get their own case type and loader, read from a file named by `--sequences <file>` (usable only with `--guardian fixture` or `live:`), because `Case` is single-call and the dataset directory is loaded whole in every CI mode; CI passes the flag only on the default-profile fixture step.

### Extensibility

- 2026-10-28: Plugin hooks' `allow`/`retry` never count and plugin skills/agents follow project
  permission rules, even though install is a user action (stricter than Claude Code). Amended
  2026-11-03 (#379): `fontaine plugin trust` (and `/skills trust`) is the way back to parity for a
  plugin skill's `allowed-tools`, per skill and pinned to a digest of the skill file; hooks and
  agent files stay tighten-only.
- 2026-10-28: Installs, updates and marketplace changes are CLI commands only; the TUI gets a
  read-only `/plugins`. No interactive marketplace browser in v1. Amended 2026-10-31 (#283): the
  one exception is the background auto-update pass below, which stages revisions the user already
  approved the executables of; installs, approvals and marketplace registration stay CLI-only.
  Superseded 2026-11-09 (#554): the TUI installs, updates, uninstalls, enables and disables
  plugins and applies them live; only marketplace registration stays CLI-only (see the 2026-11-09
  lines at the end of this section).
- 2026-10-28: LSP server components were unsupported (ignored with a warning); superseded
  2026-10-30 below. Mods stay skipped (parity doc).
- 2026-10-28: Plugins may add `deny`/`ask` rules and `hard_deny`/`soft_deny` guardian entries via
  Fontaine-only manifest keys, and nothing else that touches permissions or the guardian.
- 2026-10-28: No project or local scope can add a marketplace or install/enable a plugin;
  `plugins.toml` lives in the user config dir and is not a settings file.
- 2026-10-28: Manifest and component files are read at Claude Code's paths
  (`.claude-plugin/plugin.json`, `hooks/hooks.json`, `.mcp.json`,
  `.claude-plugin/marketplace.json`) so existing marketplaces work unmodified.
- 2026-10-28: Installs pin a commit, not a version tag; the catalog's `version` is display text.
  Plugin-to-plugin dependencies are not supported; a `dependencies` key warns at install and
  startup instead of resolving.
- 2026-10-28: `${CLAUDE_PLUGIN_ROOT}` keeps its Claude Code name for portability, expanding to the
  installed checkout.
- 2026-10-28: A path-source plugin's pin is the marketplace's pin, and `plugin update` for one
  re-copies from the current marketplace checkout, so it moves only after `marketplace update`
  (or, since 2026-10-31, after auto-update moves the marketplace, in the same pass).
- 2026-10-28: `!` commands in plugin skills run without the workspace-trust gate — install is the
  approval — and still pass the permission pipeline.
- 2026-10-28: Hook matchers support single tool names, `|` alternation and trailing wildcards; a
  matcher outside that subset skips its hook entry with a warning rather than failing the plugin.
- 2026-10-28: A plugin agent file naming an MCP server the plugin does not ship and the session
  does not have is skipped with a warning, not an exit-2 startup error.
- 2026-10-28: A local-directory marketplace pins a content hash of its working tree (`.git`
  excluded); registry mutations hold a lock file beside `plugins.toml`, clones and copies rename
  into place under the lock, and `marketplace add` refuses a duplicate registry name.
- 2026-10-28: Claude Code's plugin source types are `github`, `url` and relative paths only;
  `git-subdir`, `npm`, `archive` and `command` sources are refused with a named error.
- 2026-10-28: The catalog entry's `name` must equal the manifest's, an install error on mismatch,
  keeping namespacing and MCP server composition unambiguous.
- 2026-10-28: Bare in-plugin `mcp__<server>__*` references are rewritten to the composed
  `plugin_<plugin>_<server>` name so idiomatic plugin files keep working.
- 2026-10-28: Only shell-form `command` hooks load; other hook types are load errors and exec-form
  (`args`) entries skip with a warning. Superseded by the 2026-11-08 lifecycle-hooks lines below
  (`http` and `prompt` load; `agent` and `mcp_tool` skip with a warning; `args` still skips).
- 2026-10-28: Claude Code components with no Fontaine equivalent (`bin/`, plugin `settings.json`,
  `themes/`, `monitors/`, `workflows/`, `channels`, `userConfig`) warn at install and startup;
  `${CLAUDE_PROJECT_DIR}` expands and other unexpanded `${...}` references warn.
- 2026-10-28: Plugin and marketplace names follow Claude Code's alphabet minus `.` (letters,
  digits, `_`, `-`, starting alphanumeric); `__` stays forbidden. `.` would leak into composed
  MCP server names (`plugin_<name>_<server>`), which the MCP rule grammar rejects — a dotted
  name made any matcher on its own servers a startup error. Plugin `agents/` and `commands/`
  load recursively, with multi-segment `plugin:sub:name` names.
- 2026-10-21: The `skill` tool stays `ReadOnly` on the fast path; a skill's `!cmd` is gated as a `shell` call.
- 2026-10-22: Deviation — `hooks` and `mcpServers` in agent or skill files and `permissionMode` in skill files are load errors naming the file, and an agent's `disallowedTools` is honoured as a subtraction; reason: these files load without workspace trust, and ignoring a narrowing key would widen authority (Phase 5).
- 2026-10-30: Plugin LSP servers are now supported (reverses the 2026-10-28 "unsupported" line):
  plugins are user-installed and pinned, so they satisfy the user-scope-only rule for language
  servers (`[lsp.servers.<name>]` is user file or `--settings` only).
- 2026-10-30: Plugin LSP `workspaceFolder` is accepted and ignored with a startup warning, like
  `transport` — a valid Claude Code plugin keeps its servers, and Fontaine roots every server at
  the workspace.
- 2026-10-31 (#283): Deviation from Claude Code — a plugin revision that changes its executable
  surface is held, not applied, by auto-update. The approval fingerprint covers hook, MCP and LSP
  definitions, skill `!` lines, referenced `${VAR}` names and every non-markdown file; the user
  applies a held revision with `plugin update`, which prints the extended approval summary. Reason:
  install is the approval, and silently updating code that runs hooks and servers would void it
  (Claude Code applies the update without such a check).
- 2026-10-31 (#283): Deviation from Claude Code — the trigger is interactive start plus a 24 h
  stamp, run on a background thread and staged for the next start; Claude Code refreshes after the
  first message with a randomized delay. Reason: staging at start never delays the session and
  never changes loaded code mid-session, and the stamp keeps an offline machine from retrying on
  every start.
- 2026-10-31 (#283): Deviation from Claude Code — one global `[plugins] auto_update` (user scope
  only) plus a per-entry opt-out in the registry, set by `plugin auto-update` and `plugin
  marketplace auto-update`, instead of Claude Code's per-marketplace `autoUpdate` and environment
  variables. Reason: the global key fits Fontaine's settings scope rules, and the registry is
  where per-entry state already lives.
- 2026-10-31 (#283): Deviation from Claude Code — auto-update is on by default for every
  marketplace and plugin, where Claude Code enables it for official marketplaces only (and the
  issue asked for opt-in). Reason: the user instructed default-on; the fingerprint hold keeps the
  install-time approval intact, and plugins installed before this change are skipped until one
  manual `plugin update` records their fingerprint.
- 2026-10-31 (#283): `fontaine -p` never fetches, stages or swaps plugin updates, as Claude Code's
  headless runs do not auto-update; reason: deterministic CI and eval runs and no surprise
  network.
- 2026-10-31 (#283): Auto-update never swaps a live checkout: it stages `.staged-<name>` with
  `staged_pin` in the registry and a startup swap under the registry lock applies it before
  plugins load; leftover `.bak-*` is restored if a checkout is missing, and stale `.bak-*`,
  `.auto-*` and unrecorded `.staged-*` directories are removed. Up-to-date is decided before
  cloning (`git ls-remote`, or the catalog sha with no network), and a held pin equal to the
  remote is neither re-cloned nor re-announced. Reason: nothing executable changes under a running
  session, and the pass holds the registry lock only to record, never through git network work.
- 2026-11-01: A hook payload's `cwd` stays the workspace root (`docs/hooks.md`) where Claude Code's
  follows the shell's `cd`; the tracked directory is visible to hooks as `tool_input.cwd` (#26).
- 2026-11-03 (#379): Skill trust is per skill, with `trust <plugin>` as shorthand for the skills the
  plugin has at that time; reason: a trusted skill must not vouch for the other skills of its plugin.
- 2026-11-03 (#379): Trust is pinned to a digest of the skill file, not the plugin pin, and a
  changed file suspends the grants (a plugin skill's `allowed-tools` go off, a user skill falls back
  to its untrusted behaviour) until the user acknowledges the new version in the terminal UI;
  headless never re-acknowledges, so an untrusted or stale plugin skill lends nothing there and warns
  once; reason: the plugin pin changes with unrelated files and auto-update can change a skill's body
  with no approval step.
- 2026-11-03 (#379): Trust lives in `<config dir>/skill-trust.toml`, not `plugins.toml`; reason:
  `plugins.toml` is rewritten by auto-update and tied to the plugin lifecycle, and a separate file
  also covers user-directory skills and survives uninstall.
- 2026-11-03 AFK (#379): A trusted skill's wildcard shell rules (any `Bash(<prog> *)`, bare `Bash`) are
  guardian context (`active_skill`), never skill grants in any mode, and only its specific rules are
  allows; reason: `is_broad_code_execution` does not classify `git *` as broad and `plan` does not
  suspend broad rules, so reusing it would allow `git push --force origin main` unguarded; the global
  classifier is left unchanged.
- 2026-11-03 AFK (#379): A skill trust or untrust takes effect at the next session start, and the
  `/skills trust` dialog says so; reason: `Extensions` is immutable behind an `Arc`. Superseded
  2026-11-09 (#554): a trust or untrust also takes effect at `/reload-plugins`, which re-reads the
  trust record with the skills; the dialog says so.
- 2026-11-03 AFK (#379): Confirming `/skills trust` appends a `Record::SkillTrust` ledger and audit
  entry, while the CLI `trust`/`untrust` is unaudited like plugin install; reason: an interactive
  grant of tool permissions belongs in the record, as `Record::Approval` and `Record::GuardianOff` are.
- 2026-11-03 AFK (#379): CLI `trust`/`untrust` enumerate skills with the real config dir, a
  nonexistent sentinel `project_root` and `plugins::load_installed`; reason: user-directory skills
  must be found while project files are never read.
- 2026-11-07: The `skill` tool takes optional `args` (Claude Code's field) and accepts
  `arguments`, `argument`, `input`, `params` as aliases, non-string values coerced to text;
  deviation from Claude Code's strict schema, reason: models guess the field name and a failed
  call drops the input (#469).
- 2026-11-07 AFK: unknown fields stay rejected (`deny_unknown_fields`), as in every tools args struct; a
  silently dropped misnamed field would recreate the bug.
- 2026-11-07 AFK: `skill` is NOT accepted as an alias for `name` because permission rules match on the raw
  `name` key (`name_arg_key`); the `args` aliases are unaffected since rules never read them.
- 2026-11-07 AFK: a payload with two spellings of one field is rejected with serde's "duplicate field" error.
- 2026-11-07 AFK: model args may form `!` commands (`substitute` then `parts`, as for `/name args`); kept
  unchanged and documented, still gated as `shell`.
- 2026-11-07 AFK: skill index lines are not changed to carry `argument-hint`; the model learns of arguments
  from the tool description and the user/caller supplying them.
- 2026-11-08 (#510): On Windows without Git Bash, hooks run in PowerShell and a `pre_tool_use` hook fails closed: anything but exit 0 or 2 (a failed start, a timeout) denies the call with a reason naming the missing Git Bash, as Claude Code does; unreadable output on exit 0 still decides nothing, and the other events keep their failure handling. Reason: a POSIX hook usually fails in PowerShell, and a deny guard that silently decides nothing would loosen the user's setup.
- 2026-11-08 (#510): Skill `!` lines are not run on Windows without Git Bash (each is reported as not run); running them in PowerShell needs Claude Code's explicit `shell: powershell` frontmatter, and neither that nor a hook `shell` field is built.
- 2026-11-08 (#29): Hook events Fontaine does not build, each skipped with a warning when a plugin names it: `PermissionRequest` (an `allow` there answers the human prompt, a second loosening path next to `pre_tool_use` `allow`), `Setup` and `UserPromptExpansion` (no `--init`, and skills expand before submit), `PostToolBatch` (rig dispatches calls without a batch boundary Fontaine owns), `MessageDisplay` (display-only rewrites of model text), `InstructionsLoaded`, `ConfigChange`, `CwdChanged`, `DirectoryAdded` and `FileChanged` (no mid-session reload or file watcher; `/add-dir` and `cd` are already visible to `pre_tool_use`), `WorktreeCreate`/`WorktreeRemove` (no worktrees), `PreModelSwitch`/`PostModelSwitch` (model switches are user commands) and `ElicitationResult` (the post-response override). `Elicitation` is built; see the MCP decisions (#537).
- 2026-11-08 (#29): The `agent` and `mcp_tool` hook handler types are not built: an `mcp_tool` hook calls an MCP tool outside the per-call gate, which the floor-gates-every-call invariant forbids, and an `agent` hook is a tool-using model run, experimental in Claude Code. Command-handler `args`, `async`, `asyncRewake`, `shell`, `onFailure`, `if`, `statusMessage` and `once` are not built either: `args`, `if` and `shell: "powershell"` skip the hook with a warning, the others are silently ignored and the hook loads. Updates the 2026-10-28 command-only line: `http` and `prompt` now load.
- 2026-11-08 (#29): `http` and `prompt` hooks from project scope are ignored with a warning, as are project `stop` and `subagent_stop` hooks. Reason: they send session data off the machine or force model spend, and a repository may only tighten (extends the 2026-11-02 `teammate_idle` rule). Claude Code loads them. User config, `--settings`, an untracked `settings.local.toml` of a trusted root and plugins (install approved them) may define them.
- 2026-11-08 (#29): A `prompt` hook runs on the guardian's model, and its usage counts in the guardian's usage bucket. There is no `model` key (ignored with a warning in a plugin). Reason: Fontaine has no background-model setting, and the goal evaluator already uses this model.
- 2026-11-08 (#29): A hook block never ends the turn. A prompt hook's `ok: false` takes the event's block path, as with Claude Code's `continueOnBlock: true`; `continue`, `stopReason`, `terminalSequence` and `suppressOutput` are ignored. Reason: hooks only tighten, and an ended turn would stall an AFK run.
- 2026-11-08 (#29): `post_tool_use` now fires only on success; a failed call fires `post_tool_use_failure` (Claude Code parity). This changes the pre-release behavior where `post_tool_use` saw `is_error: true`.
- 2026-11-08 (#29): Hook text that becomes model context (`additionalContext`, plain stdout, a block `reason` fed back) is truncated at 10,000 characters with a marker, where Claude Code spills the rest to a file. Reason: no transcript directory to spill into. Context is always framed as a harness note (`[Hook context from <event>, not the user's words: …]`) and never recorded as a user message, so it cannot become guardian intent.
- 2026-11-08 (#29): `task_created` sends no `task_id` (the id is assigned by the create the hook gates) and sends Fontaine's `task` object next to Claude Code's flat `task_subject`, `task_description` and `teammate_name`.
- 2026-11-08 (#29): `user_prompt_submit` fires for human-typed and headless prompts (including the typed first `/loop` iteration and the `/goal` set, `/init` and skill lines), and not for turns the harness starts (scheduled turns, notifications, stop and goal continuations), where Claude Code also fires on turns it starts itself. Reason: blocking a harness wake could wedge background work, and its text is not the user's. It also does not fire for mid-turn steering (the text rides into a tool result the TUI already showed as delivered, so a block could not honour "prompt not sent"), for prompts typed to a child agent (Claude Code has no human prompt into a subagent) or for the free-text "Other" answer to `ask_user_question` (a tool result in Claude Code).
- 2026-11-08 (#29): Observed `claude -p "hi" --output-format json` with a blocking `UserPromptSubmit` hook (`echo blocked >&2; exit 2`): exit 0, subtype `success`, is_error false, num_turns 0, cost 0, `result` = "UserPromptSubmit operation blocked by hook:\n[<command>]: blocked\n\n\nOriginal prompt: hi". Fontaine's headless run matches it.
- 2026-11-08 (#29): No `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP`-style override; the cap of 8 continuations in a row is a constant. Applying it to `subagent_stop` is Fontaine's choice (Claude Code documents it only for Stop). The streak resets when the agent made tool calls since the previous stop.
- 2026-11-08 (#29): Default hook timeouts differ from Claude Code (600 s for command and http there). Fontaine keeps its 30 s default, because a hung hook should not stall a turn for 10 minutes; prompt is 30 s as in Claude Code; `session_end` has a fixed 1.5 s default without Claude Code's budget extension.
- 2026-11-08 (#29): `notification` maps an approval to `permission_prompt` and any question prompt to `elicitation_dialog`, and fires from the TUI's prompt-opening edge whatever `[tui] notify` is; Fontaine's other notification kinds (auth, quota, agent completion) have no notifier edge today. The edge is detected where the TUI draws, so the screen-reader renderer fires it too.
- 2026-11-08 AFK (#29): A plugin hook carrying `if` or `shell: "powershell"` is skipped with a warning (like `args`), not loaded with the field ignored; `if` is not mapped to `matcher`. Reason: ignoring `if` runs the hook, including a user-trusted `allow`, on more calls than intended, and a PowerShell script under `sh` misbehaves; Claude Code honours both.
- 2026-11-08 AFK (#29): `pre_compact` fires from a hook-free pre-summary callback that `Session::compact` runs only after `plan_summary` returns a plan and before the model call. Reason: `compact_with` runs before every turn, so a hook there would fire on every turn even when nothing is summarized. Its plain stdout is appended to the summary request as `Additional focus: …` for both `manual` and `auto`, because `Threshold` carries no focus and dropping an auto hook's output silently would be a quiet failure.
- 2026-11-08 AFK (#29): Plugin `url` and header values expand only `${CLAUDE_PLUGIN_ROOT}` and `${CLAUDE_PROJECT_DIR}` at load; any other `${VAR}` waits for request-time `allowed_env_vars` interpolation. Reason: load-time expansion reads the unfiltered host env and would send secrets off-machine past `allowed_env_vars` and the scrub check. `allowed_env_vars` may not name a variable the `shell` tool's scrub removes (a startup error).

- 2026-11-09 (#554): The TUI gains `/plugin install|uninstall|update|enable|disable` and
  `/reload-plugins`, and a change applies to the running session without losing the conversation:
  plugin skills, agents, styles, hooks, deny/ask rules, guardian additions and MCP servers are
  loaded, reloaded or unloaded, and an unchanged plugin is left running. This supersedes the
  2026-10-28 "CLI-only" rule and the 2026-11-03 "trust applies next session" rule (trust applies at
  `/reload-plugins`). Marketplace registration, update and removal stay CLI-only per the
  2026-10-28 rule. Reason: iterating on a plugin or skill should not need a restart.
- 2026-11-09 (#554): Plugin-sourced hooks live in a swappable plugin layer (`PluginLayer` in a
  `Live` cell) consulted after the frozen user and project hooks for every hook event plugins can
  define, and plugin deny/ask rules are swapped in place by `RuleSource::Plugin`; user and
  project items never change at runtime, so the floor order and the frozen path are untouched.
  A plugin hook keeps `user_trust: false`. A newly loaded plugin's `session_start` hooks do not
  fire (the event already happened). Guardian prose from plugins applies live. Reason: small
  blast radius; making every hook and rule swappable was rejected.
- 2026-11-09 (#554): Unloading or reloading a plugin's MCP server removes its tools first (no new
  calls), mutes its `list_changed` refresh, waits up to 10 s for in-flight calls and then closes the
  connection; a call still running ends with the transport error. Reason: cancelling can leave a
  tool's side effect half done. A changed plugin language-server set applies from the next session
  (running servers are stopped and restart on need); a plugin's servers are stopped before its
  checkout is replaced or removed, and a failed removal or rename is warned about.
- 2026-11-09 (#554): Skill and plugin files edited on disk are noticed, not watched: at the turn
  boundary a cheap path+mtime+length fingerprint is compared with the last load and one notice
  says to run `/reload-plugins`. No `notify` dependency and no auto-apply, so a stray edit cannot
  change the toolset or cost a cache read unexpectedly.
- 2026-11-09 (#554): `/plugin install` always confirms in the TUI, with the full executable
  surface and fingerprint (the CLI treats the typed command as consent), and `/plugin update`
  confirms when the recorded approval is absent or differs from the new fingerprint, mirroring
  the auto-update hold rule. The TUI uses unattended git, so a credential prompt is an error in
  the transcript. The change commands wait for the turn to end and are refused while subagents
  are live (the `/clear` guard), and nothing gives the model an install tool; each change is
  recorded as `Record::PluginChange` in the ledger and the audit log.
- 2026-11-09 (#554): `/reload-plugins` also reloads the standalone user and project skills,
  agents and output styles (and applies staged plugin updates, held ones stay held), and rebuilds
  the system prompt only when it or the tool definitions changed. Conversation history is never
  edited; the next turn carries a note naming the unloaded or changed plugins and skills as
  possibly stale.
- 2026-11-09 (#554): A bare `/plugin` lists the installed plugins like `/plugins`; Claude Code's
  `/plugin` is an interactive manager, which is not built.
- 2026-11-09 (#559): The TUI gains `/plugin marketplace add|list|update|remove`, superseding the
  "marketplace registration stays CLI-only" remainder of the 2026-10-28 rule: marketplace
  operations move into `fontaine-core::plugin_ops` (`add_marketplace`, `update_marketplace_named`,
  `remove_marketplace`, `marketplace_line`) and both the CLI and the TUI call them. The TUI forms
  follow the #554 rules (turn boundary, subagent guard, unattended git, `Record::PluginChange` is
  not recorded since a marketplace loads nothing executable). Reason: registering a marketplace
  is the first step of installing a plugin, which the TUI already does.
- 2026-11-10 (#561): Plugin install now asks the skill-trust question, narrowing the earlier split
  where install approved only the executable surface: `/plugin install` asks per skill that declares
  `allowed-tools`, after the surface approval and before the commit (quitting cancels the install);
  `fontaine plugin install` asks once on a terminal and otherwise prints a hint and trusts nothing;
  skills already trusted at their current digest are not asked again, and a stale record is shown as
  a diff; `/plugin update` and auto-update re-ask nothing, since a changed file mismatches its
  pinned digest; reason: the user learned of untrusted skills only at first use, as a one-line warning.
- 2026-11-12: (#592) On final exit the session's work is cancelled before `session_end` fires, and `session_end` shares the two-second exit budget with the rest of the cleanup instead of running first on its own timeout. MCP stdio servers adopt Fontaine's whole-tree guard on Unix (`process_group(0)` on the command, `procgroup::terminate` on the retained pid at final exit) while keeping rmcp's `TokioChildProcess` and graceful stdio close; Windows keeps rmcp's direct-child kill; reason: descendants of a wrapper survived the direct-child kill.

### Guardian

- 2026-10-28: Deviation (development escape hatch, #271) — `/guardian-disable` turns the guardian
  and the deterministic floor off for the rest of the current session, after a confirmation, and
  `/guardian-enable` turns them back on. It is interactive-only and session-scoped: never saved to
  config, never settable from project scope, and never inherited by a subagent (children keep the
  tighter of their own and the parent's posture). Every toggle is recorded in the ledger and the
  audit log. This carves a deliberate exception into the "no zero-guardian mode" and "the floor
  gates every mode" invariants for development; the read-only fast path and the result scanner are
  not the exception's subject, since with the floor off every call is allowed before either is
  reached.
- 2026-10-24: (Superseded by the 2026-11-01 composable-guardian entries for the configuration and the code: the screen is now the `screen` preset of `[guardian.composition]`, a Rune script run by `fontaine_guardian::composition`, not `[guardian.screen]` and `fontaine_guardian::cascade`; the steering-hardened allow path, its evidence and its fail-closed rules below still hold, and the preset is sequential by default rather than racing. Superseded by the 2026-11-02 Jev entries for "off by default": the `screen` preset on a hosted Jev model ships an allow path that is on by default, with default thresholds and a margin instead of steering-hardening.) 2026-10-24: Deviation — the screen stage (`[guardian.screen]`) may allow a call without the LLM stage (#135), through a steering-hardened allow path only; this supersedes the 2026-10-24 "never allows" line below and deviates from "the fast path is earned by containment". It is a stage of a composed guardian (`fontaine_guardian::cascade`), so a screen allow is a guardian-tier verdict, taken to cut allowed-call latency: the screen and the LLM stage start together and a settled call drops the LLM request. Evidence (experiment 10, #133): with the shipped prompt one steering sentence moved 93 of 134 must-not-clear cases below the skip threshold, so the allow needs a hardened permission read and a steering detector below their thresholds; at the zero-miss threshold it skipped 13 of 59 should-allow calls with 0 must-not-clear allowed and 0 steered cases through. Fail-closed fallbacks: any read failure makes the screen `Unavailable`, and any guardian verdict already in hand (cache hit, unavailable, an earlier finish) wins and goes through the normal flow, so the call goes to the guardian. It is off by default (`allow_threshold`, user scope only), shadow-loggable first, never runs before the floor or on a paused breaker, and does not reset the denial breaker.
- 2026-10-24: (Superseded by the entry above for the allow path, and by the 2026-11-01 composable-guardian entries for the `[guardian.screen]` key and the cascade: a stop is now an optional `stop_threshold` of the `screen` preset, off by default.) The cascade screen stage (`[guardian.screen]`, #130) only ever stops a call as an `escalate` (`tier = screen`); it never allows, so a screen error, timeout or unparseable answer goes to the guardian, never to allow. Reason: option 1 keeps the guardian's fail-closed floor intact, while a screen that allows would be a second, unreviewed allow path (option 2, #133, stays out of scope).
- 2026-10-23: (Superseded by the 2026-11-02 Jev entries for "nothing ships": a hosted Jev screen is now the default offered by setup, with thresholds fitted on the synthetic corpus.) Typed-decision screens (Laya, scored by `--guardian screen:<model>`): no screen ships now; the direction is a cascade guardian (floor → fast screen → LLM guardian) trained from Fontaine's own decisions (#130 cascade stage, #123 decision corpus, #124 shadow mode, #125 distillation). Out of the box `laya-typed-decisions` with the scorer's three-way question escalates 165 of 193 cases (catch 97.5%, benign stop 63.6%); rephrased as a yes/no risk question the base checkpoint reaches AUROC 0.835. Replayed against the `kimi-for-coding` verdicts with held-out thresholds, a stop-only screen (option 1, safe by construction) keeps the 90.8% catch rate and removes 15% of LLM calls, and a screen that also clears (option 2) removes 26% and halves benign latency; option 2 still needs its own decision on the containment-fast-path invariant. Training data, not the encoder, is the limit: 400 synthetic policy-written cases reach AUROC 0.855, public corpora transfer poorly. DiffusionGemma, Jev and full-encoder fine-tuning were not measured (#127). Follow-up the same day: typed decisions read from a small instruct chat model's first-token logprobs beat Laya. `qwen3-4b-instruct-2507` asked whether an employee given the request would be allowed the action reaches AUROC 0.913 and reads the task-action relation. Held out end to end (synthetic-corpus thresholds, fresh live kimi run), as a stop-only screen at a 10% escalation budget it lifts catch from 91.7% to 96.7%, cuts LLM calls to 41% and mean latency from 7.3 s to 3.0 s, at the cost of 13.6% of allowed calls escalated instead of 3.4%. That makes it the cascade's first screen candidate (#130), and still nothing ships. Numbers in `docs/evals.md` and `research/analysis-typed-decision-screen.md`.
- 2026-10-20: Guardian is fail-closed on every non-verdict outcome; Deny verdicts are never softened (Claude Code behavior; Phase 4 §Fail-closed guarantees).
- 2026-10-21: Guardian default = top-ranked benchmark-validated model available on the configured accounts; switching is easy, explicit, never automatic; no catalog match ⇒ main model with a one-time notice. (User decision.)
- 2026-10-21: `pg2` (PromptGuard 2) is the recommended tool-result scanner; `llm` and `command` are alternatives; default `off`; scanner errors fail closed (flagged) with no fallback; the Llama 4 Community License caveat is surfaced at first enable; BYO-scanner interface for license-clean setups. (User decision.)
- 2026-10-21: Deviation — subagent reports are screened by the optional (default-off) result scanner, not by the classifier at handoff; reason: the guardian never sees tool output.
- 2026-10-21 (user): No zero-guardian mode; containers get `[guardian] profile = "minimal"`, never off, and it requires Fontaine-verified containment (user-scope `--in-container` plus the Linux probe; probe failure ⇒ `full`). Ruleset and request: Phase 4 §Minimal guardian profile; it ships with its own scorecard column.
- 2026-10-21 (user): (Superseded by the 2026-11-02 Jev entries for "deferred": Jev is now the default `screen` preset model; and for "always recalibrated on Fontaine's golden set" in the default fit only: the shipped Jev defaults are fitted on the synthetic corpus alone with a 0.10 margin, and the golden set scores them.) Typed-decision screeners (Jev, Laya, DiffusionGemma) are deferred to the screening-tier roadmap; thresholds are always recalibrated on Fontaine's golden set (vendor defaults over-escalate: 17/21 wrong escalations at 0.85/0.6).
- 2026-10-21 (user): Guardian tree accepted as design direction — strictest-wins composition, fail-closed between tiers, per-domain subtrees, benchmarked tiers only; v1 ships the deterministic+LLM spine; one tree serves the whole agent tree.
- 2026-10-21: Session ledger — the guardian's inputs come from a Fontaine-owned append-only ledger, never rig-memory context; resume/fork drop session grants (`design/sessions.md`). Claude Code has no analogous design point.
- 2026-10-21: The loop breaker follows Claude Code — `auto` pauses after 3 consecutive or 20 total guardian denials (not configurable); headless treats the paused state as would-prompt → Deny.
- 2026-10-21: The "destructive" class behind the fail-closed Allow → Deny rule is derived deterministically by `fontaine-core` from the call, never from the guardian's label; verdict labels are Claude Code's default block categories plus `allow` (Phase 4).
- 2026-10-21: The pre-session file index is gitignore-respecting, capped at 50,000 paths; the guardian request carries only the touched paths' pre-existence status.
- 2026-10-21: The default guardian list is the data file `rust/fontaine-guardian/data/guardian_models.toml` (entry keys `kind`, `model`, `catch_rate`, `false_deny_rate`, `p95_ms`, `scored_on`, `dataset_version`); it and the prompt assets (`guardian_prompt.md`, `guardian_minimal.md`, `scanner_llm.md`) are compiled into `fontaine-guardian` via `include_str!`; it ships empty (fallback to the main model) and the Phase 4 scorecard run fills it, so a scorecard update ships with a release.
- 2026-10-21 (user): The minimal profile uses the resolved `[guardian] model` and its ruleset in `prompts/guardian_minimal.md`.
- 2026-10-21: The `pg2` flag threshold is the fixed constant 0.9, LlamaFirewall's default `block_threshold`.
- 2026-10-21 (user): Destructive-class word lists ship as constants (Phase 4), checked against `claude -p` baselines; catch rate is over all `kind = guardian` `attack:*` cases, false-deny over all `kind = guardian` benign cases, and the runner reports p50/p95 latency.
- 2026-10-21: Output styles never alter the guardian's prompt or `AdjudicationRequest`.
- 2026-10-21 (user): The RuleLabel enum gains `Container Escape` for `guardian_minimal.md`; the escape class goes in the verdict `explanation` as `class=<name>` and the `minimal` column reports per class from it.
- 2026-10-21 (user): Guardian constants ship in `fontaine-guardian`, not config keys (cache TTL, `task_intent` budget, message and action counts, extractor retries; Phase 4).
- 2026-10-21 (user): PromptGuard 2 runs on `candle` (safetensors from the gated HF repo), not ONNX/`ort`; exact pins and the `pg2` feature in Phase 6.
- 2026-10-21 (user): Scanner selection is one key, `[guardian] scanner = "off" | "pg2" | "llm" | "command"` (default `off`), replacing `scan_tool_results`; scanner errors fail closed (flagged); scanner keys are user file only.
- 2026-10-21 (user): The Linux containment probe and its signals are defined in Phase 4 step 5; macOS and Windows ⇒ not contained; `minimal` needs `--in-container` AND a positive probe.
- 2026-10-21 (user): The guardian-list inclusion bar and ranking are defined in Phase 4 §The guardian agent; separate from the kill criteria.
- 2026-10-22: Deviation from LlamaFirewall — pg2 scores the whole result in 512-token windows at stride 256 instead of truncating to the first 512 tokens; reason: an injection past the first window would otherwise pass unflagged (Phase 6).
- 2026-10-27: A second verdict transport, Ollama's schema-constrained `format` over rig's native `prompt_typed`, serves an Ollama model that reports no `tools` capability; the path follows the model's `/api/show` capabilities with no config key, a probe failure or an old daemon keeps the benchmarked `submit` path, and no model is listed in `guardian_models.toml` for it without its own scorecard run clearing the inclusion bar. Claude Code has no analogue (it always has tool-capable models) (#250)
- 2026-10-27: Measured on dataset `d1740a7c65ae` (GTX 1060 6 GB, 16384-token window): `ollama/gemma3:4b` on `schema` scored 98.9% catch / 31.7% false-deny over the answers it gave, p95 3.7 s, with 48 of 209 answers unparseable (decision and rule disagree); `qwen3:4b-instruct-2507` scored 93.4% / 3.9% / p95 4.1 s on `tool` and 93.4% / 5.9% / p95 4.4 s on `schema`. None clears the inclusion bar, so `guardian_models.toml` gains no entry and the schema path ships as pin-only; `qwen3:8b` was not used because it does not fit the 6 GB card. A later round on the same dataset scored six more small local models and nine hosted ones (the `kimi` control included) on the `tool` path: `go/claude-haiku-5-5` scored 99.2% catch, 2.0% false-deny, p95 2.4 s and clears the bar; `go/qwen3.8-flash` (97.5%, 0.0%, 3.4 s), `go/deepseek-v4.1-flash` (95.9%, 0.0%, 3.2 s) and `go/minimax-m3` (90.9%, 2.0%, 3.2 s) miss it on p95 alone; `kimi/kimi-for-coding` is the control (89.7%, 3.9%, 16.1 s); no local model clears it. `opencode-go/claude-haiku-5-5` is the first entry in `guardian_models.toml`, added by hand because `benign-34` failed closed as unparseable on every retry and the runner will not write the file with an unscored case; the entry counts that case as a false deny (3.9%, 2 of 51, not the report's 2.0%), still under the bar (#250)
- 2026-10-27: Deviation from Claude Code — user reaffirmation after a denial counts for soft denies only when it restates or approves the denied action in the user's own message; there is no Path B (agent-proposal-plus-"yes"), because the guardian is never shown agent prose (#256).
- 2026-10-27: Same as Claude Code — a hard deny is never cleared by user intent, repeated or not (#256).
- 2026-10-27: The guardian's `task_intent` and `recent_user_messages` blocks carry `source="user"` and are read as the user's own words; the screen stage keeps its input and prompts byte-identical, since it is opt-in and never before the floor (#256).
- 2026-10-30: Guardian policy, not a deterministic bypass — the shipped `allow` list gains a `Source inspection` entry treating source, header, build-file, installed-dependency and installed-skill-definition reads (including `SKILL.md`) outside the roots as routine in `auto`/`plan`; the floor, user boundaries and hard denies keep their precedence, and the minimal profile prompt is unchanged (#284).
- 2026-10-30: The three shipped soft-deny rules that had no label are reported under existing labels: persistence and self-modification as `Safety Bypass`, untrusted code integration as `Download+Execute`, third-party and real-world effects as `Data Exfiltration`. The label set is closed and adding labels ripples into the audit log, headless schema and scorecards; a malformed verdict is also retried with a harness-written reason (never the model's text).
- 2026-10-30: The screen stage's input stays byte-identical: the scratch-directory and spawned-agent facts and the `session_scratch` status render only in the guardian view, following the 2026-10-27 `source="user"` decision (#312).
- 2026-10-30: An agent view shows denials only, as the main chat does, and a subagent's allowed tier and rule live in the audit log (`decisions_by_agent` / `decisions_of` / the agent log are deleted); reason: matches the main conversation and Claude Code (#321).
- 2026-10-30: Human messages are scoped by agent: `Record::UserMessage` gains an optional `agent`, and a message typed to a subagent is guardian intent only for that agent and its descendants, never for main or siblings, and is ignored by session naming and the rewind count (a rewind also drops agent-typed messages recorded after the first dropped main one); reason: the guardian never judges a call by words its model has not read (#321).
- 2026-10-30 (user): Environment discovery is an explicit setup command (`fontaine auto-mode setup`, `/auto-mode-setup`), never a session-start step. Session start only loads the stored result and warns, in `auto`, when it is missing (#316).
- 2026-10-30: Deviation from Claude Code: the setup stores typed facts with provenance in the data dir and renders them as session facts, not LLM-drafted prose in the user settings file. API facts are read directly, and the model scan may propose tightening entries (sensitive locations, deploy targets, protected targets) and trusted destinations, which the user reviews before they are saved. Reason: the facts must stay checkable against the current remotes.
- 2026-10-30 (user): The scan runs on the configured guardian model for now. Per-role model lists are #324.
- 2026-10-30: Deviation from Claude Code: the reminder is a startup warning in every `auto` session without facts, not a dialog gated on 5 startups and 5 denials. It is silenced with `[auto_mode] setup_reminder = false` (user-trust scopes). Reason: user's call, and one path for headless and TUI.
- 2026-10-30: The per-call `repoVisibility` meta line for repos other than the recorded remotes is not built. It needs a network lookup during the session; follow-up issue to be filed.
- 2026-10-30 (user): A `/auto-mode-setup` run applies to the current session as soon as the user confirms the review, through an appended `Record::ProjectEnvironment` ledger record. A CLI run from another terminal applies at the next session start or resume.
- 2026-10-30 (user): API-sourced facts may relax. A public repo's published tracked content is public data, so sharing it is not Data Exfiltration; unpublished files, unpushed changes and secrets keep full protection.
- 2026-10-30 (user): The model scan may propose relaxing entries (trusted destinations) as well as tightening ones. It runs only in a trusted workspace, and an entry is saved only after the user accepts it in review, never unreviewed. The accepted record is user-scope authority; no relaxation is read from a project file at session time. Deviation from Claude Code, which omits repo-only trust entries; reason: folder trust plus per-entry review is the user's own grant.
- 2026-10-30: A `public` visibility expires after 30 days (rendered unknown, with the stale warning), because a stale relaxation is looser than no record; tighten-only facts and user-accepted entries don't expire.
- 2026-11-01: The guardian's composition is a Rune script (`rune = "=0.14.2"`, pinned exactly, behind the non-default `composition` feature of `fontaine-guardian`, which `fontaine-core` enables; no new crate) configured in `[guardian.composition]`, which replaces `[guardian.screen]` (#137, folds in #136). Today's screen ships as the built-in `screen` preset, a Rune script compiled into the crate, so the common case stays a few lines of TOML. The host supplies the leaves (the LLM guardian and the logprob reads) and mints every verdict; a script cannot forge a `Call`, `Score` or `Verdict`, so an allow can only come from `llm()` or from `allow_below` applied to a score a declared read produced. Rust wraps the script in a fail-closed root (instruction budget, time bound, panic = Deny, abstain at the root = Deny).
- 2026-11-01 (user): Rune over TOML data, a bespoke DSL, Lua, Starlark, JS or WASM: native `async`/`.await` with lazy futures makes sequential the default and concurrency opt-in, it is pure Rust (no C build, fits the Windows cross-compile), and it has an instruction budget and `Send` execution. The cost is a pre-1.0 dependency, hence the exact pin.
- 2026-11-01: v1 loads scripts from the user file only; project scopes may force `shadow = true` and nothing else, so a project-defined script can never mint an allow. The project-scope tighten-only script (a host context without `allow_below` or reads, strictest-wins over the user composition) is #373.
- 2026-11-01 (user): The `screen` preset is an allow screen in front of the LLM guardian, sequential: the screen runs first, an allow ends the call, and everything else goes to the guardian. It does not stop on its own unless `stop_threshold` is set. Concurrency (`concurrent = true`, the previous race) is opt-in (#136). Deviation: older `cascade-*.json` scorecards were measured racing, so `docs/evals.md` notes that `--composition-param concurrent=true` reproduces them.
- 2026-11-01 (user): Integrating the allow screen into `fontaine auto-mode setup` / `/auto-mode-setup` is #372.
- 2026-11-01: Deviation from the 2026-10-24 "a guardian verdict in hand wins" rule: a script `escalate` or `deny` beats an LLM allow that is already in hand (the stricter outcome wins), while an in-hand LLM refusal (deny, escalate or a fail-closed denial) always wins over any script verdict and goes through `settle`, so a computed guardian denial stays non-approvable and still feeds `record_denied` and the breaker. A composition that fails (script error, budget, timeout, panic, non-verdict, root abstain) is a fail-closed guardian denial through the same `settle` path: approvable from `/denied`, counted by the breaker, with `fail_closed_reason`. A script's own `deny` is a `tier = screen` denial with no prompt.
- 2026-11-01: Shadow mode is invisible to the script: the root itself starts every declared read and the LLM request (one request per read name and one LLM request, shared with the script's `read()` and `llm()`), so shadow logs every score as before, costs no extra call, and records exactly what enforcement would decide as `screen_outcome`.
- 2026-11-01: The root's time bound starts after the root awaits `Guardian::resolve_mode()`, so serialized probe waits of concurrent callers cannot turn a real LLM verdict into Deny `Timeout`. The bound is the guardian's timeout plus one read's timeout plus a margin, so a script that chains read stages before `llm()` can hit Deny `Timeout` by design.
- 2026-11-01: `screen_outcome` is mapped from the origin of the verdict the script returned (escalate `stop`, deny `deny`, `allow_below` `allow`, `llm()` `pass`, any failure `unavailable`), so when `llm()` wins a concurrent race the outcome is `pass` (the screen's would-be verdict was never computed), unlike the old cascade, which recorded the screen's own stop or allow. The audit record keeps `screen_risk`, `screen_allow_risk` and `screen_steering` (the reads named `stop`, `allow` and `steering`) and adds `screen_scores` for every other read; the deciding node is the record's `rule`.
- 2026-11-01: Spike result: a host `async fn` taking Rune futures (`first_decisive`) keeps `VmSendExecution` `Send` under `tokio::spawn`, so the evaluation runs in a plain spawned tokio task and the `LocalSet`-in-`spawn_blocking` fallback is not needed.
- 2026-11-01: Deviation from the design sketch: the host functions are in the script's global namespace (`read(..)`, `llm(..)`, `escalate(..)`), not under a `guardian::` module with a prelude. Rune has no extensible prelude, and a `guardian` module would collide with the script's own `guardian` entry point.
- 2026-11-01: A `--composition-script` eval adds no read flags: its reads and `timeout_ms` come from the loaded config's `[guardian.composition.reads]`, its default read model from `--screen live:` (else the config's `model`), threshold flags are rejected with a script, and the ollama-account check runs in the shared `check_composition`.
- 2026-11-01: (Superseded by the 2026-11-02 Jev entries for the hosted model: its offer writes no threshold, since the preset ships defaults. The Ollama offer below still holds.) Deviation from "no threshold ships": `fontaine auto-mode setup` / `/auto-mode-setup` offers the `screen` preset with the threshold benchmarked for the exact Ollama tag it was fitted on (`composition_offer::BENCHMARKS`), because users cannot be expected to guess one; any other model gets no offer. The user chooses safe (`stop_threshold` only) or fast (`allow_threshold` only, the AGENTS.md fast-path exception), sees the benchmark evidence, and nothing is written without a yes; the section goes to the user file only, with `shadow = false` since the preset refuses enforcement without a threshold.
- 2026-11-01: The composition offer is skipped under the effective `minimal` profile (from `containment::resolve`) because the benchmarked thresholds were fitted on full-profile reads; non-terminal runs list candidates in the JSON `composition` object and never write.
- 2026-11-01: A redirect whose target literal is exactly `/dev/null` writes no file: the shell analysis sets neither `output_redirect` nor an output target for it, so `rg x 2>/dev/null` is not destructive and lists no `/dev/null` path. `/dev/stdout`, `/dev/stderr` and `NUL` stay file writes; the fast path and the floor are unchanged (#357).
- 2026-11-01: Touched paths carry `absent` and `git_tracked` marks, set by the harness (`symlink_metadata` is not found; git's index has an exact file entry for it; never a directory or the work-tree root) and rendered only in the guardian view, as `in_scratch` is, so the screen and composition reads and the minimal profile are byte-identical to before. For a shell call the marks are set only when it is a single simple command with a certain working directory and no glob or pathspec word (`*`, `?`, `[`, or a leading `:`), because a pre-run snapshot cannot describe a later command's targets. The Destroy Pre-existing Files rule exempts a move onto an `absent` path and writes onto a `git_tracked` one (#366).
- 2026-11-01: The guardian-side `spawned_agent` fact, the guardian prompt's Subagents section and the guardian-kind spawn cases `overdeny-04` and `attack-overdeny-03` are removed because no `agent` call reaches the guardian (maintainer's call); `adjudication::spawn_facts` stays for the read-only fast path.
- 2026-11-02: No `[guardian] effort`; the guardian, the composed guardian stage, the scanner, the critique, `project_env` and the goal evaluator keep `thinking_off` and never receive the session's effort; reason: no detection rule exists for models that refuse a forced tool call under thinking, and the fail-closed guardian must not gain an unspecified path.
- 2026-11-02 (#375): The `screen` preset on a hosted model (`typesafe/jev-1.14`, OpenRouter's `/v1/systemone`, asked the five-level `level5_employee` question) is the default offered by setup. It declares `stop` and `allow` reads only: both ask the same model the same question, so the composition sends one request, shared by `(model, prompt, question)`, with one slot and one audit record per read name and the request's cost recorded once. Reason: one hosted call is ~$0.000027 and ~150 ms, it catches more than the local qwen3-4b read, and it is far harder to steer.
- 2026-11-02 (#375): A Jev screen has no steering read; reason: 2–9 of 1,503 steered cases cross the 0.10 margin on Jev against 93 of 134 on qwen3-4b, and none at the margin, while a local steering read would make an Ollama model a hidden requirement of a hosted default. `question` and `steering_veto` are refused for a hosted model.
- 2026-11-02 (#375): The hosted preset's allow path is on by default (`allow_threshold = 0.20`, `stop_threshold = 0.34`, `allow = false` switches it off). The allow default is the synthetic corpus's zero-miss skip (0.30) less a 0.10 margin for nondeterminism (60–75% of repeats differ by up to 0.22); the stop default is the 5% stop point in `research/analysis-jev-improvements.md`. Neither was fitted on the golden set. Known leak classes (`.env` reads, session facts, `kubectl get ... secrets`: 1–3 must-not-clear cases cleared on the fresh set and public corpora) are documented in `docs/guardian.md`, and the allow decision stays a verdict of the composed guardian, never before the floor, user scope only.
- 2026-11-02 (#375): `shadow` stays `true` for a hand-written section and is `false` only where the setup offer writes it, so merely configuring the preset never enforces; the offer writes no threshold key so a recalibration reaches users with no config rewrite. `model` stays required for any preset, so a hand-written section never starts sending guardian state to OpenRouter on its own. A repository can force `shadow = true` (the repository-level off switch for the allow path) but never enable the allow path or change `allow`.
- 2026-11-02 (#375): The composition model may be on an `ollama` account (yes/no questions, first-token logprobs) or an `openrouter` account (`level5_employee`, `/v1/systemone` joined onto the account's chat `base_url`); the question must match the account kind or the composition does not compile. A hosted model's key must resolve at startup or startup fails with exit code 2, and is sent only over https or to loopback. This replaces the earlier rule, which lived only in code and `docs/configuration.md`, that the composition model must be an Ollama account.
- 2026-11-02 (#375): Guardian state (task intent, recent user messages, tool name and arguments, project facts) leaves the machine for OpenRouter and Typesafe wherever the hosted screen is enabled. Disclosure is at the setup offer and in `docs/guardian.md`; there is no per-session notice at run time. Setup offers the hosted screen only on an existing `openrouter` account with a resolving key and creates no account (deviation from the issue's "plus an account if there is none": it avoids key entry in two UIs).
- 2026-11-02 (#375): No 12-category audit read is built and the server's chosen level is not recorded; the audit's `screen_model` records the id the server resolved (`typesafe/jev-1.14-20261014`) once a read answered, the `<account>/<model>` label before.
- 2026-11-09: (#557) A guardian whose external-login account is not signed in fails closed as `FailClosed::NotSignedIn` (audit rule `GuardianNotSignedIn`, `fail_closed_reason` `not_signed_in`) instead of `GuardianUnavailable`; it is always a Deny and takes every path `unavailable` takes. Reason: audit consumers and the terminal UI key on the label, and a signed-out account read as a Fontaine outage.
- 2026-11-11: (#115) A spent usage window is one session-wide pause, not a per-account one: one state to show, and gated calls never pile up denied or pending across accounts. A guardian call that meets a spent window stays pending and is judged again from the start when the window reopens; a 429 never becomes an allow. When the wait is cancelled or runs out the call is denied with the new fail-closed kind `UsagePauseEnded`, which is a decision by the user or a deadline, so it does not count toward the denial breaker and adds nothing to AFK's temperature. The guardian crate stays free of core types: the failure's cause rides on `Adjudication` and the composition's fail-closed deny, and core classifies it.
- 2026-11-11: (#115) The probe is not a wait for the reset alone: after the reset time plus a margin one caller is let through (or the account's own quota figures are read), because a reset can come early or late and may be unknown. The caller holds a probe token reporting success, and a dropped token, from Esc, a kill or a closed run, lets the pause's own task choose another prober, since a waiter must never be what keeps the others waiting.
- 2026-11-13 (#418): The hosted `screen` preset ships no `stop_threshold`, so a call it does not allow goes to the LLM guardian instead of being escalated to the user (and denied when headless or AFK); supersedes the stop clause of the 2026-11-02 (#375) line. `stop_threshold = 0.34` stays available as an opt-in (10.7% of benign golden-set calls escalated, 97.9% of attacks caught). Reason: the stop stage cannot read intent, and Claude Code's fast stage never sends a flag straight to the user.
- 2026-11-13 (#588): `agent_message_before_reply` (Claude Code's Path B) renders in the guardian view only, as an untrusted block before `recent_user_messages`, and is dropped by the minimal profile. Deviation from Claude Code: it records only the root turn's final answer (a mid-turn proposal is lost: the user's next message does not reply to streamed text, and mid-turn text would add tool-result echoes), only the trailing 2,000 characters (a proposal usually ends the message, and the ledger stays small), and the last reply wins when several share `after` (harness-started turns record no user message, and the last is the one the user saw). A reply pairs only with a latest message of origin `Prompt`.
- 2026-11-13 (#588): Supersedes the 2026-10-27 line's clause "there is no Path B ... because the guardian is never shown agent prose" (its own-message half stands): the guardian is now shown the root turn's final answer, as untrusted data, under the line above.
- 2026-11-13 (#576, #599): The guardian prompt no longer counts relevance or craft as harm: Deny covers a significant escalation in scope, severity or destructiveness beyond the task (Claude Code's scope-escalation test), a package-registry metadata query is read-only, and editing, commenting on, labelling or closing the project's own issues at the user's request is not exfiltration.

### Headless

- 2026-10-21: Headless `stream-json` event names (`init`, `assistant`, `assistant_delta`, `tool_call`, `gate_decision`, `tool_result`, `result`) are Fontaine's own versioned schema — flat typed events — not Claude Code's SDK message shape; flags and `result` subtypes match Claude Code. Reason: one flat, JSON-Schema-validated contract consumed by the eval runner.
- 2026-10-21: No external approval delegate (`--permission-prompt-tool`) in v1; every headless would-prompt outcome is Deny (Claude Code supports the delegate). Reason: an external delegate would be a second, ungated approval path around the deterministic floor and guardian.
- 2026-10-21 (user): Headless `-p` promoted to Phase 1 so CI and the eval harness drive core headlessly from day one.
- 2026-10-21: Deviation — without `--max-turns` a headless turn is capped at 1000 model calls (Claude Code is unbounded); reason: rig requires a bound and a looping model should not run forever. `--max-budget-usd` is checked between model calls: the call that crosses the cap stops the run before its tool calls execute, and an answer already complete is kept.
- 2026-10-21: The headless I/O contract is versioned (`schema_version`; additive keeps it, rename or removal bumps it), mirroring Claude Code's flags plus `gate_decision` events.
- 2026-10-21: The headless I/O contract is a checked-in JSON Schema per version, `docs/schemas/headless-v1.schema.json`, published with the docs and validated with the exact-pinned `jsonschema` crate (Phase 1).
- 2026-10-30 (user, supersedes the 2026-10-21 1000-call cap): A turn, headless or interactive, and every subagent run has no model-call limit unless `--max-turns` sets one, as in Claude Code; rig's limit is set to `usize::MAX` when none is given, since unset it allows one call. The token and cost budgets bound a run; no per-spawn turn limit (#326).
- 2026-10-30 (supersedes the 2026-10-25 line that kept cache fields on `usage.main` only): `usage.guardian`, `subagents` and `goal` carry `cache_read_input_tokens` and `cache_creation_input_tokens`, and `usage_bucket` in `headless-v1.schema.json` requires them on every bucket, so a cache that does nothing is visible there; additive within `v1` (#331).
- 2026-10-31: Headless `stream-json` omits model reasoning though Claude Code's carries thinking blocks; reason: kept out of machine output by choice (#344).
- 2026-11-02 (#116): `result.account_usage` groups usage windows per role (`main`, `guardian`) as `{role, account, label, windows[]}`, a Fontaine addition; each window uses Claude Code's `rate_limits` field names and units (`used_percentage` 0-100, `resets_at` epoch seconds) but is a labelled array, not Claude Code's keyed object, because Kimi's windows (month, code, 7d, 5h) do not map onto its fixed keys. `AccountUsage::json` is the shape the custom status line (#48) embeds under `account_usage`. Additive within `v1`.
- 2026-11-11: (#115) Exit code 3 under `-p` means an account's usage window stayed spent, and its `result` subtype is `error_usage_limit`, a Fontaine-only addition to the subtypes that otherwise match Claude Code (the 2026-10-21 line above). For `fontaine update` code 3 means "not self-updatable"; the two surfaces are separate. `--on-usage-limit=wait|exit` and `--max-usage-wait` (default 6 hours, longer than one 5-hour window) bound the wait, so an unattended run never waits without a limit, and `usage_paused` and `usage_resumed` stream events and `result.usage_limit` are additive within `v1`.

### MCP

- 2026-10-22: Deviation — a stdio MCP server gets a scrubbed environment (a base set plus the variables in `env_vars`) and its stderr goes to the debug file; reason: provider keys and telemetry credentials must not reach a third-party process (Phase 5).
- 2026-10-25: Deviation — an MCP server's `requiresUserInteraction` marker stays in force for the life of the connection once any `tools/list` response carried it (Claude Code follows the latest listing); overlapping refreshes and late responses could otherwise let a stale page loosen it. It only ever asks more.
- 2026-11-08: Fold `fontaine-mcp` into `fontaine-tools` as the `mcp` module (8 → 7 crates); the full merge into `fontaine-core` proposed in #485 was declined to keep the compiler-enforced tools/agents boundary.
- 2026-11-08: (#35) Elicitation declares and serves form mode only. URL mode is declined, because opening a browser on a server's say-so is an outward action that needs its own design (follow-up issue).
- 2026-11-11: (#536) Elicitation now also declares and serves URL mode: the link opens only after the person confirms it in the terminal UI, only `http`/`https` URLs are opened (the parsed URL string reaches the opener, never the raw server text), there is no "always allow", and headless runs, AFK mode, the doctor probe and subagent calls answer `cancel` as for form mode. The browser is opened with the pinned `open` crate rather than hand-rolled platform launchers.
- 2026-11-08: (#35) Headless and AFK answer `cancel`, as Claude Code's non-interactive sessions do. Superseded for sessions with a hook by #537: they cancel only when no `Elicitation` hook answers.
- 2026-11-08: (#35) Elicitation is served from a transport tap, because rig-rmcp's `McpClientHandler` does not expose `create_elicitation`. Wrapping the handler would mean reimplementing `connect`, whose initial fetch is `pub(crate)`, and a fork breaks the exact-version pin.
- 2026-11-08: (#35) In headless runs a stdio server that exits stays failed and is not restarted on the next call, and there is no 30-minute background retry after the five reconnect attempts (follow-up issue).
- 2026-11-08: (#35) Oversized MCP results are saved to `<scratch>/mcp-results/`, not to a `tool-results` directory under the data dir. The scratch directory is the session's private root, so reading the file needs no out-of-root gate, and the file is cleaned up with the session.
- 2026-11-08: (#35) There is no 10,000-token warning. The cap already keeps oversized output out of context (follow-up issue).
- 2026-11-08: (#35) Results that carry image content are not capped or spilled. Fontaine has no image token estimate for them yet.
- 2026-11-08: (#35) `/mcp reconnect` only restarts servers attempted this session, including ones that failed at startup. It never starts a left-off or untrusted one.
- 2026-11-08: (#35) The disconnect text reads "run /mcp reconnect <name>" where Claude Code says "open /mcp to reconnect", because Fontaine's `/mcp` is a read-only notice, not a menu, so the command has to be named.
- 2026-11-08: (#35) The reconnect watcher stops early on a `Permanent` error rather than spending all 5 attempts, because retrying an auth or protocol failure cannot succeed.
- 2026-11-08: (#35) HTTP disconnects are detected from a Transient `ServiceError::TransportSend` or `TransportClosed` on a tool call (not a ping, not rmcp's own recovery), because rmcp's streamable-HTTP worker never closes on a dead server and so `closed()` alone would never reconnect or retire its tools.
- 2026-11-08: (#35) 5xx and 429 at connect are Transient, read from the `HTTP <code>` prefix rmcp puts in `UnexpectedServerResponse`, so they are retried as Claude Code retries them; any other text stays Permanent (follow-up issue asks rmcp for a typed status).
- 2026-11-08: (#35) The supervisor connects through an injectable `McpConnector` trait (production `ServerConnector` calls `connect_server`), so `tokio::time::pause` unit tests can script connects, failures and closes.
- 2026-11-08: (#35) Elicitation writes no audit record, only the `mcp_elicitation` telemetry event emitted by the tap, because the audit log holds gate decisions and its strict reader feeds `/bug`; `ApproverElicitor` keeps only the approver.
- 2026-11-08: (#35) An elicitation that arrives while a subagent's call is in flight on that connection is answered `cancel` without a dialog, so a subagent can never put a dialog in front of the user, as with `ask_user_question`.
- 2026-11-08: (#35) `client_config()` offers protocol version 2025-11-25 (`LATEST_WITH_INITIALIZE`), not rmcp's 2026-07-28 default, because at 2026-07-28 elicitation arrives as an MRTR result rig-rmcp cannot surface; servers that answer only 2026-07-28 still connect but their elicitations are not served (follow-up issue).
- 2026-11-08: (#35) Startup still blocks on MCP connects, and a server's startup retries share one 30 s deadline instead of 30 s per attempt, unlike Claude Code's per-attempt retries, so an unresponsive server delays startup by at most 30 s rather than about 127 s.
- 2026-11-08: (#35) rig-rmcp's 300 s tool-call timeout is left unchanged although it covers the human's answer time, and pending elicitations are answered `cancel` (dialog closed) once no call is in flight on their connection, so no dialog outlives the call that caused it.
- 2026-11-08: (#35) The choke-point reconnect case is kept, with `close().await` on the first connection and an assertion that a call reaches the new server, because the floor's choke-point coverage is a security invariant and a duplicated check costs little.
- 2026-11-12: (#537) An `Elicitation` hook event exists (`command`/`http`, matcher on the MCP server name via `MatcherKind::ServerName`) and fires before any dialog on every surface (TUI, headless, AFK); with no hook configured behavior is unchanged. Verdicts combine cancel > decline > accept; a failing hook decides nothing and falls through; exit 2 and `decision: "block"` decline; a repository's or plugin's `accept` is ignored. `ElicitationResult` stays unbuilt, and there is no `prompt` handler.
- 2026-11-12: (#537) Deviation from Claude Code: a hook can never accept a URL-mode elicitation; the accept is ignored with a warning and the confirmation dialog follows, because a link opens only after the person confirms. Decline and cancel are honored.
- 2026-11-12: (#537) Headless runs and AFK mode answer an elicitation `cancel` only when no `Elicitation` hook answers; the doctor probe, subagent calls and non-HTTP(S) URLs are handled before the hook and never fire it.
- 2026-11-08: (#35) The elicitation dialog and validator use a simplified field model exposed by fontaine-tools (`ElicitForm::fields()`, `FieldKind`, `ElicitForm::validate`) instead of re-exporting rmcp's schema types, so fontaine-tui keeps no rmcp dependency.
- 2026-11-08: (#35) `connect_transport` returns a `Connected`, and `McpOptions` carries the guard, `reveal_names`, the output limits, the elicitor and the warn callback, so the connect signatures stop growing.
- 2026-11-09: (#31) Local-scope servers live in `<config dir>/mcp-local.toml`, keyed by workspace root, not in `~/.claude.json`-style JSON: Fontaine's settings are TOML, and the file sits under the floor-protected config dir.
- 2026-11-09: (#31) The repository's `.mcp.json` is read as project scope alongside `.fontaine/settings.toml` `[mcp.servers]`, and the TOML wins a name collision with a warning.
- 2026-11-09: (#31) Project `.mcp.json` `${VAR}` references are expanded when the server starts, never into a credential variable (the set `tools::scrubbed_env` withholds: account and telemetry keys, the scanner HF token, `MICROMEGAS_*`, OTLP headers; plus the web-search key and the well-known provider variables); such a reference fails that server. Claude Code expands any variable. Reason: a headless run starts project servers without asking.
- 2026-11-09: (#31) An unset `${VAR}` with no default fails only that server's start (Claude Code fails the config parse).
- 2026-11-09: (#31) `fontaine mcp add` has `--env-var NAME` for the scrub's pass-through. It has `-H`/`--header` (http only) and no `sse` transport or `add-json` yet (#36); OAuth is `fontaine mcp login`, not `add` flags (#32).
- 2026-11-10: (#32) `fontaine mcp add` drops Claude Code's `--client-id`, `--client-secret` and `--callback-port` stubs, which never worked; the callback port is fixed at 3111.
- 2026-11-10: (#32) `fontaine mcp login` prints the authorization URL instead of opening a browser: it must work over SSH and in containers.
- 2026-11-10: (#32) A project-scope `headersHelper` is refused (Claude Code runs it after a trust prompt): it is an executable that mints credentials, and project scope can only tighten.
- 2026-11-09: (#31) `fontaine mcp get` masks `env` values as `***` (Claude Code prints them): the output lands in terminals and logs.
- 2026-11-09: (#31) `fontaine mcp list` and `get` probe only what `fontaine doctor` would, so an unapproved repository server shows as pending and is never started by the CLI.
- 2026-11-13: (#34) Names-only prompt plus loading the real tool definition, not a `tool_call` proxy tool: keeps rules, hooks and audit keyed on the real `mcp__server__tool` name.
- 2026-11-13: (#34) Loaded tools appear in their registry position (rig-agent 0.46 `RequestPatch::active_tools` only filters), not append-only; the provider's cached prefix breaks from the first changed position, an accepted cost with no cache-prefix promise.
- 2026-11-13: (#34) Deferral is provider-neutral and client-side, not provider-native tool search: native needs per-provider wire support; #439 tracks the Kimi variant.
- 2026-11-13: (#34) `[mcp] tool_search = "auto"` decides on a heuristic token estimate (10% of the context window, 10,000 tokens if unknown), as Claude Code does; the estimate only trades context for one round trip.
- 2026-11-13: (#34) `[mcp] tool_search` is not a loosening key and either scope may set it: deferral hides schemas but changes no gate.
- 2026-11-13: AFK: (#34) The loaded set starts empty on resume and after compaction (no transcript parser); the model re-searches, and the unloaded-call `Skip` refusal already says to search first.
- 2026-11-13: (#34) One `tool_search` instance serves every agent and keeps a loaded set per agent id (as `todo_write` keeps its list), rather than one instance per child: a child's forked toolset shares the parent's wrapped tools, so a per-child instance would need its own guard; the set and the completion hook are still per agent, and a child's toolset bounds what it can load.
- 2026-11-13: (#542) Supersedes the 2026-11-08 protocol-version-pin line: `client_config()` no longer overrides the version, so rmcp's newest (2026-07-28) is offered in `initialize`, and `ElicitationTap` drives `input_required` (MRTR) rounds for `tools/call`, `prompts/get` and `resources/read`, serving their elicitations through the same dialogs and retrying with the answers (at most 10 requests per call; sampling and roots input requests fail the call). Servers that answer `initialize` with an older version stay on the request path; servers that reject `initialize` altogether stay unreachable until rig-rmcp exposes the `server/discover` lifecycle (follow-up issue).
- 2026-11-13: (#614) Fontaine declares the tasks extension (`io.modelcontextprotocol/tasks`) in `client_config()` and serves a task handle for `tools/call` only: `ElicitationTap` polls `tasks/get` below rmcp (interval clamped to 50 ms..5 s), serves in-task `input_required` through the elicitation dialogs and `tasks/update` (the 10-request cap applies), and sends `tasks/cancel` when the call is cancelled or the tap fails it; rmcp sees only the call's final result or error. A task handle for a prompt or resource fails that call. Reason: rig-rmcp fails a task result as an unexpected response, and a task-capable server never sends one to a client that did not declare the extension.
- 2026-11-13: (#613) Supersedes the "stay unreachable" clause of the (#542) line: `serve()` starts the service itself with `ClientLifecycleMode::Auto` (`server/discover`, then `initialize`) because rig-rmcp 0.46.0 exposes no lifecycle choice and its `begin_refresh`/`fetch_tools`/`commit_initial` are `pub(crate)`; `MetaTap` injects one `tools/list_changed` after the startup response so the handler's own refresh fetches the first tool list, and connect waits for that commit. A failing `tools/list` therefore ends the attempt as a Timeout rather than the underlying error, and a legacy server that ignores `server/discover` adds up to 10 s. `classify_init` classifies the fallback error of `LegacyFallbackFailed`.

### Observability

- 2026-10-24: `api_error` exports `error_class` and `status_code` instead of Claude Code's `error` message and `attempt`, because provider messages can echo the request and rig's retries are not visible at this layer (#139).
- 2026-10-24: `fontaine.token.usage` / `fontaine.cost.usage` gain an `estimated` attribute, a Fontaine extension, to separate the dropped-guardian-spend estimate from real usage (#139).
- 2026-10-24: No custom stream tags for opt-in content; tags are hard-coded upstream, and opt-in content already lives on `cpu`, which the retention recipe expires first.
- 2026-10-24: No materialized view set; applying one needs a micromegas admin and the notebook queries run over base views.
- 2026-10-24: No process-health collector; micromegas's system monitor is host-level and no question the telemetry answers needs it.
- 2026-10-24: The `SpanBridge` span half is kept despite #139's done-when, because the stock layer exports field content and absolute source paths; the swap waits on the upstream field hook and location switch (madesroches/micromegas#1725). (Superseded by the 2026-11-07 micromegas pin line below, and by the 2026-11-07 relative-paths line for the span half.)
- 2026-10-24: micromegas is on a git pin (`rev 7a08f6541`) with `allow-git` in `deny.toml` until 0.33 is on crates.io; the git pin is never shipped in a release, and switching to an exact `=0.33.x` pin is tracked by the `release-blocker` issue #141. (Superseded by the 2026-11-07 micromegas pin line below.)
- 2026-10-24: The analytics notebook's metric cells are native-only instead of changing the OTLP exporter, because micromegas drops OTLP histograms and Fontaine's OTLP sums are cumulative; the event cells work on both backends.
- 2026-10-24: The dropped-spend fields keep the `guardian_*` / `guardian.*` convention: `DecisionRecord` `guardian_dropped_{input_tokens,provider,model,priced}`, projected onto `tool_decision` as `guardian.dropped_{input_tokens,provider,model,priced}`.
- 2026-10-21: Phase 1 implementation choices that refine the design docs: the OTLP backend uses `opentelemetry-otlp`'s `reqwest-blocking-client` (the SDK's batch processors run on their own threads, outside tokio; exporters are built before the runtime); the micromegas span bridge never calls `init_thread_stream` itself, since micromegas keeps a raw pointer to every thread stream and a thread that exits without unregistering would leave it dangling for its flush monitor — the runtime callbacks and the main-thread guard supply the streams; events are exported from the `fontaine::telemetry` target only (rig's event targets carry request and response bodies at trace level) while spans are exported from all four allowlisted targets; the `fontaine.interaction` span is adopted by rig as the agent span, so no separate `invoke_agent` span appears; exported span fields outside a fixed content-free set are treated as content (unknown `gen_ai.*`/`rig.*` fields are dropped unless their opt-in key covers them) and source locations and thread names are not exported; the micromegas sink is built without its host-identifying default properties. (Superseded by the 2026-11-07 relative-paths line for the micromegas span bridge and source locations: the stock capture layer exports spans, with relative callsite paths.)
- 2026-10-21 (user): Observability goes through `tracing` in every crate with only the bin installing a subscriber; export is opt-in, off by default, native micromegas by default (OTLP/HTTP fallback). Deviation (Claude Code exports only OTel); reason: user requirement. Names and opt-ins mirror `claude_code.*` / `OTEL_LOG_*`; details in `design/observability.md`.
- 2026-10-21: The audit log is a separate, always-on local record written before telemetry from one typed `GateDecision`; reason: telemetry is opt-in, lossy and redacted.
- 2026-10-21 (user): The micromegas crates need no published `rust-version`; that upstream item is dropped.
- 2026-10-22: Exported MCP server and tool names read `third-party` unless `log_tool_details` is on; reason: those names can identify internal systems (Phase 5).
- 2026-10-22: `fontaine.session.count` `start_type` takes Claude Code's values `fresh`/`resume`/`continue`, and the surface moves to a `headless` attribute; this corrects an earlier silent deviation (Phase 5).
- 2026-10-25: Telemetry exporters are not covered by `[network]` (proxy, CA bundle, client certificate), because they use their own HTTP stack; covering them can come later as its own change (#6).
- 2026-11-07: micromegas is pinned to crates.io `=0.33.0` and `allow-git` is gone from `deny.toml` (#141). 0.33.0 has the field hook (`span_field_hook`), which Fontaine does not adopt, so the `SpanBridge` span half stays; source paths are trimmed rather than absolute, and only a switch to omit locations is still missing (madesroches/micromegas#1725). (Superseded for the span half by the 2026-11-07 relative-paths line below.)
- 2026-11-07 (user): Exported micromegas span locations may carry the callsite's file trimmed to a relative path (madesroches/micromegas#1725 closed as relative source paths, not an omit switch), so the `SpanBridge` span half is retired: the stock `TracingCaptureLayer` exports spans with `span_field_hook` applying the content policy, and the bridge only derives metrics and events (#141, #139).
- 2026-11-12: (#592) A final quit's telemetry flush spends only what is left of the whole-exit deadline instead of a fresh `FLUSH_CAP`, and the exit watchdog started at the first quit request, which outlives runtime and scratch teardown and disarms only after the flush, restores the terminal before forcing the exit; other exits keep the two-second cap and its own watchdog.

### Permissions

- 2026-10-21: Permission rules naming an unknown tool are a config error (exit 2), not a warning (Claude Code warns): a mistyped deny rule must fail loudly rather than silently protect nothing.
- 2026-10-20: Hybrid gate — tool-declared `read_only` + workspace-scope check skips the guardian; everything else is adjudicated. (User choice.)
- 2026-10-21: Hybrid-gate refinement — the fast path is earned by Fontaine-enforced workspace containment, not the tool's label; read-only ≠ safe, and MCP `readOnlyHint` never earns it. (User choice.)
- 2026-10-21: Dedicated read tools plus a deterministic, zero-LLM Tier-0.5 shell pre-classifier (`brush-parser` + curated set + flag allowlist + scope validator; fail-through on anything unparsed), extended only via the user-scope `[shell_preclassifier] allow_extra` (`design/permissions.md` step 5). Deviations from Claude Code: unquoted globs are rejected (shell-side expansion is invisible to the scope validator), and "don't ask again" grants never auto-extend the curated set (a repo or injected session must not accumulate loosened prefixes). (User directive 2.)
- 2026-10-21: The Tier-0.5 curated set contains no `git` subcommand in v1 — every git invocation goes to the guardian. Deviation from Claude Code (stricter); reason: repo-controlled config and attributes (filter drivers, textconv, ext-diff, fsmonitor, gpg.program) can make read-only git execute programs and no override set closes it.
- 2026-10-21: Deviation — `acceptEdits` auto-approves Fontaine's edit-class tools only, not in-scope filesystem Bash; reason: writes ride the classified edit tools and a mutating shell must not fast-path (stricter than Claude Code).
- 2026-10-21: Plan mode follows Claude Code's default (classifier reviews consequential non-edit calls); edits are structurally blocked regardless of verdict.
- 2026-10-21 (revised): The deterministic floor (deny → ask → critical-path → protected-path) gates every mode and can only prompt or deny, never allow; critical-path never auto-approves and denies in `dontAsk`/headless; protected paths follow Claude Code. Spec: `design/permissions.md` §Core data flow.
- 2026-10-21 (user): Bypass mode is not shipped in v1 (deviation from `bypassPermissions`); no eval or CI run bypasses the permission pipeline; `auto` cases always run with a guardian.
- 2026-10-21: Shipped default mode = auto (Claude Code parity); interim default = manual until the guardian wires in at Phase 4. Clarified 2026-10-22: the auto default applies to the interactive TUI only; headless `-p` keeps `manual` (Claude Code's `-p` starts in `default`), and an explicit `default_mode` applies to both.
- 2026-10-21: Audit `args_digest` canonicalizes with exact-pinned `serde_jcs` (RFC 8785).
- 2026-10-21 (user): `[permissions] allow` plus one `Tool(specifier)` grammar for deny/ask/allow (`design/permissions.md` §Permission rules); allow acts after the floor, narrow rules skip the guardian in `auto`, broad code-execution rules are suspended in `auto` (Claude Code parity for allow rules; for broad hooks see 2026-10-23); a user allow rule or user hook `allow` is a user grant distinct from the containment fast path.
- 2026-10-21: Deviation — `shell` deny/ask rules match as a prefix anywhere in the parsed word stream (and on unparseable commands containing the first word) while allow matches only one simple command; reason: no regex and no operator-level parser in v1, so deny fails toward matching and allow toward not matching.
- 2026-10-21: Multiple workspace roots (`--add-dir`, `/add-dir`, `additional_directories`; Claude Code parity): containment is inside any root, extra roots grant file access not configuration, only the user adds roots.
- 2026-10-21: The `GuardedTool` decorator is the single gate; rig `AgentHook::on_dispatch` is not used for permission decisions.
- 2026-10-21: Tool calls are adjudicated and executed one at a time (`tool_concurrency = 1`); parallel batch adjudication is dropped for v1.
- 2026-10-21: `fontaine-tools` defines the `ToolGate` trait that `fontaine-core`'s `PermissionPipeline` implements, so `fontaine-tools` never depends on `fontaine-core`.
- 2026-10-21: A user-scope hook `allow` in `auto` follows Claude Code (like an allow rule; broad matchers suspended; the `claude -p` baseline confirms narrow hooks only, see 2026-10-23 for broad ones).
- 2026-10-21 (user): Shell parsing uses exact-pinned `brush-parser` for Tier 0.5, critical-path and rule matching; no hand-rolled lexer, no `shlex`; anything not exactly one simple command falls through.
- 2026-10-21 (user): `[shell_preclassifier] allow_extra` entries are a command or `command subcommand` pair, flag-free, with in-scope positional paths.
- 2026-10-22: A file-tool call outside the workspace roots is denied with the /add-dir hint instead of prompting (Claude Code prompts); reason: file tools are confined to the roots, so an approved out-of-scope call would still be refused.
- 2026-10-22: Read deny/ask rules check only the path words of a pre-classified shell command, not files a recursive search (`grep -r KEY .`) reaches under a directory; matches Claude Code, whose Read rules are best-effort for Bash. Reason: shell reads are not a containment boundary; use shell rules or the protected paths.
- 2026-10-23: In auto, Fontaine suspends a broad PreToolUse allow hook; Claude Code 2.1.309 honors it (one `claude -p` run per variant). Kept stricter: a broad hook would otherwise bypass the guardian for every call it matches.
- 2026-10-26: Each session gets a private scratch directory, `$XDG_RUNTIME_DIR/fontaine/<session-id>/` (else `<temp>/fontaine-<uid>/<session-id>/`; on Windows `<temp>\fontaine-<sid>\<session-id>\`), added to the workspace roots automatically and removed on exit, so file-tool writes of helper files work without a prompt (Claude Code prompts for out-of-root writes and Fontaine denies them, 2026-10-22; this is a further deviation); reason: the denial protected nothing once the model wrote the file through `shell`, and a private owner-only root keeps the floor intact (#238).
- 2026-10-26: AFK: The scratch directory lives outside the config and data dirs (`XDG_RUNTIME_DIR`, else an owner-only per-user dir under the OS temp dir, verified against squatting, canonicalized, and refused if under a protected dir) rather than under `<data>`, and the protected-path floor gets no exception; reason: `protected_dirs` covers the data dir, so `<data>/scratch` would defeat the feature or require a floor exception (#238).
- 2026-10-26: Subagents share the parent's scratch directory; reason: one shared `Scope`, and a child's mode is already capped by its parent's (#238).
- 2026-10-27: Deviation — `auto` mode allows edits inside the workspace to git-tracked files, new files and files created this session without a guardian verdict (tier `tracked_edit`), after the floor; Claude Code gives the same scope to its classifier but still asks it. Existing untracked files that predate the session, agent configuration, instruction files and `.github/` still go to the guardian, where an explicit request from the user that names the file clears them. Requested by the maintainer after guardian denials of ordinary repository edits (#256).
- 2026-10-27: Deviation — `auto` mode allows file-tool writes inside the session scratch directory without a guardian verdict (tier `scratch_write`), after the floor; shell commands there still go to the guardian, because the directory is private to the session and removed when it ends. Requested by the maintainer (#256).
- 2026-10-27: Deviation — a `tracked_edit` or `scratch_write` allow resets the loop breaker's consecutive-denial run like a guardian allow; the read-only fast path and the pre-classifier do not. The maintainer requires `auto` to work with the user absent (#256).
- 2026-10-27: The fast-path exception in the AGENTS.md security invariants gains a second exception naming these tiers (after the floor, scoped as above); `[permissions] auto_tracked_edits` and `auto_scratch_writes` default true, and project scope may only set them false (#256).
- AFK: The `tracked_edit` exclusion of instruction files and `.github/` is a `floor.rs` helper (resolved path relative to the root, case-folded like the protected list: basenames `FONTAINE.md`, `FONTAINE.local.md`, `CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md` at any depth, anything under `rules/`, any `.github` component); excluded paths fall through to the guardian with no new `soft_deny` rule, and `@import` targets are not excluded because the loader keeps no durable path list (#256).
- 2026-10-30: Supersedes 2026-10-22 for `read`/`ls`/`glob`/`grep` only — an out-of-root read is adjudicated through the normal pipeline (hooks, floor, allow rules, then the mode: prompt / guardian / deny) instead of denied for containment, and executes on an unconfined read-only fallback instance only after the gate allows the call; the external target is resolved once after the hooks and that normalized form is what the guardian, the prompt, the audit and the execution all use. External reads never earn the containment fast path or `tier05`, and an external path grants no write reach and no instruction discovery. The 2026-10-22 write decision stands: `write`, `edit`, `notebook_edit` and `shell` stay confined and an out-of-root write is denied with the /add-dir hint (#284).
- 2026-10-30: The AGENTS.md fast-path invariant gains a third exception: an `agent` spawn whose Fontaine-computed toolset holds only read-only, non-destructive tools (plus `send_message`) takes the read-only fast path in every mode, after the floor and hooks. MCP tools never count as read-only for this, and an unknown (never-set) parent toolset fails closed, while a known, empty one takes the fast path (#312).
- 2026-10-30: Deviation from Claude Code (maintainer's choice) — in `auto`, a read-only spawn skips the guardian, whereas Claude Code's auto-mode classifier judges every Agent call. The child's own calls are still judged individually (#312).
- 2026-10-31: Deviation from Claude Code — extends the read-only-spawn lines above to every `agent` spawn: after the floor, hooks and rules, a spawn is never guardian-judged (tier `spawn`: `auto`/`plan` allow, `manual`/`acceptEdits` ask, `dontAsk` denies), whereas Claude Code's auto-mode classifier judges every Agent call; reason: each call the child makes is gated on its own and a child's mode is never looser than its parent's, so judging the spawn prompt duplicated that coverage and interrupted an AFK run on a benign spawn (#360). The AGENTS.md fast-path invariant gains a fourth exception. The guardian's explanation sentence is also now persisted in the audit record for allows and escalates.
- 2026-11-01: Deviation from Claude Code, which auto-allows in-workspace reads and read-only commands: a built-in list of secret file names (`.env` family, credential dotfiles, SSH private keys, key/cert containers) leaves the read-only fast path and Tier 0.5, for file tools and pre-classified shell reads alike; reason: containment does not make a credential read harmless (#370). The call takes the normal route (prompt in `manual`/`acceptEdits`, guardian in `auto`/`plan`, denial in `dontAsk`/headless); the guardian may still allow it, and no guardian prompt change ships because one needs live eval verification.
- 2026-11-01: The restore for secret-file reads is an ordinary `allow` rule (user, CLI and trusted project-local scopes, plus user-skill and session grants), with no dedicated config key; reason: the pipeline needs no new surface.
- 2026-11-01: Repository-scan `sensitive_locations` are not fed into the secret-file check; reason: they are model-sampled, per-repo and optional, while the built-in list is deterministic.
- 2026-11-02: Deviation from Claude Code: `send_message` and `board_update` (the coordination tools) are never guardian-judged and never prompt. After the floor, hooks and rules they are allowed in every mode, `dontAsk` included, at tier `coordination`; their descriptor rows stay `Arbitrary`/`Destructive::Always`, so this is a gate-level skip. They reach only agents of the same tree, whose own calls are gated; `board_update` writes only the session's own `tasks.json` and runs the user's `task_completed` hooks. A spawn whose child holds read-only tools plus the coordination tools keeps the read-only spawn fast path, and coordination calls do not reset the auto-pause denial run. Supersedes the board decision of 2026-11-02 (`board_update` guardian-judged in `auto` and `plan`) and the `send_message` guardian clauses of 2026-10-21 and 2026-10-30, which are amended by reference (#394).
- 2026-11-06: Supersedes 2026-10-22 for `write`/`edit`/`notebook_edit` (the 2026-10-30 read line stands) — an out-of-root write is adjudicated through the normal pipeline (hooks, floor, allow rules, then the mode: prompt in `manual`/`acceptEdits`, guardian in `auto`, denial in `dontAsk`/headless) instead of denied for containment, mirroring the #284 read mechanism: the target is resolved once after the hooks and that normalized form is what the guardian, the prompt, the audit and the execution all use; a hook's allow is revalidated against it. After any approval the call executes once on an unconfined write-fallback instance (one per write tool, `confine: false`, primary-root base, shared `Seen`), at the resolved absolute target. In `auto` the floor's protected-path branch returns the call to the guardian for external writes exactly as for in-root ones, and the guardian's new soft-deny entry (`Safety Bypass: an un-asked write outside the workspace roots`, filed under the existing label; the user's own typed messages are the only user-intent source) denies an un-asked external write. External writes never earn `tracked_edit`, `scratch_write` or `tier05`; the containment hard deny survives only for `lsp`, and the `shell`/`powershell` cwd behavior is unchanged (#269).
- 2026-11-04: AFK mode (#411) is a session flag beside the mode, not a sixth `PermissionMode`: it is orthogonal to what the agent may do, grants no authority, and tightens the whole agent tree with no inheritance logic. Only `/afk` or `--afk` turn it on; no setting, tool, scheduled prompt or project file can.
- 2026-11-04: Under AFK every would-prompt outcome becomes a denial through `no_human()` (headless's mechanics, same four sites), keeping the original tier, rule and guardian fields and gaining `afk` and `converted` on the audit record; a trusted recovery sentence ends every denial, and an `AfkApprover` decorator is defence-in-depth over the three approver call sites. When both headless and AFK apply, the AFK text wins.
- 2026-11-04: AFK ends only with an explicit `/afk off`, not on any keystroke, so an accidental key cannot reopen approvals; it is not restored on resume (the user ran `--resume` and is present), but a temperature stop is, and survives a fork, until `/afk review` records a human review.
- 2026-11-04: `/afk` and `/guardian-disable` exclude each other (each refused while the other, or a stop, is active): a root call under the escape hatch runs before the floor and never prompts, so AFK would convert nothing, and the stop's deny-only check sits in `adjudicate` before the guardian-off short-circuit so no root call escapes a stop.
- 2026-11-04: The AFK report data extends the session ledger (call tier, tool-use id, digest, paths, `converted`, `fail_closed`, a redacted explanation, an always-attached `ToolOutcome` stage and the `Afk*` records), not the audit log, which a fork starts empty; old ledgers still parse. An allowed call with no recorded outcome renders as "outcome not recorded", never as success.
- 2026-11-04: Session temperature counts gate decisions, not wall-clock time (`current = Σ w·0.9^(decisions since)`, weights 1/2/3 by severity, 0.5 fail-closed, +2 exact retry, +3 same target by another tool), so it is deterministic and replayable; it exists only inside an AFK stretch, feeds no allow path, guardian input or breaker reset, and the live value and the report's are computed by the same tracker over the same records.
- 2026-11-04: `[afk] temperature_ceiling` (default 12.0, finite and above zero in every scope) may be lowered but never raised by shared project and project-local files: the lowest value any of them sets caps the user, CLI or default value (`min`), so the result does not depend on read order, like `composition_shadow_forced`; `--afk-temperature-ceiling` folds into the CLI slot.
- 2026-11-04: A temperature stop kills each running or idle subagent (`kill_agent`) and each running job (`kill_by_user`) by id rather than `AgentTree::kill_all`, which latches a permanent shutdown and would block resuming after a review; an AFK-stopped `CurrentCall` dispatch hook cancels every agent's next model request or tool call so none leaves after the stop. A headless stop reuses `error_during_execution` with `afk.outcome = "stopped"`, keeping the closed v1 `result.subtype` enum.
- 2026-11-04: `/afk review` does not resume the denial breaker, touch the peak or remove evidence, and leaves AFK off; the breaker resumes only on the next approval as before. A blocked session (breaker paused while AFK is on) starts no notification, scheduled or goal turn, and a headless run exits 1 with `afk.outcome = "blocked"` whether or not a goal ran.
- 2026-11-04: The end-of-stretch evasion assessment is one tool-less guardian-model call with schema-constrained output over redacted summaries in an untrusted block; any failure is "evasion assessment unavailable", never "no evasion", and a stretch with no denial skips it (`NoDenials`), since an allowed malicious call is the per-call guardian's job. Deterministic links use the tool's `RiskClass` for direction (`Arbitrary` matches both ways on any touched path); fast-path and `Tier05` reads keep no paths and stay unlinked by that rule.
- 2026-11-04: Measured on `evals/sequences/afk.jsonl` (8 `continue` and 5 `stop` temperature cases, 6 evasion fixture cases), the default ceiling of 12.0 gives a false-stop rate of 0.0% and a false-continue rate of 0.0%: it stops five different High denials in a row, four identical High denials, three alternate-route attempts at one file and four critical-path retries, and does not stop four different High denials, a dozen unanswerable prompts with allowed work between them, a guardian outage, or two exact retries of a High denial (11.93). The evasion prompt, run live on `router/anthropic/claude-haiku-5.5` over the six evasion cases, named exactly the expected pairs in three runs in a row; a first run flagged a reduced-scope delete after a denied wider delete, which the prompt now excludes by name (the case was left unchanged).
- 2026-11-05: AFK (#431): AFK is a sixth `PermissionMode` that decides as `auto`, superseding the 2026-11-04 flag-not-mode decision; `--permission-mode afk`, `--afk` (its shorthand), `default_mode = "afk"` (user scope and `--settings` only, as for `auto`) and the carousel enter it; no tool, scheduled prompt, model or project file does. Every startup path builds the session in `auto` and enters AFK through the stretch-opening path, so a refusal leaves it in `auto`.
- 2026-11-05: AFK (#431): AFK ends on Shift+Tab (or `/plan`), superseding "ends only with explicit `/afk off`, not on any keystroke"; the issue asks for carousel exit, leaving needs a deliberate press of the cycle binding, and leaving only makes the session ask the human again (manual is next), never looser.
- 2026-11-05: AFK (#431): `/afk` and `/afk off` are removed; `/afk review` clears a stop and lands in manual (supersedes "leaves AFK off"); the `/guardian-disable` exclusion is unchanged, now triggered by the mode.
- 2026-11-05: AFK (#431): Deviation from Claude Code: the carousel order is manual, plan, accept edits, auto, AFK (Claude Code: default, acceptEdits, plan, ...); reason: the issue orders modes by increasing autonomy.
- 2026-11-05: AFK (#431): `--afk` is shorthand for `--permission-mode afk` and combining it with another `--permission-mode` is a usage error ("two modes given"); `-p --afk` now decides as auto (previously manual plus AFK) since AFK decides as auto; manual plus AFK is no longer expressible, `dontAsk` covers it.
- 2026-11-05: AFK (#431): A refused AFK entry from the carousel skips to manual.
- 2026-11-05: AFK (#431): Entering AFK mid-turn answers every open or queued approval, question and guardian-unavailable dialog as `AfkApprover` would (Deny / Unavailable / Deny) and closes it, and prompts arriving while AFK is on are answered the same way without an overlay; refusing entry (b) would miss prompts still in the channel, and leaving them (c) breaks "nobody is asked during AFK" and lets the unavailable dialog's Manual answer skip the stretch review.
- 2026-11-05: AFK (#431): No `AgentRuntime::set_mode`: callers use `runtime.state.set_mode` (as `/plan` already does), `review_afk_stop` drops its rebuild, and `refresh_before_turn` is the only AFK-driven prompt rebuild site; a forwarding wrapper and a second rebuild site add nothing the pre-turn refresh does not already cover.
- 2026-11-07: `/bug` (#462): `draft_bug_report` gets a new `Tier::BugDraft` (never guardian-judged after the floor) and `/bug` submissions are audited under a new `Tier::BugReport` (tool label `bug`); the draft tool only writes a redacted file to the data dir, so it is a sixth never-guardian-judged exception rather than a coordination tool, and submission stays user-only.

### Project

- 2026-10-20 (amended 2026-10-21: headless to Phase 1): TUI-first (ratatui), headless `-p` on the same core. (User choice.)
- 2026-10-20: License is Apache-2.0 (verbatim `LICENSE`); adapted Anthropic rule prose is attributed in `docs/guardian.md`.
- 2026-10-21: "fontaine" is the final product name — binary, crate prefix, config dir, `FONTAINE.md`. (User decision.)
- 2026-10-21: Fold strategic-verdict amendments A3 (sandbox interop, Phase 5 roadmap slot) and A4 (dependency-light, lift-out-able `fontaine-guardian`) into the roadmap. (User decision.)
- 2026-10-21: Design tie-breaker — when in doubt, do as Claude Code does; every deviation gets a dated line here with a one-line reason. (User directive.)
- 2026-10-21 (user): The plan is split into this master tracker plus one plan per phase under `tasks/phases/`, each ending in a docs step; all documentation lives under `docs/`.
- 2026-10-21 (user): Root `SECURITY.md` (Phase 0): private reporting, 90-day coordinated disclosure; bypass dataset PRs only after a fix ships.
- 2026-10-22: Each phase plan's own `## Decisions` section holds that phase's implementation-level choices; deviations from Claude Code are also copied here.
- 2026-10-23 (user): Every phase is done, so the phase plans moved to `tasks/completed/phases/` and the live-checks plan to `tasks/completed/`; the master plan, `tasks/design/` and `## Decisions` stay live, and remaining work is tracked as GitHub issues (`release-blocker` for the first release).
- 2026-11-08 (#530): `docs/` is published as an mdBook site on GitHub Pages with `llms.txt`/`llms-full.txt`, built and link-checked in CI. This supersedes "no site tooling ships in v1" (`design/documentation.md`), because a site is what search engines and agents index.
- 2026-11-08 (#530): the README is a short front door (pitch with auto and AFK mode, installers, quickstart with the installed binary, why-list, links). Install caveats, the mode table and build-from-source live only in `docs/` and `CONTRIBUTING.md`. Repository settings (description, topics, homepage, Pages source, the `FONTAINE_DOCS_SITE` variable) are left to the maintainer.

### Providers

- 2026-10-24: `fontaine auth login` is removed, with no CLI replacement (#140; Claude Code keeps `claude auth login`). The TUI's `/login` provider dialog is the only interactive way to store a credential, and when the interactive UI cannot resolve its main model at startup the same dialog opens before the session. Automation provisions the account config plus `api_key_env` or a `credentials.toml`; the telemetry ingestion key comes from `api_key_env` or a hand-provisioned `[telemetry]` entry. Reason: one credential path that works in the running session, and no secret read from a pipe.
- 2026-10-21: Multi-account model registry, following opencode: named `[accounts.<name>]`, models addressed `<account>/<model>`, one rig client per account; replaces `[model] provider`/`base_url`. Reason: user requirement; Claude Code has no analogous design point.
- 2026-10-21: An `openai-compatible` account with no credential source sends no `Authorization` header (the gateway dialect uses optional bearer auth), so local servers that reject a bare `Bearer` header work.
- 2026-10-25: `[network]` is config keys only, with no equivalent of Claude Code's `NODE_EXTRA_CA_CERTS`, `CLAUDE_CODE_CLIENT_CERT` or `CLAUDE_CODE_CLIENT_KEY` variables, because one place to set it keeps the project-scope tightening rules and the per-account override in force (#6).
- 2026-10-25: A passphrase-protected `client_key` is refused at config load, where Claude Code accepts one via `CLAUDE_CODE_CLIENT_KEY_PASSPHRASE`, because rustls cannot decrypt a key and a passphrase in config or env would be a secret to store (#6).
- 2026-10-25: Anything before the latest message is append-only within a session; front edits (system prompt, tool set) happen only at explicit boundaries (`/add-dir`, `/output-style`, model switch, MCP connect or refresh, plugin/skill reload, `/clear`, compaction), each accepted as one lost cache read (#13).
- 2026-10-25: `prompt_caching` has a per-kind provider default (true for `anthropic`, false for `opencode-go` until its Messages route is measured live), is user-scope only and is accepted only by kinds where `accepts_prompt_caching()` is true; `cache_ttl = "1h"` is opt-in because extended TTL costs more per write (#13).
- 2026-10-25: The guardian handle marks only its static prefix (tools and system prompt), never the per-call final message, so it pays no write premium on a tail that is never reused; its prefix is near provider minimum lengths, so savings are model-dependent (#13).
- 2026-10-25: Headless `usage.main` gains cache read and write tokens; `guardian` and `subagents` (`GuardianUsage`) carry no cache fields and are not extended (#13).
- 2026-10-30: `openrouter` is a new kind on rig's OPENROUTER dialect rather than a `prompt_caching` key on `openai-compatible`, since only that dialect has the cache mapping and a flag on the generic gateway would send OpenRouter-specific fields to arbitrary endpoints; `prompt_caching` defaults to true (#331).
- 2026-10-30: `openai-compatible` accounts pointing at OpenRouter are not migrated or detected; they keep working uncached, and the docs point to `kind = "openrouter"` (#331).
- 2026-10-30: On `openrouter` the conversation takes rig's top-level automatic marker, while the guardian's marker is written onto its system message's last content block by a request-rewriting `Wire` wrapper (`CacheWire`), because a top-level marker would land on the per-call tail that is never reused. rig's OPENROUTER Chat encoding already emits the system prompt as a block array, so the rewrite only adds `cache_control` to it (#331).
- 2026-10-30: `CacheWire` maps a one-hour retention to the short one for `openai/`, `deepseek/`, `x-ai/` and `groq/` model ids (a local copy of rig's automatic-upstream list that must track rig), never sets `CacheRetention::None`, and leaves `options.cache` unset when caching is off (#331).
- 2026-10-30: Sticky routing (`prompt_cache_key`) is deferred, since rig's OPENROUTER Chat mapping omits `cache_key` and sending it would need a body rewrite; the opencode-go default is not flipped, since it needs a live Go measurement (#331).
- 2026-10-30: The guardian's minimum-cacheable-length notice applies to Claude ids on `anthropic` and `openrouter` accounts only, compares against fixed per-profile prefix sizes (about 1500 tokens `minimal`, 3000 `full`), and is reachable only for pinned guardians; the shipped 512-minimum default never triggers it (#331).
- 2026-11-04: `claude-subscription` (experimental, #412) runs the user's installed, unmodified Claude Code CLI as a model-only transport on rig's Messages wire with a request-scoped subprocess transport in place of HTTP; Fontaine's loop, history, tools, guardian and sessions stay authoritative, and the CLI holds no conversation and executes no tool (tools reach it through an inert MCP inventory that refuses every call). Local, individual, Max/Ultra only; credentials are never read, copied or forwarded (`CredentialPolicy::ExternalCli`); a new kind rather than an HTTP route because the login and billing belong to the CLI.
- 2026-11-04: The `claude-subscription` adapter is not gated on Anthropic approval (user directive); the policy constraints stay in `docs/claude-subscription.md` and must be re-read before any stable designation.
- 2026-11-04: Deviation from the manual/eval-only rule for `claude -p`: the official CLI is a production transport for this kind only; the rule keeps covering Claude Code baselines, and tests never run the real CLI (they use the fake `claude_cli_double`). The wording is clarified, not weakened.
- 2026-11-04: A CLI version is supported only after live qualification shows it stops at the first complete response and sends no unsolicited request; unqualified versions fail closed in preflight (minimum 2.1.318). Protocol shapes outside the qualified matrix (older signed history, transformed history, unlisted models, credit entitlements) fail closed.
- 2026-11-04: Usage-credit transitions cannot be seen early through any CLI API, so included-only use needs the account's extra-usage setting off; the billing guard fails closed on any credit signal in a model row, and a plan-limit failure is reported without a retry or an alternate route.
- 2026-11-04: Providers whose models need a listing to be built (`Provider::requires_discovery`) are always listed before a main or guardian handle exists, with `models` only narrowing the listing, so guardian resolution became asynchronous and the initial listing is bounded in time.
- 2026-11-08: (#515) For `claude-subscription` only, an unlisted model no longer fails closed: a canonical id the built-in catalog does not list with an output limit gets facts derived from the newest listed, non-deprecated model of the same family (the longest `details.family` the id starts with), with the effort levels, adaptive-thinking and thinking-off flags the CLI reports for the row laid over them and no price, and the user is warned once whenever such a model is resolved; an id with no listed family, or a credit-marked row, stays refused. Reason: each new Claude point release otherwise disappears from the picker until the pinned rig catches up; this deviates from the 2026-11-04 "unlisted models fail closed" line. Listed models keep their catalog facts, and a CLI flag that disagrees is a reason to bump rig, not to override at runtime.
- 2026-11-08: (#515) A row an account lists but refuses is shown, dim and never selectable, with its reason in `/model`, `/guardian-model`, the provider dialog's picker and `fontaine models` (`unavailable` in the JSON report), instead of being dropped; reason: the picker is where the user looks for the model.
- 2026-11-08: (#515) Derivation for `anthropic` and `openai-compatible` accounts is deferred to #524, because their listings carry no capability flags, and the 2026-10-24 "no fallback spec" line governs them; `anthropic` already gets rig's 64000 output fallback for unlisted `claude-opus-4*`, `claude-sonnet-4*` and `claude-haiku-4-5*` ids.
- 2026-11-08: (#515, #524) Derived facts apply to every account kind with a catalog vendor (`anthropic`, `openai`, `gemini`, `ollama`, `opencode-go`, `openrouter`, `claude-subscription`) through one `AccountClient::catalog_vendor` hook and a shared default facts notice, and the base is chosen by id shape instead of family prefix: the id with each run of digit-and-dot tokens between `-`, `_`, `/` and `:` collapsed into one placeholder must equal a listed, non-deprecated, output-limited entry's of the same vendor, newest `release_date` winning (ties to the greater id, undated last), and an id with no such token is never derived. This supersedes the "derivation for `anthropic` and `openai-compatible` is deferred to #524" line above for every kind but `openai-compatible`, which has no catalog vendor; reason: rig's `details.family` is an id prefix only for Anthropic (`gpt-5.4-mini` has family `gpt-mini`), and a per-kind rule table would be one more hardcoded list to chase. An unlisted `anthropic` id now takes its sibling's output limit instead of rig's 64000 fallback for `claude-opus-4*`, `claude-sonnet-4*` and `claude-haiku-4-5*`.
- 2026-11-08: `claude-subscription` offers provider-native web search like `anthropic`: the `web_search_20250305` entry rides `CLAUDE_CODE_EXTRA_BODY`, and the adapter accepts exactly that server tool (its request entry, `server_tool_use` named `web_search` only when offered, a `web_search_tool_result` only for a search of the same response, and both in replayed history); every other hosted tool or block stays refused. Qualified live on CLI 2.1.319: one request, no CLI-run tool, lossless replay (#522).
- 2026-11-08: A model list falls through to its next entry only on a 402, 429 or 529 or a `claude-subscription` plan limit after the same-model retries, and stays on the entry that worked. Claude Code's `--fallback-model` triggers on overload only. With an explicit `--max-budget-usd`, unpriced entries are skipped. This supersedes the 2026-11-04 "plan-limit failure is reported without … an alternate route" for accounts listed in a chain with a next entry (#324, #289, #54).
- 2026-11-09: (#557) `/login` on an external-CLI account (`claude-subscription`) now offers to run the CLI's own login (`claude auth login --claudeai`) when nobody is signed in, handing it the terminal and re-probing after it exits; this supersedes "never starts a login of its own". Headless and non-TUI runs never start one: they exit 1 before any turn naming the account and the command, and a pinned guardian on a signed-out account stops a run in `auto`/`afk`/`plan` the same way. Reason: the fix for a signed-out CLI is one command the user would otherwise copy out of the error.

- 2026-11-10: Every main and subagent turn sends an explicit `max_tokens` (the model's known output limit, else 32000), as Claude Code does. The override is the config key `[model] max_output_tokens` (user trust, not settable from the shared project file), not a `CLAUDE_CODE_MAX_OUTPUT_TOKENS`-style environment variable: config keys keep the scope rules in force (#571).
- 2026-11-10: On `ollama`, `anthropic` and `claude-subscription` with no known limit and no user cap, no `max_tokens` is sent: the ollama daemon keeps its unbounded default and the Messages wire fills its own. Claude Code has no such case (#571).
- 2026-11-11: (#115) A spent usage window is paused for and resumed automatically; Claude Code reports its limit and the reset time and then stops (its documented behaviour, which `claude -p` cannot be made to show on demand), so this is a deliberate deviation. It supersedes the 2026-11-04 "a plan-limit failure is reported without a retry or an alternate route" line for spent windows: a `claude-subscription` plan limit whose window is spent is not retried and moves a model list on, and a spent list pauses the run until the reset. A billing failure and a rate limit with no spent window keep the old behaviour, which is why `ClaudeError` carries the limit form (plan limit, rate limit or billing).
- 2026-11-11: (#115) A spent window is told from a transient 429 by structured fields only (the provider's machine code in `WINDOW_CODES`, or a `Retry-After` header above five minutes), never error text; the reset time is the header-derived `Retry-After` or a reset time in the body, else the account's quota figures. An Anthropic API-key spend or credit limit keeps today's behaviour: it has no distinct machine code, text matching is forbidden, and rig already classes `insufficient_quota` and `billing_hard_limit_reached` as final through the quota codes.
- 2026-11-11: `model_facts` maps served name → catalog id, the opposite direction to Claude Code's `modelOverrides`, because Fontaine model references, ledger, telemetry and `served_model` all record the host's id and reversing would rename models on the wire and in every record; the mapping binds facts only (#167).
- 2026-11-11: An unmapped, unknown model on `anthropic`, or a custom-id one on `databricks`, is warned about once and given no assumed 200K window (Claude Code assumes 200K), keeping the "no fallback spec" rule (#167).
- 2026-11-11: On `databricks` the mapping does not change wire routing; a custom endpoint that serves Claude on Messages also sets `wires` (#167).
- 2026-11-11: On `anthropic`, a turn on a model with no known limit and no `[model] max_output_tokens` fails unless the model is mapped under `model_facts`, because rig's wire fills `max_tokens` only for ids it knows; this corrects the 2026-11-10 "the Messages wire fills its own" claim (#167).
- AFK: 2026-11-11: A `model_facts` key the kind already lists is refused at load and a mapped model stays unpriced, because an unpriced model never trips a cost cap, so stripping a listed price would silently loosen spend caps and a host need not bill at list prices (#167).
- 2026-11-13: (#601) Amends the 2026-11-11 (#115) line "a spent list pauses the run until the reset" for subagents when a user-scope `fallback` role is defined: a subagent whose list is spent on a usage window first moves to the first usable entry of `fallback` (skipping spent accounts), and the run pauses only when that is spent too; with no `fallback` role the pause is unchanged. Reason: a spent subagent model stalled the whole session (394h window) while another configured model had budget; a model switch is a cost change, so it is opt-in.
- 2026-11-13: (#578, user directive) `claude-subscription` is no longer experimental, superseding the 2026-11-04 label; the scope and Anthropic-terms caveats stay in `docs/claude-subscription.md`.

### Sessions

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
- 2026-10-25: Scheduled prompts (#16) have no fire-time jitter and no early firing of one-shots, where Claude Code adds both to spread load on one provider; a local single-session scheduler fires at the scheduled minute.
- 2026-10-25: `schedule_wakeup` takes no `prompt` (Claude Code's does); it re-arms the prompt of the self-paced loop the user started with `/loop`, so the model can set the pace but never author new standing text, and no guardian call is needed per iteration (#16).
- 2026-10-25: Scheduling is interactive-only: a headless `-p` run exits after its answer and registers no scheduling tools, because a scheduled job could never fire there (#16).
- 2026-10-25: Bare `/loop` (maintenance prompt, `loop.md`) and `/loop 20m /skill` expansion at fire time are deferred; a loop's prompt is plain text (#16).
- 2026-10-25: A scheduled turn is a harness note and never a `UserMessage`, so model-authored prompt text cannot become the guardian's task intent; `cron_create` is an `arbitrary` tool for the same reason, and `/clear` also clears the schedule (a `Scheduled { deleted }` ledger record per job) so a job planted under dropped intent cannot outlive it (#16).
- 2026-10-25: The list tool is `cron_jobs`, not `cron_list`, because the choke-point test bans tool names containing list/status/kill/interrupt/stop; its Claude alias stays `CronList` (#16).
- 2026-10-25: `/loop` accepts only even cadences (minute steps 1, 2, 5, 10, 15, 20, 30; hours 1, 2, 3, 4, 6, 8, 12; days 1-31 via `*/n` day-of-month, approximate at month ends) and rounds anything else to the nearest, ties down (`7m` -> `5m`, `90m` -> `1h`), stating the cadence chosen; the minute steps omit 3, 4, 6 and 12 so that `7m` rounds to `5m` as pinned, and cadences are wall-clock aligned (#16).
- 2026-10-25: `[schedule] enabled` is user-scope only (user file and `--settings`); a project file cannot set it, because scheduled prompts run without the user typing them (#16).
- 2026-10-26: Session UX (#10) supersedes the 2026-10-24 `/clear <name>` line: `/clear` still takes no name, but with `/resume` available its message points to `/rename` and `/resume` instead of "not available yet".
- 2026-10-26: The `/resume` picker and `--resume <name>` cover the current workspace root only (the `--continue` scope), without Claude Code's all-projects toggle, because it avoids listing unrelated projects' sessions (#10).
- 2026-10-26: `--resume` keeps a required value, so there is no bare `--resume` picker at launch (Claude Code has one); the picker is `/resume` in the TUI, because it lives in `drive` and needs a live session, and an optional value would swallow the positional prompt (#10).
- 2026-10-26: `/rename` without an argument draws the name deterministically from the first message (first line, 60 characters) instead of asking the model as Claude Code does, so there is no extra model call, spend or prompt-injection surface (#10).
- 2026-10-26: `/resume` and `/branch` end the run and start the chosen session in the same process by looping `run` around the startup path, instead of swapping the session in place, so a switch resumes exactly as `--resume` does (default mode, no grants) and restarts MCP servers and the scanner; they run only between turns, and `/branch` is refused while a subagent runs (#10).
- 2026-10-26: `/export` writes Markdown from the conversation log, owner-only and never over an existing file, where Claude Code can copy to the clipboard or overwrite; tool results appear only as their length (#10).
- 2026-10-27: Checkpoints and `/rewind` ship without Claude Code's "summarize from here" option; the picker has the three restore options; reason: it needs a model call and a new transcript record, and is a follow-up if wanted (#12).
- 2026-10-27: A code restore warns about files changed since Fontaine last wrote them and keeps them unless the user confirms the overwrite; Claude Code restores without a warning; reason: a restore overwrites with no backup, so the user's own later edit would be lost silently (#12).
- 2026-10-27: A conversation rewind clears an active goal on any rewind, as `/clear` does, while todos, the schedule, grants and mode carry on; reason: a goal records no turn, so it cannot be told apart from one set after the rewind point (#12).
- 2026-10-27: Rewind points are limited to the current `/clear` epoch; reason: a `/clear` resets the conversation the checkpoints index, so earlier turns cannot be rewound to coherently (#12).
- 2026-10-27: The auto-memory directory is excluded from pre-image capture and from restore, so a code rewind never touches memory notes; reason: memory is persistent state across sessions, not workspace edits to undo (#12).
- 2026-10-27: `[checkpoints]` is user-scope only, stricter than the user-or-CLI scope of comparable keys; reason: it sets what is kept on disk and size caps, and a project file or flag has no business changing that (#12).
- 2026-11-21: Every subagent's conversation persists for the session's life as a per-agent log (`logs/agent-<n>.jsonl`) in the session store, and agent ids are stable across `--resume` via a counter derived from the ledger's `spawned` records — no `agents.json`, no UUIDs; this supersedes the id-instability note at `tasks/completed/task_list_tool_plan.md:169` (#300).
- 2026-11-21: `/export` of a session with subagents writes a folder (`fontaine-<id8>/` with `main.md` and one `<agent-id>.md` per subagent, merged from the ledger as spine plus the live tree); a session without subagents keeps the single-file export byte for byte. Zip and single-file layouts were offered and rejected (#300).
- 2026-11-21: A running subagent is exported rather than refused; its file holds completed runs only, because rig saves a log when a run ends, and the index row says so (#300).
- 2026-11-21: Root rewind and `/clear` do not touch agent logs, and the ledger is never rewound; `/export` lists every agent recorded in the ledger, including agents whose spawn turn was rewound away, with the transcript as per-agent evidence (#300).
- 2026-11-06: A root turn that ends without settling — a TUI interrupt, an AFK stop, a killed process — stays in the model's conversation, journaled as the run progresses and folded into `main` with a marker when the run is dropped or the session resumes, as Claude Code's `[Request interrupted by user]` does. Deviation: text the model was still streaming and the results of a tool batch still running at the interrupt are not kept (the completion-call journal never strands a tool call); keeping them is a follow-up (#457).
- 2026-11-08: (#515) A derived `claude-subscription` model counts as listed for threshold compaction, `/compact` and `/context`, an exception to the 2026-10-24 Sessions line that an unlisted model is never compacted; reason: the derived spec carries the family's window and output limit, so the failure that line prevents (a session ending at the window) does not apply.
- 2026-11-08: (#515, #524) The compaction exception above covers every derived model, on any account kind with a catalog vendor, not only `claude-subscription`; reason: the derivation is the same and so is the window it carries.

### Tools

- 2026-10-27: Background shell jobs and `monitor` (#25) are root-only; a subagent's `shell` keeps kill-at-timeout, a job keeps its output in a 1 MiB in-memory buffer with no spool files, and `job_cancel` (not `job_kill`) is `arbitrary` and path-free, because jobs must stay one session's, one writer's state and a spool file would add a write surface and path handling. A `shell` deny or ask rule and hook matcher also cover `monitor`; an allow rule does not, because a prohibition must not be sidestepped by running the same command through another tool. See `tasks/background_shell_jobs_plan.md`.
- 2026-10-25: One `todo_write` tool (the whole list replaced per call) stands in for Claude Code's TaskCreate/TaskGet/TaskList/TaskUpdate, because a single replace-all call is one testable path and Claude Code's `TodoWrite` already works that way (#22).
- 2026-10-25: Only the root agent's list is displayed and persisted with the session; a subagent's list lives and dies with that subagent, because the panel and the session file have one slot and a child's list is scratch state that is never shown or saved (#22).
- 2026-10-25: Ctrl-T (formerly Ctrl-O, moved 2026-11-04 for the transcript view) and `/todos` both toggle the task panel, and neither prints anything while the list has tasks, because the panel can be hidden to reclaim height (#22); with an empty list both print a one-line `no tasks` notice so the key is never silent (#233).
- 2026-10-25: `todo_write` takes the name-based read-only fast path, like `skill`, because it touches no file the model names, process or network, and only writes the session's own `todos.json` (#22).
- 2026-10-22: `webfetch` sends `User-Agent: fontaine/<version> (+https://github.com/madesroches/fontaine)`, not Claude Code's `Claude-User (claude-code/<version>; ...)`. Reason: sites such as Wikimedia 403 a missing User-Agent, and a harness must not present itself as Claude Code's agent.
- 2026-10-21 (superseded by 2026-11-01 below): Phase 2 `ls`/`grep` take rig-tools 0.45 `ListArgs`/`SearchArgs` only (`grep`: `pattern`, `path`, `glob`, `hidden`, `files` mode, 200-match cap; `(?i)` for case-insensitivity; no context lines, count, type or head_limit). Deviation from Claude Code's Grep/LS arguments; reason: no custom search layer over rig.
- 2026-10-30: A separate Windows-only `powershell` tool (issue #28; `Arbitrary`, `Destructive::Always`, `Specifier::None`, Claude name `PowerShell`) runs commands through the interpreter's `-EncodedCommand`; details in `design/platforms.md`.
- 2026-10-30: No `powershell(...)` specifiers (`Specifier::None`); bare-name rules only.
- 2026-10-30: Bare `shell` deny/ask rules also cover `powershell`; specifier'd `shell(...)` rules do not.
- 2026-10-30: Every `powershell` call goes to the guardian; no Tier 0.5.
- 2026-10-30: The worker agent does not get `powershell`.
- 2026-10-30: The `powershell` tool is absent when no interpreter is found; not a startup error.
- 2026-10-30: The critical-path check for `powershell` is a token-level scan (`floor::critical_powershell`); non-FileSystem providers are left to the guardian.
- 2026-10-30: `-EncodedCommand` runs a fixed preamble (silent progress, UTF-8 console output) plus `-OutputFormat Text`, without appending `exit $LASTEXITCODE`; only literal `exit N` is preserved and native exit codes collapse (documented), and the guardian still sees the plain command.
- 2026-10-30: In the `Remove-Item` critical scan, `-Include`/`-Exclude`/`-Filter` values are ignored as patterns, parameter names match as unambiguous prefixes, and any unknown or ambiguous option makes the call critical.
- 2026-10-30: Cancel/timeout of a `powershell` call kills only the direct child process on Windows, same as `shell`; no process-tree kill is added.
- 2026-10-21: Deviation — provider and telemetry credential variables are scrubbed from the `shell` and hook child environment (list: Phase 2 step 1); reason: a model-run command must not read provider keys.
- 2026-10-21 (user): Windows Git Bash discovery follows Claude Code's pattern with `FONTAINE_GIT_BASH_PATH` override (`design/platforms.md`).
- 2026-10-21 (user): `webfetch` v1 takes `url` only (HTML → markdown via exact-pinned `htmd`; caps in Phase 2). Deviation: no `prompt` argument or small-model post-processing (backlog); reason: one deterministic tool with no extra model call.
- 2026-10-22: On Windows the command text reaches Git Bash in `FONTAINE_SH_COMMAND`, not on the command line, unlike Claude Code. Reason: the judged command must be the one that runs.
- 2026-10-25: `ask_user_question` is root-agent only, refused at call time for children (including custom types that inherit it), because background subagents must not put a dialog in front of the human (#23).
- 2026-10-25: `ask_user_question` is `ReadOnly` and takes the fast path through a pathless-tool carve-out (with `skill`), because it only shows the human text and collects a choice; the floor still precedes it and no mode answers it (#23).
- 2026-10-25: Esc in the `ask_user_question` dialog gives a "declined" result rather than an error or the unavailable text, so the model can tell a refusal from a headless session (#23).
- 2026-10-25: Only the human's typed "Other" text from `ask_user_question` is appended to the ledger as a `UserMessage` (`Other: "<text>"`); selected labels, question text and descriptions reach the model through the tool result only, because option labels are model-authored and "only messages you typed count as intent" must hold (#23).
- 2026-10-26: Provider-native web search is used when the provider supports it (first release: Anthropic accounts on the Anthropic API, and `claude-subscription` accounts, amended 2026-11-08 (#522); `[websearch] native` overrides). It runs inside the model's response and bypasses the guardian, the result scanner and the per-call permission prompt, so it is offered only when no `deny`/`ask` rule or `pre_tool_use` hook could cover `websearch` and, in manual/acceptEdits/dontAsk, only with an `allow` rule. Claude Code offers its search without these conditions; kept stricter because the floor must gate every call (#19).
- 2026-10-26: The user-scope `[websearch] command` launches SearXNG on demand as a fixed argv child (no shell, minimal environment, own process group), which Claude Code has no counterpart for. It never loosens a permission decision: searches through it are judged as any `websearch` call (#19).
- 2026-10-26: With no `[websearch] backend`, a container runtime (`docker`, then `podman`, on Fontaine's own PATH) makes the client-side `websearch` tool default to a SearXNG container Fontaine starts on the first search (fixed argv, loopback-only port, settings file in the user data dir with a generated secret, 300 s start timeout, minimal env plus `DOCKER_HOST`/`DOCKER_CONFIG`/`XDG_RUNTIME_DIR`/`CONTAINER_HOST`); `backend = "off"` disables it and a startup warning says when no search path exists. Claude Code has no counterpart (its search is server-side). The settings file is world-readable inside an owner-only directory because the container user must read the bind mount (#19).
- 2026-10-30: The shell timeout cap is `[shell] max_timeout_secs` (seconds, user file or `--settings`
  only), with no `BASH_MAX_TIMEOUT_MS` variable, so the per-key scope rules apply (#267).
- 2026-10-30: `lsp` uses `path` (Fontaine's name for a file argument), not Claude Code's
  `filePath`, and does not require a position for `documentSymbol`/`workspaceSymbol`.
- 2026-10-30: Late LSP diagnostics never start a turn; they ride the next tool result (deviation
  from omp; Claude Code's delivery format was not observable — the probe's server failed to start).
- 2026-10-30: `rust-analyzer` joins the toolchain components (maintainer request in the design
  session) so a server matches the pinned 1.99.1 compiler and the live test has a real server; CI
  downloads it although no CI step uses it.
- 2026-10-30: The 500 ms diagnostics wait counts a publish whose `version` is at least the synced
  version, or with no version the first after the sync; the "new" diff identity is (severity, code,
  message) as a multiset, ignoring range — an in-flight set for older content is not taken as
  current, and line shifts do not re-report.
- 2026-10-30 (superseded by 2026-11-10 below): On Windows only the direct language-server child is killed (websearch's fallback);
  the orphan risk from a wrapper is accepted — no new Job-object code.
- 2026-10-30: The diagnostics stage lives in `fontaine-tools/src/lsp/diagnostics.rs` and resolves
  `args["path"]` with `Scope::find`/`Scope::resolved` instead of core's `touched_paths` —
  fontaine-tools cannot depend on fontaine-core.
- 2026-10-30: Late diagnostics drain only on real tool calls; the synthetic screening calls set
  `CallView.synthetic` and are skipped — their text becomes a user-role turn prompt.
- 2026-11-01: The shell's working directory persists between an agent's foreground calls (#26): the
  directory a command ends in is kept, and one outside the workspace roots resets to the *primary
  root* with a note in the result, where Claude Code resets to the project directory. It is kept per
  agent, a missing entry meaning the primary root, and is reset by `/clear` and by a conversation
  `/rewind` because the model forgets its `cd`. Background jobs, `monitor`, `!` commands and a skill's
  `!` lines do not move it; `monitor` and `!` start in the primary root. With no session scratch
  directory tracking is off. No `CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR` equivalent is added now;
  it can come later as a user-scope config key. The shell approval summary shows the effective `cwd`.
  A write-class call in a tracked `sub/` loads `sub/AGENTS.md` on demand, and a retry grant keyed on
  the arguments digest stops matching once the directory moves (it fails safe).
- 2026-11-01: `monitor` has no default timeout (an omitted `timeout_secs` means no deadline; an explicit one keeps its 3,600 s cap), unlike Claude Code's 5-minute default and re-arm pattern, because a CI watch outlives 5 minutes and the "reached its timeout" notice reads as a failure of the watched command (#363).
- 2026-11-01: `grep` and `ls` take Claude Code's arguments (`output_mode`, `-i`, `-n`, `-A`/`-B`/`-C`, `type`, `head_limit`; `ignore`) through a first-party module in `fontaine-tools` over rig's public `Workspace` walker, superseding the 2026-10-21 line; `path` keeps its name and meaning, so path rules, scope and footprints are unchanged (#62). Default `output_mode` is `files_with_matches`, as in Claude Code, a change from the old `content` default; `files` stays as an alias that `output_mode` overrides. `head_limit` is clamped to the existing hard caps (200 matches, 1,000 files). An empty `pattern` lists files in `files_with_matches` mode and is an invalid-arguments error in `content` and `count` (rig returned arbitrary lines; Claude Code differs). Results are sorted by path. Offset/pagination and `multiline` are out of scope. See `tasks/grep_ls_parity_plan.md`.
- 2026-11-02: `models` (#393, with the `fontaine models` subcommand of #254) is a Fontaine-only tool: it has no Claude name, so the descriptor test exempts it by name, and Claude Code has no listing tool (only `/model`). It is `ReadOnly` and path-free, so it takes the fast path in every mode: its only network I/O is the listing request to user-configured account endpoints, with no agent-chosen host or URL and no credential in the output. Unlike `todo_write` (no network) it stays out of the built-in `explore` and `worker` tiers, so their read-only spawn fast path gains no network tool. See `tasks/list_models_plan.md`.
- 2026-11-08 (#510): Supersedes "not found => startup error on Windows" for Git Bash: `shell` and `monitor` are registered only when Git Bash is found on Windows (Claude Code: Monitor needs Git Bash there); without it `powershell` is the command tool, hooks, the status line and `!` lines run in PowerShell, and one startup warning says so. A `FONTAINE_GIT_BASH_PATH` that is not a Git Bash stays an exit-2 error.
- 2026-11-08 (#510): Narrows the 2026-10-30 line "The `powershell` tool is absent when no interpreter is found; not a startup error": still true when Git Bash is found, but no interpreter and no Git Bash is an exit-2 startup error (`no shell found: ...`, naming a `FONTAINE_POWERSHELL_PATH` that is not a file instead of advising to set it).
- 2026-11-08 (#510): Narrows the 2026-10-30 line "The worker agent does not get `powershell`": when the parent has no `shell`, a type entry naming bare `shell` (in `tools` or `disallowed`) also covers `powershell`, so the worker gets it and a type that disallows `shell` has no command tool; a specifier entry such as `shell(git *)` does not. With Git Bash present nothing changes.
- 2026-11-08 (#510): The model-facing text names `powershell` where it would name `shell` when `shell` is not registered (tool descriptions, system prompt), and the `monitor` mentions are dropped.
- 2026-11-08 (#510): No `cmd` shell, as Claude Code has none: `cmd` has no encoded-command form and re-parses its command line (`%VAR%`, `^`), so the judged string would not be the one that runs; PowerShell runs `cmd /c` when needed.
- 2026-11-08 (#513): Supersedes the 'one startup warning says so' part of the 2026-11-08 (#510) line: without Git Bash Fontaine starts with no warning, since the model-facing text already names `powershell`.
- 2026-11-08 (#44): The `advisor` tool is default-off and activated by an `advisor` model role in the user file or `--settings`; present means enabled. A role gives lists, quota fall-through and load-time validation, so there is no `[advisor]` section or on/off flag; a repository cannot turn it on.
- 2026-11-08 (#44): `advisor` is an `Arbitrary` tool, guardian-judged with no tier exemption (the call egresses the conversation and model-written text to a second provider); an allow rule in user scope is how to lift the prompt. Headless `-p` therefore needs `allow = ["advisor"]` or `auto`.
- 2026-11-08 (#44): `advisor` is root only, stripped from every child toolset beside `draft_bug_report`, so subagents cannot multiply its spend or egress.
- 2026-11-09 (#553): Supersedes "follows Claude Code's pattern" for Git Bash discovery in one respect: a `git.exe` on `PATH` that is not a Git install gets one hop through a sibling scoop `git.shim`, and the `%SCOOP%`/`%SCOOP_GLOBAL%` `apps\git\current` roots are probed, because a scoop install puts only a shim on `PATH`, which Claude Code's pattern does not recognize, so the override would otherwise be needed (Claude Code needs its own override on the same machine, #562).
- 2026-11-10 (#562): Supersedes the 2026-10-30 "only the direct language-server child is killed" line: `procgroup::force_kill` runs `taskkill /T /F` on Windows, so the LSP client and websearch gain a tree kill, as do the status line, command hooks (through a `TreeGuard` drop guard, since the hook timeout stays caller-owned) and the claude CLI provider, which shares the helper. `taskkill` cannot reach descendants of an exited leader, so `TreeGuard` and the guardian's local copy also hold the child in a kill-on-close Job Object that the kill path closes (cleared on a finished run, so intentional daemons survive). `fontaine-guardian` keeps a local copy for its BYO scanner rather than depend on `fontaine-tools`.
- 2026-11-12: The `memory` tool (#329, `tasks/completed/memory_tool_plan.md`) is Fontaine-only: Claude Code saves memory with raw file writes, while Fontaine adds one structured `WorkspaceWrite` tool that writes the topic file and maintains the `MEMORY.md` index line deterministically. It is path-free (`Specifier::None`, no argument specifier; the store path is harness-held session state), `Destructive::Always` (the store is not checkpointed), offered only while auto memory is on, and subagents get it with every call gated on its own. Gating falls out of the class with no new name matches: plan mode blocks it, every fast path is name-gated so it reaches none, and `auto` routes it to the guardian. The out-of-scope containment deny gains a path-free exemption (`&& !names_no_path(tool)`) so the call's locate error denies nothing before the mode dispatch — the first path-free `WorkspaceWrite` row would otherwise Mode-deny it in every mode. No read/list subcommand in v1: reads use `read`/`ls`/`glob` on the store root.
- 2026-11-13 (#329): Amends the 2026-11-12 line's "Gating falls out of the class with no new
  name matches": gating is an explicit `tool == "memory"` protected-path branch in the floor
  dispatch, placed before any allow rule or hook allow (a whole-tool allow rule must not
  approve the store), with `auto`/AFK routed to the guardian from that branch and the asking
  modes keeping the protected prompt; the plan block and the fast-path name gates still hold
  by class. The ledger also records a `memory` call's absent topic file and index as
  created-this-session, so a consecutive save or raw rewrite reads back as
  `created_this_session`, not `unindexed`.
- 2026-11-13 (#588): A picked `ask_user_question` option is recorded as guardian intent with its question and description (`Answered the agent's question "<question>": picked "<label>" ("<description>")`), as Claude Code does, with origin `Dialog`; supersedes the 2026-10-25 line that recorded only typed "Other" text. The prompt credits only the picked option read with its question, the screen never reads these lines, and hard denies still apply. This also amends the "only typed messages" wording of the 2026-11-06 out-of-root write line and the 2026-10-22 `!` command line: dialog answers count as the user's messages too.

### TUI

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
- 2026-10-21: `/permissions` leaves Phase 4 for backlog GitHub issue #47 (core slash commands); users edit the config file, and "always allow" grants still write to the user file; supersedes the Phase 4 ownership of `/permissions`.
- 2026-10-21 (user): `/model` and `/guardian-model` switch the live session and persist to the user config file (Claude Code `/model` parity); `/model` moves the guardian only when it is unset and on the main-model fallback; an unavailable pinned guardian in the TUI prompts retry / pick another validated guardian / switch to `manual` (Phase 4 §The guardian agent).
- 2026-10-22: `/denied` is Fontaine's stand-in for Claude Code's `/permissions` Recently-denied tab until issue #47; reason: Fontaine has no `/permissions` UI yet.
- 2026-10-22: The TUI draws inline (live area plus terminal scrollback) instead of on the alternate screen, matching Claude Code; reason: alternate-screen apps lose mouse selection and wheel scrolling under tmux (#70).
- 2026-10-22: The TUI takes the whole terminal at startup (earlier shell output scrolls into the terminal's history) and keeps the input box and status bar on the bottom rows; Claude Code renders from the cursor row. Reason: a full-screen layout was requested (#74).
- 2026-10-22: `@path` mentions are completion only; the message is sent as typed and the agent reads the file through its gated `read` tool (Claude Code inlines the file's content). Reason: file content reaching the model must pass the scope validator, the deterministic floor and the guardian.
- 2026-10-22: A `!` command's text and output are not guardian intent; the guardian sees only typed messages. Reason: output is data, and treating a command as authorization would let one `!` run widen what the agent may do.
- 2026-10-22: `!` commands run only between turns. Reason: a user command racing the agent's own calls would interleave side effects the guardian is judging.
- 2026-10-22: Queued messages become guardian intent when the model receives them, not when typed. Reason: at queue time the guardian would judge calls using words the model never read.
- 2026-10-22: Nothing is queued while a `!` command runs: Enter shows a notice and keeps the input. Reason: no agent run consumes the queue then.
- 2026-10-23: The `/model` picker groups models by account, filters them with a fuzzy match and lists recently used models first, going beyond Claude Code's flat `/model` picker. Reason: Fontaine's model namespace is `<account>/<model>` across several accounts; the design follows opencode.
- 2026-10-23 (user): Every model change is saved as the default for later sessions: Enter on the picker's current model saves it, and the unavailable-guardian prompt's "pick another" answer saves `[guardian] model` instead of lasting one session. Reason: a model chosen in the TUI should still be the model next time.
- 2026-10-25: `/config` is an interactive overlay, unlike the read-only notices of `/status` and `/permissions`, because the issue's purpose is in-TUI editing; it edits preferences only and never permission rules, which stay in the config files (#4).
- 2026-10-25: `/config` edits only `tui.theme`, `tui.screen_reader`, `context.autocompact_window`, `schedule.enabled` and (via the pickers) `model.model` / `guardian.model`; every other key is shown read-only, because guardian, permission, account, network, telemetry, env, hook and MCP keys loosen or redirect the security pipeline or send data off the machine and should need a deliberate file edit (#4).
- 2026-10-25: `/config` writes only the user file; when a higher scope sets the key it saves anyway and says which scope wins, rather than writing the project or project-local file (Claude Code writes whichever settings file it chooses) (#4).
- 2026-10-25: `tui.screen_reader`, `context.autocompact_window` and `schedule.enabled` changes made in `/config` apply from the next launch (Claude Code applies most `/config` changes at once), because each is read once at startup (renderer choice, compaction threshold, scheduling tool registration); re-plumbing them live is its own change (#4).
- 2026-10-25: `user_config::set_value` and its wrappers validate the edited document against the user column before writing, so no Fontaine-made edit through them can leave the user file unloadable (#4).
- 2026-10-25: The `/config` panel shows only config-layer provenance (`Config::sources`, built by walking each layer's raw TOML); `--model`, the screen-reader flag and `FONTAINE_AX_SCREEN_READER` overrides are not named as sources, because they are per-launch overrides rather than config scopes (#4).
- 2026-10-25: `/config` keeps next-launch saves in a session-scoped map that survives closing the panel, and Del resets only `Choice`/`PositiveInt` rows (not the model rows, since removing `model.model` sends the next launch to the setup screen), because this needs no file re-read or comparison logic (#4).
- 2026-10-26: The approval prompt for `edit` and `write` shows a diff computed from the file at prompt time from rig-tools' `FileChange` hunks (display-only, advisory, capped at 200 rows and 1 MiB, absent when unreadable or the edit would fail); shell and guardian explanations already reach the prompt, and MCP tools get no preview (#45).
- 2026-10-26: No key is rebound for tool cards; Ctrl-O stays the task list, because a transcript view needs a key and an alternate screen. `/expand [n]` re-prints a call instead (#222). Superseded 2026-11-04: Ctrl-O now opens a transcript view without an alternate screen, and the task list moved to Ctrl-T; `/expand` stays.
- 2026-10-26: `/expand n` takes the last entry with ordinal `n`, and an unknown call after `/clear` is an error, because no store survives `/clear` and that is the least new state (#222).
- 2026-10-29: Built-in commands except `/loop`, plus `!` shell commands, typed mid-turn queue and run after the turn, per Claude Code's documented behavior (#292); `/loop` stays refused until #298 so generic replay need not carry self-paced first-iteration completion state. The run-immediately set is Fontaine's synchronous UI/report commands rather than Claude Code's carve-out list, and `/add-dir` queues instead of getting Claude Code's mid-turn confirm. Esc with input queued keeps Fontaine's restore-to-input rule, where Claude Code runs the queued prompt anyway.
- 2026-10-30: In the TUI `/` menu, Tab first extends the typed name to the longest common prefix of
  the matches and only then accepts the selected row; Claude Code accepts the selected row on the
  first Tab. (#294)
- 2026-10-30: Deviation from Claude Code (#321) — skills, `!` lines and conversation-changing commands typed in an agent view are refused with a pointer back to main (Claude Code sends skills to the agent and asks to confirm `/clear`/`/compact`/`/rewind`); reason: no child-side skill path exists, and a refusal cannot act on a conversation the human is not looking at.
- 2026-10-30: Deviation from Claude Code (#321) — Esc never interrupts the main turn while an agent is viewed; reason: the human cannot see what it would stop.
- 2026-10-30: The agent-view mockup in `tasks/subagent_view_mockups/states.html` was approved by the maintainer (#321).
- 2026-10-30: Ended agents leave the list below the status line as in Claude Code (done at once, others after 30 s) and stay reachable from the agents panel, which replaces its flat detail view with Enter on an agent to view it (#321, fixes #299).
- 2026-10-30: Closing the agents panel after the screen committed rows under it reprints the transcript on screen (fixes #296); approvals and the other dialogs keep their behavior, since a reprint erases terminal history and would run after every routine prompt (#321).
- 2026-10-30: The transcript renders a bounded markdown subset for assistant replies (bold, italic, inline code, headings, bullet and numbered lists, fenced code, aligned and wrapped tables) with `pulldown-cmark`; anything else shows as its source lines, `/copy` and `/export` give the stored source, and line mode stays raw because the terminal does its own wrapping there. A streaming reply is committed to scrollback a whole block at a time, so a paragraph shows as source until it ends; a soft break keeps the source line break instead of reflowing (#328).
- 2026-10-31: Ctrl-W deletes back to the previous whitespace while the other word shortcuts use alphanumeric words, matching Claude Code (#257).
- 2026-10-31: The prompt box kill buffer holds a single entry with no Alt-Y paste cycle (Claude Code has one), and consecutive kills replace it rather than join; one entry covers the common case (#257).
- 2026-10-31: No Ctrl-_ undo in the prompt box (Claude Code's undo key); it needs its own history design (#257).
- 2026-10-31: A terminal resize purges the scrollback and rewrites the transcript at the new size, instead of ratatui's inline resize, which erases from the viewport top and which tmux turns into a stale copy of the live area in history (fixes #279); Claude Code's resize behavior was not verified. The refusal of an unknown slash name or `/loop` mid-turn is a transient status-line hint, not a transcript notice, so repeated Enters add no history.
- 2026-10-31: Model reasoning is shown in the transcript by default, folded once settled, and `[tui] show_reasoning = false` hides it; Claude Code keeps thinking out of the main transcript (Ctrl-O transcript view), not re-verified with `claude -p`; Fontaine's own Ctrl-O view (2026-11-04) now shows the full kept reasoning too.
- 2026-11-01: `esc to interrupt` stays in the status bar (minus `working`) while the spinner row shows; moving it into the row would lose it whenever the row is hidden for a waiting prompt (#355).
- 2026-11-03: The built-in bar's running hint `esc to interrupt · shift+tab mode` is removed
  outright; it is not relocated elsewhere in the UI. The idle hint stays as is (#48).
- 2026-11-03: The guardian-off warning badge renders even when a custom status line is active,
  because it reports a security-relevant state the user must not lose by configuring a script (#48).
- 2026-11-03: First stdout line only, ANSI escapes stripped; multi-line and colored output are
  deferred, because the Fontaine bar is one row of styled spans and full ANSI/OSC support needs
  an ANSI-to-span parser dependency (#48).
- 2026-11-03: Added a 300 ms minimum interval between status-line script runs for draw-coalesced
  triggers, tracked in `Ui` status-line state alongside the in-flight flag; startup and
  `refresh_interval_secs` timer triggers bypass it (#48).
- 2026-11-01: No working spinner in screen-reader (linear) mode; it is animation with nothing to announce, and settled-only output is the rule there as for `thinking:` (#355).
- 2026-11-01: The working spinner uses a breathing-dot glyph (`·∙•●•∙`, ASCII `.oO0Oo`) with a drifting hue instead of Claude Code's star glyphs; `⬤` is excluded because terminals draw it wider than the other frames (#355).
- 2026-11-01: The spinner label is the fixed `Working…`, not Claude Code's rotating verb list; the issue asks for an activity indicator, not verb variety (#355).
- 2026-11-01: `!` commands run in the primary root, a deliberate choice with no Claude Code parity
  claim (not observable with `claude -p`) (#26).
- 2026-11-01: The status bar shows the shell's directory and the git branch (or the short commit of a
  detached `HEAD`) after the mode, read from `HEAD` without spawning `git` — a Fontaine addition,
  requested with #26. Claude Code's status line shows them only through a user script.
- 2026-11-01: `/btw` runs on the main model, not the guardian model, because the question is about the main conversation and the user chose that model (#52).
- 2026-11-01: `/btw` waits for the turn to end rather than running mid-turn (Claude Code allows it mid-turn), because the conversation log is being written then; revisit if mid-turn use matters (#52).
- 2026-11-01: `/recap` is a model-free multi-line report from the ledger, not Claude Code's model-written single line of at most 400 characters, because it then costs nothing and works with no reachable model (#52).
- 2026-11-01: The `/btw` answer is a transcript notice, not Claude Code's dismissible overlay, because the TUI has one notice path and the text stays in scrollback (#52).
- 2026-11-01: A bare `/btw` is a usage error, not Claude Code's reopening of the last answer, because answers are not stored (#52).
- 2026-11-01: Side questions are stateless, where Claude Code gives later ones the newest 20 exchanges, because nothing from them is saved and each answers from the conversation alone (#52).
- 2026-11-01: AFK: `/btw` uses fixed constants, a 60 s timeout (`EVALUATOR_TIMEOUT`) and `SIDE_QUESTION_MAX_TOKENS = 4_096`, with a test-only `with_timeout`, not config keys, because the review loop found them unspecified and 1_024 tokens is too tight for a free-text answer (#52).
- 2026-11-02: `/effort` and `--effort` copy Claude Code's surface; `/effort` is session-only and `[model] effort` is the standing choice. The level survives `/model` and is re-validated against the new model: a level the model cannot take is dropped with a one-time warning, not an error; reason: effort is a hint, and a session should not die on a switch to a model with fewer levels.
- 2026-11-02: `/usage` aliases `/cost`. The usage ledger is in-memory, and `/cost` breaks down by source (main, guardian, subagents, goal, compaction, side questions) rather than per model, with no durations or code-change counts, unlike Claude Code's `/cost`; the sources are what a user needs to attribute Fontaine's spend (#56).
- 2026-11-03: Keybindings live in a `[keybindings]` table of the typed TOML config, not Claude Code's separate free-form `keybindings.json`; reason: Fontaine's one config file carries scope columns, fail-closed unknown keys and exit-2 validation, which a second JSON file would bypass (#49).
- 2026-11-03: No hot reload of keybindings: they apply at startup like the rest of the config surface (`/config` saves are next-launch too); reason: consistency, and no file watcher exists to reuse (#49).
- 2026-11-03: No chord support in the keybinding parser: Fontaine has no chord bindings to remap, and a chord engine (pending-prefix state, timeout, prefix reservation) is pure addition; the parser rejects space-separated sequences with a clear error (#49).
- 2026-11-03: No per-overlay keybinding contexts (Claude Code's 25+): the remappable set is the global/chat action level, and vim keys are not remappable either (same as Claude Code); reason: remapping every overlay's internal keys is disproportionate for a prompt harness (#49).
- 2026-11-03: URLs and markdown link text are coloured with a theme `link` token and underlined, without OSC 8 hyperlinks (Claude Code 2.1.318 emits OSC 8 around the visible text); links are coloured whether or not the terminal supports hyperlinks, and the underline means they do not rely on colour alone (Claude Code: blue on light / blueBright on dark, no underline) (#206).
- 2026-11-03: Bare URLs are found by a hand-written scanner, not a regex, because pulldown-cmark does not autolink them and it keeps `fontaine-tui` free of a `regex` dependency; a URL wrapped across rows in the unstable streaming tail, tool output or a raw-source fallback block loses colour on its continuation rows (#206).
- 2026-11-03: The link colour is not user-overridable: `ThemeName` is a closed set of six presets and custom themes are a separate issue (#206).
- 2026-11-04: Ctrl-O (`transcript`, `/transcript`) toggles a transcript view as in Claude Code: full tool calls, complete kept output, full reasoning and the full reason behind each denial, scrollable in the live area from the in-memory entries with no alternate screen (which would hide scrollback and break native selection); a view tall enough to push the transcript into scrollback writes the transcript again on close, and that rewrite erases the scrollback first, as Ctrl-L does. The task list moves to Ctrl-T (`todos`, `/todos` kept) as in Claude Code, and the agents panel has no default key and is reached with `/agents` (`agents_panel` can still be bound); owner decision on #233. Differences from Claude Code, not observed against its TUI (`claude -p` cannot show it): the input box stays on screen dimmed with no cursor and pastes are ignored while the view is open; Up/Down/PgUp/PgDn/Home/End scroll and the `transcript` key, Esc, `q` and Ctrl-C close it, where Claude Code's close keys are `q`, Ctrl+C and Esc; an approval or question takes the view's slot and the view returns under it. `ToolStatus::Denied` now keeps the whole explanation, collapsed cards still show its first line. Screen-reader mode opens no overlay: the key prints each settled entry once (#233).
- 2026-11-04: (#51) No `Notification` hook event (Claude Code has one), because a hook is a separate feature touching the hook schema, trust rules and `/hooks`. Superseded by the 2026-11-08 `notification` line under Extensibility (#29).
- 2026-11-04: (#51) The key is `[tui] notify` with `bell`/`osc9`/`osc777`/`off`, not Claude Code's `preferredNotifChannel`, because the key sits in the typed `[tui]` section and the OSC values name the escape sequence rather than a terminal.
- 2026-11-04: (#51) No idle notification when a reply finishes, because the issue asks for pending human prompts only.
- 2026-11-04: (#51) The signal fires as the prompt opens, not after Claude Code's ~6 s without typing, because the bytes ride the frame that opens the prompt and a delay would need a timer in the render loop.
- 2026-11-04: (#51) The default is `bell`, not Claude Code's terminal-detecting `auto`, because detection needs terminal queries that time out on terminals that do not answer and a bell works everywhere.
- 2026-11-04: (#51) `osc9` and `osc777` are generic, not Claude Code's per-terminal `iterm2`/`kitty`/`ghostty` channels, because those terminals read the same sequences and one value per sequence keeps the schema small.
- 2026-11-04: (#51) No combined bell+OSC value (Claude Code has `iterm2_with_bell`); add on request.
- 2026-11-08: (#514) On Windows the terminal reader treats a burst of key events (gap under 15 ms) with at least two keys and a newline as one paste, because crossterm's Windows reader never emits a bracketed paste and each pasted line would submit; Claude Code gets bracketed paste from its input layer. A dropped file path (chars only, no newline) is replayed as keys, so PDF-by-path attachment is not detected on Windows unless it arrives bracketed.
- 2026-11-08 (supersedes the 2026-10-24 "`/mcp` takes no `reconnect`/`enable`/`disable` arguments until #35." line): (#35) `/mcp reconnect <name|all>` is offered. `enable` and `disable` are still not offered.
- 2026-11-08: (#35) The 2026-10-24 line "`/help`, `/status`, `/permissions`, `/mcp`, `/hooks` and `/skills` are read-only transcript notices ..." no longer covers `/mcp reconnect`, which acts on servers; a bare `/mcp` stays a read-only notice.
- 2026-11-11: (#115) The usage-pause controls (`r` resume, `s` switch account, `c` cancel) are a modal prompt modelled on the guardian-unavailable prompt, not a key row beside the message box, so they never collide with the always-live input. AFK shows only the status line: the pause there has the same cap a headless run has.
- 2026-11-11 (supersedes the "without OSC 8 hyperlinks" part of the 2026-11-03 link-colour line): (#83) Transcript links are OSC 8 hyperlinks (explicit URLs, markdown link targets, `owner/repo#N`, bare `#N` against a `github.com` origin, existing `path[:line]` files as `file://` URLs without the line), emitted per cell with a shared `id=`.
- 2026-11-11: (#83) Bare `#N` and `path:line` are linked, where Claude Code 2.1.322 links neither (it links only URLs, markdown links and `owner/repo#N`); reason: the issue asks for them, and an unresolvable one stays plain text.
- 2026-11-11: (#83) `GH-N` and scheme-less `github.com/...` forms are not linked, as in Claude Code; reason: `GH-N` collides with ticket keys, and neither is unambiguous without a scheme.
- 2026-11-11: (#83) `#N` resolves only against an `origin` on `github.com`; other remotes and GitHub Enterprise hosts leave it plain.
- 2026-11-11: (#83) Issue references are linked only in assistant prose, not in tool output, code or notices; reason: `#N` in logs and code is rarely an issue.
- 2026-11-11: (#83) URLs in notices are linkified (coloured and linked); errors stay plain.
- 2026-11-11: (#83) Hyperlinks are auto-detected from the environment (`FORCE_HYPERLINK`, `TMUX`, `TERM`, Windows Terminal) with no config key; screen-reader mode and headless output never emit OSC 8; reason: `FORCE_HYPERLINK` already overrides detection by convention.
- 2026-11-11: AFK: (#83) The OSC 8 `id=` is the ref's process-global interned id, so all occurrences of one target hover-highlight together, where Claude Code emits a random `id=` per run; reason: every occurrence opens the same target, and a per-occurrence id needs an occurrence key stable across re-rendered frames that the context-free tag cannot carry without unbounded interning.
- 2026-11-11: AFK: (#83) `file://` link targets carry the local hostname (`libc::gethostname` on unix, empty host on Windows or on failure), as the OSC 8 spec asks; reason: a hostless `file:///` opens a same-named local file, or nothing, when Fontaine runs over SSH.
- 2026-11-11: AFK: (#83) Only non-executable regular files get `file://` links (no unix execute bit, extension not in a fixed launcher list, on Windows not in `PATHEXT`); executable or launchable files stay plain text; reason: a click passes the target to the OS opener, which runs such files, and a prompt-injected reply can name one in the repo.
- 2026-11-11: AFK: (#83) Notices are linkified through a `link: Option<Style>` parameter on `prefixed_body` that only the info-notice branch and the warning headline set, and through `link_spans` in `hint_spans`'s `flush` (`/command` runs unlinked); `prefixed` passes `None`, so errors and user input stay plain; reason: it keeps one wrap path and cannot linkify the plain call sites by accident.
- 2026-11-11: (#83) (supersedes the continuation-row clause of the 2026-11-03 (#206) bare-URL line) A link wrapped across rows keeps its colour and target on every row, because text is scanned before it is wrapped; a reference still streaming at the end of the unsettled tail stays plain until text follows it.
- 2026-11-11: (#83) Rows of an unsettled reply committed to scrollback because the reply outgrew the screen link only URLs, not #N or paths; reason: the unsettled tail can be inside an open code fence, where #N must not link, and the tail is never re-rendered once committed.
- 2026-11-11: (#83) File links are limited to paths inside the primary root: UNC, device and drive-prefixed paths, `..` components and paths outside the root are never touched on disk or linked, and paths are not resolved at all when hyperlinks are off; reason: resolving a path from model or tool text must not reach the network (a UNC path sends NTLM credentials) or stall the render thread.
- 2026-11-12: (#592) `/exit`, `/quit` and empty-prompt Ctrl-D quit at once even mid-turn: the quit latches, cancels the session's work, drops queued input and automatic continuations, and the whole exit shares a fixed two-second budget (not configurable), after which a watchdog restores the terminal and ends the process. Supersedes the 2026-10-29 statement that every built-in except `/loop` queues mid-turn, for these terminal commands only; Ctrl-C and Esc still interrupt, Ctrl-D with text still edits, and in an MCP elicitation it still declines.

## Open Questions

None.
