# Claude Subscription Provider Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/412

## Overview

Add an experimental `claude-subscription` account kind that uses the user's installed, unmodified
official Claude Code CLI as a model-only transport over Agent SDK/headless stream-json. The CLI
owns Claude authentication and subscription billing; Fontaine keeps its own agent loop, guardian,
permission pipeline, tools, sessions, memory, and compaction authoritative. Reuse rig's Anthropic
Messages wire for request preparation, native history, stream decoding, and usage, replacing only
its HTTP transport with a request-scoped subprocess transport. Do not read or forward Claude
credentials, spoof Claude Code's identity, nest two agents, or silently select API-key, gateway,
cloud-backend, paid-credit, or fallback-model routing.

## Current State

- Providers implement `Provider` and `AccountClient` and are added to one registry
  (`rust/fontaine-core/src/providers/mod.rs:61-127,188-213,283-292`). `ConnectContext` carries the
  account, any Fontaine-stored credential, data directory, session id, and HTTP network settings
  (`providers/mod.rs:251-270`). OpenCode Go is subscription HTTPS, not a subprocess provider
  (`providers/opencode_go.rs:90-151,194-242`).
- `Account` is a strict, provider-extensible config shape (`rust/fontaine-core/src/config.rs:348-379`),
  validated through `check_account` (`config.rs:1053-1109`). Accounts are user-file/`--settings`
  only, so no additional scope rule is needed for fields under `[accounts.*]`
  (`rust/fontaine-core/src/config_scope.rs:130`; master plan Decisions, Providers).
- `CredentialPolicy` only covers required, optional, and Fontaine-hosted sign-in credentials
  (`providers/mod.rs:35-43`). `client_for` resolves those credentials at
  `rust/fontaine-core/src/models.rs:365-415`, and `accounts::rows` presents the same states at
  `rust/fontaine-core/src/accounts.rs:136-177`. `Optional` would misleadingly display “no key
  needed”; `SignIn` would incorrectly require a Fontaine OAuth entry.
- Model resolution lists an account when `models` is absent and then constructs a `ModelHandle`
  (`models.rs:483-530`). A handle erases a normal rig `DynModel<Completion>`, carries model facts
  and billing, and has separate conversation/guardian scopes (`models.rs:112-203,213-275`).
- Rig's `Model<W, T>` deliberately separates a wire from its transport
  (`rig-core-0.45.0/src/driver.rs:38-64,149-176`). The Anthropic Messages wire accepts an encoded
  HTTP-shaped payload and decodes `WireFrame`s (`providers/anthropic/wire.rs:548-627`). Its decoder
  preserves text, signed/redacted reasoning, native tool blocks, stop reasons, and usage
  (`providers/anthropic/streaming.rs:216-220,252-334,400-495`). Native provider items and their
  signatures survive canonical history through rig's `AssistantContent::with_native`
  (`completion/message.rs:206-220,370-394`). OpenRouter already demonstrates rebuilding a model
  with a selected wire and transport (`rust/fontaine-core/src/providers/openrouter.rs:52-65`).
- Root and `$main` subagents share the erased model transport
  (`rust/fontaine-core/src/child_runner.rs:333-356,520-568`). A single stateful Claude session
  therefore cannot serve all callers. Fontaine's turn runner drops the rig stream to cancel
  (`turn.rs:600`) and retries retryable failures twice (`turn.rs:386-435`). Authentication,
  allowance, and configuration failures must not be misclassified into that retry path.
- Subscription/unpriced handling already drops cost while preserving token usage
  (`models.rs:213-217`; `turn.rs:553-574`; `fontaine-guardian/src/guardian.rs:319-326`). The span
  bridge independently suppresses cost under `model.priced = false`
  (`rust/fontaine/src/telemetry/span_bridge.rs:429-439`). `--max-budget-usd` is rejected before a
  turn on an unpriced main model (`rust/fontaine/src/lib.rs:1012-1018`).
- Process lifecycle precedent lives in `fontaine-tools/src/procgroup.rs:1-80`,
  `websearch.rs:145-212`, and `lsp/client.rs:364-490,566-620`. The shared helper only kills the
  direct child on Windows, so this adapter needs its own Windows process-tree cleanup. The
  portable `fontaine-core/examples/hook_double.rs` and `fontaine-tools/examples/fake_lsp.rs` are
  the model for cross-platform fake executables.
