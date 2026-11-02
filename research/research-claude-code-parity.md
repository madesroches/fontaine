# Research — Claude Code feature parity and triage

Date: 2026-10-21. Companion to `tasks/fontaine_harness_plan.md`. Purpose: the plan's tie-breaker
is "when in doubt, do as Claude Code does", but the plan only ever looked at Claude Code's
*permission* surface (modes, auto mode, rules, critical paths). This doc lists the rest of Claude
Code's user-facing and architectural features, marks each one against the plan, and sorts the gaps
into three buckets: what the plan should design now, what can become a GitHub issue later, and
what doesn't fit Fontaine.

Primary sources: the Claude Code docs index (`code.claude.com/docs/llms.txt`, 2026-10-21) and the
pages linked from each matrix row, read in full for permissions, permission modes, settings,
hooks, tools reference, sessions, memory, context window, sub-agents, agent teams, MCP, skills,
sandboxing, headless, CLI reference, commands, model config, costs and monitoring. Fontaine
side: the master plan (Overview, Design, Config, Decisions), the seven phase plans, and the other
`research/` docs. Nothing in this doc changes the plan. The plan-now sketches below are proposals
for the plan's owners to merge. The backlog drafts sit in a scratch JSON file and have not been
filed.

## 1. Executive summary

- **112 features** enumerated. Fontaine status: **21 planned, 15 partial, 75 missing,
  1 excluded** (`bypassPermissions`, by Decision).
- Triage of the 90 partial/missing rows: **11 rows → 8 plan-now items (P1–P8)**, **62 backlog**
  (61 issue drafts; one row folds into another issue), **17 skip** (Anthropic-hosted or
  account-bound surfaces).
- The permission core matches Claude Code closely. The plan copies the modes, the deny → ask →
  critical-path floor, auto-mode prose lists, fail-closed behavior, the denial UX and the
  tool-result probe, and records its deviations. The gaps are in the layers around that core:
  configuration scopes, workspace trust, user hooks, extension files, session and context
  history, and the headless I/O contract. Each of these feeds into or bypasses the permission
  pipeline, so each is costly to add later.
- **Four places where the plan contradicts itself or leaves a hole** (§3):
  1. "Don't ask again" grants go to "explicit user-scope config rules", but `[permissions]` has
     no `allow` key.
  2. Project-scope MCP servers would execute code, which is a loosening path. The rule that
     project scope can only tighten doesn't cover it.
  3. Compaction rewrites the history that the guardian's intent inputs are read from.
  4. "Hooks" in the plan means rig `AgentHook` bookkeeping. Claude Code's user-configured hooks
     can allow, deny or rewrite tool calls, and the plan doesn't place them in the pipeline.

**User decisions (2026-10-21), merged into the plan.** P1: as sketched — narrow allow rules skip
the guardian in `auto` and broad code-execution rules are suspended, as in Claude Code; a
user-authored allow rule counts as a user grant. P4: **not** as recommended here — the user chose
Claude Code parity (TUI trust prompt; headless `-p` loads project config without one), so no
stricter headless rule and no deviation. P8: project agent files allowed, narrowing only. The
OpenTelemetry row (triaged backlog below) moved into the plan as observability, with a native
micromegas backend and an OTLP fallback, off by default.

## 2. Legend

- **Status**: `planned` = a phase plan builds it (cited in Notes); `partial` = some of it is
  planned, gaps named in Notes; `missing` = not in the plan; `excluded` = a Decision rules it
  out.
- **Triage** (partial/missing only): `plan-now (Pn)` = shapes the architecture, so the plan
  should say something now (sketches in §4); `backlog` = can be built later without reworking the
  core (issue draft title in Notes); `skip` = doesn't fit Fontaine (reason in Notes).
- Plan-now bar: the item changes the config schema or scope table, the permission-pipeline order,
  the session/history model, the tool interface, crate or extension boundaries, or a security
  invariant. Everything else goes to the backlog, even if it matters to users.

## 3. Plan inconsistencies surfaced by the comparison

1. **No home for allow grants.** The Tier-0.5 Decision says "don't ask again" grants "land only
   as explicit user-scope config rules", and the Phase 3 approval widget offers "allow session".
   But `[permissions]` only has `deny` and `ask`. → P1.
2. **Project-scope MCP is not covered by the trust rule.** Phase 5 step 1 loads "MCP servers from
   config" without saying which scope. A project-declared stdio server runs an arbitrary binary
   when the session starts, which is loosening that "project can only tighten" never considered.
   Claude Code's interactive mode asks before connecting `.mcp.json` servers, but **`claude -p`
   connects them and runs project hooks in folders that were never trusted** (permissions docs,
   "What runs before you trust a folder"). Fontaine should be stricter here and record that as a
   Decision. → P4. *(User decision 2026-10-21: match Claude Code instead — headless loads project
   config without a trust prompt.)*
3. **Compaction vs guardian inputs.** Phase 2 adopts rig-memory compaction, and Phase 4's
   `AdjudicationRequest` reads `task_intent` / `recent_user_messages` / "actions already taken".
   After compaction, the only copy of the user's words may be a summary the model wrote, and that
   summary can carry injected text into the guardian's intent field. → P6.
4. **"Hooks" means two things.** The plan's hooks are rig `AgentHook`s used for bookkeeping. In
   Claude Code, hooks are user-configured handlers on about 30 events. The `PreToolUse` handler
   can allow, deny, ask, defer or **rewrite the input** of a tool call, and deny/ask rules are
   checked against the rewritten input. If Fontaine adds hooks later without fixing their place
   in the pipeline, they become a way around the floor. → P5.

## 4. Plan-now items

Each item lists its target phase(s), a sketch of what the plan should say (2–4 sentences, the
form asked for), and why it is costly to retrofit. A deviation from Claude Code inside a sketch is
marked **(Decision needed)**.

