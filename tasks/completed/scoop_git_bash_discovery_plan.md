# Scoop Git Bash Discovery Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/553

## Overview

On Windows, `fontaine_tools::git_bash::discover` does not find Git for Windows installed with
scoop: the only `git.exe` on `PATH` is scoop's shim, which `GitBash::from_executable` rejects, and
the standard install locations don't cover `%USERPROFILE%\scoop\apps\git\current`. Discovery
returns `NotFound`, so Fontaine falls back to PowerShell even though Git Bash is installed, and the
TUI integration tests that pass `powershell: None` panic. This plan makes discovery follow scoop's
`git.shim` files and probe scoop's install roots, keeping everything inside the testable `Probe`
abstraction.

## Current State

- `rust/fontaine-tools/src/git_bash.rs:151` `discover(probe)`: the `FONTAINE_GIT_BASH_PATH`
  override, then `git.exe` on PATH, then `ProgramFiles\Git`, `ProgramFiles(x86)\Git` and
  `LocalAppData\Programs\Git`. Every candidate must pass `is_file` and
  `GitBash::from_executable` (`git_bash.rs:82`), which only recognizes `git.exe`/`bash.exe`/
  `sh.exe` sitting in a Git install's `cmd\`, `bin\`, `usr\bin` or mingw layout. A scoop shim
  (`...\scoop\shims\git.exe`) matches none of those parent-folder names.
- `Probe` (`git_bash.rs:142`) exposes `env` and `is_file` hooks; there is no file-reading hook.
- `Probe` construction sites, all of which gain the new hook:
  `rust/fontaine/src/lib.rs:968` (production, real filesystem),
  `rust/fontaine-tools/src/git_bash.rs:308` (unit tests),
  `rust/fontaine-tools/tests/shell.rs:15,38`, `rust/fontaine-core/tests/common/mod.rs:146`,
  `rust/fontaine-tui/tests/common/mod.rs:690`.
- Docs describing discovery: `docs/tools.md:294-303` (Shell section, Windows bullet) and
  `docs/installing.md:115-117`; `tasks/design/platforms.md:11-14` records the discovery rule for
  the project. `docs/hooks.md:52` defers to "found the same way as for the `shell` tool", so it
  needs no edit.

## Design

### Shim following

Scoop writes a one-line `git.shim` next to each shim `git.exe`:

```
path = "C:\Users\<user>\scoop\apps\git\current\cmd\git.exe"
```

When a `git.exe` candidate on PATH is not recognized by `from_executable` (whether or not the
candidate exists), `discover` looks for a sibling `git.shim`, parses its `path` value and tries
that target **once, without recursing**: the resolved path gets the same `is_file` +
`from_executable` check as any other candidate, and a `git.shim` found next to the resolved target
is not read. A shim that is missing, unreadable, malformed, dangling (target not a file) or
pointing at something that is not a Git executable falls through to the next candidate — never an
error. A PATH `git.exe` that `from_executable` already recognizes wins without the shim being
read at all.

Parsing: decode the file as UTF-8 (invalid UTF-8 counts as an unreadable shim), scan its lines for one of the form `path = <value>`,
take the value, trim it, strip one pair of surrounding double quotes, and accept it when non-empty.
Every other line is ignored.

### Scoop install roots

After the existing three standard locations, `discover` also probes scoop's `current` app
junctions:

- per-user: `%SCOOP%\apps\git\current`, defaulting to `%USERPROFILE%\scoop` when `SCOOP` is unset;
- global: `%SCOOP_GLOBAL%\apps\git\current`, defaulting to `%ProgramData%\scoop` when
  `SCOOP_GLOBAL` is unset.

Each root contributes the candidate `<root>\cmd\git.exe`, checked exactly like today's install
locations (no shim hop — these are real installs). `from_executable` maps
`...\apps\git\current\cmd\git.exe` to the root `...\apps\git\current`, and `is_file` follows the
junction to the versioned directory. An unset `SCOOP`/`SCOOP_GLOBAL` — or unset
`USERPROFILE`/`ProgramData` for the defaults — simply skips that root.

### Probe hook

```rust
pub struct Probe<'a> {
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    pub is_file: &'a dyn Fn(&Path) -> bool,
    /// Full file contents, for shim following. `None` when unreadable or not a file.
    pub read_file: &'a dyn Fn(&Path) -> Option<String>,
}
```

The production closure in `fontaine/src/lib.rs` is `&|path| std::fs::read_to_string(path).ok()`.

`discover` splits into a helper so both candidate kinds share the recognition check:

```rust
fn resolve_candidate(probe: &Probe<'_>, candidate: &Path, follow_shim: bool) -> Option<GitBash> {
    if (probe.is_file)(candidate) && let Some(found) = GitBash::from_executable(candidate) {
        return Some(found);
    }
    if !follow_shim {
        return None;
    }
    let contents = (probe.read_file)(&candidate.with_file_name("git.shim"))?;
    let target = shim_target(&contents)?;
    if (probe.is_file)(&target) {
        GitBash::from_executable(&target)
    } else {
        None
    }
}
```

PATH candidates call it with `follow_shim = true`; standard-location and scoop-root candidates
with `false`.

### Precedence

