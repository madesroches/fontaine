# Trusted Skills Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/379

## Overview

A plugin skill's `allowed-tools` is ignored today (plugin content is tighten-only), and in `auto` a
user-directory skill's broad grant such as `Bash(git *)` is set aside. A user who has decided to
trust a skill has no way to say so, so unattended runs stall on steps the skill documents (#361).
This plan adds an explicit, user-scope trust record for a skill, pinned to a digest of the skill
file. While the record matches, the skill's `allowed-tools` are honoured: narrow rules act as real
allows, and broad rules reach the guardian as context instead of as an allow. A changed file
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
  trusted plugin skill's body can change through auto-update with no approval step. Trust therefore
  needs its own digest.
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
loads nothing and warns once, like `plugins.toml`.

```toml
[[skill]]
name = "dev-skills:pr"          # composed skill name
digest = "sha256:9f2c..."      # SHA-256 of the skill file bytes (frontmatter and body)
```

Granularity is per skill: a trusted `pr` does not vouch for the other skills of its plugin.
`trust <plugin>` is shorthand that records every skill the installed plugin has at that moment; a
skill added later is untrusted until trusted. Keying by composed name makes uninstalling and
reinstalling a plugin keep the record; a digest mismatch covers a changed file.

The digest covers the raw bytes of the file the loader reads, so `allowed-tools`, the body and its
`!` lines are all pinned. Commands (`commands/*.md`) are skills too and are trusted the same way.

### Which skills can be trusted

`Scope::User` and `Scope::Plugin` skills. A `Scope::Project` skill can never be trusted: a record
for a project skill's name is ignored with a warning. A user-directory skill needs no trust to lend
narrow rules (unchanged); trust for it additionally enables the broad-rule path below.

### Loading

`load_skill` parses `allowed-tools` for every scope and stores it in a new
`Skill.declared_tools: Vec<Rule>` (validated as today). `Skill.allowed_tools` keeps its meaning,
the rules lent without trust. `SkillRuntime` gets a `TrustedSkills` view (the parsed
`skill-trust.toml`, loaded at startup next to the plugins) and `expand` resolves a `SkillGrant`:

```
trust_state(skill) =
  Project scope                    -> Untrusted
  record absent                    -> Untrusted
  record digest != file digest     -> Stale   (grants off; notice once per session)
  record digest == file digest     -> Trusted
```

The digest is computed on load from the same bytes the loader parsed, not re-read at `expand`, so
it cannot differ from the body that is run.

### Grants

`expand` lends, for a `Trusted` skill (or a `Scope::User` skill as today):

- **Narrow** declared rules (`!is_broad_code_execution`) to `grant_skill_rules`: real allows for
  the turn, after the floor, deny/ask rules and hooks, exactly as a user-directory skill's today.
- **Broad** declared rules (`Bash(git *)`, bare `Bash`) to a new
  `SessionState::note_trusted_skill(agent, TrustedSkillNote { name, declared })`, not into
  `skill_grants`. The permission pipeline never sees them as allows in `auto`. Outside `auto` the
  broad rules also become allows, as a user-directory skill's do today (the guardian is not
  consulted there anyway).

A `Stale` skill lends nothing, narrow or broad, and a notice tells the user to acknowledge.
Grants still end with `end_turn` and belong to the invoking agent only; the note follows the same
lifetime and keying. A plain untrusted plugin skill is unchanged.

### Guardian context

`SessionFacts` gains `active_skills: Vec<ActiveSkill>` where
`ActiveSkill { name, trusted_by_user: bool, declared: Vec<String> }`, filled in
`adjudication::build` from the caller's notes. The guardian prompt renders it as
`active_skill: dev-skills:pr (trusted by the user; declares git *, gh *, mv *)` and states in the
shipped prompt that a trusted skill's declared classes are user intent for routine steps of that
class, never for destructive or out-of-purpose steps. Every deny rule, the floor and the
fail-closed outcomes are unchanged: this is context, not an allow, so a `git push --force origin
main` is still the guardian's call and still hits the protected-path floor. A note is only emitted
for the invoking agent, so a subagent's request carries none.

