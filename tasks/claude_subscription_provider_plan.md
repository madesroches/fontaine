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
  (`rust/fontaine-core/src/config_scope.rs:157`; master plan Decisions, Providers).
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
  (`rust/fontaine-core/src/turn.rs:598`) and retries retryable failures twice
  (`rust/fontaine-core/src/turn.rs:386-435`). Authentication,
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
  valid only on this kind. `models` remains an explicit allowlist applied after live discovery.
- `validate` rejects `api_key_env`, `base_url`, `wires`, Ollama options, caching, quota, and all
  per-account network keys. `[network]` does not implicitly configure the official CLI; its own
  supported proxy/TLS environment is inherited. Update every `Account` literal.
- `ExternalCli` needs no Fontaine credential and never passes a stale stored credential to
  `connect`. Add an owned `env: SharedEnv` to `ConnectContext`; `ResolveParams` and `client_for`
  carry that `Arc`-owned, `Send + Sync + 'static` lookup into the CLI client and request
  transports. Each preflight checks current process variables by presence without calling
  `std::env` directly or reading their values. Production `shared_env` performs a live process
  lookup on each call rather than retaining a startup snapshot; borrowed `config::EnvLookup`
  remains only for configuration loading. Eval's owned environment includes every billing-conflict
  variable, not only `api_key_env` snapshots. Project `[env]` remains unrelated to provider
  operations.
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
   timeout and exit-status check. Start with **2.1.318** as the reference-qualified minimum;
   record the minimum in one constant and in docs. An old, malformed, timed-out, or unsupported
   response is a non-retryable CLI compatibility error, not a model error to retry.
2. Before discovery and before every model request, recheck inherited conflicts through the
   retained live `SharedEnv` and run `claude auth status` with the selected config directory.
   Parse only `loggedIn`, `authMethod`, and `subscriptionType`. Require logged-in `claude.ai`
   auth and a normalized Max or Ultra plan.
   Missing login, API-key auth, Pro/Team/Enterprise, `oauth_token`, third-party auth, or an unknown
   plan fails before any user turn.
3. Reject conflicting inherited variables by **name only**, never logging values: Anthropic API/
   auth/OAuth tokens and refresh tokens, custom endpoints/headers/betas/extra body, Foundry/AWS/
   Vertex credentials and backend switches, Anthropic profiles/federation, and inherited effort or
   fallback controls. Rejecting rather than scrubbing makes the required environment change
   visible and prevents a mid-session billing switch. Preserve ordinary CLI opt-outs, proxy/TLS
   settings, and the selected `CLAUDE_CONFIG_DIR`.
4. `list()` uses the same preflight, then sends only an `initialize` control request and closes
   stdin. It performs no user turn or Messages inference. Return the listed ids and retain a
   catalog containing each id's resolved canonical id (for example `sonnet` →
   `claude-sonnet-5-5`) and any usage-credit marker. Resolve each canonical id through rig's
   built-in Anthropic `Catalog` during discovery and retain its `ModelFacts`. An id lacking the
   required catalog spec or default output limit is a non-retryable compatibility error. The same
   handshake runs before every model request: reject the selected model whenever its current row
   says it draws from usage credits, and do not hide that billing class. Discovery/handshake
   failure is explicit; it never invents a static catalog. An explicit `models` list restricts
   selectable ids but never skips discovery or the request-time billing handshake.

Add a `Provider::requires_discovery()` default-false hook, true only here. Main and guardian
resolution call `list()` whenever it is true, even with `models` set or a main-account/cached
listing available, before constructing a rig handle. Configured and cached lists may filter a
completed discovery, never replace it. Guardian discovery/resolution is asynchronous through
`resolve_with`, `Switcher`, `ChildContext::model_for`, AgentRuntime's guardian operations, every
TUI host, and eval's live connection. This is necessary because an unset `max_tokens`, guardian
`tool_choice`, and compaction depend on the canonical model's catalog entry, not on a CLI alias.

