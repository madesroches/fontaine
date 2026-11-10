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
- Headless (`fontaine/src/headless.rs`) has no terminal; it reports startup failures through
  `preflight_stop` / the account-resolution error path.

## Design

### 1. A distinct typed condition (core)

- Add `ErrorKind::NotSignedIn` to `claude_subscription::ErrorKind`. `check_login` returns it for
  `logged_in != true` and for an unreadable status (the two cases where signing in is the fix).
  Wrong auth method and wrong plan stay `Plan`; a refused login mid-session stays `Auth`. Callers
  dispatch on the kind, which is the only reason for a new enum variant.
- Message text: `the Claude CLI is not signed in` plus, outside the TUI, the hint
  (`run `claude auth login`` / with `CLAUDE_CONFIG_DIR`); "outside Fontaine" is dropped from the
  hint wording because the TUI no longer sends people outside. `failure_kind` in `turn.rs` maps
  `NotSignedIn` to `authentication_failed` like `Auth`.

### 2. The login command, owned by the provider (open/closed)

- `ClaudeCli::login_command(&self) -> ExternalLogin` where
  `ExternalLogin { program, args, env, display }` is built from the same `cli_path`,
  `claude_config_dir` and inherited-environment rules the probes use (one builder shared with
  `probe`, so a Windows `.cmd` shim or a config dir resolves identically). `display` is the
  human-readable command line for the pre-prompt message and for headless errors.
  Whether to pass a method flag (the plan check wants `claude.ai`) is decided by reading
  `claude auth login --help` on the supported minimum CLI; the default is the bare
  `claude auth login`, with no flag we cannot verify.
- `login_command` calls `check_environment()` first: an inherited `ANTHROPIC_API_KEY` etc. would
  change what the login does, so the same refusal applies.
- New `Provider::external_login(&self, account, env) -> Option<ExternalLogin>` with default
  `None`; `ClaudeSubscription` implements it. The TUI and setup never name the claude kind.

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
`source`. `models.rs::external_check` classifies by `claude_error(e).kind == NotSignedIn`
through the provider trait (a `Provider::is_not_signed_in(&ProviderError)` default `false`),
not by string.

### 4. TUI: offer, hand over, re-probe

In `ui/providers.rs::external_check`, on `ExternalCheckError::NotSignedIn`:

1. Ask with the existing confirm-prompt machinery (`confirm_prompt.rs`): "`claude` is not signed
   in. Run `claude auth login` now? (opens a browser; you may be asked to paste a code)".
   Declining ends with the existing failure outcome.
2. On acceptance run the command through `self.suspend` exactly as `memory.rs` runs the editor:
   `std::process::Command` with inherited stdin/stdout/stderr, `.envs(login.env)`, status
   captured in the closure. The terminal is the child's for the duration.
3. After it returns: a non-success status or spawn failure is a notice with the chain; either
   way re-run `accounts::external_check` once (do not trust the exit code alone — the issue
   asks for a re-probe). Success continues into the model picker as today; still-not-signed-in
   ends with the failure outcome and no further offers (no loop).

Same flow in the setup wizard (`setup.rs::external_check`) through a `Surface` hand-over method
that wraps the same raw-mode/alt-screen teardown the main UI's `Suspend` performs.

### 5. Pre-flight at selection, not at first use

- Startup / `/model` / `/login` already call discovery for `requires_discovery` providers, which
  runs `preflight`; the not-signed-in condition is therefore found when the account is selected
  and before a turn is in flight. The plan routes that path (not just `/login`) through the
  offer: when the startup model's account fails discovery with `NotSignedIn`, the TUI shows the
  offer before the first prompt is accepted. The guardian account is checked the same way at
  guardian construction, once per session.
- A not-signed-in state that appears mid-session (token revoked, logged out elsewhere) is not
  repaired mid-turn: the error text changes to point at `/login claude` ("run `/login claude`"),
  which now performs the login.

### 6. Headless: fail fast

In `headless.rs`, when account resolution/discovery for the main model or the guardian's account
fails with `NotSignedIn`, stop before the first turn with the existing preflight stop path and a
message: ``account `claude` is not signed in; run `claude auth login`, then retry`` (the `login.display`
form, so a custom config dir is included). No login is attempted, nothing is prompted, and no
`GuardianUnavailable` denial can occur later because the run never starts.

### 7. Guardian surfaces it distinctly (fail-closed unchanged)

- Add `FailClosed::NotSignedIn` and `AuditLabel::GuardianNotSignedIn`; verdict text:
  "the guardian's account `claude` is not signed in; run `/login claude`". It is **always Deny**,
  like every non-verdict outcome — only the label and text differ.
