# Claude Code-parity grep/ls Arguments Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/62

## Overview
`grep` and `ls` take rig-tools 0.45's `SearchArgs`/`ListArgs` verbatim, so they lack the arguments
models already know from Claude Code's Grep and LS: `output_mode`, `-i`, `-n`, `-A/-B/-C`, `type`,
`head_limit`, and LS `ignore` globs. Add them in a first-party implementation inside
`fontaine-tools` that reuses rig's public `Workspace` walker, so no upstream change is needed and
path-rule/scope checks, which read only `path`, are untouched.

## Current State
- `rust/fontaine-tools/src/builtins.rs` defines `Ls` (`type Args = ListArgs`) and `Grep`
  (`type Args = SearchArgs`) as thin wrappers that forward to rig's `List`/`Search`. `Glob` also
  forwards to `Search` with a `SearchArgs { files: true, .. }` and stays as is.
- `search_for(root)` / `search_external(primary)` / `list_for` / `list_external` build the confined
  and the unconfined-fallback instances; `GuardedTool::per_root_with_fallback` registers them.
- rig's `find()` returns only `path:line: text` hits capped at `MAX_MATCHES` (200); `fs::listed` is
  `pub(crate)`, but `Search { workspace, base, confine }` has public fields and `Workspace::files`
  and `Workspace::read_files` are public trait methods; `place` is an inherent method on
  `impl dyn Workspace`.
- Path checks (`scope.rs` `PathFields`, `descriptor.rs`) read the `path` key only; the footprint
  (`Touches::Reads`) is computed from `path`.
- Documented in `docs/tools.md` (rows for `ls` and `grep`); the deviation is the 2026-10-21 line in
  `tasks/fontaine_harness_plan.md` `## Decisions`. `glob` keeps using rig's `Search`.
- Fontaine's `Grep`/`Ls` do not forward `footprint` today; risk class (`ReadOnly`), `GuardedTool`
  registration, `scope.rs` path extraction, out-of-root fallback and approval prompts all key off
  `path`, which keeps its name and meaning.

## Design
New module `rust/fontaine-tools/src/search.rs` (first-party), used by `Grep` and `Ls`. Walking,
gitignore handling, hidden-file rules, confinement (`place`) and reading stay rig's: the module calls
`Workspace::place`, `files`, `read_files` and builds on `Search`/`List`'s public fields.