Version/auth probes and discovery have bounded deadlines, including the currently unbounded initial
model-resolution path. A wrong account type or auth environment is a provider/setup failure with an
actionable message, not a guardian allow and not an API fallback.

### 3. Messages wire with a request-scoped CLI transport

Resolve the requested id through the discovered catalog first, bind its retained rig `ModelFacts`
with `with_facts`, then build the completion as
`Model::new(AnthropicConfig::new("").completion(canonical_model).with_facts(facts), ClaudeTransport)`.
Both rig and the CLI receive the canonical id, while `ModelRef` keeps the user's alias and the
completion's id/spec are canonical. This preserves default output limits, native
structured-output/tool-choice behavior, compaction facts, and alias selection without a custom
dialect or static Fontaine model table. A caller-supplied `max_tokens` remains that request's
output cap on a supported id, with the catalog default used only when unset; it does not admit an
id lacking the required spec/default output limit or bypass discovery. The empty placeholder
credential is never sent anywhere: the transport reads the
encoded body's bytes and discards the dummy URI and headers except `anthropic-beta`. Translate that
one non-secret header to the private settings' `env.ANTHROPIC_BETAS`, preserving context-bound
thinking and fine-grained tool streaming; an inherited beta setting stays refused. Keep the
official Anthropic dialect/facts for model capabilities, effort, modalities, context windows, and
native replay; the existing `Unpriced` filters remove rig's catalog cost at usage/telemetry sinks,
as they do for OpenCode Go.

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
- Derive a stable CLI session UUID from the Fontaine session id, caller namespace, canonical model,
  leading system, and first frame (SHA-256, formatted as a UUID). `AccountClient` gains
  `completion_for_agent(model, namespace)`, a default method that ignores the namespace, and
  `ModelHandle` retains its `Arc<dyn AccountClient>`. Pass `request.id` into
  `ChildContext::model_for`; after `$main`, `$guardian_default`, guardian-default fallback to
  `$main`, or an explicit account/model selection, rebuild every returned child handle's active
  conversation completion and cached scopes through the retained client's `completion_for_agent`
  using that id. `completion` uses the root namespace and `guardian_completion` the guardian
  namespace; a missing eval session id gets a fresh client UUID. Identical tasks in independent
  sessions or same-type subagents therefore cannot collide on any model-selection path.
  Compaction or a model change starts a new derived id, but no native conversation is resumed.
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
--prompt-suggestions false
--include-partial-messages --tools "" --system-prompt-file <file> --settings <file>
--setting-sources "" --strict-mcp-config --disable-slash-commands --max-turns 1
--permission-mode dontAsk --permission-prompts none --no-session-persistence
--session-id <derived-uuid> --mcp-config <file>
```

The private settings/environment disable auto memory, CLAUDE.md and other instructions, auto
compaction, native retries and non-streaming fallback, native model/refusal fallback, background
work, prompt suggestions, subagents/workflows/scheduling, tool search, auto-update, feedback, and
session-title work.
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
  When a terminal `message_delta` arrives, **buffer it instead of forwarding immediately**. Keep
  reading until `message_stop` and the final `result` have been validated, stop the child, then
  release the buffered delta; rig ends its read at that delta, before a following `message_stop`
  or `result` could otherwise be consumed. A cut stream that omits the delta, stop, or result, or
  leaves tool JSON open, is truncated, never accepted as a response.
- `assistant`: use only for native error extraction, not as a second text stream.
- `system/api_retry`, an unsolicited `prompt_suggestion`, a second response, any native subagent
  event, any built-in tool or permission execution, and any unexpected model are compatibility
  failures; terminate the process rather than accept hidden native agent behavior or a fallback.
- `result`: map native errors without echoing raw output. `error_max_turns` is a normal boundary
  only when the decoded response stopped for tool use. A successful result with no complete
  response, contradictory terminal events, or missing/duplicate final state is malformed.
- Accept exactly one complete Messages response and one final `result`, then stop the child before
  releasing the buffered terminal delta. Require `result.subtype = success` for a non-tool stop;
  `error_max_turns` is valid only for a complete tool-use stop and needs no synthetic continuation.
  A native process that hangs after its result is killed at that boundary. Usage comes from the
  Messages stream and final usage is cross-checked where available. `total_cost_usd`/`modelUsage`
  list-price equivalents are never presented as subscription invoices or overage evidence.

This is deliberately not Hermes' authorization-forwarding loopback admission relay. Killing at the
first complete response and rejecting retries/second responses is the credential-free boundary.
The live qualification must prove it on supported CLI versions. If the CLI can issue an additional
request before that boundary in a way that breaks correctness or billing expectations, stop the
experiment rather than add a relay that reads or forwards subscription credentials.

### 6. Failure and cancellation behavior

- Map missing/old/timed-out CLI version probes, logged-out or timed-out auth/discovery probes
  (including request-time handshakes), wrong plan, conflicting auth/backend environment,
  usage-credit model, malformed protocol, and unsupported history to distinct, non-retryable
  errors with safe corrective text. Authentication and plan/allowance/billing failures never earn
  the turn's two retries. Overloaded/server errors and genuinely truncated transport failures
  retain the existing bounded retry behavior. Preserve stable status/code metadata where rig
  supports it and never export raw provider messages to telemetry.
- A `CliChild` guard owns the process and temporary directory. Dropping the opening/frame stream,
  cancellation, timeout, a compatibility failure, or normal completion kills and reaps the whole
  tree: POSIX process group plus SIGTERM/grace/SIGKILL; Windows `CREATE_NEW_PROCESS_GROUP` plus
  `taskkill /T /F`. Do not change the existing shell/PowerShell direct-child semantics.
- Use a 180-second read-idle deadline, reset by valid output, for version/auth/discovery/model
  processes. Only model-process read-idle expiry maps to a retryable transport timeout without
  exposing raw output. Version expiry remains a non-retryable compatibility error; auth/discovery
  expiry, including request-time handshakes, is a non-retryable provider/setup failure.
  Version/auth probes use shorter outer bounds so listings and setup cannot hang indefinitely.
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
   - Add `ExternalCli`, account fields and validation, owned `ConnectContext.env`, provider
     registration, unpriced billing and the experimental notice in `providers/mod.rs`, `config.rs`,
     `models.rs`, `accounts.rs`, and `providers/claude_subscription/mod.rs`. Thread `SharedEnv`
     through `ResolveParams` and `client_for` into the retained CLI client/transports, make
     production `shared_env` live, and include every billing-conflict variable in eval's owned
     environment; keep borrowed `config::EnvLookup` only for configuration loading.
   - Update account literals and add external-auth rows in core/account/dialog tests.
2. **CLI preflight and discovery.**
   - Implement `providers/claude_subscription/cli.rs`: resolution, version/auth probes, conflict
     detection, bounded process launch, and `initialize` model discovery with canonical routes,
     credit flags, and rig catalog facts. Add `Provider::requires_discovery` and call it before
     every main/guardian handle; reject canonical ids without the required spec/default output
     limit as non-retryable compatibility errors.
   - Make `models::{resolve_guardian,build_guardian,default_guardian}` asynchronous and await them
     through `resolve_with`; adjust default/candidate availability so discovery-required accounts
     are not rejected before listing. Make `Switcher::{guardian,guardian_default}` asynchronous
     and await the latter in `ChildContext::model_for`. Explicit `models` and cached main-account
     listings remain allowlists applied only after `list()`, never discovery shortcuts.
   - Make `AgentRuntime::{guardian_for,set_guardian_model,retry_guardian,use_guardian}`
     asynchronous, await guardian retry in `credentials_changed`, and include required discovery
     when installing the main-model guardian fallback. Propagate the awaits through
     `ui/commands/model.rs` (including `answer_unavailable`), `ui/mod.rs`, `ui/turn.rs`, and
     `ui/providers.rs`; make eval `live::connect` (`fontaine-evals/src/live.rs:66`, currently sync) asynchronous;
     its caller (`runner.rs:582`, inside sync `run_score`) bridges with an explicit `block_on` on
     the existing `runtime()` helper (`runner.rs:403`). Update all synchronous callers/tests without adding a discovery-free wrapper.
   - Add the external-account flow in `fontaine-tui/src/provider_dialog.rs`, `ui/providers.rs`, and
     setup tests. `/login` gives CLI instructions and runs preflight; it never prompts for a key.
3. **Request transport and inert inventory.**
   - Implement `providers/claude_subscription/{transport,protocol,inventory}.rs` and the Messages
     wire construction with canonical rig `ModelFacts`. Add hidden inventory dispatch in
     `fontaine/src/{cli,lib}.rs` and `fontaine-evals/src/runner.rs`.
   - Implement history replay and caller-namespaced session ids: pass `request.id` into
     `ChildContext::model_for` and rebuild the active conversation completion/scopes through
     `completion_for_agent` for `$main`, `$guardian_default` (including `$main` fallback), and
     explicit account/model handles. Add tool aliasing, beta propagation, private settings/files,
     and the buffered stream-json request/response state machine.
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
- `rust/fontaine-core/src/{config,models,accounts,child_runner}.rs` and
  `rust/fontaine-core/src/agent/models.rs` — account fields, owned environment, asynchronous
  discovery/resolution and runtime guardian operations, notices, rows, and per-agent namespaces.
- `rust/fontaine-tui/src/{provider_dialog,setup}.rs` (`setup.rs` passes the owned env to
  `Switcher::new`, call at ~line 630),
  `rust/fontaine-tui/src/ui/{providers,mod,turn}.rs`, and
  `rust/fontaine-tui/src/ui/commands/model.rs` — external-CLI account flow and async guardian hosts.
- `rust/fontaine/src/{cli,lib,auto_mode,doctor_cmd,models_cmd}.rs` (`auto_mode`, `doctor_cmd`,
  `models_cmd` change only for the live `SharedEnv`),
  `rust/fontaine-evals/src/{runner,live}.rs` — live owned environment, internal inventory mode, and
  asynchronous CLI-aware live eval.
- Tests: new `rust/fontaine-core/tests/claude_subscription.rs`,
  `rust/fontaine-core/examples/claude_cli_double.rs`, `rust/fontaine/tests/claude_subscription.rs`,
  and the thin `#[path]` wrapper `rust/fontaine/examples/fontaine_claude_cli_double.rs`; updates to
  `fontaine-core/tests/{providers,models,accounts,account_ops,account_session,config,billing}.rs`,
  `fontaine-tui/tests/{providers,setup,tui}.rs`, and `fontaine/tests/{headless_smoke,models_cmd}.rs`.
  The one fake implementation has a public `main()` and is built as each package's own example
  target. Core tests launch `claude_cli_double`; fontaine tests launch `fontaine_claude_cli_double`.
  There is no shell script, second fake implementation, or manifest entry.
