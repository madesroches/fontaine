# Phase 2 — Agent runtime & tools

**Status:** not started · **Depends on:** [Phase 1](phase1_model_headless.md) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Tools: rig-tools + a risk-metadata registry + GuardedTool wrapper (fontaine-tools)

Reuse rather than rewrite. Fontaine does **not** define its own `Tool` trait — it builds on
`rig_core::tool::Tool` and adds the two things rig doesn't have for our security model: a
**risk classification** per tool and a **guard wrapper** that enforces the permission pipeline.

- **Base tools**: `rig-tools` ships `read`, `write`, `edit`, `search`, `shell`. These are adopted
  directly as the v1 built-ins; Fontaine adds `glob`/`grep` aliases over rig's `search` and a
  `webfetch` tool. Where rig-tools' `shell`/`write` need hardening for our model (workspace
  containment, symlink resolution, timeout, output cap), Fontaine wraps or configures them rather
  than forking — keeping upgrades cheap.
- **Composite read tools + Tier 0.5 shell pre-classifier** (user directive 2026-10-21; mirrors
  Claude Code's split of dedicated read tools that skip prompts from a built-in read-only command
  set for `Bash`, per the design tie-breaker). Frequent read operations are **also** exposed as
  dedicated first-party tools — `ls`/list, `read`, `glob`/`grep` search (wrapping the `rig-tools`
  equivalents) — with workspace containment enforced *inside the tool* at the execution boundary
  (pipeline step 4), and the system prompt steers the model toward these rather than `shell` for
  reads. The **Tier 0.5 pre-classifier** (pipeline step 5) is the fallback fast path for read-ish
  shell invocations: it is purely *structural* — lex the command line, check `argv[0]`+subcommand
  against the curated set, accept only path arguments that pass the scope checker plus flags on a
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

`WorkspaceWrite` (write/edit within workdir) is auto-allowed only in `acceptEdits`/`auto`(guardian)
per mode. `Arbitrary` (shell, webfetch, MCP tools) always hits the guardian in `auto` (shell calls
may be pre-classified safe at Tier 0.5 first — pipeline step 5 precedes the mode branches, matching Claude
Code's read-only Bash set prompting in no mode), prompts in `manual`. MCP tools arrive via
`fontaine-mcp` (rig-rmcp) and are registered `RiskClass::Arbitrary` (never trusted as safe
regardless of server `readOnlyHint` annotations — annotations are claims, surfaced to the
guardian as hints, not as gates).

**Guardian injection point — decision**: two viable seams exist. (1) A `rig_agent::AgentHook`
(`HookContext`) around tool-dispatch turns; (2) the `GuardedTool` decorator above. The plan adopts
**the GuardedTool decorator as primary** because it is guaranteed to wrap *every* tool call at the
registry level (including MCP), is easy to unit-test in isolation, and doesn't depend on rig's
hook semantics staying stable across pre-1.0 releases. A thin `AgentHook` is added only for
cross-cutting concerns (logging, denial-record bookkeeping, result-scan trigger), not for the
allow/deny decision itself.

## Steps

1. `fontaine-tools`: `RiskClass` registry; `GuardedTool` decorator over `rig_core::tool::Tool`;
   register rig-tools' read/write/edit/search/shell wrapped with risk classes; expose the dedicated
   first-party read tools (`ls`/`read`/`glob`/`grep`) with containment inside the tool; add
   `webfetch`; workspace-scope/symlink validator; harden shell (timeout, output cap) via wrapper
   config. The `GuardedTool` gate body is stubbed this phase (execute-after-check pass-through);
   the real `PermissionPipeline` lands in Phase 3 and fills the stub.
2. `fontaine-core`: session orchestration on `rig_agent` (`AgentBuilder` + `AgentRun`), system
   prompt, FONTAINE.md project-instruction loading; `rig-memory` for context windows, compaction,
   and save/resume — no custom agent loop or history management to write.
3. Docs — `docs/tools.md`: the built-in tool inventory (`ls`/`read`/`glob`/`grep`/`edit`/`write`/
   `shell`/`webfetch`), each tool's `RiskClass`, what earns the read-only fast path
   (Fontaine-enforced workspace containment — never the label), and the shell hardening (timeout,
   output cap); extend `docs/architecture.md` with the agent-loop diagram (rig runtime + wrapped
   registry); link the new page from `docs/index.md`.

## Files created or modified

`rust/fontaine-tools/src/{lib,guarded,risk,scope,webfetch,shell_hardening,preclassify}.rs`;
`rust/fontaine-core/src/{lib,session,hooks}.rs`; `docs/tools.md`, `docs/architecture.md`,
`docs/index.md`.

## Tests owned by this phase

- **tools** (master Testing Strategy): workspace-scope validator (path traversal, absolute paths,
  symlinks); shell hardening wrapper (timeout, output cap); risk-class registration of every
  built-in and MCP tool — the choke-point test that fails if any registered tool is missing a
  risk class.
- GuardedTool delegation: inner tool runs exactly once, only after the gate allows.

## Definition of done

- A headless `-p` run completes a full agent turn with a tool call end-to-end through the
  wrapped registry (gate stubbed per the no-bypass invariant — nothing gates past stub until
  Phase 3).
- Choke-point test green: every registered tool has a `RiskClass`.
- `docs/tools.md` complete for the shipped inventory.
- Log the phase in the master plan's Progress log.
