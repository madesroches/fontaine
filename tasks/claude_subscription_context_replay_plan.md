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
is native parity plus about 170 tokens per user message. It applies on any CLI version at or above
2.1.319 (the version the template was taken from) for an account with an email, until a runtime
self-check finds that version's replay not read back; that version then falls back to today's layout
for the rest of the process.

## Current State

- `protocol::translate` (`rust/fontaine-core/src/providers/claude_subscription/protocol.rs:320`)
  builds the frames (`frames_of`, `:517`), derives the session id from `frames[0]` (`:350`), then
  `mark_history` (`:268`) puts `CACHE_MARKER` (`{"type":"ephemeral","ttl":"1h"}`, `:262`) on the last
  cacheable block of the last history user frame. A request with no history user frame gets no marker.
- `ClaudeTransport::prepare` (`transport.rs:199`) launches the request process, writes the
  `initialize` control request, reads its answer with `CliChild::list_models` (`cli.rs:952`), which
  returns only the model rows (`model_rows`), runs `check_row`, then writes each history frame and
  waits for its zero-turn ack. `open` writes the final frame afterwards (`transport.rs:178-183`).
- The provider has no `Approver`: its only user-visible warning path today is `tracing::warn!`
  (`cli.rs:801`, `Scratch::drop`). The request's result usage is read in `Reader::result`
  (`protocol.rs:1193`), which today only checks `output_tokens`; `Reader` also takes the shared
  `Usage` (quota) from `ClaudeTransport`.
