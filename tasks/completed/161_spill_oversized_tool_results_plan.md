# Spill Oversized Shell Results Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/161

## Overview
Foreground `shell` output past the cut limit is saved to a file in the session scratch directory
(the retained part of the output: at most the 1 MiB job buffer), and the cut result names that file
in its first lines. Pruning old results before a summary request is not built: rig-memory already
shortens every tool result to its first 2,000 characters when it serializes the conversation.
Prompt wording about previews and spills is already shipped (#443); this plan covers capture,
storage and lifecycle.

## Current State
- MCP results over the limit already spill: `rust/fontaine-sdk/src/tools/mcp/output.rs` (`cap`,
  `save`, `saved_message`) writes through `rig_tools::Spill` into the session scratch directory
  and the text names the path.
- `shell` does not: `tools/builtins.rs` `render_foreground` calls `tools/jobs/cut.rs::cut`, which
  keeps first lines, error lines and last lines and returns `OutputCut { spill: None, .. }`. The
  text says lines were cut but names no file. `tools/jobs/tools.rs:85` (`job_output`) cuts the same
  way. `search.rs:546` also builds `OutputCut { spill: None }`.
- Job output is held in a bounded ring (`tools/jobs.rs`, `BUFFER_BYTES` 1 MiB, `Chunk.missed` counts
  lines dropped from the front; `OUTPUT_CAP` 64 MiB only stops a runaway), so the dropped bytes
  cannot be rebuilt later.
- The scratch directory (`core/scratch.rs`) is owner-only, a workspace root (file tools reach it
  without a prompt) and is removed when the `Scratch` drops at session end.
- Compaction: `core/session.rs` builds `compaction_policy()` (evidence rules appended to rig-memory's
  `format`/`update` prompts, which already ask for `Output state` with recovery pointers) and runs
  rig-memory's summarizer through `compact_via`. rig-memory serializes message content, not
  `ToolContext`, so a path kept only in `OutputCut.spill` is invisible to it. Its `serialize` also
  cuts every tool result to its first `snippet_chars` (default 2,000, head only, with
  `[… N more characters]` appended); `fitted_policy` only sets reserve and summary_tokens, so
  the default applies.

## Design
### 1. Spill shell output (result time)
- `jobs/cut.rs::cut` gains the spill step ahead of the cut: when the output does not fit the
  limit and a spill directory is configured, the retained sanitized text (already escape-stripped
  and redacted per line by `sanitize`) is written with `Spill::create("shell")`, and the cut text
  starts with one pointer line `[output cut; saved for this session: <path>. Use read with offset/limit, or grep that
  file.]` ahead of the head lines. It is first because the summarizer keeps only the first 2,000
  characters of a result, so a trailing line would never reach it. There is no footer. The path is
  in the result text, not only `OutputCut.spill`, and `OutputCut.spill` is also set so tool
  metadata agrees.
- The shell tool gets its own `Spill` in a separate directory, `scratch/shell-output` (not
  `mcp-results`), built from the session scratch directory in `core/agent/mcp.rs`; no scratch
  directory means the current cut with no pointer line. `cut` does the fallback itself on a write
  error: it returns the cut text without a pointer line, the `OutputCut` fact and an `Option<io::Error>`.
  `render_read` (`tools/jobs/tools.rs`, two callers) and `render_foreground` pass the error up, and the
  caller warns once through the existing `warn` hook (`builtins.rs:1174`-style). Both
  `render_foreground` callers, the tool call (`builtins.rs:565`) and the `ShellRun::run` path
  (`builtins.rs:845`), spill.
- The file is written from the chunk the call holds, so it holds only what the ring still retains
  (at most `BUFFER_BYTES`, 1 MiB). When `Chunk.missed > 0` the pointer line says so (`first N lines
  were dropped before this file`) and never claims the file is complete. Background jobs spill in
  `job_output` the same way, one file per read that cuts. `OUTPUT_CAP` does not bound a spill file.
- Scratch growth: the shell `Spill` carries a per-session total budget of 128 MiB (constant, no
  config key) of bytes written. A spill that would pass it is skipped: the call falls back to the
  plain cut without a pointer line, and the caller warns once per session that shell output is no
  longer saved.

## Implementation Steps
1. `jobs/cut.rs`: spill-aware `cut` (new parameter `Option<&Spill>`), pointer line, session budget, tests.
2. Thread the spill through `render_foreground` and `job_output`; build it from the scratch dir.
3. Docs, CHANGELOG, snapshot updates if any prompt text changes.

## Files to Modify
`rust/fontaine-sdk/src/tools/jobs/cut.rs`, `tools/jobs/tools.rs`, `tools/builtins.rs`,
`core/agent/mcp.rs` (builds the shell spill from the scratch directory), the plumbing that carries it
into `BuiltinEnv`, `ShellSettings` and `ShellTool` in `tools/builtins.rs`,
`docs/tools.md`, `CHANGELOG.md`.

## Trade-offs
- Spilling from the retained chunk rather than streaming every line to a file: the file is bounded
  by the ring (1 MiB) and needs no change to job ingest, at the cost that output past 1 MiB is
  saved as its tail only; the pointer line says how many lines were dropped.
- Reusing `rig_tools::Spill` rather than a new store keeps one spill layout across MCP and shell.
- No pre-summary pruning: rig-memory already cuts each tool result to 2,000 characters head-only,
  so a head and tail prune of an older result would be cut again and save nothing.

## Decisions
- Spill in the result text, not only `OutputCut.spill`, because the summarizer reads text only.
- No cross-session artifact store: scratch lifetime is the session; resume dangling pointers are
  recorded by the summarizer's Output state, not recovered.
- AFK: Pre-summary pruning dropped, because rig-memory 0.46 `serialize` already cuts every tool result to `snippet_chars` (2,000, head only) and Fontaine never overrides it, so a 4,096 head + 1,024 tail prune is cut again and gains nothing; answer to the issue's "does a pre-summary prune pay for itself?": no, not on this summarizer.
- AFK: The cache-invalidation and `CompactResult::Compacted` measurement items are moot with pruning dropped; the public enum is unchanged.
- AFK: Per-session shell spill budget of 128 MiB (constant, no config key); past it, plain cut plus one warning, because scratch is typically tmpfs and nothing else bounded file count or size.
- AFK: Spill is written from the retained chunk (at most `BUFFER_BYTES`, 1 MiB), not streamed; the file is the retained tail for larger output, and `OUTPUT_CAP` does not bound it; the pointer line states dropped lines.
- AFK: The spill pointer is the first line of the cut text, not a footer, because rig-memory's 2,000-character head-only snippet drops anything at the end.

## Documentation
Update the Output bullet in `docs/tools.md` (shell limits) to say where retained output goes and that
only the retained tail is saved for output past 1 MiB.

## Testing Strategy
- `cut.rs` unit tests: output over the limit with a spill writes the retained text and the first line
  holds the path; no spill gives the old text; write failure falls back; `missed > 0` pointer line says partial;
  a spill past the session budget falls back and warns once.
- Redaction regression: a secret in the output is absent from the spill file.
