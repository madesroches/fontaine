# Claude Subscription Builtin Plugins Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/429

## Overview

A basic request over the experimental `claude-subscription` transport fails with
`the Claude CLI loaded a customization Fontaine did not ask for` on Claude CLI `2.1.318`. The
cause is that the CLI's `system/init` event now always lists the plugins bundled into the CLI
itself, even with every setting source switched off. Fontaine will accept those bundled
(`@builtin`) plugins, keep refusing every other plugin and every skill, and make the refusal name
what was loaded and say how to stop it from loading.

## Current State

- `Reader::validate_init` (`rust/fontaine-core/src/providers/claude_subscription/protocol.rs:633`)
  checks the init event: the model, the tools, exactly one `fontaine` MCP server, and then
  (`:670-680`) fails with one generic protocol error if either the `plugins` or `skills` array is
  non-empty. The error doesn't say which array tripped the check or which entries it held.
- The CLI is launched by `fixed_args` (`cli.rs:668`) with `--setting-sources ""`,
  `--strict-mcp-config` and `--disable-slash-commands`, plus private settings from `base_settings`
  (`cli.rs:656`) that switch off hooks, memory, compaction and updates.
- The test double `rust/fontaine-core/examples/claude_cli_double.rs:250` emits an init event with
  `"plugins": []`, so no test sees what a real CLI reports. `init_extra` lets a scenario override
  init keys. `unexpected_init_customization_is_refused`
  (`rust/fontaine-core/tests/claude_subscription.rs:1265`) and the `"init plugins"` case in
  `protocol/tests.rs:504` cover the refusal.

### What the real CLI reports (observed 2026-11-05, `2.1.318 (Claude Code)`)

Running the CLI with Fontaine's exact flags, settings and fixed environment, the init event has
`"skills": []`, `"slash_commands": []`, `"tools": []`, `"mcp_servers": []` and:

```json
"plugins": [
  {"name": "cc-plugin-agents-md",        "path": "builtin", "source": "cc-plugin-agents-md@builtin"},
  {"name": "cc-plugin-telemetry",        "path": "builtin", "source": "cc-plugin-telemetry@builtin"},
  {"name": "cc-plugin-privacy",          "path": "builtin", "source": "cc-plugin-privacy@builtin"},
  {"name": "cc-plugin-plugin-authoring", "path": "builtin", "source": "cc-plugin-plugin-authoring@builtin"}
]
```

A user-installed plugin (`dev-skills@dev-skills`, user scope, enabled) is **not** listed, so
`--setting-sources ""` already keeps user customization out. The builtins can be switched off with
`"enabledPlugins": {"<source>": false}` in the `--settings` file, but `claude plugin list --json`
doesn't list them, so their names can't be found before launch. With and without the builtins, the
prompt for `hi` in a checkout that has an `AGENTS.md` measured 822 and 827 cache-creation tokens:
the builtins add nothing material to what the model sees.

## Design

### Accept bundled plugins

A plugin entry is **bundled** when `path == "builtin"` **and** `source` ends with `@builtin`. Both
are required, so a user plugin that happens to have one of those strings isn't mistaken for a
bundled one. `validate_init` drops bundled entries before checking `plugins`. Every other entry
still fails, as does any `skills` entry.

### Diagnostics that name the customization

Replace the one generic message with one per kind, which names the entries and says what to do:

```text
the Claude CLI loaded plugins Fontaine did not ask for: foo@market, bar@market; only the CLI's
builtin plugins are accepted. The CLI loaded them even with every setting source switched off;
look at the CLI's managed/policy settings or the environment Fontaine passes through to it
```

The `skills` message:

```text
the Claude CLI loaded skills Fontaine did not ask for: foo, bar; no skills are accepted. The CLI
loaded them even with every setting source switched off; look at the CLI's managed/policy settings
or the environment Fontaine passes through to it
```

An entry renders as its `source`, falling back to `name`
and then to the bare string for a string entry, and to `?` otherwise. At most 5 entries are shown,
then `and N more`, and each entry is cut to 80 chars (`chars().take(80)`), so the CLI's output can't make the
message unbounded. The kind stays `ErrorKind::Protocol` (non-retryable), as now; only the text
changes.

