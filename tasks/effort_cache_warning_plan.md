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

Chosen option: mention the cost in `/effort` output (the issue's first option). Not applying changes at
compaction (a new setting, and surprising) and not doc-only (the user finds out too late).

- Expose the resolved caching on the handle: add `prompt_caching: bool` (default `false` in
  `ModelHandle::new`), set where the handle is built from an account via `PromptCaching::of`.
- Add `AgentRuntime` accessor (or reuse existing state) `conversation_has_history()`: true when the
  session's conversation log holds at least one completed turn. Use the session's existing conversation
  store; no token estimate.
- In `effort()`, after setting a level that differs from the previous one, if
  `runtime.models.main.prompt_caching && history`, append a second notice:
  `the next request re-reads the conversation uncached: a changed effort can invalidate the prompt cache`.
  Wording is hedged, since which models are affected is unmeasured. Emit nothing when the level is
  unchanged, caching is off, or the conversation is empty.
- The pure decision lives in a small function `effort_cache_warning(previous, new, caching, has_history)
  -> Option<&'static str>` so it is unit-testable without a runtime.

## Implementation Steps

1. `fontaine-core/src/models.rs` and the handle construction sites: add and populate `prompt_caching`.
2. `fontaine-core/src/agent.rs`: add the history accessor.
3. `fontaine-tui/src/ui/commands/model.rs`: add `effort_cache_warning`, call it from `effort()`.
4. Docs and changelog.

## Files to Modify

- `rust/fontaine-core/src/models.rs`, `rust/fontaine-core/src/providers/mod.rs` (construction), `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-tui/src/ui/commands/model.rs`
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
  caching account shows the warning; on an empty session or non-caching account does not.

## Open Questions

- Which current models render effort ahead of the tools and system prompt is unmeasured (`cache-bench`,
  #498 could measure it); the wording stays hedged until then.
