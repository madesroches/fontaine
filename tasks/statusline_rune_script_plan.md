# Status Line as a Rune Script Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/692

## Overview

Make the status bar's whole line the output of a Rune script instead of the hardcoded
composition in `rust/fontaine-tui/src/statusbar.rs`, and give that script a host API that
keeps a simple bar to one `ctx.format(...)` call while allowing a complex one to drop,
reorder, conditionalise and restyle every segment. An embedded default script renders the
line when nothing is configured, so the default bar gains context usage and session cost
without any configuration and without a second renderer. `[statusline] command` keeps working
unchanged; with a script configured its line becomes a value the script can place.

Scope: the TUI bar only. The status-line JSON payload of `command`, the linear
(screen-reader) renderer, multi-row output and `/statusline` are out of scope.

## Current State

- `statusbar::render` (`rust/fontaine-tui/src/statusbar.rs:202-352`) builds the row by hand:
  the mode chip, the vim badge, the two safety badges, `· dir (branch) · model`, the quota
  spans, then a trailer of effort/agents/jobs/scheduled/bug-drafts/goal/remote and either the
  idle hint, a message, or the custom command's line. Nothing above the hint is replaceable.
- The segment formatters live beside it and are unit-tested there: `mode_label`/`mode_style`
  (`:48`, `:74`), `dir_label` (`:89`, `DIR_COLUMNS = 40`), `location` (`:105`),
  `reset_label` (`:132`), `quota_segment` (`:149`, `HOT_RATIO`/`WARN_RATIO`), `jobs_clause`
  (`:184`), plus `truncate` (`transcript.rs:1092`), `row_label` (`agents_panel.rs:104`) and
  `goal::duration_text` (`fontaine-sdk/src/core/goal.rs:254`).
- The row is `Constraint::Length(1)` (`view.rs:203`), drawn once from `view.rs:220`.
  `statusbar::render` is the only call site, and `linear.rs` draws no bar.
- `[statusline] command` (`fontaine-sdk/src/core/statusline.rs`) is a user-scope shell
  command fed JSON on stdin; its first stdout line, ANSI-stripped, becomes
  `StatuslineState { line: String }` (`:26`) and replaces the hint area
  (`statusbar.rs:331-341`, left-cut by `truncate_row` into the space left over).
- The trigger machinery already exists: `StatuslineTrigger::{Draw, Scheduled}`
  (`lib.rs:475`), `Ui::trigger_statusline` with a 300 ms debounce
  (`ui/mod.rs:273-294`, `STATUSLINE_DEBOUNCE` at `:64`), the run loop's drain and select arm
  (`ui/mod.rs:860-885`, `:920-930`), `build_statusline_payload` (`:319-377`) and
  `run_statusline` (`:381-398`). It is all gated on `statusline_script: Option<...>`
  (`lib.rs:454-473`, built at `:563-569`), so it is inert with nothing configured.
- Rune is pinned at `=0.14.2` (`rust/Cargo.toml:78`) and used only by
  `fontaine-sdk/src/guardian/composition/`. The posture to copy: `Context::with_config(false)`
  (no stdio), a host module installed with `#[derive(Any)]` types and `#[rune::function]`
  methods (`composition/host.rs:202-231`), `rune::prepare` + `Module` (`composition/mod.rs:287-339`),
  `budget::with(INSTRUCTION_BUDGET, …)` around `vm.send_execute(..).async_complete()`
  (`:486-500`), and an entry-point arity check through `unit.debug_info()` (`:321-335`).
- A configured composition that does not compile stops startup with exit code 2: `check_composition`
  (`core/config.rs:4365`, compile at `:4464`) returns `ConfigError::invalid(user_path,
  "guardian.composition.script", …)`, printed by `fontaine/src/lib.rs:402-408` via
  `ConfigError::exit_code()` (`core/config.rs:92`, `CONFIG_EXIT_CODE = 2` at `:23`).