### P1 — Allow rules and a structured rule-specifier grammar
**Phase:** `tasks/design/config.md` §Config + Phase 0 (loader/schema), Phase 3 (pipeline + approval widget).
CC: [permissions#permission-rule-syntax](https://code.claude.com/docs/en/permissions#permission-rule-syntax),
[permission-modes](https://code.claude.com/docs/en/permission-modes).

Sketch: Add `[permissions] allow` (user and project-local scope only; never shared project
scope) and define one rule grammar for `deny`/`ask`/`allow`: `tool(specifier)`. For `shell`, the
specifier is an exact-argv or word-prefix match over the Tier-0.5 shlex lexer (compound commands
are split and every part must match), never regex. For path tools, it is a gitignore-style path
pattern resolved through the scope validator. For `webfetch`, it is `domain:`. For MCP, it is a
`server`/`server__tool` name. Allow rules run after the deterministic floor and before the mode
branches, and they can never reach past the floor. In `auto`, broad code-execution allow rules
(bare `shell`, interpreter or package-manager wildcards, `agent`) are suspended, as in Claude
Code. Narrow allow rules skip the guardian, and that skip must be recorded explicitly against the
"fast path is earned by containment" invariant **(Decision needed)**. "Allow session" grants live
in `SessionState`, and "always" grants are written to user scope.

Why now: it adds a pipeline step and a config key whose scope sits on the trust boundary. The
rule grammar is shared by deny/ask, which Phase 3 builds. A grammar invented after Phase 3 would
force deny/ask to be rewritten.

### P2 — Multi-root workspace (additional working directories)
**Phase:** Phase 2 (scope validator, `SessionState`), `tasks/design/permissions.md` §Core data flow step 4.
CC: [permissions#working-directories](https://code.claude.com/docs/en/permissions#working-directories).

Sketch: The workspace becomes an ordered set of roots (the cwd plus `--add-dir` / `/add-dir`
plus `additional_directories` from user/local scope). Containment is defined as "inside any root
after symlink resolution". Adding a root at runtime is a human-prompt action (never
guardian-approved, denied in headless). As in Claude Code, extra roots grant file access but not
configuration: no `.fontaine/` settings, hooks or MCP servers are loaded from them. The
guardian's `session_facts` and trust boundary list every root. Worktrees and subagent isolation
reuse the same type later.

Why now: the read-only fast path and Tier 0.5 depend on what "in workspace" means. Phase 2's
validator, the Tier-0.5 path checks, and the critical-path "cwd and its parents" list all assume
one root today.

### P3 — Settings scope model: managed > CLI > project-local > project > user
**Phase:** `tasks/design/config.md` §Config scope table + Phase 0 step 1 (loader).
CC: [settings#settings-precedence](https://code.claude.com/docs/en/settings#settings-precedence),
[managed-settings](https://code.claude.com/docs/en/managed-settings).

Sketch: The loader becomes an ordered stack of scopes, each with a trust level:
- a reserved **managed** tier (root-owned system file, empty in v1), whose deny/ask/hard_deny
  entries can't be removed and which can lock keys;
- a **CLI** `--settings` overlay for one session;
- **project-local** `.fontaine/settings.local.toml` (gitignored, personal);
- **project** `.fontaine/settings.toml`;
- **user**.

The scope table gains a column for each scope. Project-local counts as user trust only while git
reports the file as untracked (Claude Code's rule); otherwise it is treated as project scope.
List keys merge across scopes (add-only from untrusted scopes).

Why now: Phase 0 writes the loader and the "rejected from project scope" tests. Adding layers
later changes every key's scope semantics and every one of those tests. Managed settings can
wait, but they need a reserved slot in the stack (backlog issue drafted).

### P4 — Workspace trust for project-sourced executable config
**Phase:** Phase 0 (trust store + loader hook), Phase 5 (MCP, hooks, skills consume it).
CC: [permissions#project-allow-rules-and-workspace-trust](https://code.claude.com/docs/en/permissions#project-allow-rules-and-workspace-trust),
[mcp#project-server-approvals-and-workspace-trust](https://code.claude.com/docs/en/mcp#project-server-approvals-and-workspace-trust).

Sketch: State an invariant: **anything in a repository that executes code or widens access is
loosening.** That covers project MCP servers, hooks, skill `!cmd` preprocessing and
`allowed-tools`, statusline/helper commands, `env`, and additional directories. None of it loads
until the user has trusted that folder, and the trust record is kept in the user data dir,
outside the repo. Project MCP servers also need per-server approval, and the repository can't
pre-approve its own servers. Headless `-p` **never** trusts implicitly. A user-scope
`--trust-workspace` flag or an existing trust record is required. This is stricter than Claude
Code, whose `-p` runs project hooks and `.mcp.json` servers untrusted **(Decision needed)**. Add
the trust state to the guardian's `session_facts`. *(User decision 2026-10-21: not adopted — the
plan matches Claude Code; headless `-p` loads project config without a trust prompt.)*

Why now: Phase 5's MCP loader and any hooks or skills design must call this gate. Without it,
the first `.fontaine/` feature that runs code breaks the "project can only tighten" invariant.

### P5 — User hooks: place in the pipeline, trust, and failure semantics
**Phase:** Phase 3 (fix the place in the pipeline and the `SessionState` inputs), Phase 5
(build). CC: [hooks](https://code.claude.com/docs/en/hooks),
[permissions#extend-permissions-with-hooks](https://code.claude.com/docs/en/permissions#extend-permissions-with-hooks).

Sketch: Keep rig `AgentHook` for bookkeeping, and add a separate **user hooks** layer (command
handlers first, http/prompt/agent handlers later). `PreToolUse` runs **before** the
deterministic floor. Any `updatedInput` it returns replaces the call, and the floor, Tier 0.5 and
the guardian all judge the rewritten call (Claude Code checks rules against hook-returned input).
`deny` and `ask` tighten. `allow` is treated exactly like a P1 allow rule: it never passes the
floor, and it is suspended in `auto` unless the Decision says otherwise. When several hooks
disagree, the precedence is deny > ask > allow. A hook that errors, times out or returns
malformed output **never loosens**: the call goes through the normal pipeline. `PermissionDenied`
(with `retry`) and a guardian-only context annotation (Claude Code's `classifierContext`) are the
other permission-relevant events. Hook sources follow P3/P4 scope and trust rules, and no project
file can set a "disable hooks" key that turns off user-scope hooks.

Why now: P5 inserts a step ahead of step 2 of the pipeline and can rewrite tool arguments, so the
pipeline-order diagram and the floor tests have to account for it.

### P6 — Session ledger: guardian inputs survive compaction; resume/fork semantics
**Phase:** Phase 2 step 2 (compaction lands), Phase 4 (request builder), Phase 5 step 2
(persistence). CC: [sessions](https://code.claude.com/docs/en/sessions),
[context-window#what-survives-compaction](https://code.claude.com/docs/en/context-window#what-survives-compaction).

Sketch: Keep two stores per session. The first is rig-memory's model context, which can be
compacted. The second is an append-only **session ledger** that Fontaine owns: user messages
verbatim, mode changes, approvals and grants, denial records, actions taken, git remotes and the
pre-session file set captured at start, and workspace roots. `AdjudicationRequest` is built only
from the ledger, never from a compaction summary. Specify what resume and fork restore:
- **Resume** restores the context and the ledger. Mode comes back as the user's default mode,
  not the saved one. Session-scope grants are dropped.
- **Fork** (`--fork-session` / `/branch`) copies both stores.
- **Rewind** puts the context back to an earlier prompt (`/rewind`) but keeps the ledger's denial and approval history; the guardian's view of intent drops only the retracted prompts.

Persist both stores as JSONL under the user data dir, keyed by session id.

Why now: the guardian request schema (Phase 4) and the compaction adoption (Phase 2) are being
built against an unstated history model. Separating the stores after both phases ship means
redoing the request builder and the on-disk format.

### P7 — Headless I/O contract (versioned)
**Phase:** Phase 1 step 2. CC: [headless](https://code.claude.com/docs/en/headless),
[cli-reference](https://code.claude.com/docs/en/cli-reference).

Sketch: Specify `--output-format text|json|stream-json` and `--input-format stream-json` as a
versioned event schema. It covers session init (model, guardian model, mode, roots, loaded config
sources), assistant deltas, tool calls, **gate decisions with per-tier attribution** (floor /
Tier 0.5 / allow-rule / guardian with rule, explanation and confidence), tool results, and a final
result with usage, cost and denial list. Also specify these flags:
- `--max-turns` and `--max-budget-usd`;
- `--json-schema` for structured final output, via rig `structured_output`;
- `--system-prompt` / `--append-system-prompt`;
- `--allowed-tools` / `--disallowed-tools`, which become CLI-scope P1 rules;
- `--session-id` / `--continue` / `--resume`.

An external approval delegate (Claude Code's `--permission-prompt-tool`) is possible later, but
the default stays "every would-prompt outcome is a Deny".

Why now: the eval runner, CI integration tests, a future GitHub Action, ACP/IDE bridges and any
SDK consumer all parse this stream. Changing it after Phase 4 publishes a scorecard built on it
breaks all of them.

### P8 — Extension file layout: skills, agent definitions, commands
**Phase:** master §Workspace layout + `tasks/design/config.md` §Config (layout + scope), Phase 5 (build).
CC: [skills](https://code.claude.com/docs/en/skills),
[sub-agents#configure-subagents](https://code.claude.com/docs/en/sub-agents#configure-subagents),
[claude-directory](https://code.claude.com/docs/en/claude-directory).

Sketch: Fix the directory layout under `~/.config/fontaine/` and `.fontaine/`: `skills/<name>/SKILL.md`,
`agents/<name>.md`, `output-styles/`. It should be compatible with Claude Code's frontmatter, so
existing `.claude/` skills and agents port over (reading `.claude/` directly is optional and
recorded as a Decision). Plugins later bundle this same layout. Skills are progressive-disclosure
prompt content loaded by a `skill` tool that needs a `RiskClass`. Custom slash commands are
user-invocable skills. A skill's `allowed-tools` never pre-approves anything past the floor or
the guardian, and in project scope it is ignored. `!cmd` dynamic context runs through the
pipeline as a shell call. Project-defined agent files can only narrow: toolset subset, and `mode`
limited to `manual`/`dontAsk`. `model` stays user-scope. This reconciles the
"`[agents.types]` is user-only" rule with Claude Code's project-scoped `.claude/agents/`
**(Decision needed)**.

Why now: the layout, the scope rules and the skill-tool risk class are extension points that
plugins, hooks-in-frontmatter and agent types all hang off. If Phase 5 ships agent types as TOML
only, the file format becomes a second, competing definition path.

## 5. Feature matrix

### Permissions & safety

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| Permission modes: default/manual, acceptEdits, plan, auto, dontAsk; Shift+Tab cycling | [permission-modes](https://code.claude.com/docs/en/permission-modes) | planned | — | Phase 3 (modes) + Phase 4 (auto). Same names/semantics; acceptEdits shell deviation recorded in Decisions. |
| bypassPermissions / --dangerously-skip-permissions | [permission-modes#skip-all-checks-with-bypasspermissions-mode](https://code.claude.com/docs/en/permission-modes#skip-all-checks-with-bypasspermissions-mode) | excluded | — | Decision 2026-10-21 (user): no bypass mode in v1; containers get `[guardian] profile = "minimal"`. |
| Auto mode classifier (separate model, fail-closed, deny reasons fed back, prose environment/allow/soft_deny/hard_deny lists, $defaults) | [auto-mode-config](https://code.claude.com/docs/en/auto-mode-config) | planned | — | Phase 4 guardian; `tasks/design/config.md` §Config `[auto_mode]`. Local/any-model is the differentiator. |
| Recently-denied list, retry, teach-the-classifier after repeated denials; fallback to prompting after N denials | [permission-modes#when-auto-mode-falls-back](https://code.claude.com/docs/en/permission-modes#when-auto-mode-falls-back) | planned | — | Phase 4 step 3 (`/denied`, loop breaker). |
| Server-side tool-result probe for injected content | [permission-modes#server-side-classifier-review](https://code.claude.com/docs/en/permission-modes#server-side-classifier-review) | planned | — | Phase 6 local PromptGuard 2 scanner (default off). |
| Deny and ask rules evaluated before everything, in every mode | [permissions#manage-permissions](https://code.claude.com/docs/en/permissions#manage-permissions) | planned | — | `tasks/design/permissions.md` §Core data flow step 2; Phase 3. |
| Allow rules + `Tool(specifier)` rule grammar (Bash prefix/wildcard, Read/Edit gitignore-style paths, WebFetch(domain:), MCP server/tool names); `--allowedTools`/`--disallowedTools`; 'Yes, don't ask again' persistence; broad code-exec allow rules dropped on entering auto | [permissions#permission-rule-syntax](https://code.claude.com/docs/en/permissions#permission-rule-syntax) | partial | **plan-now** (P1) | Fontaine has deny/ask but no `allow` key and no specifier grammar, yet Decisions route 'don't ask again' grants to 'explicit user-scope config rules' that have no home. Approval widget has allow-once/allow-session only. |
| Critical-path removals always prompt; protected paths (.git, dotfiles, settings files) | [permission-modes#critical-paths](https://code.claude.com/docs/en/permission-modes#critical-paths) | planned | — | `tasks/design/permissions.md` §Core data flow step 2 floor. |
| Additional working directories (`--add-dir`, `/add-dir`, `permissions.additionalDirectories`) | [permissions#working-directories](https://code.claude.com/docs/en/permissions#working-directories) | missing | **plan-now** (P2) | Workspace containment is what earns the fast path; plan assumes a single root. |
| Workspace trust dialog gating project settings, project MCP servers (`.mcp.json` approvals), hooks, helper commands | [permissions#project-allow-rules-and-workspace-trust](https://code.claude.com/docs/en/permissions#project-allow-rules-and-workspace-trust) | missing | **plan-now** (P4) | Plan says project scope can only tighten, but project-declared MCP servers/hooks/skills execute code - a loosening path with no gate yet. |
| Sandboxed Bash (bubblewrap/Seatbelt via sandbox-runtime; filesystem + network-proxy isolation; `/sandbox`) | [sandboxing](https://code.claude.com/docs/en/sandboxing) | planned | — | Phase 5 roadmap slot (srt/bubblewrap spike, `docs/sandboxing.md`); not a v1 gate. |
| PreToolUse hooks as permission extension (allow/deny/ask/defer, updatedInput), precedence vs rules | [permissions#extend-permissions-with-hooks](https://code.claude.com/docs/en/permissions#extend-permissions-with-hooks) | missing | **plan-now** (P5) | See hooks row; covered by P5. |
| Network-path read prompts / first read outside the working directories | [permission-modes#the-first-read-outside-the-working-directories](https://code.claude.com/docs/en/permission-modes#the-first-read-outside-the-working-directories) | done | shipped (#284) | Every out-of-root `read`/`ls`/`glob`/`grep` call is adjudicated per call (floor, allow rules, then the mode) instead of refused for containment; the target is resolved once after hooks so approval and execution cannot drift. Unlike Claude Code's first-read prompt, Fontaine has no separate first-external-read decision and no blanket "read any path" grant: external reads never earn the containment fast path, an external path never becomes a root, and execution runs on a dedicated unconfined read-only fallback instance reached only after the gate allows the call. Writes and `shell` stay confined (#284). |
| `claude auto-mode defaults\|config\|critique` - inspect effective classifier rules | [auto-mode-config](https://code.claude.com/docs/en/auto-mode-config) | done | shipped (#1) | `fontaine auto-mode defaults\|config\|critique [--format text\|json]`. `config` prints the effective lists with the scopes that set each list (per list, not per entry) and tags entries default or custom; `critique` asks the guardian's model for advisory free-text review of the custom entries (no structured verdict) and is the only one that makes a request. `--format` on all three is not verified against `claude auto-mode`. A prompt that is exactly `auto-mode` runs the subcommand; `fontaine -p -- auto-mode` is the literal prompt. |
| MCP tools marked `requiresUserInteraction` always prompt | [mcp#require-approval-for-a-specific-tool](https://code.claude.com/docs/en/mcp#require-approval-for-a-specific-tool) | done | n/a | Read off `tools/list` by a transport tap and enforced as an implicit `ask` rule at the floor; see `docs/mcp.md` (#2). |

### Configuration

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| Settings hierarchy: managed > CLI `--settings` > project-local (`settings.local.json`) > project > user; list keys merge | [settings#settings-precedence](https://code.claude.com/docs/en/settings#settings-precedence) | partial | **plan-now** (P3) | Fontaine has user + project only; no local or managed tier, no `--settings`, merge rules only implicit (add-only). |
| Enterprise managed settings (system file / MDM, locked keys, `allowManagedPermissionRulesOnly`, managed MCP allowlists) | [managed-settings](https://code.claude.com/docs/en/managed-settings) | missing | backlog | P3 reserves the tier; the deployment mechanics can come later. Issue: "Managed (system) settings tier for organizations". |
| `/config` interactive settings UI | [settings#use-the-config-menu](https://code.claude.com/docs/en/settings#use-the-config-menu) | partial | shipped (#4) | `/config` (`/settings`) lists every key with its effective value and source scope and edits `tui.theme`, `tui.screen_reader`, `context.autocompact_window`, `schedule.enabled` and the two model keys. Deviations: writes only the user file and names a winning higher scope instead of writing it; most edits apply from the next launch; every other key is read-only (see the plan's Decisions). |
| Environment variables surface (`env` settings key, provider/env overrides) | [env-vars](https://code.claude.com/docs/en/env-vars) | partial | shipped (#5) | `[env]` table of literal pairs set on `shell`, `!` commands, hooks and stdio MCP servers (user-trust scopes only; see the plan's Decisions for the project-scope and own-process deviations). Provider and proxy settings beyond `env` remain separate issues. |
| Proxy / custom CA / mTLS network configuration | [network-config](https://code.claude.com/docs/en/network-config) | done | n/a | `[network]` and per-account keys: proxy, `no_proxy`, `ca_bundle` (added to the system roots), `client_cert`/`client_key` (unencrypted PEM only). Environment variables are still honored when no proxy key is set (#6). |

### Memory & instructions

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| CLAUDE.md hierarchy (managed/user/project/local, ancestor + lazily-loaded nested subdirectory files), `@path` imports, `.claude/rules/` path-scoped rules, AGENTS.md support, `/memory`, `/init` | [memory](https://code.claude.com/docs/en/memory) | done | shipped (#7) | `FONTAINE.md`/`CLAUDE.md`/`AGENTS.md` hierarchy, ancestor walk, on-demand nested files and path rules, `@path` imports, `.claude/rules/`, `/memory`, `/init`. Deviations: repository symlinks and imports that leave the workspace and work tree are refused (no approval dialog), imports from files above the work tree are confined, `read` deny rules keep files out of the prompt, `/memory` is a numbered list, shell reads do not load nested files, `.claude` is protected. The guardian reads a session-start snapshot of the launch set. No managed policy file. |
| Auto memory (Claude writes its own learnings to a per-project memory dir) | [memory#auto-memory](https://code.claude.com/docs/en/memory#auto-memory) | done | shipped (#8) | `[memory] auto` (user scope; on by default while tested), store in `<data dir>/memory/<key>/` instead of `~/.claude/projects/`, `MEMORY.md` index capped at 200 lines / 25,000 bytes. Deviations: every write is gated by the protected-path floor and, in `auto`, the guardian (no fast path); the notes are framed as unverified data and kept out of the guardian's project instructions; no automatic subagent memory; `/memory` lists the files and has no delete action. |
| Output styles (built-in + custom system-prompt overlays, `/output-style`) | [output-styles](https://code.claude.com/docs/en/output-styles) | missing | backlog | Issue: "Output styles: custom system-prompt overlays". |

### Sessions & context

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| Session persistence, `--continue`, `--resume` picker, naming/`/rename`, `--fork-session`/`/branch`, `/export`, transcript storage | [sessions](https://code.claude.com/docs/en/sessions) | partial | **plan-now** (P6) | Phase 5 step 2 plans save/resume via AgentRun + JSONL; no fork, naming, picker, or spec of what a resume restores (mode, grants, denials). |
| Session picker, naming, export UX | [sessions#use-the-session-picker](https://code.claude.com/docs/en/sessions#use-the-session-picker) | missing | backlog | UX on top of P6's model. Issue: "Session UX: resume picker, naming, /rename, /branch, /export". |
| Auto-compaction + `/compact [focus]`, what survives compaction, `/context` breakdown, autocompact window config | [context-window](https://code.claude.com/docs/en/context-window) | partial | **plan-now** (P6) | rig-memory compaction is planned (Phase 2 step 2) but its interaction with the guardian's intent inputs is unspecified - core of P6. |
| `/compact`, `/context`, `/clear` commands and compaction thresholds | [commands](https://code.claude.com/docs/en/commands) | missing | backlog | Issue: "Context commands: /compact [focus], /context, /clear, auto-compact threshold". |
| Checkpointing: automatic per-prompt file snapshots, `/rewind` / Esc-Esc (code, conversation, or both), summarize-from-here | [checkpointing](https://code.claude.com/docs/en/checkpointing) | done | shipped (#12) | `write`/`edit` save pre-images through the `GuardedTool` seam; `/rewind` and Esc Esc restore code, conversation or both; summarize-from-here is not offered. See docs/checkpoints.md. |
| Prompt caching managed automatically (cache breakpoints, cache hit visibility) | [prompt-caching](https://code.claude.com/docs/en/prompt-caching) | missing | backlog | Model-agnostic: Anthropic explicit breakpoints, OpenAI automatic prefix caching. Issue: "Prompt-caching strategy for the main agent and guardian". |

### Headless & automation

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| `-p` print mode, exit semantics, denials returned to agent | [headless](https://code.claude.com/docs/en/headless) | planned | — | Phase 1 step 2. |
| `--output-format text\|json\|stream-json`, `--input-format stream-json`, `--include-partial-messages`, `--json-schema`, `--max-turns`, `--max-budget-usd`, `--system-prompt`/`--append-system-prompt`, `--permission-prompt-tool` | [cli-reference](https://code.claude.com/docs/en/cli-reference) | missing | **plan-now** (P7) | Eval harness, CI and any SDK consumers bind to this contract. |
| GitHub Actions (`@claude` mentions, issue -> PR), GitLab CI | [github-actions](https://code.claude.com/docs/en/github-actions) | missing | backlog | Issue: "GitHub Action and GitLab CI recipe wrapping `fontaine -p`". |
| Agent SDK (TypeScript/Python library over the same harness: tools, hooks, permissions callbacks, sessions) | [agent-sdk/overview](https://code.claude.com/docs/en/agent-sdk/overview) | partial | backlog | fontaine-guardian is planned lift-out-able; no public embedding API for the full agent. Issue: "Embeddable Rust API for the Fontaine agent (SDK parity)". |
| Scheduled in-session prompts: `/loop`, CronCreate/List/Delete, ScheduleWakeup | [scheduled-tasks](https://code.claude.com/docs/en/scheduled-tasks) | done | n/a | `/loop` (interval and self-paced), `cron_create`/`cron_jobs`/`cron_delete`/`schedule_wakeup`, restored on resume (#16). Bare `/loop` (maintenance prompt, `loop.md`) and skill expansion at fire time are not done. |
| `/goal` completion condition (keep working until a judged condition holds) | [goal](https://code.claude.com/docs/en/goal) | done | n/a | `/goal <condition>`, `/goal` and `/goal clear` in the terminal UI and headless `-p "/goal ..."`, with the evaluator on the guardian's model (#17). Adds a turn cap and an optional cost cap; no Stop-hook event. |
| Deep links (`claude-cli://` opens a session with repo + prompt) | [deep-links](https://code.claude.com/docs/en/deep-links) | missing | backlog | Issue: "Deep links that open a Fontaine session with a prompt". |

### Built-in tools

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| Read / Write / Edit / Bash / Glob / Grep | [tools-reference](https://code.claude.com/docs/en/tools-reference) | planned | — | Phase 2 (`read`/`write`/`edit`/`shell`/`glob`/`grep` + `ls`) via rig-tools with risk classes. |
| WebFetch | [tools-reference#webfetch-tool-behavior](https://code.claude.com/docs/en/tools-reference#webfetch-tool-behavior) | planned | — | Phase 2 `webfetch` (Arbitrary). |
| WebSearch | [tools-reference#websearch-tool-behavior](https://code.claude.com/docs/en/tools-reference#websearch-tool-behavior) | done | — | Shipped as `websearch` (client-side Tavily or SearXNG backend, optionally launched on demand) plus the provider-native server tool for `anthropic` accounts; see [Web search](../docs/tools.md#web-search) (#19). |
| NotebookEdit (Jupyter cells) | [tools-reference#notebookedit-tool-behavior](https://code.claude.com/docs/en/tools-reference#notebookedit-tool-behavior) | done | shipped (#20) | `notebook_edit` (workspace write; `NotebookEdit` rules and `allowed-tools` map to it) plus a notebook-aware `read`; see [Tools](../docs/tools.md). |
| Read of images and PDFs; image paste into prompt | [tools-reference#read-tool-behavior](https://code.claude.com/docs/en/tools-reference#read-tool-behavior) | done | shipped (#21) | `read` sniffs images (PNG/JPEG/GIF/WebP) and returns them as image parts to image-capable models (5 MB cap, text note otherwise); a PDF read is always a text stub pointing at attach. The TUI attaches a clipboard image with Ctrl+V and a dropped PDF by dropping its path; both ride the prompt as base64 content parts. See [Tools](../docs/tools.md) and [TUI](../docs/tui.md#attaching-files). |
| Task list tools (TaskCreate/Get/List/Update; legacy TodoWrite) | [tools-reference](https://code.claude.com/docs/en/tools-reference) | missing | backlog | Issue: "Task/todo list tools with TUI rendering". |
| AskUserQuestion (multiple-choice clarification) | [tools-reference#askuserquestion-tool-behavior](https://code.claude.com/docs/en/tools-reference#askuserquestion-tool-behavior) | done | — | Shipped as `ask_user_question`: root agent only, headless returns "unavailable", typed "Other" text becomes guardian intent (#23). |
| EnterPlanMode / ExitPlanMode (plan presented for approval, then mode switch) | [permission-modes#review-and-approve-a-plan](https://code.claude.com/docs/en/permission-modes#review-and-approve-a-plan) | partial | backlog | Plan mode is planned (Phase 3) but the exit/approval flow and target-mode choice are unspecified. Issue: "Plan mode exit: ExitPlanMode plan-approval flow". |
| Background Bash (`run_in_background`, auto-backgrounding long commands), Monitor tool, TaskStop, output files | [interactive-mode#how-backgrounding-works](https://code.claude.com/docs/en/interactive-mode#how-backgrounding-works) | done | — | `shell` promotes a command past its timeout to a background job, `run_in_background`, `job_output`, `job_cancel` and `monitor`; see the tools page; output is kept in memory (1 MiB per job), not spooled to files (#25); the timeout cap is `[shell] max_timeout_secs` (Claude Code: `BASH_MAX_TIMEOUT_MS`). |
| Shell working-directory persistence across calls; `cd` handling in permission checks | [tools-reference#bash-tool-behavior](https://code.claude.com/docs/en/tools-reference#bash-tool-behavior) | done | shipped (#26) | The directory a foreground call ends in is kept per agent and written into the next call's `cwd` before the gate, so the scope validator, pre-classifier and floor resolve against it. Resets to the primary root (Claude Code: the project directory) when it leaves the roots; `/clear` and `/rewind` reset it. |
| LSP code-intelligence tool (definitions, references, diagnostics after edits) | [tools-reference#lsp-tool-behavior](https://code.claude.com/docs/en/tools-reference#lsp-tool-behavior) | done | shipped (#27) | `lsp` (read-only, nine operations, `path` instead of `filePath`, position optional for `documentSymbol`/`workspaceSymbol`) over servers defined in the user file or by installed plugins, and a diagnostics note on every handled `edit`/`write` result with late diagnostics riding the next tool result. Deviations: user scope only (no project-defined servers), `workspaceFolder` ignored, no rename/code actions. See [tools](../../docs/tools.md#language-servers). |
| PowerShell tool / native Windows | [tools-reference#powershell-tool](https://code.claude.com/docs/en/tools-reference#powershell-tool) | done | shipped (#28) | Separate Windows-only `powershell` tool, never pre-classified; critical-path scan for `Remove-Item`. |
| Large tool outputs truncated / spooled to file | [tools-reference](https://code.claude.com/docs/en/tools-reference) | planned | — | Phase 2 shell hardening output cap. |

### Extensibility

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| Hooks: lifecycle events (SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest, PermissionDenied, PostToolUse, Stop, SubagentStart/Stop, PreCompact/PostCompact, Notification, ...); command/http/mcp_tool/prompt/agent handler types | [hooks](https://code.claude.com/docs/en/hooks) | missing | **plan-now** (P5) | Plan's 'hooks' are rig AgentHooks for bookkeeping only; no user-configurable hooks. |
| Hook event set beyond the permission-relevant ones (build-out) | [hooks-guide](https://code.claude.com/docs/en/hooks-guide) | missing | backlog | Issue: "Lifecycle hooks: full event set and handler types". |
| Skills (SKILL.md + frontmatter, progressive disclosure, user/project/plugin scope, Skill tool, `allowed-tools`, `!cmd` dynamic context, custom slash commands merged into skills) | [skills](https://code.claude.com/docs/en/skills) | missing | **plan-now** (P8) | — |
| Custom subagent definitions as files (`.claude/agents/*.md`, frontmatter: tools, model, permissionMode, hooks), `/agents`, `--agent`/`--agents` | [sub-agents#configure-subagents](https://code.claude.com/docs/en/sub-agents#configure-subagents) | partial | **plan-now** (P8) | Plan has `[agents.types]` in user-scope TOML only; no project-defined agents, no file format. |
| Plugins (bundle skills/agents/hooks/MCP/LSP/output styles) + marketplaces, dependencies, `/plugin` | [plugins/overview](https://code.claude.com/docs/en/plugins/overview) | done | — | Installed user-scope only and pinned by commit; a read-only `/plugins` and a `fontaine plugin` CLI. Plugin content is tighten-only (its `allow` never counts); plugin dependencies and mods stay unsupported; LSP servers load (`.lsp.json` and the `lspServers` key). See [plugins](../../docs/plugins.md). |
| Mods (in-process UI/behaviour extensions, `tool.check` events) | [plugins/mods/overview](https://code.claude.com/docs/en/plugins/mods/overview) | missing | skip | Mods can approve calls past deny rules in some setups - incompatible with the deterministic-floor invariant; plugins cover the safe subset. |

### MCP

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| stdio + streamable HTTP servers from config; tools wrapped and gated | [mcp](https://code.claude.com/docs/en/mcp) | planned | — | Phase 5 step 1; all MCP tools `Arbitrary`, annotations are hints. |
| MCP scopes (local/project `.mcp.json`/user), `claude mcp add/list/get/remove`, env expansion, `/mcp` panel | [mcp#mcp-installation-scopes](https://code.claude.com/docs/en/mcp#mcp-installation-scopes) | partial | backlog | Project-scope servers need the workspace-trust gate (P4). Issue: "MCP scopes and `fontaine mcp` management CLI". |
| Remote MCP OAuth (dynamic client registration, fixed callback port, pre-configured creds, headersHelper) | [mcp#authenticate-with-remote-mcp-servers](https://code.claude.com/docs/en/mcp#authenticate-with-remote-mcp-servers) | missing | backlog | Issue: "OAuth for remote MCP servers". |
| MCP resources (@-mentions, List/ReadMcpResource tools) and MCP prompts as slash commands | [mcp#use-mcp-resources](https://code.claude.com/docs/en/mcp#use-mcp-resources) | missing | backlog | Issue: "MCP resources and prompts". |
| MCP tool search / deferred tool loading (ToolSearch) for large tool sets | [mcp#scale-with-mcp-tool-search](https://code.claude.com/docs/en/mcp#scale-with-mcp-tool-search) | missing | backlog | Issue: "Deferred MCP tool loading (tool search)". |
| MCP elicitation, dynamic tool list updates, auto-reconnect, output-size limits, images in results | [mcp#respond-to-mcp-elicitation-requests](https://code.claude.com/docs/en/mcp#respond-to-mcp-elicitation-requests) | missing | backlog | Issue: "MCP robustness: elicitation, list_changed, reconnect, output limits". |
| SSE and WebSocket MCP transports | [mcp#option-2-add-a-remote-sse-server](https://code.claude.com/docs/en/mcp#option-2-add-a-remote-sse-server) | missing | backlog | Issue: "Legacy SSE and WebSocket MCP transports". |
| Claude Code as an MCP server (`claude mcp serve`) | [mcp#use-claude-code-as-an-mcp-server](https://code.claude.com/docs/en/mcp#use-claude-code-as-an-mcp-server) | missing | backlog | Issue: "`fontaine mcp serve`: expose Fontaine's guarded tools as an MCP server". |
| Channels (MCP servers push webhooks/alerts into a running session) | [channels](https://code.claude.com/docs/en/channels) | missing | backlog | Issue: "Channels: MCP-pushed events into a running session". |
| Claude.ai connectors synced into Claude Code | [mcp#use-mcp-servers-from-claudeai](https://code.claude.com/docs/en/mcp#use-mcp-servers-from-claudeai) | missing | skip | Requires claude.ai account infrastructure; user-configured remote MCP covers the need. |
| Managed MCP allow/deny lists | [managed-mcp](https://code.claude.com/docs/en/managed-mcp) | missing | backlog | No separate issue: folded into "Managed (system) settings tier for organizations". |

### Agents & parallelism

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| Subagents (Agent tool, own context, model, restricted tools; background; depth cap; concurrency limit) | [sub-agents](https://code.claude.com/docs/en/sub-agents) | planned | — | Phase 5 step 3; child mode <= parent is a recorded deviation. |
| Built-in subagents (Explore, Plan, general-purpose) | [sub-agents#built-in-subagents](https://code.claude.com/docs/en/sub-agents#built-in-subagents) | planned | — | `[agents.types]` explore/worker seeds. |
| Subagent output scanning at handoff | [sub-agents#subagent-output-scanning](https://code.claude.com/docs/en/sub-agents#subagent-output-scanning) | planned | — | Deviation recorded: result scanner (Phase 6), not classifier. |
| Fork the current conversation into a subagent (inherits context) | [sub-agents#fork-the-current-conversation](https://code.claude.com/docs/en/sub-agents#fork-the-current-conversation) | missing | backlog | Issue: "Forked subagents that inherit the parent context". |
| Agent teams (named teammates, shared task list, SendMessage, TeammateIdle hooks, tmux display) | [agent-teams](https://code.claude.com/docs/en/agent-teams) | missing | backlog | Peer messages already planned to pass the guardian. Issue: "Agent teams: named teammates with a shared task list". |
| Agent view (dashboard of many sessions), cross-session messaging, `claude agents`/`attach`/`daemon` | [agent-view](https://code.claude.com/docs/en/agent-view) | missing | backlog | Issue: "Multi-session view and local cross-session messaging". |
| Dynamic workflows (Workflow tool: script orchestrating many subagents) | [workflows](https://code.claude.com/docs/en/workflows) | missing | backlog | Issue: "Scripted multi-agent workflows". |
| Git worktrees (`--worktree`, EnterWorktree/ExitWorktree, subagent `isolation: worktree`, `.worktreeinclude`) | [worktrees](https://code.claude.com/docs/en/worktrees) | missing | backlog | Relies on multi-root containment (P2). Issue: "Git worktree isolation for sessions and subagents". |
| Advisor tool (consult a stronger model at key moments) | [advisor](https://code.claude.com/docs/en/advisor) | missing | backlog | Plays to the multi-account model registry. Issue: "Advisor tool: consult a second model". |

### Interactive TUI

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| Streaming transcript, input box, approval prompts, status bar with mode | [interactive-mode](https://code.claude.com/docs/en/interactive-mode) | planned | — | Phase 3 step 2. |
| Approval prompt with diff preview and editable/explained command | [permissions](https://code.claude.com/docs/en/permissions) | partial | backlog | Phase 3 plans allow once/session/deny + feedback; no diff preview spec. Issue: "Approval prompt: diff preview for edits, command explanation". |
| Input modes: `!` shell passthrough, `@` file mentions, `/` commands, multiline, history + Ctrl-R, image paste, message queueing / steering mid-turn, Esc interrupt | [interactive-mode](https://code.claude.com/docs/en/interactive-mode) | missing | backlog | Issue: "TUI input modes: !shell, @mentions, history search, queued/steering messages". |
| Built-in slash commands (/help, /clear, /status, /cost, /usage, /diff, /copy, /exit, /doctor, /release-notes, /feedback...) | [commands](https://code.claude.com/docs/en/commands) | partial | backlog | Plan names /permissions, /model, /denied, /guardian-model only. Issue: "Core slash-command set". |
| Custom status line (shell command fed JSON session data) | [statusline](https://code.claude.com/docs/en/statusline) | missing | backlog | Issue: "Custom status line command". |
| Customizable keybindings (`keybindings.json`, chords), vim mode | [keybindings](https://code.claude.com/docs/en/keybindings) | missing | backlog | Issue: "Configurable keybindings and vim input mode". |
| Themes, fullscreen/alt-screen, accessibility (screen reader), terminal setup | [terminal-config](https://code.claude.com/docs/en/terminal-config) | partial | shipped (#50) | Themes (six presets, `/theme`), screen-reader mode and `docs/terminal.md` (terminal setup as docs, no `/terminal-setup` command); fullscreen/alt-screen not done. |
| Notifications when input/approval is needed (bell/desktop) | [terminal-config](https://code.claude.com/docs/en/terminal-config) | missing | backlog | Issue: "Notify when an approval or input is pending". |
| Side questions `/btw`, `/recap`, prompt suggestions | [commands](https://code.claude.com/docs/en/commands) | missing | backlog | Issue: "Session conveniences: /btw side questions, /recap". |
| Background-agents panel / `/tasks` | [interactive-mode](https://code.claude.com/docs/en/interactive-mode) | planned | — | Phase 5 agents panel. |
| Voice dictation | [voice-dictation](https://code.claude.com/docs/en/voice-dictation) | missing | skip | Out of scope for a terminal harness; OS dictation works in any terminal. |

### Models, cost & observability

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| Model selection (`/model`, `--model`, aliases), separate classifier model | [model-config](https://code.claude.com/docs/en/model-config) | planned | — | Phase 1 accounts + Phase 3 `/model` + Phase 4 `/guardian-model`. Multi-provider goes beyond CC. |
| Effort levels / extended thinking (`/effort`, `--effort`, per-subagent effort) | [model-config#adjust-effort-level](https://code.claude.com/docs/en/model-config#adjust-effort-level) | missing | backlog | Issue: "Reasoning effort / thinking budget control". |
| Fallback model chains / automatic fallback, model allowlists | [model-config#fallback-model-chains](https://code.claude.com/docs/en/model-config#fallback-model-chains) | missing | backlog | Guardian must never silently fall back (Phase 4 rule). Issue: "Main-model fallback chain (guardian excluded)". |
| Fast mode | [fast-mode](https://code.claude.com/docs/en/fast-mode) | missing | skip | Anthropic pricing/serving tier, not a harness feature; per-account model choice covers it. |
| Bedrock / Vertex / Foundry provider setup | [amazon-bedrock](https://code.claude.com/docs/en/amazon-bedrock) | partial | backlog | rig supports Bedrock/Vertex; account `kind` list omits them. Issue: "Cloud-provider account kinds: Bedrock, Vertex, Azure/Foundry". |
| LLM gateway support (custom base URL, headers) | [llm-gateway](https://code.claude.com/docs/en/llm-gateway) | planned | — | `[accounts] base_url` (openai-compatible / LiteLLM). |
| Authentication (`/login`, API keys, long-lived tokens) | [authentication](https://code.claude.com/docs/en/authentication) | planned | — | `fontaine auth login`, env-var keys (Phase 1). |
| Cost and usage tracking (`/cost`, `/usage`, `/stats`, token counter, per-model breakdown) | [costs](https://code.claude.com/docs/en/costs) | partial | backlog | Budgets (Phase 5) need cost accounting; no user-facing surface planned. Issue: "Cost and token usage tracking (/cost, /usage) incl. guardian spend". |
| OpenTelemetry metrics/events/traces export; security audit events | [monitoring-usage](https://code.claude.com/docs/en/monitoring-usage) | partial | backlog | Plan has a local audit log only. Issue: "OpenTelemetry export of usage and guardian decision events". |
| Debugging config: `/doctor`, `--debug`, `/hooks`, `/mcp` status, `claude logs` | [debug-your-config](https://code.claude.com/docs/en/debug-your-config) | missing | backlog | Issue: "`fontaine doctor` and config introspection". |

### Integrations & surfaces

| Feature | CC doc | Fontaine status | Triage | Notes |
|---|---|---|---|---|
| Git/PR workflows: commit/PR creation via gh, `--from-pr`, PR status, `/pr-comments`, `/review`, `/security-review` | [common-workflows](https://code.claude.com/docs/en/common-workflows) | missing | backlog | Issue: "PR workflows: --from-pr, /review, /security-review skills". |
| IDE integrations (VS Code, JetBrains: diffs in IDE, selection context, diagnostics) | [vs-code](https://code.claude.com/docs/en/vs-code) | missing | backlog | Issue: "IDE integration via Agent Client Protocol". |
| Dev container reference config | [devcontainer](https://code.claude.com/docs/en/devcontainer) | missing | backlog | Pairs with the minimal guardian profile. Issue: "Reference dev container with minimal guardian profile". |
| Chrome browser integration | [chrome](https://code.claude.com/docs/en/chrome) | missing | skip | Extension + Anthropic-hosted bridge; a browser MCP server gives the same capability through the gate. |
| Computer use (macOS GUI control) | [computer-use](https://code.claude.com/docs/en/computer-use) | missing | skip | macOS-specific, provider-specific tool; far outside the guardian's tested surface. |
| Claude Code on the web / cloud sessions, `--cloud`, `--teleport`, cloud environments | [claude-code-on-the-web](https://code.claude.com/docs/en/claude-code-on-the-web) | missing | skip | Anthropic-hosted execution. |
| Remote Control, mobile app, desktop app | [remote-control](https://code.claude.com/docs/en/remote-control) | missing | skip | Anthropic-hosted relay and apps. |
| Cloud scheduled tasks (routines), RemoteTrigger | [routines](https://code.claude.com/docs/en/routines) | missing | skip | Cloud scheduler; local scheduling is the backlog /loop item. |
| Slack / Claude Tag | [slack](https://code.claude.com/docs/en/slack) | missing | skip | Anthropic-hosted chat integration. |
| Artifacts (claude.ai-hosted pages) | [artifacts](https://code.claude.com/docs/en/artifacts) | missing | skip | Publishes to claude.ai. |
| Managed Code Review service, ultrareview, ultraplan | [code-review](https://code.claude.com/docs/en/code-review) | missing | skip | Anthropic-hosted multi-agent services; local review is a skill. |
| Claude Projects (parallel cloud sessions sharing memory) | [claude-projects](https://code.claude.com/docs/en/claude-projects) | missing | skip | Cloud orchestration product. |
| Apps gateway / spend limits, server-managed settings, HIPAA/ZDR configs | [claude-apps-gateway](https://code.claude.com/docs/en/claude-apps-gateway) | missing | skip | Anthropic account/compliance infrastructure. |
| Self-hosted cloud environments / runners | [self-hosted-environments](https://code.claude.com/docs/en/self-hosted-environments) | missing | skip | Runner for Anthropic's cloud-session product. |
| Plan/billing commands (/upgrade, /usage-credits, /passes, /rate-limit-options), /stickers, /radio, /powerup | [commands](https://code.claude.com/docs/en/commands) | missing | skip | Subscription/billing and marketing surfaces. |
| In-product feedback upload (`/feedback`, SendFeedback) | [commands](https://code.claude.com/docs/en/commands) | missing | skip | Sends reports to Anthropic; Fontaine links to its issue tracker instead. |

## 6. Backlog handling

The 61 issue drafts (title, body with Claude Code doc link, suggested approach and phase
dependency, labels) are in the session scratch file `parity-issues.json`, in a form ready for
`gh issue create`. **They have not been filed**: filing needs user sign-off. Suggested labels
follow `parity` + `enhancement` + one or two `area:*` labels + the phase they depend on (`phase-N`),
with `priority:low` on cosmetic or niche items. None of the backlog items touches the security
invariants. Several carry a one-line guardrail in their body (e.g. "never guardian intent",
"user scope only") that keeps it that way when the issue is picked up.

## 7. Notes on method and limits

- The docs reflect Claude Code as of 2026-10 (v2.1.30x). Features change weekly. The weekly
  "What's new" pages were not read one by one.
- `planned` means the plan has a step for it. It doesn't mean the plan matches Claude Code in
  every detail. Detailed baselines are what the `claude -p` differential captures in Phases 3/4
  are for.
- Rows group related flags or commands where they share a triage outcome. Counts are rows, not
  individual flags.
