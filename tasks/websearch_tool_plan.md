# WebSearch Tool Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/19

## Overview

Claude Code's `WebSearch` tool searches the web and returns titles, URLs and snippets. This plan adds a
`websearch` capability to Fontaine. Preference order: the provider's native (server-side) search when the
account's provider supports it; else a client-side `websearch` tool that calls a configured backend (Tavily, or
SearXNG, optionally launched on demand) over Fontaine's own HTTP client; else no `websearch` capability.
The first release has native search for `anthropic` accounts only; every other account kind uses the
client-side backend. The client-side tool keeps each search inside the permission pipeline; native search
cannot (see Permissions and security).

## Current State

- `webfetch` (`rust/fontaine-tools/src/webfetch.rs`) is the template: a rig `Tool` built with the
  shared `[network]` client (`crate::tls::build_client`), registered in
  `rust/fontaine-tools/src/builtins.rs` (~line 400) as `GuardedTool::single(tool, risk_of(NAME), guard)`.
- Every tool has one row in `rust/fontaine-tools/src/descriptor.rs` (`TOOLS`): risk class, `path_free`,
  `Destructive` kind, rule `Specifier` and Claude Code aliases. `webfetch` is `Arbitrary`,
  `Destructive::Always` (so `auto` mode sends it to the guardian), `Specifier::Domain`, alias `WebFetch`.
- `rust/fontaine-core/src/extensions.rs:468` rejects the Claude Code name `WebSearch` as an unknown tool;
  `docs/extensions.md:81` documents that.
- Tool results pass the `ResultPipeline` (`Guard.results`), which includes the optional result scanner.
- `rust/fontaine-core/src/tools.rs:~124` builds the registry with `WebfetchConfig::default()`; there is no
  user-facing webfetch config. `[network]` (`config.rs` `NetworkSection`, scope rule `user_or_cli` in
  `config_scope.rs:139`) is the closest config precedent. `[telemetry]` is the precedent for a section that
  needs a secret: `api_key_env` or a `[telemetry]` entry in `credentials.toml` (`credentials.rs`).
- `permissions::summarize` (`rust/fontaine-core/src/permissions.rs:~1694`) shows the human, and the
  guardian, a per-tool one-line summary of a call.
- The tool registry tests assert the exact registered set (`rust/fontaine-tools/tests/choke_point.rs`,
  `rust/fontaine-core/tests/choke_point.rs`); `descriptor.rs` pins the rows (`rows_are_pinned`,
  `[Pinned; 17]`) and has a hard-coded expected table in its unit tests.
- `rust/fontaine-core/prompts/system_prompt.md` lists tools statically (including `webfetch`) and has a
  snapshot test.
- `research/research-claude-code-parity.md:321` lists WebSearch as `missing`.

## Design

### Native search

For an `anthropic` account, the agent request carries a rig `ProviderToolDefinition` (`provider_tool` on
`AgentBuilder`; rig merges it into the request `tools`) with kind `web_search_20250305`, the version every
Anthropic hosting accepts (Azure supports only that one). rig-core 0.45.0 already round-trips
`server_tool_use` and `web_search_tool_result` blocks through history, resumes a `pause_turn`, maps
`web_search_result_location` citations and reads `usage.server_tool_use.web_search_requests`
(`providers/anthropic/{wire,streaming,extension}.rs`), so no custom history handling is needed.

- Attached where the builder is finished (`finish_builder` in `rust/fontaine-core/src/agent/build.rs`),
  decided per request from the current mode and rules, so a mode switch or rule change takes effect on the
  next turn.
- Only for accounts whose `base_url` is the Anthropic API (Databricks and other hosts are unverified), unless
  `[websearch] native = "on"` opts in. `native = "off"` forces the client-side backend. Default `auto`.
- Offered only when no `deny` or `ask` rule covers `websearch` (a bare `websearch` rule, or its `WebSearch`
  alias). In `auto` and `plan` it is offered otherwise; in `manual`, `acceptEdits` and `dontAsk` (where a
  non-allowed `websearch` call would ask or be denied) it is offered only when an `allow` rule for
  `websearch` exists. When native is not offered the client-side tool is registered if a backend is
  configured and is then subject to the same rules, so a `deny` disables both.
