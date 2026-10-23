# TUI Model Picker Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/117

## Overview

`/model` with no argument opens a selection overlay instead of printing one line. The overlay
groups models under one heading per configured account, marks the current model, lists recently
used models first, and filters as you type. Enter switches through the path `/model <ref>`
already uses. `/guardian-model` with no argument opens the same overlay, limited to the guardian
candidates. Claude Code's `/model` with no argument also opens an interactive picker. The
grouping, the filter box and the Recent group come from opencode's model dialog.

## Current State

- `fontaine-tui/src/lib.rs:657` `Driver::model`: with an empty argument it prints
  `model <ref> · guardian <label>`. With an argument it calls `AgentRuntime::switch_main_model`
  and reports through `finish_switch`.
- `fontaine-tui/src/lib.rs:680` `Driver::guardian_model`: with an empty argument it prints the
  guardian plus `available: <candidates>`. With an argument it calls `set_guardian_model`.
- `fontaine-core/src/agent.rs:883` `switch_main_model`: resolves through `Switcher::main`,
  rebuilds the agent, makes the guardian follow on `GuardianChoice::MainFallback`, and persists
  `[model] model`. `agent.rs:954` `guardian_candidates()` returns `Vec<ModelRef>` via
  `Switcher::candidates`. That covers the validated models the accounts offer, then every
  model an account lists explicitly. Accounts without a credential are skipped, and there is
  no network call.
- `fontaine-core/src/models.rs:346` `resolve_main_model`: an account's models are its explicit
  `models` list or, when that is absent, `client.list()`. Only the main model's account is ever
  listed. The result is kept as `Models::main_listing` (`models.rs:170`). `client_for`
  (`models.rs:239`) returns the reason no client can be built (no credential, not signed in,
  unset `api_key_env`). `with_causes` (`models.rs:277`) renders a listing failure.
- Overlays: `App` holds one `Option<…>` per overlay (`agents`, `denied`, `unavailable`,
  `server`, `approval`; `app.rs:140`). `App::covered()` (`app.rs:436`) and `App::handle_key`
  (`app.rs:444`) route keys to the open overlay. `view.rs:119` `overlay()` returns
  `(title, body, footer)`, and `draw_overlay` renders the body's **tail** when it is too tall.
  `DeniedList` (`denials.rs`) and `AgentsPanel` are the list-with-selection precedents. Their
  `handle_key` returns a small enum that `App` maps to an `Action`, and the driver dispatches
  that `Action` in its idle loop (`lib.rs:302`) and in three in-turn loops (`lib.rs:464`, `:592`,
  `:858`), which must stay exhaustive.
- Persistence precedent: `fontaine-tui/src/history.rs` keeps an owner-only file under
  `data_dir` (`lib.rs:961`). When the file cannot be used, it degrades to in-memory and reports
  the error once.
- `docs/tui.md:178-179` documents both commands.
- Tests: `fontaine-tui/tests/tui.rs:471` and `:498` cover the argument forms and the no-argument
  text output. The `World` harness (`tests/common/mod.rs`) supports config with wiremock-backed
  accounts.

## Design

### Core: listings for every account (`fontaine-core/src/models.rs`, `agent.rs`)

```rust
/// One configured account's models, or why they could not be listed.
pub struct AccountListing {
    pub account: String,
    pub kind: AccountKind,
    pub models: Result<Vec<String>, String>,
}
```

`Switcher::listings(&self, main: (&ModelRef, &[String])) -> Vec<AccountListing>` (async)
visits every account in `config.accounts` order:

- When the account has an explicit `models` list, use it with no call. This is the "models
  named in config" from the issue.
- When it is the main model's account, use the `main` listing that is already fetched, with no
  call.
- Otherwise, `client_for` → `client.list()`. An `Err` from `client_for` (missing credential,
  not signed in) or from the listing call (rendered with `with_causes`) becomes `Err(reason)`.
  These listings run concurrently (`futures::future::join_all`), each under
  `LISTING_TIMEOUT = 10 s` (`tokio::time::timeout`). `listings` delegates to
  `pub async fn listings_within(main, timeout)` (documented "Tests only", like
  `with_request_timeout` in `providers/device_flow.rs`), which `listings` calls with
  `LISTING_TIMEOUT`. A timeout becomes
  `Err("listing timed out after 10 s")`.

A failed account keeps its entry, so its heading still shows. `Sources::load` failing makes
every entry `Err` with that config error. The method never returns early on one account's
failure.

