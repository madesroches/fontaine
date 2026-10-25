# Config Scope Policy Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/182

## Overview

"Project scope can only tighten" is enforced by `check_scope`'s hand-written `if` chain plus four
per-section helpers that list the keys to refuse. A key nobody lists is accepted from a checked-in
`.fontaine/settings.toml`, so the enforcement fails open. This plan replaces those lists with one
declarative policy table keyed by dotted path, makes the default deny, and adds a completeness test
that walks every `Settings` leaf path and fails until it has a row. The same table drives the
per-column accept/reject tests, replacing the hand-kept `NO_FOR_PROJECT` / `NO_FOR_LOCAL` /
`USER_FILE_ONLY` lists. No scope decision changes; the reported key becomes the matched row's path.

## Current State

Everything is in `rust/fontaine-core/src/config.rs`:

- `check_scope` (`config.rs:849`) is called from `parse_settings_with_hint` after the typed
  `toml::from_str::<Settings>` and returns `(key, reason)`, which becomes `ConfigError::Scope`.
  It is three tiers of `if`s, each returning early:
  1. all non-user columns (CLI included): scanner keys and non-`shadow = true` `[guardian.screen]`;
  2. then `Column::Cli` returns Ok; `Project` and `ProjectLocal` additionally refuse `accounts`,
     `network`, `tui`, `permissions.default_mode` outside `manual`/`dontAsk`, and every
     `[telemetry]` key but `enabled = false`;
  3. then `ProjectLocal` returns Ok; `Project` additionally refuses `env`, `model`, the `[guardian]`
     keys `model/profile/timeout_secs/confidence_threshold/cache`, `permissions.allow` and
     `additional_directories`, `auto_mode.environment`/`allow`, `agents`, `context`,
     `shell_preclassifier`.
- Helpers that mirror struct fields by hand: `GuardianSection::scanner_keys_set` and
  `other_keys_set` (`:448`, `:465`), `GuardianScreenSection::loosens` (`:436`),
  `TelemetrySection::keys_other_than_enabled` (`:622`, a `[(&str, bool); 10]`).
- Keys accepted everywhere without any line: `permissions.deny`/`ask`,
  `auto_mode.soft_deny`/`hard_deny`, `[[hooks.*]]`, `[mcp.servers.*]`, `telemetry.enabled = false`.
  Hooks and MCP servers are gated by workspace trust elsewhere, not by this check.
- `Settings` and its sections derive only `Deserialize` with `deny_unknown_fields`; nothing links the
  struct fields to the helpers. `fontaine-core` has no `schemars` dependency (`fontaine-guardian`
  does).
- `ConfigError::Scope { key, reason, column, hint }` renders
  ``{path}: `{key}` cannot be set from {column} scope: {reason}``.
- Tests: `rust/fontaine-core/tests/config.rs:578-740` (`NO_FOR_PROJECT`, `NO_FOR_LOCAL`,
  `USER_FILE_ONLY` and the tests that loop over them), plus accept-side tests (`:742-`) and
  screen/env tests near `:1183`, `:1353`.
- Docs: the "Per-key scope table" in `docs/configuration.md:84` and `tasks/design/config.md:171`.

## Design

### The table

The table goes in a new file `rust/fontaine-core/src/config_scope.rs` (`mod config_scope;`,
`pub(crate)` items) holding only the table, the `Rule`/`Cell` types and the walker. `check_scope`
becomes a thin call into it.

Each row is a `Rule` of three cells, one per non-user column:

```text
enum Cell { No, Yes, If(fn(&toml::Value) -> bool) }
struct Rule { path: &'static str, project: Cell, local: Cell, cli: Cell, reason: &'static str,
              samples: Option<(&'static str, &'static str)> }   // (accepted, rejected) TOML literals for If cells
```

The five policies (any scope; user file, CLI and project-local; user file and CLI; user file only;
value-dependent) are patterns over `(project, local, cli)`, built with `const` constructors
`Rule::any(path)`, `user_trust(path, reason)`, `user_or_cli`, `user_only`, `shadow_true`, etc. so the
table stays one line per key. `If` predicates are the
value-dependent cells: `default_mode ∈ {manual, dontAsk}` (project, local), `enabled == false`
(project, local), `shadow == true` (project, local, cli). Each predicate is a named `fn`, unit-tested.
The user column is never consulted (user may set everything).

