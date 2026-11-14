# cache-bench Follow-ups Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/498

## Overview
Two small fixes to `fontaine-evals cache-bench`: a relative `--fontaine`/`--claude` path must work
even though the bench spawns each side from a temporary fixture directory, and the `message.id`
dedupe in the `claude -p` parser must be confirmed against the real stream.

## Current State
- `rust/fontaine-evals/src/cache_bench.rs` `run_bench` passes `options.fontaine` and
  `options.claude` unchanged to `run_side`, which calls `Command::new(program).current_dir(cwd)`.
  A relative path containing a separator is resolved against the child's new cwd, so
  `target/debug/fontaine` fails with "cannot run ...: No such file or directory".
  A bare name (`fontaine`, `claude`) is a PATH lookup and works.
- `parse_claude_stream` (same file) keeps one step per distinct `message.id`, last usage wins. A
  unit test (`claude_steps_dedupe_by_message_id_and_the_last_usage_wins`) covers it with a
  synthetic stream; nothing confirms the real stream repeats the id across the events of one
  message.
- `evals/README.md` documents the flag ("The executables; default `fontaine` and `claude`").

## Design
1. **Path resolution.** Add a helper `resolve_program(program: &str) -> Result<String, String>`
   in `cache_bench.rs`: if the value contains a path separator (`/`, or `\` on Windows —
   check `Path::components().count() > 1` or `has_root()`), return
   `std::path::absolute(program)` (no symlink resolution, no existence requirement) as a string;
   otherwise return it unchanged so PATH lookup still works. Call it once per side at the top of
   `run_bench`, before any directory is created. `claude_version` also receives the resolved
   `claude` value. Error from `absolute` is propagated as a `String` with context.
2. **Dedupe verification.** Run `claude -p --output-format stream-json --verbose` on the bench
   task and inspect whether assistant events of one message share `message.id` and whether the
   per-step usage is double counted.
   - If ids repeat: the dedupe is confirmed; save a trimmed real excerpt (usage fields and ids
     only, no content) as a test fixture and assert the parser output on it, replacing reliance on
     the synthetic shape alone.
   - If ids do not repeat: change the parser to whatever key the real stream offers (for example
     collapsing consecutive assistant events with equal usage) and record the finding in the
     docs.

## Implementation Steps
1. Add `resolve_program` and use it in `run_bench` for both executables.
2. Unit tests for `resolve_program` (see Testing Strategy).
3. Run the live check from Manual Verification; apply the matching branch of Design item 2.
4. Update `evals/README.md` flag row: relative paths are resolved against the current directory.
5. CHANGELOG entry.

## Files to Modify
- `rust/fontaine-evals/src/cache_bench.rs`
- `evals/README.md`
- `CHANGELOG.md`
- possibly a fixture under `rust/fontaine-evals/tests/` (Design item 2)

## Trade-offs
Resolving in `run_bench` rather than only in the CLI handler (`runner.rs`) keeps the library
entry point safe for any caller. Resolving by `absolute` rather than `canonicalize` avoids an
existence requirement, so the existing "cannot run" error still names a missing binary.

## Documentation
`evals/README.md` flag table (above). `docs/claude-subscription.md` needs no change unless the
live check shows the dedupe key differs.

## Testing Strategy
Unit tests in `cache_bench.rs`: relative path with a separator becomes absolute and ends with the
same components; a bare name is returned unchanged; an already absolute path is unchanged.
The pre-existing integration test `rust/fontaine-evals/tests/cache_bench.rs` keeps covering the
spawn path.

## Manual Verification
1. From `rust/`, `cargo build -p fontaine`, then
   `cargo run -p fontaine-evals -- cache-bench --model <acct>/sonnet --fontaine target/debug/fontaine --out ../scratch.json`
   — expect a scorecard, not "cannot run". Needs a signed-in account and the real CLI, so it
   cannot be automated.
2. In an empty directory, `claude -p "read a.txt then b.txt" --output-format stream-json --verbose`
   and list each assistant event's `message.id` and usage; expect repeated ids across events of one
   message and per-step totals equal to the parser's.

## Open Questions
None.
