# Explore Subagents on the Session Model Plan

## Overview

The built-in `explore` agent type defaults to `model: $guardian_default`, which resolves to the
top-ranked validated guardian model any configured account offers — currently
`opencode-go/claude-haiku-5-5`. A session whose main model is on another account (e.g. `kimi/k3`)
silently routes every `explore` subagent to the Go subscription, and when that subscription hits
its weekly quota all explore agents die with a raw 429 even though the session model is healthy
(madesroches/fontaine#286). The fix is to make the built-in `explore` type run on the session
model, like `worker` already does. `$guardian_default` stays available as an explicit opt-in in
`[agents.types]`, agent files and the `agent` tool's `model` argument.

## Current State

- `rust/fontaine-agents/src/registry.rs:169` — `AgentTypes::builtin()` gives `explore`
  `model: Some(ModelSpec::GuardianDefault)`; `worker` has `Some(ModelSpec::Main)`.
- `rust/fontaine-agents/src/spawn.rs` (~line 195) — model precedence: the call's `model`, else
  the type's, else `ModelSpec::Main`. Changing the type's default is the whole behavior change.
- `rust/fontaine-core/src/child_runner.rs:181-185` — `ChildContext::model_for` resolves
  `GuardianDefault` through `Switcher::guardian_default`
  (`rust/fontaine-core/src/models.rs:738`), which picks by credential presence
  (`account_has`, `models.rs`), not reachability or quota. A 429 at runtime is retried twice by
  `drive()` (`rust/fontaine-core/src/turn.rs`, `MAX_RETRIES = 2`) and then settles the child as
  `Failed`.
- `rust/fontaine-agents/tests/seam.rs:349` — the precedence test asserts explore resolves to
  `ModelSpec::GuardianDefault`.
- Docs: `docs/agents.md:38` lists `explore | $guardian_default` in the built-in types table;
  `docs/configuration.md:702-703` shows `[agents.types.explore] model = "$guardian_default"` as
  the example.

## Design

One-line behavior change plus its test and docs:

- In `AgentTypes::builtin()`, set `explore`'s `model` to `Some(ModelSpec::Main)` (the `worker`
  precedent; `None` would resolve identically at spawn, but the explicit value documents the
  choice and matches the sibling entry).
- Keep `ModelSpec::GuardianDefault`, `Switcher::guardian_default` and the
  `ChildContext::model_for` arm unchanged: `$guardian_default` remains a valid, documented value
  a user can set explicitly, and the guardian's own default-model selection is untouched.
- No runtime fallback machinery, no account-health tracking: with explore on the session model,
  the cross-account quota coupling the issue reports no longer exists by default.

This also matches Claude Code, whose built-in Explore agent inherits the session model.

## Implementation Steps

1. `rust/fontaine-agents/src/registry.rs` — change the `explore` entry's `model` to
   `Some(ModelSpec::Main)`.
2. `rust/fontaine-agents/tests/seam.rs` — in
   `the_models_precedence_is_the_call_then_the_type_then_the_session`, expect `ModelSpec::Main`
   for the no-`model` explore spawn (line 349).
3. `docs/agents.md` — built-in types table: `explore | $main | …`; keep the
   `$guardian_default` explanation (still a valid explicit value) and add a half-sentence that it
   is opt-in only.
4. `docs/configuration.md` — keep the `[agents.types.explore]` example and add one clause
   saying it opts `explore` back into `$guardian_default`: with the default now `$main`, the
   example reads as an explicit override, which is exactly how a user gets the old behavior.
5. `CHANGELOG.md` — entry under the unreleased section: built-in `explore` subagents now run on
   the session model instead of `$guardian_default` (#286).

## Files to Modify

- `rust/fontaine-agents/src/registry.rs`
- `rust/fontaine-agents/tests/seam.rs`
- `docs/agents.md`
- `docs/configuration.md`
- `CHANGELOG.md`

## Trade-offs

- **Runtime fallback** (on a retryable failure of a `$guardian_default`-resolved child, rebuild
  the agent on the session model and continue — the `RecoveryHook` already saves the interrupted
  turn, so continuation would work): preserves the cheap-model default but adds a
  failure-classification channel from `drive()` to the child runner and a rebuild path in
  `spawn`, all to protect a default whose only benefit is cost. Rejected for this issue; the
  general idea of falling through to another model when an account's budget runs out is tracked
  separately as a main-model preference list (#289).
- **Session-scoped account health** (`account_has` treats a recently-failed account as
  unavailable): still routes explore traffic at the Go account until it fails once per session,
  and credential-presence resolution keeps its known-blind spot. Rejected.
- **Session model for explore (chosen)**: no new machinery, no cross-account coupling, matches
  Claude Code. The cost is that explore tokens bill to the main model's account — acceptable:
  users who want the cheap route can still set `model = "$guardian_default"` explicitly, now
  with eyes open.

## Decisions

- 2026-10-28: The maintainer's call — the built-in `explore` type runs on the session model, not
  the guardian model; no runtime-fallback or account-health machinery is built for
  `$guardian_default` children (#286).

## Documentation

- `docs/agents.md` — built-in types table row and the `$guardian_default` paragraph.
- `docs/configuration.md` — `[agents.types]` example.

## Testing Strategy

- Update the existing seam test
  (`the_models_precedence_is_the_call_then_the_type_then_the_session`) to assert the new default;
  the same test still covers the per-call override and explicit `$main`, so precedence is pinned
  end to end through `spawn`.
- Add a registry unit assertion that the built-in `explore` type carries `ModelSpec::Main` (the
  seam test sees resolution, not the registry entry; a one-line table edit is exactly the kind of
  change a direct unit test pins).
- `cargo test -p fontaine-agents` plus the standard gates (`cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`). No live-API coverage: the change is a static
  default value, and no unit-testable behavior remains that a live run would catch.
