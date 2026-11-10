# Drive `claude auth login` from Fontaine Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/557

## Overview

When a `claude-subscription` account is in use but the Claude CLI is not signed in, Fontaine
today fails with "run `claude auth login` outside Fontaine", and a guardian routed through the
account masquerades as a generic `GuardianUnavailable`. This plan makes Fontaine detect the
state as a distinct, typed condition and, in the terminal UI, offer to run `claude auth login`
itself: it hands the real TTY to the CLI, waits for it to exit, re-probes `claude auth status`
and continues. Headless `-p` runs fail fast, before any turn or guardian decision, naming the
account. Issue: #557.

## Current State

- `rust/fontaine-core/src/providers/claude_subscription/cli.rs`: `check_login` (~l.394) probes
  `claude auth status` and returns a `ClaudeError` of `ErrorKind::Auth` whose text embeds
  `login_hint()` (~l.347, "run `claude auth login` outside Fontaine", with `CLAUDE_CONFIG_DIR=…`
  when the account sets `claude_config_dir`). `preflight` = `check_environment` → `check_version`
  → `check_login`; `discover` runs `preflight` and so does the transport before every request
  (`transport.rs` ~l.164).
- `ErrorKind::Auth` (`claude_subscription/mod.rs`) also covers a refused login mid-session
  (`protocol.rs` ~l.1308, ~l.1359) — not the same situation as "never signed in".
- `accounts::external_check` (`accounts.rs` ~l.296 → `models.rs` ~l.1197) returns
  `Result<String, String>`; the TUI (`fontaine-tui/src/ui/providers.rs` ~l.297) and the setup
  wizard (`fontaine-tui/src/setup.rs` ~l.618) turn the `Err` into `Outcome::Failed(reason)`.
  `/login claude` "never starts a login of its own" (`docs/claude-subscription.md`).
- The guardian's backend is built from the same connection; a failure becomes
  `Backend::Unavailable(reason)` and every call fails closed as `FailClosed::Unavailable` /
  `AuditLabel::GuardianUnavailable` (`fontaine-guardian/src/guardian.rs` ~l.228,
  `verdict.rs`), which reads as a Fontaine outage.
- The TUI already hands the terminal to a child process: `Suspend`
  (`fontaine-tui/src/lib.rs` ~l.241, built ~l.689) pauses the reader (`ReaderPause`), clears the
  live area, leaves raw mode, runs the closure and restores. `ui/commands/memory.rs` ~l.146 uses
  it to run the editor. `no_suspend()` is the test double.
- Headless (`fontaine/src/headless.rs`) has no terminal. A not-signed-in main account already fails
  in `resolve_models` → `models::resolve` before `headless::run` (`fontaine/src/lib.rs` ~l.993-1004,
  1121, 1156), pinned by `a_logged_out_cli_fails_before_any_turn_without_retries`
  (`fontaine/tests/claude_subscription.rs` ~l.339-349). `announce` already inspects
  `runtime.models.guardian`.

## Design

### 1. A distinct typed condition (core)

- Add `ErrorKind::NotSignedIn` to `claude_subscription::ErrorKind`. `check_login` returns it for
  `logged_in != true` and for an unreadable status (the two cases where signing in is the fix).
  Wrong auth method and wrong plan stay `Plan`; a refused login mid-session stays `Auth`.
- Message text: the neutral `the Claude CLI is not signed in`, with no hint: `check_login` sets the
  message at creation and runs before every request (`transport.rs` ~l.160-175), with no notion of
  surface. Each surface adds its own hint: headless/non-TUI uses `ExternalLogin.display`; the TUI
  says `/login <account>`. `login_hint()` and its "outside Fontaine" wording are dropped.
- The wrong-auth-method `Plan` message (cli.rs ~l.418-425) loses its hint too: it becomes
  "the Claude CLI is not signed in with a Claude subscription (claude.ai)", and each surface adds
  "sign in again with `claude auth login --claudeai`". That case gets the hint but no automatic offer.
  `failure_kind` in `turn.rs` maps `NotSignedIn` to `authentication_failed` like `Auth`.

### 2. The login command, owned by the provider (open/closed)

