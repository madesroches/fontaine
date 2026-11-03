# Trusted Skills Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/379

## Overview

A plugin skill's `allowed-tools` is ignored today (plugin content is tighten-only), and in `auto` a
user-directory skill's broad grant such as `Bash(git *)` is set aside. A user who has decided to
trust a skill has no way to say so, so unattended runs stall on steps the skill documents (#361).
This plan adds an explicit, user-scope trust record for a skill, pinned to a digest of the skill
file. While the record matches, the skill's `allowed-tools` are honoured: narrow rules act as real
allows, and in `auto` broad rules reach the guardian as context instead of as an allow. A changed file
suspends the grants until the user acknowledges the new version interactively.

## Current State

- `load_skill` (`rust/fontaine-core/src/extensions.rs:~243`) fills `Skill.allowed_tools` only for
  `Scope::User`; project and plugin skills get an empty list after the entries are validated.
- `SkillRuntime::expand` (`rust/fontaine-core/src/skills.rs:~249`) calls
  `SessionState::grant_skill_rules(agent, rules)` (`gate.rs:~672`); grants live in `skill_grants`,
  per invoking agent, and `end_turn` clears them. Subagents hold none.
- `permissions.rs:~937` evaluates `rules.allow` chained with the agent's skill grants, after the
  floor, deny/ask rules and hooks. In `auto`, `is_broad_code_execution` rules are skipped
  (`suspended`), skill grants included. `suspended_allow_rules` (`gate.rs:~720`) reports only
  `rules.allow`, not skill grants.
- Plugin skills are named `<plugin>:<name>` (`namespaced`, `extensions.rs:~570`). The registry
  `plugins.toml` (`plugins.rs`, `Registry`, `deny_unknown_fields`, owner-only, written under
  `file_lock` by `fontaine plugin ...`) holds each plugin's pin and executables fingerprint.
- The executables fingerprint (`approval_fingerprint`) deliberately excludes skill prose, so a
  trusted plugin skill's body can change through auto-update with no approval step.
- The guardian request (`AdjudicationRequest`, `fontaine-guardian/src/request.rs`;
  `adjudication::build`) carries `session_facts`, `hook_context` and intent; nothing about the
  active skill.
- Docs: `docs/extensions.md` (`allowed-tools`), `docs/plugins.md` ("What install approves"),
  `docs/permissions.md`, `docs/guardian.md`. The master plan's Decisions (Extensibility, 2026-10-21
  and 2026-10-28) record the project and plugin skill deviation.

## Design

### Trust record

A user-scope file `skill-trust.toml` in the config directory, beside `plugins.toml`: written only
by the `fontaine plugin trust` CLI and the TUI acknowledgement, owner-only, under `file_lock`, not a
settings file (no scope stack, nothing in project scope can read it into a grant). A corrupt file
loads nothing and warns once, like `plugins.toml`; `trust`/`untrust` error on a corrupt file
(mirroring `read_registry`) rather than overwrite it.

```toml
[[skill]]
name = "dev-skills:pr"          # composed skill name
digest = "sha256:9f2c..."      # SHA-256 of the skill file bytes (frontmatter and body)
tools = ["Bash(git *)"]        # the trusted version's declared allowed-tools
commands = ["!git status"]     # the trusted version's `!` lines
```

Granularity is per skill.
`trust <plugin>` is shorthand that records every skill the installed plugin has at that moment; a
skill added later is untrusted until trusted.

The digest covers the raw bytes of the file the loader reads, so `allowed-tools`, the body and its
`!` lines are all pinned. Commands (`commands/*.md`) are skills too and are trusted the same way.

### Which skills can be trusted

`Scope::User` and `Scope::Plugin` skills. A `Scope::Project` skill can never be trusted: a record
for a project skill's name is ignored with a warning. A user-directory skill needs no trust to lend
its rules (unchanged: all of them, broad included); trust for it additionally records the guardian
context note below. A plugin skill lends nothing without trust.

### Loading

`load_skill` parses `allowed-tools` for every scope and stores it in a new
`Skill.declared_tools: Vec<Rule>` (validated as today). `Skill.allowed_tools` keeps its meaning,
the rules lent without trust. `Extensions` carries a `TrustedSkills` view (the parsed
`skill-trust.toml`, loaded at startup next to the plugins through `LoadParams`, which also receives
the project-skill-record warning) and `expand` branches on `trust_state`:

```
trust_state(skill) =
  Project scope                    -> Untrusted
  record absent                    -> Untrusted
  record digest != file digest     -> Stale   (plugin skills: grants off; a user skill falls back to today's untrusted
                                             behaviour; notice once per session)
  record digest == file digest     -> Trusted
```

The digest is computed on load from the same bytes the loader parsed, not re-read at `expand`, so
it cannot differ from the body that is run.