Paths use dotted segments; `*` matches exactly one segment (a map key such as an account name, or a
hook event). Arrays of tables (`[[hooks.pre_tool_use]]`) are traversed without an index segment, so
`hooks.*.command` names the field of every element. Rows are leaf-level; a row for an interior node
(for example `accounts.*`, `agents.types.*`) covers the whole subtree and is used only where the
subtree is a user-defined entry that is wholly one policy (accounts, MCP servers, agent types,
`env.*`, `telemetry.resource_attributes`). Section-level rows (`accounts`, `network`, `tui`, `env`,
`agents`, `context`, `shell_preclassifier`, `guardian.screen`) match on the interior node itself, so
a present-but-empty table is still refused as it is today. The reported key is the matched row's
path.

Reasons are carried per row, reusing today's strings verbatim so error text is unchanged apart from
the key (`tests/config.rs` asserts key text only at the `env` case, which keeps its section-level row); several keys share a reason through shared `const &str`s (`TIGHTEN`, `USER_FILE`, ...).

### Deny by default

`check_scope(s, column)` becomes `check_scope(text_table: &toml::Table, column)`:

1. `Column::User` → Ok.
2. Walk the raw `toml::Table` (the same text, parsed once more as `toml::Table` after the typed
   parse has already validated the schema, so unknown keys keep their current `Parse` error). For
   each leaf (or each node that matches a subtree row), find the first row whose path matches.
3. No row → `Err` for `Project` and `ProjectLocal`; `Cli` accepts.
4. Row found → evaluate the cell for the column; `No` → `Err((path, row.reason))`; `If(p)` → `p`
   on the leaf value.

Walking order is sorted-key order (the `toml::Table` is a `BTreeMap`), not today's tier order.

The tier shape of today's code (screen/scanner checked before the CLI early-return) falls out of the
table: those rows have `cli: No`.

### Completeness test

Enumerating `Settings` leaf paths needs the struct shape. Chosen approach: derive `schemars::JsonSchema`
on `Settings` and the section types, then walk `schemars::schema_for!(Settings)`, following `$ref`s
into `$defs`, `properties` for objects, `additionalProperties` for maps (as `*`) and `items` for
arrays, collecting leaf paths. Leaf types that are enums or newtypes (`PermissionMode`,
`GuardianProfile`, `Scanner`, `TelemetryBackend`, `ThemeName`, `AccountKind`, `EnvValue`,
`fontaine_guardian::Question`, the agent-mode type) derive `JsonSchema`, except `AccountKind` and
`EnvValue`, which need `#[schemars(with = "String")]`. `fontaine_guardian::Question` (`screen.rs:30`)
derives only `Deserialize`, so it gets the derive (or the same attribute). `Hook.origin` is
`serde(skip)`, which schemars honors. `schemars` is added to `fontaine-core` as a regular
dependency (already in the workspace lockfile via `fontaine-guardian`, so no new crate);
`cargo deny check` must stay green.

Test assertions:

- every schema leaf path has a covering row (exact or wildcard) — the "adding a field without a row
  fails" acceptance criterion;
- every row matches at least one schema path (no dead rows left behind after a rename);
- no two rows overlap (first-match would otherwise hide one).

### Table-driven column tests

A second test derives accept/reject assertions from the rows themselves instead of three hand lists.
For each row and each of `Project`, `ProjectLocal`, `Cli` a *sample value* is needed to build a
valid TOML snippet. The schema walk supplies the type (`string`, `integer`, `number`, `boolean`,
`array`, `object`, enum values), from which a generator produces a minimal valid value (first enum
variant, `"x"`, `1`, `0.5`, `true`, `["x"]`); rows with `If` cells name their accepting and
rejecting sample literals in the row's `samples` field so the test exercises both sides. Values that need cross-field
validity (for example an `accounts.x` needs `kind`) use an override map in the test. The generated
snippet is passed through `parse_settings` for each column and asserted to be a `ConfigError::Scope`
for `No` cells and *not* a `Scope` error for `Yes` cells (other errors, such as value validation,
are allowed — the test is about scope).

