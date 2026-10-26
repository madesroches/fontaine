# Shell Redirect Targets in Touched Paths Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/230

## Overview
`touched_paths` builds the guardian's `paths_this_call_touches` from the argument words of each
simple command. The targets of output redirects (`>`, `>>`, `>|`, `&>`, `&>>`, `<>`, `>&file`)
are never listed, while a non-path argument that happens to contain `/`, `\` or a leading `.`
(`'# generated\n'`) is. The fact handed to the guardian is therefore wrong, and the same list
chooses the target for on-demand nested instructions. Fix: record redirect targets in the shell
analysis and list them first in `touched_paths`.

## Current State
- `rust/fontaine-tools/src/preclassify.rs`: `Walk::redirect` (~l.206) sets
  `ShellAnalysis::output_redirect` for write-kind redirects but discards the target word after
  `self.word(word)`. Redirects can sit on a simple command (`item`) or on a compound command
  (`{ echo x; } > f`), so the target is not always attached to a `SimpleCommand`. Nested
  runner command lines (`sh -c 'echo x > f'`) are merged in `Walk::nested`, which ORs
  `output_redirect` and extends `commands`.
- `rust/fontaine-core/src/destructive.rs` `touched_paths` ("shell" arm, ~l.296): iterates
  `analyze(command).commands`, words after the first; literal words that look like paths or exist
  are resolved against cwd/scope. Capped at 16, deduped.
- Consumers: `adjudication.rs:74` (guardian facts), `permissions.rs:959`,
  `instructions/on_demand.rs:128` (takes `.next()` as the target for nested instruction files).
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
  - `Walk::nested` extends `output_targets` from the nested analysis, like it does `commands`.
  - Targets whose word expands (`literal == None`) are kept; `touched_paths` skips them, like it
    skips expanding argument words. Soundness is unchanged: `output_redirect` still sends the call
    to the guardian, which also sees the raw command.
- `touched_paths` ("shell" arm): list redirect targets first, then command argument words. The
  resolve step (absolute vs. cwd-relative, `scope.resolved`) is factored into a local closure
  shared by both. Redirect targets are listed unconditionally (no path-like/exists filter): a
  redirect target is a file by construction, including a not-yet-existing bare name like `cfg.py`.
  Listing them first makes the on-demand instruction lookup pick the written file rather than a
  junk argument.

## Implementation Steps
1. `preclassify.rs`: add `output_targets` to `ShellAnalysis` (doc comment), populate in
   `Walk::redirect`, merge in `Walk::nested` (and any other site that merges nested analyses,
   e.g. ~l.401).
2. `destructive.rs`: in the shell arm of `touched_paths`, resolve and push `output_targets`
   literals before the argument-word loop.
3. Tests (below).
4. Docs: update wherever the guardian facts' `paths_this_call_touches` is described (see
   Documentation).
5. `CHANGELOG.md` entry under Unreleased/Fixed.

## Files to Modify
- `rust/fontaine-tools/src/preclassify.rs`
- `rust/fontaine-core/src/destructive.rs`
- `rust/fontaine-core/tests/destructive.rs`
- `CHANGELOG.md`
- docs page describing touched paths, if one exists (`rg paths_this_call_touches docs/`)

## Trade-offs
- Per-`SimpleCommand` targets vs. a global list: compound-command redirects have no simple
  command to attach to, so a global list is the only complete option.
- Targets first vs. appended: appended keeps the old ordering but leaves junk words ahead of the
  written file for the on-demand lookup, which is the bug's second symptom.

## Decisions
- No change to `output_redirect` semantics or to any permission verdict.

## Documentation
Update the doc page that lists session facts / `paths_this_call_touches` (find with `rg` in
`docs/`) to say shell redirect targets are included.

## Testing Strategy
Unit tests, no live APIs:
- `preclassify.rs`: for each operator `>`, `>>`, `>|`, `<>`, `&>`, `&>>`, `>&file`, assert
  `output_targets` holds the target's literal; assert `2>&1`, `>&2`, `1>&-`, `< in`, `<<< w`
  yield none; assert targets are found in compound (`{ echo x; } > f`), pipeline, substitution
  and `sh -c 'echo x > f'`; an expanding target (`> $F`) is present with `literal == None`.
- `fontaine-core/tests/destructive.rs`: `touched_paths("shell", {"command": "printf '# generated\\n' >> cfg.py"})`
  lists `<root>/cfg.py` first (pins the reported bug), for each redirect operator the target is
  present, an absolute target and a `cwd`-relative target resolve correctly, and `2>&1` adds no
  path.
- On-demand: a test that `printf x >> sub/cfg.py` selects `sub/cfg.py` as the instruction target,
  if the existing on_demand tests make this cheap.

## Open Questions
None.
