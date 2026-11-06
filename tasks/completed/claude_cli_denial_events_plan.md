# Claude CLI Denial Events Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/448

## Overview

On Claude CLI 2.1.319 every `claude-subscription` turn in which the model calls a tool fails with
`the Claude CLI sent a system event this adapter does not accept`. The CLI reports the denial of
the model's call (which the adapter relies on) with a `system/permission_denied` event the reader
has no arm for, and it now sends that event and the denial echo **mid-stream**, before the
response's terminal `message_delta`, where the reader's echo rule does not look for them. A
thinking turn is one event away from the same failure (`system/thinking_tokens`). This plan
accepts the denial events by matching them to the tool calls the response actually made, accepts
the thinking estimate, keeps failing closed on every other subtype, and adds the diagnostics whose
absence made the bug take hours instead of a minute: the rejected event named in the error, the
raw line and the accepted-event trail in the debug file, and the failed request in the session
ledger.

## Current State

- `Reader::accept` (`rust/fontaine-core/src/providers/claude_subscription/protocol.rs:616`)
  dispatches on the top-level `type`; an unknown type fails with a fixed message that does not
  name it (`:658`).
- `Reader::system` (`protocol.rs:664`) accepts `init` and `status`, refuses `api_retry`, and
  refuses everything else with a message that does not name the subtype (`:673`).
- `Reader::denial_echo` (`protocol.rs:877`) accepts a `user` event only when
  `self.stopped && stop_reason == "tool_use"` and every block is an `is_error` `tool_result`. It
  does not check which call the result answers.
- `Reader::stream_event`'s unknown-event refusal (`:869`) does not name the inner type either.
- `protocol()` (`protocol.rs:36`) builds a `ClaudeError` of kind `Protocol`. `ClaudeError`'s
  contract (`mod.rs:72`) is that its message is fixed text or text built from names, never raw CLI
  output.
- The transport (`transport.rs:168` for history acks, `:314` for the response stream) passes
  each stdout line to `accept` and turns an `Err` into the request's failure. Nothing logs the
  rejected line or what came before it.
- A failed turn ends as `TurnOutcome::Failed(text)` in `turn::drive` (`rust/fontaine-core/src/turn.rs:427`).
  The only other trace is the `api_error` telemetry event (`turn.rs:617`), which deliberately
  carries no message. The session ledger (`ledger.rs:32`, `Record`) gets nothing, so the
  session's own record says nothing about why a turn failed.
- The debug file (`--debug-file`, `rust/fontaine/src/telemetry/mod.rs:48`) receives every
  `tracing` event at the `FONTAINE_LOG` filter (default `debug`). Export backends only see events
  with target `fontaine::telemetry` (`span_bridge.rs:34`), so a module-target debug event stays
  local and owner-only.
- The test double `rust/fontaine-core/examples/claude_cli_double.rs:373-418` emits a tool call as
  start, deltas, stop, `message_delta`, `message_stop`, `assistant`, then (with `denial_echo`) the
  echo after the stop, which is the pre-2.1.319 order. It never emits `permission_denied`,
  `thinking_tokens` or the tool block's `caller` field.

### What CLI 2.1.319 actually sends

Captured on 2026-11-06 by running `claude` 2.1.319 with the adapter's exact `fixed_args`,
`FIXED_ENV`, private settings and the real `fontaine --internal-claude-inventory` MCP server.
`content_block_delta` lines are omitted:

One tool call (`haiku`):

```
system init · system status(requesting) · message_start
content_block_start {tool_use, id: toolu_A, name: mcp__fontaine__read, caller: {type: direct}}
assistant
system permission_denied {tool_name: mcp__fontaine__read, tool_use_id: toolu_A, decision_reason_type: mode, message: "..."}
content_block_stop
user [{tool_result, tool_use_id: toolu_A, is_error: true, content: "Permission to use ... denied ..."}]
message_delta {stop_reason: tool_use} · message_stop · rate_limit_event
result {subtype: error_max_turns, is_error: true, num_turns: 2}
```

Two parallel calls: each call gets its own `assistant`, `permission_denied` and echo. The
first echo arrives **after the second block has started**, so echoes interleave with open blocks.

