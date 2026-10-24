# TUI Provider Dialog Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/140

## Overview

Add a provider management dialog to the terminal UI, opened by `/login`, where every account task
happens without leaving the harness: list the configured accounts with their kind, endpoint and
sign-in state; add an account (written to the user settings file); sign in (device sign-in for
`opencode-go`, a masked key prompt for key-based kinds); sign out; and remove an account. A new
credential takes effect in the running session. When the interactive UI cannot resolve its main
model at startup (no model, unknown account, missing credential, a rejected listing), the same
dialog opens before the session starts instead of exiting, so a new user gets from zero to a
working model in one run. `fontaine auth login` is removed; the dialog is the only interactive
way to store a credential, and automation keeps provisioning `credentials.toml` or `api_key_env`.

## Current State

- `rust/fontaine/src/auth.rs` is `fontaine auth login [<account>|--telemetry]`: it runs the
  kind's `SignIn` when it has one, otherwise reads a secret with `rpassword` (or stdin) and calls
  `credentials::store`. `rust/fontaine/src/cli.rs:115` defines `Command::Auth` /
  `AuthCommand::Login`; `rust/fontaine/src/lib.rs:89` loads config with `load_unresolved` for it
  and `lib.rs:178` dispatches it. `rpassword` is used nowhere else (`rust/Cargo.toml:86`).
- Credentials (`rust/fontaine-core/src/credentials.rs`): `store(data_dir, Target, secret)` and
  `store_oauth[_locked]` do a locked read-modify-write of `credentials.toml` through
  `owner_only::write_atomic`. There is no removal function. `Target::Telemetry` is written only
  by `auth login --telemetry`; `Credentials::telemetry()` is read by
  `rust/fontaine/src/telemetry/mod.rs:137` and `config.rs:1636`.
- Providers (`rust/fontaine-core/src/providers/mod.rs`): `Provider::credential()` returns
  `Required` / `Optional` / `SignIn`; `Provider::sign_in()` returns the `SignIn` trait object, only
  `opencode-go` has one. `SignIn::sign_in` prints the verification URL and code with `eprintln!`
  (`providers/opencode_go.rs:254`), which would corrupt the TUI, and warns about the email
  lookup the same way (`opencode_go.rs:270`). `Provider::validate` holds kind-specific account
  checks (`openai_compatible` requires `base_url`, `opencode-go` refuses `api_key_env`).
- Account shape checks live inline in `config.rs:721` `validate_values` (name non-empty without
  `/`, `wires` only where accepted, `Provider::validate`). Accounts come only from the user file
  or `--settings` (`config.rs:645`).
- User settings edits (`rust/fontaine-core/src/user_config.rs`): `toml_edit`-based,
  format-preserving, atomic (`write_atomically`); today it appends allow rules and sets
  `[model] model` / `[guardian] model`. `AgentSetup::user_config` (set at
  `rust/fontaine/src/lib.rs:589`) is the path `/model` persists to.
- Credential resolution (`rust/fontaine-core/src/models.rs:196` `credential`, `:240`
  `client_for`): `api_key_env` wins over the stored entry; the stored entry is re-read on every
  `Switcher` call (`Sources::load`), so a newly stored key is seen by the next resolution. A
  built client holds its API key; an `opencode-go` client holds the process-wide
  `TokenSource::shared` (`providers/token_source.rs:160`), which adopts newer offered tokens.
  Hint strings name `fontaine auth login` (`models.rs:234`, `:258`, `token_source.rs:66-80`,
  `opencode_go.rs:68`, `:82`, `providers/mod.rs:30`, `transport.rs`), as do the doc comments at
  `config.rs:1405` (`load_unresolved`) and `providers/mod.rs:70`.
