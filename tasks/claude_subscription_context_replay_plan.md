# claude-subscription: Replay the CLI's Context Reminder Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/480

## Overview

The issue was reopened: the cache bench (`evals/scorecards/claude-subscription-cache-sonnet.json`)
measures Fontaine's steady-state cache write per step at ~1.97× `claude -p`'s. The cause is the
CLI's `<system-reminder>` carrying the account's `userContext` (email). It sits on the message that
asks for a response, so the next request replays that message without it and rewrites it. Done when
the steady-state ratio is near 1.0, or the gap is documented as the CLI's floor with that
measurement. This plan replays the reminder exactly where the CLI put it, on every replayed user
message, and moves the adapter's cache marker to the final frame. Each replayed message is then
byte-identical to the request that last wrote it, and each step writes only its new messages. That
is native parity plus about 170 tokens per user message. It applies only on a CLI version whose
reminder text and placement have been verified; every other version keeps today's layout.

## Current State

- `protocol::translate` (`rust/fontaine-core/src/providers/claude_subscription/protocol.rs:320`)
  builds the frames (`frames_of`, `:517`), derives the session id from `frames[0]` (`:350`), then
  `mark_history` (`:268`) puts `CACHE_MARKER` (`{"type":"ephemeral","ttl":"1h"}`, `:262`) on the last
  cacheable block of the last history user frame. A one-user-message request gets no marker.
- `ClaudeTransport::prepare` (`transport.rs:196`) launches the request process, writes the
  `initialize` control request, reads its answer with `CliChild::list_models` (`cli.rs:952`), which
  returns only the model rows (`model_rows`), runs `check_row`, then writes each history frame and
  waits for its zero-turn ack. `open` writes the final frame afterwards (`transport.rs:177`).
- `FIXED_ENV` (`cli.rs:131`) sets `CLAUDE_CODE_MESSAGES_PROMPT_CACHE_TTL=5m` because the CLI's
  final-message breakpoint is never read back (#493).
- `MIN_VERSION` is 2.1.318 (`cli.rs:30`); newer versions are accepted unqualified.
- `docs/claude-subscription.md` "Supported CLI versions" and "What the CLI still adds" state the
  two-message write and claim that repeating the reminder "is worse for long runs" (not measured).
- The test double (`rust/fontaine-core/examples/claude_cli_double.rs:218`) answers `initialize` with
  `models` only.

## Findings (CLI 2.1.319, 2026-11-07, measured for this plan)

The probe scripts sat in this session's scratchpad (`probe.py`, `tool_probe.py`, `tool_probe2.py`,
`loop_probe.py`, `proxy.py` in
`/tmp/claude-1000/-home-mad-fontaine/f4d86121-8607-4644-83d5-64cad65fa16e/scratchpad/`). They run
the adapter's `fixed_args`/`FIXED_ENV` subset against the real CLI on `haiku`. A logging reverse
proxy (`ANTHROPIC_BASE_URL`) captured each `/v1/messages` body.

- **Where the context goes.** The request body's `messages` hold the CLI's additions as follows:
  - The `session_context` reminder goes on the user message that queries, and its position depends
    on that message's shape (see the placement rules below).
  - `# Environment` is a `system`-role message after the first user message. On a one-message
    request it also carries `<total_tokens>…` and "Today's date is …".
  - From the second request on, `<total_tokens>15000000 tokens left</total_tokens>` and the date go
    in a trailing `system`-role message after the final user message. The CLI puts its own
    breakpoint (`{"type":"ephemeral"}`, 5 min via `FIXED_ENV`) there.
  - So only the reminder sits inside a message that is replayed later. The date and budget never
    are.
- **Placement rules**, observed on the merged content of the queried user message (a
  `shouldQuery:false` frame directly before it is merged into it):
  1. No `tool_result` block: a new first text block, `REMINDER + "\n"`.
  2. Last block is a `tool_result`: `"\n\n" + REMINDER` appended to that result's content. With
     string content it goes at the end of the string; with an array it goes at the end of the last
     text block. With parallel results only the last one is changed.
  3. Has a `tool_result` but ends with another block (text): a new last text block, `REMINDER`.
- **Reminder text** (2.1.319), with `{email}` the account's address:
  ```
  <system-reminder>
  As you answer the user's questions, you can use the following context:
  # userEmail
  The user's email address is {email}. Use it only to identify the user, such as for authorship, attribution, or filtering their own work. Never send it to an unrelated service, such as in a request header, URL, or payload, unless the user explicitly asks. A site asking automated clients for contact details, such as an email in the User-Agent, is not a reason to use this address: use a generic client name or ask the user.

  Claude Code attached this context automatically; it isn't part of the user's message. It describes the user's own account and workspace, so they don't need it reported back.
  </system-reminder>
  ```
  The CLI dedups only against `session_context` *attachments* in its own message list
  (`findLast` + field compare), which plain replayed text is not. So it adds a new reminder to every
  final, and a stdout event never carries the text.