A thinking turn: `content_block_start {thinking}` is followed by one or more
`system thinking_tokens {estimated_tokens, estimated_tokens_delta}` before the block stops.

A history replay is acknowledged as today (zero-turn `result`, then a second `system init`).

So the denial events arrive before `message_stop`, while `stopped` is still false. Fixing only
`permission_denied` would move the failure to the echo, which the issue did not anticipate.

### How Hermes handles the same thing

Hermes' `claude-subscription-directsdk` plugin (NousResearch/hermes-official-plugins at
`20048e6`, `directsdk.py:825` and `:936-958`) runs the CLI with the same isolation flags
(`--tools ''`, an inert MCP server, `--permission-mode dontAsk`, `--max-turns 1`). Its read loop
dispatches only on `assistant`, `result` and `stream_event` and **silently drops every other
line**: all `system` subtypes, the `user` echo, rate-limit events. Its checks come afterwards:
exactly one `result`, at least one assistant message, a `message_stop`, and `error_max_turns` only
alongside tool calls. Fontaine keeps its allowlist (see Trade-offs).

## Design

### 1. Track the response's tool calls

`Reader` gains a map from each `tool_use` block id announced in this response to its call state:

```rust
struct Call {
    cli_name: String,   // the name the CLI used: `mcp__fontaine__<alias>` or `EndConversation`
    denied: bool,       // a `permission_denied` was accepted for it
    echoed: bool,       // its error `tool_result` was accepted
}
calls: BTreeMap<String, Call>,
```

`content_block_start` for a `tool_use` block records `(id, cli_name)` both for a restored
Fontaine tool and for a suppressed `EndConversation` block. An id that is missing or already
recorded is a protocol failure ("a tool call has no id" / "the Claude CLI reused a tool call id").
Like today, entries are only created in `Phase::Query`, since stream events are refused in `Ack`.

### 2. `system/permission_denied`

Accepted (`Event::Nothing`) when all of these hold, refused otherwise:

- `tool_use_id` names a recorded call;
- `tool_name` equals that call's `cli_name`;
- the call has not been denied already.

The event's `message` and `decision_reason_type` are neither read nor echoed. Acceptance is
independent of `stopped`, so both the 2.1.319 order and a post-stop order work.

### 3. The denial echo (`user` event)

`denial_echo` drops the `stopped`/`stop_reason` precondition and instead requires that the content
is a non-empty list in which **every** block is a `tool_result` with `is_error: true` whose
`tool_use_id` names a recorded call that has not been echoed yet; each such call is marked
`echoed`. This is stricter than today, which accepted any error result after a tool-use stop
whatever call it named: an echo for a call this response never made, or a second one for the same
call, is refused. A non-error result is still native execution and still refused.

`permission_denied` is **not** a prerequisite for the echo: the minimum supported CLI (2.1.318)
may send only the echo, and the double's existing scripts do.

### 4. Terminal consistency

In `Reader::result`, after the existing checks: if any call was denied or echoed, the response
must have stopped for `tool_use`. A denial next to an `end_turn` stop contradicts the response
and fails with "the Claude CLI's result contradicts its response". This replaces the position
guarantee the old `stopped && stop_reason == tool_use` check gave the echo, at the point where
the stop reason is known.

### 5. `system/thinking_tokens`

Accepted as `Event::Nothing` in `Phase::Query` after `init`. It is a progress estimate carrying no
content and no state the adapter guards. Rig gets real reasoning from the stream's
`thinking_delta`s.

### 6. Every other `system` subtype keeps failing closed

The issue lists the subtypes worth deciding on deliberately. Taking the conservative option for
each:

| Subtype | Decision | Why |
|---|---|---|
| `init`, `status` | accept (unchanged) | |
| `permission_denied` | accept, matched (section 2) | the denial the adapter relies on |
| `thinking_tokens` | accept | estimate only; observed on 2.1.319 thinking turns |
| `api_retry` | refuse (unchanged) | retries are Fontaine's |
| `model_fallback` | refuse (catch-all, named by the event label) | model pinning |
| `compact_boundary` | refuse (catch-all, named by the event label) | compaction is off; history is Fontaine's |
| `hook_*` | refuse (catch-all, named by the event label) | hooks are disabled |
| `task_*` | refuse (catch-all, named by the event label) | background tasks are disabled |
| `informational`, `notification`, `turn_duration`, anything else | refuse, naming the subtype | not observed in the adapter's configuration; their payloads are unqualified |