- `claude --version` is currently used only to record a string in eval baselines
  (`rust/fontaine-evals/src/baseline.rs:247-254`); it is not a compatibility probe. Real `claude`
  invocations are manual/eval-only, never CI (`tasks/design/testing.md:106`).
- `tasks/completed/opencode_go_provider_plan.md` is the provider/billing precedent;
  `lsp_code_intelligence_plan.md` is the process-protocol precedent;
  `model_call_failure_recovery_plan.md` is the retry precedent; and
  `tui_provider_dialog_plan.md` plus the master Providers decisions define current account UX.

## Policy and Scope

Anthropic's pages reviewed on 2026-11-04 say that:

- Agent SDK and `claude -p` signed in with a Claude plan draw from subscription limits, while the
  separate monthly API-credit program applies when using a linked Console API key.
- An end user may sign in to an unmodified Claude Code binary with their own subscription, but the
  Agent SDK overview says third-party developers need prior approval to offer Claude.ai login or
  subscription rate limits. The login guidance also prohibits identity spoofing and third-party
  routing against subscription limits.

The adapter remains **experimental**. Initial scope is only ordinary, individual, local use of the
user's own Max or Ultra subscription through the official binary. No Pro/Team/Enterprise support,
credential sharing, resale, pooling, hosted/multi-tenant routing, `claude setup-token`, direct
Messages API use of `CLAUDE_CODE_OAUTH_TOKEN`, or import of `~/.claude` credentials. Anthropic's
terms must be checked again before any stable designation; shipping Hermes' equivalent adapter is
not evidence of permission.

