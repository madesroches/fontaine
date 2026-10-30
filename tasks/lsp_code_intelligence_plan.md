# LSP Code-Intelligence Tool Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/27

## Overview

Give the model what Claude Code's `LSP` tool gives it: symbol navigation through a real language
server (definition, references, hover, document and workspace symbols, implementations, call
hierarchy) and the server's errors and warnings after each `edit`/`write`, so it sees a type error
or missing import it just introduced without running a build. Fontaine spawns language servers
that only the user defines (they execute code), exposes the queries as one `lsp` tool in the
`ReadOnly` class that earns the fast path only through workspace containment, and delivers
diagnostics as untrusted tool-result text that the tool-result scanner sees, never as a
user-role message.

## Current State

- No LSP code anywhere. Installed plugins that carry `lspServers` (manifest key or directory) are
  warned about and ignored (`rust/fontaine-core/src/plugins.rs:491-507`); the master plan records
  it (`tasks/fontaine_harness_plan.md`, Decisions, 2026-10-28) and `docs/plugins.md:174` says so.
- **Tools.** Every built-in has a row in `TOOLS` (`fontaine-tools/src/descriptor.rs:75-258`:
  risk class, `path_free`, destructive kind, rule specifier, Claude Code alias), pinned by
  `rows_are_pinned` (:348). Conditional tools register through `ExtensionTools`
  (`fontaine-core/src/tools.rs:67-123`), wrapped in `GuardedTool` (`fontaine-tools/src/guarded.rs`)
  so every call passes the gate and then the result pipeline. Choke-point tests:
  `fontaine-tools/tests/choke_point.rs`, `fontaine-core/tests/choke_point.rs:169-270`; docs test
  `fontaine-core/tests/docs.rs:202` (a `docs/tools.md` row per tool).
- **Fast path.** `permissions.rs:784-790`: `ReadOnly` plus a located (in-root) path is allowed;
  `scope.rs:46-55` `fields_of` names which argument holds the path. A `ReadOnly` call whose path
  is out of every root and is not one of the four external-read tools is denied before approval
  with the `/add-dir` hint (`permissions.rs:863-880`). Path arms by tool name also live in
  `destructive.rs:275` (`touched_paths`, for the ledger) and `permission_rules.rs:562`
  (`path_args`, for rule specifiers).
- **Edits.** `edit`/`write` are rig-tools 0.45.0 wrapped in `builtins.rs:75-147`; their result is
  one line ("Edited {path}: lines 2-4 …", "Created {path}.").
- **Result pipeline.** `fontaine-tools/src/result_pipeline.rs`: `StageKind { Notifications,
  Instructions, Scanner, PostToolUse }` run in that order; `append_note` (:104) appends text to a
  result. Stages are wired in `fontaine-core/src/agent/setup.rs:406-436`. Anything a stage before
  `Scanner` adds is scanned.
- **Long-lived children.** `Jobs` (`fontaine-tools/src/jobs.rs:744`) and `SearxngChild`
  (`websearch.rs:330`) are held once in `Setup` (`fontaine-core/src/agent.rs:189-212`) so a
  registry rebuild keeps them, and are ended on `/clear` and at exit (`agent.rs:826`,
  `fontaine-tui/src/lib.rs:331`, `fontaine/src/headless.rs:850,883,902`). MCP stdio servers spawn
  with `env_clear()` + `BASE_ENV` + `env_vars` + literal `env` + the `[env]` table, cwd at the
  primary root, stderr to `tracing::debug!` (`fontaine-mcp/src/servers.rs:211-243`,
  `fontaine-core/src/agent/mcp.rs:138-198`).
- **Config scope.** `config_scope.rs:141-198` `RULES`; `Rule::user_or_cli` is how `websearch`
  keeps the command that starts a server out of project scope; a key with no row is refused from
  project scope. Settings panel rows: `config_catalog.rs:221` (`every_settings_leaf_has_a_row`).
