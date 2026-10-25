# Config Scope Policy Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/182

## Overview

"Project scope can only tighten" is enforced by `check_scope`'s hand-written `if` chain plus
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
  1. all non-user columns (CLI included): scanner keys and every `[guardian.screen]` key except `shadow = true` (a missing `shadow` and `shadow = true` are accepted; `shadow = false` and any other screen key are refused);
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

The table goes in a new file `rust/fontaine-core/src/config_scope.rs` (declared with `pub(crate) mod config_scope;` in
`rust/fontaine-core/src/lib.rs`; `pub(crate)` items) holding only the table, the `Rule`/`Cell` types and the walker. `check_scope`
becomes a thin call into it.

Each row is a `Rule` of three cells, one per non-user column:

```text
enum Cell { No, Yes, If(fn(&toml::Value) -> bool) }
struct Rule { path: &'static str, project: Cell, local: Cell, cli: Cell, reason: &'static str }
```

The five policies (any scope; user file, CLI and project-local; user file and CLI; user file only;
value-dependent) are patterns over `(project, local, cli)`, built with `const` constructors
`Rule::any(path)`, `user_trust(path, reason)` (takes a reason: `env` has its own, the other rows pass the "shared project settings may only tighten" text), `user_or_cli`, `user_only`, `shadow_true` (`If` in all three columns), `if_project_local(path, pred, reason)` (`If` for project and local, `Yes` for CLI; used by `permissions.default_mode` and `telemetry.enabled`), so the
table stays one line per key. `If` predicates are the
value-dependent cells: `default_mode ∈ {manual, dontAsk}` (project, local), `enabled == false`
(project, local), `shadow == true` (project, local, cli), set on the `guardian.screen.shadow` leaf; every other `guardian.screen.*` leaf is `No` for project, local and cli. Each predicate is a named `fn`, unit-tested.
The user column is never consulted (user may set everything).

Row paths are plain dotted prefixes (no wildcard); map keys are covered by the row for the map's path.
Arrays of tables (`[[hooks.pre_tool_use]]`) are traversed without an index segment. `HooksSection`
has fixed fields, so hooks get one row per event (`hooks.pre_tool_use`, `hooks.post_tool_use`,
`hooks.permission_denied`), each covering its elements.

Row semantics: a row's path matches a node exactly and covers its descendants. The walker takes the
first row matching a node and stops there. Interior nodes with no row are descended into, never
denied (a bare `[guardian]`, `[permissions]`, `[telemetry]`, `[auto_mode]`, `[hooks]` is accepted
today); only leaves and nodes matching a row are decision points. One row per subtree: `env` (not
`env` plus per-key rows), `accounts` (not also per-account rows), `mcp.servers`, `agents`, and so on, used where
the subtree is wholly one policy. Section-level rows (`accounts`, `network`, `tui`, `env`, `model`,
`agents`, `context`, `shell_preclassifier`) match the interior node itself, so a present-but-empty
table is still refused as it is today; `model` is such a row (`Settings.model` is
`Option<ModelSection>`, the real leaf is `model.model`). The reported key is the matched row's path.

Reasons are carried per row, reusing today's strings verbatim so error text is unchanged apart from
the key (`tests/config.rs` asserts key text only at the `env` case, which keeps its section-level row); several keys share a reason through shared `const &str`s (`TIGHTEN`, `USER_FILE`, ...).

### Deny by default

`check_scope(s, column)` becomes `check_scope(table: &toml::Table, column)` (it drops `&Settings`).
`parse_settings_with_hint` calls `toml::from_str::<toml::Table>(text).expect("typed parse already accepted this text")`
after the typed parse and passes it in:

1. `Column::User` → Ok.
2. Walk the table (the typed parse has already validated the schema, so unknown keys keep their
   current `Parse` error). For each node, find the first row whose path matches; a match decides,
   otherwise descend.
3. No row → `Err` for `Project` and `ProjectLocal`; `Cli` accepts.
4. Row found → evaluate the cell for the column; `No` → `Err((path, row.reason))`; `If(p)` → `p`
   on the leaf value.

The tier shape of today's code (screen/scanner checked before the CLI early-return) falls out of the
table: those rows have `cli: No`.

### Completeness test

Enumerating `Settings` leaf paths needs the struct shape. Chosen approach: derive `schemars::JsonSchema`
on `Settings` and the section types, then walk `schemars::schema_for!(Settings)`, following `$ref`s
into `$defs`, `properties` for objects, a schema-valued `additionalProperties` for maps (as `*`; not
`additionalProperties: false`) and `items` for arrays, collecting leaf paths. String enums are leaves. Leaf types that are enums or newtypes (`PermissionMode`,
`GuardianProfile`, `Scanner`, `TelemetryBackend`, `ThemeName`, `AccountKind`, `EnvValue`, `Wire` (`config.rs:282`, value type of `accounts.*.wires.*`),
`fontaine_guardian::Question`, the agent-mode type) derive `JsonSchema`, except `AccountKind` and
`EnvValue`, which need `#[schemars(with = "String")]`. `fontaine_guardian::Question` (`screen.rs:30`)
derives only `Deserialize`, so it gets the `JsonSchema` derive. `Hook.origin` is
`serde(skip)` and gets `#[schemars(skip)]` (`Origin` needs no `JsonSchema`). `schemars.workspace = true` is added to
`fontaine-core`'s `Cargo.toml` (already in the workspace lockfile via `fontaine-guardian`, so no new crate);
`cargo deny check` must stay green. The schema walker unwraps `anyOf` with a null branch and type arrays containing `null` (Option fields).

