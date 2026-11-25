# Spill Oversized Tool Results and Prune Old Results Before Summarising Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/161

## Overview
Foreground `shell` output past the cut limit is saved whole to a file in the session scratch
directory, and the cut result names that file in its text. Before a summary request, old tool
results over a size are shrunk to head and tail so the summarizer sees a smaller conversation.
Prompt wording about previews and spills is already shipped (#443); this plan covers capture,
storage, lifecycle and pre-summary pruning.

## Current State
- MCP results over the limit already spill: `rust/fontaine-sdk/src/tools/mcp/output.rs` (`cap`,
  `save`, `saved_message`) writes through `rig_tools::Spill` into the session scratch directory
  and the text names the path.
- `shell` does not: `tools/builtins.rs` `render_foreground` calls `tools/jobs/cut.rs::cut`, which
  keeps first lines, error lines and last lines and returns `OutputCut { spill: None, .. }`. The
  text says lines were cut but names no file. `tools/jobs/tools.rs:85` (`job_output`) cuts the same
  way. `search.rs:546` also builds `OutputCut { spill: None }`.
- Job output is held in a bounded ring (`tools/jobs.rs`, `Chunk.missed` counts lines dropped from
  the front, `OUTPUT_CAP` 64 MiB stops a runaway), so the dropped bytes cannot be rebuilt later.
- The scratch directory (`core/scratch.rs`) is owner-only, a workspace root (file tools reach it
  without a prompt) and is removed when the `Scratch` drops at session end.
- Compaction: `core/session.rs` builds `compaction_policy()` (evidence rules appended to rig-memory's
  `format`/`update` prompts, which already ask for `Output state` with recovery pointers) and runs
  rig-memory's summarizer through `compact_via`. rig-memory serializes message content, not
  `ToolContext`, so a path kept only in `OutputCut.spill` is invisible to it.

## Design
### 1. Spill shell output (result time)
- `jobs/cut.rs::cut` gains the spill step ahead of the cut: when the output does not fit the
  limit and a spill directory is configured, the full sanitized text (already escape-stripped and
  redacted per line by `sanitize`) is written with `Spill::create("shell")`, and the cut text ends
  with one footer line `[output cut; full output: <path>. Use read with offset/limit, or grep that
  file.]`. The path is in the result text, not only `OutputCut.spill`, and `OutputCut.spill` is also
  set so tool metadata agrees.
- The shell tool gets its own `Spill` in a separate directory, `scratch/shell-output` (not
  `mcp-results`), built from the session scratch directory in `core/agent/mcp.rs`; no scratch
  directory means the current cut with no footer. `cut` and `render_read` return and propagate a
  write error; callers warn once through the existing `warn` hook (`builtins.rs:1174`-style) and
  fall back to the current cut.
- Lost bytes: when `Chunk.missed > 0` the ring already dropped the front, so the file holds only the
  retained part. The footer says so (`first N lines were dropped before this file`), and never
  claims the file is complete. The spill file for a foreground call is written from the chunk the
  call holds; background jobs spill in `job_output` the same way, one file per read that cuts.
- The footer wording says "this session".
- A spill file is capped at the 64 MiB `OUTPUT_CAP`.

### 2. Prune old results before summarising
- New `core/prune.rs` (SDK core, no new dependency): `prune_old_results(messages, min_chars)`
  returns a copy where each tool result in the summarized range (before `plan.keep_from`, which
  already marks the kept tail) longer than `min_chars` (8,192 characters, head 4,096 and tail 1,024, as the issue's measurement)
  becomes head + `[… N chars pruned …]` + tail. A result that already carries a spill footer keeps
  the footer line. Images and error results are left alone.
- `compact_via` prunes a clone passed to `plan_summary` and applies the summary (`apply_summary`)
  to the unpruned conversation. Pruning preserves message count and order. The saved conversation is
  untouched, so the kept tail after compaction is byte-identical and the prompt cache is not
  invalidated by pruning (compaction invalidates it anyway).
- Trigger floor: prune only when the conversation is over the compaction threshold (it is, by
  definition, when `compact_via` runs), and never below `min_chars`, so a threshold far below the
  window cannot turn pruning into a loop.
- Measure whether it pays: a ledger-style counter of tokens before and after pruning goes into the
  existing compaction event (`CompactResult::Compacted` already reports `before` and `after`), so the
  issue's question is answered from real sessions.

## Implementation Steps
1. `jobs/cut.rs`: spill-aware `cut` (new parameter `Option<&Spill>`), footer, tests.
2. Thread the spill through `render_foreground` and `job_output`; build it from the scratch dir.
3. `core/prune.rs` plus the call in `compact_via`; tests.
4. Docs, CHANGELOG, snapshot updates if any prompt text changes.

## Files to Modify
`rust/fontaine-sdk/src/tools/jobs/cut.rs`, `tools/jobs/tools.rs`, `tools/builtins.rs`,
`core/agent/mcp.rs`, `core/session.rs`, new `core/prune.rs`,
`docs/` page on tools/context, `CHANGELOG.md`.

## Trade-offs
- Spilling in `cut` rather than raising the ring size: the ring is bounded memory; a file is not.
- Pruning a copy rather than the live history: keeps the model's own context stable and avoids a
  second cache-invalidating edit; the cost is that the summarizer sees less than the model did.
- Reusing `rig_tools::Spill` rather than a new store keeps one spill layout across MCP and shell.

## Decisions
- Spill in the result text, not only `OutputCut.spill`, because the summarizer reads text only.
- No cross-session artifact store: scratch lifetime is the session; resume dangling pointers are
  recorded by the summarizer's Output state, not recovered.
- Prune thresholds are constants, no config key.

## Documentation
Update the docs page that describes shell output limits and the context/compaction page to say
where full output goes and that old results are shortened before a summary.

## Testing Strategy
- `cut.rs` unit tests: output over the limit with a spill writes the full text and the footer holds
  the path; no spill gives the old text; write failure falls back; `missed > 0` footer says partial.
- Redaction regression: a secret in the output is absent from the spill file.
- `prune.rs` unit tests: recent results and short results untouched, old long ones shortened with
  head/tail, spill footer kept, images and errors untouched, input not mutated.
- A `ledger.rs` request-level test with `MockCompletionModel`: the summary request holds the pruned
  text while the saved history keeps the original.