- **Claude Code** (observed with `claude -p --plugin-dir` and a `.lsp.json` for rust-analyzer,
  2026-10-30): the tool is `LSP`, input `{operation, filePath, line, character, query?}`, `line`
  and `character` 1-based, operations `goToDefinition, findReferences, hover, documentSymbol,
  workspaceSymbol, goToImplementation, prepareCallHierarchy, incomingCalls, outgoingCalls`; the
  tool exists only while a server is configured. Server config fields: `command`, `args`,
  `extensionToLanguage` (required), `env`, `initializationOptions`, `settings`,
  `workspaceFolder`, `startupTimeout`, `shutdownTimeout`, `requestTimeout` (default 60000),
  `restartOnCrash` (default true), `maxRestarts`, `diagnostics` (default true). The server starts
  on first need. In the probe the rustup `rust-analyzer` proxy had no component installed:
  the server exited 1, `goToDefinition` returned
  `LSP server plugin:ra-probe:rust-analyzer crashed with exit code 1`, and the following edit
  returned a plain success with **no word that diagnostics were unavailable** — the same gap omp
  showed (`research/research-oh-my-pi-harness.md` 4.2, E4).

## Design

### Server definitions — user scope only

```toml
[lsp.servers.rust-analyzer]
command = "rust-analyzer"
args = []
extension_to_language = { ".rs" = "rust" }
env_vars = []                       # parent variables passed through, as for MCP
env = {}                            # literal overrides
initialization_options = {}         # sent in `initialize`
settings = {}                       # sent by workspace/didChangeConfiguration
startup_timeout_ms = 30000
request_timeout_ms = 60000
restart_on_crash = true
max_restarts = 3
diagnostics = true
```

- `config.rs`: `LspSection { servers: Option<BTreeMap<String, LspServer>> }`, strict
  (`deny_unknown_fields`); names use the MCP server alphabet. Validation at load:
  `extension_to_language` non-empty, keys start with `.`, `command` non-empty; an invalid entry
  is a config error naming it.
- `config_scope.rs`: `Rule::user_or_cli("lsp", …)` with the reason "starts a program". No project
  or local scope can add, change or enable a server, so no trust prompt is needed for it.
