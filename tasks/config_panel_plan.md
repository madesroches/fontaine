# Interactive `/config` Settings Panel Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/4

## Overview

Claude Code's `/config` opens an in-TUI settings menu that writes to the right settings file.
Fontaine gets a `/config` overlay that lists every settings key with its effective value and the
scope that supplied it (`default`, `user`, `project`, `project-local`, `cli`). A small set of
preference keys can be changed from the panel. Edits always go to the user file, through the
format-preserving editor `/theme` and `/model` already use. The panel never writes a project or
project-local file. When a higher scope overrides the key, the panel says so instead of writing
somewhere else.

## Current State

- **Loading.** `config::load` (`rust/fontaine-core/src/config.rs:1668`) reads four layers (CLI,
  project-local, project, user). User, project and CLI go through the public `parse_settings`
  (`:1677`, `:1682`, `:1710`); only project-local uses `parse_settings_with_hint` (`:1697`, defined
  at `:740`), which already parses the text into a raw `toml::Table` for the scope check. The layers
  are folded them low-to-high through
  `Config::apply` (`:1380`). `Config` keeps the merged values and `scopes: Vec<ScopeEntry>`
  (`:1241`), but not which layer set which key. Only rules (`SourcedRule`) and hooks/MCP servers
  (`Origin`) carry their source.
- **Scope policy.** `config_scope::RULES` (`rust/fontaine-core/src/config_scope.rs:132`) is the
  single per-key table, with a schemars-driven test (`every_settings_leaf_has_a_row`) that fails
  when a `Settings` leaf has no row. The panel's key catalog uses the same pattern.
- **User-file edits.** `user_config` (`rust/fontaine-core/src/user_config.rs`) has `set_model`,
  `set_guardian_model` and `set_theme` over a private `set_string` (`:78`). Each takes the
  cross-session file lock, edits through `toml_edit` and writes with
  `owner_only::write_atomic_keeping_mode`. Nothing re-validates the edited document before it is
  written.
- **TUI.** Slash commands come from the one `BUILTINS` table (`rust/fontaine-tui/src/commands.rs:61`),
  dispatched in `rust/fontaine-tui/src/ui/mod.rs:269`. `Ui` (with `user_config: Option<PathBuf>`) is
  defined in `rust/fontaine-tui/src/lib.rs:141`/`:153`. Overlays are `Option<…>` fields on `App`
  (`rust/fontaine-tui/src/app.rs:86`), drawn by `view::overlay` (`rust/fontaine-tui/src/view.rs:208`)
  in key-routing precedence order. The closest list-overlay precedents are `AgentsPanel`
  (`agents_panel.rs`) and `ModelPicker` (`model_picker.rs`, which also has a linear/screen-reader
  body). `/theme` (`ui/commands/appearance.rs`) shows the live-switch-and-persist path, including
  `finish_switch` (`ui/commands/model.rs:153`) for reporting a persist error.
