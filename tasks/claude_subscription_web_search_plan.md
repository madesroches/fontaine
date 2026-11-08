# claude-subscription Web Search Warning Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/522

## Overview

On a `claude-subscription` main model, Fontaine warns at startup that "web search is unavailable:
install docker or podman, or set `[websearch] backend`". Users read this as a bug, because the
model behind the subscription can search when run through Claude Code. The investigation below
shows the adapter deliberately refuses the server-tool traffic provider-native search needs, and
lifting that is out of scope here. So the fix is to make the warning say why: name the reason the
main model's account offers no built-in search (the account kind has none, `[websearch] native`
turns it off, or the endpoint is unverified). The `claude-subscription` and tools docs then explain
the limit and the `[websearch]` options.

## Current State

- `Provider::native_web_search` (`rust/fontaine-core/src/providers/mod.rs:87-91`) defaults to
  `None`. Only `anthropic` overrides it (`providers/anthropic.rs:29-32`, `web_search_20250305`).
  `claude-subscription` does not.
- `native_search::provider_entry` (`rust/fontaine-core/src/native_search.rs:46-60`) returns
  `None` when `native = "off"`, when the kind has no entry, or (under `native = "auto"`) when the
  account's `base_url` is not the kind's `default_endpoint`. `provider_available` (`:64-66`) is
  `provider_entry(..).is_some()`. Both collapse the three reasons into one `None`.
- `warn_if_no_search` (`rust/fontaine-core/src/agent.rs:266-278`) prints a fixed string when
  there is no provider search, no `[websearch] backend` and no container runtime. It is called
  once per process from `agent/setup.rs:530`. `tests/websearch_runtime.rs:216-231` pins its
  wording ("web search is unavailable", "docker or podman").
- The `claude-subscription` CLI launch (`providers/claude_subscription/cli.rs:745-803`) passes
  `--tools ""`, `--max-turns 1`, `--permission-mode dontAsk` and an inert MCP inventory
  (`inventory.rs`) that refuses every call. The Decision for #412 in the
  master plan (`### Providers`) records that the CLI "executes no tool".

### Investigation: can the CLI carry search?

- **The Anthropic server tool**: a `web_search_20250305` entry could ride the extra body Fontaine
  already writes. `CONFLICTING_ENV` only refuses `CLAUDE_CODE_EXTRA_BODY` from the parent env
  (`cli.rs:32-34`, `:309-314`); Fontaine itself writes it into the child's settings `env`
  (`protocol.rs:364-373`), and that body already carries a raw `tools` array (`:345-359`). What
  stops it is the adapter's own fail-closed refusals: hosted tool definitions in the request
  (`protocol.rs:404-412`), `server_tool_use` / `web_search_tool_result` history it cannot replay
  "without loss" (`:550`, `:571`), and a streamed `server_tool_use` block, which fails as a native
  tool (`:1069`, `:1139-1141`). Lifting them means changing the stream validator and the history
  replay. How the CLI handles server-tool blocks in its stream-json and its own message assembly
  can only be qualified live against the real CLI, which this change cannot do.
- **The CLI's own `WebSearch`**: this is a client-side tool that the CLI itself executes, by
  issuing a second model request with the server tool. Allowing it (`--tools WebSearch` plus an
  allow rule under `dontAsk`) would make the CLI run a tool outside Fontaine's floor, guardian,
  hooks and ledger. The result would also arrive as a CLI-run `tool_use`/`tool_result` pair that
  Fontaine's history could not replay to a later process without loss. And under `--max-turns 1`
  the process ends before the model ever reads the result. Lifting any of these means
  redesigning the transport's core invariant, which is far bigger than this issue.

Conclusion: provider-native search for `claude-subscription` is left as a follow-up. The issue's
own fallback applies here: reword the warning and point to the `[websearch]` options.

## Design

### `native_search.rs`: a reason, not a bare `None`

Replace the `Option` in the core of `provider_entry` with a `Result` that says why there is no
entry. The reasons live in one enum with a `Display` used by the warning:

```rust
/// Why the main model's account offers no provider search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Absence {
    /// The account kind has no server-side search Fontaine can offer (its `name()`).
    Unsupported(&'static str),
    /// `[websearch] native = "off"`.
    Off,
    /// `native = "auto"` and the account's `base_url` is not the kind's own endpoint.
    Unverified,
}
```

`Display` text (each a clause after "web search is unavailable: "):

- `Unsupported(kind)`: ``"`{kind}` accounts have no built-in search Fontaine can offer"``
- `Off`: ``"`[websearch] native = \"off\"` turns off the provider's search"``
- `Unverified`: ``"the account's `base_url` is not the provider's own endpoint, so its search needs `[websearch] native = \"on\"`"``

A pure function holds the selection, so it can be tested without building `Models`:

```rust
fn entry_for(native: NativeSearch, provider: &dyn Provider, account: Option<&Account>)
    -> Result<Value, Absence>
```