- Docs/decisions: `docs/{configuration,architecture,tui,headless,guardian,telemetry}.md`,
  new `docs/claude-subscription.md`, `docs/index.md`, `tasks/design/{config,testing,sessions}.md` (`testing.md` line ~106 `claude -p` wording),
  `AGENTS.md` (`claude -p` testing rule wording, ~line 132), `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`, and this plan's completion results.
- No new Rust or JavaScript dependency is expected; existing tokio/futures, rig, serde, sha2,
  tempfile, and uuid cover the implementation.

## Decisions

- 2026-11-04 (user): Do not require Anthropic approval before implementing the experimental
  adapter. Keep the policy constraints and re-check before stable.
- 2026-11-04 (user): Reuse existing issue #15 for the separate outbound “embed Fontaine through an
  Agent SDK” request; it is linked to #412 but is not part of this provider's scope.
- 2026-11-04 AFK: The official `claude` CLI is a production model transport for this provider; AGENTS.md's and `tasks/design/testing.md`'s manual/eval-only rule for `claude -p` covers Claude Code baselines and tests only, and tests never run the real CLI (they use the fake `claude_cli_double`). Wording is clarified, not weakened.
- 2026-11-04 AFK: Termination at the first complete response is treated as not proven to prevent every native retry/continuation on a CLI version; a CLI version is supported only after live qualification shows it, and unqualified versions fail closed (rejected by preflight).
- 2026-11-04 AFK: Protocol shapes outside the initial live matrix (older signed history, transformed/compacted history, unlisted models, credit entitlements) are unsupported and fail closed until qualified live.
- 2026-11-04 AFK: Usage-credit transitions are treated as not surfaceable early by any current CLI API; included-only use continues to require disabling credits in the Claude account, and the billing guard fails closed on any credit signal.

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
Anthropic endpoint, or real `claude` process enters unit/integration CI. When running one package's suite alone locally from `rust/`, prebuild that package's own example
first:

