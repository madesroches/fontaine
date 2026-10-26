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

- `fontaine-core/src/providers/anthropic.rs:46-48` (`Connected::completion`) returns
  `self.0.completion(model).erase()`. rig's `Messages::with_prompt_caching()`
  (`rig-core-0.45.0/src/providers/anthropic/wire.rs:451`) and `with_static_prefix_cache_ttl` are never
  called, so Anthropic requests carry no `cache_control` at all. The same holds for the Anthropic
  Messages route in `opencode_go.rs` (`GO_MESSAGES`, line 50; route selection in
  `GoClient::completion`, line 203; the Messages client is `GoClient.messages`, line 138).
- Chat/Responses wires (OpenAI, DeepSeek, gateways) cache automatically by prefix; nothing to enable,
  but nothing protects the prefix either.
- Main prompt: `prompt.rs` `layers`/`assemble` join base (with `{{workspace_roots}}` spliced in),
  `FONTAINE.md` files, skill index, scheduling note, output style, append. It is built once per
  `build_agent` (`agent/build.rs:77`) and set as the rig preamble (`finish_builder`).
- `Agent::rebuild` (`agent.rs:264`) rebuilds the prompt and re-registers tools on `/add-dir`
  (`agent.rs:542`), on output-style change (`agent.rs:348`) and on model switch
  (`agent/models.rs:111`). These edit the system prompt or tool array, which sit at the very front, so
  they invalidate the whole cached prefix. MCP servers connect after the agent is built
  (`Agent::connect_mcp`, `agent/mcp.rs`), including on-demand approval and `tools/list_changed`
  refresh, so a late connect also changes the tool array mid-session. Conversation history lives in the
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
read: `/add-dir`, `/output-style`, model switch, MCP connect, on-demand approval and `tools/list_changed`
refresh, `/clear`, compaction. The rule is recorded in
`docs/architecture.md` and as a Decisions line.

### Explicit caching on the Messages wire