`AgentRuntime` gains `listings: Vec<AccountListing>`, a per-session cache, and
`pub async fn model_listings(&mut self) -> Result<&[AccountListing], String>`:

- It returns the `switcher()` error when models cannot be switched.
- It fetches on the first call.
- Later calls refetch only the `Err` entries, so a credential added mid-session shows up.
- The main account's entry is always replaced from `models.main_listing`, which stays current
  after a switch.

The guardian overlay needs no new core API. It groups `guardian_candidates()` by account, in
config order.

### TUI: the picker (`fontaine-tui/src/model_picker.rs`, new)

```rust
pub enum PickerPurpose { Main, Guardian }

pub struct ModelPicker {
    purpose: PickerPurpose,
    header: String,            // "model <ref> · guardian <label>", the old one-line output
    current: Option<ModelRef>, // marked wherever it appears
    recent: Vec<ModelRef>,
    groups: Vec<Group>,        // config order
    query: String,
    selected: usize,           // index into the visible *model* rows
}
struct Group { account: String, kind: AccountKind, models: Result<Vec<String>, String> }

pub enum PickerKey { None, Close, Pick(PickerPurpose, String) }
```

- **Rows.** `visible()` builds the row list: a `Recent` group first, then one heading per
  account. Each heading is followed by its matching models, or by its error reason dimmed
  (`  could not list: <reason>`). Recent entries are filtered to those whose account is still
  configured. A recent entry is kept when its account's listing failed, because the switch
  validates it anyway. It is dropped when a successful listing no longer offers it. In
  guardian mode, Recent keeps only entries that are among the candidates.