- **Precedent decisions.** The master plan's Decisions make `/permissions`, `/status` and similar
  commands read-only notices (2026-10-24, #47). Rule editing stays in the config files
  (2026-10-21). `/autocompact` was folded into config only, "which belongs with `/config`"
  (2026-10-24, #4, #11).

## Design

### 1. Per-key provenance (core)

Add a provenance map to `Config`:

```rust
/// For each settings key a layer set, the layers that set it, highest precedence first.
/// Tables add a segment; arrays (including arrays of tables such as `hooks.pre_tool_use`) and
/// scalars are leaves, so `accounts.work.kind` and `env.HTTPS_PROXY` are leaves.
pub sources: BTreeMap<String, Vec<Slot>>,
```

`parse_settings_with_hint` returns the raw `toml::Table` next to `Settings` (it already builds
it). `load` keeps the table on each `Layer`, calling `parse_settings_with_hint(…, None)` for the
user, project and CLI layers to get theirs. After the merge it walks each loaded layer's table
from the highest-precedence layer down and appends the layer's `Slot` to every leaf path it
holds. Keeping all contributing slots answers both questions the panel asks: a scalar's winner is
`sources[path][0]`, and a list key (deny, ask, env…) shows every scope that contributed. The walk
is a standalone function over the raw TOML. It walks every table generically: tables add a
segment, arrays and scalars are leaves. A map-section summary row (`accounts`, `mcp.servers`,
`agents.types`, `env`, `telemetry.resource_attributes`) takes as its source label the union of
slots over every `sources` key with prefix `<section>.`, highest precedence first. It is generic, so `Config::apply` is unchanged and a new key needs no provenance
code. CLI flag rules (`--allowed-tools`, `--disallowed-tools`) are already folded into the CLI
layer's `Settings` before the merge. `load` appends them to the CLI layer's raw table too
(`permissions.allow` / `permissions.deny`) in every case, starting from an empty `toml::Table`
only when no `--settings` file was given, so they show up as `cli` with or without one. Labels come from `Slot::label()`; a key with no `sources` entry shows `default`.

`[env]` values are never shown in the panel: only names. (`EnvValue` already redacts
its `Debug`.)

### 2. Key catalog (core): `rust/fontaine-core/src/config_catalog.rs`

One descriptor table, written in the same style as `RULES`:

```rust
pub struct KeyRow {
    /// Dotted path, as in the scope table (`tui.theme`, `context.autocompact_window`).
    pub path: &'static str,
    /// The running value as the panel shows it, defaults included.
    pub effective: fn(&Config, &Running) -> String,
    /// How the panel may change it; `None` is read-only.
    pub edit: Option<Edit>,
    /// Whether a saved change applies to the running session or only from the next launch.
    pub applies: Applies,
}

/// Live state that diverges from `Config` once a session switches theme or model.
pub struct Running<'a> { pub theme: &'a str, pub main_model: &'a str, pub guardian_model: Option<&'a str> }

pub enum Edit {
    /// Cycles through fixed `(label, value)` pairs; a `None` value resets (removes) the key.
    Choice(fn() -> Vec<(&'static str, Option<toml_edit::Value>)>),
    /// A positive integer typed into a field; empty clears the key.
    PositiveInt,
    /// Opens an existing picker instead (`/model`, `/guardian-model`).
    Picker(ModelSlot),
}

pub enum ModelSlot { Main, Guardian }

pub enum Applies { Live, NextLaunch }

pub const KEYS: &[KeyRow] = &[ … ];
```

The TUI maps `ModelSlot` to its own `PickerPurpose`.

Rows:

- **Scalars**: every scalar leaf, each with an `effective` renderer. `Option` values render as
  `unset` plus what that means where the type documents it (`permissions.default_mode` →
  `unset (interactive: auto)`); unset `guardian.model` renders as
  `unset (best validated, else main model)`.
- Live edits never update `runtime.config`, so `tui.theme`, `model.model` and `guardian.model` render
  from `Running` (`app.theme`, `runtime.models.main.reference`, the guardian's current label); every
  other row renders from `Config`.
- **Lists** (including `hooks.*`, whose fields are `Option<Vec<Hook>>`): one row per list key, effective value `N entries`, with the contributing scopes.
- **Named tables / maps** (`accounts`, `mcp.servers`, `agents.types`, `env`,
  `telemetry.resource_attributes`): one summary row per section listing the entry names
  (env: names only), read-only.
- `guardian.screen.*` renders from `Config::guardian.screen` (`unset` when the stage is off).

Editable rows (all other rows are read-only, with the footer naming the file to edit):

| Key | Edit | Applies |
|---|---|---|
| `tui.theme` | `Choice` over `ThemeName::ALL` mapped through `as_str` | Live (same path as `/theme`) |
| `tui.screen_reader` | `Choice(true/false/unset)` | NextLaunch |
| `context.autocompact_window` | `PositiveInt` | NextLaunch |
| `schedule.enabled` | `Choice(true/false)` | NextLaunch |
| `model.model` | `Picker(Main)` | Live (existing `/model` path) |
| `guardian.model` | `Picker(Guardian)` | Live (existing `/guardian-model` path) |

Each `Choice` pair carries its typed TOML value (`true`/`false` → bool, theme → string); the
`unset` pair's value is `None`, and cycling onto it emits `ConfigKey::Reset(path)`.

An in-crate unit test (moving `schema_nodes` from `config_scope`'s tests into a shared
`#[cfg(test)]` helper; `Config::defaults` becomes `pub(crate)` if needed) fails when a `Settings`
leaf or map node has no catalog row. It matches nodes to rows by prefix, as the `RULES` test does
(`config_scope.rs:256-261,318`); plain sections need no row of their own.

### 3. Validated user-file writes (core): `user_config`

Generalize `set_string` to `set_value(path, &[&str] key_path, Option<toml_edit::Value>)`, where
`None` removes the key and drops a table left empty. Keep `set_model` / `set_guardian_model` /
`set_theme` as thin wrappers. Add `set_config_value(path, key: &str, value: Option<toml_edit::Value>)`
for the panel, taking the dotted catalog path. Before every write, `set_value` runs
`config::validate_settings(&doc.to_string(), path, Column::User)`. An edit that would make the
user file fail to load (e.g. `autocompact_window = 0`) is refused with the config error as the
`io::Error`'s inner error (Display shows it, `get_ref` reaches it), and the file is left untouched.

### 4. The panel (TUI): `rust/fontaine-tui/src/config_panel.rs`

```
┌ config ─────────────────────────────────────────────────────────┐
│ > tui.theme                    dark-ansi          user      ✎  │
│   tui.screen_reader            unset              default   ✎  │
│   context.autocompact_window   150000             project-local ✎ │
│   permissions.default_mode     manual             project       │
│   permissions.deny             3 entries          user, project │
│   accounts                     work, local        user          │
│ ...                                                              │
├──────────────────────────────────────────────────────────────────┤
│ ↑/↓ select · type to filter · Enter change · Del reset · Esc close │
│ saved to /home/u/.config/fontaine/config.toml                    │
└──────────────────────────────────────────────────────────────────┘
```

- `ConfigPanel { rows: Vec<PanelRow>, selected, filter: String, field: Option<String>, status: Option<String> }`
  is built from `KEYS`, the running `Config`, `Running` and the user file path. `PanelRow` holds the path,
  effective text, source label(s), edit kind, and `pending: Option<String>` (the label of a saved
  value that applies next launch, `unset` after a reset). `Ui` keeps `config_pending:
  BTreeMap<String, String>`, updated on every NextLaunch save or reset, and seeds `pending` from it
  each time the panel opens, so a pending value survives closing and reopening `/config`.
- Typed characters, including space, go into the filter (by path substring, as in the model picker). Up/Down select. Enter on a
  `Choice` row moves to the value after the row's `pending` label, else after its effective value
  (first pair if neither matches), and saves it. On a `PositiveInt` row it opens a text
  field, where Enter saves and Esc cancels. On a `Picker` row it closes the panel and opens the
  existing model picker. Del on a `Choice` or `PositiveInt` row removes the key from the user file
  (reset); on a Live row (`tui.theme`) the running theme also switches to the theme the other scopes give (the default if none sets one). Del is
  ignored on `Picker` rows (reset `model.model` / `guardian.model` by editing the file) and on
  read-only rows. Backspace on an empty filter does
  nothing, as in the model picker. Esc closes.
- `handle_key` returns `ConfigKey::{None, Close, Save { path, value: toml_edit::Value }, Reset(path), OpenPicker(ModelSlot)}`,
  mirroring `PanelKey`. `Ui` does the I/O: `user_config::set_config_value`, then for `Live` rows the
  same runtime switch `/theme` does. The panel's status line reports the outcome:
  - saved to the user file;
  - when `sources[path][0]` outranks `Slot::User` (the panel never writes that other scope's
    file), by `Applies`: Live rows: "applied now; `<scope>` sets this key and wins from the next
    launch"; NextLaunch rows: "saved, but `<scope>` sets this key and wins, so the value stays
    `<value>`";
  - saved; applies from the next launch (row shows `pending`);
  - not saved: the error chain (`fontaine_tools::error_chain::chain`), or "there is no user
    settings file" when `user_config` is `None`.
- Picker rows close the panel before the picker opens, so a model pick reports through the picker
  completion path as a transcript notice (`finish_switch`, `ui/commands/model.rs:130-153`; the
  `answer_unavailable` `use_guardian` path, `model.rs:225-233`, also persists `guardian.model`). That
  path, for `/model` and `/guardian-model` too, appends "`<scope>` sets this key and wins from the
  next launch" when `runtime.config.sources[path][0]` is not `Slot::User`.
- `App.config: Option<ConfigPanel>`, routed in `App::handle_key`, counted in `App::covered()`
  (`app.rs:466-475`, the any-overlay-open check gating paste, slash menu, @-completion, input
  dimming and Ctrl+C), and drawn in `view::overlay` next to the agents panel. A `body_as(linear)` variant renders one plain line per row for the
  screen-reader renderer, like `ModelPicker`.
- `Command::Config` in `BUILTINS` (`/config`, alias `/settings`, as in Claude Code), no
  argument.

## Implementation Steps

1. **Provenance**: `parse_settings_with_hint` returns `(Settings, toml::Table)`, and the public
   `parse_settings` keeps its signature. Add `Layer.table`, fold the CLI flag rules into it, add
   `Config::sources` and the standalone walker described in §1; `load` calls
   `parse_settings_with_hint(…, None)` for the user, project and CLI layers. Tests in `rust/fontaine-core/tests/config.rs`.
2. **`user_config::set_value` / `set_config_value`** with removal, empty-table cleanup and
   pre-write validation, with the existing wrappers rebased on it. Unit tests in `user_config.rs`.
3. **`config_catalog.rs`**: `KeyRow`, `Edit`, `Applies`, `ModelSlot`, `KEYS`. Shared
   `schema_nodes` test helper and the coverage test. Export it from `lib.rs`.
4. **`config_panel.rs`** in `fontaine-tui`: state, filtering, key handling, body/footer/linear
   body. Unit tests in the file.
5. **Wiring**: `Command::Config` + `BUILTINS` row; `App.config`, key routing, `App::covered()`,
   `view::overlay` branch; `Ui::config`, `Ui::config_pending` and the `ConfigKey` handler in a new
   `ui/commands/config.rs`, reusing `theme`'s switch (factor the "apply theme" part out of
   `Ui::theme`; the helper only switches `app.theme`, with no persistence or reporting, so `/theme`
   keeps calling `finish_switch` and the panel sets `panel.status`) and the picker-opening code
   from `Ui::model` / `Ui::guardian_model`. The picker completion path appends the
   higher-scope-override sentence from §4.
6. **Docs** (below), then `CHANGELOG.md`, the parity matrix row, and the master plan Decisions.

## Files to Modify

- `rust/fontaine-core/src/config.rs`: `sources`, layer tables, parse return
- `rust/fontaine-core/src/config_scope.rs`: test helper moved out
- `rust/fontaine-core/src/config_catalog.rs` (new)
- `rust/fontaine-core/src/user_config.rs`: `set_value`, `set_config_value`, validation
- `rust/fontaine-core/src/lib.rs`
- `rust/fontaine-core/tests/config.rs`
- `rust/fontaine-tui/src/config_panel.rs` (new)
- `rust/fontaine-tui/src/commands.rs`, `app.rs`, `view.rs`, `lib.rs`
- `rust/fontaine-tui/src/ui/mod.rs`, `ui/commands/mod.rs`, `ui/commands/config.rs` (new),
  `ui/commands/appearance.rs`, `ui/commands/model.rs`
- `rust/fontaine-tui/tests/tui.rs`
- `docs/tui.md`, `docs/configuration.md`, `docs/index.md`
- `CHANGELOG.md`, `research/research-claude-code-parity.md`, `tasks/fontaine_harness_plan.md`

## Decisions

- 2026-10-25: `/config` is an interactive overlay, unlike the read-only notices of `/status` and
  `/permissions`, because the issue's purpose is in-TUI editing; it edits preferences only and
  never permission rules, which stay in the config files.
- 2026-10-25: `/config` edits only `tui.theme`, `tui.screen_reader`, `context.autocompact_window`,
  `schedule.enabled` and (via the pickers) `model.model` / `guardian.model`. Every other key is
  shown read-only, because guardian, permission, account, network, telemetry, env, hook and MCP
  keys loosen or redirect the security pipeline or send data off the machine and should need a
  deliberate file edit.
- 2026-10-25: `/config` writes only the user file. When a higher scope sets the key it saves
  anyway and says which scope wins, rather than writing the project or project-local file
  (Claude Code writes whichever settings file it chooses).
- 2026-10-25: `tui.screen_reader`, `context.autocompact_window` and `schedule.enabled` changes
  apply from the next launch (Claude Code applies most `/config` changes at once), because each is
  read once at startup (renderer choice, compaction threshold, scheduling tool registration).
  Re-plumbing them live is its own change.
- 2026-10-25: `user_config::set_value` and its wrappers validate the edited document against the
  user column before writing, so no Fontaine-made edit through them can leave the user file
  unloadable.
- 2026-10-25: AFK: NextLaunch pending values persist across panel opens in a session-scoped `Ui::config_pending` map that seeds each `PanelRow::pending`, rather than re-reading the user file at open, because it needs no file read or comparison logic and edits from other sessions are out of scope.
- 2026-10-25: AFK: `Edit::Choice` is a typed list of `(label, Option<toml_edit::Value>)` pairs with `None` meaning reset, and `ConfigKey::Save.value` is a `toml_edit::Value`, because the panel must hand `set_config_value` a typed value and the `unset` entry must map to a removal.
- 2026-10-25: AFK: the panel shows only config-layer provenance; `--model`, the screen-reader flag and `FONTAINE_AX_SCREEN_READER` overrides outside `Config` are not named as sources (the model rows still render the running model), because those are per-launch overrides rather than config scopes, which is what the issue asks the panel to show, and plumbing their sources into the panel adds state for little gain.
- 2026-10-25: AFK: `load` appends the `--allowed-tools` / `--disallowed-tools` rules to the CLI layer's raw table in every case, starting from an empty table only without `--settings`, because otherwise a `--settings` file plus flag rules would omit `cli` from those rules' sources.
- 2026-10-25: AFK: Enter on a `Choice` row cycles from the row's pending value, else its effective value, because with pending kept across opens this avoids re-saving an overriding scope's value without reading the user file.
- 2026-10-25: AFK: Del resets `Choice`/`PositiveInt` rows only (a `tui.theme` reset also switches the running theme to what the other scopes give, else the default) and is ignored on `Picker` and read-only rows, because removing `model.model` sends the next launch to the setup screen and a live `guardian.model` reset has no defined switch path.
- 2026-10-25: AFK: provenance comes from walking each layer's raw TOML rather than tracking in `Config::apply`, and the panel renders through a catalog over `Config` rather than raw TOML, because the walk needs no per-field code and only the merged `Config` shows defaults and merged lists; this replaces the plan's Trade-offs section.

## Documentation

- The `/model` and `/guardian-model` rows in `docs/tui.md` (and "Choosing a model" in `docs/configuration.md`) mention the higher-scope override note.
- `docs/tui.md`: `/config`, `/settings` row in the commands table and a short "Settings panel"
  section covering keys, the source column, which keys are editable, overridden-by-scope and
  next-launch behavior.
- The "Settings panel" section notes that the source column names config scopes only: a value set by
  a CLI flag or environment variable (`--model`, screen-reader mode) is not labelled as such.
- `docs/configuration.md`: one paragraph saying the TUI's `/config` shows each key's effective
  value and source and edits a few preferences in the user file. Link from the "The settings scope stack"
  section.
- `docs/index.md`: update its slash-command summary (lines ~27-31) to mention `/config`.
- `CHANGELOG.md` entry. `research/research-claude-code-parity.md:280` → `partial`/`done` with
  the deviations. Master plan Decisions get the lines above.

## Testing Strategy

Unit tests (all behavior reachable with constructed inputs):

- **Provenance** (`tests/config.rs`): a key set in user only → `[User]`; in user and project-local →
  `[ProjectLocal, User]`; a list in user and project → both; `--disallowed-tools` → `Cli`; `--settings f.toml --disallowed-tools X` → the rule path includes `Cli`; an unset
  key → absent; `[env]` paths present by name.
- **`user_config`**: set/replace/remove keeps comments and other tables; removal drops an emptied
  table; an invalid value (`autocompact_window = 0`, a non-theme string) is refused and the file is
  byte-identical afterwards; the existing tests still pass on the rebased wrappers.
- **Catalog**: every schema leaf/section has a row; each
  `effective` renderer on `Config::defaults` gives the documented default (spot-check theme,
  `schedule.enabled = true`, `agents.max_parallel = 4`); `env` row never contains a value.
- **Panel**: filtering; Up/Down bounds; Enter cycles a `Choice` and emits `Save` (cycling onto `unset` emits `Reset`); Backspace on an empty filter emits nothing; Enter cycles from `pending` when set; `PositiveInt`
  field accepts digits only, empty → `Reset`; Del → `Reset` on `Choice`/`PositiveInt` rows and nothing on
  `Picker` and read-only rows; a panel built with a seeded pending value shows it; `Picker` row → `OpenPicker`;
  overridden row's status text names the winning scope; read-only rows emit nothing; linear body
  has one line per row with no box-drawing characters.
- **Commands**: `parse_command("/config")` and `/settings` → `Command::Config`; menu lists it.
- **TUI integration** (`tests/tui.rs`, driving `Ui` with a temp config dir as the existing tests
  do): open `/config`, change the theme row, assert the user file has `[tui] theme` and the app
  theme switched; set `autocompact_window` while a CLI `--settings` file sets it, assert the user file
  changed, any project-local/project files present are byte-identical, and the status names `cli`.
  That last check pins the "never silently to project scope" requirement end to end.

## Open Questions

None blocking. The edit set is a conservative starting point that can grow key by key, each one
needing its own Decisions line.