The message is built by a small helper, `customization_error(kind: &str, entries: &[&Value])`, in
`protocol.rs`, next to `protocol()`.

### Test double mirrors the real CLI

The double's default init event lists one bundled plugin, shaped exactly like the observed entries
(`{"name": "cc-plugin-telemetry", "path": "builtin", "source": "cc-plugin-telemetry@builtin"}`).
Every existing integration test then runs against a realistic init, which pins #429.

## Implementation Steps

1. `protocol.rs`: add `is_bundled(&Value) -> bool` and `customization_error`. In `validate_init`,
   filter bundled entries out of `plugins`, and fail with the named message if any remain or if
   `skills` is non-empty.
2. `examples/claude_cli_double.rs`: add the bundled plugin entry to the default init.
3. `protocol/tests.rs`: unit tests (see Testing Strategy).
4. `tests/claude_subscription.rs`: extend `unexpected_init_customization_is_refused` to assert that
   the plugin case (entry changed to `{"name": "foo", "path": "/p", "source": "foo@market"}`) has an
   error containing `foo@market`, and add the regression test.
5. `docs/claude-subscription.md`: in "What a request is", say that the CLI's own builtin plugins
   are accepted and any other plugin or skill is refused with an error naming it. Keep the
   sentence saying instruction files are switched off true: one accepted builtin is
   `cc-plugin-agents-md`, but instruction files stay off because each CLI process runs in an empty
   private scratch directory and `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1` is set. Also cover the sentence
   about what the CLI is started without.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`
- `rust/fontaine-core/src/providers/claude_subscription/protocol/tests.rs`
- `rust/fontaine-core/examples/claude_cli_double.rs`
- `rust/fontaine-core/tests/claude_subscription.rs`
- `docs/claude-subscription.md`
- `CHANGELOG.md`: a `### Fixed` entry under `[Unreleased]` citing #429

## Trade-offs

- **Disable the builtins via `enabledPlugins` instead of accepting them.** Rejected: their names
  change across CLI versions and can't be listed before launch, so every new CLI release could
  break requests again. A retry that learns the names from a failed init would double process
  startup and add state, to remove plugins whose effects are already checked or switched off.
- **Accept any `path == "builtin"` entry without checking `source`.** Rejected: requiring both
  fields costs nothing and keeps a user plugin from passing as bundled.
- **Tolerate all plugins and rely on the other checks.** Rejected: a non-bundled plugin means the
  CLI read customization despite `--setting-sources ""`, which is worth surfacing.

## Decisions

- 2026-11-05: Builtin plugins are accepted, at the maintainer's suggestion during design ("consider
  allowing the builtin plugins").

## Documentation

See Implementation Steps, step 5.

## Testing Strategy

Unit tests in `protocol/tests.rs` (driven through `Reader::accept`):

- An init with the four observed bundled entries is accepted, and the stream then decodes.
- An init with one bundled and one user plugin (`foo@market`) fails. The message contains
  `plugins` and `foo@market`, and not the bundled entry's name.
- `{"path": "builtin", "source": "x@market"}` and `{"path": "/p", "source": "x@builtin"}` each
  fail (both fields are required).
- A non-empty `skills` (string entry and object entry) fails with a message naming `skills` and the
  skill.
- Seven unbundled plugins show 5 entries and `and 2 more`; a name of `"é".repeat(500)` is cut to 80 chars without panicking.
- The existing `"init plugins"` case keeps failing (`{"name": "p"}` has no bundled markers).

Integration (`tests/claude_subscription.rs`, through the double):

- Regression for #429: with the double's default init, which now carries a bundled plugin, a basic
  `hi` request returns its text. Existing tests already exercise this once the default changes; add
  one named test, `builtin_cli_plugins_are_accepted`, so the regression is explicit.
- `unexpected_init_customization_is_refused`: the plugin case asserts the error contains `foo@market`.

## Manual Verification

1. With a `claude-subscription` account on Claude CLI `2.1.318` or later, run
   `cargo run -p fontaine -- -p "hey there" --model <account>/sonnet` from `rust/`. Expected: a
   reply, no `RequestError`. This isn't automated because the real CLI is never run in tests; the
   double reproduces the init shape captured above.
