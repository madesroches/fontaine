# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

The master tracking plan is the source of truth: `tasks/fontaine_harness_plan.md` — it holds the
cross-cutting design (workspace layout, permission-pipeline data flow, config schema), the Phase
Tracker, and the `## Decisions` section, which records every deliberate deviation from Claude
Code; `research/` holds the six companion analysis docs the plan is grounded in. The build is
split into one plan per phase under `tasks/phases/` (`phase0_scaffold.md` … `phase6_hardening.md`),
each with its own design excerpt, steps (ending in a docs step), files, tests, and definition of
done. Read the master sections you touch **and** the phase plan you are implementing before
changing design.

## Critical Rules

- **NEVER push without a direct, unambiguous instruction** — this includes `git push`,
  `git push --force`, and creating PRs. Local commits are fine; publishing them is not.
- **NEVER commit directly to `main`** — always work on a branch.
- **Commit messages**: never include AI-generated credits or co-author tags.
- **Unless asked, don't amend commits.**
- **When in doubt, do as Claude Code does.** Fontaine copies Claude Code's documented
  semantics at analogous design points. Any deviation must be recorded as a dated line in the
  plan's `## Decisions` section with a one-line reason — never deviate silently.

## Security invariants (do not weaken)

These are load-bearing properties of the product; breaking one is a correctness bug even if
all tests pass:

- **The guardian is fail-closed on every non-verdict outcome**: error, timeout, unparseable
  output, low confidence on a destructive class → Deny.
- **No config value disables the guardian.** No zero-guardian mode ships; containers get
  `[guardian] profile = "minimal"`, never off.
- **The deterministic floor (`deny → ask → critical-path`) gates EVERY mode** — no mode,
  flag, or verdict reaches past it. No regex matching on tool arguments anywhere.
- **The fast path is earned by Fontaine-enforced workspace containment, not by a label.**
  Read-only ≠ safe; MCP-declared `readOnlyHint` never skips the guardian.
- **A child subagent's effective mode is never looser than its parent's.**
- **Project-scope config can only tighten** (deny/ask additions); loosening keys are
  user-scope only.
- **Every registered tool must have a risk class** — keep the choke-point test green.

## Project layout

- Repo root holds only `rust/`, `evals/`, `docs/`, `tasks/`, `research/`.
- The Cargo workspace lives at `rust/Cargo.toml` (micromegas convention) and **every crate
  sits directly under `rust/`**: `fontaine` (bin), `fontaine-core`, `fontaine-tools`,
  `fontaine-guardian`, `fontaine-agents`, `fontaine-mcp`, `fontaine-tui`.
- The core is built on the **rig** crate family (`rig-core` / `rig-agent` / `rig-memory` /
  `rig-tools` / `rig-rmcp`, `rig-cassette` for dev tests), **pinned to exact versions**.
  No custom provider layer; no custom agent loop or history management. Do not adopt rig's
  Bevy-based `rig-harness` / `rig-tui`.
- Keep `fontaine-guardian` dependency-light and lift-out-able (embeddable by other harnesses).
- `docs/` is the standalone, website-ready home for all product documentation (one topic per
  page; `README.md` stays a thin front door). Documentation is a per-phase deliverable — every
  phase plan ends with a docs step; ship doc updates with the feature they describe.
- The plan is greenfield: check the Phase Tracker in the master plan and the owning phase plan
  (phases 0–6) before starting work, and don't jump ahead of the phase currently being built.

## Code Style

- Use Unix line endings (LF) in all files.
- **Comments are for *why*, not *what*.** Names and code should make behavior clear; only
  comment non-obvious constraints, deliberate tradeoffs, and gotchas. Default to no comment.
- **Don't cite plan section numbers, phase labels, or analysis-doc names in code comments or
  in `docs/`.** Docs and comments describe current behavior, not the history of how it got
  there. Such citations belong in `CHANGELOG.md` and the plan itself.

## Testing

- **Unit and integration tests never touch live APIs.** Use `rig_core::test_utils`
  deterministic completion models, `wiremock` for provider endpoints, and `rig-cassette`
  record/replay for full agent runs in CI.
- The eval harness in `evals/` runs manually / nightly against live guardian models — it is
  **not** a CI gate.
- Integration tests drive the core through the headless `fontaine -p` surface.
- New shell-pre-classifier behavior must keep the soundness property: anything it cannot
  fully parse as one simple command falls through to the guardian (see the plan's Testing
  Strategy golden adversarial set).

## Essential Commands

From `rust/`:

- `cargo fmt` / `cargo clippy --all-targets -- -D warnings` / `cargo test` — CI runs all
  three.
- `cargo run` — interactive TUI (`fontaine`).
- `cargo run -- -p "task"` — headless non-interactive run (CI/eval driver).
- `cargo run -p <crate> --example ...` — per-crate spikes.

## Other

- License is Apache-2.0. Rule prose adapted from Anthropic's published documentation gets an
  attribution note in `docs/guardian.md`; same for MIT-licensed LlamaFirewall prompts if
  mined. Watch the Llama 4 Community License gated-download caveat around PromptGuard 2 (ONNX
  tool-result scanner) — surface it at first enable and keep the BYO-scanner interface.
