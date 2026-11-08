# claude-subscription Web Search Warning Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/522

## Overview

On a `claude-subscription` main model, Fontaine warns at startup that "web search is unavailable:
install docker or podman, or set `[websearch] backend`". Users read this as a bug, because the
model behind the subscription can search when run through Claude Code. The investigation below
shows the subscription transport cannot carry provider-native search without breaking the
"the CLI executes no tool" design. So the fix is to make the warning say why: name the reason the
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
  (`inventory.rs`) that refuses every call. The protocol adapter refuses a request that carries a
  hosted tool (`protocol/tests.rs:193-197`, "hosted tool"), history that holds
  `server_tool_use` / `web_search_tool_result` blocks (`:203-215`, "without loss"), and a stream
  that starts a `server_tool_use` block (`:575-582`, "native tool"). The Decision for #412 in the
  master plan (`### Providers`) records that the CLI "executes no tool".

### Investigation: can the CLI carry search?

- **The Anthropic server tool**: the CLI takes no raw API tool entries. Its tools come only from
  its built-in set (`--tools`) or from MCP servers, and an MCP tool is a client tool. There is no
  flag, setting or env var that adds a `web_search_20250305` entry to the CLI's request. (The env
  var that would add body fields, `CLAUDE_CODE_EXTRA_BODY`, is in `CONFLICTING_ENV` and refused on
  purpose.)
- **The CLI's own `WebSearch`**: this is a client-side tool that the CLI itself executes, by
  issuing a second model request with the server tool. Allowing it (`--tools WebSearch` plus an
  allow rule under `dontAsk`) would make the CLI run a tool outside Fontaine's floor, guardian,
  hooks and ledger. The result would also arrive as a CLI-run `tool_use`/`tool_result` pair that
  Fontaine's history could not replay to a later process without loss. And under `--max-turns 1`
  the process ends before the model ever reads the result. Lifting any of these means
  redesigning the transport's core invariant, which is far bigger than this issue.

Conclusion: no provider-native search for `claude-subscription`. The issue's own fallback applies:
reword the warning and point to the `[websearch]` options.

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
fn entry_for(native: NativeSearch, provider: &dyn Provider, base_url: Option<&str>)
    -> Result<Value, Absence>
```

The order follows today's checks: `Off` first, then `Unsupported`, then `On` → entry, then the
endpoint comparison (a missing or empty `base_url` counts as the official one; a kind with no
`default_endpoint` is `Unverified`, which matches today's `?`). `provider_entry` becomes a thin
wrapper. It looks up the account, and a main account missing from `config.accounts` is
`Unverified`, matching today's `None` from `?`. Then it calls `entry_for`. It returns the
`Result`. `offered` keeps `Option` via `.ok()`.

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
   main account kind produces. Keep the existing two asserts.
4. Docs (see Documentation).
5. Master plan `tasks/fontaine_harness_plan.md`: add the Decisions lines below at the end of
   `### Providers`.

## Files to Modify

- `rust/fontaine-core/src/native_search.rs`
- `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-core/tests/websearch_runtime.rs`
- `docs/claude-subscription.md`
- `docs/tools.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Enable the CLI's `WebSearch`** (rejected): it gives subscription users search with no extra
  setup. But the CLI would execute a tool outside the floor and the guardian, it would break
  history replay, and it does not work under `--max-turns 1`. It contradicts the #412 Decision.
- **Suppress the warning for `claude-subscription`** (rejected): search really is unavailable
  there. Silence would leave the model without search and the user without the hint.
- **A per-kind note on the `Provider` trait** (rejected): one generic reason built from
  `name()` covers every kind without search. A trait method would add surface for no
  per-kind content.
- **Reason enum over three hand-written warning strings at the call site** (chosen): the reasons
  come from the same checks that decide whether search is offered, so they cannot drift apart.

## Decisions

Master-plan lines to add at the end of `### Providers` (dated at implementation):

- `claude-subscription` offers no provider-native web search. The CLI takes tools only through
  its built-in set or MCP (no raw server-tool entry), and allowing its own `WebSearch` would have
  the CLI execute a tool outside the floor, guardian and ledger. That contradicts the #412
  transport design, and under `--max-turns 1` the result would never reach the model. Claude Code
  on a subscription searches; the user gets Fontaine's `websearch` tool instead (#522).
- The question of running provider-native search outside the guardian and the result scanner
  does not arise for `claude-subscription`, because nothing is offered. The existing #19 Decision
  is unchanged (#522).
- The no-search startup warning names why the main model's account offers no provider search
  (kind without one, `native = "off"`, unverified endpoint). Claude Code has no such warning
  (#522).

## Documentation

- `docs/claude-subscription.md`: add a short "Web search" paragraph. It says the provider's own
  search is not available through the CLI, because the CLI runs no tool and the model's tools
  are Fontaine's. It says search goes through the `websearch` tool (default SearXNG container
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
- A kind with no `native_web_search` under `off` → `Off`, so the order of the checks is pinned.
- `Display` of each `Absence` holds its key phrase (the kind name, `native = "off"`,
  `native = "on"`).

Integration (`tests/websearch_runtime.rs`): the existing once-at-startup test also asserts the
reason clause, so the wording is checked through the real `AgentRuntime` startup path. No live
services are involved.

## Open Questions

None.