- `[statusline]` is user-scope only: `Rule::user_only("statusline", STATUSLINE)`
  (`core/config_scope.rs:244`, reason at `:139`), so a repository cannot set it. Its catalog
  rows are `config_catalog.rs:652-657`; `guardian.composition.script` is the one row that
  reports itself as `set` (`:423-429`) because what matters is that it is configured.
- Context is computed on demand by `runtime.context_usage().await`
  (`core/agent.rs:1269`, `ContextUsage::total()` at `core/context.rs:52`, `window: Option<u32>`)
  and is awaited in the run loop itself by `build_statusline_payload` (`ui/mod.rs:348-351`).
  Nothing caches it on `App`.
- Cost comes from `UsageLedger` buckets: `Bucket { cost_usd: Option<f64>, estimated: bool, … }`
  (`core/usage_ledger.rs:63-82`), summed over every source but `Guardian` at `ui/mod.rs:352-357`.
  `Bucket::estimated` is the list-price marker `/cost` prints as `est.`
  (`reports.rs:785`, `agents/budget.rs:35`); the payload drops it.
- There are no named style roles: `Theme` (`theme.rs:61-84`) holds colours by meaning and call
  sites add `BOLD`/`DIM`. `fontaine-sdk` has no `ratatui` dependency, so anything shared with
  the sdk must name a role, not carry a `Style`.

## Design

### 1. Configuration

`[statusline] script` is an **inline** Rune source string, read from the user file only:

```toml
[statusline]
script = """
pub fn statusline(ctx) {
    ctx.format("{mode} · {location} · {model} · {quota} · {context} · {cost} · {hint}")
}
"""
```

- `StatuslineSection` (`core/config.rs:1363`) gains `script: Option<String>`; the merged
  `Statusline` (`:2978`) gains `script: Option<String>`.
- No new scope rule: `Rule::user_only("statusline", …)` matches the whole subtree.
- A new `check_statusline(&config, user_path)` in `core/config.rs`, called next to
  `check_composition` in `load()` (`:4247`), compiles the script. A compile error is
  `ConfigError::invalid(user_path, "statusline.script", "the script does not compile: …")`,
  which stops startup with exit code 2. The check only compiles and checks the entry point's
  arity; it never runs the script, so a runtime error (e.g. an unknown template name) is not
  caught here. `refresh_interval_secs` keeps its existing
  "requires `command`" check (`:2268-2275`) — a script is pure over its snapshot, so a timer
  would re-render the same line.
- Catalog: `statusline.script` reports `set`/`unset` like `guardian.composition.script`,
  because `/config` and `fontaine doctor` must not print a script body.

### 2. The Rune host surface

The script defines `pub fn statusline(ctx)` and returns a `Line`, a `Fragment`, or a
`String`. The host `Module` is built in `fontaine-sdk` (it needs no terminal types):

| Kind | Surface |
|---|---|
| Values | `ctx.mode()`, `ctx.model()`, `ctx.dir()`, `ctx.branch() -> Option`, `ctx.cwd()`, `ctx.project_dir()`, `ctx.session_id()`, `ctx.version()`, `ctx.effort() -> Option`, `ctx.vim() -> Option`, `ctx.afk()`, `ctx.afk_stopped()`, `ctx.guardian_off()`, `ctx.running_agents()`, `ctx.background_jobs()`, `ctx.scheduled()`, `ctx.bug_drafts() -> Option`, `ctx.goal() -> Option`, `ctx.remote() -> Option`, `ctx.context_used_percentage() -> Option`, `ctx.context_remaining_percentage() -> Option`, `ctx.context_total_tokens() -> Option`, `ctx.context_window_size() -> Option`, `ctx.cost_total_usd() -> Option`, `ctx.cost_is_estimate()`, `ctx.command_line() -> Option` |
| Building | `ctx.format(template) -> Fragment`, `ctx.segment(name) -> Fragment`, `ctx.style(name) -> Style`, `ctx.line() -> Line` |
| `Line` | `line.push(fragment)`, `line.push_text(text)`, `line.push_styled(text, style)` |
| Styles | `normal`, `dim`, `bold`, `accent`, `warning`, `warning_bold`, `success`, `error`, `user`, `heading`, `link`, `shell`, `mode.manual`, `mode.accept`, `mode.auto`, `mode.afk` |