- Anywhere `permissions.rs` branches on `FailClosed::Unavailable` (~l.1896, ~l.2155: the
  no-human conversion and the TUI "unavailable" prompt) the new variant takes the same path;
  introduce `FailClosed::is_unavailable()` covering both so a future call site cannot miss one.
- The guardian crate stays dependency-light: it gets the variant, not any knowledge of Claude.
  The core, which builds the backend, decides when the unavailable reason is "not signed in"
  (via the typed error, not by text) and passes that to `Backend::Unavailable`'s constructor.

## Implementation Steps

1. `claude_subscription/mod.rs`, `cli.rs`: `ErrorKind::NotSignedIn`; `check_login` returns it;
   reword `login_hint`; shared command builder; `login_command`; `turn.rs::failure_kind`.
2. `providers/mod.rs`: `ExternalLogin`, `Provider::external_login`, `Provider::is_not_signed_in`;
   implement for `ClaudeSubscription`.
3. `models.rs`, `accounts.rs`: `ExternalCheckError` and its use in `external_check`; update the
   two callers' error handling.
4. Guardian: `FailClosed::NotSignedIn`, `AuditLabel::GuardianNotSignedIn`, `is_unavailable()`;
   core passes the typed reason when constructing the guardian backend; `permissions.rs` call
   sites.
5. TUI: `ui/providers.rs` offer → suspend → re-probe flow; startup/model-selection offer;
   `setup.rs` + `Surface` hand-over.
6. Headless: fail-fast message in `headless.rs`.
7. Update the mid-session auth messages in `protocol.rs` (~l.1308, ~l.1359) to name `/login`.
8. Docs and changelog (see below).

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/{mod,cli,protocol}.rs`
- `rust/fontaine-core/src/providers/mod.rs`, `rust/fontaine-core/src/{models,accounts,turn,permissions}.rs`
- `rust/fontaine-guardian/src/{guardian,verdict}.rs`
- `rust/fontaine-tui/src/{ui/providers.rs,setup.rs,lib.rs,confirm_prompt.rs}`
- `rust/fontaine/src/headless.rs`
- `docs/claude-subscription.md`, `docs/tui.md`, `docs/headless.md`, `docs/guardian.md`,
  `CHANGELOG.md`

## Trade-offs

- **Auto-run vs. offer.** The login opens a browser and can ask for a pasted code; an explicit
  one-keystroke confirmation costs little and avoids launching a browser unexpectedly (e.g. on a
  remote shell).
- **New `ErrorKind` vs. matching the message.** Nothing in the codebase matches on text; a typed
  kind is the only sound way for the TUI, headless and the guardian to dispatch.
- **New `FailClosed` variant vs. only changing the reason text.** The label is what audit
  consumers and the TUI key on; text alone would still be filed as `GuardianUnavailable`.
- **Provider-trait hook vs. TUI special-casing `claude-subscription`.** Keeps the TUI closed to
  future external-login providers.

## Decisions

(None yet.)

## Documentation

- `docs/claude-subscription.md`: replace "never starts a login of its own" and the "outside
  Fontaine" sentence with the new `/login` flow and the headless fail-fast.
- `docs/tui.md`: the sign-in offer and what the terminal does during it.
- `docs/headless.md`: the not-signed-in exit message.
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
- TUI: with a recording `Suspend` double, accept → the closure receives the login command and the
  `external_check` is re-run; decline → no spawn; failing login → still-not-signed-in outcome with
  no second offer; exit-0-but-still-signed-out → failure (re-probe, not exit code, decides).
- Integration through headless `fontaine -p`: the double reports signed-out → non-zero exit, the
  account-naming message, and zero turns/guardian calls made (regression for the issue's
  mid-run `GuardianUnavailable` denial).

## Manual Verification

1. In a real tmux session, `claude auth logout`, start `fontaine` with a `claude` account, accept
   the offer, complete the browser/paste-code flow; expect the live area to be restored intact
   and the model picker to open. Not automated: needs a real browser flow and a real TTY handoff
   (terminal layout bugs must be checked in real tmux, per the testing design doc).
2. Same signed-out state with `fontaine -p "hi"`: expect immediate failure naming `claude` and the
   login command, before any model call.
3. Ctrl-C during the login: expect Fontaine to survive and re-probe (checks that the child's
   foreground process group handling does not kill the TUI).

## Open Questions

- Does `claude auth login` accept a method flag on the minimum supported CLI (2.1.318) that forces
  claude.ai? Resolve by reading `claude auth login --help`; default is the bare command.
- Windows: confirm the shared command builder launches the `.cmd`/`.exe` shim with an inherited
  console under `Suspend` (compile-check with the windows-gnu target; hand-test is the user's).
