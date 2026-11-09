# claude-subscription Native Web Search Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/522

## Overview

Fontaine users on their personal Claude plan (`claude-subscription` accounts) get no web search
unless docker/podman or a `[websearch] backend` is available, and see a startup warning saying so,
even though the model behind the subscription can search. This plan offers the Anthropic
`web_search_20250305` server tool on `claude-subscription`, exactly as `anthropic` accounts get it,
by letting that one server tool through the CLI adapter, which today refuses all server-tool
traffic and fails closed. Everything else the adapter refuses stays refused.

## Current State

- `Provider::native_web_search` (`rust/fontaine-core/src/providers/mod.rs:87-91`) defaults to
  `None`; only `anthropic` overrides it (`providers/anthropic.rs:29-32`) with
  `{"type": "web_search_20250305", "name": "web_search"}`. `ClaudeSubscription`
  (`providers/claude_subscription/mod.rs:168`) does not, and has no `default_endpoint`.
- `native_search::provider_entry` (`rust/fontaine-core/src/native_search.rs:46-60`): under
  `native = "auto"` it requires `provider.default_endpoint()?` even when the account sets no
  `base_url`, so a kind without an endpoint of its own could never pass `auto`.
- `agent/build.rs:364-367` puts the offered entry into the request as
  `additional_params({"tools": [entry]})`. The `claude-subscription` model is rig's Anthropic
  Messages client over a subprocess transport, so the entry arrives in the encoded Messages body's
  `tools` array, which `protocol::translate` (`providers/claude_subscription/protocol.rs:279`)
  reads.
