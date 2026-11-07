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
  `split_arguments` round-trips them; object/number/bool → compact JSON text. Never fails.
- Unknown fields: ignore (drop `deny_unknown_fields`) rather than fail. A failed call costs a
  retry that loses nothing in this tool, and the argument is the only data field; an ignored stray
  key cannot widen authority since the tool reads only skill text. The schema keeps
  `additionalProperties: false` so a compliant model never sends one. No warning channel exists in
  a tool call, so ignoring is silent; it is recorded in Decisions.

### Threading

- `SkillSource::load(&self, name, arguments, agent)` gains an `arguments: &str` parameter;
  `SkillTool::call` passes `args.args`. `SkillRuntime::load` calls
  `expand(name, arguments, agent, true)`. Update `NoSkills` and the existing test call sites.
- Untrusted-input note: the model's string lands in `!cmd` text exactly as a user's `/name args`
  does, and every `!cmd` still runs through `run_command` → the `shell` path and the permission
  pipeline. No new path.

## Implementation Steps

1. `skill.rs`: new `SkillArgs`, `lenient_args`, schema/description update, `load` signature,
   module comment.
2. `fontaine-core/src/skills.rs`: `SkillSource for SkillRuntime` forwards arguments.
3. Update `choke_point.rs` `NoSkills`, `extensions.rs` call sites (`load(name, "", agent)`).
4. Tests (below).
5. Docs, Decisions, CHANGELOG.

## Files to Modify

- `rust/fontaine-tools/src/skill.rs`
- `rust/fontaine-core/src/skills.rs`
- `rust/fontaine-core/tests/choke_point.rs`, `rust/fontaine-core/tests/extensions.rs`
- `docs/extensions.md`
- `tasks/fontaine_harness_plan.md` (Decisions → Extensibility)
- `CHANGELOG.md`

## Trade-offs

- Strict single field name vs aliases: models guess `arguments`/`input`; the reported failure cost
  a retry that dropped the input silently. Aliases are cheap and invisible in the schema.
- Rejecting vs ignoring unknown fields: ignoring chosen, see above.

## Decisions

- 2026-11-07: The `skill` tool takes optional `args` (Claude Code's field) and accepts `skill`,
  `arguments`, `argument`, `input`, `params` as aliases, non-string values coerced to text, and
  ignores unknown fields; deviation from Claude Code's strict schema, reason: models guess the
  field name and a failed call drops the input (#469).

## Documentation

- `docs/extensions.md`: the `skill` tool takes the skill name and optional arguments, substituted
  as for `/name args`; model-supplied arguments reaching a `!` command are still gated as `shell`.
- Add the Decisions line above to the master plan (Extensibility subsection, end).
- CHANGELOG entry.

## Testing Strategy

Unit tests in `skill.rs` (deserialization, no live anything):
- `args` string; each alias (`skill`, `arguments`, `argument`, `input`, `params`); omitted and
  empty → empty string; array of strings (with a whitespace element) → shell-quoted join; object
  and number → JSON text; null → empty; unknown field ignored; missing `name` still an error.
- Pins the reported bug: `{"name":"x","arguments":"tasks/p.md"}` parses with args `tasks/p.md`.

Integration in `fontaine-core/tests/extensions.rs` using the existing skill fixtures:
- `load` with args and a `$ARGUMENTS` placeholder; without placeholder (appended `ARGUMENTS:`
  line); positional `$0`/`$1`; empty args unchanged from today; `disable-model-invocation` skill
  still refused with args.
- A skill with a `!` block containing `$0`, loaded with a model-supplied argument: the command
  that reaches the shell path is the substituted text and goes through the gated runner (use the
  existing recording/approving test double for `run_command`, asserting the pipeline saw it and
  that a denying approver yields `[command failed: ...]`).
- Choke-point test stays green (`skill` remains `ReadOnly`).

## Open Questions

None.
