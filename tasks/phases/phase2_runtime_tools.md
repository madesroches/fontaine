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
- **Composite read tools + Tier 0.5 shell pre-classifier** (user directive 2026-10-21; mirrors
  Claude Code's split of dedicated read tools that skip prompts from a built-in read-only command
  set for `Bash`, per the design tie-breaker). Frequent read operations are **also** exposed as
  dedicated first-party tools — `ls`/list, `read`, `glob`/`grep` search (wrapping the `rig-tools`
  equivalents) — with workspace containment enforced *inside the tool* at the execution boundary
  (pipeline step 4), and the system prompt steers the model toward these rather than `shell` for
  reads. The **Tier 0.5 pre-classifier** (pipeline step 5) is the fallback fast path for read-ish
  shell invocations: it is purely *structural* — lex the command line, check `argv[0]`+subcommand
  against the curated set, accept only path arguments (resolved against the call's `cwd`; an out-of-scope `cwd` disqualifies the fast path) that pass the scope checker plus flags on a
  per-command read-only flag allowlist (values of file-opening flags like `--files-from`/`-f`/`-m`
  are validated as path arguments; any other flag, glob, or unparsed token falls through) — which
  honors the "no regex matching on arguments anywhere" decision. Lexing uses **`shlex`** (v2.0.1,
  MIT OR Apache-2.0, actively maintained — 2.0.0 shipped 2026-05) for POSIX word splitting; a full
  bash AST via **`tree-sitter-bash`** (v0.25.1, MIT, maintained under the tree-sitter org; pulls
  in the `tree-sitter` runtime and a C build step) is the optional upgrade when operator-level
  parsing (pipelines, redirections, substitutions) must be *recognized* rather than merely
  rejected. Anything the classifier cannot fully parse **falls through to the guardian** — the
  same fail-through soundness Claude Code documents ("when it can't fully parse a command, it
  asks for approval instead of treating the command as read-only"). Curated set membership alone
  is unsound: several allowlisted commands carry write/exec-capable or file-opening flags, so the
  per-command read-only flag allowlist mirrors Claude Code's documented per-command flag handling
  (its docs prompt on `file -m`/`--magic-file`/`-f`/`--files-from`, and on unquoted globs for
  `find`/`sort`/`sed`/`git` because of write-capable flags like `sort -o` and `git log --output`).
  Out-of-workspace reads never pre-classify as safe: read-only ≠ safe, per the 2026-10-21
  hybrid-gate refinement (Fontaine-enforced containment at the execution boundary earns the fast
  path, never the read-only label), so `cat ~/.ssh/id_rsa` is adjudicated like a write. The
  curated command set ships **opinionated and conservative** (`ls`, `pwd`, `wc`, `file`, read-only
  `git status|log|diff|show`, `rg`/`grep`, `cat`/`head`/`tail`); users may extend it via
  `[shell_preclassifier]` config keys (`allow_extra`, a single mode-agnostic list applying in
  every mode — the pipeline runs the pre-classifier before the mode branches, so no per-mode keys;
  NOT the guardian's `[auto_mode].allow` prose list), and Fontaine **never auto-extends** it from
  observed usage — unlike Claude Code, whose set is fixed and whose "don't ask again" grants
  accumulate prefixes: Fontaine's grants go to explicit config rules only (recorded in Decisions
  as a deviation).
- **Risk registry**: a `RiskClass { ReadOnly | WorkspaceWrite | Arbitrary }` assigned per tool at
  registration (rig's registry supports ordered registrations). This is the deterministic,
  non-LLM half of the hybrid gate: `ReadOnly` + args validated within workspace scope (a
  `fontaine-tools` scope/symlink validator, deterministic code — not regex-on-args) ⇒ pipeline
  auto-allows without a guardian call. Two clarifications: **read ≠ safe** — a read outside the
  workspace (`~/.ssh/*`, cloud credential files, `.env`, browser cookie stores, metadata
  endpoints) is a classic recon/exfiltration step, so containment inside the workspace is what
  earns the fast path, never the read-only label; and **declaration ≠ authorization** — risk
  metadata from third-party (MCP) servers is untrusted input (a malicious server will happily
  label a write/network-capable tool with `readOnlyHint: true`), so the fast path applies only
  where *Fontaine itself* enforces containment at the execution boundary (built-in tools and
  wrapped paths validated by the scope checker). MCP tools default to full adjudication; their
  declared `readOnlyHint` is an optimization hint surfaced to the guardian, never an
  authorization token.
- **GuardedTool**: a decorator implementing `rig_core::tool::Tool` that wraps any inner tool and,
  in its `call`, runs the `PermissionPipeline` (mode check → read-only fast path → Tier-0.5 shell
  pre-classifier → guardian in `auto`) before delegating to the inner tool. This is the **single
  choke point**: every tool — rig built-in, Fontaine-added, or MCP-imported — is registered
  wrapped, so nothing can execute without passing the gate.
- **Session-state carrier**: the pipeline's inputs (mode, workspace roots, merged permission rules
  and session grants, the session ledger handle, audit-log sink, and the human-approval callback;
  the user-hook runner joins in Phase 5) travel in a shared
  `Arc<SessionState>` injected into every wrapper at registration; rig's per-call `&mut ToolContext`
  carries only per-call identity (agent id) for per-agent audit keying. Because rig-core 0.45.0's
  `Tool` has a `const NAME` and typed `Args`, the wrapper is a generic `GuardedTool<T: Tool>`
  (`NAME = T::NAME`) for built-ins; MCP tools arrive as `DynamicTool` and are wrapped by a parallel
  `DynamicTool` wrapper path. Both call the same `PermissionPipeline` entry point.

`WorkspaceWrite` (write/edit within workdir) is auto-allowed only in `acceptEdits`/`auto`(guardian)
per mode. `Arbitrary` (shell, webfetch, MCP tools) always hits the guardian in `auto` (shell calls
may be pre-classified safe at Tier 0.5 first — pipeline step 5 precedes the mode branches, matching Claude
Code's read-only Bash set prompting in no mode), prompts in `manual`. MCP tools arrive via
`fontaine-mcp` (rig-rmcp) and are registered `RiskClass::Arbitrary` (never trusted as safe
regardless of server `readOnlyHint` annotations — annotations are claims, surfaced to the
guardian as hints, not as gates).

### Workspace roots

`scope.rs` validates against an ordered set of canonicalized roots (master §Config): a path is in
scope when `rig_tools::fs::place(root, true, path)` accepts it for some root, after resolving it
against the call's `cwd`. rig file tools take one confinement base, so each root gets its own
confined instance of each rig file tool and the wrapper dispatches a call to the instance of the root
that contains the resolved path — rig's confinement and Fontaine's scope check always agree (confirm
the rig-tools 0.45 constructor shape when the step lands). `--add-dir` roots are added at start;
`/add-dir` (Phase 3 TUI) adds one at runtime; no tool can add a root.

### Session ledger

`fontaine-core/src/ledger.rs`: the append-only per-session record from the master plan's §Session
ledger (verbatim user messages, mode changes, approvals and session grants, denials, actions taken,
roots, trust state, git remotes and the pre-session file index captured at start), persisted as
JSONL beside rig-memory's context under `<user data dir>/fontaine/sessions/<session-id>/` (0600).
It is written by the session orchestrator and the pipeline, never by the model, and it is the only
source the guardian's request builder (Phase 4) reads. rig-memory compaction rewrites only the model
context.

## Steps

1. `fontaine-tools`: `RiskClass` registry; generic `GuardedTool<T: Tool>` decorator over `rig_core::tool::Tool`, a `DynamicTool`
   wrapper path for MCP, and the shared `Arc<SessionState>` carrier; register exactly this tool set
   (the choke-point test asserts it: no other names, no duplicates, `delete`/`move` absent):

   | Tool | RiskClass | Implementation |
   |---|---|---|
   | `read` | ReadOnly | rig-tools `read`, wrapped, `confine: true` |
   | `ls` | ReadOnly | new first-party `Tool` (`NAME = "ls"`) delegating to rig `list` |
   | `glob` | ReadOnly | new first-party `Tool` over rig `search` |
   | `grep` | ReadOnly | new first-party `Tool` over rig `search` |
   | `write` | WorkspaceWrite | rig-tools `write`, wrapped, `confine: true` |
   | `edit` | WorkspaceWrite | rig-tools `edit`, wrapped, `confine: true` |
   | `shell` | Arbitrary | rig-tools `shell`, wrapped; `confine: true`, `max_timeout`, output `cut` set |
   | `webfetch` | Arbitrary | new first-party |

   Removal and rename go through `shell` (Arbitrary), so the shell-only critical-path check covers
   every removal path. `scope.rs` resolves every path through `rig_tools::fs::place` (base = the
   canonicalized workspace; symlinks and `..` canonicalized) instead of re-implementing containment;
   `builtins.rs` registers the tools and sets `confine: true`, `max_timeout` (clamps the
   model-supplied `timeout_secs`) and the output `cut` — no hand-rolled hardening layer. A `shell`
   call's model-supplied `cwd` is itself resolved through `place`; an out-of-scope `cwd`
   disqualifies the fast path and Tier 0.5, and relative path arguments resolve against it. The
   `GuardedTool` gate body is a deny-by-default stub this phase: it allows only in-scope `ReadOnly`
   calls (scope check via `scope.rs`) and denies everything else, so the always-on floor holds from
   the first commit; the real `PermissionPipeline` lands in Phase 3 and fills the stub. `scope.rs`
   and the per-root tool instances are multi-root from the start (design above); `--add-dir` wires in.
   The `shell` tool's child environment drops the telemetry credential variables, `MICROMEGAS_*` and
   `OTEL_EXPORTER_OTLP_HEADERS`. `GuardedTool::call` opens a `fontaine.tool` span (beside rig's
   `execute_tool`) and emits the `tool_result` telemetry event (`risk_class`, `tool_source`).
2. `fontaine-core`: session orchestration on `rig_agent` (`AgentBuilder` + `AgentRun`), system
   prompt, FONTAINE.md project-instruction loading; `rig-memory` for context windows, compaction,
   and save/resume — no custom agent loop or history management to write; the session ledger
   (design above) alongside it, keyed by the session id (`--session-id` or a fresh UUID).
3. Docs — `docs/tools.md`: the built-in tool inventory (`ls`/`read`/`glob`/`grep`/`edit`/`write`/
   `shell`/`webfetch`), each tool's `RiskClass`, what earns the read-only fast path
   (Fontaine-enforced workspace containment — never the label), and the shell configuration (confinement,
   max timeout, output cap) — noting that rig's `list`/`search`/`delete`/`move` are not registered; extend `docs/architecture.md` with the agent-loop diagram (rig runtime + wrapped
   registry); add workspace roots (`--add-dir`, containment across roots, roots grant file access
   but not configuration) to `docs/tools.md` and `docs/configuration.md`, and the session ledger vs
   compactable model context to `docs/architecture.md`; add `fontaine.tool` / `tool_result` to
   `docs/telemetry.md`; link the new page from `docs/index.md`; append a CHANGELOG entry.

## Files created or modified

`rust/fontaine-tools/src/{lib,guarded,risk,scope,builtins,webfetch,preclassify}.rs`;
`rust/fontaine-core/src/{lib,session,ledger,hooks}.rs`; `docs/tools.md`, `docs/architecture.md`,
`docs/configuration.md`, `docs/telemetry.md`, `docs/index.md`.

## Tests owned by this phase

- **tools** (master Testing Strategy; `rust/fontaine-tools/tests/{scope,choke_point}.rs`):
  workspace-scope validator over `fs::place` (path traversal, absolute paths, symlinks, out-of-scope
  `cwd`); rig tool configuration (`confine: true`, `max_timeout` clamps `timeout_secs`, output
  `cut`); the exact registered tool set from the step-1 table (no duplicate names, `delete`/`move`
  absent); risk-class registration of every built-in and MCP tool — the choke-point test that fails
  if any registered tool is missing a risk class.
- GuardedTool delegation: inner tool runs exactly once, only after the gate allows; under the stub,
  an in-scope `read` is allowed, an out-of-scope `read` and every `write`/`edit`/`shell` call is denied.
- **workspace roots** (`rust/fontaine-tools/tests/scope.rs`): a path in an `--add-dir` root is in
  scope and reaches that root's confined instance; a symlink from one root into another stays in
  scope, out of every root falls out; settings in an added root are not loaded.
- **session ledger** (`rust/fontaine-core/tests/ledger.rs`): after a forced compaction the ledger
  still holds every user message verbatim and no summary text; the ledger is append-only (no API
  removes a record).
- **telemetry**: canary test for tool args and output (absent without `log_tool_details` /
  `log_tool_content`); the `shell` child environment has no telemetry credential or `MICROMEGAS_*`
  variable.
- Integration (`rust/fontaine/tests/headless_tools.rs`): `fontaine -p` against the scripted
  `openai-compatible` endpoint completes a read-tool round-trip.

## Definition of done

- A headless `-p` run against the scripted endpoint completes a read-tool round-trip through the
  wrapped registry (in-scope `read` allowed by the deny-by-default stub; `write`/`shell` denied —
  nothing executes past the stub until Phase 3).
- Choke-point test green: every registered tool has a `RiskClass`.
- `docs/tools.md` complete for the shipped inventory; scope checks are multi-root; the ledger
  survives compaction.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