### Grants

For a `Trusted` skill, `expand` passes every declared rule to the existing `grant_skill_rules`: real
allows for the turn, after the floor, deny/ask rules and hooks. The existing `auto` suspension of
broad rules (`is_broad_code_execution`, `permissions.rs:~935-946`) applies unchanged, so outside
`auto` broad rules allow and in `auto` they do not. Additionally, the broad declared rules
(`Bash(git *)`, bare `Bash`) are recorded through a new
`SessionState::note_trusted_skill(agent, TrustedSkillNote { name, declared })`; the narrow/broad
split in `expand` only decides what goes into the note. `permissions.rs` is unchanged;
`adjudication::build` reads the notes from `SessionState`.

- A `Scope::User` skill that is untrusted keeps today's behaviour exactly: all its rules lent, no
  note. A `Stale` user skill falls back to that same behaviour.
- A `Stale` or untrusted plugin skill lends nothing, and a stale one raises a notice telling the
  user to acknowledge. A `Project` skill is unchanged.

Notes are emitted for trusted skills only. Grants still end with `end_turn` and belong to the
invoking agent only; the note follows the same lifetime and keying.

### Guardian context

`SessionFacts` gains `active_skills: Vec<ActiveSkill>` where
`ActiveSkill { name, declared: Vec<String> }`, filled in `adjudication::build` from the caller's
notes (trusted skills only). `prompt_builder::blocks` (`fontaine-guardian/src/prompt_builder.rs`)
renders it as `active_skill: dev-skills:pr (trusted by the user; declares git *, gh *, mv *)`, only
when `guardian_view` is set and the list is non-empty (existing insta goldens depend on the line
being absent otherwise). The line is guardian-only. Both shipped prompts (`prompts/guardian_prompt.md`,
`prompts/guardian_minimal.md`) state that a trusted skill's declared classes are user intent for
routine steps of that class, never for destructive or out-of-purpose steps. `Profile::Minimal.shape`
(`profile.rs`) keeps `active_skills`, since it carries user intent. `adjudication::facts()`
initialises `active_skills` empty; `build` fills it from the caller's notes. The composed screen renders the same
`blocks(request, false, false)` (`screen.rs`), so the screen's input stays byte-identical. Every deny
rule, the floor and the fail-closed outcomes are unchanged: this is context, not an allow, so a
`git push --force origin main` is still the guardian's call (and any deny/ask rule's). A note is
only emitted for the invoking agent, so a subagent of the invoking agent carries none.

The guardian prompt change is verified with the live Haiku eval before done (project policy):
new cases in `evals/dataset` for (a) a routine `git rebase origin/main` under an active trusted
`pr` skill, allowed, and (b) `git push --force origin main` under the same skill, still denied.
Goldens for existing cases are not edited. `facts_from` in `fontaine-evals/src/live.rs` reads only
known `session_facts` keys, so it is extended to read `active_skills`; the `--guardian fixture` run
returns canned verdicts without building prompts and validates the dataset schema and labels only.

### CLI

`fontaine plugin trust <name>` and `fontaine plugin untrust <name>`, in
`rust/fontaine/src/cli.rs` (`PluginAction`) and `plugin.rs`. `<name>` is the composed skill name
exactly as `/skills` and the trust record show it (so user-directory skills and multi-segment
plugin skills are nameable). `<name>` that is an installed plugin and not a skill is the
`trust <plugin>` shorthand; a bare name matching both an installed plugin and a skill is an error.
`trust` prints, per skill, the digest and the declared `allowed-tools` split into narrow and broad,
and the skill's `!` lines, then records after a `[y/N]` confirmation. The confirmation reuses the
`confirm` helper (`rust/fontaine/src/auto_mode.rs:~320-326`) and the `stdin().is_terminal()`
refusal (~407). The `--yes` flag (a plain `#[arg(long)] bool`) is new; it skips only the
confirmation, and without a terminal and without it, `trust` refuses. `trust`/`untrust` enumerate skills through `plugins::load_installed`
plus `extensions::load` with `project_root` set to an empty path (the `validate_components` pattern,
`rust/fontaine/src/plugin.rs:~917-933`), so project files are never read. A name that is not a skill of an installed, enabled plugin or a
user-directory skill is an error. `untrust` removes records. Trust state is listed by `/skills`
(see below), not by a new `plugin list` column.

### Re-acknowledgement