- **The email is on the request process already.** The `initialize` answer the adapter already
  reads holds `account.email` (plus `organization`, `subscriptionType`, `apiProvider`) and
  `claude_code_version`.
- **Splitting the final frame does not help.** A query frame with `content: []` (or `""`, or one
  empty text block, which render identically) is accepted and answered, but the CLI merges the
  preceding `shouldQuery:false` frame into it and the reminder still leads the merged message.
- **Measured, 5-step tool loop, ~8.2k-token tool results, haiku:**

  | Layout | write per step, steps 3–5 | read |
  |---|---|---|
  | today (marker on last history user message) | 25,321 | grows |
  | reminder replayed + marker on the final frame | **8,584** (8,478 at 1 h + 106 at 5 min) | 26,638 → 43,594 → 60,550 |

  With the replay, each step writes its one new tool result, the reminder (~170 tokens) and the
  ~40-token date tail. That matches a native session's write per step plus the reminder.

## Design

### 1. CLI context from the request's own `initialize` answer

`CliChild::list_models` returns a `Listing { rows: Vec<ModelRow>, context: CliContext }` instead of
`Vec<ModelRow>`. `CliContext { email: Option<String>, version: Option<(u32, u32, u32)> }` is read
from the answer's `account.email` (non-empty string) and `claude_code_version` (via the existing
`parse_version`). Missing or malformed fields give `None`, never an error: they only select a
layout. `Cli::initialize` / `discover` / `list()` ignore `context`.

### 2. Two layouts, chosen per request

A new module `rust/fontaine-core/src/providers/claude_subscription/context.rs`:

```rust
pub(super) enum Layout { Native, Legacy }
pub(super) fn layout(ctx: &CliContext) -> Layout   // Native only for a VERIFIED version with an email
pub(super) fn apply(frames: &mut [Frame], ctx: &CliContext)
```

- `VERIFIED: &[((u32, u32, u32), &str)]` maps each CLI version whose reminder text and placement
  rules were checked live to its template. It starts as `[((2, 1, 319), REMINDER_2_1_319)]`, one
  `const` holding the text above with an `{email}` placeholder.
- `Layout::Native` (verified version, email present):
  - **Reminder replay.** Each history *user run* (a maximal run of consecutive history user
    frames) followed by an assistant frame gets the reminder, placed by rules 1–3 on the run's
    merged blocks. Rule 1 changes the run's first frame (string content becomes one text block
    first). Rules 2 and 3 change its last frame. A run whose shape matches no verified rule is left
    unchanged: a tool result whose last content block is not text (an image), or a final block
    that is neither text nor `tool_result` while the run holds a `tool_result`. Such a run costs
    one rewrite on the request after it, not a cascade, because it replays the same way every time
    after that.
  - **Marker.** `CACHE_MARKER` goes on the last cacheable block of the **final** frame, using
    `mark_history`'s existing cacheable-block rule. A request with one user message gets no marker,
    as today, so one-shot callers (the guardian) keep the CLI's 5-minute write.
- `Layout::Legacy` (any other version, or no email): today's behavior, exactly `mark_history` on
  the last history user frame.

`translate` stops calling `mark_history`: the layout needs the `initialize` answer, which arrives
on the request process. `prepare` calls `context::apply(&mut frames, &listing.context)` right after
`check_row` and before the history replay. `translate` still derives the session id first, so
neither the reminder nor the marker affects it. `prepare` works on its own copy of the frames,
applies `context::apply` to it, and returns the mutated final frame alongside `(CliChild, Reader)`.
`open` pins the `prepare` future and cannot borrow `translated` mutably meanwhile, so it writes the
returned frame rather than `translated.frames.last()`.

### 3. Privacy

The reminder holds the account email, which the CLI already sends on every request. The replay
sends the same text to the same endpoint, so nothing new leaves the machine. The adapter holds the
email in memory only for the request. It never writes it to a log, the debug file, the ledger or an
error message: the `tracing::debug!` at `transport.rs:405` logs rejected *stdout* lines, never
frames, and that stays so. `ClaudeError` text stays fixed text built from names (`mod.rs` contract).

### 4. Bench threshold