- Live switching (`rust/fontaine-core/src/agent.rs`): `Switcher` (`models.rs:592`) owns a copy of
  the `Config`; `switch_main_model` (`agent.rs:947`) resolves, `rebuild()`s, makes a
  `MainFallback` guardian follow, and persists `[model] model`; `retry_guardian` (`agent.rs:1039`)
  re-installs the guardian from fresh credentials, giving `GuardianModel::Unavailable` (fail
  closed) when there is none. `model_listings` (`agent.rs:987`) caches listings and refetches
  only accounts that failed, so an account added mid-session would never be listed.
- Startup: `run_interactive` (`rust/fontaine/src/lib.rs:634`) exits with the `ModelError` when
  `models::resolve` fails. The trust prompt (`rust/fontaine-tui/src/trust_prompt.rs:180`) is the
  precedent for a full-screen pre-session prompt.
- TUI overlays (`rust/fontaine-tui/src/app.rs`): one `Option<…>` per overlay on `App`,
  `covered()` (`app.rs:441`), key routing in `handle_key` (`app.rs:451`), `view.rs:147`
  `overlay()` returns title/body/footer. Pastes are dropped while an overlay is open
  (`app.rs:424`). Commands are parsed in `parse_command` (`app.rs:74`) and listed in
  `builtin_menu`. `/model` opens the `ModelPicker` (`model_picker.rs`) via
  `Ui::open_model_picker` (`fontaine-tui/src/lib.rs:698`). Long async work that must stay
  interruptible uses a `tokio::select!` over the work and `self.input` (`Ui::shell`, `lib.rs:586`).

## Design

### Core: one account-operations layer (`fontaine-core`)

New module `rust/fontaine-core/src/accounts.rs`, used by both TUI hosts (in-session overlay and
the pre-session setup screen). Nothing in it prints; progress and warnings go to a caller-given
sink.

```rust
/// The facts a new account needs; everything else keeps its default.
pub struct NewAccount { pub name: String, pub kind: AccountKind, pub base_url: Option<String> }

/// What an account has to authenticate with, for the dialog's list.
pub enum SignInState {
    SignedIn { detail: Option<String> },   // OAuth entry; detail = stored `email`
    KeyStored,
    KeyFromEnv { var: String },             // api_key_env set and non-empty
    EnvUnset { var: String },
    NotSignedIn,                            // Required / SignIn with nothing stored
    NoKeyNeeded,                            // Optional with nothing stored
}

pub struct AccountRow { pub name: String, pub kind: AccountKind, pub endpoint: String, pub state: SignInState }

pub fn rows(config: &Config, data_dir: &Path, env: EnvLookup<'_>) -> Result<Vec<AccountRow>, ConfigError>;
pub fn check_name(name: &str, config: &Config) -> Result<(), String>;
pub fn build(new: &NewAccount, config: &Config) -> Result<Account, String>;
pub fn add(user_file: &Path, new: &NewAccount, config: &Config) -> Result<Account, AccountsError>;
pub fn store_key(data_dir: &Path, name: &str, key: &str) -> Result<(), ConfigError>;
pub async fn sign_in(name: &str, account: &Account, data_dir: &Path, progress: &(dyn Fn(SignInStep) + Send + Sync)) -> Result<String, SignInError>;
pub fn log_out(data_dir: &Path, name: &str) -> Result<bool, ConfigError>;
pub fn remove(user_file: &Path, data_dir: &Path, name: &str) -> Result<(), AccountsError>;
```

- `rows` reuses `models.rs`'s `credential()` (made `pub(crate)`, as is the `Credential` enum it returns) so the list says exactly what
  resolution will do. `endpoint` is the account's `base_url`, or the kind's default
  (`Provider::default_endpoint()`, a new defaulted trait method returning `Option<&'static str>`;
  `opencode-go` returns `DEFAULT_BASE_URL`, the others their rig default or `None` shown as
  "default").
- `check_name`: non-empty, ASCII letters/digits/`-`/`_` only, not already in `config.accounts`.
  This is stricter than what a hand-written file may use (`/` only), so a dialog-made name is
  always addressable as `/login <name>` and in `<account>/<model>`.