- **Plugin servers.** An installed plugin's `.lsp.json` at the plugin root and its `lspServers`
  manifest key (inline map, path or array, Claude Code's precedence: `.lsp.json` first, later
  names replace earlier) load as server definitions, mapped from Claude Code's camelCase fields;
  `transport = "socket"` is accepted and runs over stdio, as Claude Code does; an unknown key
  skips that plugin's whole LSP file with a warning (Claude Code skips the file). Names are
  composed `plugin:<plugin>:<server>` (Claude Code's form). Plugins are user-scope and pinned, so
  this keeps the user-only rule. User config wins over a plugin on the same name.
- **One server per extension**: the first registered (user config in name order, then plugins in
  install order) handles an extension; a later claimant gets a startup warning
  `LSP server "<name>" is not used for <ext> files` (Claude Code's text).

### Module layout — `fontaine-tools/src/lsp/`

- `client.rs` — one connection: spawn, `initialize`/`initialized`, document sync, requests with
  a timeout, `publishDiagnostics` intake, `shutdown`/`exit`. Built on **`async-lsp` 0.2.4**
  (MIT OR Apache-2.0; JSON-RPC framing, request ids, notification routing) with
  **`lsp-types` 0.95.1** (its version), both pinned exactly, `default-features = false`, driven
  over the child's tokio pipes (not async-lsp's unix-only `stdio` feature) so Windows builds.
- `manager.rs` — `LspServers`: the config table, the extension map, and per server a state
  machine `Configured → Starting → Running | Failed { reason } | Stopped`. Started lazily on the
  first `lsp` call or edit that needs it; concurrent first uses share one start. A crash moves to
  `Failed` and restarts on next need while `restart_on_crash` and restarts < `max_restarts`.
- `diagnostics.rs` — per-file diagnostic store, the "new since last reported" diff, formatting.
- `tool.rs` — the `lsp` tool.

`LspServers` is created once in `Setup` next to `jobs` (an `Arc`), so a registry rebuild keeps
running servers; `AgentRuntime::end_jobs`'s callers (`/clear`, TUI exit, headless exits) also
call `LspServers::shutdown_all`: `shutdown` request, `exit`, wait up to the server's
`shutdown_timeout_ms` (default 2 s), then kill. `Drop` kills anything left.

**Spawn**: exactly the MCP recipe (`env_clear` + `child_env::BASE_ENV` + `env_vars` + `env` +
`[env]` table merged with `merged_server_env`), cwd at the primary root, stderr drained into
`tracing::debug!`, own process group on unix like shell jobs. `rootUri` and the first
`workspaceFolders` entry are the primary root; added roots are further `workspaceFolders`
(and `didChangeWorkspaceFolders` on `/add-dir` when the server advertises it). No
`workspaceFolder` override key — the root is the workspace.

**Document sync.** Before a query or after an edit, the manager makes the server's view current:
`didOpen` (full text from disk, the configured language id) the first time, `didChange` with full
text and a bumped version when the content hash changed, then `didSave`. Files are never closed
during a session (servers bound memory themselves); a deleted file gets `didClose`.

**Positions.** The tool takes 1-based line and character (Unicode scalar values, "as shown in
editors"); the client asks for `positionEncoding` `utf-32` and falls back to `utf-16`, converting
with the line's text. Output positions are converted back to 1-based.

### The `lsp` tool

- Input: Claude Code's schema with Fontaine's path name — `{operation, path, line, character,
  query?}`; same nine operations and names. `line`/`character` required except for
  `documentSymbol` and `workspaceSymbol` (Claude Code requires them everywhere; Fontaine ignores
  them there rather than make the model invent a position). `workspaceSymbol` requires a non-empty
  `query`. `path` picks the server by extension.
- Descriptor row: `row("lsp", ReadOnly, false, Never, Specifier::Path, &["LSP"])`. Path arms in
  `scope.rs` `fields_of` (`path` required), `destructive.rs` `touched_paths`, and
  `permission_rules.rs` `path_args` (not a tree). So an in-root call takes the fast path, an
  out-of-root one is denied before approval with the existing hint (it is not an external read),
  and `lsp(src/**)` / deny rules match like `read`.
- Registered as an extension tool only when at least one server is configured (Claude Code's
  "inactive until a server exists"). Explore/Plan read-only toolsets (`fontaine-agents/src/
  registry.rs:168,187`) gain `lsp`; a child shares the parent's `LspServers`.
- Output: plain text, locations as `path:line:col` (relative to the primary root when inside it,
  absolute otherwise) with the symbol name or kind; at most 100 locations, then `… and N more`;
  hover text capped at 4,000 characters. Results carry locations and the server's own text only —
  never file contents read by Fontaine — so a location outside the roots is a pointer the model
  must follow with the gated `read`.
- Errors are tool errors naming the state: no server for the extension
  (`no language server is configured for .py files`), `rust-analyzer failed to start: exit code 1`
  (with the last stderr line), a request timeout, or an empty result
  (`rust-analyzer returned no result (it may still be indexing)`).

### Diagnostics after edits

A new result stage, `StageKind::Diagnostics`, ordered **after `Notifications` and before
`Scanner`**, so diagnostic text is scanned. It is attached only when a server is configured.

1. On a successful `edit` or `write` whose `path` maps to a server with `diagnostics = true`, it
   syncs the file (`didChange`/`didSave`) and waits up to **500 ms** for a `publishDiagnostics`
   for that file after the sync.
2. It appends one note (via `append_note`), always, for a file a server handles:
   - `Diagnostics (rust-analyzer) for src/main.rs: 2 new — src/main.rs:1:22 error[E0308]: mismatched types; …`
     (only diagnostics not reported for that file before, severity error and warning; at most 20,
     then `… and N more`; hints/info dropped),
   - `Diagnostics (rust-analyzer) for src/main.rs: none` when the server published an empty set
     (or none new),
   - `Diagnostics (rust-analyzer) for src/main.rs: none yet; later ones will be reported with a
     later tool result` on timeout,
   - `Diagnostics unavailable: rust-analyzer failed to start (exit code 1)` when it is `Failed`.
3. **Late diagnostics.** A `publishDiagnostics` that arrives after the wait replaces that file's
   pending set (newest wins, which is the stale drop: an older version's set never survives a
   newer one). Every tool result — any tool, this turn or the next — passes the stage, which
   appends pending sets under `Late diagnostics (rust-analyzer):`, each diffed against what was
   already reported for its file, and clears them. They never start a turn on their own and are
   never a user-role message; the turn that next calls a tool sees them.

Text is wrapped as a harness note that names its source ("from the language server, not the
user"), matching how job notes say they are not the user.

### Toolchain

`rust/rust-toolchain.toml` gains the `rust-analyzer` component, so working on Fontaine with
`[lsp.servers.rust-analyzer]` uses a server matching the pinned 1.99.1 compiler, and the
optional live test below has a real server. CI keeps installing what the file lists.

## Implementation Steps

1. **Dependencies.** Add `async-lsp = { version = "=0.2.4", default-features = false, features =
   ["omni-trait", "tracing"] }` and `lsp-types = "=0.95.1"` to `fontaine-tools`; `cargo deny
   check` must pass. Add `rust-analyzer` to `rust/rust-toolchain.toml` components.
2. **Config.** `LspSection`/`LspServer` in `fontaine-core/src/config.rs` with validation;
   `config_scope.rs` row; `config_catalog.rs` row for `lsp.servers` (Map, like `mcp.servers`).
3. **Plugins.** In `plugins.rs`, stop warning on `lspServers` / `.lsp.json`; parse them into the
   same `LspServer` (camelCase mapping, strict, whole-file skip on an unknown key), composed names,
   precedence as designed. Expose plugin servers alongside plugin MCP servers to setup.
4. **Client and manager.** `fontaine-tools/src/lsp/{client,manager}.rs`: spawn via the MCP env
   recipe (factor the env build out of `fontaine-mcp/src/servers.rs` into `fontaine-tools/src/
   child_env.rs` if it isn't already shareable), lazy start, state machine, restart policy,
   document sync, position conversion, `shutdown_all`.
5. **Diagnostics store and stage.** `lsp/diagnostics.rs`; `StageKind::Diagnostics` in
   `result_pipeline.rs` (update the module doc's order sentence); wire it in
   `agent/setup.rs:406-436`.
6. **Tool.** `lsp/tool.rs`; descriptor row (and `rows_are_pinned`); path arms in `scope.rs`,
   `destructive.rs`, `permission_rules.rs`; `summarize` arm in `permissions.rs:1918` ("lsp
   findReferences src/main.rs:2:28"); register through `ExtensionTools` when servers exist;
   Explore/Plan toolsets.
7. **Lifecycle wiring.** `Setup` holds `Arc<LspServers>`; `shutdown_all` at `/clear`, TUI exit,
   headless exits; startup warnings (extension conflicts, invalid plugin LSP files) through the
   existing startup-notice path.
8. **System prompt.** One `# Tools` bullet in `fontaine-core/prompts/system_prompt.md`: prefer
   `lsp` over `grep` to find a symbol's definition or references when it is available, and read
   the diagnostics note after an edit. Keep `prefix_stability` green (the bullet is static text;
   tool availability already varies the tool list).
9. **Docs and records** (see Documentation).

## Files to Modify

- `rust/fontaine-tools/Cargo.toml`, `rust/Cargo.lock`, `rust/rust-toolchain.toml`
- `rust/fontaine-tools/src/lsp/{mod,client,manager,diagnostics,tool}.rs` (new), `src/lib.rs`
- `rust/fontaine-tools/src/descriptor.rs`, `scope.rs`, `result_pipeline.rs`, `child_env.rs`
- `rust/fontaine-tools/tests/choke_point.rs`
- `rust/fontaine-core/src/config.rs`, `config_scope.rs`, `config_catalog.rs`, `plugins.rs`,
  `tools.rs`, `permissions.rs`, `permission_rules.rs`, `destructive.rs`, `agent.rs`,
  `agent/setup.rs`, `prompts/system_prompt.md`
- `rust/fontaine-core/tests/choke_point.rs`, `tests/docs.rs` (if it enumerates extension tools)
- `rust/fontaine-agents/src/registry.rs`
- `rust/fontaine-mcp/src/servers.rs` (only if the env recipe is factored out)
- `rust/fontaine-tui/src/lib.rs`, `rust/fontaine/src/headless.rs`
- `docs/tools.md`, `docs/configuration.md`, `docs/plugins.md`, `docs/permissions.md` (if it lists
  read-only tools), `research/research-claude-code-parity.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **async-lsp vs. hand-written framing.** Content-Length framing plus a pending-request map is
  ~200 lines, but the project's reuse-first rule favours a maintained crate; async-lsp is
  client-capable, MIT/Apache, and only drags in tower-layer/tower-service. Its pinned `lsp-types`
  0.95 (url-based URIs) is older than 0.97 but is what the crate is tested with.
- **Late diagnostics ride the next tool result instead of starting a turn** (omp sends a separate
  message; job notes start a turn). A turn started only to say "warning: unused variable" would
  spend model calls the user did not ask for; riding a result keeps them in the scanned
  tool-result channel and costs nothing when the model is done.
- **Always one note per handled edit**, even "none yet", instead of silence. It costs one line;
  silence was the failure both omp and Claude Code showed in practice.
- **Rejected: project-defined servers behind workspace trust.** It would match how MCP servers
  load, but a server runs on every edit with no per-call prompt, and Claude Code itself only takes
  servers from plugins (user-installed). User scope only is simpler and strictly safer.
- **Rejected: rename / code actions.** Claude Code's tool is read-only; writes through a server
  would need the write class, checkpointing and diff previews.

## Decisions

- 2026-10-30: Plugin LSP servers are now supported (reverses the 2026-10-28 "unsupported" line):
  plugins are user-installed and pinned, so they satisfy the user-scope-only rule.
- 2026-10-30: `lsp` uses `path` (Fontaine's name for a file argument), not Claude Code's
  `filePath`, and does not require a position for `documentSymbol`/`workspaceSymbol`.
- 2026-10-30: Late diagnostics never start a turn; they ride the next tool result (deviation from
  omp; Claude Code's delivery format was not observable — the probe's server failed to start).
- 2026-10-30: `rust-analyzer` joins the toolchain components (maintainer request in the design
  session); CI downloads it although no CI step uses it.

## Documentation

- `docs/tools.md`: inventory row for `lsp` (`read-only`), a "Language servers" section
  (operations, output, the diagnostics note and its states, late diagnostics), and `lsp` in "What
  earns the read-only fast path" (in-root only, no external fallback, results are pointers).
- `docs/configuration.md`: `[lsp.servers.<name>]` reference and the scope-table row (user or
  `--settings` only).
- `docs/plugins.md`: replace the "LSP server components are unsupported" bullet with how plugin
  servers load, are named, and lose to user config.
- `research/research-claude-code-parity.md`: LSP row → done, with the deviations above.
- `tasks/fontaine_harness_plan.md`: the Decisions lines above.

## Testing Strategy

All unit/integration tests use a **fake language server**: a small bin target
`fake-lsp` in `fontaine-tools` (`[[bin]]`, test-support only, reached with
`env!("CARGO_BIN_EXE_fake-lsp")` — the pattern `fontaine-evals/tests/fixture.rs` uses) that
speaks LSP over stdio and is scripted by env vars: canned definition/reference/hover replies,
diagnostics published after a delay, empty sets, exit with a code at start, crash after N
requests, never answering. It is a real child process, so spawn, env, crash and shutdown paths
are exercised for real.

Unit (`fontaine-tools`):
- Position conversion: ASCII, multi-byte UTF-8, astral characters under `utf-16`; 1-based ↔
  0-based both ways; out-of-range line/character → tool error.
- Output formatting: path relativisation inside/outside the root, 100-location and 4,000-char
  caps, symbol kinds.
- Diagnostics store: "new since last reported" diff, newest-wins pending set (stale drop),
  severity filter, 20-item cap, the four note states.
- Config: validation errors (empty map, key without `.`, unknown key), plugin camelCase mapping,
  `.lsp.json` vs manifest precedence, whole-file skip on an unknown key, extension-conflict
  resolution and warning text.

Integration (`fontaine-tools/tests/lsp.rs`, fake server):
- Lazy start: no process before first use; two concurrent first calls start one process.
- Each operation round-trips through the fake; `workspaceSymbol` without `query` is a tool error.
- Failed start (`exit 1`) → `failed to start: exit code 1` for the tool and
  `Diagnostics unavailable` on the next edit — **pins the silent-failure bug seen in Claude Code
  and omp**.
- Crash and restart up to `max_restarts`, then stays `Failed`; `restart_on_crash = false` never
  restarts.
- Request timeout honoured; `shutdown_all` sends shutdown/exit and kills a server that ignores it
  after the timeout; the child env is `BASE_ENV` + `env_vars` + `env` only (fake echoes its env).
- Diagnostics stage: edit → inline note within 500 ms; delayed publish → "none yet" then
  `Late diagnostics` on the next unrelated tool result; two edits before the late set → only the
  newest set is reported; the note passes through the scanner stage (stage order test).

Core (`fontaine-core/tests`):
- Choke-point: `lsp` is registered with `ReadOnly` exactly when a server is configured, and
  absent otherwise; its descriptor row is pinned.
- Permissions: in-root `lsp` → `ReadOnlyFastPath`; out-of-root → denied before approval with the
  hint; `lsp(secrets/**)` deny rule blocks it; project-scope `[lsp]` is a scope error;
  `--settings` is accepted.
- Headless `fontaine -p` with a cassette/deterministic model calling `lsp` against the fake
  server, so the wiring from config to tool to pipeline is covered end to end.

Live (not CI): an `#[ignore]` test in `fontaine-tools/tests/lsp_live.rs` that runs the real
rust-analyzer from the toolchain against a two-file crate in a temp dir (definition, references,
and a type error reported after an edit). It catches what a fake cannot — a real server's
indexing latency, `utf-16` handling, and that its flycheck diagnostics arrive on the late path.

## Manual Verification

1. `rustup component list --installed` in `rust/` lists `rust-analyzer` after a `cargo --version`
   there (toolchain file honoured).
2. With `[lsp.servers.rust-analyzer]` in the user config, run `cargo run -p fontaine` in a scratch
   crate, ask for a deliberate type error with `edit`; the edit's result card shows the
   `Diagnostics (rust-analyzer)` note and a later result shows the flycheck error under
   `Late diagnostics`. Why manual: how the note reads in the TUI tool card is a judgment call.

## Open Questions

- None blocking. Whether the TUI should collapse the diagnostics note into a one-line summary
  (Claude Code's `Found N new diagnostic issues`, ctrl+o to expand) can follow as a TUI change once
  the text exists.