The guardian prompt change is verified with the live Haiku eval before done (project policy):
new cases in `evals/dataset` for (a) a routine `git rebase origin/main` under an active trusted
`pr` skill, allowed, and (b) `git push --force origin main` under the same skill, still denied.
Goldens for existing cases are not edited.

### CLI

`fontaine plugin trust <plugin>[:<skill>]` and `fontaine plugin untrust <plugin>[:<skill>]`, in
`rust/fontaine/src/cli.rs` (`PluginAction`) and `plugin.rs`. `trust` prints, per skill, the digest
and the declared `allowed-tools` split into narrow and broad, and the skill's `!` lines, then
records after a `[y/N]` confirmation. `--yes=yes` skips only the confirmation; without a terminal
and without it, `trust` refuses. A name that is not a skill of an installed, enabled plugin or a
user-directory skill is an error. `untrust` removes records. Trust state is listed by `/skills`
(see below), not by a new `plugin list` column.

### Re-acknowledgement

- At session start, after skills load, each trusted record whose digest differs marks the skill
  `Stale` and raises one [notice](../docs/tui.md#notices): `skill dev-skills:pr changed since you
  trusted it; its allowed-tools are off until /skills trust dev-skills:pr`.
- TUI: `/skills trust <name>` (interactive only) opens an approval dialog showing the
  `allowed-tools` diff against the trusted version and, for a first trust, the full list, plus a
  body diff summary (line counts, `!` lines). Confirming writes the record. The previous version's
  `allowed-tools` are kept in the record (`tools = [...]`) so the diff is available without
  keeping the old file; the body diff is limited to a `!` command-line diff and a changed-lines
  count for the same reason.
- A headless run (`fontaine -p`) never prompts: a `Stale` or `Untrusted` skill lends nothing, and
  the run prints one stderr line (via `Approver::warn`) naming it, once per skill.
- `/skills` lists each skill's trust state: `trusted`, `changed (re-trust)`, or none.

### Interaction with auto-update

Auto-update continues to apply on the executables fingerprint only. A revision that edits a
trusted skill's file applies, the record becomes `Stale` at the next start, and the grants stay off
until acknowledged. The held-revision notice is unchanged.

## Implementation Steps

1. **Trust store** (`fontaine-core/src/skill_trust.rs`, new): `TrustFile` (de)serialization with
   `deny_unknown_fields`, `read`/`write` under `file_lock` and owner-only, `digest_of(bytes)` with
   `sha2`, `trust_state`. Export from `lib.rs`.
2. **Loader** (`extensions.rs`): keep `declared_tools` for every scope; record each skill's
   digest (`Skill.digest`) from the bytes already read; `Skill.allowed_tools` stays as today.
   Thread the trust file through `LoadParams`.
3. **Grants** (`skills.rs`, `gate.rs`): split narrow and broad in `expand`; add the trusted-skill
   note to `SessionState` with `end_turn` clearing and per-agent keying; surface stale skills
   through the notice channel.
4. **Guardian** (`fontaine-guardian/src/request.rs`, prompt, `adjudication.rs`): `active_skills`
   in `SessionFacts`; prompt paragraph; eval cases.
5. **CLI** (`cli.rs`, `plugin.rs`): `trust` and `untrust`.
6. **TUI** (`fontaine-tui`, `commands.rs`, `ui/commands`, `reports.rs`): `/skills trust <name>`
   dialog and trust state in the `/skills` report.
7. **Headless** (`fontaine/src/headless`): the once-per-skill warning.
8. **Docs and plan**: see Documentation; add the Decisions lines to the master plan.

## Files to Modify

- `rust/fontaine-core/src/skill_trust.rs` (new), `lib.rs`, `extensions.rs`, `skills.rs`,
  `gate.rs`, `adjudication.rs`, `permissions.rs` (only if the notes need a read path)
- `rust/fontaine-guardian/src/request.rs`, the guardian prompt text
- `rust/fontaine/src/cli.rs`, `plugin.rs`, `lib.rs`, `headless/`
- `rust/fontaine-tui/src/commands.rs`, `ui/commands/`, `reports.rs`
- `evals/dataset/` (new cases), tests listed below
- `docs/extensions.md`, `docs/plugins.md`, `docs/permissions.md`, `docs/guardian.md`,
  `docs/tui.md`, `docs/headless.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Per skill vs per plugin.** Per skill keeps a trusted `pr` from vouching for every sibling
  skill; `trust <plugin>` shorthand keeps the common case one command. Per plugin alone was
  rejected because plugin skill prose auto-updates outside the approval fingerprint.
- **Digest of the file vs the plugin pin.** A pin changes with unrelated files; the digest changes
  only when this skill does, so re-acks are rare and meaningful.
- **Broad rules as guardian context vs as allows.** An allow would skip the guardian for
  `git push --force origin main`; context lets the guardian decide with the user's intent in view,
  and the fail-closed rule is untouched.
- **Extending `plugins.toml` vs a new file.** `plugins.toml` is rewritten by auto-update and
  `deny_unknown_fields` ties it to plugin lifecycle; a separate file also covers user-directory
  skills and survives uninstall.
- **No re-prompt in headless.** Matches the project rule that a headless run never prompts.

## Decisions

- 2026-11-03: Trust is per skill, with `trust <plugin>` as shorthand for the skills present at that
  time; reason: a skill's prose is not covered by the plugin's approval fingerprint.

## Documentation

- `docs/extensions.md` `allowed-tools`: trusted skills and the broad-rule behavior.
- `docs/plugins.md` "What install approves": install approves executables, `plugin trust`
  approves a skill's `allowed-tools`; the `trust`/`untrust` commands.
- `docs/permissions.md`, `docs/guardian.md`: `active_skill` context.
- `docs/tui.md`, `docs/headless.md`: `/skills trust`, stale notice, no prompt headless.
- `CHANGELOG.md`; master plan Decisions: amend the 2026-10-21 and 2026-10-28 lines to name
  `plugin trust` as the way back to parity.

## Testing Strategy

Unit and integration (no live API):

- Trust file: round-trip, corrupt file warns and loads nothing, unknown key rejected.
- `trust_state`: absent, matching, stale, project scope never trusted.
- Loader: plugin skill with `allowed-tools` keeps `declared_tools`, `allowed_tools` empty; digest
  changes with a body edit and an `allowed-tools` edit.
- Grants (extend `tests/extensions.rs` and `tests/plugins.rs`): a trusted plugin skill lends its
  narrow rules for the turn to the invoking agent only; a stale one lends nothing; broad rules are
  not allows in `auto` and appear as an active-skill note; outside `auto` they allow; deny/ask and
  the floor still win; `end_turn` clears both. Regression pin for #361: a trusted `pr`-style skill
  with `Bash(git *)` yields guardian context, not an allow, for `git rebase origin/main`.
- `adjudication::build`: a subagent request carries no `active_skills`.
- CLI: `trust` refuses non-interactive without `--yes`, records the digest, `untrust` removes it.
- Headless: a stale skill prints one warning and lends nothing.
- Guardian evals (live, manual): cases in Guardian prompt section, run with the Haiku eval and with
  `--guardian fixture`.

## Manual Verification

1. `fontaine plugin trust <plugin>:<skill>` against an installed plugin skill, then run it under
   `auto` in a tmux session: its narrow rules run without a prompt. Not automated: end-to-end TUI
   plumbing.
2. Edit the installed skill file; restart; the stale notice appears and `/skills trust` shows the
   `allowed-tools` diff. Not automated: dialog layout.

## Open Questions

- What does Claude Code do with a plugin skill's `allowed-tools` (`claude -p` against an installed
  plugin skill)? If it honours them on install, the master plan's Decisions line should name
  `plugin trust` as the user's way back to parity; the plan adds that wording either way, reworded
  after the check.
- Exact guardian prompt wording for `active_skill`, settled by the live eval.
