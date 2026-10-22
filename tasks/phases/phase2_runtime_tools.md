# Phase 2 — Agent runtime & tools

**Status:** not started · **Depends on:** [Phase 1](phase1_model_headless.md) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Tools: rig-tools + a risk-metadata registry + GuardedTool wrapper (fontaine-tools)

Reuse rather than rewrite. Fontaine does **not** define its own `Tool` trait — it builds on
`rig_core::tool::Tool` and adds the two things rig doesn't have for our security model: a
**risk classification** per tool and a **guard wrapper** that enforces the permission pipeline.

- **Base tools**: `rig-tools` 0.45.0 ships `read`, `write`, `edit`, `search`, `list`, `shell`,
  `delete`, `move`. Fontaine registers exactly the set in the step-1 table: `read`/`write`/`edit`/
  `shell` are rig tools wrapped as-is; `ls`/`glob`/`grep` are new first-party `Tool` impls that
  delegate to rig's `list`/`search` (rig's own `list`/`search` are not registered: `GuardedTool<T>`
  takes `NAME = T::NAME`, so a rig tool cannot be renamed, and a second registration under the same
  name would clash); `delete`/`move` are not registered. rig tools are configured (`confine: true`,
  `max_timeout`, output `cut`) rather than forked or wrapped in a hardening layer — keeping upgrades
  cheap.
- **Composite read tools + Tier 0.5 shell pre-classifier**: frequent read operations are also
  exposed as dedicated first-party tools (`ls`, `read`, `glob`, `grep`) with workspace containment
  enforced inside the tool at the execution boundary, and the system prompt steers the model toward
  them rather than `shell`. The Tier 0.5 shell pre-classifier is pipeline step 5 of `tasks/design/permissions.md` §Core data flow, built in
  Phase 3.
- **Risk registry**: a `RiskClass { ReadOnly | WorkspaceWrite | Arbitrary }` assigned per tool at
  registration (rig's registry supports ordered registrations). This is the deterministic,
  non-LLM half of the hybrid gate: `ReadOnly` + args validated within workspace scope (a
  `fontaine-tools` scope/symlink validator, deterministic code — not regex-on-args) ⇒ pipeline
  auto-allows without a guardian call. **Read ≠ safe**: a read outside the
  workspace (`~/.ssh/*`, cloud credential files, `.env`, browser cookie stores, metadata
  endpoints) is a classic recon/exfiltration step, so containment inside the workspace is what
  earns the fast path, never the read-only label; only Fontaine-enforced containment earns
  the fast path; MCP tools register `Arbitrary`.
- **GuardedTool**: a decorator implementing `rig_core::tool::Tool` that wraps any inner tool and,
  in its `call`, asks the injected `ToolGate` (`fontaine-tools/src/gate.rs`; implemented by
  `fontaine-core`'s `PermissionPipeline` — floor → mode check → read-only fast path → Tier-0.5 shell
  pre-classifier → guardian in `auto`) before delegating to the inner tool. `fontaine-tools` never
  depends on `fontaine-core`. This is the **single choke point**: every tool — rig built-in,
  Fontaine-added, or MCP-imported — is registered wrapped (registry-level wrapping covers built-ins,
  per-root instances and MCP), so nothing can execute without passing the gate. rig
  `AgentHook::on_dispatch` is not used for permission decisions; rig hooks stay bookkeeping-only.
- **ResultPipeline** (`fontaine-tools/src/result_pipeline.rs`): the post-call step in `GuardedTool::call`
  after the inner tool returns; every tool result — built-in, MCP, `agent` report — passes through it
  before the agent sees it, in fixed order: (Phase 6) the tool-result scanner, then (Phase 5)
  `post_tool_use` user hooks. Both stages are optional and absent in this phase (the pipeline is the
  identity); later phases attach stages to it rather than adding their own hook points.
- **Session-state carrier**: the pipeline's inputs (mode, workspace roots, merged permission rules
  and session grants, the session ledger handle, audit-log sink, and the human-approval callback;
  the user-hook runner joins in Phase 5) live in a shared `Arc<SessionState>` owned by
  `fontaine-core`'s pipeline, which is injected into every wrapper at registration as an
  `Arc<dyn ToolGate>`; rig's per-call `&mut ToolContext`
  carries only per-call identity (agent id) for per-agent audit keying. Because rig-core 0.45.0's
  `Tool` has a `const NAME` and typed `Args`, the wrapper is a generic `GuardedTool<T: Tool>`
  (`NAME = T::NAME`) for built-ins; MCP tools arrive as `DynamicTool` and are wrapped by a parallel
  `DynamicTool` wrapper path. Both call the same `ToolGate` entry point.