- The adapter refuses server tools at three points, all deliberately fail-closed:
  - request: `tool_definitions` (`protocol.rs:394-431`) refuses any entry whose `type` is not
    `custom` or that lacks `input_schema` ("a hosted tool definition cannot be sent through the
    Claude CLI");
  - history: `assistant_content` (`protocol.rs:560-590`) accepts only `text`, `thinking`,
    `redacted_thinking` and `tool_use` blocks;
  - stream: `Reader::stream_event`'s `content_block_start` (`protocol.rs:1015-1073`) accepts only
    `text`, `thinking`, `redacted_thinking` and `tool_use`; anything else is "the Claude CLI used
    a native tool or hosted content".
- `translate` already writes a raw `tools` array into `CLAUDE_CODE_EXTRA_BODY` in the child's
  settings `env` (`protocol.rs:343-373`); the Fontaine tools are also listed in the inert MCP
  manifest so the CLI knows their names.
- `Expect` (`protocol.rs:595-598`) carries the models and aliases a response must match; it is
  built in `transport.rs:217-221` from `Translated`.
- `Reader::result` (`protocol.rs:1167-1240`) accepts `success`, or `error_max_turns` after a
  `tool_use` stop; a second `message_start` is "a second response" (`protocol.rs:986-990`).
- rig 0.46.0's Anthropic stream decoder already handles `server_tool_use`,
  `web_search_tool_result` and `citations_delta` (the `anthropic` kind uses it today) and maps
  `pause_turn` to `FinishReason::Stop` (`rig-core/src/providers/anthropic/streaming.rs:162-164`).

### Live qualification (Claude CLI 2.1.319, Fontaine's exact `fixed_args`)

Done by hand before this plan, with a scratch settings file whose `CLAUDE_CODE_EXTRA_BODY` was
`{"tools":[{"type":"web_search_20250305","name":"web_search","max_uses":3}]}`:

- **One response, search inside it.** The stream was `message_start`, `server_tool_use`
  (`srvtoolu_…` id, `name: "web_search"`, `input_json_delta` carrying `{"query": …}`),
  `web_search_tool_result` (`tool_use_id` = that id, `content` a list of `web_search_result` with
  `encrypted_content`), `thinking`, then `text` blocks (some with `citations: []` and
  `citations_delta` of type `web_search_result_location`), `message_delta` with `end_turn`,
  `message_stop`, then a `success` result with `usage.server_tool_use.web_search_requests: 1`
  (and `modelUsage.<id>.webSearchRequests: 1`). One `message_start`, no other request.
- **The CLI runs nothing itself.** `system/init` reported `tools: []`; the search is the API's
  server tool, not the CLI's own `WebSearch`. No `user` frame, no `permission_denied`.
- **Replay.** A second process given that assistant message (all eight blocks, encrypted content
  included) as history, the earlier user frame with `shouldQuery: false`, and a new querying user
  frame answered from the earlier results with one request and `web_search_requests: 0`.
- The rate-limit frame reported `overageStatus: "rejected"` while the search ran, so the search
  was drawn from the plan, not paid credits.

## Design

### Provider

`ClaudeSubscription::native_web_search()` returns the same entry as `anthropic`. Both build it
from one shared function in `providers/mod.rs` (e.g. `pub(crate) fn anthropic_web_search() ->
Value`) holding the type and name, so the two kinds and the adapter cannot drift; the adapter
matches on the same two constants (`WEB_SEARCH_TYPE = "web_search_20250305"`,
`WEB_SEARCH_NAME = "web_search"`), exported next to it.

### `native_search::provider_entry`

Under `auto`, an account with no (or empty) `base_url` is the kind's own endpoint, so it is
accepted before `default_endpoint` is consulted; `default_endpoint()` is needed only to compare a
set `base_url`. `claude-subscription` refuses `base_url` (`ClaudeSubscription::validate` refuses it), so it always
passes `auto`. `off` and `on` are unchanged, and so is every gate in `offered` (deny/ask rules,
covering hooks, the `allow` rule in `manual`/`acceptEdits`/`dontAsk`).

### `protocol.rs`

1. **Request.** `tool_definitions` returns the custom definitions plus the accepted web-search
   entry itself (`Option<Value>`). A hosted entry is accepted only when its `type` is
   `WEB_SEARCH_TYPE`, its `name` is `WEB_SEARCH_NAME`, and it appears once; any other hosted
   entry keeps today's refusal. `translate` appends that entry as-is, unaliased, to the
   extra body's `tools` after the aliased custom tools (creating the array if there are none). It
   is not added to the MCP manifest or the aliases. `Translated` gains `web_search: bool`, true when that entry is `Some`.
   A `tool_choice` naming `web_search` stays refused (it is not an alias), as nothing in Fontaine
   sends one.
2. **History.** `assistant_content` also accepts `server_tool_use` blocks whose `name` is
   `WEB_SEARCH_NAME` and `web_search_tool_result` blocks, passed through unchanged except for the
   existing `cache_control` strip. A `server_tool_use` with any other name stays refused.
   `history_tool_names` keeps reading only `tool_use`, so server calls never get aliases.
3. **Stream.** `Expect` gains `web_search: bool` (set in `transport.rs` from `Translated`). The
   `Reader` gains a `searches: BTreeSet<String>` of server call ids. In `content_block_start`:
   - `server_tool_use` is accepted only when `expect.web_search`, the `name` is
     `WEB_SEARCH_NAME`, and the id is non-empty and new (among `calls` and `searches`); the id is
     recorded in `searches`;
   - the `tool_use` branch, which checks only `calls` today, also refuses an id already in
     `searches`;
   - `web_search_tool_result` is accepted only when its `tool_use_id` is in `searches`;
   - both open the block like any other (so the existing delta/stop bookkeeping applies) and are
     forwarded unchanged; rig decodes them as it does for `anthropic`.
   Every other block type keeps the "native tool or hosted content" refusal. Deltas
   (`input_json_delta`, `citations_delta`) need no change: they are forwarded for any open block.
4. **Stops.** No change. `pause_turn` is accepted with a `success` result like `end_turn`, which
   is what rig and the `anthropic` kind already do. If the CLI ever continued a paused turn on
   its own, the `message_delta` with the stop reason has already set `terminal`, so its second
   `message_start` fails closed as "the Claude CLI sent output after the response ended".

Server calls are never added to `calls`, so `permission_denied`, the denial echo and the
`error_max_turns` check (which need a `tool_use` stop) are unaffected: a CLI-reported denial or
echo for a server call id is still refused.

### What the guardian sees

Nothing new: provider-native search on `claude-subscription` has the same properties as on
`anthropic` (runs inside the response, outside the guardian, the result scanner, hooks and the
per-call prompt), and is gated by the same request-time conditions in `native_search::offered`.
The 2026-10-26 `### Tools` Decision already records and justifies that; this plan adds the kind.

## Implementation Steps

1. `providers/mod.rs`: add the shared web-search entry function and the two name/type constants;
   `providers/anthropic.rs` uses it.
2. `providers/claude_subscription/mod.rs`: implement `native_web_search` with it.
3. `native_search.rs`: accept a missing/empty `base_url` before `default_endpoint` under `auto`.
4. `providers/claude_subscription/protocol.rs`: request, history and stream changes above;
   `Translated::web_search`, `Expect::web_search`, `Reader::searches`.
5. `providers/claude_subscription/transport.rs`: pass `translated.web_search` into `Expect`.
6. `examples/claude_cli_double.rs`: leave hosted extra-body entries (a `type` other than
   `custom`) out of the `system/init` `tools` it builds from `CLAUDE_CODE_EXTRA_BODY.tools`, as
   the real CLI reports `tools: []` for the server tool; `validate_init` refuses `web_search`.
7. Tests (see Testing Strategy).
8. Docs (see Documentation).
9. Master plan `tasks/fontaine_harness_plan.md`: Decision line at the end of `### Providers`, and
   amend the 2026-10-26 `### Tools` line in place ("first release: Anthropic accounts on the
   Anthropic API") to add `claude-subscription` accounts, marked "amended <date> (#522)".
10. `CHANGELOG.md`: `## [Unreleased]` `### Added` entry (#522).

## Files to Modify

- `rust/fontaine-core/src/providers/mod.rs`
- `rust/fontaine-core/src/providers/anthropic.rs`
- `rust/fontaine-core/src/providers/claude_subscription/mod.rs`
- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`
- `rust/fontaine-core/src/providers/claude_subscription/protocol/tests.rs`
- `rust/fontaine-core/src/providers/claude_subscription/transport.rs`
- `rust/fontaine-core/src/native_search.rs`
- `rust/fontaine-core/examples/claude_cli_double.rs`
- `rust/fontaine-core/tests/native_search.rs`
- `rust/fontaine-core/tests/claude_subscription.rs`
- `docs/tools.md`, `docs/claude-subscription.md`, `docs/configuration.md`
- `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

## Trade-offs

- **The API's server tool via the extra body** (chosen) over **the CLI's own `WebSearch` tool**:
  `WebSearch` is a client-side tool the CLI executes by making a second model request, so it would
  break "the CLI executes no tool", run a second request under `--max-turns 1`, and leave a
  CLI-run `tool_use`/`tool_result` pair in history. The server tool runs inside the one response,
  exactly as on `anthropic`.
- **Accept exactly one server tool** over a general "hosted tools pass through" switch: each
  server tool has its own block types and replay semantics that need their own qualification;
  the adapter's fail-closed default stays for all of them.
- **Gate stream acceptance on the request having offered search** over accepting
  `server_tool_use` unconditionally: a server call the request did not offer means the CLI or its
  settings changed what was sent, which is the class of drift the adapter exists to catch.
- **History acceptance not gated on the current request** offering search: a mode or rule change
  can withdraw search between turns, and the earlier, already-run search must still replay.

## Decisions

Master-plan line to add (dated at implementation) at the end of `### Providers`:

- `claude-subscription` offers provider-native web search like `anthropic`: the
  `web_search_20250305` entry rides `CLAUDE_CODE_EXTRA_BODY`, and the adapter accepts exactly that
  server tool (its request entry, `server_tool_use` named `web_search` only when offered, a
  `web_search_tool_result` only for a search of the same response, and both in replayed history);
  every other hosted tool or block stays refused. Qualified live on CLI 2.1.319: one request, no
  CLI-run tool, lossless replay (#522).

## Documentation

- `docs/tools.md` `## Web search`, item 1: the provider's search is available for `anthropic`
  accounts on the Anthropic API and for `claude-subscription` accounts.
- `docs/claude-subscription.md`: a short `## Web search` section (provider search is offered as
  for `anthropic`, under the same `[websearch] native` and permission conditions, link to
  `tools.md#web-search`), and in "What a request is" the refused-history list says "a hosted tool
  other than web search" and the stream sentence says the provider's web search is the one server
  tool accepted.
- `docs/tools.md` `## Web search`: scope the sentence "If the endpoint rejects the request because
  of the search tool, the failure message says to set `native = \"off\"`" to `anthropic` accounts,
  and say that on `claude-subscription` a rejection is reported as a plain CLI error, for which
  `native = \"off\"` is the remedy. Item 2 ("every other account kind uses, and so does an
  `anthropic` account when the provider's search is not offered") also names a
  `claude-subscription` account.
- `docs/configuration.md`, the `native` row (about line 510): `auto` applies to `anthropic`
  accounts on the Anthropic API and to `claude-subscription` accounts; name both.

## Testing Strategy

Unit tests in `protocol/tests.rs`, with stream literals trimmed from the captured CLI output:

- Request: a body with custom tools plus the web-search entry → the extra body's `tools` ends
  with the entry unchanged and unaliased, the manifest and aliases list only the custom tools,
  `web_search` is true. The entry alone (no custom tools) still yields a `tools` array. Refused:
  another hosted type, `web_search_20250305` under another name, the entry twice. The existing
  `what_cannot_be_replayed_without_loss_is_refused` assertions that the `web_search_20250305`
  entry is refused ("hosted tool") and that an assistant `web_search_tool_result` is refused
  ("without loss") are retargeted to an unsupported hosted type (e.g. `code_execution_20250522`)
  and a `server_tool_use` with another name.
- History: an assistant message holding `server_tool_use` + `web_search_tool_result` + cited
  text replays unchanged; a `server_tool_use` with another name is refused.
- Stream, with search offered: the captured sequence (server call with `input_json_delta`,
  result, thinking, cited text with `citations_delta`, `end_turn`, `success` result) is accepted
  and forwarded in order; a `pause_turn` stop with a `success` result completes.
- Stream refusals: `server_tool_use` when search was not offered; under another name; a reused id
  (including one reused from a `tool_use`, and a `tool_use` reusing a `server_tool_use` id); a
  `web_search_tool_result` with an unknown `tool_use_id`; a `permission_denied` or echo naming a
  server call id; a second `message_start` after a `pause_turn` stop ("output after the response
  ended"). The new refusals keep the existing "the Claude CLI used a native tool or hosted
  content" message.

`native_search.rs`/`tests/native_search.rs`: a `claude-subscription` main model under `auto`
gets the entry on the request (the integration test asserts `runtime.native_search_active()` and
the entry on the sent tools; `provider_available` being true for the kind is asserted in a unit
test in `native_search.rs`'s own `mod tests`, as it is `pub(crate)` and `build_with_models` uses a
`DenyAll` approver); `off` withholds it; the existing deny/ask/hook/mode gates hold for the kind.

Integration (`tests/claude_subscription.rs`, through `claude_cli_double`): a request carrying the
entry reaches the CLI's settings `CLAUDE_CODE_EXTRA_BODY` `tools`, and a scripted response with a
search decodes through rig into the assistant content (server call, result, cited text). This
covers the wiring from rig's encoded body through the subprocess transport, which unit tests of
`translate` and `Reader` alone cannot. The double must not announce `web_search` in init `tools`
(see Implementation Steps).

## Open Questions

None.