- `FIXED_ENV` (`cli.rs:133`) sets `CLAUDE_CODE_MESSAGES_PROMPT_CACHE_TTL=5m` because the CLI's
  final-message breakpoint is never read back (#493).
- `MIN_VERSION` is 2.1.318 (`cli.rs:30`); newer versions are accepted unqualified.
- `docs/claude-subscription.md` "Supported CLI versions" and "What the CLI still adds" state the
  two-message write and claim that repeating the reminder "is worse for long runs" (not measured).
- The test double (`rust/fontaine-core/examples/claude_cli_double.rs:218`) answers `initialize` with
  `models` only.

## Findings (CLI 2.1.319, 2026-11-07, measured for this plan)

The probe scripts sat in this session's scratchpad (`probe.py`, `tool_probe.py`, `tool_probe2.py`,
`tool_probe3.py` (the probe for rules 4-5), `loop_probe.py`, `proxy.py` in
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
  4. Last block is a `tool_result` followed by an empty text block `{"type":"text","text":""}` (the
     common tool-loop shape Fontaine sends): the CLI drops the empty block and appends a new last
     text block whose text is exactly `REMINDER` (no separator). Verified live on 2.1.319.
  5. `tool_result` content variants under rule 2, verified live on 2.1.319: content `""` becomes the
     string `REMINDER` (no `"\n\n"` prefix); absent content becomes the string `REMINDER`; content
     `[]` becomes `[{"type":"text","text": REMINDER}]`.
- **Reminder text** (2.1.319), with `{email}` the account's address:
  ```
  <system-reminder>
  As you answer the user's questions, you can use the following context:
  # userEmail
  The user's email address is {email}. Use it only to identify the user, such as for authorship, attribution, or filtering their own work. Never send it to an unrelated service, such as in a request header, URL, or payload, unless the user explicitly asks. A site asking automated clients for contact details, such as an email in the User-Agent, is not a reason to use this address: use a generic client name or ask the user.

  Claude Code attached this context automatically; it isn't part of the user's message. It describes the user's own account and workspace, so they don't need it reported back.
  </system-reminder>
  ```
  The reminder can hold sections `userEmail`, `attachedProject`, `gitStatus` and `perforceMode`.
  Measured live on 2.1.319 (2026-11-07) with an ancestor git repository (initialized, one commit)
  above the CLI's cwd, with and without `GIT_CEILING_DIRECTORIES`, and with
  `CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS=1` or `includeGitInstructions:false`: no `gitStatus`
  section appears in this mode. The stable `# Environment` block then says "Is a git repository:
  true", which is stable per machine and does not affect replay. `attachedProject` and
  `perforceMode` need env vars the adapter's rebuilt environment drops. A future CLI that adds a
  section is caught by the runtime self-check (Design section 2).
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
layout. `Cli::initialize` (and through it `discover` / `list()`) ignores `context`.

### 2. Two layouts, chosen per request

A new module `rust/fontaine-core/src/providers/claude_subscription/context.rs`:

```rust
pub(super) enum Layout { Native, Legacy }
pub(super) fn layout(ctx: &CliContext) -> Layout // reads the static broken-version set
pub(super) fn apply(frames: &mut [Frame], ctx: &CliContext, layout: Layout)
```

- `const TEMPLATE_SINCE: (u32, u32, u32) = (2, 1, 319)` and `REMINDER_2_1_319`, one `const` holding
  the text above with an `{email}` placeholder. It is the one template, applied to every version
  at or above `TEMPLATE_SINCE`.
- `layout` is `Native` when `ctx.version >= TEMPLATE_SINCE`, `ctx.email` is present and that
  exact version is not in the process-wide broken set. Otherwise `Legacy`.
- **Verdict store.** One process-wide static in `context.rs` (a `LazyLock<Mutex<…>>`), so it
  survives the new `Connected` that `client_for` builds per main model, `/model` switch, subagent
  with an explicit model and non-reused guardian. It holds the set of broken CLI versions, sticky
  for the process lifetime and never persisted. A sibling static holds the per-conversation record,
  keyed by the derived session id in a bounded map (the last 256 conversations; a first-party `HashMap` plus a `VecDeque` for eviction order, no new dependency): the previous
  Native request's pre-`apply` frame fingerprint (a hash per frame), its usage
  (`cache_read_input_tokens`, `cache_creation.ephemeral_1h_input_tokens`), its completion time and the per-frame block counts of the frames as sent (after `apply`).
  The statics choose only a cache layout (advisory, not permission, spend or durable), so they use
  `lock_recovering` with a one-line reason at the call site.
- **Self-check**, a pure function `check(prev: &Record, now: &Record) -> bool` (true when the request shows a broken replay), run when
  a Native request completes and the same conversation's previous request was Native. All of these
  must hold, else `false`: the frames extend the previous request's frames (prefix fingerprints
  equal); the gap is under 55 minutes (1 h TTL with margin); and the step added no more than 18
  blocks since the previous final frame (from the `Record`'s per-frame block counts) (inside the API's 20-block lookback). The expected read is
  `prev.read + prev.write_1h`. A read short of it by more than 64 tokens returns `true` and the
  caller adds the version to the broken set; a version never leaves it.
- **Usage source.** `Reader::result` (`protocol.rs:1193`), on the accepted final result, already
  reads `value["usage"]`; it keeps `cache_read_input_tokens` and
  `cache_creation.ephemeral_1h_input_tokens` and the stream hands them to `transport.rs`'s
  `stream` at `Event::Complete`, which records the request and runs `check`.
- **First broken verdict for a version** emits one warning naming the CLI version (never the email),
  saying the cache optimization is off for that version and that `cache-bench` measures it. The
  provider has no `Approver` today, only `tracing::warn!`. Routing it through `Approver::warn`
  (AGENTS.md) is part of step 5: an `Approver` handle is passed into the transport from the
  provider's construction site, with the `tracing::warn!` kept as the fallback when none is given.
- `Layout::Native`:
  - **Reminder replay.** Each history *user run* (a maximal run of consecutive history user
    frames) followed by an assistant frame gets the reminder, placed by rules 1–5 on the run's
    merged blocks. Rule 1 changes the run's first frame (string content becomes one text block
    first). Rules 2 to 5 change its last frame; rule 4 removes the trailing empty text block(s) and
    appends the reminder block. A run whose shape matches no known rule is left
    unchanged: a tool result whose array content ends in a `document` or image block, or a final
    `document`/image block in a run with no `tool_result`. Such a run costs
    one rewrite on the request after it, not a cascade, because it replays the same way every time
    after that.
  - **Marker.** `CACHE_MARKER` goes on the last cacheable block of the **final** frame, using
    `mark_history`'s existing cacheable-block rule. No marker when there is no history user frame (as `mark_history` does today),
    so one-shot callers (the guardian) keep the CLI's 5-minute write.