### `grep` arguments (`GrepArgs`, `deny_unknown_fields`)
| Field | Meaning |
|---|---|
| `pattern` | Rust regex (required) |
| `path`, `glob`, `hidden` | as today |
| `output_mode` | `content` \| `files_with_matches` \| `count`; default `files_with_matches` (Claude Code's) |
| `-i` | case-insensitive (compile with `RegexBuilder::case_insensitive`, not by editing the pattern) |
| `-n` | line numbers in `content` mode; default true |
| `-A`, `-B`, `-C` | context lines after/before/both (`content` only; `-C` sets both unless `-A`/`-B` given) |
| `type` | ripgrep file type (`rust`, `py`, ...), resolved with the `ignore` crate's built-in `TypesBuilder::add_defaults()`, applied to the walked file list; unknown type is an invalid-arguments error listing nothing guessed |
| `head_limit` | max output entries (lines in `content`, files in `files_with_matches`, files in `count`); clamped to the existing hard caps (200 matches, 1000 files); `0` means unlimited, still bounded by the hard caps |
| `files` | deprecated alias: `files: true` is `output_mode: files_with_matches`; when `output_mode` is also given, `output_mode` wins |

An empty `pattern` lists the files `Workspace::files` returns without reading them and publishes no
`FilesRead`, in `files_with_matches` mode (and under `files: true`); in `content` and `count` mode it
is an invalid-arguments error.

Output: `content` renders `path:line: text` for matches and `path-line- text` for context lines with
`--` between non-adjacent groups (ripgrep/Claude Code form; without `-n`, `path: text`); `count`
renders `path:N` per file; `files_with_matches` one path per line. Text is clipped at 300 chars as
now, the stop line says `[stopped at N ...]` when `head_limit` or the hard cap cut the result.
Context lines never count toward `head_limit`, matches do.

Search internals reuse rig's approach: files from `Workspace::files(root, glob, listing)`, read
via `read_files(.., MAX_FILE_BYTES)`, UTF-8 only. The whole-text prefilter in rig's `find` is an
optimization and is dropped; per-line matching is enough. The tool publishes `FilesRead` for files
with a match exactly as rig's `Search::call` does (same `FileRead::new(canonical, content_hash)`),
so the read-before-edit tracking is unchanged. Relative path display below `base` follows rig's
(the module re-bases displayed paths itself), and the module re-implements the `fs::listed` error
mapping: invalid glob is invalid-args, missing root is not-found, and a stat when the file list is
empty. `Grep` and `Ls` override `footprint`, returning `Touches::Reads` from `path` as rig's
`Search`/`List` do.

### `ls` arguments (`LsArgs`)
`path`, `depth`, `glob`, `hidden` as today plus `ignore: Option<Vec<String>>` (Claude Code's glob
list). After `Workspace::files`, drop files whose path below the listed root, or any ancestor
directory of it, matches an ignore glob (`globset`, already a workspace dependency). Rendering,
`depth` folding, `MAX_LINES`/`MAX_BYTES` cut and the published `FilesListed`/`OutputCut` follow rig's
`list`; since `list()` there is private, the module re-implements that short fold (about 40 lines)
over the filtered file list rather than patching rig.

## Implementation Steps
1. Add `ignore.workspace = true`, `globset.workspace = true` and `schemars.workspace = true` to
   `rust/fontaine-tools/Cargo.toml`, and `regex` as a new exact-pinned `[workspace.dependencies]`
   entry in `rust/Cargo.toml` referenced from fontaine-tools; `cargo deny check`.
2. Create `fontaine-tools/src/search.rs`: `GrepArgs`, `OutputMode`, `run_grep(&Search, ctx, args)`,
   `LsArgs`, `run_ls(&List, ctx, args)`, pure render helpers (`render_content`, `render_count`).
   Export from `lib.rs`.
3. `builtins.rs`: switch `Grep`/`Ls` to `GrepArgs`/`LsArgs`, schemas via `args_schema`, rewrite the
   descriptions (drop the `(?i)` advice, name the new args). Drop the `SearchArgs`/`ListArgs`
   imports left unused (`Glob` still builds a `SearchArgs`).
4. Unit and tool-level tests (see Testing Strategy).
5. Docs: `docs/tools.md` rows; `CHANGELOG.md` entry.
6. Master plan: amend the 2026-10-21 `ls`/`grep` line in `### Tools` to say it is superseded, and
   add the dated Decision (see Decisions) at the end of `### Tools`.

## Files to Modify
- `rust/fontaine-tools/src/search.rs` (new), `src/lib.rs`, `src/builtins.rs`, `Cargo.toml`,
  `rust/Cargo.toml` (`regex`)
- `rust/fontaine-tools/tests/` (new `grep_ls.rs`)
- `docs/tools.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions)
- `rust/fontaine-core/prompts/system_prompt.md` and its snapshot test in
  `rust/fontaine-core/tests/snapshots/` are unchanged; the approval prompt (`permissions.rs`) keeps
  showing only `pattern [in path]` and intentionally does not show the new arguments.

## Trade-offs
- **Upstream into rig-tools**: rig is pinned to exact versions and an upstream release cycle would
  block this; a first-party module over the public `Workspace` trait needs no fork. Rejected.
- **Shell out to `rg`**: adds a runtime binary dependency and bypasses `Workspace`/confinement.
  Rejected.
- **Edit the pattern for `-i` (`(?i)` prefix)**: breaks patterns that already start with flags;
  use the regex builder.

## Decisions
- 2026-11-01: Default `output_mode` is `files_with_matches`, as in Claude Code, a change from
  today's `content` default; callers that relied on the old `content` default now get paths. The
  `files` argument stays as an alias, with `output_mode` taking precedence.
- 2026-11-01: `head_limit` is clamped to the existing hard caps rather than replacing them.
- 2026-11-01: Offset/pagination and `multiline` are out of scope (not in the issue).

## Documentation
`docs/tools.md` (`ls`/`grep` rows and the paragraph on caps); `CHANGELOG.md`. Describe current
behavior only, no plan citations.

## Testing Strategy
All unit/tool-level, no live APIs, temp-dir fixtures:
- `GrepArgs` parsing: each new key (`-i`, `-A`, `-n`), defaults, unknown key rejected, `files` alias.
- `content` mode with `-n` on/off, `-A`/`-B`/`-C` including overlapping and adjacent groups and `--`
  separators, file start/end boundaries.
- `count` and `files_with_matches` output; `head_limit` below and above the hard cap and the stop line.
- `-i` matches case-insensitively and a pattern beginning with its own `(?-i)` flag still works.
- `type`: `rust` keeps `.rs` only; unknown type errors.
- `hidden` includes hidden files; non-UTF-8 skipped; out-of-root `path` refused by the confined instance.
- `FilesRead` is still published for files with a match (regression guard for read-before-edit); an
  empty `pattern` publishes none; `head_limit: 0`; `output_mode` over `files`.
- `ls` `ignore`: file glob, directory glob, ancestor match, combined with `depth` and `glob`.
