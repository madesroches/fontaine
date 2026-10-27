# NotebookEdit Tool Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/20

## Overview

Add a cell-level `notebook_edit` built-in tool for Jupyter `.ipynb` files (Claude Code's
`NotebookEdit`), registered `WorkspaceWrite` so `acceptEdits` and `plan` semantics apply exactly as
for `edit`/`write`, plus notebook-aware rendering in `read` so the model sees cells, ids and
outputs instead of raw JSON.

## Current State

- Built-ins are assembled in `rust/fontaine-tools/src/builtins.rs` (`build`): rig's `Read`, `Write`,
  `Edit` configured per workspace root via `read_for`/`write_for`/`edit_for`, wrapped by
  `GuardedTool::per_root`, sharing one `Seen` for read-before-edit tracking.
- Each tool's decision inputs live in one row of `TOOLS` in `rust/fontaine-tools/src/descriptor.rs`
  (risk class, `path_free`, `Destructive`, `Specifier`, Claude Code names); `rows_are_pinned` pins
  the table and `rust/fontaine-tools/tests/choke_point.rs` pins risk classes.
- Path-bearing tools are named in three hand-maintained matches: `fields_of` in `scope.rs:47`
  (root selection and containment), `path_args` in `fontaine-core/src/permission_rules.rs:550`
  (rule matching), and `touched_paths` in `fontaine-core/src/destructive.rs:287`.
- `permissions.rs:640` derives "edit class" from `RiskClass::WorkspaceWrite`, so acceptEdits/plan
  need no per-tool code.
- `fontaine-core/src/preview.rs` builds approval diffs for `edit`/`write` only.
- `rust/fontaine-core/src/extensions.rs` rejects `NotebookEdit` in skill/agent `allowed-tools`
  frontmatter (test `unmapped_names_and_bad_specifiers_are_errors` uses it as the unmapped example).
- `rig_tools::Read` reads a `.ipynb` as raw JSON text; there is no notebook awareness.

## Design

### Tool: `notebook_edit` (new `rust/fontaine-tools/src/notebook.rs`)

Arguments mirror Claude Code's: `path` (Claude's `notebook_path`, named `path` like the other file
tools so every path-keyed site handles it), `cell_id` (optional; for `insert` it is the cell to
insert after, none = at the start), `cell_type` (`code`|`markdown`, required for `insert`),
`edit_mode` (`replace` default | `insert` | `delete`), `new_source` (required except for
`delete`). `deny_unknown_fields`.

Behavior, all on the parsed JSON (`serde_json::Value`, `preserve_order` so key order and unrelated
fields survive):
- Refuse a path not ending in `.ipynb`, a non-notebook JSON, or an unsupported `nbformat` (< 4).
- Require the notebook to have been read first via the shared `Seen` (same rule as `edit`): resolve
  with `workspace.place`, take `Seen::hash`/`text` (none = refuse), and apply the edit to that seen
  text so the write-time check is the hash precondition below.
- `replace`: set `source` of the cell; for a code cell clear `outputs` and `execution_count`;
  `cell_type` change allowed (adds/removes `outputs`/`execution_count`).
- `insert`: new cell with a fresh id (nbformat >= 4.5 only; older notebooks get no `id`), empty
  `metadata`, and `outputs: []`/`execution_count: null` for code.
- `delete`: remove the cell.
- Cell lookup tries `id` first and `cell-N` (0-based index, as Claude Code does) only if no id
  matches. Cells without an `id` in a 4.5+ notebook are addressable only by `cell-N`.
- `source` form: `replace` preserves the cell's existing form (string or line array); new cells and
  `cell_type` changes write a line array (each line keeping its `\n`, the last without). `replace`
  keeps the cell `id`. New cells use sorted key order (`cell_type`, `execution_count`, `id`,
  `metadata`, `outputs`, `source`). Render joins array sources.