- If the provider rejects the request because of the native tool (an HTTP 400 naming it, as Azure does for
  other versions), Fontaine drops the tool, retries the request once without it, and keeps native off for the
  rest of the session, so the client-side tool takes over when a backend is configured. Any other error is not
  retried.
- Native searches are not tool calls in Fontaine, so they emit no tool events. They are logged from the
  response: each `server_tool_use` block is recorded in the debug log, and `usage.server_tool_use` counts are
  added to the session's usage record.

### Tool

The client-side `websearch` lives in a new `rust/fontaine-tools/src/websearch.rs`, shaped like `webfetch.rs`.

Arguments (`deny_unknown_fields`), matching Claude Code:

- `query: String`, required, non-empty.
- `allowed_domains: Option<Vec<String>>`, `blocked_domains: Option<Vec<String>>`. Applied by Fontaine to
  the returned results (host equals the domain or is a subdomain of it), so they work the same for every
  backend. Giving both is an `invalid_args` error, as in Claude Code.

Output: a numbered markdown list, one entry per result: title, URL, snippet. Capped at the
configured `max_results` (default 10) and at `output_cap_chars` (same default as `webfetch`), saying so when it
cuts. No results is a successful, plainly worded result, not an error.

Errors use `ToolExecutionError` constructors as `webfetch` does (`timeout`, `network`, `invalid_args`,
`other`), and the cause chain is rendered with `error_chain::chain` and `without_url()`. An HTTP 401/403/429
from the backend is reported with its status so the model and user can tell a bad key from a rate limit.
The API key never appears in an error or in the result.

### Backends

A small closed enum, not a trait, since each backend is one request builder and one response parser:

```
enum Backend { Tavily { api_key }, Searxng { base_url } }
```

- **Tavily**: `POST https://api.tavily.com/search` with JSON body `{query, max_results}` and
  `Authorization: Bearer <key>`. Results come from `results[]` (`title`, `url`, `content`).