Everything is synchronous — no host function does I/O, so there is no future surface and no
`Result` to unwrap in the common path. `Fragment` and `Line` are opaque `#[derive(Any)]` types
with no settable fields, so a script can only build a line through the host's own constructors.

`ctx.format(t)` returns a **fragment of styled runs**, not a `String`, and a lone
`ctx.format(...)` returned as the script's value is the line. That is what lets one call
reproduce the built-in bar's colours; a `String` return is coerced to one `normal` run for the
simplest scripts.

### 3. Template expansion and the separator rule

`ctx.format` splits its argument into `{name}` placeholders, `{{`/`}}` escapes and literal
text, and appends one run per piece: a placeholder appends its segment's runs, a literal
appends its text in `dim`. A value placeholder expands to its plain number (cost to four
decimals, as the `cost` segment shows it) as one `dim` run, and to nothing when the value is
`None`. A name that is not a segment or a value is an error, not literal
text; it is raised when `ctx.format` runs, so it is a render-time error handled by the §7
fallback, not a compile failure. `ctx.segment(name)` is the same lookup for one name.

The built-in bar's pieces are **bare**: no segment carries its own `·` separator or padding,
and the template supplies both. A literal made only of spaces that is directly followed by a
placeholder expanding to nothing is not appended. A *separator run* is a literal whose trimmed
text is exactly `·`. After expansion the run list is normalised once, on runs: drop empty runs,
then collapse each maximal group of adjacent separator runs to one (keeping the first), then drop
a separator run that is first or last. That gives the empty-collapses-with-its-separator
behaviour the compact form needs: `{model} · {goal} · {hint}` with no goal and a hint present
reads `… · shift+tab mode …`, with neither goal nor hint it reads the model alone; and
`{mode} · {location} · {model}` with an empty `{location}` reads `manual · m`, not `manualm`.

```rune
pub fn statusline(ctx) {
    let line = ctx.line();
    line.push(ctx.format("{mode} · {location} · {model} · {quota}"));
    if let Some(pct) = ctx.context_used_percentage() {
        let style = if pct >= 80 { ctx.style("warning") } else { ctx.style("dim") };
        line.push_styled(format!(" · ctx {pct}%"), style);
    }
    line
}
```

### 4. Segments, values and roles

**Segments** (preformatted, `None`/empty when there is nothing to show):

`mode`, `vim`, `location`, `dir`, `branch`, `model`, `quota`, `quota.guardian`, `effort`,
`agents`, `jobs`, `scheduled`, `bug_drafts`, `goal`, `remote`, `hint`, `context`, `cost`,
`session_id`, `version`, `command`.

**Values** (bare, for thresholds and hand-written wording): `context.used_percentage`,
`context.remaining_percentage`, `context.total_tokens`, `context.window_size`,
`cost.total_cost_usd`.

The formatting itself stays in Rust: `location` keeps `dir_label`'s `~` and left-cut,
`branch` keeps the 24-column truncation, `quota`/`quota.guardian` keep `reset_label` and the
hot/warn ratios, `jobs` keeps the count grammar and the `(keys to manage)` hint, `goal` keeps
`duration_text`. Segments carry no leading or trailing space: `mode` is `manual` or
`⏵⏵ auto mode on`, `vim` is `vim:insert`, `location` is `~/w (main)`. Today's `statusbar.rs`
tests keep covering all of them; the only change is that the leading `· ` and the padding come
off `mode`, `vim`, `location`, `jobs_clause` and the quota spans (`" · 1 shell running"` becomes
`"1 shell running"`).

