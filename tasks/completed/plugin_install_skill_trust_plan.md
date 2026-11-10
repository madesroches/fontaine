# Trust a Plugin's Skills at Install Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/561

## Overview

Installing a plugin approves its executable surface but leaves every skill that declares
`allowed-tools` untrusted; the user discovers this later, at first use, as a one-line warning.
This plan asks the trust question at install time — the same question `/skills trust` asks, with
the same digest pinning — in the terminal UI, and offers it on the interactive
`fontaine plugin install` too. Headless installs never trust implicitly: no terminal, no
question, skills stay untrusted.

## Current State

- **TUI install flow** (`rust/fontaine-tui/src/ui/commands/plugin.rs`, `plugin_install`):
  `prepare_install` → `confirm_plugin` (the executable-surface approval, a `ConfirmPrompt`) →
  `commit_install` → `apply_plugin_changes` (a `reload_extensions` that re-reads
  `skill-trust.toml` via `extensions::load`, `rust/fontaine-core/src/extensions.rs:518`). Dropping
  the `PreparedInstall` removes its scratch checkout.
- **CLI install flow** (`rust/fontaine/src/plugin.rs`, `install`): `prepare_install` → print
  summary → `commit_install`, fully non-interactive today.
- **The trust record** (`rust/fontaine-core/src/skill_trust.rs`): `skill-trust.toml` in the
  config dir, one `TrustRecord` per skill (name, `sha256:` digest of the file, declared tools,
  `!` commands). `TrustedSkills` is read-only for a session's lifetime; a reload re-reads the
  file, so a trust written at install takes effect in the same session through the install's own
  reload (`skill_changes` in `agent/reload.rs` reports `skill trust applied: …` only for a skill
  that existed before the reload and whose trust state changed — a freshly installed skill shows
  under `skills added: …` instead).
- **The question to reuse**: `rust/fontaine-tui/src/skill_trust_prompt.rs::new` builds the
  `/skills trust` dialog from a `Skill` (`describe` shows the split `allowed-tools`, the `!`
  commands, the file and digest). The `/skills trust` command loop
  (`rust/fontaine-tui/src/ui/commands/skills.rs`) answers it with
  `Action::SkillTrust`, then calls `skill_trust::trust` and `runtime.state.record_skill_trust`
  (ledger `Record::SkillTrust` + audit event). `ConfirmPrompt` is default-deny: only `y` accepts,
  `n`/Esc decline, and an answer-grace period swallows typed-ahead keys.
- **The later warning** (`rust/fontaine-core/src/gate.rs`, `warn_skill_not_trusted`): fires once
  per skill at first use when the session does not honour its `allowed-tools`.
- **Skill loading from a plugin checkout** (`extensions.rs`): `load_plugin` turns a `PluginDir`
  (`rust/fontaine-core/src/plugins.rs`, from `build_plugin` at prepare time) into namespaced
  `Skill`s (`skills/<name>/SKILL.md` and `commands/**/*.md`, scope `Plugin`). `load_skill` is
  private and computes the digest the trust record pins. `PluginBuild.dir` on `PreparedInstall`
  names the validated scratch checkout's files.
- **Existing enumeration precedent**: `fontaine plugin trust <plugin>`
  (`rust/fontaine/src/plugin.rs`, `named_skills` + `print_trust_details`) resolves a plugin to
  its skills after install; its interactive confirm uses `crate::auto_mode::confirm` and refuses
  without a terminal.
- **Docs**: `docs/plugins.md` ("What install approves", "Trusting a skill") records the
  deliberate split this plan narrows; the master plan's `## Decisions` needs a dated entry per
  the issue's notes.

## Design

### Core: load a checkout's skills without a session

Factor the skill/command loading out of `extensions::load_plugin` into a new public function:

```rust
pub fn load_plugin_skills(plugin: &PluginDir) -> Result<Vec<Skill>, ExtensionError>
```

It runs the existing `skills` + `commands` loops unchanged — same namespacing
(`namespaced(plugin.name, segments, base)`), same `valid_namespaced` rejection for each name
(same error text) and duplicate-name rejection within those two kinds — and returns the `Skill`s
instead of pushing them into `Extensions`. `load_plugin` calls it and keeps the
`valid_namespaced` rejection for agent and style names alongside its cross-kind duplicate check
against the returned names (the existing `seen` set semantics stay intact: build the set from the
returned names, then check agents and styles against it). No behavior change for session loads.

A skill is **trustable** when `!skill.declared_tools.is_empty()`. The records to offer are
`skill_trust::record_of(skill)` — name, digest, tools, commands — exactly what `/skills trust`
records.

### TUI `/plugin install`: a trust step in the install wizard

In `plugin_install`, after the executable-surface approval answers `Some(true)` and before
`commit_install`:

1. Enumerate `load_plugin_skills(&prepared.build.dir)`, keep the trustable ones, in load
   order. On an enumeration error, print the parse error notice, drop the `PreparedInstall`
   (its scratch is removed on drop) and cancel the install.