- **SearXNG**: `GET <base_url>/search?q=…&format=json`. Results come from `results[]` (`title`, `url`,
  `content`). The instance must have the `json` format enabled; a 403 is reported with that hint.
  - **Launch on demand.** With `command` set, Fontaine starts the instance itself: fixed argv, no shell, run
    with `tokio::process::Command` and `kill_on_drop(true)` and the scrubbed environment of the MCP stdio
    launcher (`rust/fontaine-mcp/src/servers.rs`, `BASE_ENV`), whose rmcp `TokioChildProcess` is the same
    kill-on-drop lifecycle; there is no other shared child-process manager (`user_hooks.rs` and the guardian
    scanner are one-shot). The first search starts it, then polls `base_url` until it answers or
    `startup_timeout_secs` elapses; later searches reuse it. It is stopped when the session ends, including
    on interrupt, by dropping the handle. A spawn failure or readiness timeout is a search error with the
    cause chain (and the child's stderr tail); there is no fallback to another backend. SearXNG remains an
    external HTTP service, never bundled.

Each backend maps to the common `Hit { title, url, snippet }` list in one function, so domain filtering,
capping and formatting are shared. Brave and others are follow-ups (one more enum arm and one parser each).

`Websearch::new(config, &LoadedNetwork)` passes a closure to `tls::build_client` that sets
`.redirect(Policy::none())`, `.timeout(..)` and `.user_agent(FONTAINE_USER_AGENT)`, since `build_client`
sets none of them. Any redirect, for either backend, is returned as an error (stricter than `webfetch`,
which follows same-host redirects manually): reqwest does not strip custom headers such as the
`Authorization` header on a cross-host redirect, so `Policy::none()` is what keeps the key from being sent
to another host. A non-JSON or schema-mismatched body is an error that names the backend.

### Configuration

New `[websearch]` section in `config.rs` (`WebsearchSection`, `deny_unknown_fields`), scope rule
`user_or_cli` like `[network]`, so project files cannot choose where queries are sent, which key is used or
which command runs. `command` is treated like an MCP server or hook command: it never loosens a permission
decision.

| Key | Meaning | Default |
|---|---|---|
| `backend` | `tavily` or `searxng`. | unset: no client-side tool |
| `base_url` | SearXNG instance URL. Required for `searxng`; rejected for `tavily`. | unset |
| `command` | `searxng` only: argv (no shell) that starts the instance serving `base_url`, e.g. `["docker", "run", "--rm", ...]`. Rejected for `tavily`. | unset: Fontaine does not launch it |
| `startup_timeout_secs` | How long to wait for `base_url` to answer after launching. Needs `command`. | `60` |
| `native` | `auto`, `on` or `off`. `auto` uses provider-native search where supported; `on` also for non-Anthropic-API hosts of a supported kind; `off` never (see Native search). | `auto` |
| `api_key_env` | Env var holding the key (`tavily`). Omit to use the stored credential. | unset |
| `max_results` | Results returned, 1–20. | `10` |
| `timeout_secs` | Request timeout. | `30` |

- **Client-side tool registered only when a backend is configured.** With no `backend`, the client-side tool
  does not exist for the model, so there is no always-failing tool in the prompt; native availability does not
  require config. This is the one place the registered set depends on config.
- **Credentials.** A stored key lives in `credentials.toml` under `[websearch]` (`api_key`), kept apart from
  accounts like `[telemetry]`. `Credentials::websearch()` mirrors `Credentials::telemetry()`. Keys are never
  written inline in settings. A configured `tavily` backend with no resolvable key is a configuration error
  (exit 2), like telemetry, so a missing key fails at startup rather than at the first search.
  SearXNG needs no key. The entry is hand-edited like `[telemetry]`; there is no login command.
  - `check()` in `credentials.rs` is extended for the `[websearch]` entry: `api_key` only, no `oauth`
    (as for `[telemetry]`). A violation takes the existing `ConfigError::Credentials` path.
  - One function resolves the key (`api_key_env`, else the stored credential) and is used both where
    config is validated and where the tool is built, so the two cannot disagree (telemetry resolves twice:
    `resolve_telemetry` and `rust/fontaine/src/telemetry/mod.rs:~135-150`).
  - Any struct holding the key has a manual `Debug` that redacts it (precedent: `OAuthTokens`).
- Resolved in `config.rs` next to `resolve_telemetry`; exposed in `config_catalog.rs` so `/config` shows it,
  with the key redacted if shown at all.
- `command` validation: non-empty argv, only with `searxng`, and `startup_timeout_secs` only with `command`.
- `RegistryParams` (`fontaine-core/src/tools.rs`) gains `websearch: Option<WebsearchConfig>`, passed into
  `BuiltinEnv`, and `builtins::build` registers the tool when it is `Some`.
- The system prompt is left unchanged: the tool is conditionally registered, so a static line would name a
  tool that may be absent; the model gets the description from the registry.

### Permissions and security

- Descriptor row: `websearch`, `RiskClass::Arbitrary`, `path_free: false` (as `webfetch`), `Destructive::Always`,
  `Specifier::None`, aliases `&["WebSearch"]`. A bare `websearch` rule is how Claude Code's `WebSearch`
  rules map; the `domain:` form does not apply because the destination is the configured backend, not a
  model-chosen host.
- `Destructive::Always` plus `Arbitrary` means: never the read-only fast path, `auto` mode sends it to the
  guardian, `manual` asks. The guardian sees the query through `summarize`: `"websearch" => text("query")`.
- The deterministic floor and deny/ask rules apply as for any tool. No regex over the query.
- The result goes through the existing `ResultPipeline`, so the scanner sees snippets.
- **Native search is outside the guardian and the scanner.** It runs inside the model response, so the query
  is not checked per call and results are not scanned. The only gate is the request-time one in Native search.
- `webfetch`'s SSRF behavior is unchanged; search results can hand the model URLs to fetch, and that call is
  gated as any other `webfetch`.
- `Scope::locate("websearch", …)` is covered by the unreadable-arguments test like `webfetch`.

### Claude Code mapping

`map_tool_entry("WebSearch")` in `extensions.rs` now resolves through the descriptor alias. A skill or agent
that lists `WebSearch` in `allowed-tools` / `disallowedTools` gets the Fontaine tool. When no backend is
configured the tool is absent, so an allow entry is inert and a disallow entry still parses.

## Implementation Steps

1. `fontaine-tools`: add `websearch.rs` (args, `WebsearchConfig`, `Backend`, request builders, parsers,
   domain filter, formatter, `Tool` impl) and `pub mod websearch` in `lib.rs`.
2. `fontaine-tools`: add the `websearch` row to `descriptor.rs`; register it in `builtins::build` when
   `BuiltinEnv.websearch` is `Some`; add the lazily started, kill-on-drop SearXNG process handle.
3. `fontaine-core`: add `WebsearchSection`, its merge into `Config`, validation and resolution (backend
   shape, key present) in `config.rs`; the `user_or_cli` scope rule in `config_scope.rs`; catalog rows in
   `config_catalog.rs`; `Credentials::websearch()` in `credentials.rs`.
4. `fontaine-core`: thread `websearch` through `RegistryParams` in `tools.rs` and its call sites; add
   `"websearch"` to `permissions::summarize`.
5. Update `extensions.rs` test and `docs/extensions.md` (alias table at line 63 and the unsupported list at
   line 81) for the `WebSearch` mapping.
6. Tests (see below), including: `BuiltinEnv` literals in `rust/fontaine-tools/tests/result_pipeline.rs`
   (~40, ~122); `RegistryParams` literals in `rust/fontaine-core/tests/extensions.rs` (~315) and
   `rust/fontaine-core/src/agent/build.rs` (~144); the `rows_are_pinned` bump (`[Pinned; 17]` -> 18) and the
   new expected-table row in `descriptor.rs`; a `websearch` entry in the `SNIPPETS` table in
   `config_scope.rs` (~368, test `every_row_has_a_snippet_and_no_snippet_lacks_a_row`); one `row!` per
   `websearch.*` leaf in `config_catalog.rs`; a `websearch`/`WebSearch` entry in `TOOL_NAMES` in
   `rust/fontaine-evals/src/baseline.rs:34-46`.
7. `fontaine-core`: in `agent/build.rs`, attach the Anthropic `web_search_20250305` provider tool when the
   native gate allows; log `server_tool_use` blocks and usage counts.
8. Docs and changelog.

## Files to Modify

- `rust/fontaine-tools/src/websearch.rs` (new), `lib.rs`, `descriptor.rs`, `builtins.rs`
- `rust/fontaine-core/src/config.rs`, `config_scope.rs`, `config_catalog.rs`, `credentials.rs`, `tools.rs`,
  `agent/build.rs` (`finish_builder`: provider tool), `permissions.rs`, `extensions.rs`
- `rust/fontaine-evals/src/baseline.rs` (`TOOL_NAMES`)
- Tests: `rust/fontaine-tools/tests/websearch.rs` (new), `tests/common/mod.rs`, `tests/result_pipeline.rs`,
  both `choke_point.rs`, `scope.rs`; `rust/fontaine-core/tests/config.rs`, `permission_rules.rs`,
  `extensions.rs`, `destructive.rs`
- `docs/tools.md`, `docs/configuration.md`, `docs/permissions.md`, `docs/extensions.md`, `docs/guardian.md`
  (list `websearch` among network-reaching tools), `docs/architecture.md` (~108-113, tool list),
  `CHANGELOG.md`, `research/research-claude-code-parity.md`

## Decisions

- 2026-10-26: Provider-native search is used when the provider supports it (first release: `anthropic`
  accounts), else the client-side backend, else none. The user explicitly asked for this. Accepted risk:
  native queries are not seen by the guardian and native results are not seen by the result scanner; it is
  offered only when no `deny`/`ask` rule covers `websearch` (see Native search). More providers (OpenAI
  Responses `web_search`, xAI, Gemini grounding) are follow-up issues.
- 2026-10-26: SearXNG may be launched on demand by a fixed-argv `command` (user/CLI scope, lazy start, stopped
  at session end, no fallback on failure); the user asked for this. It stays an external service.
- 2026-10-26: First backends are SearXNG and Tavily (Brave's free tier needs a card); Brave is a follow-up.

## Documentation

- `docs/tools.md`: a "Web search" section (native vs client-side preference order, arguments, output shape,
  caps, backends, the `command` launch option; queries are sent to the provider or configured backend) beside "Web fetch", and a row in the tool table (`arbitrary`). Recommend
  `docker run searxng/searxng` on localhost with `base_url` (or the same as `command`) as the free, no-account path; SearXNG is AGPL
  and stays an external HTTP service, never bundled.
- `docs/configuration.md`: the `[websearch]` table (noting queries are sent to the configured backend; keys `command`,
  `startup_timeout_secs`, `native`), the credentials entry, scope (user/CLI only; scope table ~95-115), `websearch` in the rule-tool-name list
  (~527-530), and the rule that no backend means no client-side tool (native search needs no config).
- `docs/permissions.md`: `websearch` row in the rule-specifier table (bare rule only); when native search is
  offered per mode and that a `deny`/`ask` rule disables it.
- `docs/extensions.md`: `WebSearch` now maps to `websearch`; drop it from the unsupported list.
- `docs/guardian.md`: add `websearch` where network-reaching tools are listed, and state plainly that
  provider-native search is outside the guardian and the result scanner.
- `docs/` TOML blocks are schema-validated by `rust/fontaine-core/tests/docs.rs`, so examples must parse.

## Testing Strategy

All offline: `wiremock` stands in for the backends.

- `fontaine-tools/tests/websearch.rs`: Tavily (POST body, bearer header) and SearXNG (path, query) request
  shape and response parsing into the formatted list; domain allow/block filtering including
  subdomain and both-given error; `max_results` and output caps with the cut notice; empty results; HTTP
  401/403/429/500 mapped to distinct messages; SearXNG JSON-disabled 403 hint; malformed body; timeout;
  redirect returned as an error for both backends; the key absent from every error; one short test that a
  search goes through the configured `[network]` proxy (like `webfetch_goes_through_the_configured_proxy`).
- Native search (`fontaine-core`): the request carries the `web_search_20250305` provider tool only when the
  gate allows (per mode, including the allow-rule requirement); a `deny` or `ask` rule removes it; non-Anthropic
  account kinds and non-Anthropic `base_url` (unless `native = "on"`) use the client-side backend; `native =
  "off"` does too; a wiremock Anthropic response with `server_tool_use` + `web_search_tool_result` blocks (and
  `usage.server_tool_use`) round-trips through history and the next request replays them; `server_tool_use`
  blocks are logged and usage counts recorded; a 400 naming the tool is retried once without it and keeps
  native off for the session, while another 400 is not retried.
- SearXNG launch (`fontaine-tools/tests/websearch.rs`): a fake child script that serves the responder
  starts once across several searches, is terminated when the tool is dropped, and a script that exits
  non-zero or never answers surfaces a start failure / readiness timeout with the chain and no fallback;
  config tests cover `command` shape, project scope rejected, `startup_timeout_secs` without `command`.
- `descriptor` / choke-point tests: `rust/fontaine-core/tests/choke_point.rs` computes `expected` as `TOOLS`
  minus `websearch` when unconfigured, with a separate configured case where it is included; `websearch`
  is handled in the `built_ins` / `expected - built_ins == extras` equality and the `len()==8` no-extension
  case. The pinned descriptor row marks `websearch` `Arbitrary`.
- `fontaine-core/tests/config.rs`: `[websearch]` parses; unknown keys rejected; project scope rejected;
  `searxng` without `base_url` and `tavily` with `base_url` rejected; `tavily` without a key is exit-2; key from
  `api_key_env` and from `credentials.toml`; credentials file with `[websearch]` round-trips.
- `permission_rules.rs`: bare `websearch` rule parses, a `domain:` specifier is rejected, `WebSearch` alias
  resolves in rules and in `extensions` frontmatter.
- `rust/fontaine-core/tests/destructive.rs` (~247): a one-line assertion that `websearch` is destructive
  (never the fast path, guardian in `auto`).
- Optionally one exfiltration-in-query case in `evals/dataset`.

## Manual Verification

Only the live backends are checked by hand, since tests never touch live APIs.

1. Set `[websearch] backend = "tavily"` and `api_key_env`, run `fontaine -p "search the web for rust 2024
   edition and list three URLs"`; expect a `websearch` tool call and cited URLs. This catches a Tavily response
   shape change that wiremock fixtures cannot.
2. Point a SearXNG instance with `json` enabled at `base_url` and repeat.

## Open Questions

None.
