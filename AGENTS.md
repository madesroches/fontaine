# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

The master tracking plan is the source of truth: `tasks/fontaine_harness_plan.md` — it holds the
Phase Tracker, workspace layout, and the `## Decisions` section, which records every deliberate
deviation from Claude Code. The cross-cutting design (permission pipeline, config, sessions,
observability, platforms, testing strategy) lives in topic docs under `tasks/design/`, indexed
from the master's `## Design docs`; `research/` holds the companion analysis docs the plan is grounded in, including
`research-claude-code-parity.md` (Claude Code feature matrix). Backlog features not in the plan
are GitHub issues (`gh issue list`); a `phase-N` label means "can be taken after phase N", not
part of that phase's definition of done, and the plan's Decisions win over issue text. All phases (0–6)
are done; their plans (`phase0_scaffold.md` … `phase6_hardening.md`) are archived under
`tasks/completed/phases/` as the record of each phase's design excerpt, steps, tests and
definition of done. Remaining work is tracked as GitHub issues; the ones that gate the first
release carry the `release-blocker` label. New work gets its own plan in `tasks/`, moved to
`tasks/completed/` when it ships. Read the master and design-doc sections you touch before
changing design.

## Critical Rules

- **Pushing a work branch and opening a PR need no instruction** — this project is AI-first, and
  the maintainer's review of the PR is the gate. Never push to `main`, never push tags, and never
  `git push --force` without a direct, unambiguous instruction.
- **NEVER commit directly to `main`** — always work on a branch.
- **Commit messages**: never include AI-generated credits or co-author tags.
- **Unless asked, don't amend commits.**
- **When in doubt, do as Claude Code does.** Fontaine copies Claude Code's documented
  semantics at analogous design points. Any deviation must be recorded as a dated line in the
  plan's `## Decisions` section with a one-line reason — never deviate silently.
  When unsure what Claude Code does, run `claude -p` to observe it directly rather than
  guessing.

## Security invariants (do not weaken)

These are load-bearing properties of the product; breaking one is a correctness bug even if
all tests pass:

- **The guardian is fail-closed on every non-verdict outcome**: error, timeout, unparseable
  output, low confidence on a destructive class → Deny.
- **No config value disables the guardian.** No zero-guardian mode ships; containers get
  `[guardian] profile = "minimal"`, never off. The one exception is the interactive-only,
  session-scoped `/guardian-disable` command (see the floor invariant below): it is not config,
  not a mode, never persisted, never settable from project scope, and never inherited by a
  subagent.
- **The deterministic floor (`deny → ask → critical-path → protected-path`) gates EVERY mode** — no mode,
  flag, or verdict reaches past it. No regex matching on tool arguments anywhere. The one
  deliberate exception is `/guardian-disable`: after an explicit confirmation in the terminal UI
  it turns the guardian and the floor off for the current session only, until `/guardian-enable`,
  as a development escape hatch; it is blocked in headless runs, is not inherited by subagents,
  and every toggle is recorded in the ledger and the audit log. The record of this deviation
  lives in the master plan's Decisions.
- **The fast path is earned by Fontaine-enforced workspace containment, not by a label.**
  Read-only ≠ safe; MCP-declared `readOnlyHint` never skips the guardian. There are two
  exceptions. The opt-in, user-scope `[guardian.screen] allow_threshold`: a verdict of the composed
  guardian (a steering-hardened local screen), never before the floor. And in `auto`, after the
  floor and hooks, file-tool edits to git-tracked, new or session-created files in the primary
  root (`tracked_edit`) and file-tool writes into the session scratch directory (`scratch_write`);
  instruction files and `.github/` are excluded, and project scope can only switch them off. See
  the master plan's Decisions.
- **A child subagent's effective mode is never looser than its parent's.**
- **Project-scope config can only tighten** (deny/ask additions); loosening keys are
  user-scope only. Project-defined executables (MCP servers, hooks, skill commands) load as in
  Claude Code; they can tighten a permission decision but never loosen one.
- **Every registered tool must have a risk class** — keep the choke-point test green.

## Project layout

- Root directories are only `rust/`, `evals/`, `analytics/`, `docs/`, `tasks/`, `research/` (plus `.github/`);
  root files are `README.md`, `LICENSE`, `CHANGELOG.md`, `CONTRIBUTING.md`, `AGENTS.md`,
  `SECURITY.md`, `.gitignore`, `.ignore`.
