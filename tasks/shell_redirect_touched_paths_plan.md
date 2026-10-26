# Shell Redirect Targets in Touched Paths Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/230

## Overview
`touched_paths` builds the guardian's `paths_this_call_touches` from the argument words of each
simple command. The targets of output redirects (`>`, `>>`, `>|`, `&>`, `&>>`, `<>`, `>&file`)
are never listed, while a non-path argument that happens to contain `/`, `\` or a leading `.`
(`'# generated\n'`) is. The fact handed to the guardian is therefore wrong. Fix: record redirect targets in the shell
analysis and list them first in `touched_paths`.

## Current State
- `rust/fontaine-tools/src/preclassify.rs`: `Walk::redirect` (~l.206) sets
  `ShellAnalysis::output_redirect` for write-kind redirects but discards the target word after
  `self.word(word)`. Redirects can sit on a simple command (`item`) or on a compound command
  (`{ echo x; } > f`), so the target is not always attached to a `SimpleCommand`. Nested
  runner command lines (`sh -c 'echo x > f'`) are merged in `Walk::nested`, which ORs
  `output_redirect` and extends `commands`; the command-substitution handler in `Walk::expanding`
  (~l.395-402) merges nested analyses the same way.
- `rust/fontaine-core/src/destructive.rs` `touched_paths` ("shell" arm, ~l.296): iterates
  `analyze(command).commands`, words after the first; literal words that look like paths or exist
  are resolved against cwd/scope. Capped at 16, deduped.
- Consumers: `adjudication.rs:74` (guardian facts), `permissions.rs:959` (feeds only the
  audit-ledger paths, not a verdict input).
- The shell destructive check already treats any `output_redirect` as destructive (`destructive.rs:93`),
  so no verdict changes; only the reported fact does.

## Design
- `ShellAnalysis` gains `pub output_targets: Vec<ShellWord>`: the target word of every redirect
  that writes a file, in source order, including targets inside compound commands, substitutions
  and nested runner command lines.
  - `Walk::redirect` pushes the classified word in the same cases that set `output_redirect`:
    `Filename` with Write/Append/ReadAndWrite/Clobber kinds, `OutputAndError` (`&>`, `&>>`), and
    `Duplicate` with `DuplicateOutput` when the word is not a descriptor copy (`>&file`).
    Descriptor copies (`2>&1`, `>&2`, `>&-`) push nothing.
  - `Walk::nested` and the command-substitution handler in `Walk::expanding` both extend
    `output_targets` from the nested analysis, like they do `commands`.
  - The push applies no `-` or path-like filter (`> -x` is a target) but keeps the empty and
    expanding-literal skips; process-substitution targets push nothing.
  - Targets whose word expands (`literal == None`) are kept; `touched_paths` skips them, like it
    skips expanding argument words. Soundness is unchanged: `output_redirect` still sends the call
    to the guardian, which also sees the raw command.
- `touched_paths` ("shell" arm): list redirect targets first, then command argument words. The
  resolve step (absolute vs. cwd-relative, `scope.resolved`) is factored into a local closure
  shared by both. Redirect targets are listed unconditionally (no path-like/exists filter): a
  redirect target is a file by construction, including a not-yet-existing bare name like `cfg.py`.

## Implementation Steps
1. `preclassify.rs`: add `output_targets` to `ShellAnalysis` (doc comment), populate in
   `Walk::redirect`, merge in `Walk::nested` and in the command-substitution handler in
   `Walk::expanding`.
2. `destructive.rs`: in the shell arm of `touched_paths`, resolve and push `output_targets`
   literals before the argument-word loop.
3. Tests (below).
4. Docs: update `docs/guardian.md` (the `paths_this_call_touches` session_facts row ~l.49, and
   ~l.277 if needed) to say that for a shell command, it lists its redirect targets and
   path-like arguments.
5. `CHANGELOG.md` entry under Unreleased/Fixed, ending in `(#230)`.

## Files to Modify
- `rust/fontaine-tools/src/preclassify.rs`
- `rust/fontaine-core/src/destructive.rs`
- `rust/fontaine-core/tests/destructive.rs`
- `CHANGELOG.md`
- `docs/guardian.md`

## Trade-offs
- Per-`SimpleCommand` targets vs. a global list: compound-command redirects have no simple
  command to attach to, so a global list is the only complete option.
- Redirect targets are listed first as presentation order only.

## Decisions
- No change to `output_redirect` semantics or to any permission verdict.

## Documentation
Update `docs/guardian.md` (`paths_this_call_touches` row, ~l.49; ~l.277 if needed): for a shell
command, it lists its redirect targets and path-like arguments.

## Testing Strategy
Unit tests, no live APIs:
- `preclassify.rs`: extend the existing test `only_a_redirect_to_a_file_sets_output_redirect`
  (~l.949) operator lists with `output_targets` assertions (target's literal for `>`, `>>`, `>|`,
  `<>`, `&>`, `&>>`, `>&file`); assert `2>&1`, `>&2`, `1>&-`, `< in`, `<<< w`
  yield none; assert targets are found in compound (`{ echo x; } > f`), pipeline, substitution
  and `sh -c 'echo x > f'`; an expanding target (`> $F`) is present with `literal == None`.
- `fontaine-core/tests/destructive.rs`: `touched_paths("shell", {"command": "printf '# generated\\n' >> cfg.py"})`
  lists `<root>/cfg.py` first (pins the reported bug), a `cwd`-relative target resolves against
  cwd, a target that is also an argument is deduped, and an expanding target is skipped.

## Open Questions
None.
