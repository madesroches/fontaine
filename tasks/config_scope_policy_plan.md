# Config Scope Policy Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/182

## Overview

"Project scope can only tighten" is enforced by `check_scope`'s hand-written `if` chain plus four
per-section helpers that list the keys to refuse. A key nobody lists is accepted from a checked-in
`.fontaine/settings.toml`, so the enforcement fails open. This plan replaces those lists with one
declarative policy table keyed by dotted path, makes the default deny, and adds a completeness test
that walks every `Settings` leaf path and fails until it has a row. The same table drives the
per-column accept/reject tests, replacing the hand-kept `NO_FOR_PROJECT` / `NO_FOR_LOCAL` /
`USER_FILE_ONLY` lists. No scope decision changes.

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

A new module `rust/fontaine-core/src/config/scope_policy.rs` is not needed; `config.rs` is already
2144 lines, so the table goes in a sibling file `rust/fontaine-core/src/config_scope.rs`
(`mod config_scope;`, `pub(crate)` items) holding only the table, the `Policy` type and the walker.
`check_scope` becomes a thin call into it.

```text
pub(crate) enum Policy {
    /// Every scope: the key can only tighten (deny/ask lists, hooks, mcp).
    Any,
    /// User file, CLI, and a user-trust project-local file; not shared project.
    UserTrust,
    /// User file and CLI only (accounts, network, tui, telemetry sinks).
    UserOrCli,
    /// User file only (scanner keys, screen stage knobs).
    UserOnly,
    /// Any scope, but the value must satisfy a predicate in the non-user-trust columns.
    Value { cli: Cell, local: Cell, project: Cell }   // see below
}
```

A cleaner encoding avoids the enum/`Value` split: each row is a `Rule` of three cells, one per
non-user column, because the five policies above are just patterns over `(project, local, cli)`:

```text
enum Cell { No, Yes, If(fn(&toml::Value) -> bool) }
struct Rule { path: &'static str, project: Cell, local: Cell, cli: Cell, reason: &'static str }
```

with `const` constructors `Rule::any(path)`, `user_trust(path, reason)`, `user_or_cli`,
`user_only`, `shadow_true`, etc. so the table stays one line per key. `If` predicates are the
value-dependent cells: `default_mode ∈ {manual, dontAsk}` (project, local), `enabled == false`
(project, local), `shadow == true` (project, local, cli). Each predicate is a named `fn`, unit-tested.
The user column is never consulted (user may set everything).

Paths use dotted segments; `*` matches exactly one segment (a map key such as an account name, or a
hook event). Arrays of tables (`[[hooks.pre_tool_use]]`) are traversed without an index segment, so
`hooks.*.command` names the field of every element. Rows are leaf-level; a row for an interior node
(for example `accounts.*`, `agents.types.*`) covers the whole subtree and is used only where the
subtree is a user-defined entry that is wholly one policy (accounts, MCP servers, agent types,
`env.*`, `telemetry.resource_attributes`).

Reasons are carried per row, reusing today's strings verbatim so error text is unchanged apart from
the key; several keys share a reason through shared `const &str`s (`TIGHTEN`, `USER_FILE`, ...).

### Deny by default

`check_scope(s, column)` becomes `check_scope(text_table: &toml::Table, column)`:

1. `Column::User` → Ok.
2. Walk the raw `toml::Table` (the same text, parsed once more as `toml::Table` after the typed
   parse has already validated the schema, so unknown keys keep their current `Parse` error). For
   each leaf (or each node that matches a subtree row), find the first row whose path matches.
3. No row → `Err((path, DEFAULT_DENY_REASON))` for `Project`, `ProjectLocal` **and `Cli`** is *not*
   chosen: the issue says deny in project and project-local; CLI is user-trust, so a missing row is
   a hard fail only in the two project columns and is also reported by the completeness test (so
   it can never ship). CLI with no row is accepted, matching "CLI accepts everything but the
   user-file-only keys".
4. Row found → evaluate the cell for the column; `No` → `Err((path, row.reason))`; `If(p)` → `p`
   on the leaf value.

Walking order is document order (the `toml::Table` is a `BTreeMap`, so sorted by key), not today's
tier order. When a file violates several rules the reported key can change. Existing tests assert
only that a scope error occurs or name a single key; the plan keeps the test for the multi-violation
case out of scope. The `Decisions` section records the accepted change.

