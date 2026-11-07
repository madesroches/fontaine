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
- 2026-11-07 Probe, CLI 2.1.319, through a logging proxy on the real requests. The reminder is a
  `session_context` attachment (email) and the date a `date` attachment; both are computed at the
  start of a query turn against the attachments already in the message list, and a
  `shouldQuery:false` frame creates none. The first query turn of a request is always the final
  frame, so the reminder lands there, as the first block of the final user message, and the date
  and a token-budget line follow as a trailing `system` message. `# Environment` is a `system`
  message after the first user message, stable. Candidate 1 (no switch): sequentially,
  `CLAUDE_CODE_REMOVE_PROMPT_STRINGS`, `CLAUDE_CODE_OVERRIDE_DATE`,
  `CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS`, `CLAUDE_CODE_DISABLE_ATTACHMENTS`,
  `CLAUDE_CODE_SESSION_START_ANNOUNCEMENTS_BEFORE_PROMPT`, `CLAUDE_CODE_ATTACHMENTS_IN_USER_DATA`
  and `CLAUDE_CODE_ENTRYPOINT=sdk-cli` left email, date, environment and identity all present;
  `CLAUDE_CODE_SIMPLE=1` removes them only by making the CLI "not logged in" (it implies an API
  key). Candidate 2 (no frame form): a first frame that is a query costs a model call; a local
  slash command (`/cost`) makes no call but attaches no context and adds its own output to the
  history. Candidate 3 rejected: the reminder would have to be repeated in every replayed user
  message (about 250 tokens each for the rest of the conversation) because each earlier final
  carried it, which costs more than the rewrite it saves once a conversation is long.
- 2026-11-07 Measured per step, haiku, four 6000-token user messages, new process per step with the
  history marker. Native single process: writes 6646, 6421, 6078, 6043 (reads 0, 6269, 12690,
  18768). Adapter: writes 6645, 12667, 12636, 12289 (reads 0, 0, 6049, 12409). No candidate reaches
  write parity (the previous final carried the reminder, so it is written again). Candidate 4
  applies, plus one cost lever found in the probe: `CLAUDE_CODE_MESSAGES_PROMPT_CACHE_TTL=5m` makes
  the CLI's unreadable final-message breakpoint a 5-minute write while the adapter's marker stays at
  1 hour. Measured with it: per step 1 h writes 6050, 6351, 6014 and 5 min writes 6609, 6272, 6272
  (reads 0, 6050, 12401); same token counts, about 19% less billed write cost than all at 1 h
  (2x for 1 h, 1.25x for 5 min).

## Documentation

`docs/claude-subscription.md` as in step 4. No master-plan Decisions entry unless a candidate
deviates from Claude Code's behavior.

## Testing Strategy

- Unit tests (args / env / settings) pin the chosen switch: `fixed_args` and the fixed env contain
  it and nothing else changed.
- If candidate 3: unit tests on history translation (the replayed text equals the injected text;
  an unknown CLI version falls back to no insertion).
- The existing `claude_cli_double` tests in `rust/fontaine-core/tests/claude_subscription.rs` keep the replay protocol green:
  `aliases_resolve_to_canonical_ids_and_the_request_is_translated`,
  `session_ids_are_stable_per_conversation_and_separate_per_caller` and
  `a_signed_thinking_block_and_a_tool_call_decode_with_their_names_restored`.
- If candidate 2: a double-based test of the new frame form.

## Manual Verification

1. Run two consecutive requests against the real CLI (as in the #449 measurement) and compare
   `cache_creation_input_tokens` per step to a native session's: expect one message's worth,
   not two. Not automatable: it needs a live subscription and the real CLI.
