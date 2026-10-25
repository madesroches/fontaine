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
  (`rig-core completion/request.rs:692`), `SessionEvent::Usage` (`turn.rs:45`, emitted at 246) forwards
  the full `Usage`, and the span bridge exports the fields (`telemetry/span_bridge.rs:195`). The drop is
  in headless `Totals::add` (`fontaine/src/headless.rs:200-212`), which keeps only input, output and
  cost, so the JSON `usage.*` has no cache fields; the TUI transcript ignores `Usage`
  (`fontaine-tui/src/transcript.rs:207`).

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
only if the account (`accounts.<name>`) opts in (extended TTL has a higher write price); default is
the five-minute TTL. A new account key `prompt_caching` (default `true`, user scope only because it
changes spend and what the provider stores) lets an account turn it off for gateways that reject
unknown `cache_control` fields. The Go Messages route (`GO_MESSAGES`) default is unresolved pending the Open
Question below; no default is asserted here.

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

Builtin and extension tool order and MCP server order (BTreeMap config, sequential connect) are already
deterministic. Only within-server MCP tool order, which follows each server's `tools/list`, is new:
`GuardedSink::wrap_all` in `fontaine-mcp/src/register.rs` sorts a server's tools by name. MCP tools are
added after `register_tools` and refreshed later, so the sort lives at that seam, not in a static
registry.

### Accounting

Add cache read and cache write token counts to headless `Totals` and to the `usage.*` fields of the
headless JSON output. No TUI usage line and no `/cost` summary (new design surface, out of scope).
`GuardianUsage` (`gate.rs:170`) is checked for cache fields; if it lacks them, that is noted in the PR,
not extended here. No new metric names.

## Implementation Steps

1. `config.rs`: add `prompt_caching` (bool, default true) and `cache_ttl` (optional enum
   `"5m" | "1h"`, default `"5m"`) to `accounts.<name>`. `cache_ttl` is meaningless when
   `prompt_caching = false`, and both keys are rejected at config validation for non-Messages kinds.
   User scope only: `Rule::user_or_cli("accounts", ...)` in `config_scope.rs:135` already covers
   `accounts.*`, so no scope-table edit.
2. `Provider::connect`: read the settings and store them on `Connected` (`providers/anthropic.rs`) and
   `GoClient` (`providers/opencode_go.rs`).
3. `providers/anthropic.rs`: apply `with_prompt_caching()` (and the optional one-hour static TTL) to
   the wire in `Connected::completion`.
   `providers/opencode_go.rs`: same for the Messages route.
4. `fontaine-mcp/src/register.rs`: sort each server's tools by name in `GuardedSink::wrap_all`.
5. `fontaine/src/headless.rs`: add cache read/write to `Totals` (`Totals::add`) and to the JSON
   `usage.*` output; check `GuardianUsage` (`gate.rs:170`) for cache fields and note the result.
6. `fontaine-guardian`: add the prefix-stability tests; no behavior change.
7. Docs and `tasks/fontaine_harness_plan.md` Decisions line.

## Files to Modify

- `rust/fontaine-core/src/providers/anthropic.rs`, `providers/opencode_go.rs`
- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-mcp/src/register.rs`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine-guardian/src/prompt_builder.rs` (tests only)
- `docs/architecture.md`, `docs/configuration.md`, `docs/guardian.md`
- `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **Append-only runtime state vs a "context reminder" message that is rewritten each turn:**
  rewriting would be simpler for the model but costs the cache from that message onward every turn.
- **Cache the guardian at all:** its prefix is ~4-5k tokens re-sent for every gated call, and
  providers bill cached reads at a fraction; the verdict cache covers only identical requests.

## Decisions

- 2026-10-25: Anything before the latest message is append-only within a session; front edits happen only at explicit boundaries.
- 2026-10-25: `prompt_caching` defaults on for Anthropic Messages accounts.
- 2026-10-25: The one-hour cache TTL is opt-in (`cache_ttl = "1h"`); default is five minutes, since extended TTL costs more per write.
- 2026-10-25: The one lost cache read per compaction is accepted.

## Documentation

- `docs/architecture.md`: the prefix rule and which actions are cache boundaries.
- `docs/configuration.md`: `prompt_caching`, `cache_ttl` account keys, with scope.
- `docs/guardian.md`: the guardian request is cacheable and its system prompt holds no per-call text.

## Testing Strategy

- Unit: wiremock-backed `kind = "anthropic"` test (`fontaine-core/tests/providers.rs`) sends two
  sequential requests and asserts the second request's prefix (tools + system bytes) is byte-identical
  to the first. Marker assertions cover only what Fontaine owns: `Connected::completion` sets the
  flag, and the account key is honored (absent when `prompt_caching = false`; one-hour static TTL only
  when configured).
- Unit: permutation test on the MCP sort: any `tools/list` order yields the same definition sequence.
- Unit: guardian `system_prompt` bytes equal for two requests differing only in call fields; the
  user message carries the call fields.
- Unit: config parsing for the new keys, including rejection for non-Messages kinds; a cheap
  confirmation that project scope rejects them.
- Unit: static 1h + tail 5m marker ordering is accepted by rig (it rejects `CacheRetention::None` when
  the wire places markers, and requires one-hour markers before five-minute ones).
- Unit: cache read/write tokens survive from `Usage` into headless `Totals` and the JSON `usage.*` output.

## Manual Verification

Live providers are needed to see real hit rates, and a live bench is not a CI gate:

1. `cargo run -p fontaine -- -p "list the files, then read the README"` against an Anthropic account.
2. Expect the second request's usage to report non-zero cache read tokens in the headless JSON `usage` output.
   Not automated: needs a live key, and hit rates depend on provider minimum prompt lengths.

## Open Questions

- Does the Go gateway's Messages route honor `cache_control` the way `api.anthropic.com` does? #154
  measured automatic caching on its Chat route only. If it rejects the field, default
  `prompt_caching` off for that kind.
