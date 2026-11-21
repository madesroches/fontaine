# Shell Workspace Edits Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/743

## Overview

In `auto`, editing a tracked file with the `edit` tool skips the guardian (`tracked_edit`). Making
the same edit with `sed -i`, then running `git add` and `git rebase --continue`, still goes to the
LLM guardian. The guardian keeps denying such calls for reasons about scope, not harm: "not the
requested step", "cannot verify the resolution", "did not name these files". Since #256 this has
come back as #418, #636, #665, #666 and #743, and each was patched by rewording the guardian prompt.
This plan stops asking the guardian about that class of call. A new deterministic tier, `shell_edit`,
is the shell counterpart of `tracked_edit`. It allows a shell call that Fontaine can fully parse, as
long as every command in it is either a read-only command already in the pre-classifier's curated
set, or an in-root edit or local git step that can be undone from git. Anything the parser cannot
fully understand still goes to the guardian, as it does today.

## Current State

- **Pipeline order.** `PermissionPipeline` in `rust/fontaine-sdk/src/core/permissions.rs` runs the
  floor (deny, then ask, then protected path), the hooks, the read-only fast path, then Tier 0.5
  (`Preclassifier::read_paths`, about line 1379). After those come the allow rules, and last the
  mode. For `Auto | Afk` the mode stage calls `unjudged_edit` (line 1578). It returns
  `Tier::TrackedEdit` or `Tier::ScratchWrite` for `edit`/`write`/`notebook_edit` only (lines
  1630-1680); every other call returns `Verdict::Guardian`.
- **`tracked_edit` qualification** (`unjudged_edit`):
  - the target is in the primary root;
  - it is not under the memory root;
  - it is not an instruction file or CI path (`floor::is_instruction_or_ci_path`);
  - `trust::tracking` (`core/trust.rs:376`) answers `Tracked`, or the target is absent, or the
    ledger says it was created this session;
  - `Tracking::Unknown` (no work tree) answers `None`.
- **The allow side effect.** `allow()` (line 1883) resets the guardian loop breaker for
  `TrackedEdit | ScratchWrite`.
- **Tier 0.5** (`rust/fontaine-sdk/src/tools/preclassify.rs`):
  - It accepts exactly one simple command (`analysis.simple`) from the curated `SPECS` table, a
    read-only `git` subcommand from `GIT_SPECS` (`parse_git`), or a user `allow_extra` entry.
  - Every path operand must resolve inside a root (`Environment::in_scope`).
  - Chains such as `a && b` never qualify.
  - The `ShellAnalysis` walker (lines 150-300) flattens a whole program, recording `commands`,
    `output_targets` and an analysis-wide `assignments` flag.
- **The decision this plan supersedes.** Harness plan, Permissions decisions, 2026-10-21:
  "`acceptEdits` auto-approves Fontaine's edit-class tools only, not in-scope filesystem Bash ... a
  mutating shell must not fast-path". Claude Code auto-approves in-scope filesystem Bash in
  `acceptEdits`.
- **Config.** `[permissions] auto_tracked_edits` / `auto_scratch_writes`:
  - fields: `core/config.rs:640` (raw) and `:2602` (resolved);
  - catalog: `core/config_catalog.rs:475`;
  - project scope may only set them `false`: `core/config_scope.rs:213`;
  - wired into the pipeline at `core/agent/setup.rs:560` (`AutoEdits { tracked, scratch }`).
- **Tests.** `rust/fontaine-sdk/tests/permissions_fast_edits.rs` drives the pipeline in a real git
  repository (`repo_rig`) with a fake guardian whose call count is asserted.