- `Layout::Legacy` (version below `TEMPLATE_SINCE`, no version, no email, or a `Broken` verdict):
  today's behavior, exactly `mark_history` on
  the last history user frame.

`translate` stops calling `mark_history`: the layout needs the `initialize` answer, which arrives
on the request process. `prepare` fingerprints the frames, then calls `layout(&listing.context)`
(reading the static verdicts) and passes the chosen `Layout` into
`context::apply(&mut frames, &listing.context, layout)` right after `check_row` and before the
history replay. `translate` still derives the session id first, so
neither the reminder nor the marker affects it. `prepare` takes the `Translated` by value (`open` uses
`translated` after the select only for `frames.last()`, `transport.rs` ~178-181), applies
`context::apply` in place, and returns the final frame alongside `(CliChild, Reader)` for `open` to
write after login passes.

### 3. Privacy

The reminder holds the account email, which the CLI already sends on every request. The replay
sends the same text to the same endpoint, so nothing new leaves the machine. The adapter holds the
email in memory only for the request. It never writes it to a log, the debug file, the ledger or an
error message: the `tracing::debug!` at `transport.rs:405` logs rejected *stdout* lines (up to 4 KB
each), never frames. The `initialize` answer holds `account.email` and is taken by `list_models`
before it reaches the reader or `rejected`, and must stay so. The self-check warning names the CLI
version only, never the email. `ClaudeError` text stays fixed text built from names (`mod.rs` contract).

### 4. Bench threshold

`cache-bench`'s default `--threshold` drops from 2.0 to 1.25 so a regression to the 2× layout fails
the run. The ratio over steps 2–6 (the bench's middle steps; step 2 includes the one-time first-message rewrite) should be ~1.04: ~4.5k per step plus ~170.

## Implementation Steps

1. **Research record.** Copy all six probe scripts from the scratchpad path above into
   `research/claude-subscription-cli-context/`, with a short `README.md` holding the findings
   table. The scripts read the email from `claude auth status --json` at runtime and contain none. The
   README also records the git-ancestor probe (run inline in a shell, no script): an initialized
   one-commit git repository above the CLI's cwd, run with and without `GIT_CEILING_DIRECTORIES`
   and with `CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS=1` / `includeGitInstructions:false`, to see
   whether a `gitStatus` section appears.
2. **Verify the remaining shapes (live, manual, recorded in Decisions).** With `proxy.py` and the
   probe harness, check the remaining shapes on 2.1.319:
   - a `tool_result` whose array content ends in a `document` or image block;
   - a final `document`/image block in a run with no `tool_result`;
   - that a consecutive-user-frame run (two `shouldQuery:false` user frames before an assistant)
     merges as described.
   Add each confirmed shape to the rules; leave unconfirmed ones as "left unchanged".
3. **`cli.rs`:** `Listing`, `CliContext`, parsing in `list_models`; adjust `Cli::initialize` /
   `discover` callers and the `listing_with` unit-test helper (`cli.rs:1194-1212`, returns
   `Vec<ModelRow>` from `list_models`) with its three tests (~`cli.rs:1216`, `1231`, `1246`).
4. **`context.rs`:** `TEMPLATE_SINCE`, the 2.1.319 template, the verdict statics and `Record`, the
   pure `check`, `layout`, `apply` (reminder placement and marker). Move `mark_history` and
   `CACHE_MARKER` here from `protocol.rs`; reword their doc comments (`protocol.rs:260-261`, `264-267` say the
   marker goes on the last user frame before the final because a breakpoint there is never read
   back) to describe both layouts: Legacy marks the last history user frame, Native the final
   frame. `mark_history` becomes
   `mark(frame)`, used by both layouts with the frame each layout selects. Marker-test migration:
   `the_last_history_user_message_carries_the_one_marker` (`protocol/tests.rs:1128`),
   `string_content_becomes_one_marked_text_block` (`:1141`, whose :1149 assertion that the final
   frame stays a string is false under Native) and `a_single_user_message_gets_no_marker` (`:1153`)
   move to `context.rs`, calling `context::apply` in `Legacy`, each with a Native counterpart;
   `the_marker_does_not_change_the_session_id` (`protocol/tests.rs:1177`) only checks session ids
   across 1/2/3-message conversations and stays in `protocol/tests.rs` (the new derive-apply-compare
   test is added beside it, not replacing it); `a_request_cache_control_is_stripped_and_never_replaces_the_marker`
   (`:1158`) is split, the strip half stays and the marker half moves; the `marked` (`:1097`) and
   `tool_loop` (`:1110`) helpers move or are shared. Reword the `FIXED_ENV` comment at
   `cli.rs:155-157` ("the adapter's own 1 h marker earlier in the history is the one that is" read
   back) for the native layout.
5. **`protocol.rs` / `transport.rs` / `mod.rs`:** drop the `mark_history` call from `translate`.
   `prepare` takes the `Translated` by value, fingerprints the frames, applies `context::apply` in
   place after `check_row` and returns the final frame with `(CliChild, Reader)`; `open` writes
   that returned frame. `prepare` also returns the request's record inputs (session id, CLI version,
   chosen layout, pre-`apply` fingerprint); `Running` (`transport.rs` ~380-385) stores them.
   `Reader` exposes the result's cache usage (`cache_read_input_tokens`,
   `cache_creation.ephemeral_1h_input_tokens`, kept by `Reader::result`) through an accessor that
   `stream` reads at `Event::Complete` (a unit variant, `protocol.rs` ~660), then records the
   request in the context statics and runs `check`. Add the warning route (`Approver::warn`, see Design 2).
