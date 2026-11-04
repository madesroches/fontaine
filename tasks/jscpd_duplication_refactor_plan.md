# jscpd Duplication Refactor Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/422

## Overview

A jscpd scan of `rust/` found a handful of src-level clones worth removing (issue #422). This plan
folds the four y/n confirmation prompts in `fontaine-tui` into one `ConfirmPrompt`, shares the
list-navigation core of the model and resume pickers, replaces the 34-field `DecisionRecord`
literal repeated on the permission path with a constructor, and does the same for the `Account`
literal. It is a pure refactor: no user-visible, permission or audit-format behavior changes.

## Current State

- **Confirm prompts.** `fontaine-tui/src/{guardian,server,setup,skill_trust}_prompt.rs` each define
  a struct with `id`, `opened`, and an identical `handle_key`/`handle_key_at` (ignore non-`Press`,
  ignore Ctrl, ignore anything within `ANSWER_GRACE` of `opened`, then `y` → `Some(true)`,
  `n`/Esc → `Some(false)`). They differ only in `body` (a heading line then wrapped/bold text) and
  `footer` (one dim line). `ANSWER_GRACE` lives in `server_prompt.rs` and is also imported by
  `trust_prompt.rs`, `question.rs`, `setup_prompt.rs`, `skill_trust_prompt.rs`,
  `guardian_prompt.rs` and a test in `app.rs`. `setup_prompt.rs` also holds `CompositionPrompt`
  (a multi-stage prompt returning `Option<Option<Offer>>`); it is not a y/n prompt.
- **Pickers.** `model_picker.rs::handle_key` and `resume_picker.rs::handle_key` share the same
  Release/Esc/Up-Down-wrap/Backspace/typed-char arms, and both compute a selection-centred window
  (`selected.saturating_sub(window / 2).min(len - window)`). They differ in row type, filter,
  Enter payload, and the model picker's heading-aware window adjustment.
- **Permission-path records.** `fontaine-core/src/gate.rs` (the `record_session_event` helper near line
  538), `permissions.rs` (`record` near line 1190) and `user_shell.rs` (near line 67) each spell out
  every `DecisionRecord` field, 19–23 of them `None`.
- **Account literal.** `accounts.rs::build` and the `account()` helper in `user_config.rs` tests
  each list every `Account` field with `None` except `kind` and `base_url`.

## Design

### 1. `ConfirmPrompt` (`fontaine-tui/src/confirm_prompt.rs`)

```rust
pub struct ConfirmPrompt {
    pub(crate) id: u64,
    opened: Instant,
    heading: String,
    blocks: Vec<Block>,
    footer: &'static str,
    max_rows: Option<usize>,   // setup review: cap with a "… N more lines" tail
}
pub enum Block { Text(String), Bold(String) }
```

- `handle_key` / `handle_key_at` move here unchanged. `ANSWER_GRACE` moves here and all importers
  are re-pointed (including `app.rs` tests).
- `body(width, theme)` renders the heading (`bold.fg(theme.heading)`), then each block: `Text`
  wrapped plain, `Bold` wrapped bold; separators are `Text(String::new())` (`input::wrap("")` yields
  one empty row). With `max_rows`, rows past the cap are replaced by the dim
  `… N more lines; the whole list is in the transcript above` line, hard-coded (the setup review is
  its only user).
- `footer()` returns the one dim line.
- Each of the four modules keeps only its text: a constructor function returning `ConfirmPrompt`
  (`ServerPrompt::new` → `server_prompt::new(name, description)` etc.), plus `skill_trust_prompt`'s
  public `describe`. `GuardianOffPrompt`, `ServerPrompt`, `SetupReviewPrompt`, `SkillTrustPrompt`
  types are removed; `App` fields change to `Option<ConfirmPrompt>`. `ServerPrompt::name`/`.description`
  and `SkillTrustPrompt::name` have no reader outside their own body, so those fields are dropped and
  the values go straight into the heading/blocks.
- The prompt kinds are told apart by the `App` field that holds them, as today, so no kind enum is
  needed. `CompositionPrompt` is left alone but uses the moved `ANSWER_GRACE`; the private `ANSWER_GRACE`
  constants in `approval.rs` and `denials.rs` are replaced by it too.

### 2. Shared picker core (`fontaine-tui/src/picker.rs`)

Small free functions, not a trait or a generic widget (the rows and Enter payloads differ too much):

- `step(selected: usize, count: usize, up: bool) -> usize` — wrap-around move; `count == 0` returns
  `selected`.
- `edit_query(query: &mut String, selected: &mut usize, key: &KeyEvent) -> bool` — Backspace and
  unmodified/shift characters (not Ctrl/Alt); resets `selected` to 0; returns whether it consumed
  the key.
- `window_start(selected_row: usize, total: usize, window: usize) -> usize` — the centred window;
  returns 0 when `total <= window`.
- `MAX_ROWS` (both pickers define `const MAX_ROWS: usize = 14`) moves here.

Both `handle_key`s call them; the model picker applies its heading adjustment on top of
`window_start`, only when the list overflows. Esc/Enter stay per-picker.

### 3. `DecisionRecord::new`

In `fontaine-core/src/audit.rs`:

```rust
impl DecisionRecord {
    #[allow(clippy::too_many_arguments)] // 8 inputs; every one is required
    pub fn new(session_id, agent_id, tool, args_digest, tier: Tier, decision: Decision,
               permission_mode: String, headless: bool) -> DecisionRecord
}
```

All optional fields start `None`, `latency_ms` 0. There is deliberately no `Default`: tier and
decision have no safe default (a default `Decision` is a fail-open hazard), and an audit record must
always state its decision. Callers set the
few fields they own (`rule`, `explanation`, `latency_ms`, `tool_use_id`, `agent_name`, …) by field
assignment. `new` sets `ts` from `now_rfc3339()` itself. Convert the three named sites (the only production
literals); leave test-only literals unless trivially identical. The
`with_guardian`/`annotate` code that fills guardian and screen fields afterwards is unchanged. This
touches only audit-record construction; no permission decision logic moves.

### 4. `Account::new`

- Add `Account::new(kind: AccountKind, base_url: Option<String>)` (all other fields `None`) and use
  it in `accounts::build` and the `user_config.rs` test helper (and `config.rs`'s `openrouter(base_url)` test helper).

## Implementation Steps

1. Add `audit.rs::DecisionRecord::new`; convert `gate.rs`, `permissions.rs`, `user_shell.rs`. Add the unit test below.
2. Add `Account::new` in `config.rs`; use it in `accounts.rs`, `user_config.rs` and `config.rs`'s
   `openrouter(base_url)` test helper.
3. Add `fontaine-tui/src/confirm_prompt.rs` (register in `lib.rs`), move `ANSWER_GRACE`, port the
   four prompts to constructors, update `app.rs`, `view.rs`, `linear.rs`, `ui/mcp.rs`,
   `ui/commands/{skills,auto_mode_setup}.rs`, `trust_prompt.rs`, `question.rs`,
   `approval.rs`, `denials.rs` (private `ANSWER_GRACE` → `confirm_prompt::ANSWER_GRACE`).
4. Add `fontaine-tui/src/picker.rs` (register in `lib.rs`); use it in `model_picker.rs` and
   `resume_picker.rs`.
5. Run the full CI step set from `AGENTS.md`, including the Windows clippy check.

## Files to Modify

- `rust/fontaine-core/src/{audit,gate,permissions,user_shell,accounts,user_config,config}.rs`
- `rust/fontaine-tui/src/{confirm_prompt,picker}.rs` (new), `lib.rs`
- `rust/fontaine-tui/src/{guardian,server,setup,skill_trust}_prompt.rs`
- `rust/fontaine-tui/src/{app,view,linear,trust_prompt,question,approval,denials,model_picker,resume_picker}.rs`
- `rust/fontaine-tui/src/ui/mcp.rs`, `ui/commands/{skills,auto_mode_setup}.rs`

## Decisions

- Test-only duplication (`Warnings` approver in `schedule`/`todos`/`tasks`) is not extracted: the
  issue puts test↔test clones out of scope.

## Documentation

None: internal refactor; no doc names the removed types.

## Testing Strategy

- Existing tests are the behavior pin: the four per-prompt key tests are replaced by one
  `ConfirmPrompt` key test in `confirm_prompt.rs` (grace period, `y`/`n`/Esc, Enter ignored), and
  `setup_prompt.rs`'s `a_long_review_is_cut_with_a_pointer_to_the_transcript` moves there as the
  overflow test; picker tests (`up_and_down_skip_headings_and_wrap`,
  `the_window_follows_the_selection_in_a_long_list`, resume `arrows_wrap…`) stay unchanged and must
  keep passing; the audit and ledger integration tests cover record contents.
- New unit tests: `picker::{step,edit_query,window_start}` edge cases (empty list, wrap, Ctrl/Alt
  chars, short list); `ConfirmPrompt::body` for each block kind;
  `DecisionRecord::new` leaves every optional field `None` and `latency_ms` 0.
- Terminal layout is unchanged; compare a TUI prompt against `main` in tmux once if the rendered
  lines differ in any snapshot test.