1. `FONTAINE_GIT_BASH_PATH` — unchanged; a bad value stays `GitBashError::BadOverride`.
2. `git.exe` on PATH, now with one shim hop.
3. The three standard install locations — unchanged, same order.
4. Scoop per-user root, then scoop global root.

`GitBashError` gains no variant: every shim failure mode is a fall-through, per the issue.

## Implementation Steps

1. `rust/fontaine-tools/src/git_bash.rs`: add `read_file` to `Probe`; add the `shim_target`
   parser; add `resolve_candidate`; rewire `discover` (shim hop for PATH candidates, scoop roots
   appended after the standard locations).
2. Add the real-filesystem `read_file` closure at the five construction sites.
3. Extend the unit tests' `probe_with` with a file-contents map next to the file set; write the
   tests listed below.
4. Reword the comment at `rust/fontaine/tests/headless_smoke.rs:71-72` to say that without PATH,
   only the standard and scoop install folders are searched.
5. Docs: the Windows bullet of the Shell section in `docs/tools.md`; one line in
   `docs/installing.md`; the discovery sentence in `tasks/design/platforms.md`; the
   `CHANGELOG.md` `[Unreleased]` entry citing #553; a dated `(#553)` line at the end of the
   Tools subsection of `## Decisions` in `tasks/fontaine_harness_plan.md`.

## Files to Modify

- `rust/fontaine-tools/src/git_bash.rs` (discovery, parser, unit tests)
- `rust/fontaine/src/lib.rs` (one closure)
- `rust/fontaine-tools/tests/shell.rs` (two closures)
- `rust/fontaine-core/tests/common/mod.rs` (one closure)
- `rust/fontaine-tui/tests/common/mod.rs` (one closure)
- `rust/fontaine/tests/headless_smoke.rs` (one comment)
- `docs/tools.md`, `docs/installing.md`, `tasks/design/platforms.md`, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (dated Decisions line)

## Trade-offs

- **Read the shim's recorded target instead of deriving the scoop layout from the shim's own
  location.** The shim names the exact `git.exe` scoop runs; translating `...\scoop\shims` into
  `...\scoop\apps\git\current` would guess at scoop's directory layout rather than read it, and
  would miss non-default `SCOOP` roots on PATH anyway (the default-root probe already covers the
  not-on-PATH case).
- **Probe the `current` junction, not the versioned `apps\git\<version>` directories.** `current`
  is the junction scoop maintains and the shim points at; globbing versioned directories adds
  ordering questions for no benefit.
- **A generic `read_file` hook rather than a shim-specific `read_shim` one.** Slightly wider than
  strictly needed, but it keeps `Probe` the single seam for "the filesystem as discovery sees it",
  matching how `env` and `is_file` are already general.
- **No recursion into a second shim.** One hop covers what scoop generates; a chain is treated as
  "not a Git executable" and falls through, which keeps the failure modes linear.

## Decisions

- 2026-11-09 (#553): Scoop shim following and the scoop roots deviate from Claude Code's discovery pattern; recorded in the master plan's Tools decisions.
- 2026-11-09 (#553): The plan had omitted the master-plan Decisions line, a `CHANGELOG.md` entry and the stale `headless_smoke.rs` comment; all three are now steps. Invalid UTF-8 in a shim is treated as unreadable rather than decoded lossily.

## Documentation

- `docs/tools.md` — Shell section, Windows bullet: discovery follows scoop shims (a sibling
  `git.shim`) and probes `%SCOOP%` / `%SCOOP_GLOBAL%` roots, so a scoop-installed Git for Windows
  is found without `FONTAINE_GIT_BASH_PATH`.
- `docs/installing.md` — the "Git for Windows is recommended but not required" note adds that a
  scoop install is discovered automatically.
- `tasks/design/platforms.md` — the discovery sentence gains the shim-following and scoop-root
  clause.
- `CHANGELOG.md` — an `[Unreleased]` entry citing #553.

## Testing Strategy

Unit tests through `Probe` only — the issue's explicit requirement; no real scoop install needed.
`probe_with` gains a contents map beside the file set, and paths are built from `PathBuf`/
`std::env::join_paths` so the tests run on all three CI OSes (single-entry `PATH` strings are
portable as plain text, as the existing tests already rely on):

- a PATH `git.exe` that is a scoop shim (rejected by `from_executable`) with a well-formed
  `git.shim` beside it resolves to the target install;
- a recognized PATH `git.exe` with a sibling `git.shim` is used directly — the shim is not
  followed;
- a malformed `git.shim` (garbage, or no `path` line) falls through to the install locations;
- a dangling `git.shim` (target not in the file set) falls through;
- a shim whose target is itself a shim location (a chain) is not recursed into and falls through;
- `%SCOOP%\apps\git\current\cmd\git.exe` is found when `SCOOP` is set;
- the `%USERPROFILE%\scoop` default applies when `SCOOP` is unset, and the `%ProgramData%\scoop`
  default when `SCOOP_GLOBAL` is unset;
- PATH still wins over every install location, the override still wins over everything, and the
  existing discovery tests keep passing unchanged.

## Open Questions

(none — precedence, defaults and fall-through behavior all follow the issue text)