### 7. Name the rejected event in every `Protocol` failure of `accept`

`Reader::accept` computes the line's **label** once, before dispatch:

- `type`, plus `/subtype` for `system` and `result`, plus `/<event.type>` for `stream_event`
  (e.g. `system/permission_denied`, `stream_event/content_block_start`, `user`).
- Each component passes through `safe_name`: kept only if it is 1-64 characters of
  `[A-Za-z0-9_.-]`, else replaced by `?`. That keeps the `ClaudeError` contract (text built from
  names, never raw output). A missing component renders as `(none)`.

If the dispatched handler returns an error whose `claude_error` kind is `Protocol`, `accept`
returns the same error with ` (event: <label>)` appended to its message. Other errors
(`Truncated`, native auth/limit/server errors) pass through unchanged. `ClaudeError` gets a small
`with_detail(self, detail: &str) -> Self` (appending to the message and keeping the source).
`accept` rebuilds the boxed provider error from it. The non-JSON line failure gets the label
`(not JSON)`.

The transport's refusal of a `Frame` or `Complete` answering a history frame stays unlabelled:
the Ack phase cannot produce one, and `next()`'s read failure has no line.

The catch-all messages keep their wording, and the label supplies the missing name:
`the Claude CLI sent a system event this adapter does not accept (event: system/notification)`.

### 8. Record the rejected line and the trail in the debug file

`Reader` keeps a bounded **trail** of the labels it accepted for the current request: at most 64
entries, consecutive repeats collapsed as `label ×N` (deltas would otherwise fill it), oldest
dropped first with a leading `…`. `Reader::trail(&self) -> String` joins it with ` → `.

The transport has one helper used at both `accept` call sites (`transport.rs:168`, `:314`):

```rust
fn rejected(line: &[u8], reader: &Reader, error: &ProviderError)
```

On any `accept` error whose kind is `Protocol`, it emits one `tracing::debug!` (module target, so
it reaches only the owner-only debug file and never an export backend) with fields:

- `error` (the full chain),
- `trail` (the accepted labels),
- `line` (the raw line, lossy UTF-8, cut at a char boundary to 4 KiB, with `…(N bytes)` appended
  when cut).

The `Reader` stays process-free; only the transport logs. The trail and line are not logged on
success.

### 9. Record failed requests in the session ledger

A new ledger record, written by `turn::drive` when a turn settles as `TurnOutcome::Failed` after
its retries (`turn.rs:427`), for any provider and any agent:

```rust
/// A model request failed and ended the agent's turn. `class` is the stable error class the
/// `api_error` telemetry event carries; `message` is the failure as shown to the user.
RequestFailed {
    agent: String,
    class: String,
    message: String,
},
```

It is appended only when `error_class(&error)` is `Some`: a cancelled turn (the kill hook ends every
killed subagent that way), a memory failure or an unknown tool call is not a request failure and
writes nothing.