The existing hand-written accept-side tests (`project_scope_accepts_the_tightening_cells`,
`project_local_scope_accepts_...`, `cli_scope_accepts_...`) stay: they pin that the loaded config
actually carries the values, which the table test does not.

### Removed code

`GuardianSection::scanner_keys_set`, `other_keys_set`, `GuardianScreenSection::loosens`,
`TelemetrySection::keys_other_than_enabled` and the body of `check_scope` are deleted, not reduced.

## Implementation Steps

1. Add `schemars` to `rust/fontaine-core/Cargo.toml`; derive/annotate `JsonSchema` on `Settings`,
   every section/entry struct and every leaf type reachable from them (`config.rs`,
   plus `fontaine_guardian::Question` if it does not derive it yet).
2. Create `rust/fontaine-core/src/config_scope.rs` with `Cell`, `Rule`, the `RULES` table covering
   every current leaf, the predicate fns, and the walker returning `Result<(), (String, String)>`.
3. Replace `check_scope` and the four helpers in `config.rs`; thread the `toml::Table` from
   `parse_settings_with_hint`.
4. Add `#[cfg(test)]` unit tests inside `config_scope.rs` (completeness, no-dead-rows, no-overlap,
   predicate tests, and "a key with no row is rejected from project and project-local" using a
   synthetic table via an inner function that takes `&[Rule]`). Add
   `rust/fontaine-core/tests/config_scope.rs` with only the `parse_settings`-driven per-column
   test. Delete `NO_FOR_PROJECT`, `NO_FOR_LOCAL`, `USER_FILE_ONLY` and the three loop
   tests from `tests/config.rs`; keep the specific screen/env/telemetry tests that assert messages.
5. Docs and design: update `docs/configuration.md` (scope-table intro and a one-line note that
   every key has a declared policy and an undeclared key is refused from project scopes) and
   `tasks/design/config.md` (name the table as the single source). Add a `CHANGELOG.md` entry.
6. Run from `rust/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
   `cargo deny check`, the two `fixture` evals.

## Files to Modify

- `rust/fontaine-core/Cargo.toml` (schemars)
- `rust/fontaine-core/src/config.rs` (derives, remove helpers, call the table)
- `rust/fontaine-core/src/config_scope.rs` (new)
- `rust/fontaine-core/src/lib.rs` (module declaration, if modules are declared there)
- `rust/fontaine-guardian/src/*` only if `Question` lacks `JsonSchema`
- `rust/fontaine-core/tests/config.rs`, `rust/fontaine-core/tests/config_scope.rs` (new, per-column test only; table-internal tests are unit tests in `config_scope.rs`)
- `docs/configuration.md`, `tasks/design/config.md`, `CHANGELOG.md`

## Trade-offs

- **Table vs. per-field attribute/derive macro.** A proc-macro adds a crate and hides the policy
  from a reader; a `const` table is greppable and reviewable as one block. The completeness test
  gives the same guarantee a derive would.
- **Walking the raw `toml::Table` vs. the typed `Settings`.** The typed walk would need an
  accessor per field (the problem being fixed). The raw walk sees exactly what the file wrote,
  including a key set to its default.
- **Schema enumeration for the test vs. a macro that emits the field list.** schemars adds derives
  but no new crate and no change to deserialization; a field-list macro would restructure every
  section struct.

## Decisions

- 2026-10-25: the reported key and the order in which multiple violations are reported are not stable across this change.

## Documentation

`docs/configuration.md` (scope table prose); `tasks/design/config.md` (scope-table paragraph);
`CHANGELOG.md`. The optional docs-column generation from the table is out of scope.

## Testing Strategy

All unit-level and reachable by constructed inputs, so no live or manual tier:

- unit tests in `config_scope.rs`: completeness, dead-row and overlap checks against the schema;
  predicates (both sides of every `If`); a synthetic table with an unlisted key proves
  deny-by-default for `Project`/`ProjectLocal` (the real table cannot be used because the
  completeness test forbids an unlisted key);
- `tests/config_scope.rs`: table-driven per-column accept/reject through `parse_settings` for every row;
- the existing `tests/config.rs` scope tests that remain (screen shadow, env, telemetry, accounts
  network keys) pass unchanged, pinning the decisions and the error text.

## Open Questions

None.
