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
| `-i` | case-insensitive; named in the description (compile with `RegexBuilder::case_insensitive`, not by editing the pattern) |
| `-n` | line numbers in `content` mode; default true; off, context lines render `path- text` |
| `-A`, `-B`, `-C` | context lines after/before/both (`content` only; unsigned integers; an explicit `-A`/`-B` overrides `-C` for its side) |
| `type` | ripgrep file type (`rust`, `py`, ...), resolved with the `ignore` crate's built-in `TypesBuilder::add_defaults()`, applied to the walked file list, so `type` and `glob` combine as AND; unknown type is an invalid-arguments error listing nothing guessed |
| `head_limit` | max output entries (matching lines in `content`, files in `files_with_matches` and `count`); clamped to the hard caps (200 matches, 1000 files); `0` means unlimited, still bounded by them. Only the output is capped: the walk and scan are not |
| `files` | deprecated alias: `files: true` is `output_mode: files_with_matches`; when `output_mode` is also given, `output_mode` wins |

An empty `pattern` lists the files `Workspace::files` returns without reading them and publishes no
`FilesRead`, in `files_with_matches` mode (and under `files: true`); in `content` and `count` mode it
is an invalid-arguments error.

Output: `content` renders `path:line: text` for matches and `path-line- text` for context lines with
`--` between non-adjacent groups (ripgrep/Claude Code form; without `-n`, `path: text`); `count`
renders `path:N` per file, N the number of matching lines, files with none omitted;
`files_with_matches` one path per line. Text is clipped at `MAX_LINE` (300; private in rig, so the
module redefines it). When `head_limit` or a hard cap cut the result, the last line is rig's:
`[stopped at N matches]` for `content`, `[stopped at N files; narrow the search with path or glob]`
for `files_with_matches` and `count`, N the effective limit. Context lines never count toward
`head_limit`, matches do; context stays within its file and the last counted match's
trailing context is still emitted.

Search internals reuse rig's approach: files from `Workspace::files(root, glob, listing)`, read
via `read_files(.., MAX_FILE_BYTES)`, UTF-8 only. The whole-text prefilter in rig's `find` is an
optimization and is dropped; per-line matching is enough. The tool publishes `FilesRead` for every
file with a match in all three modes, including files whose output `head_limit` cut, as rig does
(same `FileRead::new(canonical, content_hash)`), so the read-before-edit tracking is unchanged; the
empty-pattern file listing publishes none. Files over `MAX_FILE_BYTES` are skipped. Relative path display below `base` follows rig's
(the module re-bases displayed paths itself, in all three modes and for the `FilesRead` canonical
paths), and the module re-implements the `fs::listed` error
mapping: invalid glob is invalid-args, a bad pattern is invalid-args, missing root is not-found, and a stat when the file list is
empty. `Grep` and `Ls` override `footprint`, returning `Touches::Reads` from `path` as rig's
`Search`/`List` do.

### `ls` arguments (`LsArgs`)
`path`, `depth`, `glob`, `hidden` as today plus `ignore: Option<Vec<String>>` (Claude Code's glob
list). After `Workspace::files`, drop files whose path below the listed root, or any ancestor
directory of it, matches an ignore glob (`globset`, already a workspace dependency): `node_modules` matches the
directory at any depth, an invalid ignore glob is invalid-args, and `hidden` still governs which
files the walk returns. A file root behaves as in rig's `list`. Rendering,
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
- 2026-11-01: An empty `pattern` lists files in `files_with_matches` mode (and under `files: true`)
  and is an invalid-arguments error in `content` and `count`, unlike rig (a valid regex returning up
  to 200 arbitrary lines) and Claude Code.
- 2026-11-01: Offset/pagination and `multiline` are out of scope (not in the issue).

## Documentation
`docs/tools.md` (`ls`/`grep` rows and the paragraph on caps); `CHANGELOG.md`. Describe current
behavior only, no plan citations.

## Testing Strategy
All unit/tool-level, no live APIs, temp-dir fixtures:
- `GrepArgs` parsing: each new key (`-i`, `-A`, `-n`), defaults, unknown key rejected, `files` alias,
  negative or non-integer `-A/-B/-C` rejected.
- `content` mode with `-n` on/off, `-A`/`-B`/`-C` including overlapping and adjacent groups and `--`
  separators, file start/end boundaries, explicit `-A`/`-B` overriding `-C`, context at the
  `head_limit` cut, `-n` off with context lines.
- `count` (matching lines, zero-match files omitted) and `files_with_matches` output; `head_limit`
  below and above the hard cap in all three modes, with each mode's exact stop line.
- `-i` matches case-insensitively and a pattern beginning with its own `(?-i)` flag still works.
- `type`: `rust` keeps `.rs` only, combined with `glob` as AND; unknown type errors.
- `hidden` includes hidden files; non-UTF-8 and over-`MAX_FILE_BYTES` files skipped; out-of-root `path` refused by the confined
  instance; bad pattern, bad glob and missing path errors; displayed paths re-based in every mode.
- `FilesRead` is published for every file with a match in each mode, including `content` with
  `head_limit` (regression guard for read-before-edit); an empty `pattern` publishes none; `head_limit: 0`; `output_mode` over `files`.
- `ls` `ignore`: file glob, directory glob, ancestor match, combined with `depth`, `glob` and `hidden`; `node_modules` at any depth; invalid ignore glob errors;
  a file as root.