- `build` produces the `Account` and runs the same shape checks the config loader runs. Those
  checks move out of `validate_values` into `pub(crate) fn check_account(name, &Account) ->
  Result<(), (String, String)>` in `config.rs`, called from both places (DRY: one rule set).
  For `openai-compatible` the base URL is required; for `ollama` and `opencode-go` it is optional;
  the other kinds ask for none. A given base URL must parse as http(s) with a host; that rule lives only in
  `accounts::build`, not at load time. `check_account` wraps the name check, the `wires` check and
  `Provider::validate`.
- `add` writes `[accounts.<name>]` with `kind` and, when given, `base_url` to the **user** file via
  a new `user_config::add_account(path, name, &Account)` (format-preserving; refuses when the
  table already exists in the file, never overwrites). It first deletes any stored credential for
  the new name (`credentials::remove`), so an orphaned entry (hand-deleted table, failed remove)
  is never sent to the new account's endpoint. It returns the `Account` so the caller can install
  it live.
- `store_key` is `credentials::store(.., Target::Account(name), ..)`.
- `sign_in` dispatches to `Provider::sign_in()`; the `SignIn` trait gains a `progress` parameter:
  `enum SignInStep { Code { verification_url: String, user_code: String }, Waiting, Warning(String) }`,
  taken as `&(dyn Fn(SignInStep) + Send + Sync)` in both `SignIn::sign_in` and `accounts::sign_in`
  (the trait's `BoxFuture` must be `Send`).
  `opencode_go::sign_in_at` reports through it instead of `eprintln!`. It is the single sign-in
  implementation (the issue's "reuse the existing credential code path").
- `log_out` is a new `credentials::remove(data_dir, name) -> Result<bool, ConfigError>` (locked
  read-modify-write; `false` when nothing was stored). It removes the local credential only; the
  provider-side token is not revoked (no revocation endpoint is used today).
- `remove` deletes `[accounts.<name>]` from the user file (`user_config::remove_account`, `Ok(false)`
  when the table is not in the user file, which `remove` reports as "defined in another settings
  file") and then the stored credential, so no orphaned secret is left. It has no view of the live
  session, so the in-use refusal is not its outcome: the in-session host calls
  `AgentRuntime::removable(name)` first (below). `AccountsError` is a small enum only because the
  dialog dispatches on it (`InUse`, `NotInUserFile`, `Invalid(String)`, `Io` with `source()`).

`Target` and `Target::Telemetry` are removed with the CLI: `credentials::store` takes `name: &str`
like `store_oauth`; reading the `[telemetry]` entry stays.

### Core: live session effects (`AgentRuntime`)

New `AgentRuntime` methods, all driven by the dialog:

- `account_added(&mut self, name, Account)`: inserts into `AgentRuntime.config.accounts` and the
  `Switcher`'s `config.accounts`. `Setup` has no `config` field. The switcher becomes one
  `Arc<RwLock<Switcher>>` shared by the runtime and `ChildContext` (which `model_for` resolves
  through), like `main: Arc<RwLock<MainModel>>`, so added and removed accounts reach subagents
  (step 4 makes the change).
- `credentials_changed(&mut self, name) -> CredentialEffect`: called after a key is stored, a
  sign-in finishes, or a log-out.
  - Drops `name`'s cached entry from `self.listings`; `model_listings` additionally fetches any
    configured account absent from the cache, so a new account is listed.
  - If the live main model is on `name`: re-resolves the same reference through
    `Switcher::main` and installs it without persisting (split `switch_main_model` into a private
    `install_main(text) -> Result<…>` plus the persisting wrapper). On failure the old handle
    stays and the error is returned for the dialog to show.
  - If the live guardian's reference is on `name`: `retry_guardian()`. A missing or invalid
    credential still yields `GuardianModel::Unavailable`, so the guardian fails closed; nothing
    here reads or writes `[guardian]` settings.
  - Returns what changed (`main_rebuilt`, `guardian_ready: Option<bool>`) for the transcript line.
- `removable(&self, name) -> Result<(), AccountsError>`: `Err(AccountsError::InUse)` when the live
  main model, the live guardian or `[guardian.screen] model` is on that account. The host calls it
  before `accounts::remove`, so nothing is written for a refused removal. (The pre-session setup
  has no live session and runs no in-use check.)
- `account_removed(&mut self, name)`: called after a successful `accounts::remove`; removes the
  account from `AgentRuntime.config`, the shared `Switcher`'s config and the listing cache.

Log-out on the account the live main model uses: the stored credential is removed and
`credentials_changed` runs; resolution then fails for a key kind, so the live client keeps the key
it was built with until the session ends or the model is switched (there is no failing `DynModel`
today and the key in memory is the user's own), and the dialog says so. An
`opencode-go` client stops at its next token refresh (`TokenError::NoSignIn`).

### TUI: the dialog (`rust/fontaine-tui/src/provider_dialog.rs`)

A pure state machine like `ModelPicker`: `ProviderDialog::handle_key(KeyEvent) -> DialogKey` and
`handle_paste(&str)`, `body(width, rows)`, `footer()`. It never performs I/O; it returns
requests the host executes and feeds results back with `ProviderDialog::finish(Outcome)`.

```
 providers ─────────────────────────────────────────────────────────────
  work        anthropic          default                    key stored
  go          opencode-go        https://opencode.ai/inf…   signed in (me@example.com)
 > local      ollama             http://localhost:11434     no key needed
  + add account
 ───────────────────────────────────────────────────────────────────────
  Enter log in · m pick model · o log out · d remove · a add · Esc close
```

Screens (`enum Screen`): `List`; `Kind` (choose one of `providers::names()`); `Name` (text,
validated live with `accounts::check_name`); `BaseUrl` (only when the kind needs or allows one;
empty skips when optional); `Key` (masked — every character drawn as `•`, Enter with empty input
on an `Optional` kind skips); `Waiting { url, code }` (device sign-in in progress, Esc cancels);
`Confirm(Remove | LogOut)`; `Message` (result or error, any key returns to `List`).

`DialogKey`: `None`, `Close`, `Add(NewAccount)`, `StoreKey { account, key: Secret }`,
`SignIn(String)`, `CancelSignIn`, `LogOut(String)`, `Remove(String)`, `PickModel(String)`.
`DialogKey`, `NewAccount` and `Secret` derive `Clone, PartialEq, Eq` (`Action` derives them and
holds a `DialogKey`). `Secret` is a newtype over `String` with a hand-written redacting `Debug` and no `Display`, so a key can
never be formatted into a log line; it is dropped as soon as `store_key` returns.

Flow:
- `/login` opens on `List`. `/login <account>` opens on that account's log-in action
  (`Key` or a started sign-in); an unknown name opens `List` with an error line.
- Add: `Kind` → `Name` → `BaseUrl` (if applicable) → `Add` request. On success the host
  installs the account (`account_added`) and the dialog goes straight to that account's log-in
  action (`Key` for key kinds, sign-in for `opencode-go`, `Key` with "Enter to skip" for
  `ollama`). After the log-in succeeds — or is skipped on a keyless kind — the dialog closes and
  the model picker opens on the new account's models (`PickModel(name)`); picking switches the
  main model through the existing `/model` path (and persists it), Esc keeps the current model.
- `m` on an account row emits `PickModel(name)` in both hosts, so an existing account's model can
  be chosen (a missing or stale main model needs it).
- Log in on an account with `api_key_env` set shows the existing warning that the stored key is
  ignored while the variable is set (moved from `auth.rs`).
- Log out and Remove go through `Confirm`.

The dialog takes the keyboard via `App::covered()`; `App::handle_input` routes `Input::Paste` to
it (so a pasted key lands in the masked field) instead of dropping it. Its text never touches
`App::input`, the history, the transcript, the ledger or tracing: the host writes only summary
lines ("stored the key for `work`", "signed in to OpenCode Go as …").

### TUI: hosting

- In session (`fontaine-tui/src/lib.rs`): `Command::Login(String)` in `parse_command`
  (`/login [account]`) and a `builtin_menu` entry. `App` gains `providers: Option<ProviderDialog>`
  and `Action::Providers(DialogKey)` for the requests. `data_dir`, `user_config` and `env` are
  private on `Setup`, so `drive` copies them into `Ui` before building the runtime and passes env
  as `&*env` to `accounts::*`. The idle loop executes them. The dialog
  can stay open when a background-agent notification starts a turn, so the in-turn loops (`turn`,
  `shell`, `expand_serving`) store an incoming `Action::Providers` request in `pending_provider`,
  following `pending_pick`; the idle loop runs it before the notification check.
  A sign-in runs as `tokio::select!` over the `accounts::sign_in` future, `self.input` (keys go
  to the dialog; Esc cancels by dropping the future) and `self.prompts`, following `Ui::shell`'s
  pattern. The progress callback sends each `SignInStep` into an unbounded channel whose receiver
  is a further `select!` arm; each step updates the dialog before the next draw, as the turn loop
  does (`fontaine-tui/src/lib.rs:905-909`, ~968). Results go to the transcript as notices; write
  failures of the settings or credentials file are reported, never swallowed.
- Before the session (`rust/fontaine-tui/src/setup.rs`, called from `run_interactive`): when
  `models::resolve` fails in the interactive UI with an error the dialog can act on,
  `setup::run(SetupParams { config, dirs, env, error, model_override, session_id, user_file })`
  (`model_override` is `cli.model`, `session_id` is `prep.session.id`, as the first resolve uses
  them, `rust/fontaine/src/lib.rs:479-485`) opens a full-screen terminal (pre-session, like `trust_prompt::ask`, but not synchronous
  key-only polling: it reuses `read_terminal` and the `UnboundedReceiver<Input>` channel,
  `EnableBracketedPaste` and the panic-restore hook from `fontaine-tui/src/lib.rs`, so pastes reach
  the masked field, and runs the same `select!` over the sign-in future, step channel and input as
  the in-session host, so Esc cancels a sign-in; it hosts `ModelPicker` for `PickModel`) showing the resolution error above the same `ProviderDialog`. It executes requests
  directly against `accounts::*`, and installs added accounts into its own copy of the `Config`.
  To offer models for `PickModel(name)` it builds a `Switcher` from that config and lists the
  account through `Switcher::listings` with `only = [name]`; `main` there becomes
  `Option<(&ModelRef, &[String])>`, `None` before a session. When the user picks a model it is
  saved as `[model] model` (`user_config::set_model`). `models::resolve` runs again after every
  successful add, log-in or pick, not only after a pick; success starts the session normally,
  failure shows the new error and stays in the dialog. Failures the dialog cannot act on (a
  malformed credentials file, i.e. `ModelError::Config` wrapping a credentials read error) exit as
  today without opening the setup screen. `setup::run` returns `(Models, Config)` (or `None` on
  Esc, with the original error), and `run_interactive` builds `Launch`, the scanner and
  `agent_setup` from the returned config. Esc exits with the original error's message and exit
  code, so dismissing it behaves exactly as today. After a pick, the picked
  reference replaces `model_override` for the retry (`resolve_with` prefers `model_override` over
  `config.model`, and `/model` switches regardless of `--model`); `--model` is honored only for
  retries following an add or log-in. Headless runs (`-p`) never open it.

### CLI removal

Delete `rust/fontaine/src/auth.rs`, the `Command` enum (its only variant is `Auth`), the
`Cli.command` field and `AuthCommand` (`cli.rs`), the `lib.rs:30` import, its dispatch and
the `load_unresolved` branch in `lib.rs` (`config::load_unresolved` is then called only by `load`:
make it private and drop its doc comment, `config.rs:1405-1406`), and the `rpassword` dependency. Every hint that names
`fontaine auth login <account>` names `/login <account>` instead (for example "account `go` is
not signed in; run `/login go` in `fontaine`"). The telemetry ingestion key becomes
`api_key_env` or a hand-provisioned `[telemetry]` entry in `credentials.toml`, as automation
already does; no TUI command is added for it.

## Implementation Steps

1. **Core shape checks and settings edits**
   - `config.rs`: extract `check_account` from `validate_values`; call it from there.
   - `user_config.rs`: `add_account`, `remove_account` (format-preserving, atomic, refuse
     overwrite).
   - `credentials.rs`: `remove`; remove `Target` (`store` takes `name: &str`); keep the telemetry read.
2. **Sign-in progress**
   - `providers/mod.rs`: `SignInStep`, `progress` parameter on `SignIn::sign_in`,
     `Provider::default_endpoint`.
   - `providers/opencode_go.rs`: report through `progress`; update its hint strings.
3. **`accounts.rs`**: `NewAccount`, `SignInState`, `AccountRow`, `rows`, `check_name`, `build`,
   `add`, `store_key`, `sign_in`, `log_out`, `remove`, `AccountsError`; make `models::credential`
   and `Credential` `pub(crate)`.
4. **Live effects** (`agent.rs`, `child_runner.rs`, `models.rs`): `AgentSetup.switcher`,
   `Setup.switcher` and `ChildContext.switcher` (plain `Option<Switcher>` today) become one shared
   `Arc<RwLock<Switcher>>` wired in `AgentRuntime::new`; callers clone the switcher out of the read
   guard before any await. `install_main` split, `account_added`,
   `credentials_changed`, `removable`, `account_removed`; `model_listings` fetches uncached
   accounts; `Switcher::listings`' `main` becomes `Option<(&ModelRef, &[String])>`.
5. **Dialog widget** (`fontaine-tui/src/provider_dialog.rs`, `view.rs` overlay arm, `app.rs`
   `covered`/`handle_key`/paste routing, `Command::Login`, `builtin_menu`).
6. **In-session host** (`fontaine-tui/src/lib.rs`): `drive` copies `setup.data_dir`/`user_config`/`env` into `Ui`;
   dispatch `Action::Providers` (`pending_provider` for requests arriving in a turn; call
   `AgentRuntime::removable` before `accounts::remove`), sign-in `select!` with the step
   channel, open the model picker on the new account (`open_model_picker` gains an optional
   account filter passed through `model_listings`/`Switcher::listings`' existing `only`; with a
   filter, `model_listings` fetches the named account, cached or not, and returns only that
   account's listing, where it otherwise sets `only` itself to the failed accounts).
7. **Pre-session setup** (`fontaine-tui/src/setup.rs`, `rust/fontaine/src/lib.rs`
   `run_interactive`): `SetupParams` carries `model_override`, `session_id` and the user file
   path; `setup::run` returns `(Models, Config)` and `run_interactive` builds `Launch`, the
   scanner and `agent_setup` from it; retry after every add, log-in or pick. Setup reuses
   `read_terminal`, the `Input` channel, bracketed paste and the panic-restore hook, runs the
   sign-in `select!`, hosts `ModelPicker`, and a pick replaces `model_override` for the retry.
8. **CLI removal and hints**: `auth.rs`, `cli.rs`, `lib.rs`, `Cargo.toml` (`rpassword`), hint
   strings in `models.rs`, `token_source.rs`, `opencode_go.rs`, `providers/mod.rs`,
   `transport.rs`, and the tests asserting them (`fontaine-core/tests/{accounts,providers,
   token_source,opencode_go,credentials}.rs`, where `credentials.rs` drops `Target::Telemetry` and
   `a_later_login_keeps_the_earlier_entries` seeds a hand-written `[telemetry]` entry; in `rust/fontaine/tests/headless_smoke.rs` rewrite
   `auth_is_a_subcommand_never_a_prompt` (:245) to pin `fontaine auth login` → exit 2 and delete
   `auth_login_*` (:273, :293, :312)); also the doc comments naming `auth login`
   (`providers/mod.rs:30`, `:70`).
9. **Master plan**: add a dated line to `tasks/fontaine_harness_plan.md` `## Decisions` recording
   the removal of `fontaine auth login|logout|status`-style CLI login (Claude Code keeps
   `claude auth login`) in favor of `/login`; automation provisions config plus `api_key_env` or
   `credentials.toml`.
10. **Docs** (see Documentation), `CHANGELOG.md` via the PR step.

## Files to Modify

- `rust/fontaine-core/src/{config,credentials,user_config,models,agent,lib,transport}.rs`
- `rust/fontaine-core/src/accounts.rs` (new)
- `rust/fontaine-core/src/providers/{mod,opencode_go,token_source}.rs`
- `rust/fontaine-tui/src/{app,view,lib}.rs`, `provider_dialog.rs` (new), `setup.rs` (new)
- `rust/fontaine/src/{lib,cli}.rs`, `auth.rs` (deleted), `rust/fontaine/Cargo.toml`,
  `rust/Cargo.toml`, `rust/Cargo.lock`
- `rust/fontaine-core/tests/*.rs` asserting hint text
- `rust/fontaine/tests/headless_smoke.rs`
- `tasks/fontaine_harness_plan.md` (Decisions line for the CLI login removal)
- `tasks/design/config.md`
- `docs/{tui,configuration,headless,architecture,telemetry}.md`

## Trade-offs

- **Pre-session setup screen vs. a session with no model.** Starting the session without a main
  model would make `Models::main` optional across the runtime, the agent builder, the guardian's
  main-model fallback and the status bar, and would need a placeholder model for turns. Running
  the same dialog before the session is built keeps every runtime invariant (a main model always
  exists; the guardian is resolved by the normal path) and still needs no restart.

## Decisions

- 2026-10-24: Missing or broken main model at interactive startup opens the dialog before the
  session instead of a model-less session (see Trade-offs).
- 2026-10-24: The telemetry ingestion key gets no TUI command; it comes from `api_key_env` or a
  provisioned `credentials.toml`.
- 2026-10-24: A dialog-created account name is limited to ASCII letters, digits, `-` and `_`,
  stricter than the file rule, so it is always addressable as `/login <name>`.
- 2026-10-24: Removing an account also removes its stored credential, and is refused while the
  live main model, live guardian or the screen stage uses it.
- 2026-10-24: `fontaine auth login` is removed with no CLI replacement (Claude Code keeps
  `claude auth login`); `/login` is the only interactive path, and automation provisions config
  plus `api_key_env` or `credentials.toml`. Recorded in the master plan's Decisions.
- 2026-10-24: `/login` never switches the model by itself; it opens the model picker on the new
  account and the user chooses.

## Documentation

- `docs/tui.md`: `/login [account]` in the command table; a "Managing providers" section (list
  columns, add, log in, log out, remove, masked key entry, startup setup when no model resolves).
- `docs/configuration.md`: credentials section rewritten around `/login`; remove
  `fontaine auth login` from the command table and examples; note the user file is edited in
  place with comments kept; CI provisioning (`api_key_env` or a shipped `credentials.toml`);
  `opencode-go` sign-ins in CI: refresh tokens rotate, so a copied `credentials.toml` works on one
  machine at a time and the copy left behind stops working after the first refresh — use a
  key-based account for CI.
- `docs/tui.md` (`:209-211`): successful model listings are no longer kept for the whole session
  (a credential change drops that account's entry); update the sentence.
- `docs/index.md` (`:27-29`): add `/login` to the TUI's command list.
- `docs/headless.md`: drop the two `auth login` rows; say how a headless run gets credentials.
- `docs/architecture.md`: drop `auth.rs` (`:30`), the "`auth login` writer" (`:31`) and the
  `fontaine auth login` line (`:58`); add `accounts`, `provider_dialog`, `setup`.
- `docs/telemetry.md`: ingestion key from `api_key_env` or a provisioned `[telemetry]` entry.
- `tasks/design/config.md` (`:48`, `:134`): `fontaine auth login <name>` / `--telemetry` become
  `/login` and a provisioned `credentials.toml`.

## Testing Strategy

Unit tests (all reachable with constructed inputs):
- `user_config::add_account` / `remove_account`: comments and layout kept, refuses an existing
  table, missing file created, symlink preserved, remove reports "not in this file".
- `credentials::remove`: removes one entry and keeps the others; missing file / missing entry →
  `false`; a malformed file is refused untouched.
- `config::check_account` shared by loader and `accounts::build`: same errors both ways
  (`openai-compatible` without `base_url`, `wires` on a kind that refuses them).
- `accounts::check_name`: empty, `/`, space, taken, valid. `accounts::build` URL validation.
- `accounts::add` clears a pre-existing credentials entry under the new name.
- `accounts::rows`: each `SignInState` (stored key, OAuth with email, env set, env unset, keyless
  ollama, `opencode-go` not signed in, a key left by a changed kind counts as not signed in).
- `ProviderDialog`: every transition (add flow per kind, skip on optional, Esc at each screen,
  `/login <account>` entry, unknown account, confirm/decline log-out and remove, paste into the
  masked field); `body()` renders `•` and never the key text; `Secret`'s `Debug` redacts.
- Host: an `Action::Providers` request arriving during a turn is held in `pending_provider` and
  run when the loop returns to idle.
- `App`: paste routed to the dialog; `covered()`; `parse_command("/login go")`; type and paste a key
  on the Key screen, press Enter: `App::input` is empty, no `Submit`/`Queue` action, the action is
  `Providers(StoreKey)`.
- `opencode_go::sign_in_at` against the existing wiremock Console
  (`fontaine-core/tests/opencode_go.rs`): progress receives `Code` (with the URL and code) and `Waiting`, tokens stored.

Integration (`fontaine-core/tests`, deterministic models / wiremock, no live APIs):
- `AgentRuntime::credentials_changed` after storing a key for the main model's account rebuilds
  the main model (wiremock sees the new key); on the guardian's account with the key removed, the
  guardian is `Unavailable` and an adjudication denies — pins the fail-closed invariant.
- `model_listings` lists an account added mid-session.
- `removable` refuses the main/guardian/screen account; `account_removed` drops the account and its listing.
- In `rust/fontaine/tests/headless_smoke.rs`: `fontaine auth login` is a clap usage error, exit 2
  (a bare `fontaine auth` parses as a positional prompt).

## Manual Verification

Terminal rendering and the pre-session screen need a real TTY; breakage is obvious on first use.

1. `FONTAINE_CONFIG_DIR=$(mktemp -d) FONTAINE_DATA_DIR=$(mktemp -d) cargo run -p fontaine`
   with no settings → the setup dialog
   shows "no main model…"; add an `anthropic` account `work`, paste a key (shows `•`), pick a
   model → the session starts on it; `config.toml` holds `[accounts.work]` and `[model]`, no key.
2. In that session `/login` → `work` shows "key stored"; `o` then confirm → "not signed in";
   `/login work`, enter the key again → "key stored".
3. Add an `opencode-go` account → URL and code shown inside the dialog, Esc cancels cleanly;
   repeat and approve → "signed in to OpenCode Go as …".

## Open Questions

None.