`WorkspaceWrite` (write/edit within workdir) is auto-allowed only in `acceptEdits`/`auto`(guardian)
per mode. `Arbitrary` (shell, webfetch, MCP tools) always hits the guardian in `auto` (shell calls
may be pre-classified safe at Tier 0.5 first — pipeline step 5 precedes the mode branches, matching Claude
Code's read-only Bash set prompting in no mode), prompts in `manual`.

### Workspace roots

`scope.rs` validates against an ordered set of canonicalized roots (`tasks/design/config.md` §Config): a path is in
scope when `rig_tools::fs::place(root, true, path)` accepts it for some root, after resolving it
against the call's `cwd`. rig file tools and `shell` take one confinement base (`shell` places its `cwd` against a single launcher dir), so each root gets its own
confined instance of each rig file tool and of `shell` (`launcher.in_dir(root)`) and the wrapper dispatches a call to the instance of the root
that contains the resolved path (for `shell`, the resolved `cwd`) — rig's confinement and Fontaine's scope check always agree (confirm
the rig-tools 0.45 constructor shape when the step lands). `--add-dir` roots are added at start;
`/add-dir` (Phase 3 TUI) adds one at runtime; no tool can add a root.

### Session ledger

`fontaine-core/src/ledger.rs`: the append-only per-session record from `tasks/design/sessions.md` §Session
ledger (verbatim user messages, mode changes, approvals and session grants, denials, actions taken,
roots, trust state, git remotes (via `gix`, config read without following `include`/`includeIf`; none when there is no `.git` or `gix` errors; Fontaine never spawns `git`) and the pre-session file index captured at start — gitignore-respecting via the `ignore` crate (`=0.4.33`; already transitive via rig-tools — align the pin with the version rig-tools resolves),
capped at 50,000 paths with overflow recorded as truncated), persisted as
JSONL beside rig-memory's context under `<data dir>/sessions/<session-id>/` (owner-only).
It is written by the session orchestrator and the pipeline, never by the model, and it is the only
source the guardian's request builder (Phase 4) reads. rig-memory compaction rewrites only the model
context.

## Steps

1. `fontaine-tools`: `RiskClass` registry; the `ToolGate` trait; generic `GuardedTool<T: Tool>` decorator over `rig_core::tool::Tool`, a `DynamicTool`
   wrapper path for MCP; `fontaine-core` holds the shared `Arc<SessionState>` and the `ToolGate` impl injected at registration; register exactly this tool set
   (the choke-point test asserts it: no other names, no duplicates, `delete`/`move` absent; the Phase-1
   test-only `echo` spike tool is removed here, and the `headless_smoke.rs` tool round-trip and `headless_contract.rs` `--max-turns` scripts that used it are retargeted to the in-scope `read` tool; Phase 5 extends the set and the test):

   | Tool | RiskClass | Implementation |
   |---|---|---|
   | `read` | ReadOnly | rig-tools `read`, wrapped, `confine: true` |
   | `ls` | ReadOnly | new first-party `Tool` (`NAME = "ls"`) delegating to rig `list`; args are rig-tools 0.45 `ListArgs`: `path`, `depth`, `glob` (include filter), `hidden` |
   | `glob` | ReadOnly | new first-party `Tool` over rig `search`; args `pattern`, optional `path` |
   | `grep` | ReadOnly | new first-party `Tool` over rig `search`; args are rig-tools 0.45 `SearchArgs`: `pattern`, `path`, `glob`, `hidden`, `files` mode; 200-match cap. Case-insensitivity is `(?i)` in the pattern; no context lines, count, type or head_limit |
   | `write` | WorkspaceWrite | rig-tools `write`, wrapped, `confine: true` |
   | `edit` | WorkspaceWrite | rig-tools `edit`, wrapped, `confine: true` |
   | `shell` | Arbitrary | rig-tools `shell`, wrapped; `confine: true`, `max_timeout`, output `cut` set |
   | `webfetch` | Arbitrary | new first-party; args `url` only; HTML → markdown via `htmd =0.5.6`; 10 MB fetch cap, 100,000-character output cap (truncation noted in the result), 30 s timeout; never follows a cross-host redirect (returns the redirect URL to the model, as Claude Code does). No `prompt` argument / small-model post-processing in v1 (backlog) |

   Path rules and the scope check apply to `path` (default: the primary root) and, for `glob`, also to the pattern's anchored base.

   Removal and rename go through `shell` (Arbitrary), so the shell-only critical-path check covers
   every removal path. `scope.rs` resolves every path through `rig_tools::fs::place` (base = the
   canonicalized workspace; symlinks and `..` canonicalized) instead of re-implementing containment;
   `builtins.rs` registers the tools and sets `confine: true`, `max_timeout` (clamps the
   model-supplied `timeout_secs`) and the output `cut` — no hand-rolled hardening layer. A `shell`
   call's model-supplied `cwd` is itself resolved through `place`; an out-of-scope `cwd`
   disqualifies the fast path and Tier 0.5, and relative path arguments resolve against it. The
   `ToolGate` implementation is a deny-by-default stub this phase: it allows only in-scope `ReadOnly`
   calls (scope check via `scope.rs`) and denies everything else, so the always-on floor holds from
   the first commit; the real `PermissionPipeline` lands in Phase 3 and replaces the stub. `scope.rs`
   and the per-root tool instances are multi-root from the start (design above); `--add-dir` wires in.
   On Windows the `shell` tool runs through Git Bash, discovered per `tasks/design/platforms.md` §Platforms and directories (`git_bash.rs`); `scope.rs` handles the Windows path forms listed there.
   The `shell` tool's child environment drops the telemetry credential variables, `MICROMEGAS_*`,
   `OTEL_EXPORTER_OTLP_HEADERS`, every configured `[accounts.*] api_key_env` name and
   `[guardian] scanner_hf_token_env`. `GuardedTool::call` opens a `fontaine.tool` span (beside rig's
   `execute_tool`) and emits the `tool_result` telemetry event (`risk_class`, `tool_source`).
2. `fontaine-core`: session orchestration on `rig_agent` (`AgentBuilder` + `AgentRun`), the system
   prompt (versioned asset `rust/fontaine-core/prompts/system_prompt.md`, compiled in via `include_str!`; it must cover tool steering toward `ls`/`read`/`glob`/`grep` over `shell`, how to handle denials and retries, the workspace roots, and no secrets in output; layering order: base → `FONTAINE.md` → skill index → output style → `--append-system-prompt`, where `--system-prompt` replaces the base only), FONTAINE.md loading (v1 set: `<primary root>/FONTAINE.md` plus the user-level `FONTAINE.md` in the user config dir; no parent-directory hierarchy or imports — backlog GitHub issue #7); `rig-memory` for context windows, compaction,
   and save/resume — no custom agent loop or history management to write; the agent is built with
   `tool_concurrency = 1` (tool calls run one at a time, in order); the session ledger
   (design above) alongside it, keyed by the session id (`--session-id` or a fresh UUID).
3. Docs — `docs/tools.md`: the built-in tool inventory (`ls`/`read`/`glob`/`grep`/`edit`/`write`/
   `shell`/`webfetch`), each tool's `RiskClass`, what earns the read-only fast path
   (Fontaine-enforced workspace containment — never the label), and the shell configuration (confinement,
   max timeout, output cap) — noting that rig's `list`/`search`/`delete`/`move` are not registered; extend `docs/architecture.md` with the agent-loop diagram (rig runtime + wrapped
   registry); add workspace roots (`--add-dir`, containment across roots, roots grant file access
   but not configuration) to `docs/tools.md` and `docs/configuration.md`, and the session ledger vs
   compactable model context to `docs/architecture.md`; add `fontaine.tool` / `tool_result` to
   `docs/telemetry.md`; update `docs/headless.md` with `--add-dir` and `--session-id`; link the new page from `docs/index.md`; append a CHANGELOG entry.

## Files created or modified

`rust/Cargo.toml` (new crates: `reqwest =0.13.5` (webfetch; the version rig-core 0.45's `rig-reqwest` resolves), `htmd =0.5.6`, `ignore =0.4.33`, `url` 2.x caret (IDNA host normalization), `insta` 1.x caret (dev; system-prompt snapshot)); `rust/fontaine-tools/src/{lib,gate,guarded,result_pipeline,risk,scope,builtins,webfetch,git_bash}.rs`;
`rust/fontaine-core/src/{lib,session,ledger}.rs`; `rust/fontaine-core/prompts/system_prompt.md`; `rust/fontaine-tools/tests/{scope,choke_point}.rs`; `rust/fontaine-core/tests/{ledger,config}.rs`; `rust/fontaine/tests/{headless_smoke,headless_contract}.rs` (retargeted from `echo` to `read`); `rust/fontaine/tests/headless_tools.rs`; `docs/tools.md`, `docs/architecture.md`,
`docs/configuration.md`, `docs/telemetry.md`, `docs/headless.md`, `docs/index.md`.

## Tests owned by this phase

- **tools** (`tasks/design/testing.md` Testing Strategy; `rust/fontaine-tools/tests/{scope,choke_point}.rs` — `choke_point.rs` here is the builtins-only check; the full exact-set + risk-class test over every registered tool lives in `rust/fontaine-core/tests/choke_point.rs` from Phase 5):
  workspace-scope validator over `fs::place` (path traversal, absolute paths, symlinks, out-of-scope
  `cwd`; the model-supplied `shell` `cwd` is itself resolved through `fs::place`); rig tool configuration (`confine: true`, `max_timeout` clamps `timeout_secs`, output
  `cut`); the exact registered tool set from the step-1 table (no duplicate names, `delete`/`move`
  absent; registered names are a subset of `BUILTIN_TOOL_NAMES` from `fontaine-tools` (`src/names.rs`)); risk-class registration of every built-in tool — the choke-point test that fails
  if any registered built-in is missing a risk class.
- **ResultPipeline** (`rust/fontaine-tools/tests/`): every tool result passes through it after the inner tool returns and before the caller sees it; with test-double stages attached in the wrong registration order, they still run scanner first, then `post_tool_use`; with no stages it is the identity.
- GuardedTool delegation (against a fake `ToolGate`): inner tool runs exactly once, only after the gate allows; under the stub,
  an in-scope `read` is allowed, an out-of-scope `read` and every `write`/`edit`/`shell` call is denied.
- **workspace roots** (`rust/fontaine-tools/tests/scope.rs`): a path in an `--add-dir` root is in
  scope and reaches that root's confined instance; a `shell` call whose `cwd` is in an added root
  reaches that root's `Shell` instance and is not refused by rig; a symlink from one root into another stays in
  scope, out of every root falls out; no registered tool can add a root.
- **added-root settings** (`rust/fontaine-core/tests/config.rs`): settings in an added root are not loaded.
- **windows** (`#[cfg(windows)]`; compile-checked locally with `cargo check --all-targets --target x86_64-pc-windows-gnu`, executed in the windows CI job): scope validator over drive letters, UNC
  paths, case-insensitive comparison, junction and symlink escape, and MSYS-style `/c/...` paths
  (untranslatable ⇒ out of scope); the `shell` tool launches through Git Bash; Git Bash discovery
  (discovery cases per `tasks/design/platforms.md`, including the platform-independent path-derivation unit test).
- **webfetch** (wiremock): HTML converted to markdown; a body over 10 MB is cut at the fetch cap; output
  over 100,000 characters is truncated with the truncation noted; a response slower than 30 s times out;
  a cross-host redirect is returned, not followed; only `url` is accepted.
- **system prompt** (`rust/fontaine-core/tests/`): `insta` snapshot of the assembled prompt (base → `FONTAINE.md` → append; `--system-prompt` replaces only the base); the base contains the required steering content.
- **session ledger** (`rust/fontaine-core/tests/ledger.rs`): after a forced compaction the ledger
  still holds every user message verbatim and no summary text; the ledger is append-only (no API
  removes a record); the file index respects `.gitignore` and truncates at the cap with the
  truncation recorded.
- **telemetry**: canary test for tool args and output (absent without `log_tool_details` /
  `log_tool_content`); the `shell` child environment has no telemetry credential, `MICROMEGAS_*`
  variable, configured `api_key_env` variable or `scanner_hf_token_env` variable.
- Integration (`rust/fontaine/tests/headless_tools.rs`): `fontaine -p` against the scripted
  `openai-compatible` endpoint (built with the Phase-1 `common/mod.rs` helper) completes a read-tool round-trip (passing `--permission-mode manual` explicitly).

## Definition of done

- Offline gates (no keys, no human): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green from `rust/`, including `rust/fontaine-tools/tests/{scope,choke_point}.rs`,
  `rust/fontaine-core/tests/ledger.rs`, `rust/fontaine/tests/headless_tools.rs`.
- A headless `-p` run against the scripted endpoint completes a read-tool round-trip through the
  wrapped registry (in-scope `read` allowed by the deny-by-default stub; `write`/`shell` denied —
  nothing executes past the stub until Phase 3).
- Choke-point test green: every registered built-in tool has a `RiskClass`.
- Needs a human (the agent cannot push): push the branch and confirm the windows CI job runs the `#[cfg(windows)]` tests green.
- `rust/fontaine-core/tests/docs.rs` green with `docs/tools.md` covering the shipped inventory; scope checks are multi-root; the ledger
  survives compaction.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
