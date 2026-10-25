# Interactive `/config` Settings Panel Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/4

## Overview

Claude Code's `/config` opens an in-TUI settings menu that writes to the right settings file.
Fontaine gets a `/config` overlay that lists every settings key with its effective value and the
scope that supplied it (`default`, `user`, `project`, `project-local`, `CLI`). A small set of
preference keys can be changed from the panel. Edits always go to the user file, through the
format-preserving editor `/theme` and `/model` already use. The panel never writes a project or
project-local file. When a higher scope overrides the key, the panel says so instead of writing
somewhere else.

## Current State

- **Loading.** `config::load` (`rust/fontaine-core/src/config.rs:1668`) reads four layers (CLI,
  project-local, project, user), parses each with `parse_settings_with_hint` (`:740`, which already
  parses the text into a raw `toml::Table` for the scope check) and folds them low-to-high through
  `Config::apply` (`:1380`). `Config` keeps the merged values and `scopes: Vec<ScopeEntry>`
  (`:1241`), but not which layer set which key. Only rules (`SourcedRule`) and hooks/MCP servers
  (`Origin`) carry their source.
- **Scope policy.** `config_scope::RULES` (`rust/fontaine-core/src/config_scope.rs:139`) is the
  single per-key table, with a schemars-driven test (`every_settings_leaf_has_a_row`) that fails
  when a `Settings` leaf has no row. The panel's key catalog uses the same pattern.
- **User-file edits.** `user_config` (`rust/fontaine-core/src/user_config.rs`) has `set_model`,
  `set_guardian_model` and `set_theme` over a private `set_string` (`:78`). Each takes the
  cross-session file lock, edits through `toml_edit` and writes with
  `owner_only::write_atomic_keeping_mode`. Nothing re-validates the edited document before it is
  written.
- **TUI.** Slash commands come from the one `BUILTINS` table (`rust/fontaine-tui/src/commands.rs:60`),
  dispatched in `rust/fontaine-tui/src/ui/mod.rs:269`. Overlays are `Option<…>` fields on `App`
  (`rust/fontaine-tui/src/app.rs:86`), drawn by `view::overlay` (`rust/fontaine-tui/src/view.rs:208`)
  in key-routing precedence order. The closest list-overlay precedents are `AgentsPanel`
  (`agents_panel.rs`) and `ModelPicker` (`model_picker.rs`, which also has a linear/screen-reader
  body). `/theme` (`ui/commands/appearance.rs`) shows the live-switch-and-persist path, including
  `finish_switch` for reporting a persist error.
