# Skill Tool Arguments Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/469

## Overview

The model-invoked `skill` tool takes only `name`, so a call like
`skill {"name": "...", "arguments": "tasks/plan.md"}` fails to parse
(`unknown field 'arguments'`), the model retries without it, and the skill loads without the input
it was meant to act on. Give the tool an optional argument string, threaded into the existing
`substitute`, so a model-invoked skill behaves exactly like `/name args`, and accept the field
names models are likely to guess.

## Current State

- `rust/fontaine-tools/src/skill.rs`: `SkillArgs { name }` with `deny_unknown_fields`; schema has
  `additionalProperties: false`; `SkillSource::load(name, agent)` has no arguments parameter.
- `rust/fontaine-core/src/skills.rs`: `SkillRuntime::expand(name, arguments, agent, by_model)`
  already substitutes arguments (`substitute`: `$ARGUMENTS`, `$ARGUMENTS[N]`, `$N`, appended
  `ARGUMENTS: <args>` when the body has no placeholder), then runs `!cmd` parts through
  `run_command`. The `SkillSource` impl passes `""`. Only the `/name args` path supplies arguments.
- Other `SkillSource` implementors: `NoSkills` in `fontaine-core/tests/choke_point.rs`. Callers of
  `runtime.load` in `fontaine-core/tests/extensions.rs` (lines ~510–587).
- `docs/extensions.md` (line ~99) says the tool's "only argument is the skill's name"; the
  `skill.rs` module comment says the same. The plan's Decisions has no entry for this.
- Claude Code's `Skill` tool takes `skill` plus an optional `args` string.

## Design

### Argument type (`fontaine-tools/src/skill.rs`)

```
pub struct SkillArgs {
    #[serde(alias = "skill")]
    pub name: String,
    #[serde(default, alias = "arguments", alias = "argument", alias = "input", alias = "params",
            deserialize_with = "lenient_args")]
    pub args: String,
}
```

- `args` is canonical (Claude Code's name); schema and description advertise only `args` and
  `name`. Aliases are accepted silently on input only.
- `lenient_args` coerces: string → as is; null → empty; array → each element stringified (strings
  as is, others as JSON), joined by a space with elements containing whitespace double-quoted so
  `split_arguments` round-trips them; object/number/bool → compact JSON text. Never fails on the
  value's type.
- Unknown fields: keep `deny_unknown_fields`, as every `fontaine-tools` args struct does; a
  silently dropped misnamed field would recreate this bug. Recorded in Decisions.
- Aliases are the same serde field, so a payload carrying two spellings (`skill` and `name`, or
  `args` and `arguments`) is rejected with serde's "duplicate field" error; no custom merge.

### Threading

- `SkillSource::load(&self, name, arguments, agent)` gains an `arguments: &str` parameter;
  `SkillTool::call` passes `args.args`. `SkillRuntime::load` calls
  `expand(name, arguments, agent, true)`. Update `NoSkills` and the existing test call sites.
- Untrusted-input note: `expand` runs `substitute` then `parts`, so model-supplied args can form
  `!` commands (an inline `!`cmd`` or a line-start ```! block), exactly as a user's `/name args`
  does today. Every such command still runs through `run_command` → the `shell` path, gated by
  `project_commands` trust and the subagent shell check. `lend_rules` grants of a user-scope skill
  can apply to commands formed this way. Behavior is unchanged and documented.

## Implementation Steps

1. `skill.rs`: new `SkillArgs`, `lenient_args`, schema/description update, `load` signature,
   module comment, and `SkillTool::call` (the only production caller of `SkillSource::load`)
   passing `args.args`.
2. `fontaine-core/src/skills.rs`: `SkillSource for SkillRuntime` forwards arguments.
3. Update `choke_point.rs` `NoSkills`, `extensions.rs` call sites (`load(name, "", agent)`).
4. Tests (below).
5. Docs, Decisions, CHANGELOG.

## Files to Modify

- `rust/fontaine-tools/src/skill.rs`
- `rust/fontaine-core/src/skills.rs`
- `rust/fontaine-core/tests/choke_point.rs`, `rust/fontaine-core/tests/extensions.rs`
- `docs/extensions.md`, `docs/tools.md`, `docs/permissions.md`
- `tasks/fontaine_harness_plan.md` (Decisions → Extensibility)
- `CHANGELOG.md`

## Trade-offs

- Strict single field name vs aliases: models guess `arguments`/`input`; the reported failure cost
  a retry that dropped the input silently. Aliases are cheap and invisible in the schema.
- Rejecting vs ignoring unknown fields: rejecting kept, see above.

## Decisions

- 2026-11-07: The `skill` tool takes optional `args` (Claude Code's field) and accepts `skill`,
  `arguments`, `argument`, `input`, `params` as aliases, non-string values coerced to text;
  deviation from Claude Code's strict schema, reason: models guess the field name and a failed
  call drops the input (#469).
- AFK: unknown fields stay rejected (`deny_unknown_fields`), as in every tools args struct; a
  silently dropped misnamed field would recreate the bug.
- AFK: a payload with two spellings of one field is rejected with serde's "duplicate field" error.
- AFK: model args may form `!` commands (`substitute` then `parts`, as for `/name args`); kept
  unchanged and documented, still gated as `shell`.

## Documentation

- `docs/extensions.md`: the `skill` tool takes the skill name and optional arguments, substituted
  as for `/name args`; model-supplied arguments can form `!` commands, which are still gated as
  `shell` (`project_commands` trust, subagent shell check); `lend_rules` grants of a user-scope
  skill can apply to them.
- `docs/tools.md` (lines ~27, ~126-128): the `skill` tool takes a name and optional `args`, not
  only a name.
- `docs/permissions.md` (~82, ~262, ~370): "A skill's `!` lines run through `shell` only" gains a
  note that this includes commands formed from model-supplied arguments.
- Add the Decisions line above to the master plan (Extensibility subsection, end).
- CHANGELOG entry.

## Testing Strategy

Unit tests in `skill.rs` (deserialization, no live anything):
- `args` string; each alias (`skill`, `arguments`, `argument`, `input`, `params`); omitted and
  empty → empty string; array of strings (with a whitespace element) → shell-quoted join; object
  and number → JSON text; null → empty; unknown field rejected; both `skill` and `name` (or `args` and `arguments`) rejected as duplicate
  field; missing `name` still an error.
- Pins the reported bug: `{"name":"x","arguments":"tasks/p.md"}` parses with args `tasks/p.md`.

Integration in `fontaine-core/tests/extensions.rs` using the existing skill fixtures:
- `load` with args and a `$ARGUMENTS` placeholder; without placeholder (appended `ARGUMENTS:`
  line); positional `$0`/`$1`; empty args unchanged from today; `disable-model-invocation` skill
  still refused with args.
- A skill with a `!` block containing `$0`, loaded with a model-supplied argument: the command
  that reaches the shell path is the substituted text and goes through the gated runner (use the
  existing recording/approving test double for `run_command`, asserting the pipeline saw it and
  that a denying approver yields `[command failed: ...]`).
- A skill whose body contains an inline `!`cmd`` fed by a model-supplied argument, using the
  `skills()` helper (extensions.rs ~342-383): with a denying approver the command is gated as
  `shell` and the result is `[command failed:`.
- Choke-point test stays green (`skill` remains `ReadOnly`).

## Open Questions

None.