- The Cargo workspace lives at `rust/Cargo.toml` (micromegas convention) and **every crate
  sits directly under `rust/`**: `fontaine` (bin), `fontaine-core`, `fontaine-tools`,
  `fontaine-guardian`, `fontaine-agents`, `fontaine-mcp`, `fontaine-tui`, `fontaine-evals`.
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

## Error handling

- Typed error enums only where a caller dispatches on the kind: exit codes, retry, fail-closed
  classification. Everywhere else, carry context as a chain (a `source()`), not a flattened
  `String`.
- Keep the cause as `source()`; never `format!("{e}")` an error into another error's message.
  Render the full chain once, where the error reaches a human, with
  `fontaine_tools::error_chain::chain`. The exception is wrapping a third-party error whose own
  type drops `source()` (rig's `ProviderError::Http`): its chain may go into Display, as
  `CausedClient` does.
- `expect("why this cannot fail")` instead of `unwrap()` outside tests. An invariant panics
  loudly rather than falling back silently.
- Don't swallow errors. A failure the user would care about (audit, ledger, config, credentials)
  reaches them through `Approver::warn` at least once; core reports warnings through the
  `Approver` (whose default implementation writes to stderr) rather than writing to stderr directly.
  `let _ =` is only for best-effort cleanup and sends to closed channels.
- Lock poisoning: a lock whose contents feed a permission or guardian decision, a spend tally, the
  model in use or a durable record panics on poison (`fontaine_tools::lock::{lock, read, write}`);
  only display or advisory state recovers (`lock_recovering` and kin), with a one-line reason at
  the call site. Don't write `unwrap_or_else(|e| e.into_inner())` anywhere else.
- Error text may explain a Deny, never change it.

## Testing

- **Unit and integration tests never touch live APIs.** Use `rig_core::test_utils`
  deterministic completion models, `wiremock` for provider endpoints, and `rig-cassette`
  record/replay for full agent runs in CI.
- The eval harness in `evals/` runs manually / nightly against live guardian models — it is
  **not** a CI gate.
- `claude -p` may be run manually or from `evals/` to capture Claude Code baselines and
  compare Fontaine's behavior as features land — never from unit/integration tests or CI.
- Integration tests drive the core through the headless `fontaine -p` surface.
- Terminal layout bugs (live area, scrollback) must be reproduced in a real tmux, not only in
  `TestBackend`: see `tasks/design/testing.md`, "Reproducing terminal layout bugs".
- New shell-pre-classifier behavior must keep the soundness property: anything it cannot
  fully parse as one simple command falls through to the guardian (see the golden adversarial set
  in `tasks/design/testing.md`).

## Essential Commands

From `rust/`:

- `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test` /
  `cargo deny check` /
  `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture` and the same with
  `--profile minimal` — the core CI steps; the two `fixture` evals and the pg2 clippy/test run on
  Linux only (macOS and Windows run `cargo check --features fontaine/pg2`), everything else on all
  three OSes. Later phases add steps (`--guardian none` eval run, `cargo dist plan`); the master
  plan's workspace layout lists the full `ci.yml` step set.
- `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings` — local compile check
  of the Windows code (the toolchain file installs the target).
- `RIG_PROVIDER_TEST_MODE=record cargo test -p fontaine --test cassettes` —
  re-record cassettes (needs live provider keys; a human step).
- `cargo run -p fontaine` — interactive TUI (`fontaine`); two bin crates, so `-p` is required.
- `cargo run -p fontaine -- -p "task"` — headless non-interactive run (CI/eval driver).
- `cargo run -p <crate> --example ...` — per-crate spikes.

## Other

- Search with `rg`, not `grep -r`: it skips gitignored paths and reads `.ignore` (which excludes
  `target/`), so build output never pollutes results.
- License is Apache-2.0. Rule prose adapted from Anthropic's published documentation gets an
  attribution note in `docs/guardian.md`; same for MIT-licensed LlamaFirewall prompts if
  mined. Watch the Llama 4 Community License gated-download caveat around PromptGuard 2
  (candle/safetensors tool-result scanner, `[guardian] scanner = "pg2"`) — surface it at first
  enable and keep the BYO-scanner interface.