```sh
cargo build -p fontaine-core --example claude_cli_double
cargo test -p fontaine-core --test claude_subscription
cargo build -p fontaine --example fontaine_claude_cli_double
cargo test -p fontaine --test claude_subscription
```

A single-package `cargo test -p <pkg> --test claude_subscription` does not build a dependency
package's examples; CI's workspace `cargo test --no-run` builds every example first, so this applies
only to the local single-package run. The explicit targets produce the distinct
`claude_cli_double` and `fontaine_claude_cli_double` artifacts from the one implementation.

- **Config/account unit tests:** registry round trip; every accepted/rejected field; project scopes;
  external credential ignores stale Fontaine entries; accurate account row and `/login` behavior;
  billing notice and unpriced budget refusal without any inference request.
- **Preflight unit/process tests:** supported/current/malformed/old versions; missing/non-executable/
  timeout/nonzero probes; Max/Ultra acceptance; logged-out/API-key/other-plan refusal; every
  conflicting variable named without value disclosure; live lookups observe a conflict introduced
  after connection, and eval's owned lookup contains every conflict name; unrelated secrets absent
  from the rebuilt CLI environment and from the inert inventory's safe baseline; separate config
  directories; discovery sends `initialize` only, dedupes ids, and flags credit-billed rows.
  Explicit models and cached main-account listings still discover before every main/guardian
  handle, including default/fallback guardians and eval live connections. Fake argv capture
  asserts `--prompt-suggestions false` on every discovery/model invocation.