- `message` is `failure_text` cut to 2,000 characters. The ledger is local and already holds the
  user's messages verbatim, so a provider message that echoes the request adds no exposure. It is
  never exported (the export reads only the ledger's agent spine).
- Written through `SessionState::append`, so a write failure reaches the user via the existing
  `sink_failed` path.
- Not part of the guardian's view of intent: it joins the no-op arm of the ledger replay
  (`ledger.rs:600-615`).

This covers item 5 of the issue for every provider. Claude Code's transcript likewise records an
API error as a system entry, so this is not a deviation.

## Implementation Steps

1. **Reader call tracking and denial events**: in `protocol.rs`:
   - Add `Call`, `calls`, and the id recording in `content_block_start` (restored and suppressed
     tool blocks).
   - Add the `permission_denied` and `thinking_tokens` arms in `Reader::system`.
   - Rewrite `denial_echo` as in Design 3.
   - Add the terminal-consistency check in `Reader::result`.
2. **Labels and detail**: add `safe_name`, the label computation in `accept`, the
   ` (event: …)` detail on `Protocol` failures, and `ClaudeError::with_detail` in `mod.rs`.
3. **Trail**: add the bounded, repeat-collapsing trail to `Reader`, appended on every accepted
   line, with `trail()`.
4. **Transport logging**: add `rejected(...)` in `transport.rs` and call it at both `accept`
   sites before the error is returned.
5. **Ledger record**: add `Record::RequestFailed` in `ledger.rs` (and its replay arm). Append it in
   `turn::drive` on the final `Failed` return when `error_class` is `Some`, using `error_class`
   and `failure_text`.
6. **Test double**: in `claude_cli_double.rs`, add a script key `denial: "mid_stream"` that emits
   the 2.1.319 order for every scripted call:
   - `content_block_start` with `caller: {type: direct}`, then the deltas, `assistant`,
     `permission_denied`, `content_block_stop` and the echo;
   - after all blocks: `message_delta`, `message_stop`, `rate_limit_event`, and
     `result error_max_turns` with `num_turns: 2`.

   Every echo, the existing `denial_echo: true` one included, and the new `permission_denied` use
   each scripted call's `id` (read as at `:384`, defaulting to `toolu_double`) instead of the
   hard-coded `toolu_double`, because the reader now refuses an echo naming no recorded call.

   Add a `thinking_tokens: true` key that inserts one `system/thinking_tokens` line inside the
   existing `thinking` block. Keep the existing `denial_echo: true` behavior (post-stop order) for
   2.1.318. Add both new keys (`denial`, `thinking_tokens`) to the doc comment listing the script
   keys at the top of the file.
7. **Tests**: see Testing Strategy.
8. **Docs**: see Documentation.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`
- `rust/fontaine-core/src/providers/claude_subscription/protocol/tests.rs`
- `rust/fontaine-core/src/providers/claude_subscription/mod.rs` (`ClaudeError::with_detail`)
- `rust/fontaine-core/src/providers/claude_subscription/transport.rs`
- `rust/fontaine-core/src/ledger.rs`
- `rust/fontaine-core/src/turn.rs`
- `rust/fontaine-core/examples/claude_cli_double.rs`
- `rust/fontaine/tests/claude_subscription.rs`
- `docs/claude-subscription.md`, `docs/architecture.md`
- `CHANGELOG.md`

## Trade-offs

- **Ignore unknown events (Hermes) vs. keep the allowlist.** Hermes drops every line that is not
  `assistant`, `result` or `stream_event`, which makes it immune to new CLI subtypes. But it then
  cannot detect a model fallback, compaction, hooks, background tasks or a real native tool run,
  and those detections are what back the adapter's "the CLI holds no conversation and executes no
  tool" guarantee. Fontaine keeps failing closed and makes each new failure a one-minute
  diagnosis instead (Design 7-9).
- **Match denials to call ids vs. keep a position rule.** The issue proposed accepting
  `permission_denied` only after a `tool_use` stop. The capture shows both denial events arrive
  before the stop, so no position rule works across 2.1.318 and 2.1.319. Matching the id and the
  CLI tool name to a call the response announced is order-independent and stricter than the old
  position rule.
- **Logging every accepted event vs. a trail logged on failure.** Logging each line at debug
  would flood the debug file with deltas on every request. A bounded, collapsed trail emitted
  only with a rejection gives the position of the bad event at no cost on success.
- **Ledger vs. `Approver::warn`** for the failed request. The failure is already shown to the
  user as the turn's error. What was missing is the durable session record, which the ledger is.
  A ledger record also covers every provider, not just this adapter.

## Decisions

- 2026-11-06 AFK: the uncommitted working-tree change that appended the subtype to the system-event error was stashed (`ship-auto: WIP set aside`), not committed. Design 7 supersedes it with a sanitized label on every `Protocol` failure.
- 2026-11-06 AFK: of the subtypes the issue lists, only `thinking_tokens` is accepted (observed on 2.1.319). `informational`, `notification` and `turn_duration` stay refused until a capture shows them in the adapter's configuration (conservative option).
- 2026-11-06 AFK: the "How Hermes handles the same thing" subsection stays, not folded into Trade-offs. The user asked that the design refer to how Hermes does the same thing, and the subsection is that reference; its overlap with the first Trade-offs bullet costs nothing (conservative option).

## Documentation

- `docs/claude-subscription.md`:
  - "What a request is": the CLI's report of the denied call (`permission_denied` and the error
    `tool_result`) is accepted only for a call the response made.
  - "Failures": a protocol failure names the rejected event, and with `--debug-file` the
    debug file holds the rejected line (cut to 4 KiB) and the events accepted before it.
  - "Supported CLI versions": 2.1.319's mid-stream denial order is supported.
- `docs/architecture.md` session-ledger bullet: add `request_failed`, a model request that ended
  an agent's turn, with its error class and message.
- `CHANGELOG.md`: a `### Fixed` entry under `[Unreleased]` citing #448.

## Testing Strategy

Unit tests in `protocol/tests.rs` (all reader logic is reachable with constructed lines):

- **Regression for #448**: the exact 2.1.319 single-call sequence from Current State (including
  `assistant`, `permission_denied` before `content_block_stop`, the echo before `message_delta`,
  `rate_limit_event` and `result error_max_turns` with `num_turns: 2`) completes with
  `Event::Complete`.