6. **Double** (`examples/claude_cli_double.rs`): `initialize` answers with `account` (scenario key
   `account`, default `{"email": "user@example.com", …}`) and `claude_code_version` (scenario key,
   default "2.1.319", not derived from the double's `version`, which defaults to 2.1.318). A
   scenario can omit either. Document both new keys in the double's module doc
   (`examples/claude_cli_double.rs:9-18`).
7. **Bench:** threshold default 1.25 (`default_value_t` at `fontaine-evals/src/runner.rs:80`, plus
   any doc comment or help text there) and the `--threshold` row in `evals/README.md` (:387, default
   2.0 -> 1.25). `rust/fontaine-evals/tests/cache_bench.rs:113`
   (`an_unflagged_run_writes_the_scorecard_and_passes_the_shared_prompt`) runs at the default with
   a fixture ratio of 1.5 and asserts exit 0 (:116) and not flagged (:121): change its fixture so
   the ratio is under 1.25 and the run stays unflagged (and update the `1.5` ratio assertion).
8. **Tests** (below), **docs**, **CHANGELOG**.
9. **Follow-up issues** to file:
   - persisting verdicts across processes;
   - updating the template (and `TEMPLATE_SINCE`) when the self-check warning fires for a new CLI
     version, after measuring it with the bench.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/context.rs` (new), `mod.rs` (module)
- `rust/fontaine-core/src/providers/claude_subscription/cli.rs` (`Listing`, `CliContext`; reword
  the `FIXED_ENV` comment at `:155-157`)
- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`, `protocol/tests.rs` (marker
  moved out; the marker half of the split cache-control test and the `marked`/`tool_loop` helpers
  move or are shared; the session-id test stays)
- `rust/fontaine-core/src/providers/claude_subscription/transport.rs` (`prepare`, `open`, `stream`
  records the request); `protocol.rs` also: `Reader::result` keeps the cache usage
- `rust/fontaine-core/examples/claude_cli_double.rs`, `rust/fontaine-core/tests/claude_subscription.rs`
- `rust/fontaine-evals/src/runner.rs`, `rust/fontaine-evals/tests/cache_bench.rs`, `evals/README.md`
- `research/claude-subscription-cli-context/` (new)
- `docs/claude-subscription.md`, `CHANGELOG.md`

## Trade-offs

- **Cost of a reminder per message.** About 170 tokens written once per user message (1 h rate),
  then read at 0.1× on every later step, against a rewrite of the previous message (a full step,
  ~2–8k tokens) per step today. Break-even is a few hundred steps per conversation. The context
  window grows by ~170 tokens per message, ~17k at 100 tool calls.
- **One-message requests stay unmarked**, so the guardian's one-shot reviews are not moved from the
  CLI's 5-minute write to the 1-hour rate. The cost is that a main conversation's second step
  rewrites the first message once.

## Decisions

- 2026-11-07 Priority stays usage parity with Claude Code (issue reopen comment; prior user
  direction).
- 2026-11-07 User direction (CLI updates are frequent, so an exact-version list would leave the
  optimization off almost always): native layout on any version >= 2.1.319 with a per-version
  runtime self-check falling back to Legacy. A wrong template costs about one full-history rewrite
  in one conversation before fallback; the four-breakpoint limit (two system, the CLI's trailing
  one, the adapter's) still forbids also keeping the history marker.
- 2026-11-07 Verdicts are per process (one static, not per connection) and not persisted;
  persistence is a follow-up.
- 2026-11-07 Replay the reminder rather than make the CLI drop it: dropping it is impossible on
  2.1.319, and `--bare` / `CLAUDE_CODE_SIMPLE` break subscription sign-in.
- 2026-11-07 Replay rather than `--resume` of a persisted CLI session: the CLI transcript holds
  denial results Fontaine's history lacks, and it writes transcripts into the user's config dir.
- 2026-11-07 Supersedes the archived plan's rejection of replaying the reminder
  (`tasks/completed/claude_subscription_cli_context_plan.md`, "Candidate 3 rejected"): measured
  ~170 tokens per user message against 8,584 vs 25,321 tokens written per step.

## Documentation

`docs/claude-subscription.md`:

- **Supported CLI versions:** state that on versions from 2.1.319 replayed history is read
  from the prompt cache up to the previous request's final message. Describe the self-check: the
  adapter compares a conversation's cache read with what the previous request wrote, and on a
  shortfall turns the layout off for that CLI version for the rest of the process and warns once
  (naming the version); `cache-bench` measures it. Each step then writes only its
  new messages plus about 170 tokens of the CLI's reminder per user message, near a native session's
  write. On versions below 2.1.319, or after a warning, the adapter keeps the earlier layout (about twice the native write).
  Reword `docs/claude-subscription.md:158-162` (lines 158-160 say the cache is read "up to the
  previous request's last replayed user message", that each request writes the last two messages,
  "about twice a native … write"; 160-162 the never-read 5-minute write): on the native layout the never-read 5-minute write
  is the CLI's trailing date/budget block and the adapter's marker on the final message keeps the
  hour; on the earlier layout the current text holds.
- **What the CLI still adds:** keep the four items. Replace the closing paragraph with the measured
  per-step numbers for both layouts, and say that the adapter repeats the CLI's reminder on replayed
  messages so they match byte for byte. The email it holds is the one the CLI already sends.
  Replace "worse for long runs" with the context cost (~170 tokens per user message).
- **Checking cache usage:** rewrite the whole parenthetical at `docs/claude-subscription.md:204`
  ("(default 2.0, the gap described under What the CLI still adds)"), not just the number: e.g.
  default 1.25, above the native layout's expected ratio; a version on the earlier layout will exceed it.

- **Transport overview (`docs/claude-subscription.md:96-97`):** reword "Fontaine marks the last
  replayed user message for the prompt cache": the marker is on the final message on the native
  layout, on the last replayed user message otherwise.
- **Wording fix (`docs/claude-subscription.md:152-153`):** reword "(no version is live-qualified
  yet)" so it does not contradict the native layout's 2.1.319 threshold; that concerns the
  cache layout only.

`evals/README.md`: the `--threshold` row's default becomes 1.25.

`CHANGELOG.md` `[Unreleased]` → `### Changed` → `#### Providers`: one entry, issue #480. It states
that on the native layout the cache marker moves to the final message, which amends the #449
Providers entry saying the last replayed user message carries a cache marker.

## Testing Strategy

Unit tests (`context.rs`):

- `layout`: versions below 2.1.319 → `Legacy`; version >= 2.1.319 with an email and not broken → `Native`; a broken version → `Legacy`; no email or no version → `Legacy`.
- `check` (pure): previous read plus 1 h write met → `false`; a shortfall beyond 64 tokens →
  `true`; non-extending history, a gap of 55 minutes or more, or a step adding more than 18
  blocks → no verdict; a broken version stays broken after a passing check.
- Each placement rule against its verified shape, asserting the exact resulting JSON:
  - text-only run (array and string content);
  - a run ending in a string-content `tool_result`;
  - a run ending in an array-content `tool_result`;
  - parallel results, where only the last one changes;
  - `tool_result` plus trailing text;
  - `tool_result` plus a trailing empty text block (rule 4): the empty block is removed and the
    last block is the bare `REMINDER` text;
  - `tool_result` content `""`, absent and `[]` (rule 5), one test each.
- An unknown shape (tool result whose array content ends in an image) is left unchanged, and
  later runs still get the reminder.
- A consecutive user run: rule 1 hits its first frame, rules 2/3 its last.
- The final frame never gets the reminder.
- Marker: in `Native`, exactly one marker, on the final frame's last cacheable block (e.g. its trailing
  `tool_result`), and no reminder text in the final frame. No marker when there is no history user
  frame (as `mark_history` does today). In `Legacy`, the existing `mark_history` assertions are
  kept, rewritten to call `context::apply` in `Legacy` (the three tests named in step 4 and the marker half of the split
  `a_request_cache_control_is_stripped_and_never_replaces_the_marker` moves from
  `protocol/tests.rs`; its strip half stays).