- **Translation unit tests:** canonical alias routes (including unset `max_tokens` and forced
  guardian choice), canonical rig facts/default output limits, and non-retryable rejection of ids
  lacking a required spec or default output limit even with explicit `max_tokens`; a supported
  request's explicit `max_tokens` is preserved. Assert `ModelRef` retains the alias while the
  erased completion's id and spec are canonical. Cover beta propagation, system/history/tool
  schema and generation fields, signed native blocks, stable session ids, identical independent
  sessions, and distinct same-type child namespaces on `$main`, `$guardian_default`, its `$main`
  fallback, and explicit selections; historical `shouldQuery` and zero-turn acks, final user/
  tool-result query, and unsupported prefill/roles/hosted tools.
- **Stream unit tests:** arbitrary chunk boundaries and CRLF; text, reasoning, tool args/results,
  stop reasons, usage, and alias restoration; no duplicate assistant text; terminal-delta buffering
  until `message_stop` and `result`, including a hang after result; malformed JSON, invalid UTF-8,
  missing/duplicate/contradictory terminal state, unsolicited `prompt_suggestion`, second response,
  retry, subagent, native tool/permission activity, unexpected model, and bounded output.
- **Lifecycle tests:** drop while starting/mid-stream/after result, read-idle expiry, early EOF,
  nonzero exit, stderr caps, temp cleanup, and descendant cleanup on Unix and Windows. Version,
  auth, and discovery expiries (including request-time handshakes) get zero setup retries, while
  model-process read-idle expiry gets two inference retries. The portable test proves the
  mechanism; the live Windows check below proves the official shim behavior.