A run carries a `Role`, not a colour: `Role` is a plain enum in `fontaine-sdk` and the TUI maps
it to a `Style` from `Theme` in one function. That keeps `fontaine-sdk` free of `ratatui` and
makes the whole render path testable without a terminal. Roles are assigned exactly as
`statusbar.rs` does today: `mode` gets the mode's colour plus `BOLD`, `vim` gets `accent`,
`quota` gets `warning` past `WARN_RATIO` else `dim`, everything else `dim`.

`context` renders `ctx 42%` and is absent until the window is known; `cost` renders `$0.1234`,
with ` est.` appended when any non-Guardian bucket is `estimated`, and is absent until a call
was priced. Both are absent rather than guessed, as the other segments are.

### 5. The default line

An embedded `default.rn` (`include_str!`), compiled by the same `StatuslineProgram` as a user
script and rendered by the same path:

```rune
pub fn statusline(ctx) {
    ctx.format(" {mode} {vim} · {location} · {model} · {quota} · {quota.guardian} · {effort} · {agents} · {jobs} · {scheduled} · {bug_drafts} · {goal} · {remote} · {context} · {cost} · {hint}")
}
```

The leading space keeps the bar's one-column left margin; the single space between `{mode}`
and `{vim}` goes with an empty `{vim}`. With no context, no cost, no safety badge and no message
showing, this is today's bar except that a set vim badge sits one space from the mode chip
instead of two; with a safety badge set it differs in the badge's position (§9). The golden
test encodes the new exact line. That keeps `rust/fontaine-tui/tests/common/mod.rs`'s `idle()`
anchor (`shift+tab mode · /add-dir`) and the existing `bar()` assertions valid.

### 6. Compatibility with `command`

`command` alone behaves as it does today: it runs on the same triggers, receives the same JSON
and its first line replaces the hint. The mechanism is that `{hint}` resolves to the command's
last good line when no script is configured, and the `hint` segment is:

```
message (running refusal → attach notice → viewed-agent state)  →  command line (no script)  →  None while the turn runs  →  idle hint
```

which is the precedence `statusbar.rs:229-251` and `:331-346` implement today, including the
two edges that matter: a command line shows while a turn runs, and a refusal or attach notice
still wins. With `script` set, `{hint}` is the message or the idle hint and the command's line is
reachable as `{command}` / `ctx.command_line()`. `refresh_interval_secs` keeps re-running the
command, which is the only part of this that changes with time.

### 7. Execution, bounds and fallback

Two compiled programs: the user's (when configured) and the embedded default. Both are compiled
at startup — the config check compile is thrown away and the TUI compiles again, as
`build_composition` does for the guardian — so a bad default is a test failure, and a bad user
script stops startup instead of degrading silently.

The script does not use the `StatuslineTrigger` machinery. `Ui::draw` builds the snapshot and
renders the program synchronously after it copies runtime state onto `App` and before it calls
`(self.render)(&mut self.app)`, caching the runs in `App.statusline` (`StatuslineState { runs }`),
so every frame is current whichever of `draw`'s callers drew it. The ratatui render closure
(`self.render`, and `statusbar::render` inside it) reads the cache and never evaluates Rune. The `command` keeps its existing channel,
300 ms debounce, in-flight flag and 5 s timeout, and publishes only `App.command_line`, so a
slow command cannot freeze the bar.

The only bound is an instruction budget through `budget::with`, enforced and tested. The
composition's `INSTRUCTION_BUDGET` (1 000 000) is far more than a status line needs; because
the render is synchronous and per frame, the status line uses 50 000, an order of magnitude
above a full default-line render, which keeps the worst case sub-millisecond.

A runtime error, panic or exhausted budget keeps the previous good line and warns once per
session through the transcript notice path; with no good line yet — a script that fails at
startup — the default program's output is used, so the bar is never blank.

