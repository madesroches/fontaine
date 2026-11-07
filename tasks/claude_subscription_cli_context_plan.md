# claude-subscription: CLI-injected context Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/480

## Overview

With `--system-prompt-file` the Claude CLI still adds its own context to every request: an
identity line, an `# Environment` block, a `<system-reminder>` carrying the account email
(`userContext`) and a "Today's date" line. Priority order for this work: **token / plan efficiency
first (parity with a native Claude Code session), everything else (privacy, fidelity) second.**
The efficiency cost is that the reminder and date sit only on the message that asks for a
response, so the next request's replayed copy of that message differs and the cache entry written
at the last-message breakpoint is never read: each request writes about two messages where a
native session writes one. The goal is to make that injection stable across requests, or remove
it, and to measure the result against cache-token usage.

## Current State

- `fixed_args` (`rust/fontaine-core/src/providers/claude_subscription/cli.rs`) passes
  `--system-prompt-file`, `--setting-sources ""`, `--no-session-persistence`, and a fixed env
  (`FIXED_ENV`, same file) that already sets `CLAUDE_CODE_ENTRYPOINT=fontaine`,
  `CLAUDE_CODE_DISABLE_CLAUDE_MDS` and others. `CLAUDE_CONFIG_DIR` is passed through.
- `Transport::prepare` (`transport.rs`) replays every history user frame and waits for a zero-turn
  ack; only the final frame asks for a response. `protocol.rs` (`CACHE_MARKER`, ~line 262) puts the
  cache marker on the last replayed history user message, one before the injected one.
- `docs/claude-subscription.md` ("Supported CLI versions") states the two-message write cost
  and the scope statement that Fontaine never presents itself as Claude Code.

## Findings (CLI 2.1.319, measured here)

- Reproduced the issue: with a replaced system prompt the model reports the Agent SDK identity
  line, the `# Environment` block (cwd, git, platform, shell, OS) and, on the first API-bearing
  user message, the `userEmail` reminder.
- In a plain multi-message stream-json conversation where every user turn runs, the reminder
  stays attached to the **first** user message and later messages carry nothing; the second
  request read the first from the cache (665 read / 184 written). The instability therefore comes
  from the adapter's zero-turn acked history: the reminder is attached only to the first message
  that actually reaches the API, which is always the final one.
- Binary strings (2.1.319): `userEmail` is read from the logged-in account's `emailAddress`
  (skipped only when `ANTHROPIC_UNIX_SOCKET` is set, a tunnelling mode that is not an option);
  `CLAUDE_CODE_USER_EMAIL` is only a fallback when no account is present. `currentDate` is built
  with the same user context; `CLAUDE_CODE_OVERRIDE_DATE` exists but pins the date, not removes it.
  The identity line "You are a Claude agent, built on Anthropic's Claude Agent SDK." is one of
  three fixed prefixes, chosen by mode; the prefix is likely part of the subscription contract and
  is not expected to be removable.
- A scratch `CLAUDE_CONFIG_DIR` holding only `.credentials.json` and a minimal `.claude.json`
  authenticates on Linux, but the email is **still injected** (the account profile is re-fetched),
  so a private config dir does not remove it, and it would break stored-login lookup on platforms
  that key the credential by config dir.
- A parallel run of the env-var candidates (`CLAUDE_CODE_REMOVE_PROMPT_STRINGS`,
  `CLAUDE_CODE_OVERRIDE_DATE`, `CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS`, `CLAUDE_CODE_SIMPLE`,
  `CLAUDE_CODE_ENTRYPOINT=sdk-cli`) produced interleaved output and is inconclusive; step 1
  repeats it sequentially.

## Design

Candidates, tried in order; the first that reaches per-step cache-write parity with a native
session (measured with the `usage` fields of consecutive requests) is implemented, and the
later ones are dropped:

1. **A switch that removes the reminder and date** (env var, settings key or flag found by the
   sequential probe in step 1). Cheapest, and also fixes privacy. Goes into `FIXED_ENV` or
   `base_settings` with a comment on why.
2. **Make the context land on a stable message.** The reminder attaches to the first message
   that reaches the API. If the replay could make the first history user frame a real (not
   zero-turn) turn, it would carry the reminder and every later request would replay it
   identically. Rejected unless that turn costs no model call; step 1 checks whether a frame
   form exists that attaches the context without generating a response.
3. **Replay the reminder in history.** The adapter writes the same text into the replayed copy of
   the formerly-final message, so replay equals the cached request. The text is the CLI's, not
   ours, so this couples the adapter to a CLI string and is taken only if 1 and 2 fail, behind a
   version check that falls back to today's behavior.
4. **Accept and document.** If none holds, record the measurement and the Agent SDK identity note
   in the docs; the existing one-message-earlier marker stays.

The identity line and the `# Environment` block are not removable with a replaced system prompt;
the plan documents them rather than working around them.

## Implementation Steps

1. Sequential probe of the candidate switches against the real CLI (manual, recorded in this
   plan's Decisions): for each, report whether email, date, environment block and identity are
   present, and the cache-write tokens of two consecutive requests with identical replayed
   history. Also probe whether any frame form attaches the context without a model call.
2. Implement the winning candidate (1, 2 or 3 above) in `cli.rs` / `transport.rs` /
   `protocol.rs`.
3. Update `claude_cli_double` (`rust/fontaine-core/examples/claude_cli_double.rs`) only if the
   chosen candidate changes the frames or args the double must accept.
4. Update `docs/claude-subscription.md`: the measured cache-write figure, what the CLI still
   adds (identity line, environment block), and a note that the Agent SDK identity line is the
   CLI's, not Fontaine's, so the scope statement stays true.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/cli.rs` (env / settings / args)
- `rust/fontaine-core/src/providers/claude_subscription/transport.rs`, `protocol.rs` (only for
  candidates 2 and 3)
- `rust/fontaine-core/examples/claude_cli_double.rs`, `rust/fontaine-core/tests/claude_subscription.rs`
- `docs/claude-subscription.md`, `CHANGELOG.md`

## Trade-offs

- Privacy and fidelity are secondary by direction; a candidate that fixes only those and not the
  cache cost is not worth coupling to CLI internals.
- A private config dir is rejected: the email survives it and it risks stored-login lookup.
- `ANTHROPIC_UNIX_SOCKET` is rejected: it changes the CLI's transport and policy checks.

## Decisions

- 2026-11-07 Priority is usage parity with Claude Code; privacy and fidelity are secondary
  (user direction mid-run).

## Documentation

`docs/claude-subscription.md` as in step 4. No master-plan Decisions entry unless a candidate
deviates from Claude Code's behavior.

## Testing Strategy

- Unit tests (args / env / settings) pin the chosen switch: `fixed_args` and the fixed env contain
  it and nothing else changed.
- If candidate 3: unit tests on history translation (the replayed text equals the injected text;
  an unknown CLI version falls back to no insertion).
- `claude_cli_double` integration test through `fontaine -p` keeps the replay protocol green.

## Manual Verification

1. Run two consecutive requests against the real CLI (as in the #449 measurement) and compare
   `cache_creation_input_tokens` per step to a native session's: expect one message's worth,
   not two. Not automatable: it needs a live subscription and the real CLI.

## Open Questions

- Whether any CLI-side switch exists is answered by step 1; the candidates above are ordered so
  the plan holds whichever way it falls.
