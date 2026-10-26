# Instruction File Hierarchy Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/7

## Overview

Fontaine reads two `FONTAINE.md` files today: the user's and the primary root's. Claude Code
reads a hierarchy of `CLAUDE.md` files. They come from the user, from every ancestor directory,
from subdirectories on demand, through `@path` imports, from `.claude/rules/` (some rules scoped
to paths), and from `AGENTS.md` when a repo has no `CLAUDE.md`. Claude Code also offers `/memory`
and `/init`. This plan gives Fontaine the same hierarchy under three names: `FONTAINE.md` (native),
`CLAUDE.md` and `AGENTS.md`. A repo already set up for Claude Code or for other agents then works
unchanged. The plan also defines dedup across the names and confines symlinks and imports in
repo-controlled files to the workspace roots and the enclosing git work tree. It pins exactly which files feed the guardian's
`project_instructions`, and adds `/memory` and `/init` to the TUI.

## Current State

- `rust/fontaine-core/src/prompt.rs:17-37`: `INSTRUCTIONS_FILE = "FONTAINE.md"`.
  `load_instructions(config_dir, primary_root)` reads `<config dir>/FONTAINE.md`, then
  `<primary root>/FONTAINE.md`. Each `InstructionFile { path, text }` that is not blank becomes
  one framed layer (`Instructions from <path>:`) in `layers()` (`prompt.rs:96-110`).
- The loader is called in three places, each re-reading the disk:
  - `agent/build.rs:113` `prompt_layers`, which runs on every agent rebuild, i.e. at the
    rebuild points: model switch, output-style change and `/add-dir` (`AgentRuntime::rebuild`).
    `/clear` and compaction do not rebuild the agent.
  - `agent/setup.rs:191` `session_state`, which computes the guardian's
    `AutoContext::project_instructions` once per session through
    `adjudication::project_instructions` (`adjudication.rs:44-54`: `From <path>:` blocks, capped
    at `INSTRUCTION_CHARS = 16_000`).
  - `agent/setup.rs:381` the subagent preamble closure (no skill index, same instruction files).
- Tool results pass through `fontaine_tools::result_pipeline::ResultPipeline`, whose stages run
  in the fixed `StageKind` order `Notifications → Scanner → PostToolUse`. Each stage sees a
  `CallView` with `tool`, `args` and `agent`. Stages are attached in `agent/setup.rs:274-300`.
  `result_pipeline::append_note` appends text to a result.
- `destructive::touched_paths` (`destructive.rs:275`) resolves the `path` argument of
  `read`/`write`/`edit` against the `Scope`.
- `frontmatter.rs` parses the Claude-Code-compatible YAML subset used by skills, agent files and
  output styles: `Document::get`, `text`, `body`, and `split_commas`.
- `permission_rules::matches(rule, &CallContext { tool, args, scope, home }, Posture::Deny)`
  evaluates a deny rule against a constructed call. `SessionState::rules()` returns the live
  rules.
- `trust.rs:256` `boundary(dir)` finds the enclosing git work tree with `gix` (no `git` spawn,
  safe options) and returns a `Boundary`. It is private. Folder trust is already bounded at that
  work-tree root (`trust.rs:289`).