- The session id is unchanged by `apply` (derive before, apply, compare); added beside
  `the_marker_does_not_change_the_session_id`, which stays in `protocol/tests.rs`.
- The email appears in no `ClaudeError` text (exercise every error path of `list_models` parsing
  with an email in the answer).

`cli.rs`: `list_models` parses `account.email` and `claude_code_version`, and missing, empty or
non-string fields give `None`.

Integration against the double (`tests/claude_subscription.rs`). The cache layout is a silent
failure class, since a wrong layout changes only usage, so it is automated:

- With the default scenario (`claude_code_version` stays "2.1.319", email), the frames the double logged for a
  3-user-message request carry the reminder in every history user frame and the marker only on the
  final frame.
- With `claude_code_version` set below 2.1.319, or `account` without an email, the
  logged frames equal today's: no reminder, marker on the last history user frame. This updates the
  `aliases_resolve_to_canonical_ids_and_the_request_is_translated` (line ~790, asserts
  `frames[0]["message"]["content"][0]["text"]`) is updated for the leading reminder block.
- Self-check, using the double's per-response `usage` script key. The broken-version set is process-wide and the tests share one binary and parallel threads, so each self-check test scripts its own `claude_code_version` that no other test uses (e.g. "2.1.400" and "2.1.401"); the default "2.1.319" is never marked broken. Unit tests of `check` and the broken set use their own instance or distinct versions too: scripted so the second request
  of a conversation reads short, the third request's logged frames are Legacy and exactly one
  warning is emitted; scripted to match, the third stays Native.
- Existing billing-gate, precedence and replay tests pass unchanged.

## Manual Verification

Needs a signed-in subscription and the real CLI, which tests never run by policy.

1. `cargo run -p fontaine-evals -- cache-bench --model claudemax/sonnet --out ../evals/scorecards/claude-subscription-cache-sonnet.json`
   (from `rust/`). Expect the ratio over the middle steps at ≤ 1.25, near 1.05, and the run to exit
   0. Commit the new scorecard and quote the old and new ratios in the PR. Not automated: live
   subscription.
2. On the installed CLI (any version >= 2.1.319), run a multi-step tool loop in the TUI and confirm
   no self-check warning appears; the bench in step 1 is the measurement a warning points to.

## Open Questions

None.
