# Shell Max Timeout Setting Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/267

## Overview

Issue #267 asks that a `shell` command which outlives its timeout keep running as a job the model
controls, rather than be killed. Its items 1-4 and 6 shipped with background shell jobs (#25,
`tasks/completed/background_shell_jobs_plan.md`): foreground by default, promote-on-timeout for the
main conversation, `run_in_background`, `job_output` / `job_cancel` / `monitor`, completion notices
through the result pipeline, and the guardian still judging the launch. One item is left, item 5:
`ShellSettings::max_timeout_secs = 600` is hardcoded, so a user with a long build cannot raise the
ceiling on how long a foreground call blocks (or how long a subagent's command may run). This plan
adds a user-trust `[shell] max_timeout_secs` key, wired to every model-facing `shell` tool. No tool
argument can lift it.

## Current State

- `ShellSettings { max_timeout_secs: 600, .. }` (`rust/fontaine-tools/src/builtins.rs:535-551`)
  feeds `shell_for` (`builtins.rs:637`, `max_timeout: settings.max_timeout_secs` at `:654`).
  `ShellTool::call_job` clamps `timeout_secs` to `1..=max` (`builtins.rs:354-359`) and the
  description quotes `at most {max}` (`:454`). The subagent path is rig's `Shell::call`, which cuts
  to the same `max_timeout`.
- Root calls past the timeout are promoted to a job (`jobs.foreground(.., promote)`,
  `builtins.rs:386`). A job's lifetime has no deadline, so `max_timeout_secs` bounds only how long a
  root call blocks. For a subagent, and for a skill's shell injection (`NoPromote`), it bounds how
  long the command lives.
- `rust/fontaine-core/src/tools.rs:134-139` builds `ShellSettings` with
  `..ShellSettings::default()`, so the config never reaches it. `RegistryParams`
  (`tools.rs:79`) is filled in `rust/fontaine-core/src/agent/build.rs:170`, and also in
  `rust/fontaine-core/tests/choke_point.rs:146` and `rust/fontaine-core/tests/extensions.rs:317`.
- `!` mode (`rust/fontaine-core/src/user_shell.rs:36-50`) calls `ShellTool::run` with no
  `timeout_secs`, so it always uses the 120 s default. The cap has nothing to cut there.
- Config: sections are `Option<…Section>` on the settings struct (`rust/fontaine-core/src/config.rs`,
  e.g. `context: Option<ContextSection>` at `:806`, validated at `:1150`, merged at `:1833`).
  Scope policy is `RULES` in `rust/fontaine-core/src/config_scope.rs:139`; `[goal]` is
  `Rule::user_or_cli` with a reason const. Every settings leaf must have a `/config` catalog row
  (`config_catalog.rs` `every_settings_leaf_has_a_row`, `:559`).
- `docs/tools.md:167` says "a longer request is cut to 600 seconds, and no argument lifts that cap".
- Claude Code: `BASH_MAX_TIMEOUT_MS` (default 600000) is an environment setting the user owns, and
  the model's `timeout` is cut to it.

## Design

### Config

```toml
[shell]
max_timeout_secs = 1800
```

- `config.rs`: `ShellSection { max_timeout_secs: Option<u64> }` with `deny_unknown_fields`, held as
  `shell: Option<ShellSection>` on the settings struct. The merged config gains
  `shell: Shell { max_timeout_secs: u64 }`, default `600`. Validation: `1..=86_400`, else
  `bad("shell.max_timeout_secs", "must be from 1 to 86400")`. The upper bound keeps
  `Instant + Duration` far from overflow and is still a full day.
- `config_scope.rs`: `Rule::user_or_cli("shell", SHELL)` with
  `SHELL = "how long a model's shell command may block or run is set only in the user file or --settings"`.
  Like `[goal]`: a shared repository must not stretch how long commands run. Project-local stays
  out too, which keeps the rule simple and matches the issue's "user-scope setting".
- `config_catalog.rs`: a `shell.max_timeout_secs` row, `Edit::PositiveInt`, `NextLaunch`. The value
  is read when the registry is built.

### Wiring

- `RegistryParams` gains `shell_max_timeout_secs: u64`, and `registry` sets
  `ShellSettings.max_timeout_secs` from it. `agent/build.rs` passes `p.config.shell.max_timeout_secs`.
  The two test call sites pass `600`.
- Nothing else changes. The clamp, the description's `at most {max}`, rig's own cut for subagents,
  and `timed_out_line(.., max, ..)` already read `self.shell.max_timeout`. `!` mode keeps
  `ShellSettings::default()`, since it never passes a timeout.
- `DEFAULT_TIMEOUT.min(max)` already lowers the 120 s default when the user sets a smaller cap.

### What the cap means (no behavior change beyond the value)

- Main conversation: how long a call blocks before it is promoted. A job then lives until it ends,
  is cancelled, or the session ends.
- Subagent and skill injection: how long the command runs before it is stopped. This kill-at-timeout
  is kept by the 2026-10-27 decision that jobs are root-only.

## Implementation Steps

1. `rust/fontaine-core/src/config.rs`: `ShellSection`, the merged `Shell` (default 600), validation,
   merge. Unit tests: parse, default, `0` and `86401` rejected, unknown key rejected.
2. `rust/fontaine-core/src/config_scope.rs`: rule and reason const; add `("shell", "[shell]\nmax_timeout_secs = 900\n", None)`
   to the scope-table test list near `:520`.
3. `rust/fontaine-core/src/config_catalog.rs`: the row.
4. `rust/fontaine-core/src/tools.rs`, `agent/build.rs`, `tests/choke_point.rs`, `tests/extensions.rs`:
   thread `shell_max_timeout_secs`.
5. Tests (see Testing Strategy).
6. Docs (see Documentation).

## Files to Modify

- `rust/fontaine-core/src/config.rs`, `config_scope.rs`, `config_catalog.rs`, `tools.rs`,
  `agent/build.rs`
- `rust/fontaine-core/tests/choke_point.rs`, `tests/extensions.rs`
- `rust/fontaine-tools/src/builtins.rs`: tests only
- `docs/tools.md`, `docs/configuration.md`, `research/research-claude-code-parity.md`, `CHANGELOG.md`

## Trade-offs

- **Only `max_timeout_secs`, not also a default-timeout key** (Claude Code has
  `BASH_DEFAULT_TIMEOUT_MS`). The issue asks for the cap, and promote-on-timeout already makes the
  120 s default harmless in the main conversation. A default key is easy to add later.
- **A config key, not an environment variable.** Fontaine's settings live in TOML with per-key scope
  policy. An env var would bypass that policy.
- **`user_or_cli`, not `user_trust`.** Raising the cap lengthens subagent commands that run
  unattended, which matches `[goal]`'s reason. `--settings` has user trust.

## Documentation

- `docs/tools.md` `## Shell` Timeout bullet: "a longer request is cut to `[shell] max_timeout_secs`
  (default 600 seconds), and no argument lifts that cap". Link to the configuration key.
- `docs/configuration.md`: a `### [shell]` key-reference section with an example, and a per-key
  scope table row `[shell] max_timeout_secs | no | no | yes`.
- `research/research-claude-code-parity.md`: note on the background Bash row that the cap is
  `[shell] max_timeout_secs` (Claude Code: `BASH_MAX_TIMEOUT_MS`).
- `CHANGELOG.md` entry.

## Testing Strategy

Unit tests (no live API):

- Config: the parse, default, bounds and unknown-key tests from step 1. Project and project-local
  files setting `[shell]` are errors that name the key and the scope; `--settings` is accepted
  (scope-table test).
- Wiring (`fontaine-core`): `registry` built with `shell_max_timeout_secs: 900` produces a `shell`
  tool whose description says `at most 900`. This is the check that the value is not dropped at
  `..ShellSettings::default()`, which is the bug today.
- `builtins.rs` (cfg(unix), real `sh`): with `max_timeout_secs = 1`, a root call with
  `timeout_secs: 30` on `sleep 3; echo done` is promoted after about 1 s, not 30 s. A subagent call
  with the same arguments is stopped and its line names the 1 s cap. Both show that the model's
  argument cannot lift the configured cap.
- Catalog: `every_settings_leaf_has_a_row` stays green.

## Decisions

- 2026-10-30: Items 1-4 and 6 of #267 shipped with #25. This plan covers only item 5.
- 2026-10-30: A subagent's and a skill injection's `shell` keep kill-at-timeout, per the
  2026-10-27 master-plan decision that jobs are root-only. That decision is not reopened here.
- 2026-10-30: Promotion past the 8-running-jobs cap still stops the command (`Kill::Cap`). This is
  a resource bound that the result names, not the silent kill-on-timeout the issue reports.