`cache-bench`'s default `--threshold` drops from 2.0 to 1.25 so a regression to the 2× layout fails
the run. The ratio over steps 2–6 (the bench's middle steps; step 2 includes the one-time first-message rewrite) should be ~1.04: ~4.5k per step plus ~170.

## Implementation Steps

1. **Research record.** Copy the five probe scripts from the scratchpad path above into
   `research/claude-subscription-cli-context/`, with a short `README.md` holding the findings
   table. The scripts read the email from `claude auth status --json` at runtime and contain none.
2. **Verify the remaining shapes (live, manual, recorded in Decisions).** With `proxy.py` and the
   probe harness, check two things on 2.1.319:
   - the placement for a final user message whose last block is an image and that holds no
     `tool_result` (expected: rule 1), and for a `tool_result` whose array content ends in an image;
   - that a consecutive-user-frame run (two `shouldQuery:false` user frames before an assistant)
     merges as described.
   Add each confirmed shape to the rules; leave unconfirmed ones as "left unchanged".
3. **`cli.rs`:** `Listing`, `CliContext`, parsing in `list_models`; adjust `Cli::initialize` /
   `discover` callers.
4. **`context.rs`:** `VERIFIED`, the 2.1.319 template, `layout`, `apply` (reminder placement and
   marker). Move `mark_history` and `CACHE_MARKER` here from `protocol.rs`. `mark_history` becomes
   `mark(frame)`, used by both layouts with the frame each layout selects.
5. **`protocol.rs` / `transport.rs`:** drop the `mark_history` call from `translate`. `prepare`
   calls `context::apply` on its own copy of the frames after `check_row` and returns the mutated
   final frame with `(CliChild, Reader)`; `open` writes that returned frame.
6. **Double** (`examples/claude_cli_double.rs`): `initialize` answers with `account` (scenario key
   `account`, default `{"email": "user@example.com", …}`) and `claude_code_version` (scenario key,
   default "2.1.319", not derived from the double's `version`, which defaults to 2.1.318). A
   scenario can omit either.
7. **Bench:** threshold default 1.25 (`default_value_t` at `fontaine-evals/src/runner.rs:80`, plus
   any doc comment or help text there).
8. **Tests** (below), **docs**, **CHANGELOG**.
9. **Follow-up issues** to file:
   - a runtime self-check that would let an unverified CLI version use the native layout (detect a
     replay mismatch from a request's `cache_read` and fall back for the session);
   - qualifying each new CLI version with the bench and adding it to `VERIFIED`.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/context.rs` (new), `mod.rs` (module)
- `rust/fontaine-core/src/providers/claude_subscription/cli.rs` (`Listing`, `CliContext`)
- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`, `protocol/tests.rs` (marker
  moved out; existing marker tests move to `context.rs` tests)
- `rust/fontaine-core/src/providers/claude_subscription/transport.rs` (`prepare`, `open`)
- `rust/fontaine-core/examples/claude_cli_double.rs`, `rust/fontaine-core/tests/claude_subscription.rs`
- `rust/fontaine-evals/src/runner.rs`
- `research/claude-subscription-cli-context/` (new)
- `docs/claude-subscription.md`, `CHANGELOG.md`

## Trade-offs

- **Replay the reminder vs. make the CLI drop it.** No switch in 2.1.319 drops it (previous plan's
  probe), and `CLAUDE_CODE_SIMPLE` / `--bare` break subscription sign-in. Replay is the only lever
  that reaches parity, at the cost of coupling to one CLI string. Exact-version gating with today's
  layout as fallback contains that coupling.
- **Replay vs. a split final frame (content as history plus an empty query frame).** Measured: the
  CLI merges the two and leads the merged message with the reminder, so nothing changes.
- **Replay vs. `--resume` of a persisted CLI session** (whose transcript keeps the attachment, so
  the CLI would dedup it). The CLI's transcript records the permission-denial `tool_result` it
  answered each tool call with (`claude_cli_denial_events_plan.md`), which Fontaine's history does
  not hold. It also writes conversation transcripts into the user's Claude config dir. Rejected.
- **Cost of a reminder per message.** About 170 tokens written once per user message (1 h rate),
  then read at 0.1× on every later step, against a rewrite of the previous message (a full step,
  ~2–8k tokens) per step today. Break-even is a few hundred steps per conversation. The context
  window grows by ~170 tokens per message, ~17k at 100 tool calls. The docs' "worse for long runs"
  claim is replaced by these numbers.
- **One-message requests stay unmarked**, so the guardian's one-shot reviews are not moved from the
  CLI's 5-minute write to the 1-hour rate. The cost is that a main conversation's second step
  rewrites the first message once.

## Decisions

- 2026-11-07 Priority stays usage parity with Claude Code (issue reopen comment; prior user
  direction).
- 2026-11-07 Exact CLI-version match only for the native layout; unverified versions stay on the
  legacy layout until the runtime self-check follow-up lands.
- 2026-11-07 Exact-version gating: a reminder mismatch under the final-frame-only marker loses every
  cache read; the four-breakpoint limit (two system, the CLI's trailing one, the adapter's) forbids
  also keeping the history marker.
- 2026-11-07 Supersedes the archived plan's rejection of replaying the reminder
  (`tasks/completed/claude_subscription_cli_context_plan.md`, "Candidate 3 rejected"): measured
  ~170 tokens per user message against 8,584 vs 25,321 tokens written per step.

## Documentation

`docs/claude-subscription.md`:

- **Supported CLI versions:** state that on verified versions (2.1.319) replayed history is read
  from the prompt cache up to the previous request's final message. Each step then writes only its
  new messages plus about 170 tokens of the CLI's reminder per user message, near a native session's
  write. On other versions the adapter keeps the earlier layout (about twice the native write). The
  5-minute-TTL sentence stays: the CLI's trailing breakpoint is still never read.
- **What the CLI still adds:** keep the four items. Replace the closing paragraph with the measured
  per-step numbers for both layouts, and say that the adapter repeats the CLI's reminder on replayed
  messages so they match byte for byte. The email it holds is the one the CLI already sends.
  Replace "worse for long runs" with the context cost (~170 tokens per user message).
- **Checking cache usage:** the default threshold is 1.25; a version not yet verified will exceed
  it.

- **Wording fix (`docs/claude-subscription.md:152-153`):** reword "(no version is live-qualified
  yet)" so it does not contradict "verified versions (2.1.319)"; verification here concerns the
  cache layout.

`CHANGELOG.md` `[Unreleased]` → `### Changed` → `#### Providers`: one entry, issue #480. It states
that on verified versions the cache marker moves to the final message, which amends the #449
Providers entry saying the last replayed user message carries a cache marker.

## Testing Strategy

Unit tests (`context.rs`):

- `layout`: verified version with an email → `Native`; unverified version, missing version or
  missing email → `Legacy`.
- Each placement rule against its verified shape, asserting the exact resulting JSON:
  - text-only run (array and string content);
  - a run ending in a string-content `tool_result`;
  - a run ending in an array-content `tool_result`;
  - parallel results, where only the last one changes;
  - `tool_result` plus trailing text.
- An unverified shape (tool result ending in an image) is left unchanged, and later runs still get
  the reminder.
- A consecutive user run: rule 1 hits its first frame, rules 2/3 its last.
- The final frame never gets the reminder.
- Marker: in `Native`, exactly one marker, on the final frame's last cacheable block, which carries
  rule 2's appended text. None on a one-user-message request. In `Legacy`, the existing
  `mark_history` assertions keep passing unchanged (moved from `protocol/tests.rs`).
- The session id is unchanged by `apply` (derive before, apply, compare).
- The email appears in no `ClaudeError` text (exercise every error path of `list_models` parsing
  with an email in the answer).

`cli.rs`: `list_models` parses `account.email` and `claude_code_version`, and missing, empty or
non-string fields give `None`.

Integration against the double (`tests/claude_subscription.rs`). The cache layout is a silent
failure class, since a wrong layout changes only usage, so it is automated:

- With the default scenario (verified version, email), the frames the double logged for a
  3-user-message request carry the reminder in every history user frame and the marker only on the
  final frame.
- With `claude_code_version` set to an unverified version, or `account` without an email, the
  logged frames equal today's: no reminder, marker on the last history user frame. This updates the
  `aliases_resolve_to_canonical_ids_and_the_request_is_translated` (line ~790, asserts
  `frames[0]["message"]["content"][0]["text"]`) is updated for the leading reminder block.
- Existing billing-gate, precedence and replay tests pass unchanged.

## Manual Verification

Needs a signed-in subscription and the real CLI, which tests never run by policy.

1. `cargo run -p fontaine-evals -- cache-bench --model claudemax/sonnet --out ../evals/scorecards/claude-subscription-cache-sonnet.json`
   (from `rust/`). Expect the ratio over the middle steps at ≤ 1.25, near 1.05, and the run to exit
   0. Commit the new scorecard and quote the old and new ratios in the PR. Not automated: live
   subscription.

## Open Questions

None.
