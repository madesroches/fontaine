# Tilde Expansion for Workspace Roots Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/672

## Overview

`/add-dir ~/.config/fontaine` is refused with "it does not exist", because the entry is joined onto
the launch directory verbatim, producing a relative path whose first component is a directory
literally named `~`. A leading `~`, alone or before a separator, must expand to the user's home
directory before the join — the directory a shell would name, and the same home the session already
uses when it judges `rm -rf ~`. The expansion belongs in `workspace::resolve`, the one place every
root source funnels through, so `--add-dir`, `/add-dir` and `[permissions] additional_directories`
are fixed together.

## Current State

- `rust/fontaine-sdk/src/core/workspace.rs:38` — `resolve(primary, launch_dir, added)` builds the
  root list. For each entry it does `launch_dir.join(&entry)` (`:50`) and canonicalizes it; nothing
  expands `~` first, so `~/.config/fontaine` becomes `<launch_dir>/~/.config/fontaine`.
  `RootSource::label` (`:18`) supplies the source wording in warnings, and a bad entry is skipped
  with `ignoring <source> entry `<entry>`: <why>` and never widens scope.
- Every root source reaches it: `--add-dir` and `[permissions] additional_directories` through
  `assemble::resolve_roots` (`rust/fontaine-sdk/src/assemble.rs:95`), and `/add-dir` through
  `AgentRuntime::add_dir` (`rust/fontaine-sdk/src/core/agent.rs:827`). `resolve_roots` callers:
  `rust/fontaine/src/lib.rs:145` (flag/settings roots for both surfaces), `rust/fontaine/src/doctor_cmd.rs:263`,
  `rust/fontaine/src/auto_mode.rs:706` (a direct `workspace::resolve` call), and
  `rust/fontaine-sdk/src/assemble.rs:376` (the SDK path used by the TUI, headless runs and the
  embedded builder's `Session::add_dir`).
- The session's home directory is already known wherever roots are resolved, but is not passed to
  `resolve`: `assemble::home_dir(env)` (`assemble.rs:110`) reads `$HOME` / `%USERPROFILE%` through
  the injected environment, falls back to `dirs::home_dir()`, and canonicalizes; it feeds
  `SessionPrep.home` (`assemble.rs:470`) and is exposed at runtime as `AgentRuntime::home()`
  (`agent.rs:1218`). A `Host` carries the same environment for the binary
  (`rust/fontaine/src/lib.rs:150`).
- A string-level `expand_tilde` already exists for marketplace sources
  (`rust/fontaine-sdk/src/core/plugin_ops.rs:1049`): leading `~` or `~/` only, relative to
  `dirs::home_dir()`, everything else untouched.
- `resolve`'s tests live in `rust/fontaine-sdk/tests/workspace.rs`: order and dedup, resolution
  against the launch directory, and the skip-with-warning cases.

## Design

**Expansion helper** (private to `workspace.rs`): the entry is expanded path-component-wise, so both
separators work on Windows and a directory named `~` deeper in the path stays literal.

```rust
/// A leading `~`, alone or before a separator, names `home`. `None` when the entry names the home
/// directory and the session does not know it.
fn expand_home(entry: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let mut rest = entry.components();
    match rest.next() {
        Some(Component::Normal(first)) if first == "~" => {
            let home = home?;
            let rest = rest.as_path();
            Some(if rest.as_os_str().is_empty() {
                home.to_path_buf()
            } else {
                home.join(rest)
            })
        }
        _ => Some(entry.to_path_buf()),
    }
}
```

Component-wise matching is what keeps `./~/x` and `a/~/b` literal (the first component is `CurDir`
or `a`, not `~`) and `~user/x` literal (the component is `~user`); only a genuine leading `~`
component expands. It also normalizes `~/` the same as `~`.

**`resolve`** gains the home directory as a fourth parameter and expands before joining:

```rust
pub fn resolve(
    primary: &Path,
    launch_dir: &Path,
    home: Option<&Path>,
    added: impl IntoIterator<Item = (RootSource, PathBuf)>,
) -> Roots
```

```rust
let Some(expanded) = expand_home(&entry, home) else {
    warnings.push(skip("the home directory is unknown"));
    continue;
};
let path = launch_dir.join(&expanded);
```

An entry that names an unknown home is skipped with a warning rather than falling back to the
literal `~` component: a wider root is a permission widening, so the unknown case must fail closed,
and a project that happens to contain a directory named `~` must not be picked up by accident. The
warning keeps the entry exactly as the user wrote it.

`launch_dir.join` stays as it is — it already ignores the launch directory when the expanded path is
absolute, and a relative entry keeps resolving against the project directory.

**Wiring** — pass the home the session already resolved, not a second lookup:

- `assemble::resolve_roots(config, launch_dir, home, add_dirs)` (`assemble.rs:95`) forwards it;
  `prepare_session` computes `let home = home_dir(env);` once, ahead of `resolve_roots`
  (`assemble.rs:376`), and reuses it for `SessionPrep.home` (`assemble.rs:470`).
- `rust/fontaine/src/lib.rs:145` passes `home_dir(host)`; `doctor_cmd.rs:263` is unchanged in
  behavior because it calls that helper.
- `rust/fontaine/src/auto_mode.rs:706` passes `crate::home_dir(host)`.
- `AgentRuntime::add_dir` (`agent.rs:827`) passes `self.home()`, so `/add-dir` expands against the
  same home the session judges `~` by.

Nothing else in the pipeline changes: the returned root is still canonicalized, still deduplicated,
and still the value `/add-dir` reports back to the transcript.

## Implementation Steps

1. `rust/fontaine-sdk/src/core/workspace.rs`: add `expand_home`, add the `home` parameter to
   `resolve`, expand before the join, and extend the doc comment to say a leading `~` names the
   home directory and an unknown home is a warning.
2. Thread the parameter through `assemble::resolve_roots` and its four callers (`assemble.rs:376`,
   `rust/fontaine/src/lib.rs:145`, `rust/fontaine/src/doctor_cmd.rs` via that helper,
   `rust/fontaine/src/auto_mode.rs:706`), computing the home once in `prepare_session`.
3. `rust/fontaine-sdk/src/core/agent.rs:828`: pass `self.home()` to `resolve`.
4. `rust/fontaine-sdk/tests/workspace.rs`: pass a home at each existing call site and add the
   expansion, literal-`~`-deeper-in-the-path, `~user`, and unknown-home cases.
5. `rust/fontaine-tui/tests/common/mod.rs:66`: make the world's `home` field public so a script can
   name it.
6. `rust/fontaine-tui/tests/tui.rs`: add the reported command as a regression test — `/add-dir
   ~/shared` adds the world's home directory's `shared` child.
7. `rust/fontaine/tests/headless_contract.rs`: a headless run with `HOME` pointed into the sandbox
   and `--add-dir ~/shared` shows the expanded root in `init.workspace_roots`.
8. Docs (see below), and the changelog entry the `pr` skill writes.

## Files to Modify

- `rust/fontaine-sdk/src/core/workspace.rs`
- `rust/fontaine-sdk/src/core/agent.rs`
- `rust/fontaine-sdk/src/assemble.rs`
- `rust/fontaine/src/lib.rs`
- `rust/fontaine/src/auto_mode.rs`
- `rust/fontaine-sdk/tests/workspace.rs`
- `rust/fontaine-tui/tests/common/mod.rs`
- `rust/fontaine-tui/tests/tui.rs`
- `rust/fontaine/tests/headless_contract.rs`
- `docs/tools.md`, `docs/configuration.md`, `docs/tui.md`

## Trade-offs

- **Expanding in `resolve` vs. at each entry point.** Doing it per surface (flag parsing, the
  command, the settings list) would mean three implementations and a fourth for the embedded
  builder. `resolve` is the single funnel, so one expansion covers every source — which is also
  what the issue asks for.
- **Path-level helper vs. reusing `plugin_ops::expand_tilde`.** The existing helper takes and
  returns `String`, so it would mangle a non-UTF-8 entry, and it resolves against
  `dirs::home_dir()` rather than the session's injected `$HOME` / `%USERPROFILE%`. Reusing it
  would also couple root resolution to the plugin module. The overlap is a four-line match, so the
  duplication is cheaper than the coupling.
- **Unknown home is a warning, not a literal fallback.** `claude -p --add-dir` was observed to
  ignore unresolvable entries silently, and Fontaine's existing rule is "skip it, say why, never
  widen scope"; falling back to a directory named `~` would both surprise the user and add a root
  they did not ask for.
- **`~user/x` stays literal.** Resolving a named user needs a passwd lookup the standard library
  does not offer, and the issue asks only for `~` and `~/…`; a literal `~user` entry simply fails
  the existence check with a warning naming what the user typed.

## Decisions

## Documentation

- `docs/tools.md:214` ("Workspace roots"): next to the relative-entry rule, state that a leading
  `~`, alone or before a separator, expands to the session's home directory (`$HOME` /
  `%USERPROFILE%`), while a `~` after the first path component and `~user` stay literal, and an
  entry naming an unknown home is skipped with a warning like any other bad entry.