- Two parallel calls with interleaved denials and echoes complete.
- The pre-2.1.319 order (echo after `message_stop`, no `permission_denied`) still completes, as in
  `tool_names_are_restored_and_the_stop_may_be_max_turns`, whose echo gains `"tool_use_id": "t"`
  (block "t") so it names the recorded call.
- `permission_denied` refused when:
  - its `tool_use_id` is unknown;
  - its `tool_name` differs from the call's CLI name;
  - it is a second denial for the same call;
- Echo refused when:
  - it answers an unknown id;
  - it echoes a call a second time;
  - one block is not an error;
  - the content is empty.
- A denial with an `end_turn` stop fails at the result.
- A suppressed `EndConversation` call's denial and echo are accepted.
- A reused or missing tool block id is refused.
- `thinking_tokens` accepted in a thinking response; refused in `Ack`.
- A `model_fallback` refusal names `system/model_fallback`; `notification` is refused with the
  catch-all.
- Error text: the catch-all names `(event: system/notification)`, and an unknown stream event
  names `(event: stream_event/<type>)`. A subtype that is too long or has disallowed characters
  renders as `?`. The non-JSON line reads `(event: (not JSON))`. A `Truncated` failure is not
  relabelled.
- Trail: repeats collapse to `×N`, and the cap drops the oldest entries with a leading `…`.

Integration tests in `rust/fontaine/tests/claude_subscription.rs` (headless `fontaine -p` with
the double):

- **End-to-end regression for #448**: the existing read scenario with `denial: "mid_stream"`
  runs the read through Fontaine's gate, and the next request's answer is the result. This tier
  also pins what unit tests cannot: rig's decoder accepting a tool block carrying `caller`, and
  the transport's handling of mid-stream non-frame events.
- A thinking turn scripted with `"thinking": {...}` and `thinking_tokens: true` answers.
- With `--debug-file`, an `after_stop` line of `{"type":"system","subtype":"notification", "marker":"UNIQUE-RAW"}`
  fails the run. The error names `system/notification`, the debug file contains `UNIQUE-RAW` and
  a trail ending in `stream_event/message_stop → assistant`, and the session's `ledger.jsonl` holds a
  `request_failed` record naming the root agent. The debug file and the ledger are silent sinks:
  nothing would notice them breaking, so they get automated coverage.

## Manual Verification

1. From `rust/`, with a `claudemax` account configured:
   `cargo run -p fontaine -- -p "Use the read tool to read LICENSE, then say the first line." --model claudemax/opus`.
   Expected: exit 0 and the license's first line. This is the issue's live reproduction. It needs
   a real subscription login, which tests never use.
