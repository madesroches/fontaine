# WebSearch Tool Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/19

## Overview

Claude Code's `WebSearch` tool searches the web and returns titles, URLs and snippets. This plan adds a
`websearch` tool to Fontaine that runs **entirely in the client**: Fontaine calls a configured search
backend (Brave Search API or a self-hosted SearXNG) over its own HTTP client and returns the results to
the model as an ordinary tool result. It does not use provider-native (server-side) search.

Running it in the client keeps every search inside the permission pipeline. A provider-native search
executes inside the model response, so the deterministic floor, the guardian (the query text is an
exfiltration channel) and the tool-result scanner never see it. It also works for every account kind,
including Ollama and gateways with no hosted search.

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
  `rust/fontaine-core/tests/choke_point.rs`).
- `research/research-claude-code-parity.md:321` lists WebSearch as `missing`.

## Design

### Tool

`websearch` in a new `rust/fontaine-tools/src/websearch.rs`, shaped like `webfetch.rs`.

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
enum Backend { Brave { api_key }, Searxng { base_url } }
```

- **Brave**: `GET https://api.search.brave.com/res/v1/web/search?q=…&count=N` with the
  `X-Subscription-Token` header. Results come from `web.results[]` (`title`, `url`, `description`).
  Description may contain `<strong>` tags, which are stripped.
- **SearXNG**: `GET <base_url>/search?q=…&format=json`. Results come from `results[]` (`title`, `url`,
  `content`). The instance must have the `json` format enabled; a 403 is reported with that hint.

Each backend maps to the common `Hit { title, url, snippet }` list in one function, so domain filtering,
capping and formatting are shared. Tavily is left to a follow-up issue (one more enum arm and one
parser; Tavily takes a POST with the key in the body).

Redirects are not followed (`Policy::none()`), as `webfetch` does, so the key header is never sent to a
different host. A non-JSON or schema-mismatched body is an error that names the backend.

### Configuration

New `[websearch]` section in `config.rs` (`WebsearchSection`, `deny_unknown_fields`), scope rule
`user_or_cli` like `[network]`, so project files cannot choose where queries are sent or which key is used.

| Key | Meaning | Default |
|---|---|---|
| `backend` | `brave` or `searxng`. | unset: the tool is not registered |
| `base_url` | SearXNG instance URL. Required for `searxng`; rejected for `brave`. | unset |
| `api_key_env` | Env var holding the key (`brave`). Omit to use the stored credential. | unset |
| `max_results` | Results returned, 1–20. | `10` |
| `timeout_secs` | Request timeout. | `30` |

- **Not registered unless configured.** With no `backend`, the tool does not exist for the model, so there is
  no always-failing tool in the prompt. This is the one place the registered set depends on config.
- **Credentials.** A stored key lives in `credentials.toml` under `[websearch]` (`api_key`), kept apart from
  accounts like `[telemetry]`. `Credentials::websearch()` mirrors `Credentials::telemetry()`. Keys are never
  written inline in settings. A configured `brave` backend with no resolvable key is a configuration error
  (exit 2), like telemetry, so a missing key fails at startup rather than at the first search.
  SearXNG needs no key.
- Resolved in `config.rs` next to `resolve_telemetry`; exposed in `config_catalog.rs` so `/config` shows it,
  with the key redacted if shown at all.
- `RegistryParams` (`fontaine-core/src/tools.rs`) gains `websearch: Option<WebsearchConfig>`, passed into
  `BuiltinEnv`, and `builtins::build` registers the tool when it is `Some`.

### Permissions and security

- Descriptor row: `websearch`, `RiskClass::Arbitrary`, `path_free: true`, `Destructive::Always`,
  `Specifier::None`, aliases `&["WebSearch"]`. A bare `websearch` rule is how Claude Code's `WebSearch`
  rules map; the `domain:` form does not apply because the destination is the configured backend, not a
  model-chosen host.
- `Destructive::Always` plus `Arbitrary` means: never the read-only fast path, `auto` mode sends it to the
  guardian, `manual` asks. The guardian sees the query through `summarize`: `"websearch" => text("query")`.
- The deterministic floor and deny/ask rules apply as for any tool. No regex over the query.
- The result goes through the existing `ResultPipeline`, so the scanner sees snippets.
- `webfetch`'s SSRF behavior is unchanged; search results can hand the model URLs to fetch, and that call is
  gated as any other `webfetch`.
- The `names_no_path`/scope logic needs no change because `path_free` is set. `Scope::locate("websearch", …)`
  is covered by the unreadable-arguments test like `webfetch`.

### Claude Code mapping

`map_tool_entry("WebSearch")` in `extensions.rs` now resolves through the descriptor alias. A skill or agent
that lists `WebSearch` in `allowed-tools` / `disallowedTools` gets the Fontaine tool. When no backend is
configured the tool is absent, so an allow entry is inert and a disallow entry still parses.

## Implementation Steps

1. `fontaine-tools`: add `websearch.rs` (args, `WebsearchConfig`, `Backend`, request builders, parsers,
   domain filter, formatter, `Tool` impl) and `pub mod websearch` in `lib.rs`.
2. `fontaine-tools`: add the `websearch` row to `descriptor.rs`; register it in `builtins::build` when
   `BuiltinEnv.websearch` is `Some`.