- Write back with the notebook's original indent (detect 1 vs 2 vs 4 spaces; default 1, nbformat's
  own) and trailing newline, via `Workspace::write_if(path, bytes, seen_hash)` as rig's `Edit` does:
  it writes only while the file still holds the seen text; on a hash mismatch nothing is written and
  the call is refused as stale. `Local` writes with `write_atomic` (temp file and rename) but takes
  no lock, and `write_if` reads then writes, so a concurrent writer between the two is not excluded.
  On success `seen.saw_whole`; on an I/O error `seen.forget`.
- Publish `FilesChanged` itself (no fontaine crate does today): build the `FileChange` with rig-tools'
  public `file_change(path, before, after)` helper.

### Notebook-aware `read`

Wrap rig's `Read` in a `NotebookRead` tool named `read` (same pattern as `Ls`/`Glob` wrapping rig
tools): for a `.ipynb` path return the rendered form — per cell `[cell N id=… type=…]`, source, and
text outputs (stream/`text/plain`; images summarized as `[image/png output omitted]`), truncated
by the same output limit as `read` — otherwise delegate unchanged. It records the notebook as
`Seen` under the path resolved via `workspace.place`, passing the raw file text (not the rendered
form) to `saw_whole`. `Seen::saw_whole` keeps text only up to 8 MiB (`SEEN_TEXT_BYTES`), so a larger
notebook is refused as "not read", and emits the same `FilesRead` result as rig's
`Read` does, so `notebook_edit` is allowed. `edit`/`write` on a `.ipynb` keep working but are steered to
`notebook_edit` in tool descriptions.

### Gate wiring

- `TOOLS` row: `("notebook_edit", WorkspaceWrite, path_free: false, Destructive::FileWrite,
  Specifier::Path, &["NotebookEdit"])`. A `NotebookEdit(path-pattern)` rule therefore works like an
  `Edit(...)` rule.
- Add one `"notebook_edit"` arm next to `"edit"` at each site: `scope.rs` `fields_of`,
  `permission_rules.rs` `path_args`, `destructive.rs` `touched_paths`, and in `permissions.rs` the
  protected-path check (`matches!(tool, "edit" | "write")` before `floor::protected_path`),
  `publish_preview` (`permissions.rs:1682`, `matches!(tool, "edit" | "write")`), and `summarize` (so
  the approval card shows the path); plus the `preview.rs:53` gate with its `notebook_edit` arm, and
  `instructions/on_demand.rs:203` (`matches!(call.tool, "read" | "write" | "edit")`, which appends
  instruction notes after file tools).
- One test iterates the `TOOLS` rows that are `WorkspaceWrite` and not `path_free` and asserts each is
  handled by `fields_of`, `path_args`, `touched_paths` and `protected_path`.
- Register in `builtins::build` via `GuardedTool::per_root`.
- `extensions.rs`: `NotebookEdit` now maps (via `find_claude`); update the test that used it as the
  unmapped example to another name.
- Agents: add `notebook_edit` to the built-in `worker` toolset in `fontaine-agents/src/registry.rs`.
- Approval preview: extend `preview.rs` with a `notebook_edit` arm showing the cell's before/after
  source diff (display-only, same as `edit`).

## Implementation Steps

1. `notebook.rs`: pure notebook model functions (parse, render, apply edit, serialize with indent
   preservation) with no I/O; then the `NotebookEdit` and `NotebookRead` tools over them.
2. Descriptor row; add the `notebook_edit` arm at each site (three matches plus the `permissions.rs`
   sites and `on_demand.rs`); the `TOOLS`-row coverage test; register in `builtins::build`; update
   `rows_are_pinned` and the `choke_point.rs` tables (including `EXPECTED` and the `built_ins` list in
   `fontaine-core/tests/choke_point.rs`) in the same step.
3. Add to `worker` toolset; fix `extensions.rs` mapping and test.
4. Preview arm in `preview.rs`.
5. Docs and changelog.
6. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

## Files to Modify

- `rust/fontaine-tools/src/notebook.rs` (new), `lib.rs`, `builtins.rs`, `descriptor.rs`, `scope.rs`
- `rust/fontaine-tools/tests/choke_point.rs`, `rust/fontaine-core/tests/choke_point.rs` (add
  `notebook_edit` to `built_ins`, ~line 195)