Sources: [plan billing](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan),
[legal/compliance](https://code.claude.com/docs/en/legal-and-compliance),
[SDK overview](https://code.claude.com/docs/en/agent-sdk/overview),
[login rules](https://support.claude.com/en/articles/13189465-log-in-to-your-claude-account),
[monthly API credits](https://support.claude.com/en/articles/17154008-monthly-api-credits-for-max-ultra-and-team-plans),
[Hermes reference](https://hermes-agent.nousresearch.com/docs/plugins/claude-subscription-directsdk).

## Design

### 1. Account and external-auth configuration

```toml
[accounts.claude]
kind = "claude-subscription"
# Optional; normally just `claude auth login` and select `claude/sonnet`:
# cli_path = "/absolute/path/to/claude"
# claude_config_dir = "/absolute/path/to/separate/cli/config"
# models = ["sonnet", "opus", "haiku"]
```

- Register `ClaudeSubscription` in `providers/mod.rs` with `Billing::Unpriced`, fixed/no base URL,
  no native web search, no prompt-caching keys, and a new `CredentialPolicy::ExternalCli`. Do not
  use Fontaine's OAuth/token store or `SignIn` machinery.
- Add optional `cli_path` and `claude_config_dir` paths to `Account`. `cli_path` selects the
  official executable; default is `claude` on `PATH`. `claude_config_dir` becomes the child-only
  `CLAUDE_CONFIG_DIR`, allowing two isolated CLI profiles without reading either profile. Both are
  valid only on this kind. `models` remains the explicit escape from live discovery.
- `validate` rejects `api_key_env`, `base_url`, `wires`, Ollama options, caching, quota, and all
  per-account network keys. `[network]` does not implicitly configure the official CLI; its own
  supported proxy/TLS environment is inherited. Update every `Account` literal.
- `ExternalCli` needs no Fontaine credential and never passes a stale stored credential to
  `connect`. Add `env: EnvLookup` to `ConnectContext` so the provider checks process variables by
  presence without calling `std::env` directly or reading their values. Project `[env]` remains
  unrelated to provider operations.
- Add an external-CLI account-row state and experimental/billing notice. `/login` on this kind runs
  the CLI preflight and opens model selection instead of showing a key prompt or starting a nested
  interactive login. Its failure says exactly to run `claude auth login` externally. Removing or
  “logging out” the Fontaine account never changes or deletes the CLI's own login state.
- Selecting this account is the experimental opt-in. Main-model resolution emits a notice naming
  the official-CLI subscription path, unpriced usage, and the account's own extra-usage setting.
  Keep the existing unpriced `--max-budget-usd` usage error.

### 2. CLI preflight and billing guard

Create one `Cli` wrapper around executable discovery, version/auth probes, and process launch. It
uses fixed argv, never a shell, and keeps raw stdout/stderr out of logs. Every launch starts with
`env_clear` and rebuilds only the required environment: program/OS/user identity and temp paths,
locale, the selected CLI config dir, proxy/TLS settings, and privacy opt-outs. No unrelated
Fontaine account, telemetry, websearch, or other credential variable reaches the CLI. Set
`CLAUDE_CODE_MCP_ALLOWLIST_ENV=1` as well, so the inert inventory receives only Claude's safe
baseline rather than a copy of the CLI's own sensitive environment.

1. Resolve `cli_path` or `claude`, then run `claude --version` once per connected client with a
   timeout and exit-status check. Start with **2.1.263** as the reference-qualified minimum
   (Hermes qualified that protocol version); record the minimum in one constant and in docs. An
   old, malformed, timed-out, or unsupported response is a CLI compatibility error, not a model
   error to retry.
2. Before discovery and before every model request, recheck inherited conflicts and run
   `claude auth status` with the selected config directory. Parse only `loggedIn`, `authMethod`,
   and `subscriptionType`. Require logged-in `claude.ai` auth and a normalized Max or Ultra plan.
   Missing login, API-key auth, Pro/Team/Enterprise, `oauth_token`, third-party auth, or an unknown
   plan fails before any user turn.
3. Reject conflicting inherited variables by **name only**, never logging values: Anthropic API/
   auth/OAuth tokens and refresh tokens, custom endpoints/headers/betas/extra body, Foundry/AWS/
   Vertex credentials and backend switches, Anthropic profiles/federation, and inherited effort or
   fallback controls. Rejecting rather than scrubbing makes the required environment change
   visible and prevents a mid-session billing switch. Preserve ordinary CLI opt-outs, proxy/TLS
   settings, and the selected `CLAUDE_CONFIG_DIR`.
4. `list()` uses the same preflight, then sends only an `initialize` control request and closes
   stdin. It performs no user turn or Messages inference. Return resolved model ids from the
   response, deduplicated. The same handshake runs before every model request: reject the selected
   model whenever its current row says it draws from usage credits, and do not hide that billing
   class. Discovery/handshake failure is explicit; it never invents a static catalog. An explicit
   `models` list skips the picker listing, never the request-time billing handshake.

Version/auth probes and discovery have bounded deadlines, including the currently unbounded initial
model-resolution path. A wrong account type or auth environment is a provider/setup failure with an
actionable message, not a guardian allow and not an API fallback.

### 3. Messages wire with a request-scoped CLI transport

Build the completion as `Model::new(AnthropicConfig::new("").completion(model), ClaudeTransport)`.
The empty placeholder is never sent anywhere: the transport reads only the encoded body's bytes
and discards the dummy URI and headers. Keep the official Anthropic dialect/facts for model
capabilities, effort, modalities, context windows, and native replay; the existing `Unpriced`
filters remove rig's catalog cost at usage/telemetry sinks, as they do for OpenCode Go.

Each completion starts a fresh CLI process in an owner-only temporary working directory. It never
uses `--bare` (which disables subscription OAuth) or native resume/session persistence. Fontaine's
ledger and rig-memory remain the only authoritative conversation stores.

The request translation is:

- Read the Messages body's `system`, `messages`, `model`, `tools`, and generation fields. Write the
  system prompt, private settings, and inert tool manifest into the temporary directory. Use the
  file forms rather than OS argument/environment-size-sensitive values where available.
- Convert messages in order to stream-json frames. Historical user frames carry
  `shouldQuery: false`; each must produce a zero-turn result acknowledgment before replay
  continues. Historical assistant frames carry their native blocks, including signed thinking.
  Only the final nonempty user/tool-result frame queries. Reject assistant prefill, a mid-history
  role the CLI cannot represent, or lossy content instead of silently rewriting it.
- Derive a stable CLI session UUID from the model, leading system, and first frame (SHA-256,
  formatted as a UUID). This isolates root, guardian, and subagent histories despite their shared
  transport and changes naturally when compaction rewrites the beginning or the model changes. No
  native conversation is resumed.
- Expose request tools through a `fontaine` inert MCP inventory. Apply `mcp__fontaine__` plus a
  stable, reversible alias for names that need length/character normalization, and use exactly the
  same names in the extra-body tool definitions and inside `tool_choice`. Preserve schemas,
  descriptions, `max_tokens`, thinking/effort, output format, and supported generation fields;
  reject hosted tool definitions and unsupported parameters explicitly.
- Set the private settings' `env.CLAUDE_CODE_EXTRA_BODY` from the remaining generation fields and
  aliased tools, avoiding the OS environment-string limit. Map the encoded effort to `--effort`
  when present so native's own effort annotation cannot override it.

Fixed invocation flags:

```text
-p --model <model> --input-format stream-json --output-format stream-json --verbose
--include-partial-messages --tools "" --system-prompt-file <file> --settings <file>
--setting-sources "" --strict-mcp-config --disable-slash-commands --max-turns 1
--permission-mode dontAsk --permission-prompts none --no-session-persistence
--session-id <derived-uuid> --mcp-config <file>
```

The private settings/environment disable auto memory, CLAUDE.md and other instructions, auto
compaction, native retries and non-streaming fallback, native model/refusal fallback, background
work, subagents/workflows/scheduling, tool search, auto-update, feedback, and session-title work.
Set `CLAUDE_CODE_ENTRYPOINT=fontaine` and disable Artifact. Fontaine's own system prompt is used;
no Claude Code system prompt, user agent, authorization field, or other identity field is copied.
Unknown native customization is rejected at `system/init`, not trusted.

### 4. Internal inert MCP inventory

The external CLI only accepts tools through MCP, so implement a small stdio MCP inventory handler
in `fontaine-core` and expose it through a hidden internal mode in both `fontaine` and
`fontaine-evals` (the latter is needed for live guardian evals). The transport uses `current_exe`
by default and takes an injected inventory command in tests; no Python, Node, SDK package, second
shipped binary, or network listener is introduced.

The handler implements `initialize`, `tools/list`, and `tools/call`. Listing returns only the
request's schemas, each carrying `_meta["anthropic/requiresUserInteraction"] = true`. Every call
returns an error saying only Fontaine executes tools. `dontAsk` plus the interaction marker denies
native execution even if a rule changes; no callback can run Fontaine code inside the CLI. Unit
coverage drives the protocol directly, including unknown methods and malformed calls.

### 5. Stream decoding and completion boundary

Parse bounded NDJSON stdout while draining bounded stderr concurrently.

- `system/init`: require the requested model or its resolved alias target, only the inert MCP
  server, no plugin/MCP errors, and no unexpected tools or customizations. If the CLI exposes
  `EndConversation`, tolerate it only as native control flow and never return it as a Fontaine tool
  call; every other built-in is refused.
- `stream_event`: require `parent_tool_use_id = null`, one main response, and valid Anthropic event
  shapes. Rewrite aliased tool names back to Fontaine names on tool blocks, then pass the nested raw
  event as a `WireFrame` to rig's existing Messages decoder. This yields normal text/reasoning/
  argument deltas, native signed blocks, tool calls, finish reasons, and per-completion token usage.
  Completion requires a terminal `message_delta` and complete blocks, not just `message_stop`: a cut
  stream that omits the delta or leaves tool JSON open is truncated, never accepted as a response.
- `assistant`: use only for native error extraction, not as a second text stream.
- `system/api_retry`, a second response, any native subagent event, any built-in tool or permission
  execution, and any unexpected model are compatibility failures; terminate the process rather than
  accept hidden native agent behavior or a fallback.
- `result`: map native errors without echoing raw output. `error_max_turns` is a normal boundary
  only when the decoded response stopped for tool use. A successful result with no complete
  response, contradictory terminal events, or missing/duplicate final state is malformed.
- Accept exactly one complete Messages response, then stop the child immediately after its valid
  terminal delta and `message_stop`; a tool-use stop needs no successful process exit, final
  `result`, or synthetic continuation. A result received earlier is validated; successful completion
  never waits for one.
  Usage comes from the Messages stream. `total_cost_usd`/`modelUsage` list-price equivalents are
  never presented as subscription invoices or overage evidence.

This is deliberately not Hermes' authorization-forwarding loopback admission relay. Killing at the
first complete response and rejecting retries/second responses is the credential-free boundary.
The live qualification must prove it on supported CLI versions. If the CLI can issue an additional
request before that boundary in a way that breaks correctness or billing expectations, stop the
experiment rather than add a relay that reads or forwards subscription credentials.

### 6. Failure and cancellation behavior

- Map missing/old CLI, logged out, wrong plan, conflicting auth/backend environment, usage-credit
  model, malformed protocol, and unsupported history to distinct, non-retryable errors with safe
  corrective text. Authentication and plan/allowance/billing failures never earn the turn's two
  retries. Overloaded/server errors and genuinely truncated transport failures retain the existing
  bounded retry behavior. Preserve stable status/code metadata where rig supports it and never
  export raw provider messages to telemetry.
- A `CliChild` guard owns the process and temporary directory. Dropping the opening/frame stream,
  cancellation, timeout, a compatibility failure, or normal completion kills and reaps the whole
  tree: POSIX process group plus SIGTERM/grace/SIGKILL; Windows `CREATE_NEW_PROCESS_GROUP` plus
  `taskkill /T /F`. Do not change the existing shell/PowerShell direct-child semantics.
- Use a 180-second read-idle deadline, reset by valid output, for version/auth/discovery/model
  processes. Map it to a retryable transport timeout without exposing raw output. Version/auth
  probes use shorter outer bounds so listings and setup cannot hang indefinitely.
- Temporary files contain prompts/history, not credentials. Create them owner-only and remove them
  on every path, warning only with safe context on cleanup failure. Drain pipes to prevent deadlock
  and cap line/error buffers.

### 7. Billing and overage truth

The adapter never selects API-key or paid-credit routing and filters models the CLI marks as
credit-billed. Account-level extra usage can still switch a Max/Ultra session to paid usage credits
when an included limit is exhausted. The CLI owns that setting; Fontaine cannot reliably infer it
from token counts or list-price estimates. Setup text and docs must tell users to disable usage
credits at `https://claude.ai/settings/usage` if they require included-subscription-only use.
Native plan-limit/billing errors surface without an automatic retry or alternate account/model.
This is an explicit limitation of the experiment, not a claim of overage prevention.

## Implementation Steps

1. **Provider/config foundation.**
   - Add `ExternalCli`, account fields and validation, `ConnectContext.env`, provider registration,
     unpriced billing and the experimental notice in `providers/mod.rs`, `config.rs`, `models.rs`,
     `accounts.rs`, and `providers/claude_subscription/mod.rs`.
   - Update account literals and add external-auth rows in core/account/dialog tests.
2. **CLI preflight and discovery.**
   - Implement `providers/claude_subscription/cli.rs`: resolution, version/auth probes, conflict
     detection, bounded process launch, and `initialize` model discovery with credit flags.
   - Add the external-account flow in `fontaine-tui/src/provider_dialog.rs`, `ui/providers.rs`, and
     setup tests. `/login` gives CLI instructions and runs preflight; it never prompts for a key.
3. **Request transport and inert inventory.**
   - Implement `providers/claude_subscription/{transport,protocol,inventory}.rs` and the Messages
     wire construction. Add hidden inventory dispatch in `fontaine/src/{cli,lib}.rs` and
     `fontaine-evals/src/runner.rs`.
   - Implement history replay, stable session ids, tool aliasing, private settings/files, and the
     stream-json request/response state machine.
4. **Lifecycle, errors, and usage.**
   - Add process-tree cleanup, read-idle timeout, safe error mapping, native stream decoding, usage
     retention, and compatibility checks. Keep all existing permission/guardian paths unchanged.
5. **Docs and decisions.**
   - Add the documentation below, the changelog entry, and dated Decisions entries explaining that
     the adapter is experimental, reuses rig's Messages wire with only a new transport, is
     local/individual-only, and does not gate the experiment on Anthropic approval.
6. **Qualification.**
   - Run the deterministic suites, then the live Max and Ultra checks. Record exact CLI versions,
     platforms, protocol limitations, and any unqualified failure case in the plan before moving
     it to `tasks/completed/`. A failed credential-free admission result blocks shipping.

## Files to Modify

- `rust/fontaine-core/src/providers/mod.rs` — registration, external credential policy, context env.
- `rust/fontaine-core/src/providers/claude_subscription/{mod,cli,transport,protocol,inventory}.rs` — new provider.
- `rust/fontaine-core/src/{config,models,accounts}.rs` — account fields, resolution, notices, rows.
- `rust/fontaine-tui/src/{provider_dialog,setup}.rs`, `rust/fontaine-tui/src/ui/providers.rs` — external-CLI account flow.
- `rust/fontaine/src/{cli,lib}.rs`, `rust/fontaine-evals/src/{runner,live}.rs` — internal inventory mode and CLI-aware live-eval environment.
- Tests: new `rust/fontaine-core/tests/claude_subscription.rs`,
  `rust/fontaine-core/examples/claude_cli_double.rs`, and
  `rust/fontaine/tests/claude_subscription.rs`; updates to
  `fontaine-core/tests/{providers,models,accounts,account_ops,config,billing}.rs`,
  `fontaine-tui/tests/{providers,setup,tui}.rs`, and `fontaine/tests/{headless_smoke,models_cmd}.rs`.
  Both suites launch the one core example through the shared target `examples` path; no shell script
  or second fake implementation.
- Docs/decisions: `docs/{configuration,architecture,tui,headless,guardian,telemetry}.md`,
  new `docs/claude-subscription.md`, `docs/index.md`, `tasks/design/{config,testing,sessions}.md`,
  `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`, and this plan's completion results.
- No new Rust or JavaScript dependency is expected; existing tokio/futures, rig, serde, sha2,
  tempfile, and uuid cover the implementation.

## Trade-offs

- **Rig Messages wire + local transport vs a parallel provider implementation.** The wire already
  owns options, history, native signed items, streaming, and usage. Only SDK process translation is
  new. A separate completion type would duplicate rig and violate the intended provider seam.
- **Direct headless stream-json vs a TypeScript Agent SDK sidecar.** The CLI exposes the required
  protocol without a new npm/runtime/package surface. The inert MCP helper is the only auxiliary
  mode and stays in the existing executable.
- **Request-scoped replay vs persistent native sessions.** Full replay is more expensive but keeps
  Fontaine's history authoritative across subagents, retries, resume/fork, model switches, and
  compaction. Native sessions would mix callers and create a second source of truth.
- **Inert MCP inventory vs native tool execution.** MCP is how the model learns arbitrary Fontaine
  tool schemas. Requiring interaction plus `dontAsk` and an always-error handler makes the CLI a
  model-only producer; rig-agent performs every real call through the normal gate.
- **No admission relay vs Hermes' relay.** A relay would see authorization material and conflict
  with this issue's credential boundary. Immediate termination and strict second-response rejection
  are acceptable only if live qualification proves them sufficient; otherwise the experiment stops.
- **Reject conflicting environment vs silently scrub it.** Rejection exposes the required change
  and avoids depending on secret values or undocumented precedence. Normal CLI network and privacy
  settings continue unchanged.
- **Preflight auth on every request vs once at startup.** The extra local process is the cost of
  detecting account/auth changes before inference. Version parsing is cached per client.
- **Official Anthropic facts vs an unpriced custom dialect.** Official facts preserve effort,
  modalities, and compaction. Existing unpriced sinks already suppress catalog cost; a bare custom
  dialect would degrade core behavior and generation-option support.

## Decisions

- 2026-11-04 (user): Do not require Anthropic approval before implementing the experimental
  adapter. Keep the policy constraints and re-check before stable.
- 2026-11-04 (user): Reuse existing issue #15 for the separate outbound “embed Fontaine through an
  Agent SDK” request; it is linked to #412 but is not part of this provider's scope.

## Security

- Fontaine never opens `~/.claude` or credential stores, accepts Claude tokens in config, refreshes
  tokens, copies auth headers, or prints credential values. Only the official CLI touches its auth.
  Conflict checks inspect variable presence, and errors use fixed/sanitized native categories rather
  than raw CLI output.
- `cli_path`, CLI profile, and account settings are user/CLI-scope only. A repository cannot choose
  an executable, redirect billing, or inject project settings/hooks/MCP/instructions. The CLI runs in
  a private temp cwd with native customization disabled.
- The inert MCP server holds schemas only. Native built-ins, plugins, subagents, and tool execution
  are denied in depth. Real calls still pass through `GuardedTool`, the deterministic floor,
  guardian, result scanner, and ledger unchanged; provider/guardian failures fail closed.
- No hosted mode, credential pooling, subscription resale, or multi-tenant claim is supported.
  Temporary prompt files are owner-only and removed; logs and telemetry exclude raw CLI output and
  auth/config contents.

## Performance

A process and full history replay per model call adds startup and input/cache overhead, and auth
status plus `initialize` adds a local/billing preflight per request. This favors correctness and
isolation over native-session performance. Stable derived ids and unchanged initial history give
Claude's own cache the best available prefix without a relay or hand-managed cache markers. The
experimental docs must not promise native-session latency or cache rates; live qualification
records them instead.

## Documentation

- New `docs/claude-subscription.md`: experimental status and scope, install/login, Max/Ultra only,
  configuration examples, unpriced/token usage, `--max-budget-usd`, extra-usage warning, API-credit
  distinction, environment conflicts, supported CLI minimum/matrix, protocol limitations, and
  credential/policy guarantees.
- `docs/configuration.md`: kind/table fields, external credential source, model discovery, exit
  behavior, and a link to the dedicated page.
- `docs/architecture.md`: Messages-wire/subprocess-transport boundary and authoritative Fontaine
  loop; no second agent/session store.
- `docs/tui.md`: external-CLI account row, `/login` preflight/instructions, model picker, and the
  fact that account removal does not touch CLI auth.
- `docs/headless.md`: noninteractive CLI auth, error mapping, cancellation, and unpriced usage.
- `docs/guardian.md`: the same forced-tool/fail-closed behavior and unpriced guardian calls.
- `docs/telemetry.md`: token usage retained and cost absent, with no invoice inference.
- `docs/index.md`: link the new page. `CHANGELOG.md`: experimental account kind.
- `tasks/fontaine_harness_plan.md` Providers and `tasks/design/{config,testing,sessions}.md`:
  record the transport/auth/scope and fake-process/manual-live rules. Docs describe current
  behavior only; version/policy history stays in this plan and the changelog.

## Testing Strategy

All automated coverage is offline. Use a portable Rust CLI double, following `hook_double` and
`fake_lsp`, with scenario controls for version/auth output, argv/environment capture, stream chunks,
history assertions, malformed output, hangs, exit status, and descendants. No live credential,
Anthropic endpoint, or real `claude` process enters unit/integration CI.

- **Config/account unit tests:** registry round trip; every accepted/rejected field; project scopes;
  external credential ignores stale Fontaine entries; accurate account row and `/login` behavior;
  billing notice and unpriced budget refusal without any inference request.
- **Preflight unit/process tests:** supported/current/malformed/old versions; missing/non-executable/
  timeout/nonzero probes; Max/Ultra acceptance; logged-out/API-key/other-plan refusal; every
  conflicting variable named without value disclosure; unrelated secrets absent from the rebuilt
  CLI environment and from the inert inventory's safe baseline; separate config directories;
  discovery sends `initialize` only, dedupes ids, and flags credit-billed rows.
- **Translation unit tests:** system/history/tool schema and generation fields; signed native blocks;
  stable alias and session id; root/guardian/subagent isolation; historical `shouldQuery` and
  zero-turn acks; final user/tool-result query; unsupported prefill/roles/hosted tools.
- **Stream unit tests:** arbitrary chunk boundaries and CRLF; text, reasoning, tool args/results,
  stop reasons, usage, and alias restoration; no duplicate assistant text; malformed JSON, invalid
  UTF-8, missing/duplicate/contradictory terminal state, second response, retry, subagent, native
  tool/permission activity, unexpected model, and bounded output.
- **Lifecycle tests:** drop while starting/mid-stream/after result, read-idle expiry, early EOF,
  nonzero exit, stderr caps, temp cleanup, and descendant cleanup on Unix and Windows. The portable
  test proves the mechanism; the live Windows check below proves the official shim behavior.
- **Full headless integration:** fake two-call conversation streams an inert-MCP read, Fontaine's
  real gate executes it, and the next request contains its durable result. Prove native writes do
  not happen before Fontaine approval, forced guardian output works/fails closed, usage has tokens
  but no cost even with a fake `total_cost_usd`, resume/compaction preserve history, and auth/plan
  failures have the documented exit/retry counts. Drive the real inventory mode independently and
  prove every `tools/call` is denied.
- **Existing gates:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
  `cargo deny check`, both fixture evals, and the Windows GNU clippy check. CI runs on all three
  OSes; no live subscription qualification becomes a CI gate.

## Manual Verification

These checks use real subscriptions and the unmodified CLI, which a fake cannot qualify for actual
entitlement, server-side accounting, wire drift, or official launcher behavior. Never intentionally
exhaust a plan merely to produce an error.

1. **Max and Ultra tool sessions.** On an account of each plan, install/sign in externally, then
   create the settings file and run the session from `rust/`:

   ```toml
   # <scratch>/claude-subscription.toml
   [accounts.claude]
   kind = "claude-subscription"
   ```

   ```sh
   FONTAINE_CONFIG_DIR=<scratch>/config FONTAINE_DATA_DIR=<scratch>/data \
     cargo run -p fontaine -- --settings <scratch>/claude-subscription.toml \
       --model claude/sonnet -p "Read README.md and summarize it"
   ```

   Expect streamed output, the read through Fontaine's normal gate, a final answer, token usage,
   and no API key/cost. Repeat with an edit in manual mode and confirm no file changes before
   Fontaine approval. This qualifies actual subscription/tool behavior, not model quality.
2. **Auth and billing guards.** With an empty isolated `CLAUDE_CONFIG_DIR`, repeat the command and
   expect the external-login instruction without a prompt or inference. With
   `ANTHROPIC_API_KEY=test-placeholder`, expect refusal naming only that variable and no inference.
   Disable usage credits in the account for the included-only check; if a safe already-exhausted
   account exists, verify plan-limit text and zero retries, otherwise record it unqualified.
3. **Streaming and history.** In the TUI, run a longer task with at least five tool rounds plus
   `/resume`, `/clear`, `/compact`, and a subagent, selecting the full resolved model id if an alias
   has no catalog entry for compaction. Expect each CLI request to contain only its own history,
   signed reasoning where supplied, no second responses/native tools, and correct usage. This is
   the credential-free admission gate; any hidden additional request blocks shipping.
4. **Cancellation.** During a long streamed response, press Esc in the TUI and terminate a headless
   run. Expect the CLI and descendants to exit, temp files to be removed, and the next turn to work.
   Repeat on Windows with the official executable form and a path containing spaces/non-ASCII text;
   portable tests cannot establish the official shim's process-tree behavior.
5. **Version/platform matrix.** Run checks 1–4 on the minimum supported CLI and the current stable
   on Linux, macOS, and Windows. Record exact versions and any untested failure class here. Re-read
   the policy links and update the dedicated doc before any stable claim; no stable designation is
   part of this issue.

## Open Questions

No policy approval gates implementation (Decisions). The remaining questions are qualification
results, not preferences to settle by assumption:

- Does immediate termination at the first complete response prevent every problematic native retry/
  continuation on each supported CLI version, without sacrificing completed tool batches or usage?
- Which protocol shapes outside the initial live matrix (older signed history, transformed/
  compacted history, particular models, credit entitlements) must remain explicitly unsupported?
- Can account-level usage-credit transitions be surfaced early enough by a future official CLI API,
  or must included-only use continue to require disabling credits in the Claude account?
