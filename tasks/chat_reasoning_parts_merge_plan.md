# Chat Route: Merge Streamed Reasoning Content Parts Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/567

## Overview

Some Chat Completions hosts stream Claude reasoning as `content` parts of
`{"type": "reasoning", "summary": [{"type": "summary_text", "text", "signature"}]}` instead of
`reasoning` / `reasoning_details` fields. The first chunk is an empty placeholder and the signature
comes in a later chunk. rig keeps each chunk as its own content part and replays them all on the
next request, so the host gets a `thinking` block with no thinking and no signature and returns
`400 each thinking block must contain thinking`. Any turn that thinks and then calls a tool breaks the
next request. Fontaine will fix the outgoing request: the streamed fragments of one reasoning part
get merged back into a single part, and a part left with no text and no signature gets dropped.
The rewrite is added to the two kinds the issue names, `openrouter` and `openai-compatible`.

## Current State

- **rig decoder** (`rig-core =0.46.0`, `providers/openai/wire/chat.rs`): `ChatDecoder::delta`
  sends an array `content` through `ChatDecoder::part`. `Part::of` knows `text`, `refusal`,
  `thinking` and `image_url`. A `reasoning` part becomes `Part::Unknown`, and every chunk is kept
  as a separate `Block::Opaque { replay: true }` that holds the part exactly as it arrived.
  `merge_details` merges `reasoning_details` fragments (a fragment continues the previous entry of
  the same type/index, appends `text`, fills missing fields). Content parts get no such merge.
- **rig encoder** (`Chat::assistant`, same file): a replayable opaque that has a `type` is pushed
  into the assistant message's `content` array in order. That is how the replayed message ends up
  with two `reasoning` parts, the empty placeholder first.
- **Fontaine wiring**:
  - `rust/fontaine-core/src/providers/openrouter.rs:67-80`: `Connected::model` wraps the
    `Chat` wire in `CacheWire`.
  - `rust/fontaine-core/src/providers/cache_wire.rs:98-147`: `CacheWire` is a `Wire` wrapper. It
    forwards decode/reassembly and rewrites the encoded JSON body (`Body::Bytes`) after the inner
    `encode`. That is the pattern to reuse.
  - `rust/fontaine-core/src/providers/openai_compatible.rs:85-87`: returns
    `self.client.completion(model).erase()` with no wrapper.
- **Test harness**: `rust/fontaine/tests/common/mod.rs` runs the `fontaine` binary against a
  scripted `wiremock` Chat endpoint (`Turn`, `chat_response`, `Endpoint::requests()` returns every
  request body). `Sandbox::compatible` sets up an `openai-compatible` account.

## Design

### Where the fix lives

The fix goes in Fontaine's request path, as a new `Wire` wrapper modelled on `CacheWire`.

### New module `rust/fontaine-core/src/providers/reasoning_parts.rs`

```rust
/// Merges the streamed fragments of a Chat `reasoning` content part back into one part before
/// replay; see `merge_reasoning_parts`.
#[derive(Clone)]
pub(super) struct ReasoningPartsWire<W> { inner: W }

impl<W> Wire for ReasoningPartsWire<W> where W: Wire<Op = Completion, Payload = Encoded> {
    // describe / decoder / reassembler forward to `inner`, as in CacheWire.
    fn encode(&self, request, mode) -> Result<Encoded, EncodeError> {
        let mut encoded = self.inner.encode(request, mode)?;
        // Body::Bytes only, and only when the bytes contain `"type":"reasoning"` (rig serializes
        // compact JSON), so a Chat request without reasoning parts is never parsed. Parse; if
        // merge_reasoning_parts(&mut body) changed anything, re-serialize. If the body is not
        // JSON, leave it as it is.
        Ok(encoded)
    }
}

/// For every `role: "assistant"` message whose `content` is an array: merge each run of
/// fragments of one reasoning part, then drop reasoning parts that carry nothing.
/// Returns whether the body changed.
pub(super) fn merge_reasoning_parts(body: &mut Value) -> bool
```

