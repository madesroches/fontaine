# Prompt-Caching Strategy Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/13

## Overview

Every main-agent turn and every guardian call resends a long, mostly identical prefix (system
prompt, tool definitions, history; guardian rules). Providers reuse it only when the prefix is
byte-identical from the start, and on Anthropic only when a `cache_control` breakpoint asks for it.
This plan (1) sets the prefix rule Fontaine follows (anything before the latest message is
append-only), (2) turns on explicit caching on the Messages wire, (3) makes the guardian request
prefix-stable, and (4) makes cache reads visible so the rule can be checked in telemetry. The
guardian is not weakened: its prompt content, fail-closed handling and verdict cache are unchanged.

## Current State

- `fontaine-core/src/providers/anthropic.rs:33` builds the Messages client and returns
  `self.0.completion(model).erase()`. rig's `Messages::with_prompt_caching()`
  (`rig-core-0.45.0/src/providers/anthropic/wire.rs:451`) and `with_static_prefix_cache_ttl` are never
  called, so Anthropic requests carry no `cache_control` at all. The same holds for the Anthropic
  Messages route in `opencode_go.rs` (`GO_MESSAGES`, line 50).
- Chat/Responses wires (OpenAI, DeepSeek, gateways) cache automatically by prefix; nothing to enable,
  but nothing protects the prefix either.
- Main prompt: `prompt.rs` `layers`/`assemble` join base (with `{{workspace_roots}}` spliced in),
  `FONTAINE.md` files, skill index, scheduling note, output style, append. It is built once per
  `build_agent` (`agent/build.rs:77`) and set as the rig preamble (`finish_builder`).
- `Agent::rebuild` (`agent.rs:264`) rebuilds the prompt and re-registers tools on `/add-dir` and on
  output-style change (`set_output_style`, `agent.rs:328`). Both edit the system prompt, which sits
  at the very front, so they invalidate the whole cached prefix. Conversation history lives in the
  session store and is appended, not rewritten.
- Tools are registered in `tools::registry` order (builtins, then extension tools, `tools.rs:108`),
  then MCP tools; rig's definitions follow registration order
  (`rig-agent .../migrated_tests.rs:184`). Tool definitions precede the system prompt on Anthropic,
  so any change to the tool array invalidates everything.
- Guardian: `prompt_builder.rs` `system_prompt` splices `environment` and rule lists into the
  versioned profile prompt (stable per session/profile); all per-call material is in `user_message`.
  `guardian.rs:244` sends it through `ExtractorBuilder::append_preamble` with
  `ToolChoice::Required`, and no cache marker. The guardian prefix is system + the one forced verdict
  tool, so it is identical across calls unless the rules change.
- Accounting: `Usage` already carries `cached_input_tokens`/`cache_creation_input_tokens`
  (`rig-core completion/request.rs:692`) and the span bridge exports them
  (`telemetry/span_bridge.rs:195`), but nothing shows them to a user or tests that a second request
  actually reuses the prefix.

## Design

### The rule

Nothing before the latest message changes between requests of a session. Runtime state (queued steering,
schedule notes, task list, denial notices) reaches the model as appended messages or tool results,
which is already how `steering.rs` and the scheduler work. A change that must edit the front
(system prompt, tool set) is allowed only at an explicit boundary and is accepted as one lost cache
read: `/add-dir`, `/output-style`, MCP reconnect, `/clear`, compaction. The rule is recorded in
`docs/architecture.md` and as a Decisions line.

### Explicit caching on the Messages wire