3. `fontaine-core`: add `WebsearchSection`, its merge into `Config`, validation and resolution (backend
   shape, key present) in `config.rs`; the `user_or_cli` scope rule in `config_scope.rs`; catalog rows in
   `config_catalog.rs`; `Credentials::websearch()` in `credentials.rs`.
4. `fontaine-core`: thread `websearch` through `RegistryParams` in `tools.rs` and its call sites; add
   `"websearch"` to `permissions::summarize`.
5. Update `extensions.rs` test and `docs/extensions.md` for the `WebSearch` mapping.
6. Tests (see below).
7. Docs and changelog.

## Files to Modify

- `rust/fontaine-tools/src/websearch.rs` (new), `lib.rs`, `descriptor.rs`, `builtins.rs`
- `rust/fontaine-core/src/config.rs`, `config_scope.rs`, `config_catalog.rs`, `credentials.rs`, `tools.rs`,
  `permissions.rs`, `extensions.rs`
- Tests: `rust/fontaine-tools/tests/websearch.rs` (new), `tests/common/mod.rs`, both `choke_point.rs`,
  `scope.rs`; `rust/fontaine-core/tests/config.rs`, `permission_rules.rs`
- `docs/tools.md`, `docs/configuration.md`, `docs/permissions.md`, `docs/extensions.md`, `docs/guardian.md`
  (list `websearch` among network-reaching tools), `CHANGELOG.md`, `research/research-claude-code-parity.md`

## Trade-offs

- **Provider-native search (rejected).** It bypasses the floor, the guardian's query check and the result
  scanner; rig only forwards the tool definition (`ProviderToolDefinition`) and each dialect differs
  (Anthropic `web_search_*`, OpenAI, xAI, Gemini grounding); Azure supports only one version, Databricks is
  unknown, and Ollama and most gateways have none. It also cannot be replayed through wiremock.
- **Trait vs. enum for backends.** Two backends that differ in one request and one parser do not justify a
  trait; the enum is replaced by a trait only when a third needs state the enum cannot hold.
- **Not registering when unconfigured vs. registering and failing.** An always-failing tool wastes prompt
  tokens and invites retries.
- **Domain filters in the client vs. passing them to the backend.** Backends disagree on whether and how they
  filter; filtering the returned list is uniform and testable.

## Decisions

- 2026-10-26: `websearch` runs in the client; provider-native search is not used. It would bypass the
  guardian and result scanner.
- 2026-10-26: First backends are Brave and SearXNG; Tavily is a follow-up issue.

## Documentation

- `docs/tools.md`: a "Web search" section (arguments, output shape, caps, backends) beside "Web fetch", and a
  row in the tool table (`arbitrary`).
- `docs/configuration.md`: the `[websearch]` table, the credentials entry, scope (user/CLI only), and the
  rule that no backend means no tool.
- `docs/permissions.md`: `websearch` row in the rule-specifier table (bare rule only).
- `docs/extensions.md`: `WebSearch` now maps to `websearch`; drop it from the unsupported list.
- `docs/guardian.md`: add `websearch` where network-reaching tools are listed.
- `docs/` TOML blocks are schema-validated by `rust/fontaine-core/tests/docs.rs`, so examples must parse.

## Testing Strategy

All offline: `wiremock` stands in for the backends.

- `fontaine-tools/tests/websearch.rs`: Brave and SearXNG request shape (path, query, auth header, count) and
  response parsing into the formatted list; `<strong>` stripping; domain allow/block filtering including
  subdomain and both-given error; `max_results` and output caps with the cut notice; empty results; HTTP
  401/403/429/500 mapped to distinct messages; SearXNG JSON-disabled 403 hint; malformed body; timeout;
  redirect not followed; the key absent from every error; routing through the configured `[network]` proxy
  (like `webfetch_goes_through_the_configured_proxy` in `tests/network.rs`).
- `descriptor` / choke-point tests: the registered set is `EXPECTED` plus `websearch` when configured and
  unchanged when not; `websearch` is `Arbitrary` and has a row.
- `fontaine-core/tests/config.rs`: `[websearch]` parses; unknown keys rejected; project scope rejected;
  `searxng` without `base_url` and `brave` with `base_url` rejected; `brave` without a key is exit-2; key from
  `api_key_env` and from `credentials.toml`; credentials file with `[websearch]` round-trips.
- `permission_rules.rs`: bare `websearch` rule parses, a `domain:` specifier is rejected, `WebSearch` alias
  resolves in rules and in `extensions` frontmatter.
- Permission pipeline: `websearch` never takes the fast path in any mode and reaches the guardian in `auto`
  with the query in its summary (a case added to the existing `destructive`/`pipeline` fixtures and to
  `evals/dataset` as a benign and an exfiltration-in-query case).

## Manual Verification

Only the live backends are checked by hand, since tests never touch live APIs.

1. Set `[websearch] backend = "brave"` and `api_key_env`, run `fontaine -p "search the web for rust 2024
   edition and list three URLs"`; expect a `websearch` tool call and cited URLs. This catches a Brave response
   shape change that wiremock fixtures cannot.
2. Point a SearXNG instance with `json` enabled at `base_url` and repeat.

## Open Questions

None.