- **Prior art.** `tasks/completed/afk_in_workspace_shell_edits_plan.md` (#636) and
  `tasks/completed/guardian_workspace_scope_plan.md` (#666) both changed only guardian prompt text.

## Design

### Where the tier sits

`unjudged_edit` gains a `shell` branch, so the tier runs where `tracked_edit` runs: in
`auto`/`afk` only, at the mode stage. By then the deny and ask rules, the protected-path floor, the
hooks, Tier 0.5 and the allow rules have all had their say, so nothing in this tier reaches past
the floor. The branch runs only when:

- `tool == "shell"` (not `powershell` or `monitor`);
- the new `[permissions] auto_shell_edits` switch is on;
- the call's working directory resolves inside the primary root (root index 0).

If the branch accepts, it returns the new `Tier::ShellEdit` (`"shell_edit"`). `allow()` treats
`ShellEdit` like `TrackedEdit`: it resets the loop breaker.

### The classifier: `rust/fontaine-sdk/src/tools/shell_edit.rs`

A new module, `classify(command, &Environment, &dyn TargetCheck) -> bool`. It parses the command
with `brush-parser` and walks the AST itself. It does **not** use the lossy `ShellAnalysis`
summary, so every construct it does not recognise rejects the whole call.

**Accepted structure.** A program is a list of items joined by `;` or newlines (never `&`). Each
item is an and-or list joined by `&&` or `||`. Each pipeline in it:

- may start with `!`, but may not use `time`;
- is a single simple command, or a pipeline whose every stage is a read-only command.

**Rejected anywhere.** Any of these sends the whole call to the guardian:

- compound commands, function definitions and extended tests;
- background items;
- a word that is not literal (parameter or command substitution, unquoted glob, brace expansion,
  tilde);
- a heredoc that needs expansion;
- process substitution;
- an input redirect from a file;
- a `cd` or `pushd`.

**Prefix assignments.** Only `GIT_EDITOR` and `GIT_SEQUENCE_EDITOR` are allowed, with the literal
value `true` or `:`, and only on a `git` command. Any other assignment rejects.

**Output redirects** (`>`, `>>`, `>|`, `&>`) must target a literal path that passes the write-target
check below. `/dev/null` and descriptor copies are fine.

**Simple commands.** Each simple command must match one of these:

1. **Read-only.** The argv passes the existing curated check. `Preclassifier` gets a
   `pub(crate) fn read_only_argv(&self, argv, env) -> Option<Vec<String>>`, and `read_paths`
   delegates to it, so the two cannot drift. Every read path must also pass `floor::secret_path`:
   a credential read never rides this tier. Read-only `git` subcommands match through `parse_git`'s
   spec table. As in Tier 0.5, they are not run through gix here: they are part of a shell call the
   real shell runs.
2. **`sed -i`.**
   - Flags: `-i` / `--in-place` with no backup suffix, `-E` / `-r`, `-n`, `-s`, `--posix`,
     `-e SCRIPT` / `--expression=SCRIPT` any number of times, `--`.
   - When no `-e` is given, the first positional is the script.
   - Operands: at least one file, and every file passes the edit-target check.
   - `-f`, `--follow-symlinks`, `-z` and anything else reject.
   - Every script must parse under the restricted grammar below.
3. **`mkdir [-p] DIR...`.** Every directory resolves inside the primary root and is not under
   `.git`.
4. **`mv` / `cp`.** `mv [-n] SRC... DST` and `cp [-n] [-r|-R] SRC... DST`, with `--` allowed.
   - Sources must be in the primary root and pass `floor::secret_path`. A `mv` source also passes
     the edit-target check, because moving a file deletes it from its old place.
   - The destination passes the write-target check. When it is an existing directory, the check
     applies to each `DST/basename(SRC)`.
5. **`git add`.** Flags: `-A`, `--all`, `-u`, `--update`, `-v`, `--`. Every pathspec is a literal
   path inside the primary root (`.` is allowed) and not under `.git`.
6. **`git rm`.** Flags: `--cached`, `-r`, `-q`, `--`. Never `-f` / `--force`. Paths are literal and
   in the primary root, and without `--cached` each must pass the instruction/CI exclusion. Without
   `-f`, git itself refuses to remove a file whose content differs from HEAD or the index, so
   anything removed can be recovered from HEAD.
7. **`git mv [-v] SRC DST`.** Never `-f` / `-k`. Both paths are in the primary root and pass the
   instruction/CI exclusion. Git refuses to overwrite an existing destination.
8. **`git commit`.** Flags: `-m MSG` / `--message=MSG` (repeatable), `-F FILE` (an in-root path that
   passes `secret_path`), `-a` / `--all`, `-q`, `--no-edit`. Never `--amend`, `--no-verify` / `-n`,
   `--fixup`, `--squash`, `-c` / `-C`, or a pathspec.
9. **`git <op> --continue`.** `<op>` is `rebase`, `merge`, `cherry-pick` or `revert`, followed by
   exactly that one flag. `--abort` and `--skip` stay with the guardian: both throw work away.

A `git` invocation with any global option between `git` and the subcommand (`-C`, `-c`,
`--git-dir`, `--work-tree`, ...) rejects.

**Target checks.** A `TargetCheck` trait is implemented by the pipeline, which owns the ledger,
scope and memory root. `tools` must not import `core`. It has two methods:

- **`edit_target(path)`** is the `tracked_edit` rule, factored out of `unjudged_edit` into a shared
  helper so both tools use the same rule. The resolved path must:
  - be in the primary root;
  - not be under the memory root or the scratch directory;
  - not be an instruction file or CI path;
  - not be protected (`floor::protected_path`);
  - be `Tracked`, or created by this session.

  It must also exist: `sed -i` on an absent file is an error anyway.
- **`write_target(path)`** applies the same rule, but also accepts an absent path whose parent
  resolves inside the primary root.

`Tracking::Unknown` (no repository) fails both checks.

**The restricted `sed` grammar.** It is a small hand-written parser, not a regex. A script is a
sequence of commands separated by `;` or newlines. Each command is an optional address
(`N`, `$`, or `/RE/` with escapes honoured), optionally followed by `,` and a second address, then
one of:

- `d`;
- `s<delim>RE<delim>REPL<delim>FLAGS`, where the delimiter is any single character except a
  newline or backslash, escapes are honoured, and `FLAGS` may contain only `g`, `i`, `I` and
  digits.

Everything else rejects. That includes `e`, `w`, `W`, `r`, `R`, `{}` blocks, the `s///e`,
`s///w FILE`, `s///m` and `s///p` flags, `y`, `a`/`i`/`c` text commands, and labels and branches.
GNU `e` runs a shell command and `w` writes an arbitrary file, so accepting either would break
containment.

### Config

`[permissions] auto_shell_edits`, default `true`. Project scope may only set it `false`, through
the same rule as `auto_tracked_edits` in `config_scope.rs`. It is a separate key so a user can keep
file-tool edits fast while sending shell edits back to the guardian. `AutoEdits` gains a `shell`
field.

### Accepted risk: repository-controlled code

`git commit` and `git rebase --continue` run hooks, and git config can name filters or an
fsmonitor. The guardian never inspected those either. `.git/` is a protected path, so the agent
cannot plant them through this tier. The tier therefore adds no new exposure, and this is recorded
as an accepted risk rather than mitigated.

### What this closes and what it leaves

- **Closed:**
  - #743's exact call, when the conflicted file is tracked;
  - `git add <f> && git commit -m ...` (#418);
  - `sed -i`, redirect and heredoc edits of tracked or new files (#636);
  - a plain `git rm <tracked>` or `git mv` restructuring with in-root doc rewrites (#665's second
    case without `-f`).
- **Left with the guardian, deliberately:**
  - opaque scripts (#665's Python run);
  - `git rm -f`, `rm`, and other deletions of untracked or modified files;
  - pushes, `--abort` / `--skip`, `--amend`;
  - anything touching instruction files or `.github/`.

## Implementation Steps

1. **Tier and config.**
   - Add `Tier::ShellEdit` (`"shell_edit"`, doc comment: `auto` only, a fully parsed in-root shell
     edit after the floor) in `core/tier.rs`, and extend `Tier::ALL`.
   - Add `auto_shell_edits` to `core/config.rs` (raw and resolved, default true), the catalog row
     in `config_catalog.rs`, and the project-scope `false`-only rule in `config_scope.rs` with its
     test.
   - Wire `AutoEdits.shell` in `core/agent/setup.rs`.
2. **Shared target rule.** In `core/permissions.rs`, factor the qualification part of
   `unjudged_edit` (memory-root, instruction/CI and protected checks, tracked/absent/created) into
   a helper used by the file-tool branch and by a `TargetCheck` implementation on the pipeline. The
   file-tool behaviour must not change; the existing `permissions_fast_edits.rs` tests guard that.
3. **Pre-classifier hook.** In `tools/preclassify.rs`, split `read_paths` into
   `read_only_argv(argv, env)` plus the existing single-command wrapper, and expose `parse_git`'s
   per-argv form to the crate.
4. **Classifier.** Write `tools/shell_edit.rs` with the strict AST walk, the per-verb argument
   parsers, the `sed` script parser and the `TargetCheck` trait, and register the module in
   `tools.rs`. Keep the verb tables as `const` data, like `SPECS` / `GIT_SPECS`.
5. **Pipeline branch.** In `unjudged_edit`, return `Tier::ShellEdit` for a `shell` call that the
   classifier accepts under `auto_shell_edits`, with its working directory in the primary root. In
   `allow()`, add `Tier::ShellEdit` to the loop-breaker reset.
6. **Tests** (see Testing Strategy).
7. **Records and docs** (see Documentation), including the AGENTS.md invariant and the harness
   plan's Decisions.

## Files to Modify

- `rust/fontaine-sdk/src/core/tier.rs`
- `rust/fontaine-sdk/src/core/config.rs`, `config_catalog.rs`, `config_scope.rs`
- `rust/fontaine-sdk/src/core/agent/setup.rs`
- `rust/fontaine-sdk/src/core/permissions.rs`
- `rust/fontaine-sdk/src/tools/preclassify.rs`
- `rust/fontaine-sdk/src/tools/shell_edit.rs` (new), `rust/fontaine-sdk/src/tools.rs`
- `rust/fontaine-sdk/tests/permissions_shell_edits.rs` (new)
- `AGENTS.md`, `tasks/fontaine_harness_plan.md`, `tasks/design/permissions.md`,
  `tasks/design/testing.md`, `tasks/design/config.md`
- `docs/permissions.md`, `docs/guardian.md`, `docs/configuration.md`, `docs/afk.md`

## Trade-offs

- **Another guardian prompt fix instead of a tier.** This has been tried five times (#256, #418,
  #636, #666 and the prompt half of #665). Haiku keeps finding new scope-based reasons. Prompt
  changes also need a live eval on every model and cannot be pinned by a deterministic test.
  Rejected.
- **Splitting a compound call and judging each part separately.** The guardian would still judge
  `sed -i` alone, and splitting changes what the model sees without removing the false denies. The
  tier instead accepts the compound only when every part qualifies; otherwise the guardian sees the
  whole call, as today.
- **Setting `GIT_EDITOR=true` in every shell's environment** instead of accepting the prefix
  assignment. That would be a new deviation (Claude Code does not set it) and would override a
  user's editor in user-run shells. Accepting the two literal assignments is narrower.
- **Including `rm` of unmodified tracked files.** Checking "unmodified" needs a status diff per path
  (gix `status`). `git rm` without `-f` gets the same safety from git itself, so `rm` stays with the
  guardian.
- **Extending the tier to `acceptEdits`** (Claude Code parity). Left out to keep this change to the
  mode where the false denies happen. `acceptEdits` prompts a human, who can answer, so the
  2026-10-21 decision stays in force for it.
- **Reusing the `auto_tracked_edits` switch.** Rejected so a user can turn shell edits off on their
  own.
- **Guardian-side changes** (a `git_state` session fact, a verdict field naming a permitted
  alternative). Useful for calls that still fall through, but they change the guardian prompt and
  need a live eval. Deferred to a follow-up issue so this change stays deterministic.

## Decisions

- 2026-11-21: AFK: the tier covers `auto`/`afk` only; `acceptEdits` keeps prompting for mutating shell (conservative; the human there can answer).
- 2026-11-21: AFK: guardian-side `git_state` fact and structured deny alternatives are a follow-up issue, not part of this change, so no guardian prompt or live eval is in scope.

## Documentation

- **`docs/permissions.md`**: the tier list gains `shell_edit`: what qualifies, that it runs after
  the floor in `auto`/`afk`, and how to switch it off.
- **`docs/guardian.md`**: in the list of what never reaches the guardian, parsed in-root shell
  edits and the local git steps.
- **`docs/configuration.md`**: the `auto_shell_edits` row in the `[permissions]` table and in the
  scope table (line 134).
- **`docs/afk.md`**: one line saying conflict resolution and commit steps no longer stop an
  unattended run.
- **`AGENTS.md`**: the `tracked_edit` exception in the fast-path invariant grows to "file-tool and
  fully parsed shell edits".
- **`tasks/fontaine_harness_plan.md`**: a Permissions decision that supersedes the 2026-10-21
  `acceptEdits` line for `auto`/`afk` and names the accepted repository-hook risk.
- **`tasks/design/permissions.md`**: the pipeline step list.
- **`tasks/design/testing.md`**: the golden adversarial set gains the shell-edit cases.
- **`tasks/design/config.md`**: the new key.

## Testing Strategy

**Unit tests in `tools/shell_edit.rs`**, with a stub `TargetCheck`:

- **Accepted:**
  - each verb in its allowed forms;
  - `!` before a read-only grep;
  - `&&` / `||` / `;` chains;
  - a heredoc `cat > f <<'EOF'`;
  - `GIT_EDITOR=true git rebase --continue`;
  - multiple `-e` scripts, `s` with an alternate delimiter and escaped delimiters, and `/RE/d`
    addresses.
- **Rejected** (the golden adversarial set):
  - sed `e`, `w`, `r`, `s///e`, `s///w f` and `{}` blocks;
  - sed with `-f` or `--follow-symlinks`;
  - a redirect outside the root or to an expanding target;
  - `cd .. && sed -i ...`;
  - `FOO=1 git commit`, and `GIT_EDITOR=vim`;
  - `$(...)` and backtick words;
  - a trailing `&`;
  - `sed -i ... | sh`;
  - `git -c core.hooksPath=x commit`;
  - `git commit --amend`, `--no-verify`;
  - `git rm -f`, `git mv -f`;
  - `git rebase --abort`, `--skip`;
  - `git push`;
  - `rm f`;
  - `mv` of a secret file;
  - `for`, `if` and subshells;
  - input redirect `< /etc/passwd`;
  - an unparseable command;
  - a `sed -i` on an instruction file.

**Unit test in `preclassify.rs`**: `read_paths` and `read_only_argv` agree on the existing cases.

**Integration tests in `tests/permissions_shell_edits.rs`** (pattern of
`permissions_fast_edits.rs`: a real git repository, fake guardian, assert tier and guardian call
count):

- **#743 regression.** Build a repository with a `CHANGELOG.md` conflict mid-rebase (two branches
  editing the same line, `git rebase` stopped). Then check the issue's exact command:

  ```
  sed -i -e '/^<<<<<<< HEAD$/d' -e '/^=======$/d' -e '/^>>>>>>> .../d' CHANGELOG.md && ! grep -n '^<<<<<<<\|^>>>>>>>' CHANGELOG.md && git add CHANGELOG.md && GIT_EDITOR=true git rebase --continue
  ```

  It is allowed at `shell_edit` with zero guardian calls.
- `git add f && git commit -m msg` on a tracked file → `shell_edit`.
- `sed -i` on an untracked file that predates the session → guardian.
- `sed -i` on `AGENTS.md` → guardian.
- A redirect into `.git/config` → protected floor or guardian, never `shell_edit`.
- `auto_shell_edits = false` → guardian.
- `manual` and `acceptEdits` modes → unchanged (prompt).
- A deny rule `shell(git commit)` → denied at `deny_rule` before the tier.
- A `shell_edit` allow resets the loop breaker like `tracked_edit`.

**Config tests**: project scope `auto_shell_edits = true` is rejected, and `false` is accepted.

**Choke-point and tier-list tests** stay green with the new tier (`Tier::ALL`, and the audit and
telemetry serialisation round trip).

No live-API test is needed: the tier is deterministic and the guardian prompt is unchanged.

## Manual Verification

1. In a scratch clone, run `cargo run -p fontaine` in `auto`. Ask for a rebase that conflicts, and
   let the agent resolve it with `sed`, `git add` and `git rebase --continue`. Expected: no
   guardian verdict for that call; `/audit` (or the audit log) shows tier `shell_edit`. This is not
   automated because it exercises the model's own choice of command, which no deterministic test
   controls.