The tier shape of today's code (screen/scanner checked before the CLI early-return) falls out of the
table: those rows have `cli: No`.

### Completeness test

Enumerating `Settings` leaf paths needs the struct shape. Chosen approach: derive `schemars::JsonSchema`
on `Settings` and the section types, then walk `schemars::schema_for!(Settings)`, following `$ref`s
into `$defs`, `properties` for objects, `additionalProperties` for maps (as `*`) and `items` for
arrays, collecting leaf paths. Leaf types that are enums or newtypes (`PermissionMode`,
`GuardianProfile`, `Scanner`, `TelemetryBackend`, `ThemeName`, `AccountKind`, `EnvValue`,
`fontaine_guardian::Question`, the agent-mode type) either derive `JsonSchema` or use
`#[schemars(with = "String")]` where their `Deserialize` is hand-written. `Origin` and other
`serde(skip)` fields use `#[schemars(skip)]`. `schemars` is added to `fontaine-core` as a regular
dependency (already in the workspace lockfile via `fontaine-guardian`, so no new crate);
`cargo deny check` must stay green.

Fallback if the derive churn proves larger than expected during implementation: a test-only
`fn leaf_paths()` can be derived from serde's own error, deserializing `{ "<x>": 0 }` into each
section and parsing `expected one of` from `deny_unknown_fields`, but that is brittle and only used
if schemars cannot be applied cleanly. This is called out in Open Questions.

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
rejecting sample values explicitly in the table (`Cell::If { ok: "manual", bad: "plan", .. }`
extended with literal samples) so the test exercises both sides. Values that need cross-field
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
4. Port the multi-column tests: add `rust/fontaine-core/tests/config_scope.rs` (completeness,
   no-dead-rows, no-overlap, table-driven columns, predicate unit tests, "a key with no row is
   rejected from project and project-local" using a test-only table via an inner function that
   takes `&[Rule]`). Delete `NO_FOR_PROJECT`, `NO_FOR_LOCAL`, `USER_FILE_ONLY` and the three loop
   tests from `tests/config.rs`; keep the specific screen/env/telemetry tests that assert messages.
5. Docs and design: update `docs/configuration.md` (scope-table intro and a one-line note that
   every key has a declared policy and an undeclared key is refused from project scopes) and
   `tasks/design/config.md` (name the table as the single source). Add a dated line to
   `tasks/fontaine_harness_plan.md` `## Decisions` if the order-of-report change is judged a
   deviation; add a `CHANGELOG.md` entry.
6. Run from `rust/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
   `cargo deny check`, the two `fixture` evals.

## Files to Modify

- `rust/fontaine-core/Cargo.toml` (schemars)
- `rust/fontaine-core/src/config.rs` (derives, remove helpers, call the table)
- `rust/fontaine-core/src/config_scope.rs` (new)
- `rust/fontaine-core/src/lib.rs` (module declaration, if modules are declared there)
- `rust/fontaine-guardian/src/*` only if `Question` lacks `JsonSchema`
- `rust/fontaine-core/tests/config.rs`, `rust/fontaine-core/tests/config_scope.rs` (new)
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

(None yet.)

## Documentation

`docs/configuration.md` (scope table prose); `tasks/design/config.md` (scope-table paragraph);
`CHANGELOG.md`. The optional docs-column generation from the table is out of scope.

## Testing Strategy

All unit-level and reachable by constructed inputs, so no live or manual tier:

- completeness, dead-row and overlap checks against the schema;
- table-driven per-column accept/reject for every row, both sides of every `If` predicate;
- a synthetic table with an unlisted key proves deny-by-default for `Project`/`ProjectLocal`
  (the real table cannot be used because the completeness test forbids an unlisted key);
- the existing `tests/config.rs` scope tests that remain (screen shadow, env, telemetry, accounts
  network keys) pass unchanged, pinning "every decision is unchanged" and the error text.

## Open Questions

- Whether `JsonSchema` derives on every leaf type are acceptable in `fontaine-core`, or the
  serde-error enumeration fallback is preferred.
- Whether the multi-violation reporting order change needs a `## Decisions` line in the master plan.