### 8. Data plumbing and the draw path

`App` gains `context: Option<ContextFigures { used: u64, window: Option<u32> }>` and
`command_line: Option<String>`; `StatuslineState` (`core/statusline.rs:26`) changes from `{ line: String }` to `{ runs }`;
`App.statusline` is already `Option<StatuslineState>`. `Ui` gains `statusline_program: Option<StatuslineProgram>`,
`statusline_default: StatuslineProgram` and `statusline_warned: bool`.

The context figures are refreshed by `Ui::refresh_context(runtime)` before the first render in
`drive` (`lib.rs`; a `/resume` or `/branch` starts a new `drive`, so this covers a resumed
session), at the turn-settled point (`ui/turn.rs:1720` `finish_turn`), in `/compact`
(`ui/commands/context.rs:82`), in `/clear` (`Ui::clear`, same file, `:12`), in `/model`
(`ui/commands/model.rs:33`, since the window comes from the main model) and in `/rewind`
(`Ui::run_rewind`, `ui/commands/rewind.rs`, when `conversation_restored`), and read — never
awaited — when a snapshot is built. Cost is read from `UsageLedger` at snapshot time as `build_statusline_payload` does today (`ui/mod.rs:352-357`).

### 9. Rendering

`statusbar::render` becomes: the two safety badges (`guardian off`, `AFK STOPPED`) drawn by
Rust, always first; then `App.statusline`'s runs converted to `Span`s through the role map.
Nothing else. Every other segment, the mode chip included, is the script's to place or drop.

Width: the row clips on the right as the built-in bar does today, so a long line loses its
tail, except the command line — `{command}` and a command line standing in for `{hint}` — whose
run is marked `tail` and left-cut exactly as `truncate_row` cuts the custom command's line today, to the row width minus
the width of every other run on the line.

## Implementation Steps

1. **SDK — snapshot types.** New `rust/fontaine-sdk/src/core/statusline/snapshot.rs` (the
   existing `statusline.rs` becomes `statusline/mod.rs`): `Role`, `StyledText { text, role,
   tail: bool }`, `StatuslineSnapshot { segments: BTreeMap<&'static str, Vec<StyledText>>,
   values: Values, … }`, and the `SEGMENTS` / `VALUES` / `STYLE_NAMES`
   name tables that both the host and the docs are checked against. `Values` is typed —
   `Option<u32>` for the percentages, `Option<u64>` for the token counts, `Option<f64>` for the
   cost — so the getters return Rune numbers and
   `pct >= 80` compares numbers.
2. **SDK — the Rune program.** New `rust/fontaine-sdk/src/core/statusline/script.rs`:
   `StatuslineProgram::compile(source) -> Result<Self, StatuslineError>` following
   `composition::build` (no stdio, diagnostics rendering, one-argument `statusline` entry point
   check); `render(&self, snapshot) -> Result<Vec<StyledText>, String>` running the VM under
   `budget::with(STATUSLINE_BUDGET, || vm.call(["statusline"], (ctx,))).call()` (50 000), the call wrapped in
   `std::panic::catch_unwind(AssertUnwindSafe(..))` with a panic mapped to `Err`; the `Ctx`,
   `Line`, `Fragment` and `Style` host types; the template parser and the separator
   normalisation. `default.rn` beside it with `include_str!`.
3. **SDK — config.** `StatuslineSection.script`, `Statusline.script` (copied in `Config::apply`, `:3552`, and
   `script: None` in the default `Statusline`, `:3214`), and `check_statusline`
   called from `config::load` next to `check_composition`; the `statusline.script` catalog row
   reporting `set`/`unset`.