**Merge rule** (it mirrors rig's `merge_details`):

- A *reasoning part* is an object with `"type": "reasoning"`.
- A part is *signed* when any `summary[]` entry has a non-empty `signature`, or the part has a
  non-empty top-level `signature` or `encrypted_content`.
- Walk the `content` array in order. If a reasoning part comes directly after a reasoning part
  that is **not yet signed**, it continues that part:
  - Its `summary` entries merge position by position. A string `text` is appended. Any other
    field that the earlier entry is missing (absent, `null` or `""`) is filled in, which is how
    `signature` arrives. Extra entries are appended.
  - Top-level fields that the earlier part is missing are filled in the same way.
- A reasoning part after a **signed** part starts a new part. Several complete thinking blocks
  (interleaved thinking) therefore stay separate.
- Once merging is done, drop any reasoning part that is neither signed nor has non-empty `text`
  in any summary entry. This is the issue's fallback rule, and it also covers a stray placeholder
  that was not followed by a fragment.
- If dropping leaves `content` empty, remove the `content` key when the message has
  `tool_calls`; otherwise remove the whole assistant message from `messages`, matching rig's own
  rule of never sending a message with neither.
- Other part types and other messages are not touched.

Result for the issue's stream: one part
`{"type":"reasoning","summary":[{"type":"summary_text","text":"","signature":"CAQ…"}]}`, which
the issue reports the host accepts.

### Wiring

- `openrouter.rs`: `CacheWire::new(ReasoningPartsWire::new(chat), …)`. The reasoning rewrite
  runs inside, and `CacheWire` marks the result.
- `openai_compatible.rs`: `let routed = self.client.completion(model);
  Model::new(ReasoningPartsWire::new(routed.wire), routed.transport).erase()`. `OpenAiWire`
  implements `Wire<Op = Completion, Payload = Encoded>`. `openai-compatible` always uses the Chat
  route: the GATEWAY dialect has no route hook and defaults to `Route::Chat`.
- `providers/mod.rs`: `mod reasoning_parts;`.

## Implementation Steps

1. Create `rust/fontaine-core/src/providers/reasoning_parts.rs` with `ReasoningPartsWire` and
   `merge_reasoning_parts` as specified, plus unit tests (see Testing). Register it in
   `providers/mod.rs`.
2. Wrap the Chat wire in `openrouter.rs` (`Connected::model`).
3. Wrap the routed wire in `openai_compatible.rs` (`Connected::completion`). `rig_core::Model`
   must be imported there.
4. Add the end-to-end regression test in `rust/fontaine/tests/` with a new scripted `Turn`
   variant in `rust/fontaine/tests/common/mod.rs` (see Testing). The new
   `Turn::ContentReasoningThenTool` variant also needs an arm in `chat_json` (non-streamed
   reply) and in `responses_response`, which it reaches through `Turn::answer()`'s wildcard; or
   map it in `Turn::answer()` to the equivalent `Turn::Tool`.

## Files to Modify

- `rust/fontaine-core/src/providers/reasoning_parts.rs` (new)
- `rust/fontaine-core/src/providers/mod.rs`
- `rust/fontaine-core/src/providers/openrouter.rs`
- `rust/fontaine-core/src/providers/openai_compatible.rs`
- `rust/fontaine/tests/common/mod.rs`
- `rust/fontaine/tests/<new or existing headless test file>.rs`

## Trade-offs

- **Fix in rig's decoder (a `Part::Reasoning` that merges like `merge_details`)**: the right
  place in the long run, but rig is pinned at `=0.46.0` and is not edited from here. The decoder
  fix would also not repair sessions saved before it. A Fontaine-side request rewrite fixes both
  live and resumed conversations now, and can be removed once rig merges these parts itself.
- **Drop-only (no merge)**: smaller, but if a host also streams thinking text over several
  chunks, that text stays split into one part per chunk, and only the last one is signed. Merging
  keeps one signed block per thinking block, which is what the host expects.
- **Put the rewrite in `CacheWire`**: `openai-compatible` does not use `CacheWire`, and caching
  is unrelated, so a separate single-purpose wrapper is clearer (open/closed) and can be reused
  by other Chat-route kinds.
- **Scope**: only `openrouter` and `openai-compatible`, the kinds the issue names. Other Chat
  users (`opencode-go`'s Chat wire, `kimi`) can add the wrapper if they ever hit this.

## Documentation

No user-facing doc change: there is no new config or behavior beyond the fix. The `pr` stage adds
the CHANGELOG entry.

## Testing Strategy

**Unit tests** (in `reasoning_parts.rs`), on `merge_reasoning_parts` with JSON bodies:
- The issue's exact pair (empty placeholder, then a signed empty-text part) becomes one part with
  the signature. Other parts and `tool_calls` are unchanged.
- Text fragments over several unsigned parts followed by a signed part become one part with the
  joined text and the signature.
- Two signed parts one after the other stay two parts (interleaved thinking).
- A lone empty unsigned part is dropped. A lone unsigned part that has text is kept.
- A placeholder separated from the signed part by a text part is dropped, and the signed part is
  kept.
- A message whose only content was a dropped placeholder loses its `content` key when it has
  `tool_calls`.
- A message with no `tool_calls` whose content was only a dropped placeholder is removed from
  `messages`.
- User messages, string `content`, and a body with no reasoning parts are unchanged, and the
  function returns `false` for them.
- At the wire level: `ReasoningPartsWire<Chat>` encodes a `CompletionRequest` whose assistant turn
  holds two replayable `Opaque` reasoning items (placeholder, then signed), and the encoded body
  has one reasoning part. `Completion::prepare` runs `history::adapt_for`, which drops opaques
  whose `origin` does not match the target, and `AssistantMessage::new` leaves `origin: None`; so
  the hand-built assistant turn must set `origin` to an `Origin` matching the Chat wire's
  api/provider/model (rig completion/message/native.rs), or the test calls `wire.encode`
  directly without `prepare`. A body without `"type":"reasoning"` comes out byte-identical.

**End-to-end regression test** (pins issue #567, the bug seen in the wild). It goes in
`rust/fontaine/tests/`: add a `Turn::ContentReasoningThenTool` variant to `common/mod.rs` that
streams the issue's frames (placeholder reasoning part, signed reasoning part, a text delta, a
tool call, `finish_reason: "tool_calls"`). Run the headless binary for that turn followed by a
`Turn::text`, then check that the **second** request in `Endpoint::requests()` has exactly one
`reasoning` part in its assistant message, carrying the signature. Run it for
both kinds. `openai-compatible` uses `Sandbox::compatible`. `openrouter` uses `Sandbox::new()` plus
`user_config(...)` with an account of `kind = "openrouter"`, `base_url = endpoint.url()`, and an
`api_key_env` whose variable is set via `set_env` (patterns:
`rust/fontaine/tests/opencode_go_headless.rs:19-22`, `Sandbox::priced` in `common/mod.rs`). This
covers what the unit tests cannot: the real rig
decode → store → replay path through the provider's wiring.

## Manual Verification

Optional. Re-run the issue's reproduction against the real host. It isn't automated because it
needs a real Databricks workspace and token.
