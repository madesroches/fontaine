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
   whether the command is *compound-refused*: it does not parse as one simple command (pipe, list,
   redirect, background, substitution, subshell) but a limit rule matches when evaluated against its
   first simple command. If so, append a hint to the existing message: "the command is a chain, pipe,
   redirect or substitution, which is refused; run it as one plain command". The decision only changes
   text, never the Deny, and does no regex on arguments: reuse the shell pre-classifier's parse result
   (`preclassify.rs`) and the existing rule matcher (`matches(rule, call, Posture::Allow)`) on a
   constructed call holding the first simple command. If the parse yields no single leading command
   (unparseable), keep the current message.
2. **`REVIEWER_PROMPT`.** Add: run each command bare, with no pipes, redirects or `2>&1`, and do not
   trim output with `tail`/`head`.
3. Docs: `docs/agents.md` reviewer section notes the denial now names the cause and that the prompt
   steers the reviewer to bare commands.

## Implementation Steps
1. `gate.rs`: add a helper returning the first simple command of a shell call when the call is
   compound, and use it in `restriction_on` to choose the message suffix.
2. `registry.rs`: extend `REVIEWER_PROMPT`.
3. Unit tests (below). 
4. `docs/agents.md` update; `CHANGELOG.md` entry.

## Files to Modify
- `rust/fontaine-sdk/src/core/gate.rs`
- `rust/fontaine-sdk/src/agents/registry.rs`
- `rust/fontaine-sdk/src/tools/preclassify.rs` (only if the first-simple-command accessor needs exposing)
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
- Compound command whose first simple command does not match any limit gets no hint.
- A test on `REVIEWER_PROMPT` asserting it mentions pipes/redirects.
No live APIs.

## Open Questions
- Should the hint also list the first matched pattern? Default: no, keep the message short.