4. **TUI — the snapshot builder.** `statusbar::snapshot(app, data) -> StatuslineSnapshot`,
   where `data` carries what is not on `App`: `session_id`, `version`, the cost
   total and the cost `estimated` flag, and reusing every existing formatter (`project_dir` is `app.primary_root`), plus `role_style(theme, role) -> Style`. Extract the
   message/hint precedence out of `render` into `fn hint(app, command_line, scripted) ->
   Option<StyledText>`, the finished `hint` segment (`tail` set when the text is the command's
   line), stored in `segments["hint"]`.
5. **TUI — wiring.** `App`'s new fields; `Ui`'s programs and `refresh_context`; `Ui::draw`
   builds the snapshot and renders the program synchronously after the `App` sync and before
   `(self.render)(&mut self.app)`, caching the runs in `App.statusline`; `run_statusline` and the
   trigger channel serve only the `command` and publish only `App.command_line`, with the result channel (`Ui.statusline_result_tx`/`_rx`,
   `lib.rs:460-461`; `ui/mod.rs:395`, `:411`, `:923`) retyped from `Option<StatuslineState>` to
   `Option<String>`; the
   `statusline_tx`/`statusline_rx`, `trigger_statusline`, `statusline_tick` and `run_statusline`
   gates keep meaning "no `command` configured" and are not lifted; `refresh_context` called
   in `drive` (`lib.rs`) before the first render; warn-once on failure; update the `Ui`
   literal in `ui/tests.rs`.
6. **TUI — render.** `statusbar::render` reduced to badges plus runs, with the `tail` run cut;
   the existing custom-line tests move to runs and the `line` field goes.
7. **Docs and changelog** (below), then the parity row.

## Files to Modify

- `rust/fontaine-sdk/src/core/statusline.rs` → `statusline/mod.rs` (`StatuslineState { line }` →
  `{ runs }`), plus new `snapshot.rs`,
  `script.rs`, `default.rn`
- `rust/fontaine-sdk/src/core/config.rs` — `StatuslineSection.script`, `Statusline.script`
  (including `Config::apply` and the default literal), `check_statusline`, its call in `load`, value checks
- `rust/fontaine-sdk/src/core/config_catalog.rs` — the `statusline.script` row
- `rust/fontaine-tui/src/statusbar.rs` — `snapshot`, `hint`, `role_style`, the reduced `render`,
  the `jobs_clause`/`location`/quota separator change, and their tests
- `rust/fontaine-tui/src/app.rs` — `context`, `command_line`
- `rust/fontaine-tui/src/lib.rs` — `Ui`'s program fields and their construction in `drive`, the
  first `refresh_context` call there, and the retyped result channel (`:460-461`)
- `rust/fontaine-tui/src/ui/mod.rs` — the snapshot render inside `Ui::draw`, `refresh_context`,
  warn-once, the retyped result channel (`:395`, `:411`, `:923`)
- `rust/fontaine-tui/src/ui/turn.rs`, `rust/fontaine-tui/src/ui/commands/context.rs` (holds
  both `/compact` and `/clear`), `rust/fontaine-tui/src/ui/commands/model.rs`,
  `rust/fontaine-tui/src/ui/commands/rewind.rs` — the other five `refresh_context` call sites
- `rust/fontaine-tui/src/ui/tests.rs` — the test-only `Ui` constructor
- `rust/fontaine-tui/tests/statusline.rs` — integration coverage
- `rust/fontaine-sdk/tests/docs.rs` — the status line names test
- `docs/tui.md`, `docs/doctor.md`, `docs/configuration.md`, `docs/guardian.md`, `CHANGELOG.md`,
  `research/research-claude-code-parity.md`

## Decisions

- The script is inline source, not a file.
- Segments are bare and the template supplies the separators, not self-delimiting segments.
- One Rune renderer draws the default line, not a Rust default plus an override.
- Context figures are cached on `App` and never awaited per snapshot; accepted risk: the context
  percentage holds its last refreshed value while a turn runs.