- A fallible free function `login_command(cli_path, config_dir, env) -> Result<ExternalLogin,
  ClaudeError>` where `ExternalLogin { program, args, env, display }` is built from the same
  program path (`cli_path` or bare `claude`) and `CLAUDE_CONFIG_DIR` as the probes. It is a free
  function because `Cli::new` (cli.rs ~l.257-277) needs data_dir/timeouts/warn and loads a verdicts
  file. It does not reuse `Cli::command` (cli.rs ~l.509-522): that does `env_clear()` plus an allowlist
  (no `BROWSER`/`DISPLAY`/`WAYLAND_DISPLAY`/`DBUS_*`), forces `FIXED_ENV`, sets `process_group(0)`
  on Unix (SIGTTIN when reading a pasted code) and `CREATE_NEW_PROCESS_GROUP` on Windows (no
  Ctrl-C), and uses piped stdio. The login is a std `Command` with inherited env and stdio and no
  process-group/creation flags, as `memory.rs` ~l.138-146 does for the editor. `display` is the
  human-readable command line for the pre-prompt message and for headless errors.
  The command is `claude auth login --claudeai` (Claude subscription; matches the
  `auth_method == "claude.ai"` check). On Windows it launches the same `.exe` the probes use
  (`.cmd` shims are unsupported, as today).
- `login_command` runs the env-conflict check first: an inherited `ANTHROPIC_API_KEY` etc. would
  change what the login does, so the same refusal applies. That check (today `Cli::check_environment`,
  cli.rs ~l.328-345) is pulled into a free function that both `Cli::check_environment` and
  `login_command` call.
- New `Provider::external_login(&self, account, env) -> Option<Result<ExternalLogin, ClaudeError>>`
  with default `None`; `ClaudeSubscription` implements it. The TUI and setup never name the claude kind.

### 3. A typed check result

`accounts::external_check` returns `Result<String, ExternalCheckError>`:

```rust
pub enum ExternalCheckError {
    /// The program says nobody is signed in; `login` is what signs in.
    NotSignedIn { account: String, login: ExternalLogin, source: Box<dyn Error + Send + Sync> },
    Failed(String),
}
```

`Display` for `Failed` is the existing chain text; `NotSignedIn` displays the chain with its
`source`. `models.rs::external_check` classifies with
`claude_error(e).is_some_and(|c| c.kind == ErrorKind::NotSignedIn)` (as models.rs ~l.490 and
`turn.rs` ~l.135-137, 493-497 do), not by string. That needs a provider error to inspect, but
`list_models` flattens it into `ModelError::Provider(String)` (models.rs ~l.595-598, variant
~l.53) and `external_check` does `.map_err(|e| chain(&e))` (~l.1213). So `ModelError::Provider`
keeps the provider error as `source()` (AGENTS.md chain convention): its new shape is
`Provider { message: String, source: Option<Box<ProviderError>> }` (rig's `ProviderError` is
`Clone + Send + Sync + 'static`). `Display` is unchanged; `source` is `None` for the timeout arm
(models.rs ~l.599-601). `source()` is used only for typed classification:
`resolve_models`/`run_interactive` still print only `{e}` (`fontaine/src/lib.rs` ~l.1001-1004).

### 4. TUI: offer, hand over, re-probe

In `ui/providers.rs::external_check`, on `ExternalCheckError::NotSignedIn`:

1. Ask "`claude` is not signed in. Run `claude auth login` now? (opens a browser; you may be asked
   to paste a code)", awaiting the answer inline the same way `ui/providers.rs::sign_in` does
   (`self.input.recv()`), with `ConfirmPrompt::handle_key` as the widget (the setup screen uses
   the provider dialog's `Confirm` state with a new variant). Declining ends with the existing
   failure outcome.
2. On acceptance run the command through `self.suspend` as `memory.rs` runs the editor: the std
   `Command` from `login_command` (inherited stdin/stdout/stderr), status captured in the closure.
   The terminal is the child's for the duration.
3. After it returns: a non-success status or spawn failure is a notice with the chain; either
   way re-run `accounts::external_check` once (do not trust the exit code alone — the issue
   asks for a re-probe). Success continues into the model picker as today; still-not-signed-in
   ends with the failure outcome and no further offers (no loop).

Same flow in the setup wizard (`setup.rs::external_check`). `setup::run` (setup.rs ~l.177-213) uses raw mode for Printed
and `ratatui::try_init()` (raw + alt screen) for Drawn, and its reader gets a throwaway
`&ReaderPause::default()` (~l.193-197) that would eat the child's keys. So `setup::run` keeps a
shared `ReaderPause`; the hand-over pauses it, then for Drawn leaves the alt screen with
`ratatui::restore()` and re-enters with `ratatui::try_init()` afterwards, while Printed uses
`restore_terminal` (raw mode only); it runs the child and redraws. The hand-over is a closure of
the existing `Suspend` type: the real one is built in `setup::run`, and `drive`/`drive_linear` pass
`no_suspend()`, so tests need no new `Surface` impl. The main UI's `Suspend` itself is not reused
because it is built around the main `LiveScreen` and `ReaderPause`.

### 5. Pre-flight at selection, not at first use

- Startup / `/model` / `/login` already call discovery for `requires_discovery` providers, which
  runs `preflight`; the not-signed-in condition is therefore found when the account is selected
  and before a turn is in flight.
- Main model at startup: in `run_interactive` (`fontaine/src/lib.rs` ~l.1185-1229) a main-model
  `ModelError::Provider` goes through `setup_can_help` (~l.1160) to `fontaine_tui::setup::run`, so
  the main TUI never starts. The offer therefore opens inside the setup screen, keyed on the typed
  error: `SetupParams` carries it (alongside `SetupParams.error`, a `String` today) and setup shows
  the offer directly instead of waiting for the user to pick `DialogKey::ExternalCheck`.
- Guardian account at startup: `build_guardian` yields `GuardianModel::Unavailable` and the TUI
  starts normally, so there is no offer; the main TUI shows a startup notice pointing at
  `/login <account>`, and the existing `/login` flow rebuilds the guardian through
  `credentials_changed`.
- `/model` and `/login` use the `ui/providers.rs` flow above.
- A not-signed-in state that appears mid-session (token revoked, logged out elsewhere) is not
  repaired mid-turn: the TUI surface's error text points at `/login <account>`, which now performs
  the login.

### 6. Headless: fail fast

- Main model: a not-signed-in account already fails in `resolve_models` before `headless::run`;
  only the message changes, to ``account `claude` is not signed in; run `claude auth login`, then
  retry`` (the `login.display` form, so a custom config dir is included).
- Guardian on a separate account (the new case): `headless.rs` stops before the first turn when
  the guardian's account is signed out, using what `announce` already inspects in
  `runtime.models.guardian`. The stop applies only to modes that consult the guardian (auto, afk,
  plan), following `announce`'s existing split (~l.751-768); manual/acceptEdits runs with a
  signed-out pinned guardian on another account keep working as today. The guardian model also
  backs, in every mode, the llm scanner (`scanner_stage.rs` ~l.56-71), the goal evaluator
  (`goal.rs` ~l.861-870) and prompt hooks (`user_hooks.rs` ~l.466-476); in manual/acceptEdits those
  behave as today (pause/fail with the unavailable reason) and the headless stop does not cover them.
- The stop exits 1, like the main-model failure (`ModelError::exit_code`, models.rs ~l.57-62;
  `docs/headless.md` ~l.66-67), and runs before `record_session_start`/`init` in `run_session`
  (headless.rs ~l.1250-1266).
- No login is attempted and nothing is prompted; no `GuardianUnavailable` denial can occur later
  because the run never starts.

### 7. Guardian surfaces it distinctly (fail-closed unchanged)

- Add `FailClosed::NotSignedIn` (`as_str` value `not_signed_in`) and
  `AuditLabel::GuardianNotSignedIn`; verdict text:
  "the guardian's account `claude` is not signed in; run `/login claude`". It is **always Deny**,
  like every non-verdict outcome — only the label and text differ.
- Anywhere `permissions.rs` branches on `FailClosed::Unavailable` (~l.1896, ~l.2155: the
  no-human conversion and the TUI "unavailable" prompt) the new variant takes the same path;
  introduce `FailClosed::is_unavailable()` covering both so a future call site cannot miss one.
- The guardian crate stays dependency-light: it gets the variant, not any knowledge of Claude.
  The core, which builds the backend, decides when the unavailable reason is "not signed in"
  (via the typed error, not by text) and passes that to `Backend::Unavailable`'s constructor.
- `build_guardian` does `Err(e) => Err(e.to_string())` (models.rs ~l.808) and builds
  `GuardianModel::Unavailable { reference, reason: String }` (~l.820); `Guardian::unavailable`
  (guardian.rs ~l.117-129) takes text only. Both gain a typed not-signed-in marker, fed by the
  typed error kept by `ModelError::Provider`.

## Implementation Steps

1. `claude_subscription/mod.rs`, `cli.rs`: `ErrorKind::NotSignedIn`; `check_login` returns it with
   the neutral message (the per-request `check_login` path included); drop `login_hint`;
   `login_command`; `turn.rs::failure_kind`.
2. `providers/mod.rs`: `ExternalLogin`, `Provider::external_login`; implement for
   `ClaudeSubscription`.
3. `models.rs`, `accounts.rs`: `ModelError::Provider { message, source: Option<Box<ProviderError>> }`
   keeps its provider error as `source()` (also
   touches the `ModelError` impls ~l.60, 69 and `setup_can_help`, `fontaine/src/lib.rs` ~l.1163);
   `ExternalCheckError` and its use in `external_check` via `claude_error`; update the two
   callers' error handling.
4. Guardian: `FailClosed::NotSignedIn`, `AuditLabel::GuardianNotSignedIn`, `is_unavailable()`;
   typed not-signed-in marker on `GuardianModel::Unavailable` and `Guardian::unavailable`, fed
   from step 3's typed error; update every match site and test literal; `permissions.rs` call
   sites; add the `not_signed_in` value to the headless schema and `audit.rs` doc comment.
5. TUI: `ui/providers.rs` offer → suspend → re-probe flow; `setup.rs` (shared `ReaderPause`,
   `Suspend`-type hand-over closure) with the startup offer keyed on the typed error carried in
   `SetupParams`; startup notice for a signed-out guardian account.
6. Headless: new message for the main model; guardian-account stop in `headless.rs` (exit 1,
   before `record_session_start`/`init`); document that it does not cover the scanner, goal
   evaluator or prompt hooks in manual/acceptEdits.
7. Update the mid-session auth messages in `protocol.rs` (~l.1308, ~l.1359) to name `/login`.
8. Docs and changelog (see below).

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/{mod,cli,protocol}.rs`
- `rust/fontaine-core/examples/claude_cli_double.rs`: `auth login` subcommand (today any `auth`
  subcommand is status) that persists the flipped state by rewriting `double.json`'s `auth` (it is
  re-read each launch), with `login_*` scenario keys for a failing login and for "exit 0 but still
  signed out"; `rust/fontaine-tui` has no wrapper example, so its tests reuse
  `fontaine_claude_cli_double`
- `rust/fontaine-core/tests/claude_subscription.rs`: ~l.500-503, 511 and 1617 assert the error
  contains "claude auth login" (the neutral message breaks them); ~l.1270-1276 asserts
  `ErrorKind::Auth` for the mid-session stderr case, to be checked
- `rust/fontaine-core/src/providers/mod.rs`, `rust/fontaine-core/src/{models,accounts,turn,permissions,audit}.rs`
- `rust/fontaine-guardian/src/{guardian,verdict}.rs`
- `rust/fontaine-tui/src/{ui/providers.rs,setup.rs,lib.rs,provider_dialog.rs}`
- `rust/fontaine/src/{headless,lib,doctor_cmd,auto_mode}.rs`
- `GuardianModel::Unavailable` / `Guardian::unavailable` match sites (under
  `rust/fontaine-core/src`): `agent/setup.rs` ~l.100, 105; `project_env.rs` ~l.612;
  `scanner_stage.rs` ~l.67; `user_hooks.rs` ~l.472; `goal.rs` ~l.868; `gate.rs` ~l.242; test
  literals in `rust/fontaine-tui/tests/common/mod.rs` ~l.333 and
  `rust/fontaine-core/tests/{goal,accounts,account_session,models}.rs`
- `docs/claude-subscription.md`, `docs/configuration.md`, `docs/tui.md`, `docs/headless.md`,
  `docs/afk.md`, `docs/guardian.md`, `docs/schemas/headless-v1.schema.json`, `CHANGELOG.md`

## Decisions

- Offer the login with a confirmation rather than auto-running it: it opens a browser and can ask
  for a pasted code, so launching it unprompted (e.g. on a remote shell) would surprise.
- New `ErrorKind::NotSignedIn` rather than matching the message: nothing in the codebase matches
  on text, and the TUI, headless and the guardian must dispatch on the kind.
- New `FailClosed` variant rather than only changing the reason text: audit consumers and the TUI
  key on the label, and text alone would still be filed as `GuardianUnavailable`.
- A signed-out guardian account at TUI startup gets a notice pointing at `/login <account>`, not an
  automatic offer: the TUI starts normally with `GuardianModel::Unavailable`, and the existing
  `/login` flow rebuilds the guardian.
- Provider-trait hook rather than TUI special-casing `claude-subscription`: keeps the TUI closed
  to future external-login providers.

## Documentation

- `docs/claude-subscription.md`: replace "never starts a login of its own" and the "outside
  Fontaine" sentence with the new `/login` flow and the headless fail-fast.
- Stale "outside Fontaine" / "never starts a login" text: `docs/configuration.md` ~l.478; doc
  comments at `accounts.rs` ~l.295 and ~l.310; test text at `fontaine-tui/src/provider_dialog.rs`
  ~l.1072-1076. "Guardian unavailable" conversion wording in `docs/afk.md` ~l.84 and
  `docs/headless.md` ~l.187, 232.
- `docs/schemas/headless-v1.schema.json` (~l.214 `fail_closed_reason` enum) and the `audit.rs`
  doc comment (~l.116): add `not_signed_in`.
- `docs/tui.md`: the sign-in offer and what the terminal does during it.
- `docs/headless.md`: the not-signed-in exit message, the guardian-account stop (exit 1, before the
  session starts), and that it does not cover the scanner, goal evaluator or prompt hooks in
  manual/acceptEdits.
- `docs/guardian.md`: the `GuardianNotSignedIn` label (fail-closed, Deny).
- `CHANGELOG.md`; plan Decisions entry in `tasks/fontaine_harness_plan.md` for the changed
  `/login` semantics for external-CLI accounts and the new audit label.

## Testing Strategy

Automated, using the portable `claude_cli_double` fake (never the real binary):

- Unit: `check_login` returns `NotSignedIn` for `logged_in:false`, for unparseable status; `Plan`
  for a non-claude.ai method; `login_command` carries `cli_path`, `CLAUDE_CONFIG_DIR`, refuses
  with a conflicting env var; `external_check` yields `NotSignedIn` with the command attached.
- Unit (guardian): a `NotSignedIn` backend denies every call with the new label and never allows;
  `is_unavailable()` covers both variants; the no-human conversion treats them identically.
- TUI: point `cli_path` at `claude_cli_double` (extended with an `auth login` scenario that flips
  signed-in state), run with `no_suspend()`, and assert on the re-probe result: accept → login
  runs and `external_check` is re-run; decline → no spawn; failing login → still-not-signed-in
  outcome with no second offer; exit-0-but-still-signed-out → failure (re-probe, not exit code,
  decides).
- Setup (`fontaine-tui/tests/setup.rs`): via `drive` with the double and `no_suspend()`: accept →
  login → re-probe → model picker.
- Integration through headless `fontaine -p`: issue #557's scenario, a signed-in main account plus
  a signed-out pinned guardian account (own `claude_config_dir` and double scenario) in auto mode
  → non-zero exit, the account-naming message, and zero turns/guardian calls made (regression for
  the mid-run `GuardianUnavailable` denial). The existing ~l.339 test is extended only for the
  new main-model message.

## Manual Verification

1. In a real tmux session, `claude auth logout`, start `fontaine` with a `claude` account, accept
   the offer, complete the browser/paste-code flow; expect the live area to be restored intact
   and the model picker to open. Not automated: needs a real browser flow and a real TTY handoff
   (terminal layout bugs must be checked in real tmux, per the testing design doc).
2. Ctrl-C during the login: expect Fontaine to survive and re-probe (checks that the child's
   foreground process group handling does not kill the TUI).
