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
- Configured lists: `Config.auto_mode: AutoMode` (`rust/fontaine-core/src/config.rs:1301`) is the
  union of every scope's `[auto_mode]` entries. Per-entry origin is not kept, but
  `Config.sources` (`config.rs:1450`) maps each dotted key (`auto_mode.allow`, …) to the `Slot`s
  (managed / cli / project-local / project / user) that set it, highest precedence first.
- `session_state` (`rust/fontaine-core/src/agent/setup.rs:191-227`) is the only place the
  effective lists are built for a session; the prompt is rendered by
  `fontaine_guardian::prompt_builder::system_prompt` from the `guardian_prompt.md` /
  `guardian_minimal.md` assets (`Profile::prompt`).
- `rust/fontaine/src/cli.rs` has no subcommands; `run` (`rust/fontaine/src/lib.rs:231`) parses
  flags, loads config via `load_config`, then starts a headless or interactive session.
- Guardian model resolution: `fontaine_core::models::resolve` yields `Models.guardian`
  (`ModelHandle`, with `DynModel<Completion>`); `build_guardian` wraps it for structured verdicts.

## Design

### CLI shape

Introduce an optional clap subcommand on `Cli`:

```
fontaine auto-mode defaults   [--format text|json]
fontaine auto-mode config     [--format text|json]
fontaine auto-mode critique   [--format text|json]
```

`#[command(subcommand)] command: Option<Command>` with the existing top-level flags kept global
where they already are (`--cd`, `--settings`, `--debug-file`). A subcommand and `-p`/positional
prompt are mutually exclusive: the positional `PROMPT` must not swallow `auto-mode`, so clap's
`args_conflicts_with_subcommands = true` is set. `run` dispatches to a new module
`rust/fontaine/src/auto_mode.rs` right after `apply_cd` and `load_config` (so `--cd` and
`--settings` apply), before the TTY check, trust prompt and session preparation — none of the
subcommands start a session or run project executables.

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
2. Per list, the header names the contributing scopes from `config.sources`
   (`auto_mode.allow  (user, project-local)`) or `shipped defaults` when no scope set it. For
   `hard_deny`/`soft_deny` the output notes that shipped entries are always kept; for
   `environment`/`allow` it notes whether the list replaces the defaults or splices them
   (`$defaults` present).
3. The configured-but-not-merged form (the `$defaults` marker unexpanded) is shown with
   `config` too, marking each entry `default` or `custom`, so users can see which lines are theirs.
   Entries cannot be attributed to an individual scope (the merge keeps only the union); the plan
   deliberately does not add per-entry origin tracking.
4. The `[guardian] profile = "minimal"` case prints a note that the minimal prompt carries a
   reduced ruleset and the rule lists are not sent (`RuleConfig::minimal`).

JSON output mirrors the text: each list is `{ "scopes": [...], "entries": [{"text": ..., "origin": "default"|"custom"}] }`.

### `critique`

Asks the configured guardian model to review the custom entries (those not equal to a shipped
entry) for ambiguity, overbreadth, redundancy with the defaults, and contradictions with the
hard-deny classes. Behavior:

