# Subagent shell allow rules: explain compound-command refusals Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/697

## Overview
A subagent type holding `shell(...)` allow entries (built-in `reviewer`, or user-defined) refuses any
pipe, redirect, chain or substitution. Models habitually append `2>&1 | tail -60`, get denied, and the
message lists the allowed patterns without saying why the command did not match. This plan makes the
denial say so and tells the `reviewer` to run commands bare. The narrow suffix-stripping matcher
(issue proposal 3) is a follow-up and out of scope here, because it loosens Claude Code's behavior and
needs its own golden adversarial set and Decisions line.

## Current State
- `restriction_on` in `rust/fontaine-sdk/src/core/gate.rs` (~line 1826) builds the child-type denial:
  `this agent type may use `shell` only as `shell(git diff *)`, ...`. It does not distinguish "no
  pattern matches" from "a pattern matches the leading words but the command is compound".
- Shell allow rules never match a compound command (`shell_allow_never_matches_a_compound_command` in
  `rust/fontaine-sdk/tests/permission_rules.rs`); one leading `cd <dir> &&` is tolerated.
- `REVIEWER_PROMPT` in `rust/fontaine-sdk/src/agents/registry.rs` (~line 268) says not to work around a
  refused command but never mentions pipes or redirects.
- `docs/agents.md` (reviewer section, ~line 107) says chains, pipes and redirections are refused.

## Design
1. **Denial message.** In `restriction_on`, when the call is `shell` and no limit matched, decide
   whether to hint: the analysis parsed and ANY entry of `analysis.commands` (each rebuilt as command
   text by joining its `raw` words with spaces) matches a limit rule via
   `matches(rule, call, Posture::Allow)` on a constructed `shell` call. `matches` re-parses the command
   and, for Allow, accepts only `analysis.simple`/`after_cd`, which need every word literal. If so, append a hint to the existing message: "the command is a chain, pipe,
   redirect or substitution, which is refused; run it as one plain command". The hint only changes
   wording, never the Deny, and does no regex on arguments: reuse the shell pre-classifier's parse
   result (`preclassify.rs`). This covers `cd <dir> && cargo clippy 2>&1 | tail` (`analysis.commands` already holds the post-cd
   command) and a substitution wrapping a whole literal command (`$(cargo clippy)`). A matching command
   with a non-literal word (`cargo clippy $(foo)`) gets no hint: the rebuilt text never matches, so the
   original message stays. If the analysis did not parse (`commands` empty) or the command name is
   expanded (`$X cargo clippy`), keep the current message.
2. **`REVIEWER_PROMPT`.** Add: run each command bare, with no pipes, redirects or `2>&1`, and do not
   trim output with `tail`/`head`.
3. Docs: `docs/agents.md` reviewer section notes the denial now names the cause and that the prompt
   steers the reviewer to bare commands.

## Implementation Steps
1. `gate.rs`: add a helper that reports whether any simple command of a parsed shell call matches a
   limit rule, and use it in `restriction_on` to choose the message suffix.
2. `registry.rs`: extend `REVIEWER_PROMPT`.
3. Unit tests (below). 
4. `docs/agents.md` update; `CHANGELOG.md` entry.

## Files to Modify
- `rust/fontaine-sdk/src/core/gate.rs`
- `rust/fontaine-sdk/src/agents/registry.rs`
- `rust/fontaine-sdk/src/tools/preclassify.rs` (only if the analysis accessor needs exposing)
- `docs/agents.md`, `CHANGELOG.md`

## Trade-offs
- Stripping harmless suffixes (proposal 3) would remove the failure outright but loosens Claude Code's
  matching and risks the pre-classifier soundness property; deferred to a follow-up issue.
- A hint computed from a parse could in principle disagree with the matcher; it only affects wording,
  so a wrong hint cannot change a decision.

## Decisions
- 2026-11-17: Proposals 1 and 2 only; the suffix matcher stays a follow-up, so no deviation from Claude
  Code's matching is introduced here.

## Documentation
`docs/agents.md` (reviewer section); `CHANGELOG.md`.

## Testing Strategy
Unit tests in `gate.rs` for `restriction_on`:
- `cargo clippy --all-targets -- -D warnings 2>&1 | tail -60` against `shell(cargo clippy *)` yields the
  compound hint and still denies (pins the reported bug).
- Pipe, `&&` chain, redirect, `$(...)` variants each carry the hint.
- A bare non-matching command (`rm x`) and an unparseable command keep the original message without the hint.
- A matching bare command is allowed (no restriction).
- `cd <dir> && cargo clippy 2>&1 | tail` against `shell(cargo clippy *)` carries the hint.
- `$(cargo clippy)` and `echo $(cargo clippy)` carry the hint.
- `cargo clippy $(foo)` gets no hint (non-literal word; original message).
- `$X cargo clippy` and an unparseable command get no hint.
- Compound command none of whose simple commands match any limit gets no hint.
- A test on `REVIEWER_PROMPT` asserting it mentions pipes/redirects lives in `registry.rs`.
The `restriction_on` tests are `gate.rs` unit tests and build `ChildRestrictions`/`CallContext` directly.
No live APIs.

## Open Questions
- Should the hint also list the first matched pattern? Default: no, keep the message short.