- `rust/fontaine-core/src/{permissions,permission_rules,destructive,preview,extensions}.rs`,
  `rust/fontaine-core/src/instructions/on_demand.rs`,
  `rust/fontaine-core/tests/extensions.rs` (lines 117-119 expect `tools: Read, NotebookEdit` to be an
  error; change the example)
- `rust/fontaine-evals/src/baseline.rs` (map `path` to `notebook_path` for `notebook_edit`, near the
  `TOOL_NAMES` table and the `file_path` mapping)
- `rust/fontaine-agents/src/registry.rs`
- `rust/Cargo.toml` (`serde_json` with `preserve_order`), `rust/fontaine-tools/Cargo.toml` (`uuid`
  if used for cell ids)
- `docs/tools.md`, `docs/permissions.md`, `docs/extensions.md` (line 82 names `NotebookEdit` as
  unmapped), `docs/agents.md` (line 39 `worker` toolset), `docs/tui.md` and `docs/terminal.md`
  (lines 161, 165: `edit`/`write` diff cards), `docs/configuration.md` (line 562 tool list),
  `CHANGELOG.md`, `research/research-claude-code-parity.md` (mark NotebookEdit done)

## Trade-offs

- **Own tool vs. extending `edit`**: `edit` is string-replace over text; cell addressing,
  output clearing and id generation are a different contract, and a distinct tool keeps `Specifier`
  and rules (`NotebookEdit(...)`) aligned with Claude Code.
- **Wrapping `read` vs. a separate `notebook_read`**: issue and Claude Code both make `read`
  notebook-aware; one name for the model, one rule name.
- **Argument name `path` rather than `notebook_path`**: reuses every path-keyed site; the eval
  baseline maps it to Claude's name like `file_path`.

## Decisions

- 2026-10-27: Risk class is `WorkspaceWrite`, per the issue; only the protected-path
  floor applies (the critical-path check is shell-only) because `notebook_edit` is added to the path matches and to the hardcoded
  `edit`/`write` sites in `permissions.rs`.
- 2026-10-27: `edit`/`write` stay allowed on `.ipynb` (as in Claude Code) and are steered to
  `notebook_edit` via tool descriptions.
- 2026-10-27: Image outputs render as an omitted-image placeholder.
- 2026-10-27: `notebook_edit` is included in the `on_demand.rs` file-tool match; a notebook edit is a
  file edit under the primary root, so instruction notes apply.

## Documentation

`docs/tools.md` (new row, notebook-aware `read`), `docs/permissions.md` (acceptEdits/plan lists and
rule specifier table name `notebook_edit`), `docs/extensions.md`, `docs/agents.md`, `docs/tui.md`,
`docs/terminal.md`, `docs/configuration.md`, `CHANGELOG.md`, parity matrix.

## Testing Strategy

Unit tests (pure functions in `notebook.rs`): replace/insert/delete on code and markdown cells;
output clearing; id vs `cell-N` lookup; insert-at-start; nbformat 4.4 (no ids) vs 4.5; indent and
trailing-newline round trip; unknown-field preservation; error paths (not `.ipynb`, invalid JSON,
nbformat 3, missing cell, missing `new_source`, bad `cell_type`); render with stream/error/image
outputs. Tool tests over a tempdir: not-read-first refusal, stale-after-read refusal, confinement
outside the root, atomic write, a notebook over 8 MiB refused as not read. Gate tests: descriptor pin, choke-point class, `acceptEdits` runs it
inside the root, `plan` blocks it, a `.git/` path hits the protected-path floor through the real
permission pipeline, a `NotebookEdit(...)` rule matches, preview diff, the `TOOLS`-row coverage test.
A new registry test that `worker` includes it (`registry.rs:268` is only a generic intersection test).
The workspace `serde_json` dependency gets `preserve_order` enabled explicitly (it is active today
only through feature unification); `uuid` (a workspace dependency) is added to `fontaine-tools` if
used for cell ids.