- Exits with a clear message and exit code 0 when there are no custom entries ("nothing to
  critique").
- Resolves the guardian model through the same path a session uses (`models::resolve`, with the
  guardian default selection), so the account, credentials and the "unavailable" cases behave
  identically; a missing guardian model or credential is a config-class error (exit code
  `EXIT_CONFIG`), never a silent skip.
- Sends one plain-text completion (no tools, no structured verdict, same token cap style as the
  guardian) using a new prompt asset `fontaine-guardian/prompts/critique.md`, with the effective
  lists rendered through the existing `bullets` redaction path so credential-shaped text never
  leaves the machine. The custom entries are placed in the user message and treated as untrusted
  text (`neutralize`).
- Prints the model's reply and the cost line; never changes any config. The critique is advisory
  and has no effect on any permission decision (error text may explain, never change, a Deny).
- A timeout (`[guardian] timeout_secs`) or provider error is reported with the error chain
  (`fontaine_tools::error_chain::chain`) and a non-zero exit.

The critique call lives in `fontaine-guardian` as `critique::review(model, config) -> Result<String,
CritiqueError>` so it is embeddable by other harnesses and unit-testable with rig's
deterministic test model.

### Security considerations

- Read-only: no subcommand writes config or touches session state; `defaults` and `config` make
  no network calls.
- Project-scope config is already tighten-only at load; `config` shows the post-merge result, so
  it cannot disagree with what a session enforces (the shared helper above guarantees that).
- Output of `config` includes project-scope text; it is printed to the terminal as data only.
  Terminal control characters in entries are shown as escapes (the same treatment the approval
  rule line uses, #242).
- The critique prompt includes user-authored rule text sent to the guardian account's provider;
  `critique` states which model/account it is about to call before sending.

## Implementation Steps

1. `fontaine-core`: add the shared effective-lists helper on `Config`; switch `session_state`
   (`agent/setup.rs`) to it. Unit test that it equals the previous `expand` composition.
2. `fontaine-guardian`: add `critique.rs` and `prompts/critique.md`; export from `lib.rs`.
3. `fontaine/src/cli.rs`: add `Command::AutoMode(AutoModeArgs)` with the three subcommands and
   `--format`; set `args_conflicts_with_subcommands`.
4. `fontaine/src/auto_mode.rs`: implement the three handlers (text and JSON renderers); wire the
   dispatch into `run` in `lib.rs`.
5. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/config.rs` (helper), `rust/fontaine-core/src/agent/setup.rs`
- `rust/fontaine-guardian/src/lib.rs`, new `src/critique.rs`, new `prompts/critique.md`
- `rust/fontaine/src/cli.rs`, `rust/fontaine/src/lib.rs`, new `rust/fontaine/src/auto_mode.rs`
- `docs/guardian.md`, `docs/configuration.md`, `CHANGELOG.md`
- `research/research-claude-code-parity.md` (flip the row from `missing` to shipped)

## Trade-offs

- **Subcommand vs. TUI slash command only**: the issue asks for a CLI surface mirroring Claude
  Code and it is usable in CI/scripts; a slash command can be added later reusing the renderers.
- **Per-entry origin vs. per-list scopes**: tracking origin per entry means changing the merge in
  `config.rs` for all list keys. Per-list scopes from the existing `sources` map answer "where did
  this come from" well enough at no merge change.
- **`critique` as structured verdict vs. free text**: free text matches Claude Code and the
  output is advisory; a structured verdict would invite treating it as a gate.

## Decisions

(none yet)

## Documentation

- `docs/guardian.md`: new section "Inspecting the rules" covering the three subcommands, with
  the attribution note unchanged.
- `docs/configuration.md`: cross-link from the `[auto_mode]` section.
- `CHANGELOG.md`: an `Added` entry under Unreleased.

## Testing Strategy

- Unit: effective-lists helper equals the session composition for unset, `$defaults`, replace and
  tighten-only cases; text and JSON renderers (golden strings, including control-character
  escaping and the minimal-profile note); custom-entry detection for the critique; critique
  prompt rendering redacts credential-shaped text; `critique::review` with rig's deterministic
  test model for success, provider error and timeout.
- Integration (`rust/fontaine/tests/auto_mode_cli.rs`, driving the binary like `cd_flag.rs`):
  `defaults` output matches `rules.rs`; `config` in a temp project with user and project
  settings shows the right scopes and merged entries; `critique` with no custom entries exits 0
  without a network call; clap parsing keeps `fontaine -p "auto-mode"` a prompt.
- `critique` against a live model is not part of CI (no live APIs in tests); a wiremock provider
  covers the request/response plumbing.

## Open Questions

- Should `critique` also be offered for `[permissions]` rules? Out of scope here; the issue only
  names the guardian lists.
- Should `config` print the fully rendered system prompt (a `--prompt` flag) for debugging? Not
  requested; left out unless the reviewer wants it.