- **Full headless integration:** fake two-call conversation streams an inert-MCP read, Fontaine's
  real gate executes it, and the next request contains its durable result. Prove native writes do
  not happen before Fontaine approval, forced guardian output works/fails closed, usage has tokens
  but no cost even with a fake `total_cost_usd`, resume/compaction preserve history, and auth/plan
  failures have the documented exit/retry counts. Assert no `prompt_suggestion` event or
  suggestion request occurs. Drive the real inventory mode independently and prove every
  `tools/call` is denied.
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
   `/resume`, `/clear`, `/compact`, and a subagent. Run `/compact` through the ordinary
   `claude/sonnet` alias without substituting its resolved id; require actual compaction success
   and assert the compacting CLI request carries the canonical model id. Expect each CLI request
   to contain only its own history, signed reasoning where supplied, no second responses/native
   tools, no unsolicited `prompt_suggestion` events or suggestion requests, and correct usage.
   This is the credential-free admission gate; any hidden additional request blocks shipping.
4. **Forced guardian protocol.** Run
   `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian live:claude/sonnet --filter 'attack:secret-exposure' --out <scratch>/guardian-attack.json`
   and the same command with `--filter benign`. Expect typed verdicts through the real CLI's
   aliased inert tool and fail-closed behavior on malformed/provider failures. This checks the
   actual forced `tool_choice`/extra-body protocol, not guardian scorecard quality; it never runs
   in CI.
5. **Cancellation.** During a long streamed response, press Esc in the TUI and terminate a headless
   run. Expect the CLI and descendants to exit, temp files to be removed, and the next turn to work.
   Repeat on Windows with the official executable form and a path containing spaces/non-ASCII text;
   portable tests cannot establish the official shim's process-tree behavior.
6. **Version/platform matrix.** Run checks 1–5 on minimum supported CLI **2.1.318** and the current
   stable on Linux, macOS, and Windows. At each version, verify the adjacent argv pair
   `--prompt-suggestions false` on every model invocation and the absence of `prompt_suggestion`
   events and suggestion requests; continue to reject any unsolicited suggestion/second response.
   Record exact versions and any untested failure class here. Re-read the policy links and update
   the dedicated doc before any stable claim; no stable designation is part of this issue.

## Open Questions

None outstanding; qualification-only questions were resolved as conservative stances (Decisions, `AFK:`).

## Completion results

Deterministic implementation, recorded for review:

- Offline suites: `fontaine-core/tests/claude_subscription.rs`, `fontaine/tests/claude_subscription.rs`,
  the protocol, inventory and CLI unit tests in `providers/claude_subscription`, the provider dialog
  and `/login` tests in `fontaine-tui`, and the registry/config round trips. All run against the
  scripted `claude_cli_double`; the real `claude` binary was never started.
- Not done, and blocking any stable designation or the move to `tasks/completed/`: the live Max and
  Ultra qualification under Manual Verification (every step needs a real subscription and the
  unmodified CLI). No CLI version, platform or protocol limitation has been observed live, so
  **2.1.318** is the planned minimum and not a qualified one, the stream-json frame and
  `initialize` model-row shapes (`value`, `resolvedModel`, the usage-credit marker) follow the
  documented Agent SDK shapes as understood and are unverified against a live CLI, and the
  credential-free admission result (no hidden second request before the first complete response)
  is unproven. A failed live admission result blocks shipping.