- `floor.rs:14` `PROTECTED_DIRS` lists `.git`, `.vscode`, `.idea`, `.husky` and `.fontaine`;
  `.claude` is absent because Fontaine never read it (`docs/extensions.md`: "`.claude/` itself
  is not read").
- TUI slash commands come from one table (`fontaine-tui/src/commands.rs:63` `BUILTINS`) and are
  dispatched in `fontaine-tui/src/ui/mod.rs:263` `submit`. A command can send a hidden prompt
  through `converse(runtime, text, Some(prompt))`, as skills do. Terminal input is read on a
  thread (`fontaine-tui/src/lib.rs:313` `read_terminal`) gated by a stop flag. Nothing suspends
  the terminal for a child process today.
- `/context` (`context.rs:33`, `fontaine-tui/src/reports.rs:288`) lists each instruction file
  with its token cost.
- Master plan Decision (2026-10-21, user): "`FONTAINE.md` v1 loads `<primary root>/FONTAINE.md`
  plus the user-level file; no hierarchy or imports (backlog GitHub issue #7)". This plan
  delivers that backlog item.

### What Claude Code does (observed and documented)

From https://code.claude.com/docs/en/memory, plus a `claude -p` probe run on 2026-10-26. The
probe used a git repo `a/` holding `CLAUDE.md` and `.claude/rules/{r.md, p.md (paths: b/c/**)}`,
`a/b/{CLAUDE.md, AGENTS.md}` and `a/b/c/CLAUDE.md`, and was launched in `a/b`:

- At launch it loaded `a/CLAUDE.md`, `a/.claude/rules/r.md` (an **ancestor's** unconditional
  rule) and `a/b/CLAUDE.md`. It did **not** load `a/b/AGENTS.md`, because a `CLAUDE.md` exists.
- After `Read c/f.txt` it injected `a/b/c/CLAUDE.md` and `a/.claude/rules/p.md` as reminders
  attached to the tool result.
- Order: filesystem root down to the cwd. Within a directory, `CLAUDE.md` comes before
  `CLAUDE.local.md`. User files come before project files, and user rules before project rules.
- Imports: `@path` outside code spans and fenced blocks. A relative path resolves against the
  importing file, and absolute and `~/` paths are allowed. A backslash escapes a space. The
  maximum depth is four hops. An import from a project file that resolves outside the working
  directory needs a one-time approval dialog; user-scope files import freely.
- `AGENTS.md` (and `.claude/AGENTS.md`) is read in every directory of the walk only when no
  `CLAUDE.md`, `.claude/CLAUDE.md` or `CLAUDE.local.md` exists in the cwd or above. The user
  file, the managed file and rules do not count toward that check. A `CLAUDE.md` that imports or
  symlinks `AGENTS.md` never loads it twice.
- Block-level HTML comments are stripped outside code blocks. Rule frontmatter is removed, and
  only `paths` is read (a YAML list or a comma string). Frontmatter that does not parse means
  no `paths`.
- Rule `paths` globs are relative to the directory containing the rule's `.claude`. User rules'
  globs are relative to the primary working directory.
- `--add-dir` directories contribute no `CLAUDE.md` by default.
- After `/compact`, the root instructions survive, and nested files and path rules reload on
  demand.
- `/memory` lists the memory files, including the user and project `CLAUDE.md` when they do not
  exist yet, and opens the chosen one in `$EDITOR`, creating it first if needed. `/init`
  analyzes the codebase and writes a `CLAUDE.md`, or suggests improvements to an existing one.

## Design

### File kinds and locations

Every Claude Code location gets a Fontaine twin: `<config dir>` ↔ `~/.claude`, `.fontaine/` ↔
`.claude/`, `FONTAINE*.md` ↔ `CLAUDE*.md`. **Within any one location, Claude Code's file comes
first and Fontaine's second**, so the native file is read last at its level.

**User tier** (the user's own files, loaded every session):

1. `~/.claude/CLAUDE.md`
2. `~/.claude/rules/**/*.md`
3. `<config dir>/FONTAINE.md`
4. `<config dir>/rules/**/*.md`

**Directory walk** (launch time), over each directory `D` from the filesystem root down to the
primary root, in that order:

1. `D/CLAUDE.md`, `D/.claude/CLAUDE.md`, `D/FONTAINE.md`, `D/.fontaine/FONTAINE.md`
2. `D/.claude/rules/**/*.md`, then `D/.fontaine/rules/**/*.md` (only the rules without `paths`;
   path-scoped rules are registered for on-demand matching)
3. `D/CLAUDE.local.md`, `D/FONTAINE.local.md`
4. In **AGENTS mode** only: `D/AGENTS.md`, then `D/.claude/AGENTS.md`

**AGENTS mode** is on when none of the six "counting" names exists in the primary root or any
ancestor: `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`, `FONTAINE.md`,
`.fontaine/FONTAINE.md` and `FONTAINE.local.md`. User-tier files and rules do not count. This
mirrors Claude Code's default `claude-md-or-agents-md`, with the `FONTAINE*` names counting like
their `CLAUDE*` twins. A repo whose `CLAUDE.md` says `@AGENTS.md` loads `AGENTS.md` through the
import, exactly once. This repository (`AGENTS.md` only) runs in AGENTS mode.

Only the primary root's hierarchy is walked. Roots added with `--add-dir` contribute nothing, as
in Claude Code's default.

Rule files within one rules directory are ordered by their relative path (byte order), so the
prompt is deterministic. Prefix stability matters to prompt caching.

### Imports

`@path` tokens are parsed by one small markdown scanner, `instructions/markdown.rs`. The same
scanner strips block-level `<!-- … -->` comments. It skips fenced code blocks (```` ``` ```` and
`~~~`) and inline code spans. An import token is an `@` at the start of a line or after
whitespace, followed by a path that runs to the next unescaped whitespace (`\ ` is a space).
`user@example.com` is therefore not an import. Paths may be relative (to the importing file's
directory), absolute, or `~/…`; quoted paths are not imports.

Expansion is depth-first and preorder: each imported file becomes its own `InstructionFile`,
placed right after the file that imported it, and the `@path` text stays in the importer. Depth is
capped at **four hops**, and a cycle stops at a file already loaded (see dedup below).

- An import that names no existing file is skipped silently, because `@mentions` in prose are
  common.
- An import that exists but is refused (depth cap, confinement, deny rule, not UTF-8,
  unreadable) is skipped with a warning naming both files.

### Confinement and the floor

Who controls a file decides how far it may reach:

- **Repo-controlled** files: a file is repo-controlled when its path, as found, lies inside the
  **confinement set**. That set is the canonicalized workspace roots plus the enclosing git work
  tree of the primary root, from a new `pub(crate) fn work_tree_root(dir) -> Option<PathBuf>` in
  `trust.rs`: `match boundary(dir) { Boundary::WorkTree(p) => Some(p), _ => None }`. The work tree bounds folder trust today too, and it
  covers a monorepo launched from a subdirectory. Rules:
  - The file itself must canonicalize inside the confinement set. This covers a checked-in
    `CLAUDE.md -> ~/.ssh/id_rsa` and symlinked rules directories.
  - Its imports must canonicalize inside the confinement set as well.
  - Anything that escapes is skipped with a warning.
- **Directory-walk files above the confinement set** (for example `~/CLAUDE.md`) load as found,
  following symlinks as Claude Code does. Their **imports** are confined like a repo-controlled
  file's, because a writable ancestor such as `/tmp` may hold another local user's file.
- **User-tier** files: they and their imports are unconfined, as in Claude Code (user files are
  the user's own configuration).
- **Every** candidate file (directory-walk files such as `./CLAUDE.md`, `CLAUDE.local.md`,
  `AGENTS.md` and ancestors, imports, on-demand files and rule files), user tier included, is
  skipped with a warning
  when the session's `deny` rules deny `read` of its path. The check calls
  `permission_rules::matches(rule, &CallContext { tool: "read", args: {"path": …}, scope, home },
  Posture::Deny)` for each deny rule. A deny rule written for the read tool therefore also keeps
  that file out of the prompt.

Walking the `rules/` directories follows symlinks and remembers visited canonical directories,
which breaks cycles.

### Dedup

Each load keeps a set of canonical paths and a set of SHA-256 digests of the **effective text**:
the text after frontmatter and comment stripping, trimmed. A candidate already in either set is
skipped silently. That covers:

- `CLAUDE.md` importing `AGENTS.md`, which never loads it twice;
- `CLAUDE.md -> AGENTS.md` symlinks;
- `FONTAINE.md` and `CLAUDE.md` with identical content;
- `~/.claude/CLAUDE.md` met again when the walk passes through `~`.

The first occurrence in load order wins.

Files whose effective text is blank are dropped, as today.

### Path-scoped rules and on-demand files

Rule frontmatter goes through `frontmatter::parse`. `paths` is read through `Document::list`
(`frontmatter.rs:57`), which handles a list or a comma string. Other keys are ignored. A parse error (`parse` returns Err with no body) means no `paths`, and the
whole raw file text, after comment stripping, loads as an unconditional rule, as in Claude Code. Patterns compile with `globset` (new exact-pinned
workspace dependency, `globset = "=0.4.20"`) with `literal_separator(true)`, so
`*.md` matches only at the base and `**` crosses directories. Braces are supported natively, so
no expansion budget is needed. An invalid pattern is dropped with a warning, and the rule's other
patterns keep working. The base directory is the directory holding the rule's
`.claude`/`.fontaine` (user-tier rules: the primary root).

A new `ResultStage`, `InstructionStage` in `instructions/on_demand.rs`, runs after a
**successful** `read`, `write` or `edit` call. It takes the resolved path `P` from
`touched_paths`, then:

1. For each directory strictly below the primary root, down to `P`'s parent, it collects that
   directory's walk files (steps 1–4 above, AGENTS files only in AGENTS mode and only when the
   directory has none of the six counting names) and its unconditional rules. Those directories'
   path-scoped rules are added to the matcher list.
2. It matches `P` against every registered path-scoped rule.
3. It expands imports of everything new (same confinement, deny and dedup rules), drops what the
   agent already has, and appends the rest with `append_note`:

```
[Harness note: instruction files that apply to <P>, loaded from disk.]

Instructions from <path>:

<text>
```

Bookkeeping is **per agent** (`CallView::agent`), in a lock-protected map from agent id to
loaded paths and digests. Each agent's entry is seeded with the launch set, and the main agent's
entry is reset by `/clear` and by compaction so files load again on demand. The stage has a
`reseed(&Loaded)` entry point that `build_agent` calls with the rebuilt `Loaded`: it replaces the
path rules and resets the main agent's seed. The lock feeds no
permission decision, so it uses `lock_recovering`, with a one-line reason.

Its `StageKind::Instructions` sits between `Notifications` and `Scanner`, so the tool-result
scanner sees instruction text that arrives through a tool result, like any other repo content in
a result. Files outside the primary root (other roots) trigger nothing. A shell `cat` does not
count as a read (Claude Code's does), because Fontaine does not parse shell commands into file
reads for side effects.

### What feeds what

| Consumer | Files | When read |
|---|---|---|
| Main agent system prompt | The launch set (user tier, then the directory walk, imports in place) | At every agent build (model switch, output-style change, `/add-dir`, a new session) |
| Subagent preamble | The same launch set | When the subagent starts (as now) |
| Tool results (any agent) | On-demand nested files and matched path rules | After the triggering call |
| Guardian `project_instructions` | The launch set **as it stood at session start**, most specific first (primary-root level, then each ancestor toward `/`, then the user tier), imports kept after their importer, capped at 16,000 characters | Once, in `session_state` |

The guardian never sees on-demand files. Its snapshot is fixed at session start. The
most-specific-first order means the cap drops the broadest files, not the repo's own.

### Types and module layout

`fontaine-core/src/instructions/` replaces `load_instructions` in `prompt.rs`:

- `mod.rs`:
  - `InstructionFile { path, text, origin }`. It moves from `prompt.rs` and is re-exported there
    so existing users compile.
  - `Origin` is one of `User`, `Walk`, `Local`, `Agents`, `Rule`, or `Import { from: PathBuf }`;
    it derives `Clone, Debug, PartialEq, Eq` and is used by `/memory`.
  - `Sources { config_dir, home, primary_root, roots, work_tree, deny, scope }`.
  - `Loaded { files, path_rules, agents_mode, warnings }`.
  - `pub fn load(&Sources) -> Loaded`.
  - `pub fn guardian_order(&[InstructionFile]) -> Vec<InstructionFile>` (owned, as `adjudication::project_instructions` takes `&[InstructionFile]`).
- `markdown.rs`: the comment stripper and the import scanner. Pure functions.
- `imports.rs`: expansion with depth, dedup, confinement and deny checks.
- `rules.rs`: rules-directory walk, frontmatter, `PathRule { base, globs, file }`.
- `on_demand.rs`: `InstructionStage` and the per-agent loaded map.

The three call sites build `Sources` from `AgentSetup` / `SessionState`:

- `build_agent` reports `Loaded::warnings` through `Approver::warn`, each distinct warning once
  per session (shown warnings are remembered, like the `take_warning` pattern), so rebuilds do not
  repeat them.
- `session_state` and the preamble ignore the warnings, because the same files raise the same
  warnings in `build_agent`.
- `setup.rs` attaches `InstructionStage` in `result_pipeline`, with the launch `Loaded`.

The guardian is not modified: `adjudication::project_instructions` takes `guardian_order(...)`.

Size: no hard cap on the agent prompt (Claude Code has none). When the launch set's total text
passes 40,000 characters, the build warns once, naming the largest files.

### Protected paths

`.claude` joins `PROTECTED_DIRS`. Fontaine now reads `.claude/CLAUDE.md` and `.claude/rules/`
into future system prompts, so a model write there is a persistence vector exactly like
`.fontaine/`. Claude Code also protects `.claude`. Root-level `FONTAINE.md`/`CLAUDE.md`/
`AGENTS.md` stay unprotected, as in Claude Code, so `/init` on them needs no protected-path prompt. A
`.claude/CLAUDE.md` target goes through the protected-path floor.

### `/memory`

- `/memory` shows a numbered notice that lists:
  - every file of the launch set (path, origin, import parent);
  - the files loaded on demand so far for the main agent;
  - `<config dir>/FONTAINE.md` and `<primary root>/FONTAINE.md`, marked "not created" when
    absent.
- `/memory <n>` opens entry `n` in the user's editor, creating the file (and its directory)
  first if it does not exist. The editor is chosen as follows:
  - `$VISUAL`, then `$EDITOR`, else `vi` (Unix) or `notepad` (Windows).
  - On Unix it runs as `sh -c '<editor> "$1"' editor <path>`, as git does, so `code --wait`
    works.
  - On Windows the same `sh -c '<editor> "$1"' editor <path>` form runs through Git Bash, as user
    command strings do (`user_hooks.rs`, `fontaine-tools/src/git_bash.rs`), so
    `C:\Program Files\…` paths survive. The editor string is split on whitespace only when Git
    Bash is absent. The command is built with `fontaine_tools::git_bash::sh_command` plus the
    `git_bash.path_value(..)` PATH prepend, as `run_user_shell` (`fontaine-core/src/user_shell.rs`)
    does, and the GitBash comes from the public `AgentSetup.git_bash`.
- `run()` passes a suspend/resume handle (screen plus the `read_terminal` pause flag) into
  `drive()` (`fontaine-tui/src/lib.rs:177-184`, `372-378`), and `/memory` dispatch calls it.
- While the editor runs, the TUI:
  1. pauses `read_terminal` (a new pause flag beside the stop flag);
  2. clears the live area;
  3. leaves raw mode;
  4. waits for the child;
  5. re-enters raw mode and redraws.
- In the linear (screen-reader) interface, the same flow runs without the live-area steps.
- Afterwards a notice says edits apply at the next model switch, output-style change, `/add-dir` or new session. The guardian
  keeps its session-start snapshot.

The editor is the user's own program, started by the user's own command, so it is not gated,
like `!` shell commands.

### `/init`

`/init` sends a hidden prompt, the versioned asset `rust/fontaine-core/prompts/init.md`. The
prompt asks the agent to read the codebase and write, or improve, the project instruction file
at `{{target}}`. It covers build, test and lint commands, layout, and conventions a newcomer
would not discover. It says to keep the file under about 200 lines and not to invent rules.
`{{target}}` is chosen by core, not the model:

- the first existing file among `<root>/FONTAINE.md`, `<root>/CLAUDE.md`,
  `<root>/.claude/CLAUDE.md` and `<root>/AGENTS.md`, which the prompt says to **improve, never
  overwrite**;
- otherwise `<root>/FONTAINE.md`, created new.

The ledger records the `UserMessage` `/init: create or improve <target>`, so the guardian's task
intent covers the write. The write itself goes through the normal permission pipeline. `/init`
takes no argument and is TUI-only, like the other built-ins.

## Implementation Steps

1. **Markdown scanner** — `instructions/markdown.rs`: `strip_comments(text) -> String` and
   `imports(text) -> Vec<String>` (fence- and span-aware, `\ ` escapes, start/whitespace rule).
2. **Rules** — `instructions/rules.rs`:
   - the symlink-following, cycle-safe walk, sorted by relative path;
   - frontmatter `paths` via `Document::list` (a parse error means unconditional);
   - `PathRule` with `globset` (add `globset = "=0.4.20"` to `rust/Cargo.toml`
     `[workspace.dependencies]` and to `fontaine-core/Cargo.toml`; `cargo deny check`).
3. **Imports, confinement, dedup** — `instructions/imports.rs`:
   - expansion (four hops, preorder);
   - confinement by repo-controlled origin;
   - the deny-rule `read` check;
   - canonical-path and digest dedup;
   - warnings.
   Expose `trust::work_tree_root`.
4. **Discovery** — `instructions/mod.rs` `load`: user tier, AGENTS-mode test, the root-down walk
   with per-directory order, blank-drop, 40k-character warning, and `guardian_order`. Move
   `InstructionFile` here and re-export it from `prompt.rs`. Delete `load_instructions`, and move
   its tests in `tests/system_prompt.rs` to `instructions::load`. The `instructions()` fixture
   there (lines 17, 21) builds `InstructionFile { path, text }` literals, which gain an `origin`.
5. **Call sites** — `agent/build.rs` `prompt_layers` and `build_agent` (warnings),
   `agent/setup.rs` `session_state` (guardian order) and the `child_runner` preamble.
   `adjudication::project_instructions` keeps its signature, and its caller passes the
   guardian-ordered slice.
6. **On-demand stage**:
   - add `StageKind::Instructions` to `fontaine-tools/src/result_pipeline.rs`, between
     `Notifications` and `Scanner`;
   - write `instructions/on_demand.rs` `InstructionStage`, attached in `setup.rs`
     `result_pipeline`;
   - add `InstructionStage::reseed`, called by `build_agent` with the rebuilt `Loaded`;
   - keep the reset handle on `Session`, fired inside `Session::compact` (`session.rs:402`) only
     on `CompactResult::Compacted` and inside `Session::clear` (`session.rs:488`), so no caller
     changes.
7. **Protected dir** — add `.claude` to `floor::PROTECTED_DIRS` (array length 6).
8. **`/memory`**:
   - `Command::Memory(String)` and a `BUILTINS` entry (`hint: "[n]"`) in
     `fontaine-tui/src/commands.rs`;
   - a `reports::memory` list renderer;
   - the editor resolution and command builder (pure, unit-tested);
   - the terminal suspend/resume helper in `fontaine-tui/src/lib.rs` or `screen.rs`, with a pause
     flag for `read_terminal`;
   - dispatch in `ui/mod.rs`, using the suspend/resume handle `run()` passes into `drive()`.
   The runtime exposes the launch `Loaded` and the main agent's on-demand list.
9. **`/init`**:
   - `prompts/init.md` and a `prompt::init_prompt(target)` that fills `{{target}}`;
   - `instructions::init_target(root)`;
   - `Command::Init` and its `BUILTINS` entry;
   - dispatch: append the ledger `UserMessage`, then `converse(runtime, "/init", Some(prompt))`.
10. **Master plan** — add the dated Decisions listed under `## Decisions` below to
    `tasks/fontaine_harness_plan.md`. Mark the 2026-10-21 "no hierarchy or imports" line as
    superseded by a new dated line rather than editing it. Update the
    `research/research-claude-code-parity.md` row (`partial` → `done`, and the `backlog`
    priority column, noting the deviations), `tasks/design/permissions.md`,
    `tasks/design/config.md` and `CHANGELOG.md`.
11. **Docs** (see Documentation).

## Files to Modify

- New: `rust/fontaine-core/src/instructions/{mod,markdown,imports,rules,on_demand}.rs`,
  `rust/fontaine-core/prompts/init.md`, `docs/instructions.md`
- `rust/fontaine-core/src/{prompt,adjudication,trust,floor,lib,session}.rs`,
  `rust/fontaine-core/src/agent/{build,setup}.rs`, `rust/fontaine-core/src/agent.rs` (runtime
  accessors)
- `rust/fontaine-tools/src/result_pipeline.rs`
- `rust/fontaine-tui/src/{commands,reports,lib,screen}.rs`, `rust/fontaine-tui/src/ui/mod.rs`
  (and a new `ui/commands/memory.rs`)
- `rust/Cargo.toml`, `rust/fontaine-core/Cargo.toml`
- Tests: `rust/fontaine-core/tests/system_prompt.rs` (+ snapshot),
  `rust/fontaine/tests/{headless_tools,auto_headless}.rs`, `rust/fontaine-core/tests/docs.rs`,
  `rust/fontaine-tui/tests/tui.rs`, `rust/fontaine-tui/tests/common/mod.rs` (calls `drive` at
  :428 and :464; pass a no-op suspend handle)
- Docs: `docs/{index,guardian,tui,extensions,configuration,permissions}.md`
- Plans: `tasks/fontaine_harness_plan.md`, `tasks/design/{permissions,config}.md`,
  `research/research-claude-code-parity.md`, `CHANGELOG.md`

## Trade-offs

- **AGENTS.md: "or" (Claude Code's default) vs always loading all three names.** Loading
  everything and deduping only identical text would double up a repo that keeps a Claude-specific
  `CLAUDE.md` beside a generic `AGENTS.md` with paraphrased overlap. The "or" rule matches what
  Claude Code users already see, and the `@AGENTS.md` import covers the "both" case explicitly.
- **System prompt vs a first-message reminder for launch files.** Claude Code puts them in a
  reminder on the first user message. Fontaine keeps them in the system prompt (existing
  2026-10-21 layering decision), which caches well and survives compaction without re-injection.
- **External imports: approval dialog vs skip-and-warn.** A dialog needs a persisted per-project
  approval, a headless story and a TUI surface. Skipping is strictly tighter, and the work-tree
  bound already covers the common monorepo case.
- **Guardian sees on-demand files?** No. It would make the guardian's input depend on which files
  the model chose to read, letting repo text steer the guardian mid-session, and it would churn
  the guardian's cached prefix.
- **`/memory` picker vs numbered list.** A list plus `/memory <n>` reuses the `/copy [n]` pattern,
  works in the linear interface, and needs no new overlay.
- **`gitignore` matcher (already used by permission rules) vs `globset` for rule `paths`.**
  Gitignore semantics make `*.md` match at any depth; Claude Code's documented `*.md` means the
  base only. `globset` with a literal separator matches the documentation.

## Decisions

- 2026-10-26: Claude Code walks ancestor `.claude/rules/` at launch and injects nested
  `CLAUDE.md` plus matching path rules into the tool result after a Read (observed with
  `claude -p`); the design copies both.
- 2026-10-26: External imports from repo-controlled files are skipped with a warning instead of
  approved; an approval dialog needs persisted per-project state and a headless story.
- 2026-10-26: A repo-controlled file that is a symlink resolving outside the confinement set is
  refused, where Claude Code follows symlinks; a checked-in `CLAUDE.md -> ~/.ssh/id_rsa` must not
  reach the prompt.
- 2026-10-26: Imports from directory-walk files above the confinement set (for example
  `~/CLAUDE.md`) are confined like repo-controlled ones, unlike Claude Code; those files can be
  planted by anything writing above the repo.
- 2026-10-26: Files denied by `read` deny rules are kept out of the prompt, user tier included;
  Claude Code does not apply deny rules to instruction files. The deterministic floor gates every
  source.
- 2026-10-26: `/memory` is a numbered list with `/memory <n>`, not an interactive picker; it
  reuses the `/copy [n]` pattern and works in the linear interface.
- 2026-10-26: Shell reads (`cat`) do not trigger on-demand loading; Fontaine does not parse shell
  commands into file reads.
- 2026-10-26: The `FONTAINE*` names count toward AGENTS mode like their `CLAUDE*` twins, so a
  repo with native files does not also load `AGENTS.md`.
- 2026-10-26: The guardian's `project_instructions` is a snapshot taken at session start, so
  mid-session edits and on-demand files cannot steer it.
- 2026-10-26: `.claude` joins the protected directories because Fontaine now reads it into
  system prompts, matching Claude Code.

## Documentation

- New `docs/instructions.md`: the locations table with load order, AGENTS mode, imports (syntax,
  depth, confinement), rules and `paths`, on-demand loading, dedup, comment stripping, what the
  guardian reads, the 40k warning, `/memory`, `/init`. It cites Anthropic's memory documentation
  as the semantics source (attribution note, as for other adapted prose).
- `docs/guardian.md:46`: the `project_instructions` row describes the launch-set snapshot, its
  order and its cap, and links to `instructions.md`.
- `docs/tui.md`: `/memory`, `/init`; `/context` lists imported and rule files.
- `docs/extensions.md`: "`.claude/` itself is not read" becomes "skills, agent files and output
  styles are not read from `.claude/`; instruction files are (see Instructions)".
- `docs/permissions.md`: `.claude` in the protected directories; deny rules on `read` also keep
  files out of the instructions.
- `docs/configuration.md:794`: the trust paragraph names the new instruction locations.
- `tasks/design/permissions.md` (protected directories list, ~lines 120-123) gains `.claude`, and its parenthetical "(`.fontaine` in place of `.claude`)" is removed or reworded;
  `tasks/design/config.md:39` (prompt content) names the instruction hierarchy.
- `CHANGELOG.md`: an Unreleased entry.
- `docs/index.md` links the new page and adds `/memory` and `/init` to its TUI slash-command list; `rust/fontaine-core/tests/docs.rs` lists it.

## Testing Strategy

All unit tests use `tempfile` directories. No live APIs.

- `markdown.rs`:
  - imports found at the line start and after whitespace;
  - none in fences (both styles), in code spans, in emails, or in quoted paths;
  - `\ ` escapes;
  - comment stripping of block comments, keeping comments inside fences;
  - an unterminated fence.
- `rules.rs`:
  - list and comma `paths`;
  - a parse error loads the rule unconditionally;
  - an invalid glob is dropped while the others keep working;
  - `*.md` matches only at the base and `**` matches at depth, braces included;
  - the base dir for project rules vs user rules;
  - sorted order;
  - a symlink cycle in a rules dir terminates.
- `imports.rs`:
  - relative to the importing file, `~/`, absolute;
  - the four-hop cap (the fifth is refused with a warning);
  - a cycle loads each file once;
  - a missing import is silent;
  - a repo file importing outside the confinement set is refused with a warning;
  - a symlinked repo `CLAUDE.md` pointing outside is refused;
  - an ancestor-above-repo file loads, but its outside import is refused;
  - a user-tier import outside is allowed;
  - a deny rule `read(secrets/**)` refuses an import of `secrets/x.md`;
  - a non-UTF-8 file gives a warning.
- `mod.rs` `load`:
  - the full order across the user tier, ancestors and the root, with Claude-before-Fontaine
    within a location;
  - AGENTS mode on and off, with each of the six counting names toggling it;
  - a deny rule keeps a root `CLAUDE.local.md` out;
  - `CLAUDE.md` with `@AGENTS.md` loads `AGENTS.md` once;
  - identical `CLAUDE.md`/`FONTAINE.md` load once;
  - `~/.claude/CLAUDE.md` is met once when the walk crosses home;
  - `--add-dir` roots are not walked;
  - blank files are dropped;
  - the 40k warning;
  - `guardian_order` puts the most specific first, and the 16k cap keeps the root file.
- `on_demand.rs`:
  - a read under `sub/` injects `sub/CLAUDE.md` once and not again;
  - a matching path rule is injected, and a non-matching one is not;
  - a failed call injects nothing;
  - a path outside the primary root injects nothing;
  - two agents each get their own first load;
  - a reset after clear loads again;
  - AGENTS-mode nested `AGENTS.md` is injected only when the directory has no counting file;
  - stage order (Instructions before Scanner) is asserted through `ResultPipeline` with
    recording stages.
- Floor: `.claude/settings.json` and `.claude/rules/x.md` writes hit `protected_path`.
- TUI:
  - `/memory` and `/init` parse;
  - the menu lists them;
  - the `reports::memory` rendering includes "not created" entries;
  - editor resolution covers `$VISUAL` over `$EDITOR`, the fallback, the `sh -c` argv shape, and
    the Windows whitespace-split fallback;
  - `init_target` picks an existing file over creating `FONTAINE.md`;
  - `fontaine-tui/tests/tui.rs`: `/memory` lists entries including "not created", and `/init`
    dispatches a hidden prompt carrying the target while the ledger records
    `/init: create or improve <target>`.
- Integration (`fontaine -p` with a wiremock endpoint; `headless_tools.rs`, and the guardian
  snapshot case in `auto_headless.rs`, which has the two-endpoint guardian fixture):
  - the system prompt carries markers from an ancestor `CLAUDE.md`, a root `AGENTS.md` (AGENTS
    mode), an `@import`ed file and a `.claude/rules` file;
  - a scripted `read` of `sub/f.txt` produces a second request whose tool result carries the
    nested file's marker;
  - the guardian request (fixture guardian) contains the launch-set markers and not the on-demand
    one.
  This is the only tier that proves the stage is actually wired into the live pipeline and that
  the guardian snapshot excludes on-demand text, which is a silent security property.
- `system_prompt.rs` snapshot updated for the new loader. The tests at `system_prompt.rs:142-160`
  that call `prompt::load_instructions` move to `instructions::load`, and the one asserting
  nested/parent `FONTAINE.md` is "never read" is inverted or removed.

## Manual Verification

The editor handoff needs a real terminal (raw mode, the live area, a child taking the TTY), which
no test harness here drives.

1. `EDITOR=nano` then `/memory 1`. Expected: nano opens the file. Saving and quitting returns to
   an intact TUI with the transcript redrawn, and a notice says when edits apply.
2. A second `/init` in a scratch repo improves the existing file rather than replacing it.

## Open Questions

None.
