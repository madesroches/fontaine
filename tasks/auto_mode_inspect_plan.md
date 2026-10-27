# `fontaine auto-mode` Inspection Subcommand Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/1

## Overview

Add `fontaine auto-mode defaults | config | critique`, the counterpart of Claude Code's
`claude auto-mode defaults|config|critique`. `defaults` prints the shipped environment and rule
lists, `config` prints the effective lists a session would hand the guardian (after `$defaults`
splicing and the tighten-only rules) with the scope that contributed each list, and `critique`
asks the configured guardian model to review the user's custom entries. The guardian's prose is
open, so this is cheap, and it lets users see exactly what the guardian judges against.

## Current State

- Shipped lists: `DEFAULT_ENVIRONMENT`, `DEFAULT_HARD_DENY`, `DEFAULT_SOFT_DENY`, `DEFAULT_ALLOW`
  in `rust/fontaine-guardian/src/rules.rs`. `expand`, `expand_keeping_defaults`,
  `EnvironmentConfig::expand` and `RuleConfig::expand` turn configured lists into the effective
  ones (`hard_deny`/`soft_deny` always keep the defaults; `environment` and `allow` replace them
  unless `$defaults` is present).
- Configured lists: `Config.auto_mode: AutoMode` (`rust/fontaine-core/src/config.rs:1301`), held in `Config.auto_mode` (`config.rs:1463`), is the
  union of every scope's `[auto_mode]` entries. Per-entry origin is not kept, but
  `Config.sources` (`config.rs:1450`) maps each dotted key (`auto_mode.allow`, …) to the `Slot`s
  (managed / cli / project-local / project / user) that set it, highest precedence first.
- `session_state` (`rust/fontaine-core/src/agent/setup.rs:187-227`) is the only place the
  effective lists are built for a session; the prompt is rendered by
  `fontaine_guardian::prompt_builder::system_prompt` from the `guardian_prompt.md` /
  `guardian_minimal.md` assets (`Profile::prompt`).
- `rust/fontaine/src/cli.rs` has no subcommands; `run` (`rust/fontaine/src/lib.rs:231`) parses
  flags, loads config via `load_config`, then starts a headless or interactive session.
- Guardian model resolution: `fontaine_core::models::resolve` yields `Models.guardian`
  (an `Option<GuardianModel>`: `Ready(ModelHandle)` or `Unavailable { reference, reason }`;
  the handle carries the `DynModel<Completion>`, the thinking-off `additional_params` and
  `catalog_billed()`); `build_guardian` wraps it for structured verdicts.

## Design

### CLI shape

Introduce an optional clap subcommand on `Cli`:

```
fontaine auto-mode defaults   [--format text|json]
fontaine auto-mode config     [--format text|json]
fontaine auto-mode critique   [--format text|json]
```

`#[command(subcommand)] command: Option<Command>` with the existing `global = true` flags
(`--cd`, `--settings`, `--debug-file`) applying to the subcommand. `args_conflicts_with_subcommands`
is not set: it makes any matched arg, including a global one, stop clap recognising the subcommand.
The prompt is a plain optional positional and `-p` a bool flag, so clap matches a positional
against the subcommand names first: both `fontaine auto-mode` and `fontaine -p "auto-mode"`
parse as the subcommand. A literal prompt "auto-mode" is written `fontaine -p -- auto-mode`.
Session-only flags (`--model`, `-p`, …) are ignored with a subcommand. `run` dispatches to a new
module `rust/fontaine/src/auto_mode.rs` right after `apply_cd` and `load_config` (so `--cd` and
`--settings` apply), before the TTY check, trust prompt and session preparation — none of the
subcommands start a session or run project executables. `check_session_flags` runs earlier, so
it still rejects its own misuse (an invalid `--name`, `--fork-session` without `--continue` or
`--resume`, `--session-id` with `--continue`/`--resume`) alongside a subcommand; every other
session flag is ignored.

### `defaults`

Prints the shipped lists as written in `rules.rs`, grouped `environment`, `allow`, `soft_deny`,
`hard_deny`, one entry per item, in a form that pastes straight into `[auto_mode]` TOML arrays
by default (`--format text`) so a user can copy and edit them; `--format json` prints
`{"environment": [...], "allow": [...], "soft_deny": [...], "hard_deny": [...]}`. Needs no config.

### `config`

Prints, per list, the effective entries a session would use and where the list came from:

1. Library support in `fontaine-guardian`: none new — reuse `EnvironmentConfig::expand` and
   `RuleConfig::expand`. To avoid the subcommand and `session_state` building the lists in two
   places (DRY), extract one function in `fontaine-core` (e.g. `Config::effective_auto_mode()
   -> (EnvironmentConfig, RuleConfig)`) and call it from both `session_state` and the subcommand.
