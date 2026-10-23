# Phase 3 — Permission pipeline & manual TUI

**Status:** done · **Depends on:** [Phase 2](phase2_runtime_tools.md) · **Master:** [fontaine_harness_plan.md](../../fontaine_harness_plan.md)

Fills the Phase-2 `GuardedTool` stub with the real gate: the pipeline order is specified by the
`tasks/design/permissions.md` §Core data flow diagram (steps 1–9). **Milestone: usable Claude-Code-like agent
with manual approvals.**

## Design (excerpt owned by this phase)

### Permission modes

Five modes with the **same semantics as Claude Code**: `manual`, `acceptEdits`, `plan`, `auto`,
`dontAsk`. The deterministic deny → ask → critical-path → protected-path floor (and, from Phase 4, the guardian) gates every
call in every mode. Shift+Tab cycles manual → acceptEdits → plan → auto. `auto` is powered by our local
guardian against any model.

### TUI (fontaine-tui) — base surface

ratatui + crossterm, codex-rs-style: transcript pane (streaming assistant text, collapsed tool
calls), input box, status bar showing mode (`⏵⏵ auto mode on`). Approval prompt widget for
manual/escalate (allow once / allow session / always allow / deny + feedback; floor-forced prompts and shell calls that are not one literal simple command offer only allow once / deny, since allow rules cannot act there): "allow session" adds
a session-scope grant to `SessionState` (recorded in the ledger); "always allow" shows the exact
`Tool(specifier)` rule it will write and appends it to the user file's `[permissions] allow` and adds it to `SessionState`'s allow rules immediately, so the next matching call in the same session skips the prompt (user-file writes — "always allow" — go through `toml_edit =0.25.17`, preserving the file's formatting and comments); the
proposed rule is the narrowest that covers the call (exact argv for `shell`, the resolved path for
path tools, the host for `webfetch`); the shown rule is serialized so that `parse_rule` plus matching reproduces it (`//` for absolute paths, gitignore escaping, shell quoting). `/add-dir <path>` adds a
workspace root; Shift+Tab cycles modes. (The denial UX — inline `[Rule]` rendering,
`/denied`, `/guardian-model` — is designed and built in Phase 4; the background-agents panel in
Phase 5; see those plans.)

### Tier-0.5 initial allowlist

Conservative starting table; any flag not listed, any `--flag=value` whose flag is not listed, and
any combined short-flag cluster containing an unlisted letter fall through to the guardian (`ls -la`
is accepted as `-l -a`). A value-taking short flag accepts an attached value (`-n5`) or, as the last letter of a cluster, the next token (`-nA 3`); any other placement falls through. Positional tokens are validated as paths. Numeric flag values are parsed as integers, not pattern-matched. A path-valued flag (`FILE`) pre-classifies only when its value resolves in scope; an out-of-scope value falls through to the guardian. Check the
table against `claude -p` baselines (step 3) and record differences in Decisions.

| Command | Allowed flags | Values validated as paths |
|---|---|---|
| `ls` | `-a -A -l -h -1 -d -t -r -S -F` | positionals |
| `pwd` | none | — |
| `wc` | `-l -w -c -m` | positionals |
| `file` | `-b -i --mime --mime-type --brief -m FILE --magic-file FILE` | positionals; `-m`/`--magic-file` value. `-f`/`--files-from` not allowed (indirect reads) |
| `cat` | `-n -b -s` | positionals |
| `head`, `tail` | `-n N -c N -q -v` (`tail -f`/`-F` not allowed) | positionals |
| `rg` | `-i -s -S -n -N -H -w -x -F -v -c -l -A N -B N -C N -m N -t TYPE -T TYPE --hidden --no-heading -f FILE --file FILE --ignore-file FILE` | positionals; `-f`/`--file`, `--ignore-file` values. `--pre`, `--pre-glob`, `--hostname-bin`, `-z`/`--search-zip`, `-L`/`--follow`, `-g`/`--glob` not allowed |
| `grep` | `-i -n -v -w -x -F -E -c -l -L -H -h -o -q -s -r -A N -B N -C N -f FILE --file FILE` | positionals; `-f`/`--file` value. `-R` and `--exclude-from` not allowed |

No `git` subcommand is in the curated set: every git invocation goes to the guardian, because repo-controlled config and attributes can make a read-only subcommand execute a program.

### Permission rules and allow (pipeline steps 2 and 5a)

The `tasks/design/permissions.md` §Permission rules is the spec: one `Tool(specifier)` grammar (parsed at load since
Phase 0; Phase 0 checks only the outer shape, tool name, `mcp__…` names and `webfetch` `domain:` form, and this phase adds per-kind specifier parsing behind the same load-time error) for deny/ask/allow; `shell` specifiers matched on the Tier-0.5 `brush-parser` argv — deny/ask
as a prefix anywhere in the word stream and on any unparseable command containing the rule's first
word, allow only on one simple command; path specifiers gitignore-style (matched with the `ignore` crate, `=0.4.33`) against scope-validator
resolved paths; `webfetch(domain:)`; MCP names. Allow rules are step 5a, after the floor; the broad
code-execution list that `auto` suspends (`tasks/design/permissions.md` §Permission rules, Where allow acts) is a shipped
constant in `permission_rules.rs`, applied when Phase 4 wires `auto`. The pipeline also gets its step-1a slot: a `PreToolUseDecision` input
(deny / ask / allow / `updated_input`) with the composition and failure rules of `tasks/design/permissions.md` §User
hooks, driven in this phase only by test doubles — user hooks load in Phase 5.

`DecisionRecord` (fontaine-core `audit.rs`; the design docs' rich `GateDecision` record) is the one typed record per gate decision — a human answer to a prompt adds a second with `tier = human`: written to the audit log first, then projected to
the `tool_decision` telemetry event and the headless `gate_decision` event; all three carry the one tier enum in `tasks/design/permissions.md` §Core data flow unchanged. The fontaine-tools `GateDecision` stays the thin `Allow { args }` / `Deny { message }` verdict `ToolGate::check` returns.

## Steps

1. `PermissionPipeline` (implements the `ToolGate` trait, replacing the Phase-2 stub; injected at registration) with modes `manual`/`acceptEdits`/`plan`/`dontAsk` (`auto` wires in at
   Phase 4 step 2) and the step order from the `tasks/design/permissions.md` §Core data flow
   diagram (the step-2 deterministic floor: deny → ask → critical-path (rm/rmdir targets prompt; deny in
   `dontAsk`/headless) → protected-path (the enumerated list in `tasks/design/permissions.md` §Core data flow; `edit`/`write` only — prompt in manual/acceptEdits,
   deny in `dontAsk`/headless), in every mode, with shell commands parsed by the Tier-0.5 `brush-parser` pass (`brush-parser =0.4.0` in `fontaine-tools`; no hand-rolled lexer); steps 4–5:
   read-only fast path, then the Tier-0.5 shell pre-classifier with its curated set +
   per-command read-only flag allowlist (initial table above) + `allow_extra` entries (a command or `command subcommand` pair; no flags — any flag falls through; positional args validated as in-scope paths) + audit log, schema per
   `tasks/design/permissions.md` §Core data flow); rule matching and allow rules (design above; `--allowed-tools` /
   `--disallowed-tools` as CLI-scope rules: two repeatable flags in `rust/fontaine/src/cli.rs`, merged by `rust/fontaine-core/src/config.rs` into the CLI layer's `permissions.deny`/`allow` and validated by the same load-time rule parser); the step-1a hook-decision slot; `DecisionRecord` → audit
   log, then `tool_decision` telemetry and the headless `gate_decision` event; `fontaine.permission`
   and `fontaine.tool.blocked_on_user` spans, `permission_mode_changed` event; human-approval
   callback trait.
   Plan mode's step-9 guardian leg falls through to the human prompt until the guardian ships (audit `tier = mode`, including its headless Deny).
   Delete `StubGate` and `rust/fontaine-core/tests/gate_stub.rs`; rewrite the stub-era tests in `rust/fontaine/tests/headless_tools.rs` under pipeline semantics; the pipeline keeps appending a ledger Action/Denial record for every decision (as `StubGate` did in `gate.rs`).
   The interactive TUI rejects `--permission-mode auto` with the same usage error (exit 2) as `-p`: move the check in `rust/fontaine/src/lib.rs` above the `if !cli.print` early return.
   Telemetry also emits the `fontaine.tool_decision.count` metric (attrs `tier`, `decision`; `tasks/design/observability.md`), the pipeline records `tier` and `decision` on the `fontaine.permission` span, and `SpanBridge::derive` in `rust/fontaine/src/telemetry/span_bridge.rs` gains a `fontaine.permission` arm emitting it.
   Headless `-p` semantics: `tasks/design/permissions.md` §Headless. The stream loop collects each Deny decision, from the same path that feeds the headless `gate_decision` event, into the `result` event's `denials` (items `{tool, tier, rule, explanation}`, per `docs/schemas/headless-v1.schema.json`; `headless.rs` currently hardcodes `[]`).
   Call ids: a rig-agent `AgentHook` whose `on_dispatch` copies `DispatchEvent.call_id` into a per-agent current-call slot on `SessionState` (sound because tool calls run one at a time, `tool_concurrency = 1`); the pipeline stamps it on the `DecisionRecord` as `tool_use_id` (an added audit field). Decisions reach headless and the TUI through a decision channel on `SessionState` that the shared turn runner (step 2) drains into its gate-decision event; headless emits `gate_decision` and collects `result.denials` from those events.
   Rule sources: every merged rule carries its source (`config::Slot`, or `session` for session grants; a live "always allow" rule is labelled `user`), `union_rules` dedups on (rule, source), and the audit `rule` field is the rule text plus that source.
2. First move the agent build and session/state setup (`build_agent`, `SessionState`/scope construction, `record_session_start`, the stream loop) out of `rust/fontaine/src/headless.rs` / `prepare_headless` in `lib.rs` into `fontaine-core` (next to `tools.rs`/`session.rs`), so headless and the TUI both call it (`fontaine-tui` cannot depend on the bin crate). The stream loop becomes a fontaine-core turn runner that yields a typed session-event enum (assistant delta, assistant, tool call, gate decision, tool result, usage, final/error) to a consumer callback, which can stop the run between turns, and returns a mode-neutral outcome (answer, max turns, stopped, failed); headless maps the events to its JSON lines and the outcome to `Subtype`, and keeps `include_partial_messages`, `--max-budget-usd` and JSON-schema validation; the agent builder takes resolved `Models` and mode-neutral parameters, never the bin crate's `cli` types. `SessionState`'s mode, rules, session grants and scope sit behind a lock, so Shift+Tab, grants, "always allow" and `/add-dir` take effect on the next call. `/add-dir <path>` validates and canonicalizes like `--add-dir`, then between turns rebuilds the scope, registry (reusing the session-wide `Seen`) and agent (preamble included) through the shared setup, keeping the conversation and session store, and appends a ledger record of the added root; split `prepare_headless` into headless-only validation and a shared session setup (session id, git_bash, roots/`--add-dir`, `Session::create`, `scrubbed_env`, `models::resolve`) that both modes call, with `lib.rs` dispatching the non-`-p` path to `fontaine-tui`. The moved code needs `tokio` (`record_session_start` uses `tokio::task::spawn_blocking`), so `tokio` (workspace dep) moves from dev-dependencies to `[dependencies]` of `fontaine-core`. Then `fontaine-tui`: transcript, streaming render, input, approval prompts (incl. allow session /
   always allow), status bar, mode cycling, `/add-dir`. Add `rig-core`/`rig-agent` with `test-utils` as dev-dependencies of `fontaine-tui`, and `tokio` and `futures` (workspace deps) as its dependencies; terminal events are read through `spawn_blocking` (ratatui's re-exported crossterm lacks `event-stream`, and there is still no direct crossterm pin).
3. Differential baseline (manual/eval-side, never CI — see AGENTS.md): implement the `fontaine-evals`
   runner mode `--guardian none` (the runner is currently one clap struct with a required `--dataset` that rejects anything but `fixture`; `case.rs` must materialize the case `workspace` (a case without `workspace` runs in an empty scratch root); `fontaine-evals` gains `fontaine-tools` and an async runtime as dependencies, and `tempfile` moves from dev-dependencies to `[dependencies]`) (`tasks/design/testing.md` Testing Strategy, Eval harness: `pipeline` cases through the real `PermissionPipeline` for the non-auto modes) and the
   `baseline capture` / `baseline diff` subcommands (Differential
   baselines; `diff` runs against a `--guardian none --out` file) and run them for manual/acceptEdits/plan/dontAsk over the Tier-0.5 adversarial set and
   the floor/headless cases (the cases' `permissions` field is written as equivalent `.claude/settings.json` permissions in `Tool(specifier)` syntax in the scratch workspace, mapping tool names `shell`→`Bash`, `read`→`Read`, `write`→`Write`, `edit`→`Edit`, `glob`→`Glob`, `grep`→`Grep`, `webfetch`→`WebFetch`, `agent`→`Agent`, `mcp__…` unchanged, specifiers kept; a word-only `shell(w)` deny/ask rule, a prefix match in Fontaine, becomes both `Bash(w)` and `Bash(w *)`; `capture` skips and lists any case that sets `allow_extra` or another key with no Claude Code equivalent); also confirm the protected-path list and the allowlist table against
   the captured `claude -p` behavior. Fix or record each divergence. In `none` mode the runner builds the pipeline non-headless with a human-approval callback that answers deny, and scores each case on the pipeline's first decision record (`decision`/`tier`), so would-prompt outcomes score `prompt`; each case's risk class comes from `builtin_risk(tool)`, falling back to `Arbitrary` as `risk_of` in `fontaine-tools/src/builtins.rs` does (`agent` is not in `BUILTIN_RISK`). When a case workspace's symlink cannot be created or its target does not exist (the tier05 workspaces' `linked -> /etc` on Windows), the runner reports that case as skipped — counted and listed, not scored, never a failure. Dataset fixes for the `none` gate to exit 0: relabel `tier05-chain-rm-no-space` (`ls;rm x`) and `tier05-and-rm` (`ls && rm x`) in `evals/dataset/tier05.jsonl` to `expected_tier` `critical_path` with `fixture_verdict` `{prompt, critical_path}` (the floor treats `rm` in a non-simple command as critical); in `evals/dataset/floor.jsonl` rewrite `floor-protected-path-edit` args to the real rig-tools `EditArgs` shape `{path, edits: [{old_text, new_text, replace_all}]}`; in `evals/dataset/spawn.jsonl` rewrite the four `spawn-*` pipeline cases' args to the agent tool's argument names (`agent_type`, `prompt`, optional `model`, optional `background`; Phase 5 defines them) instead of `{type, prompt, tools}`. Add `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian none` (from `rust/`) to `ci.yml` on all three OSes.
4. Cassette test (offline build; recording stays the human step): `rust/fontaine/tests/cassettes.rs` with the manual-mode
   test (in-scope read round-trip) marked `#[ignore = "cassette not recorded"]`, the rig-cassette scrubbing
   configuration, and the account `base_url` pointed at the cassette proxy.
5. Docs — `docs/permissions.md`: the mode table (`manual`/`acceptEdits`/`plan`/`dontAsk`; the default is
   `manual`; `auto` is not yet available), the deterministic floor (deny → ask →
   critical-path with its rm/rmdir list → protected-path with its enumerated list), the
   `Tool(specifier)` rule grammar and allow rules (sources, step order, session vs always grants,
   suspension of broad rules in `auto`), and the Tier-0.5 pre-classifier (curated set,
   per-command read-only flag allowlist, fail-through rule, `[shell_preclassifier] allow_extra` —
   user trust scopes only (per the config scope table), flag-free, positionals must be in-scope paths); add the permission-pipeline diagram to `docs/architecture.md`, update its crate tree (add `preclassify`; `fontaine-tui` is no longer a skeleton), the `fontaine-evals` dependency line (add `fontaine-tools`), its Crates table (`fontaine/headless.rs` row, "Skeleton crates" row, the `fontaine-evals` "scores canned verdicts" row), the "Without -p it starts and exits" passage, and module table (`permissions`/`audit` modules); fix `docs/headless.md` (bare `fontaine` "starts and exits") and `docs/telemetry.md` (audit log "does not write one yet");
   start `docs/tui.md` (keybinds incl. Shift+Tab, approval-prompt options, `/add-dir`); add
   the `--allowed-tools` / `--disallowed-tools` flags to the `docs/headless.md` flag table and rewrite its Denials paragraph (no longer deny-by-default; `gate_decision` is emitted; `result.denials` is populated); add
   the `tool_decision` event, the `fontaine.tool_decision.count` metric and permission spans to `docs/telemetry.md`; in `docs/tools.md` replace "Permissions today" with a pointer to `docs/permissions.md` and repoint the inbound links in `docs/headless.md` and `docs/configuration.md`; in `docs/configuration.md` update the `[permissions]` intro, the `allow_extra` "not yet active" note and the CLI table (add the two flags); add `docs/index.md` entries for `docs/permissions.md` and `docs/tui.md`, and add both pages to the required-pages list in `rust/fontaine-core/tests/docs.rs` (`the_required_pages_exist_with_content`); add a mode table to `README.md` and the new pages to its `## Documentation` list; in `evals/README.md` document `--guardian none`, `baseline capture|diff` and the `none` exit-code rule (it currently calls `fixture` the only supported mode); rewrite the eval-runner paragraph of `docs/architecture.md` (it says the runner only scores fixtures); add the `--guardian none` command to `CONTRIBUTING.md` and fix its count of CI commands; in `tasks/fontaine_harness_plan.md` append `, fontaine-tools` to the `fontaine-evals` line of the crate dependency graph; append a CHANGELOG entry.

## Files created or modified

`rust/Cargo.toml` (new crates: dev `rig-cassette =0.45.0` (features `http`, `agent`), `ratatui =0.30.2` (crossterm via `ratatui::crossterm`, no direct pin), `brush-parser =0.4.0`, `toml_edit =0.25.17`, `sha2 =0.11.1` (audit `args_digest`), `serde_jcs =0.2.0` (args canonicalization)); `rust/fontaine-core/Cargo.toml` (`sha2`, `serde_jcs`, `toml_edit`, `tokio` as a regular dependency); `rust/fontaine-tools/Cargo.toml` (`brush-parser`); `rust/fontaine/Cargo.toml` (dev `rig-cassette`); `rust/fontaine-core/src/{permissions,permission_rules,audit}.rs`; the agent/session setup moved from `rust/fontaine/src/headless.rs` into `rust/fontaine-core/src/` (step 2); `rust/fontaine-core/src/gate.rs` (StubGate removed); `rust/fontaine/src/telemetry/span_bridge.rs`; `rust/fontaine/tests/telemetry_otlp.rs` (metric assertion); `rust/fontaine-tui/Cargo.toml`; `.github/workflows/ci.yml` (adds the `--guardian none` eval step on all three OSes); `rust/fontaine-evals/{Cargo.toml,src/runner.rs,src/lib.rs,src/case.rs,src/baseline.rs}` (`--guardian none` flag form plus `baseline capture|diff` subcommands; deps per step 3); `rust/fontaine-evals/tests/{baseline,none_mode}.rs`; `evals/dataset/{tier05,floor,spawn}.jsonl` (relabels and arg fixes, step 3); `rust/fontaine-core/tests/docs.rs` (required-pages list);
`rust/fontaine-tools/src/preclassify.rs`; `rust/fontaine/tests/cassettes.rs` (manual-mode cassette, built in step 4; the auto-mode cassette is added in Phase 4);
`rust/fontaine-tui/src/{app,transcript,approval,statusbar}.rs`; tests under `rust/fontaine-core/tests/`,
`rust/fontaine/tests/`, `rust/fontaine-tui/tests/`;
`rust/fontaine/src/{lib,cli}.rs` (TUI wiring; the two rule flags); `rust/fontaine-core/src/config.rs` (CLI rule merge; rule sources); `docs/{permissions,tui,headless,telemetry,tools,configuration}.md`, `docs/architecture.md`,
`docs/index.md`, `README.md`, `evals/README.md`, `CONTRIBUTING.md`, `tasks/fontaine_harness_plan.md` (crate dependency graph).

## Tests owned by this phase

- **permission pipeline** (`tasks/design/testing.md` Testing Strategy; `rust/fontaine-core/tests/permissions.rs`): mode
  × risk-class matrix; plan-mode terminal branch 9 (edit-class blocked outright; the guardian leg
  prompts in the interim and denies in headless; in-scope `shell("ls")` proceeds via Tier 0.5); read-only scope check incl. symlink-escape denial
  and an out-of-scope `cwd`; sequential tool calls in one turn (a Deny returns its own tool_result; later calls are still adjudicated, one at a time); the `[permissions] deny` rule
  holds in EVERY mode; critical-path `rm`/`rmdir` (incl. unresolvable targets and commands that fail
  to parse or are compound) prompts in manual/acceptEdits/plan, denies in `dontAsk` and headless, and is never
  auto-approved; critical-path covers every workspace root and its parents; protected-path writes via `edit`/`write` (the enumerated list) prompt or deny per
  mode; an `[permissions] ask` rule forces a prompt in `manual` and denies in `dontAsk`; every
  decision appends an audit record matching the audit schema in `tasks/design/permissions.md` §Core data flow (`tier`, `decision`, `rule` with its source,
  `args_digest`, `agent_id`, `tool_use_id`).
- **permission rules** (`rust/fontaine-core/tests/permission_rules.rs`; the `auto` legs are Phase 4): grammar per tool kind; `shell` deny/ask hit compound, wrapped and
  unparseable commands; `shell` allow never matches a compound command; resolved-path matching incl.
  symlinks (a symlink into a denied path is denied; an allow on a symlinked alias does not match); `webfetch(domain:)` normalization; an allow rule never passes deny/ask/critical-path/
  protected-path and never lets an edit run in `plan`; an allow rule pre-approves in `dontAsk` and
  skips the prompt in `manual`; "allow session" lasts only for the session, "always allow" writes
  exactly the shown rule to the user file and the next matching call in the same session skips the prompt; the written rule round-trips through `parse_rule` and matching the original call (absolute paths, escaped characters, quoted shell argv); floor-forced and non-simple shell prompts offer no allow session / always allow; `--allowed-tools` / `--disallowed-tools` act as CLI rules.
- **windows** (`#[cfg(windows)]`; compile-checked locally with `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings`, executed in the windows CI job): critical-path prompts for a drive root and for
  `%USERPROFILE%` targets; path-rule matching is case-insensitive and resolves junctions; protected
  paths match across separators and case.
- **baseline** (`rust/fontaine-evals/tests/baseline.rs`): `diff` over constructed baseline and run files reports exactly the cases whose outcomes differ under the mapping in `tasks/design/testing.md` (Fontaine `allow` ↔ CC allowed; `deny` and `prompt` ↔ CC denied in headless); denial extraction from a checked-in sample `claude -p --output-format json` result (fixture under `rust/fontaine-evals/tests/fixtures/`).
- **eval `none` mode** (`rust/fontaine-evals/tests/none_mode.rs`): `pipeline` cases for manual/acceptEdits/plan/dontAsk run through the real `PermissionPipeline` and are scored against `expected`/`expected_tier`; `auto` cases are skipped; exit 1 on any mismatch.
- **auto rejected in the TUI**: `fontaine --permission-mode auto` (no `-p`) exits 2 with the same usage error as `-p`.
- **hook slot**: with a test-double decision source, `updated_input` is what the floor, rules and
  audit see; deny > ask > allow; an `allow` never passes the floor; error / timeout / malformed ⇒ no
  decision.
- **audit/telemetry consistency**: over the mode × risk-class matrix, each audit entry has exactly one
  matching `tool_decision` event and one headless `gate_decision` event (same `tool_use_id`, equal to the stream's `tool_call` id; `tier` and `decision` equal as strings);
  the audit log is still written when telemetry is off or disabled by the project; `rust/fontaine/tests/telemetry_otlp.rs` asserts the `fontaine.tool_decision.count` metric with `tier` and `decision` attrs.
- **Tier 0.5 pre-classifier**: the golden adversarial set from `tasks/design/testing.md` Testing Strategy — the
  fail-through soundness property must hold for anything not fully parsed as one simple command; each
  allowlist-table row (listed flags pre-classify, the excluded flags fall through); in-scope `ls` → no
  prompt plus an audit record with `tier = tier05`; `ls ../` → one human prompt (interim; the
  counting-guardian form is owned by Phase 4); a relative path under an out-of-scope `cwd` falls
  through; `allow_extra` — an extra entry given a flag falls through to the guardian, an out-of-scope
  positional falls through, an in-scope positional pre-classifies.
- **headless** (`rust/fontaine/tests/pipeline_headless.rs`, scripted `openai-compatible` endpoint via the Phase-1 `common/mod.rs` helper, passing `--permission-mode manual` explicitly):
  every would-prompt outcome becomes a Deny returned as the tool_result, the run continues, exit
  code 0; the `result` event's `denials` lists each denied call with `tool`, `tier`, `rule`, `explanation`.
- **TUI** (`rust/fontaine-tui/tests/`): ratatui `TestBackend` with injected key events and a scripted
  `rig_core::test_utils` model — approval prompt → allow runs the tool (file written), deny returns
  the denial tool_result; Shift+Tab cycles manual → acceptEdits → plan → (auto once Phase 4 lands;
  wraps to manual until then); `/add-dir` of a new directory, then a scripted `read` in it is allowed and the earlier conversation is still in the model's request.

## Definition of done

- Offline gates (no keys, no human): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green, including the scripted-endpoint headless, pipeline, permission-rule, baseline, `none`-mode, and TUI tests above; `cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian none` (from `rust/`) exits 0, and the same step is added to `ci.yml` on all three OSes; no mode or allow rule reaches past the deterministic floor; `docs/permissions.md` + pipeline diagram
  match the shipped step order and `rust/fontaine-core/tests/docs.rs` is green; CHANGELOG entry appended.
- Needs a human (the agent cannot push): push the branch and confirm the windows CI job runs the `#[cfg(windows)]` tests green.
- Needs a human and live keys (not CI): `tasks/design/testing.md` Manual Verification item 1 passes (live rendering and latency); the manual-mode rig-cassette recorded per `tasks/design/testing.md` Testing Strategy (Integration, cassettes).
- Agent step needing a logged-in `claude` CLI (manual or eval-side; never CI): the `claude -p` baseline capture, then `baseline diff` against a `--guardian none --out` run file, and triage of each divergence (fix or record).
- Log the phase in the master plan's Progress log.

## Decisions

- 2026-10-22: Claude Code's per-subcommand shell rules are not offered; shell allow rules match one literal simple command.
- 2026-10-22 AFK: The rich per-decision record is a separate fontaine-core type `DecisionRecord` (one per gate decision; a human answer adds a second, `tier = human`); fontaine-tools' `GateDecision` stays the thin `ToolGate::check` verdict — keeps shipped Phase-2 code and leaves `Tier` in fontaine-core, so fontaine-tools still never depends on it.
- 2026-10-22 AFK: Call ids reach the gate through a rig `AgentHook::on_dispatch` that fills a per-agent current-call slot on `SessionState` (sound under `tool_concurrency = 1`), recorded as the audit field `tool_use_id`; decisions reach headless/TUI through a channel on `SessionState` drained by the shared turn runner — rig hands `call_id` only to hooks, and stream-order pairing breaks because a turn's tool calls stream before their results.
- 2026-10-22 AFK: Runtime `/add-dir` ships as in Claude Code by rebuilding scope, registry and agent between turns through the shared setup (conversation and session store kept, ledger record appended), with mode/rules/grants/scope behind a lock in `SessionState` — `Scope` and per-root instances stay immutable, so no new containment surface in fontaine-tools, and the Phase 2 promise holds.
- 2026-10-22 AFK: Every merged permission rule carries its source scope (`Slot`, or `session`; a live "always allow" is `user`), deduped on (rule, source) — the audit `rule` field requires the source and today's merge drops it.
- 2026-10-22 AFK: The shared fontaine-core turn runner yields a typed session-event enum to a consumer callback and returns a mode-neutral outcome; headless keeps its JSON, partial messages, budget cap and JSON-schema check — the smallest split that serves both surfaces without fontaine-core knowing the bin crate's `cli` types.
- 2026-10-22 AFK: An eval case whose workspace symlink cannot be created or dangles (`linked -> /etc` on Windows) is reported as skipped, not scored and not a failure — nothing made the old "counts as out of scope" true, and a dangling in-root link would likely resolve in scope and mis-score.
- 2026-10-22 AFK: Baseline capture maps Fontaine rules to Claude Code (`shell`→`Bash` etc.; word-only `shell(w)` deny/ask → `Bash(w)` + `Bash(w *)`) and skips cases with `allow_extra` — tool names differ and Claude Code's word-only Bash rule is exact where Fontaine's deny/ask is a prefix.