2. If none, commit as today.
3. Otherwise, for each trustable skill, show the same dialog `/skills trust` shows, with an
   install-aware footer variant:
   `begin_skill_trust(skill_trust_prompt::new(skill, None, SkillTrustFooter::Install))` and
   the same input loop as
   `skills.rs` (decline clears the prompt and moves to the next skill). Collect the accepted
   skills' records. The footer says the trust takes effect when the install finishes loading
   (the install's own reload applies it moments later) instead of "after /reload-plugins or at
   the next session start"; the wording is a parameter to `skill_trust_prompt::new`, so
   `/skills trust`'s footer is unchanged.
4. If the interface goes away mid-question (`None`, as `confirm_plugin` treats `Action::Quit`),
   drop the `PreparedInstall` (its scratch is removed) with the notice `install cancelled`, and
   break — quitting the wizard cancels the whole install, printing the same `install cancelled`
   notice the decline path uses.
5. On `commit_install` success, write the accepted records with `skill_trust::trust` (its file
   locking and replace-by-name semantics are unchanged), then for each call
   `runtime.state.record_skill_trust(name, digest)` so the ledger and audit log carry the trust
   as `/skills trust`'s does; if a write fails, surface the same
   `could not record the trust: <chain>` error notice `/skills trust` uses, leaving the completed
   install itself intact. Notice per trusted skill:
   `` `<name>` trusted: its allowed-tools count from this reload onward ``.
6. If any trustable skill was declined, one notice naming them and the way to trust later:
   `` left untrusted: <a>, <b>; `/skills trust <name>` or `fontaine plugin trust <plugin>` honours them later ``.
   (The existing first-use warning in `gate.rs` is unchanged.)
7. `apply_plugin_changes` runs as today; the reload re-reads `skill-trust.toml`, so accepted
   trusts apply to the just-installed plugin in this session.

The trust question is a separate prompt after the executable-surface approval, never folded into
it, and default-deny through the reused `ConfirmPrompt`. Asking before the commit (rather than
after) keeps the wizard one atomic sequence, and the `file:`/`digest:` lines in the dialog name
the scratch checkout's real files. A decline never blocks the install — only the trust.

### CLI `fontaine plugin install`: offer the question on a terminal, hint otherwise

In `install` (`rust/fontaine/src/plugin.rs`), after the summary and before `commit_install`:

1. Enumerate the same way; if no trustable skills, install as today.
2. If stdin is a terminal, print each trustable skill with the existing
   `print_trust_details` (the same details `fontaine plugin trust` shows) and ask once, with
   `crate::auto_mode::confirm`: `Honour the allowed-tools of these N skills?` — mirroring
   `fontaine plugin trust <plugin>`. On yes, record after a successful commit with
   `skill_trust::trust` and print an install-aware "trusted N skill(s)…" line ("from the
   install's own reload"), not the trust command's "next session start, or at /reload-plugins"
   wording, which is stale for an install with no running session. On no, continue untrusted.
   Note: enumeration runs on the pre-commit scratch checkout, so the `file:` path
   `print_trust_details` prints is the scratch path that `commit_install` moves moments later —
   harmless, since the digest, not the path, pins the trust.
3. If stdin is not a terminal, no question: after `installed plugin …`, print one hint line —
   `note: N skill(s) declare allowed-tools and stay untrusted; \`fontaine plugin trust <plugin>\` reviews and trusts them`.

Headless `fontaine -p` never installs plugins (install is a separate subcommand), so there is
nothing to gate there; the rule "headless never trusts implicitly" holds through the terminal
check, matching `fontaine plugin trust`'s refusal to trust without a terminal.

One deliberate side effect: enumerating skills parses their frontmatter at install time, before
the commit, so a malformed skill file now fails the install with its parse error instead of
failing the first session load. Same cause, surfaced earlier, at install time rather than at
first session load.

### Out of scope

`/plugin update` and auto-update do not re-ask: a changed skill file mismatches its pinned
digest and becomes untrusted (or stale) automatically, and the issue covers install only.

## Implementation Steps

1. **`rust/fontaine-core/src/extensions.rs`**: extract `load_plugin_skills` from `load_plugin`;
  adjust `load_plugin` to use it while keeping its cross-kind duplicate check. Unit-test the new
  function: namespacing, `valid_namespaced` rejection (same error text), skill/command duplicate
  rejection, `declared_tools` population, digest.
2. **`rust/fontaine-tui/src/ui/commands/plugin.rs`**: add the trust step to `plugin_install`
  per Design. Decline/quit paths leave nothing recorded. Add the install-aware footer wording
  as a parameter to `skill_trust_prompt::new`
  (`rust/fontaine-tui/src/skill_trust_prompt.rs`; the footer text leaves the hardcoded last
  line of `describe`), with the call-site touch-ups in `ui/commands/skills.rs` and `linear.rs`.
3. **`rust/fontaine/src/plugin.rs`**: the CLI offer/hint per Design, reusing
  `print_trust_details` and `auto_mode::confirm`.
4. **Docs**: update `docs/plugins.md` ("What install approves" / "Trusting a skill") and, if it
  restates the split, `docs/extensions.md` ("Trusted skills"). Update the install description
  in the `docs/tui.md` `/plugin` row and the `docs/plugins.md` "In the terminal UI" install row
  — both currently describe install as a single executable-surface approval — to include the
  per-skill trust question. Add a dated line to the master
  plan's `## Decisions` recording this deliberate change to the install/trust split. The `pr`
  step appends the CHANGELOG entry.
5. **Tests** per below.

## Files to Modify

- `rust/fontaine-core/src/extensions.rs`
- `rust/fontaine-tui/src/ui/commands/plugin.rs`
- `rust/fontaine-tui/src/skill_trust_prompt.rs` (install-aware footer parameter)
- `rust/fontaine-tui/src/ui/commands/skills.rs`, `rust/fontaine-tui/src/linear.rs` (call-site
  touch-ups)
- `rust/fontaine/src/plugin.rs`
- `rust/fontaine-tui/tests/plugins_live.rs` (tests)
- `rust/fontaine/tests/plugin.rs` (tests)
- `docs/plugins.md`, `docs/tui.md`, `docs/extensions.md`, `tasks/fontaine_harness_plan.md` (Decisions)
- `CHANGELOG.md`

## Trade-offs

- **Ask before commit vs after**: asking after a successful commit would leave an installed
  plugin when the user quits at the trust prompt; asking before keeps the wizard atomic and the
  dialog's `file:` lines real. Chosen: before.
- **One prompt for all skills vs one per skill**: the TUI asks per skill, because a trust record
  is pinned to one skill file's digest and `/skills trust`'s dialog is per skill; a single
  bundled yes/no would hide which file each digest covers. The CLI asks once for the whole
  plugin, matching `fontaine plugin trust <plugin>`.
- **New dialog vs reused**: the install question reuses `skill_trust_prompt` with an
  install-aware footer variant, so the install-time and `/skills trust` questions stay identical
  except for when the trust takes effect.
- **Enumeration at install time, before the commit** slightly widens what install validates
  (skill frontmatter), trading an install-time error for a first-load error.

## Decisions

- The trust question is asked after the executable-surface approval and before the commit, per
  the Trade-offs comparison; quitting at it cancels the install.
- TUI asks per skill; CLI asks once for all of the plugin's trustable skills, only on a terminal.
- `/plugin update` and auto-update intentionally re-ask nothing; digest pinning handles changed
  skills by making them untrusted again.

## Documentation

- `docs/plugins.md`: "Trusting a skill" — install now offers the question in the terminal UI and
  on interactive CLI installs; headless installs stay untrusted with a hint. "What install
  approves" — the trust question is separate from the executable-surface approval.
- `docs/tui.md` (`/plugin` row) and `docs/plugins.md` ("In the terminal UI"): the install
  description gains the per-skill trust question after the executable-surface approval.
- `docs/extensions.md` ("Trusted skills"): same narrowing, if the split is restated there.
- `tasks/fontaine_harness_plan.md` `## Decisions`: dated line recording the change.
- `CHANGELOG.md`: entry (by the `pr` step).

## Testing Strategy

- **Unit** (`fontaine-core`): `load_plugin_skills` — namespaced names for `skills/` and
  `commands/` files, duplicate rejection between them, `declared_tools` parsed, digest is
  `sha256:`. Existing `extensions` tests keep `load_plugin` behavior pinned.
- **TUI live** (`rust/fontaine-tui/tests/plugins_live.rs`): the fixture marketplace gains a
  second plugin beside `demo` whose skill declares `allowed-tools` (`demo` itself stays
  untouched, so the existing tests driving `demo@local` keep answering only the
  executable-surface prompt and never stall at the trust dialog):
  - approve the install → the trust dialog appears naming the skill and its tools; `y` →
    `skill-trust.toml` holds the record, the ledger carries `skill_trust`, `/skills` shows the
    skill trusted;
  - `n` at the trust dialog → plugin installs, no trust record, the "left untrusted" notice
    appears, `/skills` shows the skill without `(trusted)`;
  - a plugin whose skills declare no `allowed-tools` installs with no trust dialog (the existing
    tests already cover this implicitly; keep one explicit);
  - quit at the trust dialog (interface gone / Ctrl+C) cancels the install, registry stays empty;
  - `Esc` at the trust dialog declines that skill and the install still commits untrusted
    (registry holds the plugin, no trust record).
- **CLI integration** (`rust/fontaine/tests/plugin.rs`): a piped (non-terminal) install of a
  plugin with a trustable skill succeeds, writes no trust record, and prints the hint line;
  existing `plugin trust` tests stay green.
- The docs-consistency test (`fontaine-core/tests/docs.rs`) must pass after the doc edits.

## Manual Verification

None beyond the automated coverage: the TUI live test drives the real prompt, the CLI test
drives the real non-terminal path, and `fontaine plugin trust` covers the terminal-only confirm
already.