2. Only the effective entries are printed, each tagged `default` or `custom` by equality with a
   shipped string. Per list, the header names the contributing scopes from `config.sources`
   (`auto_mode.allow  (user, project-local)`) or `shipped defaults` when no scope set it, and
   states the list semantics: `hard_deny`/`soft_deny` always keep the shipped entries;
   `environment`/`allow` replace them unless `$defaults` is present, in which case it splices them.
3. The `[guardian] profile = "minimal"` case prints a note that the minimal prompt carries a
   reduced ruleset and the rule lists are not sent (`RuleConfig::minimal`).

JSON output mirrors the text: each list is `{ "scopes": [...], "entries": [{"text": ..., "origin": "default"|"custom"}] }`.

### `critique`

Asks the configured guardian model to review the custom entries (those not equal to a shipped
entry) for ambiguity, overbreadth, redundancy with the defaults, and contradictions with the
hard-deny classes. Behavior:

- Checks for custom entries first, before any model or credential resolution: with none it exits 0
  with "nothing to critique", needing no model.
- Resolves the guardian model through the same path a session uses (`models::resolve`, with the
  guardian default selection), so the account, credentials and the "unavailable" cases behave
  identically. That path also requires a main model (`no main model` otherwise) and may make
  its account-listing call; this is accepted, and only the no-custom-entries path is guaranteed
  to make zero requests. `critique` matches on `GuardianModel`: `Unavailable` and an absent
  `Models.guardian` are `EXIT_CONFIG`, never a silent skip.
- Sends one plain-text completion (no tools, no structured verdict) through a plain rig
  completion/agent prompt on the guardian's `DynModel`: guardian.rs and scanner.rs use
  `ExtractorBuilder`, so no plain-text path exists yet. The token cap reuses the private
  `MAX_TOKENS` (1_024) in `fontaine-guardian/src/guardian.rs`, exposed for this call; no new
  config key. The call takes the whole `ModelHandle`'s model and `thinking_off` params, as the
  guardian does. It uses a new prompt asset `fontaine-guardian/prompts/critique.md`, with the effective
  lists rendered through the existing `bullets` redaction path (private in
  `prompt_builder.rs`; made `pub(crate)` for `critique.rs`) so credential-shaped text never
  leaves the machine. The custom entries are placed in the user message and treated as untrusted
  text (`neutralize`). The prompt names which shipped `environment`/`allow` entries a custom list
  replaced (no `$defaults`) and states that `hard_deny`/`soft_deny` always keep theirs.
- With `[guardian] profile = "minimal"` the rule lists are not sent to the guardian, so it prints
  a note saying so and exits 0 without a call.
