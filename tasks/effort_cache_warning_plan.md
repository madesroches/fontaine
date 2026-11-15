# /effort Cache-Cost Warning Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/632

## Overview

Changing the reasoning effort mid-session can invalidate the provider's prompt cache (Anthropic lists
`output_config.effort` changes as able to invalidate the messages cache, and on some models the tool and
system caches). Nothing tells the user. `/effort <level>` will say so once, when the main model's account
caches prompts and the conversation already has content, and `docs/tui.md` will document it.

## Current State

- `fontaine-tui/src/ui/commands/model.rs` `effort()` sets `runtime.state.set_effort(level)`, then prints
  `effort is now <level> for this session`. Generation options are rebuilt per turn, so the change takes
  effect on the next request.
- Whether the main account caches is `PromptCaching::of(account).enabled`
  (`fontaine-core/src/providers/mod.rs`), resolved from `account.prompt_caching` or the kind's default.
  `ModelHandle` carries `kind` but not the resolved setting.
- `docs/tui.md` (`/effort` row, and the model-choosing paragraph) says nothing about caching.

## Design

Chosen option: mention the cost in `/effort` output (the issue's first option).

- Expose whether the main model's requests carry cache markers: add a `pub prompt_caching: bool` field like `quota`
  (default `false` in `ModelHandle::new`), set in `resolve_main_model` (`fontaine-core/src/models.rs`, where
  `main_account` is in hand) as `kind.provider().accepts_prompt_caching() && PromptCaching::of(account).enabled`.
  Both switch paths reach `resolve_main_model` through `Switcher::main`, so a `/model` switch refreshes it with no further change.
- History check: reuse the existing `AgentRuntime::conversation_len()` (async, `Result<usize, SessionError>`);
  history means `> 0`. No new accessor, no token estimate.
- `effort()` becomes `async` and the `Command::Effort` arm in `ui/mod.rs` awaits it. A failed
  `conversation_len()` skips the cache notice and is reported with `self.app.transcript.warning(chain(&e), None)`.
- In `effort()`, after setting a level that differs from the previous one, if
  `runtime.models.main.prompt_caching && history`, append a second notice:
  `the next request may re-read the conversation uncached: a changed effort can invalidate the prompt cache`. Emit nothing when the level is
  unchanged, caching is off, or the conversation is empty.
- The pure decision lives in a small function `effort_cache_warning(previous, new, caching, has_history)
  -> Option<&'static str>` so it is unit-testable without a runtime.

## Implementation Steps

1. `fontaine-core/src/models.rs`: add `prompt_caching` to `ModelHandle`, set it in `resolve_main_model`.
2. `fontaine-tui/src/ui/commands/model.rs` and `ui/mod.rs`: add `effort_cache_warning`, make `effort()` async and call it.
3. Docs and changelog.

## Files to Modify

- `rust/fontaine-core/src/models.rs`
- `rust/fontaine-tui/src/ui/commands/model.rs` (with an inline `#[cfg(test)]` module for the `effort_cache_warning` unit tests), `rust/fontaine-tui/src/ui/mod.rs`
- `rust/fontaine-tui/tests/common/mod.rs`, `rust/fontaine-tui/tests/tui.rs`
- `docs/tui.md`, `CHANGELOG.md`

## Trade-offs

- Compaction-deferred apply: more machinery and a new setting for a one-line problem.
- Doc only: users hit the cost before reading it.
- Token-count threshold for "non-trivial": adds a magic number; any completed turn already has a cache
  entry worth knowing about.

## Documentation

`docs/tui.md`: extend the `/effort` row and the paragraph at the model-choosing section to say a
mid-session change can invalidate the prompt cache on accounts that cache, and that the command warns.

## Testing Strategy

- Unit tests for `effort_cache_warning`: warns only when level changed, caching on, history present;
  silent for each other combination.
- A TUI test in `fontaine-tui/tests/tui.rs` style: `/effort high` on a session with a prior turn on a
  caching account (`World` gains a `with_prompt_caching()` builder whose flag `setup()` applies to the main handle) shows the warning; on an empty session or non-caching account does not.

## Decisions

- Which models render effort ahead of the tools and system prompt is unmeasured (`cache-bench`, #498
  could measure it); the notice wording stays hedged until then.
- The notice can over-warn on opencode-go chat-route models, which send no cache markers; accepted.