- **Precedent decisions.** The master plan's Decisions make `/permissions`, `/status` and similar
  commands read-only notices (2026-10-24, #47). Rule editing stays in the config files
  (2026-10-21). `/autocompact` was folded into config only, "which belongs with `/config`"
  (2026-10-24, #4, #11).

## Design

### 1. Per-key provenance (core)

Add a provenance map to `Config`:

```rust
/// For each settings key a layer set, the layers that set it, highest precedence first.
/// Paths use the scope table's convention: tables add a segment, a map entry adds its key,
/// arrays and scalars are leaves.
pub sources: BTreeMap<String, Vec<Slot>>,
```

`parse_settings_with_hint` returns the raw `toml::Table` next to `Settings` (it already builds
it). `load` keeps the table on each `Layer`. After the merge it walks each loaded layer's table
from the highest-precedence layer down and appends the layer's `Slot` to every leaf path it
holds. Keeping all contributing slots answers both questions the panel asks: a scalar's winner is
`sources[path][0]`, and a list key (deny, ask, env…) shows every scope that contributed. The walk
is generic over the raw TOML, so `Config::apply` is unchanged and a new key needs no provenance
code. CLI flag rules (`--allowed-tools`, `--disallowed-tools`) are already folded into the CLI
layer's `Settings` before the merge. The walk inserts them into that layer's table as well, so they
show up as `CLI`.

`[env]` values are never put in `sources` or the panel: only names. (`EnvValue` already redacts
its `Debug`.)

### 2. Key catalog (core): `rust/fontaine-core/src/config_catalog.rs`

One descriptor table, written in the same style as `RULES`:

```rust
pub struct KeyRow {
    /// Dotted path, as in the scope table (`tui.theme`, `context.autocompact_window`).
    pub path: &'static str,
    /// The running value as the panel shows it, defaults included.
    pub effective: fn(&Config) -> String,
    /// How the panel may change it; `None` is read-only.
    pub edit: Option<Edit>,
    /// Whether a saved change applies to the running session or only from the next launch.
    pub applies: Applies,
}

pub enum Edit {
    /// Cycles through fixed values (`tui.theme`'s names, `true`/`false`).
    Choice(fn() -> Vec<&'static str>),
    /// A positive integer typed into a field; empty clears the key.
    PositiveInt,
    /// Opens an existing picker instead (`/model`, `/guardian-model`).
    Picker(PickerPurpose),
}

pub enum Applies { Live, NextLaunch }

pub const KEYS: &[KeyRow] = &[ … ];
```

`PickerPurpose` lives in the TUI crate. The core enum therefore carries a plain
`Picker(ModelSlot)` (`Main` / `Guardian`), and the TUI maps it to its own picker.

Rows:

- **Scalars**: every scalar leaf, each with an `effective` renderer. `Option` values render as
  `unset` plus what that means where the type documents it (`permissions.default_mode` →
  `unset (interactive: auto)`, `guardian.model` → `unset (best validated)`).
- **Lists**: one row per list key, effective value `N entries`, with the contributing scopes.
- **Named tables / maps** (`accounts`, `mcp.servers`, `agents.types`, `hooks.*`, `env`,
  `telemetry.resource_attributes`, `network`): one summary row per section listing the entry names
  (env: names only), read-only.
- `guardian.screen.*` renders from `Config::guardian.screen` (`unset` when the stage is off).

Editable rows (all other rows are read-only, with the footer naming the file to edit):

| Key | Edit | Applies |
|---|---|---|
| `tui.theme` | `Choice(ThemeName::ALL)` | Live (same path as `/theme`) |
| `tui.screen_reader` | `Choice(true/false/unset)` | NextLaunch |
| `context.autocompact_window` | `PositiveInt` | NextLaunch |
| `schedule.enabled` | `Choice(true/false)` | NextLaunch |
| `model.model` | `Picker(Main)` | Live (existing `/model` path) |
| `guardian.model` | `Picker(Guardian)` | Live (existing `/guardian-model` path) |

Permission, guardian-tuning, auto-mode, account, network, telemetry, env, hook and MCP keys stay
read-only. Those keys loosen or redirect the security pipeline or send data off the machine, so
changing them takes a deliberate edit of the file (Decisions).

A unit test (moving `schema_nodes` from `config_scope`'s tests into a shared `#[cfg(test)]`
helper) fails when a `Settings` leaf, or a map/section node, has no catalog row. A second test
checks that every editable row's path has a user-column `RULES` row. This always holds today
because the user column allows everything. It is there so a future user-file restriction cannot
be bypassed by the panel.

### 3. Validated user-file writes (core): `user_config`

Generalize `set_string` to `set_value(path, &[&str] key_path, Option<toml_edit::Value>)`, where
`None` removes the key and drops a table left empty. Keep `set_model` / `set_guardian_model` /
`set_theme` as thin wrappers. Add `set_config_value(path, key: &str, value: Option<toml_edit::Value>)`
for the panel, taking the dotted catalog path. Before every write, `set_value` runs
`config::validate_settings(&doc.to_string(), path, Column::User)`. An edit that would make the
user file fail to load (e.g. `autocompact_window = 0`) is refused with the config error as its
`source()`, and the file is left untouched.

### 4. The panel (TUI): `rust/fontaine-tui/src/config_panel.rs`

```
┌ config ─────────────────────────────────────────────────────────┐
│ > tui.theme                    dark-ansi          user      ✎  │
│   tui.screen_reader            unset              default   ✎  │
│   context.autocompact_window   150000             project-local ✎ (user value 90000 is overridden) │
│   permissions.default_mode     manual             project       │
│   permissions.deny             3 entries          user, project │
│   accounts                     work, local        user          │
│ ...                                                              │
├──────────────────────────────────────────────────────────────────┤
│ ↑/↓ select · type to filter · Enter/Space change · Del reset · Esc close │
│ saved to /home/u/.config/fontaine/config.toml                    │
└──────────────────────────────────────────────────────────────────┘
```

- `ConfigPanel { rows: Vec<PanelRow>, selected, filter: String, field: Option<String>, status: Option<String> }`
  is built from `KEYS`, the running `Config` and the user file path. `PanelRow` holds the path,
  effective text, source label(s), edit kind, and `pending: Option<String>` (a saved value that
  applies next launch).
- Typing filters by path substring, as in the model picker. Up/Down select. Enter or Space on a
  `Choice` row moves to the next value and saves it. On a `PositiveInt` row it opens a text
  field, where Enter saves and Esc cancels. On a `Picker` row it closes the panel and opens the
  existing model picker. Del/Backspace on an empty filter removes the key from the user file
  (reset). Esc closes.
- `handle_key` returns `ConfigKey::{None, Close, Save { path, value }, Reset(path), OpenPicker(ModelSlot)}`,
  mirroring `PanelKey`. `Ui` does the I/O: `user_config::set_config_value`, then for `Live` rows the
  same runtime switch `/theme` does. The panel's status line reports the outcome:
  - saved to the user file;
  - saved, but `<scope>` sets this key and wins, so the running and next-launch value stays
    `<value>` (shown when `sources[path][0]` outranks `Slot::User`). The panel never writes that
    other scope's file;
  - saved; applies from the next launch (row shows `pending`);
  - not saved: the error chain (`fontaine_tools::error_chain::chain`), or "there is no user
    settings file" when `user_config` is `None`.
- `App.config: Option<ConfigPanel>`, routed in `App::handle_key` and drawn in `view::overlay`
  next to the agents panel. It sits after the model picker and provider dialog, so a picker opened
  from the panel takes the keys. A `body_as(linear)` variant renders one plain line per row for the
  screen-reader renderer, like `ModelPicker`.
- `Command::Config` in `BUILTINS` (`/config`, alias `/settings`, as in Claude Code), no
  argument. Only between turns, like `/login`, because a live switch (theme, model) mid-turn is
  already gated that way for `/model`.

### Data flow

```
/config ─► Ui::config(runtime) ─► ConfigPanel::new(&KEYS, &runtime.config, user_file)
key ─► ConfigPanel::handle_key ─► ConfigKey::Save{path,value}
      ─► user_config::set_config_value(user_file, path, value)   (lock → edit → validate → write)
      ─► Applies::Live ? apply to runtime (theme / picker path) : mark row pending
      ─► panel.status = outcome (incl. "overridden by <scope>")
```

## Implementation Steps

1. **Provenance**: `parse_settings_with_hint` returns `(Settings, toml::Table)`, and the public
   `parse_settings` keeps its signature. Add `Layer.table`, fold the CLI flag rules into it, add
   `Config::sources` and the walker (reuse the path convention from `config_scope::walk`, factoring
   the leaf-path iterator out so both use it). Tests in `rust/fontaine-core/tests/config.rs`.
2. **`user_config::set_value` / `set_config_value`** with removal, empty-table cleanup and
   pre-write validation, with the existing wrappers rebased on it. Unit tests in `user_config.rs`.
3. **`config_catalog.rs`**: `KeyRow`, `Edit`, `Applies`, `ModelSlot`, `KEYS`. Shared
   `schema_nodes` test helper and the two coverage tests. Export it from `lib.rs`.
4. **`config_panel.rs`** in `fontaine-tui`: state, filtering, key handling, body/footer/linear
   body. Unit tests in the file.
5. **Wiring**: `Command::Config` + `BUILTINS` row; `App.config`, key routing, `view::overlay`
   branch; `Ui::config` and the `ConfigKey` handler in a new `ui/commands/config.rs`, reusing
   `theme`'s switch (factor the "apply theme" part out of `Ui::theme`) and the picker-opening
   code from `Ui::model` / `Ui::guardian_model`.
6. **Docs** (below), then `CHANGELOG.md`, the parity matrix row, and the master plan Decisions.

## Files to Modify

- `rust/fontaine-core/src/config.rs`: `sources`, layer tables, parse return
- `rust/fontaine-core/src/config_scope.rs`: shared leaf-path walk; test helper moved out
- `rust/fontaine-core/src/config_catalog.rs` (new)
- `rust/fontaine-core/src/user_config.rs`: `set_value`, `set_config_value`, validation
- `rust/fontaine-core/src/lib.rs`
- `rust/fontaine-core/tests/config.rs`
- `rust/fontaine-tui/src/config_panel.rs` (new)
- `rust/fontaine-tui/src/commands.rs`, `app.rs`, `view.rs`, `lib.rs`
- `rust/fontaine-tui/src/ui/mod.rs`, `ui/commands/mod.rs`, `ui/commands/config.rs` (new),
  `ui/commands/appearance.rs`, `ui/commands/model.rs`
- `rust/fontaine-tui/tests/tui.rs`
- `docs/tui.md`, `docs/configuration.md`
- `CHANGELOG.md`, `research/research-claude-code-parity.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Provenance from raw TOML vs. tracking in `Config::apply`.** Tracking inside `apply` means a
  line per field and a new place to forget when a key is added. Walking the raw tables is generic
  and uses the scope table's path convention, so provenance can't drift from the scope policy.
- **Catalog table vs. rendering raw TOML.** Rendering the winning layer's raw value would need no
  catalog but cannot show defaults or the merged result of lists/maps. A catalog with an
  `effective` renderer over `Config` shows what is actually running, and the coverage test keeps
  it complete, as `RULES`' test does.
- **Re-reading files on open vs. the startup `Config`.** The panel shows the running session's
  values. It marks values saved for the next launch as `pending`, so it never shows a value that
  isn't actually running as if it were.
- **Narrow edit set vs. every key.** Claude Code's menu exposes a curated list of preferences,
  not every setting. Fontaine's settings include the guardian and permission policy, so the
  panel's edit set is limited to preferences that cannot loosen a decision.

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
- 2026-10-25: Every `user_config` write validates the edited document against the user column
  before writing, so no Fontaine-made edit can leave the user file unloadable.

## Documentation

- `docs/tui.md`: `/config`, `/settings` row in the commands table and a short "Settings panel"
  section covering keys, the source column, which keys are editable, overridden-by-scope and
  next-launch behavior.
- `docs/configuration.md`: one paragraph saying the TUI's `/config` shows each key's effective
  value and source and edits a few preferences in the user file. Link from the scope-stack
  section.
- `CHANGELOG.md` entry. `research/research-claude-code-parity.md:280` → `partial`/`done` with
  the deviations. Master plan Decisions get the lines above.

## Testing Strategy

Unit tests (all behavior reachable with constructed inputs):

- **Provenance** (`tests/config.rs`): a key set in user only → `[User]`; in user and project-local →
  `[ProjectLocal, User]`; a list in user and project → both; `--disallowed-tools` → `Cli`; an unset
  key → absent; `[env]` paths present by name with no value stored.
- **`user_config`**: set/replace/remove keeps comments and other tables; removal drops an emptied
  table; an invalid value (`autocompact_window = 0`, a non-theme string) is refused and the file is
  byte-identical afterwards; the existing tests still pass on the rebased wrappers.
- **Catalog**: every schema leaf/section has a row; every editable path is user-settable; each
  `effective` renderer on `Config::defaults` gives the documented default (spot-check theme,
  `schedule.enabled = true`, `agents.max_parallel = 4`); `env` row never contains a value.
- **Panel**: filtering; Up/Down bounds; Enter cycles a `Choice` and emits `Save`; `PositiveInt`
  field accepts digits only, empty → `Reset`; Del → `Reset`; `Picker` row → `OpenPicker`;
  overridden row's status text names the winning scope; read-only rows emit nothing; linear body
  has one line per row with no box-drawing characters.
- **Commands**: `parse_command("/config")` and `/settings` → `Command::Config`; menu lists it.
- **TUI integration** (`tests/tui.rs`, driving `Ui` with a temp config dir as the existing tests
  do): open `/config`, change the theme row, assert the user file has `[tui] theme` and the app
  theme switched; set `autocompact_window` while a project-local file sets it, assert the user file
  changed, the project-local file is byte-identical, and the status names `project-local`. That
  last check pins the "never silently to project scope" requirement end to end.

## Open Questions

None blocking. The edit set is a conservative starting point that can grow key by key, each one
needing its own Decisions line.
