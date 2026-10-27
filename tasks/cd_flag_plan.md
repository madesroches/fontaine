# `--cd <DIR>` Flag Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/145

## Overview

Add `--cd <DIR>` (short `-C`) so Fontaine starts with `<DIR>` as its project directory instead of
the launch directory. The project root, project-scope settings, trust decision, extension loading
and session identity all derive from `Host.cwd`, so the flag is applied before any of them is
resolved. It exists mainly so Fontaine's own repo can be developed from the repo root while
`cargo run` is still launched inside `rust/` (where `rust-toolchain.toml` lives).

## Current State

- `fontaine/src/main.rs:18` builds `Host` from `std::env::current_dir()` (`Host::from_process`,
  `fontaine/src/lib.rs:52`) before the command line is parsed.
- `fontaine::run` (`lib.rs:201`) parses `Cli`, then opens the debug sink (`cli.debug_file`), loads
  config with `launch_dir: &host.cwd` (`lib.rs:87`), runs the trust prompt on
  `config.workspace`, resolves workspace roots from `host.cwd` (`lib.rs:586`) and passes
  `host.cwd` to the TUI (`lib.rs:923`).
- Relative `--settings`, `--add-dir` and `--debug-file` values are resolved against the process
  working directory / launch directory.
- There is no way to point Fontaine at another project directory. `--add-dir` only adds a file-tool
  root; settings, trust and session identity stay on the launch directory.

## Design

**Flag** (`fontaine/src/cli.rs`): 

```
/// Start in this directory instead of the launch directory: it becomes the project root ...
#[arg(short = 'C', long = "cd", value_name = "DIR", global = true)]
pub cd: Option<PathBuf>,
```

**Application** — a small function `apply_cd(cli, host) -> Result<(), CdError>` in `lib.rs`
(or a `cd` module), called in `run` right after `check_session_flags` and before the debug sink is
opened, so every later step sees the new directory. `run` takes `mut host`. It:

1. Joins `<DIR>` onto `host.cwd` (an absolute `<DIR>` replaces it), then `canonicalize`s it. A
   path that does not exist or is not a directory is a startup error: `fontaine: --cd <DIR>: <cause>`
   with exit code `EXIT_CONFIG`, cause kept as a `source()` chain and rendered with
   `fontaine_tools::error_chain::chain`.
2. Calls `std::env::set_current_dir` on the canonical path and sets `host.cwd` to it. Changing the
   process directory (not only `host.cwd`) makes everything that still reads the process cwd agree
   with `Host.cwd`: child processes spawned without an explicit cwd, and the relative
   `--settings`, `--add-dir` and `--debug-file` values, which therefore resolve against `<DIR>`
   (the `git -C` / `codex --cd` behaviour).

Canonicalizing matches how the workspace primary root and the session's project identity are
already compared, so the same directory reached through a symlink is one project.

No change to config loading, trust or session code: they already take `host.cwd`. The trust prompt
therefore names `<DIR>`'s workspace, never the launch directory, because `apply_cd` runs before
`load_config`.

Applies to the interactive and `-p` surfaces alike (`run` is shared).

## Implementation Steps

1. `fontaine/src/cli.rs`: add the `cd` field with `-C`/`--cd`.
2. `fontaine/src/lib.rs`: make `host` mutable in `run`; add `apply_cd`; call it before
   `DebugSink::open`. Map failure to `usage_code`-style stderr output and `EXIT_CONFIG`.
3. Tests (below).
4. Docs: `docs/headless.md` flag table, `docs/configuration.md` workspace-roots paragraph (relative
   entries resolve against the project directory, which `--cd` sets), and the CLI flag list in
   `docs/index.md`/`README.md` if flags are enumerated there.
5. `tasks/fontaine_harness_plan.md` `## Decisions`: dated line recording the deviation.
6. `CHANGELOG.md` `Added` entry.

## Files to Modify

- `rust/fontaine/src/cli.rs`
- `rust/fontaine/src/lib.rs`
- `rust/fontaine/tests/` (new `cd_flag.rs`)
- `docs/headless.md`, `docs/configuration.md`
- `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

## Trade-offs

- **Process chdir vs. only overriding `Host.cwd`.** Overriding only `host.cwd` is test-friendly but
  leaves relative CLI paths and spawned children on the launch directory, which is the
  inconsistency the issue complains about. `Host` is injected so only `main`-reachable code
  touches the real environment; `set_current_dir` in `run` is acceptable because `run` already owns
  the process (it is called once per process; tests drive the binary).
- **Name.** `--cd`/`-C` follows `git -C` and `codex --cd`; `--cwd` and `--project-dir` are
  rejected as longer or as misleading about the process-directory effect.
- **Relative paths in other flags resolve against `<DIR>`** rather than the launch directory:
  one consistent rule ("as if launched from `<DIR>`") beats a per-flag one.

## Decisions

- AFK: Name is `--cd` with short `-C` (issue's open question): matches the issue's proposal and the two precedents it cites.

## Documentation

`docs/headless.md` (flag table), `docs/configuration.md` (workspace roots: relative entries and
`--add-dir` resolve against the project directory). Fontaine's own `AGENTS.md` mentions
`cargo run -p fontaine`; leave unchanged.

## Testing Strategy

Integration tests in `rust/fontaine/tests/cd_flag.rs` drive the binary (as `debug_file.rs` does)
with `FONTAINE_CONFIG_DIR`/`FONTAINE_DATA_DIR` sandboxed, launched from a different directory than
`<DIR>`:

- `-p` with `--cd <abs>`: the stream-json init event / workspace roots report `<DIR>` (canonical),
  not the launch directory.
- Relative `--cd sub` resolves against the launch directory.
- Project-scope settings in `<DIR>/.fontaine/` take effect, and ones in the launch directory do not
  (pins that config derives from `<DIR>`).
- Nonexistent path and a path to a regular file each exit with `EXIT_CONFIG` and a message naming
  `--cd`.
- A relative `--debug-file` lands inside `<DIR>`.
- `-C` short form works.

The trust-prompt-names-`<DIR>` behaviour is covered structurally (config load receives the changed
`host.cwd`); the prompt needs a TTY and is not driven here.

## Open Questions

None.