`Connected::completion` (anthropic) and the Go Messages route enable `with_prompt_caching()` on the
wire, which puts breakpoints on the last tool, the system prompt and the final message block. The
static prefix (tools + system) gets a one-hour TTL via `with_static_prefix_cache_ttl(CacheTtl::OneHour)`
only if the account (`accounts.<name>`) opts in (extended TTL has a higher write price); default is
the five-minute TTL. A new account key `prompt_caching` (user scope only because it
changes spend and what the provider stores) has a per-kind default: `true` for `anthropic`, `false`
for `opencode-go` until a live request shows the gateway's Messages route honors `cache_control`
(not measured; #154 covered only its Chat route). A Go user can opt in with `prompt_caching = true`;
on Go the keys affect only models routed to the Messages wire (other wires cache automatically).
The default lives on the provider (`default_prompt_caching()`), not in the config struct, so the
account field is `Option<bool>`.

### Guardian prefix stability

The guardian request already puts rules in the system prompt and call facts in the user message.
Changes:
1. Mark the guardian's static prefix (forced verdict tool + system prompt) cacheable, prefix only:
   the guardian handle uses `with_static_prefix_cache_ttl` without `with_prompt_caching()`, so the
   final user message (per-call facts, never reused) carries no marker and pays no write premium.
   The prefix TTL follows the account's `cache_ttl` (five minutes by default). The guardian handle is
   built by the same provider code, so `Connected::completion` takes a "prefix only" parameter that
   the guardian path sets. Anthropic caches a prefix only above a model-dependent minimum length
   (about 1024 tokens on Sonnet/Opus, higher on Haiku; check the current docs); below it the marker
   is a silent no-op.
2. Pin ordering inside the system prompt: rule lists and environment entries render in configured
   order (already the case); a unit test asserts `system_prompt` is byte-identical for two requests
   that differ only in call-specific fields, so a future edit that leaks per-call text into the
   prefix fails CI.
3. The verdict cache and `rules_version` digest are untouched.

### Tool order stability

Builtin and extension tool order and MCP server order (BTreeMap config, sequential connect) are already
deterministic. Only within-server MCP tool order, which follows each server's `tools/list`, is new (the sort
stabilizes order within one server's list only; a late connect or refresh remains a boundary):
`GuardedSink::wrap_all` in `fontaine-mcp/src/register.rs` sorts a server's tools by name. MCP tools are
added after `register_tools` and refreshed later, so the sort lives at that seam, not in a static
registry.

### Accounting

Add cache read and cache write token counts to headless `Totals` and to the `usage.*` fields of the
headless JSON output. No TUI usage line and no `/cost` summary (new design surface, out of scope).
`GuardianUsage` (`fontaine-core` `gate.rs:170`) is checked for cache fields; if it lacks them, that is noted in the PR,
not extended here. No new metric names.

## Implementation Steps

1. `config.rs`: add `prompt_caching` (optional bool; unset means the provider default) and `cache_ttl`
   (optional enum `"5m" | "1h"`, default `"5m"`) to `accounts.<name>`. `cache_ttl` is meaningless when
   caching is off. Add `Provider::accepts_prompt_caching()` and `default_prompt_caching()` next to
   `accepts_wires()`/`accepts_model_options()` (`providers/mod.rs:79-86`): accepts is true for
   `anthropic` and `opencode-go`, default is true for `anthropic` and false for `opencode-go`.
   `check_account` (`config.rs`, beside the `wires` check) rejects both keys for other kinds, naming
   `accounts.<name>.<key>`. Update the config test in `tests/providers.rs` (the style of
   `wires_belong_to_opencode_go_only`) to cover accepted and rejected kinds.
   User scope only: `Rule::user_or_cli("accounts", ...)` in `config_scope.rs:135` already covers
   `accounts.*`, so no scope-table edit.
2. `Provider::connect`: read the settings and store them on `Connected` (`providers/anthropic.rs`) and
   `GoClient` (`providers/opencode_go.rs`, where `GoClient.messages` at line 138 is the Messages client).
3. `providers/anthropic.rs`: apply `with_prompt_caching()` (and the optional one-hour static TTL) to
   the wire in `Connected::completion` for the main agent; for the guardian handle apply only
   `with_static_prefix_cache_ttl`.
   `providers/opencode_go.rs`: same for `GoClient.messages`, selected in `GoClient::completion`,
   only when the account's resolved `prompt_caching` is true (Go default false).
4. `fontaine-mcp/src/register.rs`: sort each server's tools by name in `GuardedSink::wrap_all`.
5. `fontaine/src/headless.rs`: add cache read/write to `Totals` (`Totals::add`) and to the JSON
   `usage.*` output; check `GuardianUsage` (`fontaine-core` `gate.rs:170`) for cache fields and note the result.
6. `fontaine-guardian`: add the prefix-stability tests; no behavior change.
7. Docs and `tasks/fontaine_harness_plan.md` Decisions line.

## Files to Modify

- `rust/fontaine-core/src/providers/anthropic.rs`, `providers/opencode_go.rs`, `providers/mod.rs`
- `rust/fontaine-core/tests/account_session.rs`, `tests/providers.rs`
- `rust/fontaine-core/src/config.rs`
- `rust/fontaine-mcp/src/register.rs`
- `rust/fontaine/src/headless.rs`, `docs/schemas/headless-v1.schema.json`
- `rust/fontaine-guardian/src/prompt_builder.rs` (tests only)
- `docs/architecture.md`, `docs/configuration.md`, `docs/guardian.md`, `docs/headless.md`
- `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **Append-only runtime state vs a "context reminder" message that is rewritten each turn:**
  rewriting would be simpler for the model but costs the cache from that message onward every turn.
- **Cache the guardian at all:** its prefix (profile prompt ~1.2k tokens, plus rules and the verdict
  tool schema, roughly 1.5-2k) is re-sent for every gated call, and providers bill cached reads at a
  fraction; the verdict cache covers only identical requests. The prefix sits near the provider
  minimum cacheable length (about 1024 tokens on Sonnet/Opus, higher on Haiku), so on some guardian
  models the marker may never take effect; that costs nothing but yields no saving, and the
  prefix-only choice avoids a write premium on the per-call tail.

## Decisions

- 2026-10-25: Anything before the latest message is append-only within a session; front edits happen only at explicit boundaries.
- 2026-10-25: `prompt_caching` defaults on for Anthropic Messages accounts.
- 2026-10-25: AFK: `prompt_caching` has a per-kind provider default (true for `anthropic`, false for `opencode-go` until its Messages route is measured live); Go users opt in; the keys are accepted only by kinds where `accepts_prompt_caching()` is true.
- 2026-10-25: AFK: the guardian handle marks only the static prefix (`with_static_prefix_cache_ttl`, no tail marker), since its final user message is per-call; its prefix is ~1.5-2k tokens, near provider minimums, so savings are model-dependent.
- 2026-10-25: The one-hour cache TTL is opt-in (`cache_ttl = "1h"`); default is five minutes, since extended TTL costs more per write.
- 2026-10-25: The one lost cache read per compaction is accepted.

## Documentation

- `docs/architecture.md`: the prefix rule and which actions are cache boundaries.
- `docs/configuration.md`: `prompt_caching`, `cache_ttl` account keys, with scope.
- `docs/guardian.md`: the guardian request is cacheable and its system prompt holds no per-call text.
- `docs/headless.md`: the `usage` JSON (main/guardian/subagents) gains cache read/write token fields; update `docs/schemas/headless-v1.schema.json` to match.

## Testing Strategy

- Unit: one wiremock-backed `kind = "anthropic"` test in `fontaine-core/tests/account_session.rs`
  (helpers `gateway()`, `received_requests()`) asserts `cache_control` on the last tool and on the
  system block when `prompt_caching = true`, none when `false`, and a 1h TTL only when
  `cache_ttl = "1h"`.
- Unit: Fontaine's own prefix stability, without HTTP: build the agent twice from the same inputs and
  compare the system prompt and tool order.
- Unit: permutation test on the MCP sort: any `tools/list` order yields the same definition sequence.
- Unit: guardian `system_prompt` bytes equal for two requests differing only in call fields; the
  user message carries the call fields.
- Unit: config parsing for the new keys, including rejection for kinds where
  `accepts_prompt_caching()` is false and the per-kind defaults; a cheap confirmation that project
  scope rejects them.
- Unit: cache read/write tokens survive from `Usage` into headless `Totals` and the JSON `usage.*` output.

## Manual Verification

Live providers are needed to see real hit rates, and a live bench is not a CI gate:

1. `cargo run -p fontaine -- -p "list the files, then read the README"` against an Anthropic account.
2. Expect the second request's usage to report non-zero cache read tokens in the headless JSON `usage` output.
   Not automated: needs a live key, and hit rates depend on provider minimum prompt lengths.

## Open Questions

None.