`Connected::completion` (anthropic) and the Go Messages route enable `with_prompt_caching()` on the
wire, which puts breakpoints on the last tool, the system prompt and the final message block. The
static prefix (tools + system) gets a one-hour TTL via `with_static_prefix_cache_ttl(CacheTtl::OneHour)`
only if the account's `[providers]` entry opts in (extended TTL has a higher write price); default is
the five-minute TTL. A new account key `prompt_caching` (default `true`, user scope only because it
changes spend and what the provider stores) lets an account turn it off for gateways that reject
unknown `cache_control` fields. Gateway `compatible` Messages routes (`GO_MESSAGES`) default on, because
the Go gateway accepts the field (measured in #154) and a rejection would surface as a normal provider
error, not a security change.

### Guardian prefix stability

The guardian request already puts rules in the system prompt and call facts in the user message.
Changes:
1. Mark the guardian request cacheable through the same wire flag (the guardian model handle is built
   by the same provider code, so it inherits it).
2. Pin ordering inside the system prompt: rule lists and environment entries render in configured
   order (already the case); a unit test asserts `system_prompt` is byte-identical for two requests
   that differ only in call-specific fields, so a future edit that leaks per-call text into the
   prefix fails CI.
3. The verdict cache and `rules_version` digest are untouched.

### Tool order stability

Tool definitions are sorted deterministically before the agent sees them: builtins and extension
tools keep table order; MCP tools are ordered by (server name, tool name). A unit test pins that
re-running `register_tools` after a rebuild yields the same definition sequence. This is the only
non-additive code change in the main path.

### Accounting

Surface what telemetry already holds: the per-turn usage line and `/cost`-style session summary show
`cache read` and `cache write` token counts beside input/output when the provider reported them
(rig's `Usage` Display already does, `request.rs:1081`; the work is making sure core forwards the
fields on `SessionEvent::Usage` rather than dropping them). No new metric names.

## Implementation Steps

1. `providers/anthropic.rs`: apply `with_prompt_caching()` (and the optional one-hour static TTL)
   to the wire in `Connected::completion`; read the account's `prompt_caching`/`cache_ttl` settings
   through `ConnectContext`.
2. `providers/opencode_go.rs`: same for the Messages route.
3. `config.rs`/`config_scope.rs`: add `prompt_caching` (bool, default true) and `cache_ttl`
   (`"5m" | "1h"`) to the account table, user-scope only in the scope-policy table (loosening/spend
   keys are user scope).
4. `agent/build.rs`/`tools.rs`: deterministic tool ordering with MCP tools sorted by
   (server, tool); keep the registration order for the rest.
5. `turn.rs`: confirm `SessionEvent::Usage` carries cache read/write through to the TUI/headless
   usage rendering; fix any drop.
6. `fontaine-guardian`: add the prefix-stability tests; no behavior change.
7. Docs and `tasks/fontaine_harness_plan.md` Decisions line.

## Files to Modify

- `rust/fontaine-core/src/providers/anthropic.rs`, `providers/opencode_go.rs`, `providers/mod.rs` (`ConnectContext`)
- `rust/fontaine-core/src/config.rs`, `config_scope.rs`
- `rust/fontaine-core/src/tools.rs`, `agent/build.rs`, `agent/mcp.rs`
- `rust/fontaine-core/src/turn.rs` (only if usage fields are dropped)
- `rust/fontaine-guardian/src/prompt_builder.rs` (tests only)
- `docs/architecture.md`, `docs/configuration.md`, `docs/guardian.md`
- `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **Automatic breakpoints (rig `with_prompt_caching`) vs hand-placed `cache_control`:** rig already
  places the three useful markers and respects the four-marker budget; hand placement would be a
  custom provider layer the project forbids.
- **Append-only runtime state vs a "context reminder" message that is rewritten each turn:**
  rewriting would be simpler for the model but costs the cache from that message onward every turn.
- **Cache the guardian at all:** its prefix is ~4-5k tokens re-sent for every gated call, and
  providers bill cached reads at a fraction; the verdict cache covers only identical requests.
- **One-hour TTL by default:** rejected; it costs more per write and most sessions that matter
  refresh the five-minute entry on every turn.

## Decisions

## Documentation

- `docs/architecture.md`: the prefix rule and which actions are cache boundaries.
- `docs/configuration.md`: `prompt_caching`, `cache_ttl` account keys, with scope.
- `docs/guardian.md`: the guardian request is cacheable and its system prompt holds no per-call text.

## Testing Strategy

- Unit: wiremock-backed Anthropic completion asserts `cache_control` markers appear on the last
  tool, the system block and the last message block when enabled, and are absent when
  `prompt_caching = false`; and that the one-hour static TTL appears only when configured.
- Unit: tool definition order is identical across two `register_tools` runs and across MCP
  listing order permutations.
- Unit: guardian `system_prompt` bytes equal for two requests differing only in call fields; the
  user message carries the call fields.
- Unit: config parsing and scope policy for the new keys (project scope rejects them).
- Unit: cache read/write tokens survive from `Usage` to the rendered usage line.
- Cassette: an existing multi-turn Anthropic cassette run asserts the second request body's prefix
  (tools, system) is byte-identical to the first; a silent regression here costs money without any
  visible failure.

## Manual Verification

Live providers are needed to see real hit rates, and a live bench is not a CI gate:

1. `cargo run -p fontaine -- -p "list the files, then read the README"` against an Anthropic account.
2. Expect the second request's usage to report non-zero cache read tokens in the headless usage output.
   Not automated: needs a live key, and hit rates depend on provider minimum prompt lengths.

## Open Questions

- Does the Go gateway's Messages route honor `cache_control` the way `api.anthropic.com` does? #154
  measured automatic caching on its Chat route only. If it rejects the field, default
  `prompt_caching` off for that kind.
- Should compaction preserve a stable summary prefix? Out of scope here; one lost read per
  compaction is accepted.