- **Filter.** A case-insensitive fuzzy subsequence match on `account/model`. Matching models
  are ordered by score: contiguous runs and matches at the start of the model id or after
  `/ - _ .` score higher, and config or listing order breaks ties. `(current)` marking and
  Recent pruning match with `kind.provider().lists(models, id)`, not plain equality, because
  providers accept aliases (Ollama's bare `llama3` for the listed `llama3:latest`). A heading stays while any of
  its models match. A failed account stays when the query matches its account name or the
  query is empty. Recent filters the same way. The matcher is a small pure function in the
  module, and no crate is added.
- **Keys.**
  - Printable characters without CONTROL or ALT edit `query` (SHIFT is allowed; other modified
    characters are ignored, as in `search_key`, `app.rs:321`, because the picker is routed
    before the Ctrl-T and Ctrl-D checks). `j`/`k`/`q` are text here, unlike the other panels.
    Backspace deletes.
  - Up/Down move between model rows only, skipping headings and wrapping around. The selection
    resets to the first match when the query changes.
  - Enter returns `Pick(purpose, "<account>/<model>")` when there is a selection.
  - Esc returns `Close`. Release events are ignored.
  - Ctrl-C keeps its global meaning through `App::handle_key`.
- **Rendering.** `body(width)` emits `header`, then the rows. The
  selected row is bold and reversed. The current model gets a trailing `(current)`. A model row
  is `  <model>` under its heading; a Recent row is `  <account>/<model>`. Because
  `draw_overlay` shows the tail of a long body, the picker windows its own rows to
  `MAX_ROWS = 14` around the selection, keeping the selected row's heading visible when it
  fits. `footer()` carries the `filter: <query>▏` line, since the footer is always laid out in
  full, followed by `type to filter · ↑/↓ select · Enter switch · Esc cancel`. With no matches the body says
  `no model matches "<query>"`.

### TUI: recent models (`fontaine-tui/src/recent.rs`, new)

`RecentModels` follows `history.rs`: one owner-only JSON file `data_dir/recent_models.json`,
`{"main": ["a/m", …], "guardian": […]}`. Each list is most recent first, deduplicated, and
capped at `RECENT_CAP = 5`.

- `load(path) -> (RecentModels, Option<String>)`: a missing file is empty. An unreadable or
  unparseable file degrades to in-memory, and the error is shown once.
- `record(purpose, &ModelRef) -> Option<String>`: moves the entry to the front and writes the
  file. On a failed write the store stays in memory and the error is returned once.

Every successful switch records it: `/model <ref>`, `/guardian-model <ref>` and picks from the
picker. The guardian list is recorded only when the guardian ends up ready, not when it is
unavailable. Recording happens in `Driver::model` and `Driver::guardian_model` after `Ok`, so
the argument form and the picker share it. The picker form is the argument form. The recorded
value is `ModelRef::parse(&switched.reference)`, since `Switched.reference` is a `String`.

### Wiring (`app.rs`, `lib.rs`, `view.rs`)

- `App.models: Option<ModelPicker>`. `covered()` includes it, and `handle_key` routes to it
  right after `server`, mapping `PickerKey::Pick` to `Action::PickModel(purpose, reference)`
  and closing the picker. `Action::PickModel` is ignored (`=> {}`) in the in-turn loops, where
  the picker cannot be open anyway: commands are only parsed from an idle submit.
- `Driver::model` with an empty argument:
  1. Post the notice `listing models…` and draw, since listing other accounts may take up to
     the timeout.
  2. Await `runtime.model_listings()`. On `Err`, show it as `/model: <e>` and do not open.
  3. Otherwise open `ModelPicker` with `PickerPurpose::Main`, `current = main.reference`, the
     recent `main` list and the groups.
- `Driver::guardian_model` with an empty argument opens the picker with
  `PickerPurpose::Guardian`. Its groups are `guardian_candidates()` grouped by account, and
  `current = ModelRef::parse(runtime.state.guardian().label())` (`None` for `"none"`), because
  `runtime.models.guardian` goes stale after `use_guardian`/`retry_guardian`. The header keeps the unavailable reason when there is
  one. An empty candidate list keeps today's one-line notice, since there is nothing to pick.
- Idle loop `Action::PickModel(Main, r)` → `self.model(runtime, &r).await`.
  `Action::PickModel(Guardian, r)` → `self.guardian_model(runtime, &r)`. Persistence, the
  guardian-follows-main rule and error reporting are therefore those of the argument form.
- `view::overlay` gains `if let Some(p) = &app.models { return ("model", p.body(width),
  p.footer()); }`, or `"guardian model"` for the guardian purpose.

### Not in scope

- Context window and price per model.
- Paused (#115) and near-limit (#116) markers. Neither exists yet. The `Group` row shape leaves
  room for a per-model suffix later.

## Implementation Steps

1. **Core listings.** Add `AccountListing`, `LISTING_TIMEOUT` and `Switcher::listings` in
   `models.rs`. Add the `listings` cache and `model_listings` in `agent.rs`, reached as
   `fontaine_core::models::AccountListing`; no crate-root re-export.
2. **Recent store.** Create `fontaine-tui/src/recent.rs`, load it next to the history in
   `lib.rs` (`setup.data_dir.join("recent_models.json")`), and show a load error once in the
   transcript.
3. **Picker.** Create `fontaine-tui/src/model_picker.rs` with the fuzzy matcher, row model,
   keys, windowed body and footer. Register it in `lib.rs` (`pub mod`).
4. **Wiring.** Add `App.models`, `covered`, key routing and `Action::PickModel` in `app.rs`.
   Update every exhaustive `match` on `Action` in `lib.rs`. Add the `overlay` branch in
   `view.rs`. Make the no-argument `Driver::model` and `Driver::guardian_model` open the
   picker, and record recents after successful switches.
5. **Docs.** Update `docs/tui.md` (command table rows 178–179, plus a short "Choosing a model"
   subsection covering the keys, grouping, Recent, failed accounts and the 10 s listing
   timeout), and `docs/configuration.md` (a `<data dir>/recent_models.json` row, owner-only, in
   the data-directory file list). Add a master-plan `## Decisions` line (see Decisions).
6. **Tests** (see Testing Strategy), then `cargo fmt --check`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test`, and the Windows clippy check.

## Files to Modify

- `rust/fontaine-core/src/models.rs`: `AccountListing`, `Switcher::listings`
- `rust/fontaine-core/src/agent.rs`: listings cache, `model_listings`
- `rust/fontaine-core/tests/models.rs`: listing tests
- `rust/fontaine-tui/src/model_picker.rs` (new)
- `rust/fontaine-tui/src/recent.rs` (new)
- `rust/fontaine-tui/src/app.rs`, `lib.rs`, `view.rs`
- `rust/fontaine-tui/tests/tui.rs`: picker flows, and the updated no-argument assertions
- `docs/tui.md`
- `docs/configuration.md`: `recent_models.json` row
- `tasks/fontaine_harness_plan.md`: `## Decisions`

## Trade-offs

- **Fetching other accounts' listings when the overlay opens, or only showing what is already
  known.** Only the main account and accounts with explicit `models` lists are known without a
  call. Showing just those would hide most of a multi-account setup, which defeats the
  grouping. Fetching concurrently with a timeout, and caching the successes for the session,
  costs one wait on the first open.
- **Fetching up front, or opening the overlay at once and filling it in.** Up front is simpler:
  no async state in `App`, and no redraw-on-arrival plumbing. Opening at once only helps
  setups with slow accounts. It can be added later behind the same `ModelPicker` API.
- **A dedicated picker, or reusing `UnavailablePrompt`'s pick list.** That list has no grouping
  or filter, and it lives inside a fail-closed prompt with a grace period. The `/model` overlay
  has no pending call and needs neither.
- **A hand-written fuzzy matcher, or a crate (`nucleo`, `fuzzy-matcher`).** The lists are tens
  to hundreds of entries. A ~40-line subsequence scorer avoids a dependency and a
  `cargo deny` review.

## Decisions

- 2026-10-23: The picker's grouping by account, fuzzy filter and Recent group go beyond Claude
  Code's flat `/model` picker. They follow opencode, because Fontaine's model namespace is
  `<account>/<model>` across several accounts. Record this line in the master plan's
  `## Decisions` as well.
- 2026-10-23: "Models named in config" means the accounts' explicit `models` lists. A
  `[model] model` or `[guardian] model` naming a model its account does not list is not
  injected, because switching to it would fail with `does not offer model`.
- 2026-10-23: Recent models are recorded only on successful switches, from either form. The
  model a session starts on is not recorded.

## Documentation

`docs/tui.md`: rewrite the `/model` and `/guardian-model` rows for the no-argument form, and
add a "Choosing a model" subsection covering the overlay's keys, the account groups, Recent
(stored in `recent_models.json` in the data directory), failed-account headings and the
listing timeout.

`docs/configuration.md`: add a `<data dir>/recent_models.json` row (owner-only) next to
`trust.toml`, `credentials.toml` and `history.jsonl`.

## Testing Strategy

Unit (`model_picker.rs`):

- Fuzzy matching: subsequence hits and misses, case-insensitivity, ranking order.
- Filtering keeps the headings of groups with matches and drops empty ones.
- A failed account's heading shows with its reason when the query is empty or matches the
  account name.
- Up/Down skip headings and wrap. The selection resets to the first match on a query edit.
- Enter returns `Pick` with the full reference, and returns nothing when nothing matches.
  Esc returns `Close`. `j`/`q` are typed into the filter.
- `(current)` marks the current model in both Recent and its account group.
- An Ollama-kind group: after switching to bare `local/llama3`, the `llama3:latest` row is
  marked `(current)` and its Recent entry is not pruned.
- Windowing keeps the selected row inside `MAX_ROWS` for a long list, including a selection
  past the first window.
- Guardian mode's Recent keeps only candidates.

Unit (`recent.rs`, with `tempfile`):

- A record moves the entry to the front, deduplicates and caps at 5. Main and guardian lists
  are separate.
- A round-trip through the file works, and the file is owner-only on Unix.
- A missing file is empty. Garbage in the file degrades to in-memory with one error. A
  failed write returns the error and keeps the in-memory state.

Integration (`fontaine-core/tests/models.rs`, wiremock):

- `Switcher::listings`: an explicit `models` list makes no request, and the main account reuses
  `main_listing` with no request.
- Another OpenAI-compatible account is listed via `/models`.
- A 500 response gives `Err` with the cause, and an account with no credential gives the
  `client_for` reason. The order follows config.
- A delayed response beyond a shortened timeout gives the timeout reason. The test calls
  `Switcher::listings_within` with a short timeout on a real clock plus a wiremock
  `set_delay`, not paused time, so it does not wait 10 s.

These run against a wiremock HTTP server rather than a fake client, because the listing goes
through the real provider client's request and error mapping.

Integration (`fontaine-tui/tests/tui.rs`, through the `World` harness):

- `/model` opens the overlay showing the account headings and `(current)`. Typing filters,
  Enter switches, and the transcript shows `model is now other/second`, the settings file is
  saved, and a second `/model` lists `other/second` under Recent.
- Esc closes the overlay without switching.
- An account whose wiremock `/models` returns 500 still shows its heading with the reason.
- `/guardian-model` opens the overlay with only the candidates, and Enter pins the guardian
  (`guardian is now cloud/judge-b`).
- Update the two existing tests' no-argument assertions (`ui.wait_for("guardian other/second")`,
  `available: cloud/judge-b`) to the overlay's header and rows.

## Open Questions

None.
