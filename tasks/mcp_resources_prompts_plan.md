# MCP Resources and Prompts Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/33

## Overview

Claude Code lets users @-mention MCP resources, exposes `ListMcpResources`/`ReadMcpResource`
tools to the model, and turns MCP prompts into `/mcp__server__prompt` slash commands
(https://code.claude.com/docs/en/mcp#use-mcp-resources). Fontaine's MCP client is tools-only
today (`docs/mcp.md:99`: "Only tools are used; resources and prompts are not."). This plan adds
the two other MCP primitives with Fontaine's security posture:

- **Resources** are read through two built-in tools, `ListMcpResources` and `ReadMcpResource`,
  both `RiskClass::Arbitrary`, so every read is floor-gated and guardian-judged, and the content
  flows through the result scanner like any other tool result. The TUI @-mention popup lists
  resource URIs from connected servers and inserts `@<uri>` verbatim into the prompt; the model
  reads the content through the gated tool (the same completion-only model as file mentions).
- **Prompts** become user-invoked slash commands `/mcp__<server>__<prompt>`, enumerated from
  `prompts/list` at connect time and merged into the `/` menu. Invoking one runs `prompts/get`
  and sends the returned messages as a user turn. The server writes the injected text, but only
  when the user explicitly types the command — the same trust boundary as a skill.

The issue's dependency ("Depends on Phase 5 step 1") is the MCP client itself, which shipped in
Phase 5 (connect, lifecycle, tool registration, elicitation, `/mcp reconnect`).

## Current State

- **Client core** (`rust/fontaine-tools/src/mcp/`): `servers.rs` connects over stdio or
  streamable HTTP and delegates discovery to rig-rmcp's `McpClientHandler`, which issues
  `tools/list` and pushes `DynamicTool`s into `GuardedSink` (`register.rs`). Every MCP tool is
  wrapped by `guard_callback(.., RiskClass::Arbitrary, ..)`, so calls are gated and results pass
  through `Guard.results` — the `ResultPipeline` whose `ScannerStage`
  (`rust/fontaine-core/src/scanner_stage.rs`) scans server content when a scanner is enabled.
  `client_config()` (`servers.rs`) declares only the elicitation capability; no `resources` or
  `prompts` capability is declared. `Connected` holds `connection: McpConnection` (re-exported
  as `McpConnectionHandle`), `tool_count`, `tool_names`.
- **rig-rmcp 0.46.0 / rmcp 3.9.0** (exact pins, `rust/Cargo.toml:58-60`): `McpConnection` already
  wraps `resources()` and `read_resource()`; prompts are reachable through its `peer()` escape
  hatch (`list_all_prompts()`, `get_prompt()`). rmcp 3.9 also has MRTR variants returning
  `InputRequired` mid-round; Fontaine uses neither MRTR variant today (elicitation is served
  from a transport tap, `ElicitationTap`, not from MRTR rounds).
- **Supervisor** (`rust/fontaine-core/src/agent/mcp_supervisor.rs`): `Supervisor` + `ServerSlot`
  own lifecycle; `McpServerState::Connected { tools }` feeds `AgentRuntime::mcp_states()` → the
  `/mcp` notice (`rust/fontaine-tui/src/reports.rs`). `Connected` values live behind the
  supervisor's slots; `AgentRuntime` reaches them through it.
- **Tool registry** (`rust/fontaine-core/src/tools.rs`): `registry(RegistryParams)` builds the
  built-in toolset; risk classes come from the descriptor table `fontaine_tools::risk::
  registered_risk` (a tool without a row is `Arbitrary`, so the choke-point test
  `rust/fontaine-core/tests/choke_point.rs` stays green as long as rows are added).
- **@-mentions** (`rust/fontaine-tui/src/complete.rs`, `app.rs`): completion-only. The candidate
  pipeline is `Vec<String>` end to end (`FileIndex::completion_paths()` → `App.files` →
  `complete::rank()` → `CompletionList.matches` → `accept_completion` string-splice). The message
  is sent as typed — the recorded 2026-10-22 decision (`tasks/completed/tui_input_modes_plan.md`)
  requires content reaching the model to pass the scope validator, floor and guardian. There is
  no submit-time expansion point.
- **Slash commands** (`rust/fontaine-tui/src/commands.rs`): one static `BUILTINS` table; unknown
  names parse to `Command::Other { name, argument }`, which dispatches skills by name
  (`rust/fontaine-tui/src/ui/mod.rs` `dispatch_inner`). The `/` menu (`App.menu`,
  `rust/fontaine-tui/src/lib.rs:494`) is built from builtins + `runtime.slash_skills()` and
  rebuilt by `refresh_menu` (`rust/fontaine-tui/src/ui/commands/plugin.rs:53`). A command that
  starts a turn returns `CommandOutcome::Turn(Submission::Expanded { text, prompt })` and may
  append `Record::UserMessage` to the ledger first (the `/init` and skill paths are the
  template). Skills (`AgentRuntime::expand_skill`) are the closest existing analog for MCP
  prompts. Headless `fontaine -p` has no slash commands.
- **System prompt**: assembled in `rust/fontaine-core/src/context.rs`; the file-mention
  explanation lives there today and is the place to explain `@<uri>` resource mentions.

## Design

### Discovery

`client_config()` in `fontaine-tools/src/mcp/servers.rs` additionally declares the `resources`
and `prompts` client capabilities (subscribe: false — listings are pulled at connect, not pushed).

After a server connects (and on reconnect), the core pulls both listings once, alongside the
existing `tools/list` flow:

- `resources`: `connection.resources()` (rig-rmcp's paginated wrapper over
  `peer().list_all_resources()`), giving `uri`, `name`, `description`, `mime_type` per resource.
- `prompts`: `peer().list_all_prompts()`, giving `name`, `description`, `arguments` (name +
  required flag) per prompt.

Both listings hang off the per-server state next to `tool_count`/`tool_names` and are exposed
through `AgentRuntime` alongside `mcp_states()`. `resources/templates/list` (templated resource
URIs) is out of scope: reads of templated URIs would need user-supplied parameters, which the
issue does not ask for; the listing simply omits templates.

On a `notifications/resources/list_changed` or `notifications/prompts/list_changed` notification,
the listings for that server are re-pulled. `MetaTap` (`fontaine-tools/src/mcp/meta.rs`) already
observes the transport and records facts; extend it to also forward these two notifications (it
passes everything through today), and let the supervisor refresh the listings the way it already
reconciles `tools/list_changed`. A failed refresh keeps the previous listings.

MRTR: `read_resource`/`get_prompt` are called through their plain (non-MRTR) forms. A server
that answers a plain call with `InputRequired` gets a declined round — the call fails with a tool
error saying the server asked for input the client does not provide in this release. This keeps
resource reads and prompt fetches single-round; elicitation during a round still works through
the existing `ElicitationTap` transport path.

### Resource tools

Two new built-in tools, registered in `fontaine-tools/src/builtins.rs` (construction) and
`fontaine-tools/src/risk.rs` (two new descriptor rows, both `RiskClass::Arbitrary`), assembled
into the registry through `RegistryParams` in `rust/fontaine-core/src/tools.rs`. They receive a
new `RegistryParams` field — an `Arc<dyn McpResourceQueries>` trait object implemented by the
runtime over the supervisor's connected servers — so the tools can reach live connections while
the tools crate stays free of runtime types.

- `ListMcpResources() -> [{server, uri, name, description, mime_type}]` across all connected
  servers that declared the resources capability; empty (with an explanatory line) when none did.
  Read-only in spirit but `Arbitrary` in class: the *listing* is server-declared text and its
  read is a server round trip, so it takes the same gate as everything else MCP. The `Arbitrary`
  class costs a guardian judgment per listing in `auto`; accepting that keeps the "MCP is never
  fast-pathed" invariant uniform instead of carving out an exception. A cached listing returned
  without a server call would be a quieter, cheaper tool, but then `ListMcpResources` results
  would be the one MCP surface the scanner and gate never see — rejected.
- `ReadMcpResource(server: string, uri: string) -> contents` — reads one resource through
  `connection.read_resource(uri)` and renders text and blob contents (blobs noted with their
  mime type and size, matching how the model is told about images). Unknown server or uri is a
  tool error naming what is connected. Results are capped with the same `OutputLimits` policy as
  MCP tool results (spill to `<scratch>/mcp-results/` over the limit), applied before the result
  pipeline so the scanner and redaction see what the model will.

Because both tools are wrapped by the same `Guard` as every other tool, floor → hooks → rules →
guardian apply in every mode, and the result passes through the scanner stage — the issue's
"gated; server content is untrusted and goes through the result scanner when enabled" falls out
of the existing choke point, with no new gate code.

### @-mention of resources

The TUI @-popup gains MCP resources as a second candidate source, keeping the completion-only
model (the message is sent as typed; no submit-time expansion is added):

- The runtime keeps `mcp_mentions: Arc<RwLock<Arc<Vec<McpResourceSummary>>>>`, refreshed after
  connect/reconnect/list_changed, in the same slot style as `AgentRuntime.files`. The driver
  copies it each draw next to `set_files` (`rust/fontaine-tui/src/ui/mod.rs:127`).
- `complete.rs` gains a structured candidate (`File(path)` / `Resource { server, uri, name,
  description }`) threaded through `rank()`/`CompletionList`/`accept_completion`, replacing the
  bare `Vec<String>` for mentions only. The `/` menu's `MenuEntry` is the in-repo model for a
  typed candidate list. Accepting a resource inserts `@<uri>` plus a trailing space, exactly like
  a file accept.
- The system prompt (assembly in `context.rs`) gains a short section, present only while at
  least one resources-capable server is connected: an `@<uri>` token names an MCP resource, and
  the model should call `ReadMcpResource` with the `server` from `ListMcpResources` to fetch its
  content (it must not pretend to know the content). This is the mechanism that keeps resource
  content behind the gate: the mention itself is inert text, and content crosses the permission
  pipeline only as a gated tool result.

### Prompt commands

`prompts/list` results become slash commands named `mcp__<server>__<prompt>`, with non-`[A-Za-
z0-9_-]` characters cleaned to `_` — the same `mcp_tool_name` cleaning, so rule grammar and
naming stay uniform.

- `AgentRuntime::slash_prompts() -> Vec<(name, hint, description)>` (name = the full
  `mcp__server__prompt` string, hint = the prompt's declared argument names, description = the
  server-supplied description) mirrors `slash_skills()`. Menu construction
  (`lib.rs:494` and `refresh_menu`) appends prompt rows; a prompt shadowed by a builtin or skill
  is hidden with a notice, same as skill shadowing.
- Dispatch: a `Command::Other` whose name starts with `mcp__` resolves against
  `slash_prompts()` before the skill lookup. Resolution path (mirroring `expand_skill_command`
  in `ui/turn.rs`): `refuse_if_stopped`, hook gate (`prompt_passes`), then
  `AgentRuntime::run_mcp_prompt(name, argument)` in core: parse `key=value` pairs out of the
  command argument, check the prompt's declared arguments, call `prompts/get` with them, and
  return the messages' concatenated text.
- Missing required arguments or a `key=value` parse failure does not open a dialog: the command
  fails with a notice naming the prompt's declared arguments and an example invocation. (A
  fill-in form is a possible follow-up; the notice keeps this release non-interactive beyond the
  existing surfaces.)
