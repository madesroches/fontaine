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
    pub name: String,
    #[serde(default, alias = "arguments", alias = "argument", alias = "input", alias = "params",
            deserialize_with = "lenient_args")]
    pub args: String,
}
```

- `args` is canonical (Claude Code's name); schema and description advertise only `args` and
  `name`. Aliases are accepted silently on input only.
- Schema description of `args`: "Arguments for the skill, as a single string; shell-style quoting
  groups words. Omit when the skill takes none." Tool description gains the sentence: "Pass
  optional `args` to give the skill arguments."
- `lenient_args` coerces: string → as is; null → empty; array → each element stringified (strings
  as is, others as JSON), joined by a space. `split_arguments` has no escape character, so an element that is empty or
  contains whitespace or a quote character is wrapped in the quote character it does not contain
  (an element containing both kinds of quote is best-effort and may split differently); object/number/bool → compact JSON text. Never fails on the
  value's type.
- Unknown fields stay rejected and duplicate spellings of `args` are rejected; see Decisions.

### Threading

- `SkillSource::load(&self, name, arguments, agent)` gains an `arguments: &str` parameter;
  `SkillTool::call` passes `args.args`. the `SkillSource` impl for
  `SkillRuntime` calls `expand(name, arguments, agent, true)`. Update `NoSkills` and the existing test call sites.
- Model-supplied args can form `!` commands exactly as `/name args` does today; see the `AFK:`
  Decisions line.

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

## Decisions

- 2026-11-07: The `skill` tool takes optional `args` (Claude Code's field) and accepts
  `arguments`, `argument`, `input`, `params` as aliases, non-string values coerced to text;
  deviation from Claude Code's strict schema, reason: models guess the field name and a failed
  call drops the input (#469).
- 2026-11-07 AFK: unknown fields stay rejected (`deny_unknown_fields`), as in every tools args struct; a
  silently dropped misnamed field would recreate the bug.
- 2026-11-07 AFK: `skill` is NOT accepted as an alias for `name` because permission rules match on the raw
  `name` key (`name_arg_key`); the `args` aliases are unaffected since rules never read them.
- 2026-11-07 AFK: a payload with two spellings of one field is rejected with serde's "duplicate field" error.
- 2026-11-07 AFK: model args may form `!` commands (`substitute` then `parts`, as for `/name args`); kept
  unchanged and documented, still gated as `shell`.
- 2026-11-07 AFK: skill index lines are not changed to carry `argument-hint`; the model learns of arguments
  from the tool description and the user/caller supplying them.

## Documentation

- `docs/extensions.md`: the `skill` tool takes the skill name and optional arguments, substituted
  as for `/name args`; model-supplied arguments can form `!` commands, which are still gated as
  `shell` (`project_commands` trust, subagent shell check); `lend_rules` grants of a user-scope
  skill can apply to them.
- `docs/tools.md` (lines ~27, ~126-128): the `skill` tool takes a name and optional `args`, not
  only a name.
- `docs/permissions.md` (~82, ~262, ~370): "A skill's `!` lines run through `shell` only" gains a
  note that this includes commands formed from model-supplied arguments.
- Copy these Decisions lines to the master plan's Extensibility subsection (end of subsection).
- CHANGELOG entry.

## Testing Strategy

Unit tests in `skill.rs` (deserialization, no live anything):
- `args` string; each alias (`arguments`, `argument`, `input`, `params`); omitted and
  empty → empty string; array of strings (with a whitespace element and an empty-string element) → shell-quoted join; object
  and number → JSON text; null → empty; unknown field rejected (including `{"skill": ...}`); both `args` and `arguments` rejected as duplicate field; missing `name` still an error.
- Pins the reported bug: `{"name":"x","arguments":"tasks/p.md"}` parses with args `tasks/p.md`.

Integration in `fontaine-core/tests/extensions.rs` using the existing skill fixtures:
- One `load("name", "alpha \"b c\"", ROOT_AGENT)` test on an existing fixture asserting the
  substituted text (proves `load` forwards args with `by_model = true`), plus one assertion that a
  `disable-model-invocation` skill is still refused when args are given. Substitution, `!` gating
  and denial are already covered by the `substitute` unit tests in `fontaine-core/src/skills.rs`,
  `arguments_are_substituted_before_the_commands_run` and
  `a_denied_skill_command_is_replaced_by_the_denial_and_the_skill_still_loads`.
- Choke-point test stays green (`skill` remains `ReadOnly`).

## Open Questions

None.