- `docs/configuration.md:952` (the same rule restated for `--add-dir` and `additional_directories`):
  one sentence pointing at the expansion.
- `docs/tui.md:711` (`/add-dir`): say the path is expanded like `--add-dir`, so `~` works.
- `docs/headless.md:47` links to `tools.md#workspace-roots` and needs no edit.
- No new page, so `docs/SUMMARY.md` and `docs/index.md` are untouched.

## Testing Strategy

- **Unit-level** (`rust/fontaine-sdk/tests/workspace.rs`, home injected directly): `~` expands to
  home; `~/sub` expands to home's `sub` (canonicalized); `a/~/sub` resolves literally under the
  launch directory; `~user/x` is not expanded; with `home: None` a `~` entry is left out with a
  warning saying the home directory is unknown *even when a directory named `~` exists in the launch
  directory* — the fail-closed case. The existing three tests keep their assertions with the home
  passed in.
- **Terminal integration** (`rust/fontaine-tui/tests/tui.rs`): the reported bug — `/add-dir
  ~/shared` in a session whose home is the test world's home directory reports
  `added workspace root <home>/shared`. This is the tier that reproduces the original report: it
  exercises the real command path and the session's own home, not a constructed one.
- **Headless end-to-end** (`rust/fontaine/tests/headless_contract.rs`): `HOME` set into the sandbox,
  `--add-dir ~/shared`, and `init.workspace_roots[1]` is the canonicalized `<sandbox home>/shared`.
  It is the only tier that proves the flag's value reaches `resolve` through the process
  environment (`Host.env` → `home_dir`), which no unit test can reach.
- **Gate**: from `rust/`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`, and the Windows compile check `cargo clippy --all-targets --target
  x86_64-pc-windows-gnu -- -D warnings`, since the separator handling is platform-dependent.

## Open Questions

- Should `~user/x` resolve, as a login shell would?