- Prints the model's reply and the cost line (text mode), or both in the JSON object; the cost is
  the returned `Usage.cost` when the handle is `catalog_billed()` (the session's own rule for
  dropping an unpriced model's cost), otherwise token counts only. Never changes any config. The critique is advisory
  and has no effect on any permission decision (error text may explain, never change, a Deny).
- A timeout (`[guardian] timeout_secs`) or provider error is reported with the error chain
  (`fontaine_tools::error_chain::chain`) and a non-zero exit.

The critique call lives in `fontaine-guardian` as `critique::review(handle, config) -> Result<(String, Usage),
_>` (a chain-preserving error, not a typed enum: no caller dispatches on its kind) so it is embeddable by other harnesses and unit-testable with rig's
deterministic test model.

### Security considerations

- Read-only: no subcommand writes config or touches session state; `defaults` and `config` make
  no network calls.
- At load, project scope may not set `auto_mode.environment`/`allow` without user trust, while
  `soft_deny`/`hard_deny` are accepted from project scope (add only, `config_scope.rs:168-171`);
  `config` shows the post-merge result, so
  it uses the same merge and helper a session does. Dispatch runs before the trust prompt, so
  `config` shows what an untrusted or non-interactive load yields, which can differ from a trusted
  interactive session; `Config.sources` is built from raw layer tables, so it may list a project
  scope whose `environment`/`allow` the scope rule dropped.
- Output of `config` includes project-scope text; it is printed to the terminal as data only.
  In text mode terminal control characters in entries are shown as escapes: the private
  `display_rule` (`rust/fontaine-tui/src/approval.rs:289`) moves to a shared crate
  (`fontaine-tools` or `fontaine-core`). JSON output needs none, since serde_json escapes them.
- The critique prompt includes user-authored rule text sent to the guardian account's provider;
  `critique` states which model/account it is about to call on stderr before sending, and carries
  the cost or token counts in the JSON object. Project-scope `soft_deny`/`hard_deny` text goes to the guardian
  provider with no extra prompt, the same exposure a session's guardian prompt already has.

## Implementation Steps

1. `fontaine-core`: add the shared effective-lists helper on `Config`; switch `session_state`
   (`agent/setup.rs`) to it. Unit test that it equals the previous `expand` composition.
2. `fontaine-guardian`: add `critique.rs` and `prompts/critique.md`; export from `lib.rs`.
3. `fontaine/Cargo.toml`: add `fontaine-guardian.workspace = true` (the bin crate depends only on core,
   tools, tui; core does not re-export guardian types).
   `fontaine/src/cli.rs`: add `Command::AutoMode(AutoModeArgs)` with the three subcommands and
   `--format`. Test: parse `fontaine --cd X auto-mode config` (cd_flag.rs style).
4. `fontaine/src/auto_mode.rs`: implement the three handlers (text and JSON renderers); wire the
   dispatch into `run` in `lib.rs`. Tests: renderer goldens; integration tests in
   `rust/fontaine/tests/auto_mode_cli.rs` (see Testing Strategy).
5. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/config.rs` (helper), `rust/fontaine-core/src/agent/setup.rs`
- `rust/fontaine-guardian/src/lib.rs`, new `src/critique.rs`, new `prompts/critique.md`
- `rust/fontaine/Cargo.toml` (add `fontaine-guardian.workspace = true`)
- `rust/fontaine/src/cli.rs`, `rust/fontaine/src/lib.rs`, new `rust/fontaine/src/auto_mode.rs`
- `docs/guardian.md`, `docs/configuration.md`, `docs/headless.md`, `CHANGELOG.md`
- `research/research-claude-code-parity.md` (flip the row from `missing` to shipped)

## Trade-offs

- **Subcommand vs. TUI slash command only**: the issue asks for a CLI surface mirroring Claude
  Code and it is usable in CI/scripts; a slash command can be added later reusing the renderers.
- **Per-entry origin vs. per-list scopes**: per-entry origin means changing the merge in
  `config.rs` for all list keys.
- **`critique` as structured verdict vs. free text**: a structured verdict would invite treating
  it as a gate.

## Decisions

- 2026-10-27: a prompt that is exactly a subcommand name (`auto-mode`) runs the subcommand, with
  or without `-p`; `--` is the escape for a literal prompt.
- 2026-10-27: `config` records scopes per list, not per entry; no merge change.
- 2026-10-27: `critique` output is free-text advisory; no structured verdict.
- 2026-10-27: `--format text|json` is offered on all three subcommands; not verified against
  `claude auto-mode`.

- 2026-10-27: AFK: `fontaine` takes a direct `fontaine-guardian.workspace = true` dependency for the guardian types `auto_mode.rs` needs, rather than re-exporting them through `fontaine-core` (no new public API surface; the workspace dependency already exists).

## Documentation

- `docs/headless.md`: a row in the "Command line" table for `fontaine auto-mode …`.
- `docs/guardian.md`: new section "Inspecting the rules" covering the three subcommands, with
  the attribution note unchanged.
- `docs/configuration.md`: cross-link from the `[auto_mode]` section.
- `CHANGELOG.md`: an `Added` entry under Unreleased.

## Testing Strategy

- Unit: effective-lists helper equals the session composition for unset, `$defaults`, replace and
  tighten-only cases; text and JSON renderers (golden strings, including control-character
  escaping and the minimal-profile note); critique
  prompt rendering redacts credential-shaped text; `critique::review` with rig's deterministic
  test model for success, provider error and timeout; custom-entry detection (tested once, here).
- Integration (`rust/fontaine/tests/auto_mode_cli.rs`, driving the binary like `cd_flag.rs`):
  `defaults` output matches `rules.rs`; `config` in a temp project with user and project
  settings shows the right scopes and merged entries; `critique` with no custom entries exits 0
  and zero requests reached the wiremock endpoint (`Endpoint::requests()` in
  `rust/fontaine/tests/common/mod.rs`); `fontaine --cd X auto-mode config` parses; `fontaine -p
  -- auto-mode` stays a prompt while `fontaine -p "auto-mode"` runs the subcommand. `Sandbox::headless` adds `-p`, so subcommand
  tests invoke `run` directly.
- Tiers: rig's deterministic test model covers the library (`critique::review`); the wiremock
  `Endpoint` covers the binary's request/response plumbing. No live APIs in tests.