- The guardian-off and `AFK STOPPED` badges stay Rust-drawn and cannot be dropped by a script,
  carrying forward the custom status line's decision; `badges` is therefore not a format name.
- `{hint}` stays a segment and takes the message precedence, so a script that omits it drops
  refusal and attach notices — the visible consequence of owning the line.
- The safety badges come first in every case, not only when a custom line is active.
- The left-cut run is the command line specifically — `{command}` and a command line standing in
  for `{hint}` — while every other run, messages included, clips on the right, so a refusal or
  attach notice keeps its beginning.

## Documentation

- `docs/tui.md` — a new `### The status line script` under `## Status bar`: the host API table,
  the separator rule, the default line as the worked example, the bounds and the fallback; the
  existing `### Custom status line` gains the `command`-as-`{command}` note and the safety
  badges' position (first on the bar, no longer "next to" the mode chip, `docs/tui.md:119`);
  the `## Status bar` segment list (`:114-120`), the running-turn sentence (`:159-161`) and the
  `### Custom status line` placement sentence (`:166-168`) are updated to include `ctx N%` and
  `$cost`/`est.`.
- `docs/doctor.md:59` and `docs/tui.md:954` — the sentences listing redacted rows gain
  `statusline.script` shown as `set`.
- `docs/configuration.md` — `statusline.script` in `### [statusline]`, the inline-source rule,
  the exit-2 rule, and the `#### [guardian.composition]` cross-reference.
- `docs/guardian.md` — one paragraph in `### Writing a composition script` noting the status
  line is the same Rune hosting with a smaller instruction budget and a fallback instead of a
  denial.
- `CHANGELOG.md` — `### Added` → `#### TUI`.
- `research/research-claude-code-parity.md:383` — the custom status line row gains the script
  form.

## Testing Strategy

- **Unit (`statusline/script.rs`).** Template parsing: placeholders, `{{`/`}}` escapes, a
  trailing literal, an unknown name error. Separator normalisation: each combination of empty
  and present neighbours, a leading and a trailing `·`, two adjacent `·`. Host surface: every
  value getter's `None` case, `push_text`/`push_styled`/`push` ordering, an unknown style name,
  a `String` return, a `Line` return, a `Fragment` return. Compile: a syntax error names the
  diagnostics, a missing or two-argument `statusline` is refused. Runtime errors: `ctx.format` with an
  unknown name fails at render time. Bounds: an infinite loop ends on the instruction budget, a
  host panic is an error rather than a crash.
- **Unit (`statusline/snapshot.rs`).** Role tables and name tables agree; the default script
  compiles.
- **Docs (`rust/fontaine-sdk/tests/docs.rs`).** A test, in the style of
  `the_tools_page_names_every_registered_built_in_tool`, asserting `docs/tui.md` names every
  entry of `SEGMENTS`, `VALUES` and `STYLE_NAMES`.
- **Unit (`statusbar.rs`).** `bar()` against the default program: the golden line with nothing
  known (the exact line from §5), with context known, with cost known, with cost estimated,
  with both, with a branch, without one, while running, and with a refusal. Segment
  granularity: each segment present and absent, the quota warning colour, `jobs`' grammar after
  losing its leading separator, `{context}`/`{cost}` absent when unknown. A user script that
  drops a segment, reorders two, and styles one with `warning`. The two badges ahead of any
  script. The `tail` cut and the right clip.
- **Unit (`config.rs`, `config_scope.rs`).** A script parses; a script that does not compile is `exit_code() == 2` naming
  `statusline.script`.
- **Unit (`config_catalog.rs`).** `effective("statusline.script", …)` reports `set` and never
  the source.
- **Integration (`rust/fontaine-tui/tests/statusline.rs`).** `World::with_config` with a
  `[statusline] script` renders its line through the real `Ui::run` on a `TestBackend`; a script that errors keeps the previous good
  line; `command` plus `script` puts the command's line where the script asks.
- No live API, network, database or container at any tier.