Test assertions:

- every schema leaf path has a covering row (a row covers a schema node if its path is a prefix of it) — the "adding a field without a row
  fails" acceptance criterion;
- every row matches at least one schema node, interior or leaf (no dead rows left behind after a rename);
- no two rows overlap (one row's path a prefix of another's; first-match would otherwise hide one).

### Table-driven column tests

A second test derives accept/reject assertions from the rows themselves instead of three hand lists.
The snippets live in a test-only table in the `#[cfg(test)]` module of `config_scope.rs`, keyed by row path (one valid TOML snippet per row; an (accept, reject) pair for `If`
rows). For each row and each of `Project`, `ProjectLocal`, `Cli` the snippet is passed through
`parse_settings` and asserted to be a `ConfigError::Scope` for `No` cells and *not* a `Scope` error
for `Yes` cells (other errors, such as value validation, are allowed — the test is about scope). The
completeness test needs paths only. A test asserts every row has a snippet entry and no entry lacks a row. These tests live in the `#[cfg(test)]` module of `config_scope.rs`.
The loader's file-to-column mapping (`config.rs:1837-1870`) is pinned by two small tests in `tests/config.rs`: a user-file-only key via `w.cli_file` + `load_with_settings`, and a local-rejected key (`[tui]` or `[accounts.x]`) via a trusted `w.local_file`, each expecting `Scope`.

The existing hand-written accept-side tests (`project_scope_accepts_the_tightening_cells`,
`project_local_scope_accepts_...`, `cli_scope_accepts_...`) stay: they pin that the loaded config
actually carries the values, which the table test does not.

### Removed code

`GuardianSection::scanner_keys_set`, `other_keys_set`,
`TelemetrySection::keys_other_than_enabled` and the body of `check_scope` are deleted, not reduced. `GuardianScreenSection::loosens` stays: the settings merge (`config.rs:~1604`) also calls it.

## Implementation Steps

1. Add `schemars` to `rust/fontaine-core/Cargo.toml`; derive/annotate `JsonSchema` on `Settings`,
   every section/entry struct and every leaf type reachable from them (`config.rs`,
   plus `fontaine_guardian::Question`, `screen.rs:30`).
2. Create `rust/fontaine-core/src/config_scope.rs` with `Cell`, `Rule`, the `RULES` table covering
   every current leaf, the predicate fns, and the walker returning `Result<(), (String, String)>`.
3. Replace `check_scope` and delete `scanner_keys_set`, `other_keys_set` and `keys_other_than_enabled` in
   `config.rs` (keep `loosens`); pass the `toml::Table` parsed in `parse_settings_with_hint`.
4. Add `#[cfg(test)]` unit tests inside `config_scope.rs` (completeness, no-dead-rows, no-overlap,
   predicate tests, the `parse_settings`-driven per-column test, and "a key with no row is rejected from project and project-local" using a
   synthetic table via an inner function that takes `&[Rule]`). Delete `NO_FOR_PROJECT`, `NO_FOR_LOCAL`, `USER_FILE_ONLY` and the loop
   tests from `tests/config.rs` other than `cli_scope_rejects_user_file_only_keys`, which is replaced by the two loader tests described under Table-driven column tests; keep `assert_scope_error` (`tests/config.rs:1381` uses it); keep the specific screen/env/telemetry tests that assert messages.
5. Docs and design: update `docs/configuration.md` (scope-table intro and a one-line note that
   every key has a declared policy and an undeclared key is refused from project scopes) and
   `tasks/design/config.md` (name the table as the single source). Add a `CHANGELOG.md` entry under `### Changed` in `[Unreleased]`.
6. Run from `rust/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
   `cargo deny check`, the two `fixture` evals.

## Files to Modify

- `rust/fontaine-core/Cargo.toml` (`schemars.workspace = true`)
- `rust/fontaine-core/src/config.rs` (derives, remove helpers, call the table)
- `rust/fontaine-core/src/config_scope.rs` (new)
- `rust/fontaine-core/src/lib.rs` (`pub(crate) mod config_scope;`)
- `rust/fontaine-guardian/src/screen.rs` (`JsonSchema` on `Question`)
- `rust/fontaine-core/tests/config.rs`
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

- AFK: 2026-10-25: named the `if_project_local` constructor (checked against `check_scope`: default_mode and telemetry.enabled are value-gated in project/local, unrestricted for CLI) and noted `user_trust` takes a reason (env's own, others the "tighten" text) -- applied, as it only clarifies the plan.
- AFK: 2026-10-25: moved the per-row TOML samples out of the production `Rule` into a test-only table keyed by row path in `config_scope.rs`'s `#[cfg(test)]` module, with a test that every row has an entry -- applied, as it keeps test data out of the release binary and `Rule` at path, cells, reason.
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
- unit tests in `config_scope.rs`: table-driven per-column accept/reject through `parse_settings` for every row;
- loader tests in `tests/config.rs`: CLI and trusted project-local mapping;
- the existing `tests/config.rs` scope tests that remain (screen shadow, env, telemetry, accounts
  network keys) pass unchanged, pinning the decisions and the error text.

## Open Questions

None.