- At session start, after skills load, each trusted record whose digest differs (auto-update still
  applies it) marks the skill `Stale` and raises one [notice](../docs/tui.md#notices). For a plugin
  skill: `skill dev-skills:pr changed since you trusted it; its allowed-tools are off until /skills
  trust dev-skills:pr`. For a user skill, whose rules stay lent: `skill my-skill changed since you
  trusted it; its trust note for the guardian is off until /skills trust my-skill`.
- TUI: `/skills trust <name>` (interactive only) opens an approval dialog showing the
  `allowed-tools` diff against the trusted version and, for a first trust, the full list, plus a
  `!` command-line diff. Confirming writes the record. The previous version's `allowed-tools` and
  `!` lines are kept in the record (`tools`, `commands`) so the diffs are available without keeping
  the old file.
- A headless run (`fontaine -p`) never prompts: a `Stale` or `Untrusted` plugin skill lends
  nothing (a user-directory skill keeps today's behaviour). When such a skill that declares
  `allowed-tools` is invoked, core `expand` warns via `Approver::warn`, deduplicated per skill with
  the once-per-key pattern of `warn_effort_refused` in `gate.rs` (~441-451: the `effort_warned`
  `HashSet`), so it covers model-invoked skills and the TUI's `expand_skill` alike. This is separate
  from the load-time stale warning (`prep.warnings`, `lib.rs`).
- `/skills` lists each skill's trust state: `trusted`, `changed (re-trust)`, or none.

## Implementation Steps

1. **Trust store** (`fontaine-core/src/skill_trust.rs`, new): `TrustFile` (de)serialization with
   `deny_unknown_fields`, `read`/`write` under `file_lock` and owner-only, `digest_of(bytes)` with
   `sha2` (already a dependency in `rust/Cargo.toml` and `fontaine-core`; lift an existing hex
   helper, `plugins.rs` ~2170 or `checkpoint.rs` ~49), `trust_state`. Export from `lib.rs`. Use
   `owner_only::write_atomic`, `file_lock::lock`/`read_shared` and `config::bare_toml_message`, and
   model the digest-pinned store on `approve_server` in `trust.rs` (~128-175). If the docs show a
   fenced `skill-trust.toml` example, add a validator branch in
   `rust/fontaine-core/tests/docs.rs` (~94-96).
2. **Loader** (`extensions.rs`): keep `declared_tools` for every scope; `read_document` (~214-226)
   also returns the raw text the digest is computed from; record each skill's digest
   (`Skill.digest`) from it; `Skill.allowed_tools` stays as today. Thread the trust file and the
   project-skill-record warning through `LoadParams` into `Extensions`' `TrustedSkills` view.
3. **Grants** (`skills.rs`, `gate.rs`): split narrow and broad in `expand`; add the trusted-skill
   note to `SessionState` with `end_turn` clearing and per-agent keying; surface stale skills
   through the notice channel; the once-per-skill warning for untrusted or stale plugin skills that
   declare `allowed-tools`, in `expand`.
4. **Guardian** (`fontaine-guardian/src/request.rs`, `prompt_builder.rs`, `profile.rs`, both prompt
   files, `adjudication.rs`): `active_skills` in `SessionFacts` (the `request()` fixture in
   `prompt_builder.rs` tests, ~446-465, is a full `SessionFacts` literal and needs the field); set it
   in `adjudication::build`, since `adjudication::facts` takes no state or caller; render in
   `blocks`; keep it in `Profile::Minimal.shape`; prompt paragraph in both prompts, tuned until eval
   cases (a) and (b) pass; eval cases; extend `facts_from` in `fontaine-evals/src/live.rs` with
   `active_skills`, and add an `active_skills` shape check in `rust/fontaine-evals/src/case.rs`
   `validate` (~291-300) so the fixture run validates it.
5. **CLI** (`cli.rs`, `plugin.rs`): `trust` and `untrust`, the confirmation prompt (existing
   `confirm` helper) and the `--yes` flag.
6. **TUI** (`fontaine-tui`, `commands.rs`, `ui/commands`, `reports.rs`): `/skills` takes no
   arguments today (`commands.rs:~399-403`), so add an argument-parsing `Command` variant for
   `/skills trust <name>`, a new skills file under `ui/commands`, the dialog, and trust state in
   the `/skills` report.
7. **Docs and plan**: see Documentation; observe what Claude Code does with a plugin skill's
   `allowed-tools` (`claude -p` against an installed plugin skill) before wording the Decisions
   amendment; add the Decisions lines to the master plan.

## Files to Modify

- `rust/fontaine-core/src/skill_trust.rs` (new), `lib.rs`, `extensions.rs`, `skills.rs`,
  `gate.rs`, `adjudication.rs` (reads the notes from `SessionState`; `permissions.rs` is unchanged)
- `rust/fontaine-guardian/src/request.rs`, `prompt_builder.rs`, `profile.rs`,
  `prompts/guardian_prompt.md`, `prompts/guardian_minimal.md`
- `rust/fontaine-evals/src/live.rs`, `case.rs`
- `rust/fontaine/src/cli.rs`, `plugin.rs`, `lib.rs`
- `rust/fontaine-tui/src/commands.rs`, `ui/commands/` (new skills file), `reports.rs`
- `evals/dataset/` (new cases), `evals/README.md` (~66-85) and `docs/evals.md` (~120) for the
  `active_skills` `session_facts` key, tests listed below
- `docs/extensions.md`, `docs/plugins.md`, `docs/permissions.md`, `docs/guardian.md`,
  `docs/tui.md`, `docs/headless.md`, `docs/configuration.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Decisions

- 2026-11-03: Trust is per skill, with `trust <plugin>` as shorthand for the skills present at that
  time; reason: a trusted skill must not vouch for the other skills of its plugin.
- 2026-11-03: Trust is pinned to a digest of the skill file, not the plugin pin; reason: the pin
  changes with unrelated files, the digest only when this skill does.
- 2026-11-03: Trust lives in its own `skill-trust.toml`, not `plugins.toml`; reason: `plugins.toml`
  is rewritten by auto-update and tied to plugin lifecycle, and a separate file also covers
  user-directory skills and survives uninstall.

## Documentation

- `docs/extensions.md` `allowed-tools`: trusted skills and the broad-rule behavior.
- `docs/plugins.md` "What install approves": install approves executables, `plugin trust`
  approves a skill's `allowed-tools`; the `trust`/`untrust` commands.
- `docs/permissions.md`, `docs/guardian.md`: `active_skill` context.
- `docs/tui.md`, `docs/headless.md`: `/skills trust`, stale notice, no prompt headless.
- `docs/configuration.md`: a `<config dir>/skill-trust.toml` row, distinguished from workspace
  `trust.toml`.
- `evals/README.md`, `docs/evals.md`: the `active_skills` `session_facts` key.
- `CHANGELOG.md`.
- Master plan Decisions (`tasks/fontaine_harness_plan.md`), each line in its subject's subsection:
  - Config: amend the 2026-10-21 project-skill `allowed-tools` line (~424) to say project skills can
    never be trusted.
  - Extensibility: amend the "plugin skills/agents follow project permission rules ... stricter
    than Claude Code" line (~472) to name `plugin trust` as the way back to parity, and the
    2026-10-28 line likewise.
  - New deviations (Extensibility unless the plan has a better-fitting subsection): a trusted
    skill's broad rules are guardian context, not allows, in `auto`; trust is pinned to a file
    digest and suspended when stale; headless never re-acknowledges.

## Testing Strategy

Unit and integration (no live API):

- Trust file: round-trip (including `tools` and `commands`), corrupt file warns and loads nothing,
  unknown key rejected.
- `trust_state`: absent, matching, stale, project scope never trusted.
- Loader: plugin skill with `allowed-tools` keeps `declared_tools`, `allowed_tools` empty; digest
  changes with a body edit and an `allowed-tools` edit.
- Grants (extend `tests/extensions.rs` and `tests/plugins.rs`): a trusted plugin skill's narrow rule
  yields `Tier::AllowRule` with the guardian fake not called (pattern: `tests/permissions_auto.rs`
  ~483-530), for the invoking agent only; a stale one lends nothing; broad rules are
  not allows in `auto` and appear as an active-skill note; outside `auto` they allow; deny/ask and
  the floor still win; `end_turn` clears both. An untrusted or stale user-directory skill lends all
  its rules with no note. Regression pin for #361: a trusted `pr`-style skill with `Bash(git *)`
  yields guardian context, not an allow, for `git rebase origin/main`.
- `adjudication::build`: a subagent of the invoking agent carries no `active_skills`.
- Guardian: `blocks` renders the `active_skill` line only when `guardian_view` is set and the list
  is non-empty; a screen-input test (like `prompt_builder.rs:~588`) shows the composed screen's
  input is unchanged when `active_skills` is non-empty; `facts_from` reads `active_skills` (unit test like `live.rs:~684-700`).
- CLI: `trust` refuses non-interactive without `--yes`, records the digest, `untrust` removes
  it; a user-directory and a multi-segment plugin skill name resolve; an ambiguous bare name errors.
- Warning: invoking a stale or untrusted plugin skill that declares `allowed-tools` warns once per
  skill through `Approver::warn` (core test) and lends nothing.
- `!`-line diff between the trusted and current versions.
- TUI: `/skills trust` parsing (`commands.rs` tests ~809, ~1031); the trust column in
  `reports::skills` (`reports.rs` test ~1171); the acknowledgement writes the record (ui harness
  `src/ui/tests.rs`, `tests/tui.rs` ~420).
- Once-per-session stale notice.
- Guardian evals (live, manual): cases in Guardian prompt section, run with the Haiku eval; the
  `--guardian fixture` run validates dataset schema and labels only.

## Manual Verification

1. Edit the installed skill file; restart; the stale notice appears and `/skills trust` shows the
   `allowed-tools` diff. Not automated: dialog layout.