- On success the UI appends `Record::UserMessage { text: raw }` to the ledger (so the guardian
  judging the turn sees the invocation) and returns
  `CommandOutcome::Turn(Submission::Expanded { text: raw, prompt })`, so the transcript shows
  the command and the model receives the fetched text. On failure (server gone, `prompts/get`
  error, declined round) a transcript error appears and no turn starts.
- Prompt text is server-controlled, but injection happens only when the user types the command —
  the same boundary as running a skill shipped by a plugin. The fetched text is marked as
  harness/user-supplied in the transcript so its provenance is visible in scrollback; it is not
  scanner-staged (it enters as the user's own message, not as a tool result), matching how skill
  expansion is treated today.

### /mcp and telemetry

The bare `/mcp` notice gains two counts per connected server, `N resources` / `M prompts`,
next to the tool count (suppressed when zero — the common case stays one line). No new commands;
`enable`/`disable` stay unoffered. Telemetry: `ListMcpResources`/`ReadMcpResource` are built-in
tools with their own names; MCP prompt invocations record as user messages like skill runs.
`hide_mcp_names` already covers third-party naming for server-supplied content.

## Implementation Steps

1. **fontaine-tools — capabilities and listings.** Declare `resources` + `prompts` in
   `client_config()`; add a `Listings` type (`resources`, `prompts` per server) and a
   `pull_listings(connection) -> Listings` helper using `resources()` /
   `peer().list_all_prompts()`; extend `MetaTap` to forward `resources/list_changed` and
   `prompts/list_changed` notifications to a callback. Store listings on `Connected` (or in a
   side map keyed by server next to it — follow where `tool_names` lives).
2. **fontaine-tools — resource tools.** Implement `ListMcpResources` and `ReadMcpResource`
   against a `McpResourceQueries` trait (list servers+resources; read one resource), with
   `OutputLimits` capping on reads; add both to `builtins.rs` construction and two `Arbitrary`
   rows in `risk.rs`.
3. **fontaine-core — runtime wiring.** `agent/mcp.rs`/`mcp_supervisor.rs`: pull listings at
   connect/reconnect, refresh on the two notifications, drop them on remove; expose
   `mcp_mentions()` (resource summaries), `slash_prompts()`, and `run_mcp_prompt(name, argument)`
   on `AgentRuntime`; implement `McpResourceQueries` over the supervisor; pass it through
   `RegistryParams` in `tools.rs`/`agent/build.rs`; refresh the mention slot like `files`.
4. **fontaine-core — system prompt.** `context.rs`: the `@<uri>` + resource-tools section,
   emitted only while a resources-capable server is connected.
5. **fontaine-tui — mention completion.** Structured candidate through `complete.rs` /
   `CompletionList` / `accept_completion` / `view.rs` rows; wire the runtime mention slot next to
   `set_files`.
6. **fontaine-tui — prompt commands.** Menu merge in `lib.rs` + `refresh_menu`; the `mcp__*`
   dispatch arm in `dispatch_inner` next to `Command::Other`; the `run_mcp_prompt` UI wrapper
   following `expand_skill_command`; `/mcp` counts in `reports.rs`.
7. **Example server.** Extend `rust/fontaine/examples/repo_mcp_server.rs` with one resource and
   one prompt (and a `list_changed` poke if trivial) for manual verification.
8. **Docs.** `docs/mcp.md` — replace "Only tools are used; resources and prompts are not." with
   Resources and Prompts sections (tool names, gating, scanner, @-mention, command syntax,
   `key=value` arguments); `docs/tui.md` file-mentions section gains the resource mention;
   `docs/architecture.md:110` tool list gains the two new tools.
9. **CHANGELOG.md** entry.

Ordered so each step compiles: 1–2 are fontaine-tools, 3–4 core, 5–6 TUI, 7–9 polish.

## Files to Modify

- `rust/fontaine-tools/src/mcp/servers.rs` — capabilities, listings, notification forwarding
- `rust/fontaine-tools/src/mcp/meta.rs` — list_changed observation
- `rust/fontaine-tools/src/mcp.rs`, `rust/fontaine-tools/src/mcp/output.rs` — re-exports, read capping reuse
- `rust/fontaine-tools/src/builtins.rs`, `rust/fontaine-tools/src/risk.rs` — the two tools + risk rows
- `rust/fontaine-core/src/agent/mcp.rs`, `mcp_supervisor.rs` — listing lifecycle, runtime surface
- `rust/fontaine-core/src/agent.rs`, `agent/build.rs` — `RegistryParams` plumbing, mention slot
- `rust/fontaine-core/src/tools.rs` — registry field
- `rust/fontaine-core/src/context.rs` — system prompt section
- `rust/fontaine-tui/src/complete.rs`, `app.rs`, `view.rs`, `ui/mod.rs` — mention candidates
- `rust/fontaine-tui/src/lib.rs`, `ui/commands/plugin.rs`, `ui/mod.rs`, `ui/turn.rs` — prompt commands
- `rust/fontaine-tui/src/reports.rs` — `/mcp` counts
- `rust/fontaine/examples/repo_mcp_server.rs` — manual-verification server
- `docs/mcp.md`, `docs/tui.md`, `docs/architecture.md`, `CHANGELOG.md`

## Trade-offs

- **Per-resource tools vs two generic tools.** Per-resource tools (`mcp__server__resource__name`)
  would mirror per-tool registration and make reads individually allow-rule-able, but resources
  are data with URIs, not actions with schemas; templated resources don't fit a fixed schema at
  all, and a big listing would bloat every agent's toolset. The two generic tools match Claude
  Code's surface and keep the toolset constant-size. Chosen: generic tools.
- **@-mention expansion at submit vs completion-only.** Inlining content at submit (Claude
  Code's model) needs a new expansion point that would inject server content into the user
  message outside the gate, breaking the recorded 2026-10-22 decision's rationale; routing the
  mention through the gated `ReadMcpResource` tool keeps every byte of server content behind the
  floor and guardian. Chosen: completion-only, model reads via tool.
- **Always-registered vs on-connect resource tools.** Registering the pair only when a
  resources-capable server connects would keep small toolsets smaller, but toolset churn on MCP
  connect/reconnect already costs a cache invalidation (recorded decision), and the tools are
  cheap when they report "no servers". Always-registered is simpler and matches how `websearch`
  behaves when unconfigured. Chosen: always registered.
- **Transport tap vs `peer()` for prompts.** rig-rmcp exposes prompts only via `peer()`; a tap
  could intercept and serve `prompts/*` off the wire like elicitation, but nothing about prompts
  needs to leave rmcp's request path (no mid-request user interaction), and `peer()` is the
  documented escape hatch. Chosen: `peer()`.
- **`ListMcpResources` gated vs free.** A free listing tool would be quieter, but it is still a
  server round trip returning untrusted text; uniformity with the "MCP is never fast-pathed"
  invariant wins. Chosen: `Arbitrary`.

## Decisions

- Resources are read only through the two gated built-in tools and only listed via the same gate;
  no MCP surface bypasses the guardian or the scanner.
- MCP prompt text enters the conversation only as a user-invoked command's turn content, marked
  in the transcript; it is not scanner-staged (it is user-message content, like skill expansion).

## Documentation

- `docs/mcp.md`: new "Resources" and "Prompts" sections under Tool names (or siblings), covering
  the tools, gating, scanning, `@<uri>` mentions, `/mcp__server__prompt` syntax and `key=value`
  arguments; the "resources and prompts are not" sentence goes away.
- `docs/tui.md`: file-mentions section gains resource mentions.
- `docs/architecture.md`: the built-in tool list gains `ListMcpResources`/`ReadMcpResource`.
- No new docs page; nothing added to `docs/SUMMARY.md`.

## Testing Strategy

Unit and integration tests only; no live APIs (project rule). New coverage:

- `fontaine-tools` mcp module: listings parsed from a scripted in-process server
  (`connect_transport` + `tokio::time::pause`, the established pattern); read capping boundary;
  notification forwarding from `MetaTap`.
- `fontaine-core`: supervisor-level test with an injectable `McpConnector` that the resource
  tools call through (list with zero/one/many servers, unknown server/uri errors); the two risk
  rows asserted in `tests/choke_point.rs`; `slash_prompts()`/menu data; `run_mcp_prompt`
  argument parsing (present, missing-required, malformed) with a fake connection; system prompt
  section present/absent by capability; scanner-stage invocation on a resource read result (a
  fake scanner flagging content proves the pipeline path).
- `fontaine-tui`: candidate ranking/merge (file vs resource, no cross-contamination), accept
  insertion of `@<uri>`, prompt-command dispatch success/failure, shadowing notice, `/mcp`
  counts. Rendering checks run on `TestBackend`; anything touching the live area follows
  `tasks/design/testing.md`.
- End-to-end smoke: headless `fontaine -p` against the in-repo example server (step 7) asserting
  the tools list and a `ReadMcpResource` result through the fixture guardian — catches wiring a
  unit test cannot see (registry → runtime → gate).

## Manual Verification

1. `cargo run -p fontaine --example repo_mcp_server` run as an MCP server in a scratch config
   with a second stdio server from the MCP SDK that exposes a resource and a prompt; start
   `cargo run -p fontaine` in a trusted scratch workspace with both servers configured.
2. Type `@` — the popup shows files and the server's resource URIs (labelled with the server);
   accept one; the message sends with `@<uri>` verbatim.
3. Ask the model to read the mentioned resource — a `ReadMcpResource` call is gated (a prompt in
   `manual`, judged in `auto`) and its result reaches the model.
4. `/` menu lists `mcp__<example>__<prompt>`; invoking it with correct `key=value` arguments
   starts a turn whose transcript shows the invocation and whose model message contains the
   fetched prompt text; invoking with a missing argument prints the notice naming declared
   arguments and starts no turn.
5. `/mcp` shows `N tools · M resources · K prompts` for the connected server.
   Not automated because it needs real MCP servers and a live TTY; the automated tiers cover the
   parsing, gating and wiring, and a regression here is immediately visible to any user with an
   MCP server configured.

## Open Questions

- Should `fontaine mcp list`/`get` or `fontaine doctor` surface resource/prompt counts? Cheap to
  add from the same listings, but the issue doesn't ask; leave out unless review flags it.
- Should headless `fontaine -p` grow a way to invoke MCP prompts (e.g. an input prefix)? Slash
  commands don't exist headless at all today; consistent to skip.