The order is `Unsupported` first, then `Off`, then `On` → entry, then the endpoint comparison (a
missing or empty `base_url` counts as the official one; a kind with no `default_endpoint` is
`Unverified`, which matches today's `?`). `Unsupported` precedes `Off` so a kind with no search is
never told that `native = "off"` turns off a search it never had. `provider_entry` becomes a thin
wrapper that looks up the account and passes the `Option` into
`entry_for`, which maps a missing main account to `Unverified` only on the `auto`
endpoint-comparison branch, after `Unsupported`/`Off`/`On` are decided, matching today's early
returns. It returns the `Result`. `offered` keeps `Option` via `.ok()`.

`provider_available` is replaced by:

```rust
/// Why the main model's account has no provider search, whatever the mode and rules say this
/// turn; `None` when it has one.
pub(crate) fn absence(config: &Config, models: &Models) -> Option<Absence>
```

### `agent.rs`: the warning names the reason

`warn_if_no_search` uses `native_search::absence(..)` in place of `!provider_available(..)`. It
keeps the other two conditions and prints:

```
web search is unavailable: {absence}; install docker or podman, or set `[websearch] backend`
```

On a `claude-subscription` main model this reads: "web search is unavailable:
`claude-subscription` accounts have no built-in search Fontaine can offer; install docker or
podman, or set `[websearch] backend`". Nothing changes about when the warning fires, which
backend is used, or what is offered to the model.

## Implementation Steps

1. `rust/fontaine-core/src/native_search.rs`: add `Absence` with `Display`, `entry_for`, and the
   `Result`-returning `provider_entry`. Replace `provider_available` with `absence`. Adjust
   `offered`. Add unit tests (see Testing Strategy).
2. `rust/fontaine-core/src/agent.rs`: rewrite `warn_if_no_search` to interpolate the absence.
   Update its doc comment.
3. `rust/fontaine-core/tests/websearch_runtime.rs`: extend
   `no_search_path_at_all_is_said_once_at_startup` to assert the reason clause the test world's
   main account kind produces: "`openai-compatible` accounts have no built-in search Fontaine can
   offer". Keep the existing two asserts.
4. Docs (see Documentation).
5. Master plan `tasks/fontaine_harness_plan.md`: add the claude-subscription Decisions line at the
   end of `### Providers` and the warning Decision at the end of `### Tools`.
6. `CHANGELOG.md`: add an `## [Unreleased]` `### Changed` entry for the reworded startup warning
   (#522).
7. Open a follow-up GitHub issue for provider-native search on `claude-subscription`, referencing
   #522.

## Files to Modify

- `rust/fontaine-core/src/native_search.rs`
- `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-core/tests/websearch_runtime.rs`
- `docs/claude-subscription.md`
- `docs/tools.md`
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Suppress the warning for `claude-subscription`** (rejected): search really is unavailable
  there. Silence would leave the model without search and the user without the hint.
- **A per-kind note on the `Provider` trait** (rejected): one generic reason built from
  `name()` covers every kind without search. A trait method would add surface for no
  per-kind content.
- **Reason enum over three hand-written warning strings at the call site** (chosen): the reasons
  come from the same checks that decide whether search is offered, so they cannot drift apart.

## Decisions

Master-plan lines to add (dated at implementation). The first goes at the end of `### Providers`,
the second at the end of `### Tools`.

- `claude-subscription` offers no provider-native web search yet. A server-tool entry could ride
  the extra body Fontaine already writes; the adapter's fail-closed refusals (hosted tools in the
  request, server-tool blocks in the stream, history it cannot replay without loss) stop it, and
  lifting them needs live qualification against the real CLI. Left as a follow-up; the user gets
  Fontaine's `websearch` tool meanwhile (#522).
- The no-search startup warning names why the main model's account offers no provider search
  (kind without one, `native = "off"`, unverified endpoint). Claude Code has no such warning
  (#522).

## Documentation

- `docs/claude-subscription.md`: add a short "Web search" paragraph. It says the provider's own
  search is not offered through this kind yet. It says search goes through the `websearch` tool (default SearXNG container
  with docker/podman, or `[websearch] backend`), and that `[websearch] native` has no effect for
  this kind. Link to `tools.md#web-search`.
- `docs/tools.md` `## Web search`: item 2 names `claude-subscription` among the kinds that use
  the `websearch` tool. The "Without a container runtime" paragraph says the warning names why
  the provider's search does not apply.

## Testing Strategy

Unit tests in `native_search.rs` (`entry_for` with real providers via
`AccountKind::named(..).provider()`):

- `claude-subscription` under `auto` and `on` → `Unsupported("claude-subscription")`. This pins
  #522: the kind gets no entry, and the reason names it.
- `anthropic`: `off` → `Off`. `auto` with no/empty/official `base_url` (with and without a
  trailing `/`) → the entry. `auto` with a gateway `base_url` → `Unverified`. `on` with a
  gateway `base_url` → the entry.
- A kind with no `native_web_search` under `off` → `Unsupported`, so the order of the checks is pinned.
- A missing main account under `off`, or for a kind with no search → not `Unverified`; under `auto` with an entry kind → `Unverified`.
- `Display` of each `Absence` holds its key phrase (the kind name, `native = "off"`,
  `native = "on"`).

Integration (`tests/websearch_runtime.rs`): the existing once-at-startup test also asserts the
reason clause ("`openai-compatible` accounts have no built-in search Fontaine can offer"), so the wording is checked through the real `AgentRuntime` startup path. No live
services are involved.

## Open Questions

None.
