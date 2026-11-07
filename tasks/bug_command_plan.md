# `/bug` Command Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/462

## Overview

Add a `/bug` slash command that assembles a Fontaine bug report (environment, provider/model,
redacted config, warnings, guardian summaries, optionally the transcript tail), shows the user
exactly what will leave the machine, and on confirmation files it as a GitHub issue. It also adds
a draft-only `draft_bug_report` tool so the model can queue a report; only the user submits. Every
submission is recorded in the ledger and audit log.

## Current State

- Slash commands are one table in `fontaine-tui/src/commands.rs` (`Command` enum, `Builtin`
  table, `parse_command`); handlers live in `fontaine-tui/src/ui/commands/*.rs` and dispatch in
  `fontaine-tui/src/ui/mod.rs` (~L1147 for `Export`).
- `/export` (`fontaine-core/src/export.rs`) already renders the conversation as Markdown from the
  conversation logs; `ledger.rs` holds the session ledger (`Record` enum variants such as
  `GuardianOff`); `audit.rs` has `DecisionRecord` and `AuditLog`.
- `doctor_report.rs` builds config rows via the same renderers `/config` uses, so no credential or
  env value appears. `credentials.rs` loads stored keys; `models.rs::credential()` resolves an
  account key.
- `Approver::warn` is the user-visible warning channel; warnings are not retained today.
- `RiskClass` (`fontaine-tools/src/risk.rs`) has only `ReadOnly`/`WorkspaceWrite`/`Arbitrary`; each
  tool's class lives in the `TOOLS` table in `fontaine-tools/src/descriptor.rs` (`builtins.rs::risk_of`
  only looks it up); the choke-point test enforces it. "Coordination" is `Tier::Coordination`
  (`fontaine-core/src/tier.rs`), assigned in `permissions.rs` (~L1144) via `descriptor::is_coordination` (hard-coded
  `COORDINATION` list); those tools skip the guardian after the floor.
- No `build.rs` exists in `fontaine`; the only build metadata is `env!("CARGO_PKG_VERSION")`.
- `export.rs::render` renders the whole conversation; it has no turn-limited variant.
- `DecisionRecord::new` requires tool, args_digest, `Tier`, `Decision`, permission_mode, headless.

## Design

### 1. Report builder (`fontaine-core/src/bug_report.rs`, new)

Pure, no I/O: `BugReport::build(&Facts, Level) -> BugReport` and `BugReport::render() -> Rendered
{ title, body, bundle }`.

- `Facts` is gathered by the caller: version, git sha and build target (compile-time `env!`
  from a new `rust/fontaine/build.rs` that emits git sha and target, `unknown` when `.git` is absent), OS/arch,
  `$TERM` and size, install method, provider/model, guardian profile/model, permission mode,
  config rows (from the `doctor_report` renderers), contributing config scopes, recent warnings,
  guardian verdict summaries (from the ledger), session id, user description, and the transcript
  tail (via a new turn-limited tail helper in `export.rs`, since `render` covers the whole conversation).
- `Level::Minimal` (default): environment + config snapshot + warnings + guardian verdict
  summaries + description. `Level::Full` adds the last N turns (default 20) of transcript with
  tool calls/results.
- Issue body: `Minimal` content inline; the transcript in a collapsed `<details>` block only when
  the whole body fits the URL/issue limits, otherwise only in the local bundle file.

### 2. Redaction (`fontaine-core/src/redact.rs`, new)

`Redactor::new(secrets: impl IntoIterator<Item = Secret>)` scrubs **known secret values** (stored
credentials, values of env vars named by `api_key_env` of configured accounts, MCP server env
values, OAuth tokens) from every string, replacing them with `[redacted]`, then a token-pattern
backstop pass (`sk-…`, `ghp_…`, bearer headers, PEM blocks). Values under 8 chars are skipped to
avoid shredding the text. This is report scrubbing, not tool-argument matching, so the permission
pipeline's no-regex invariant is unaffected. Redaction runs on the final rendered text (body and
bundle), after assembly, so a secret in any field is caught.

### 3. Submission (`fontaine-core/src/bug_submit.rs`, new)

Constants: `REPO = "madesroches/fontaine"`, `LABELS = ["bug"]`; not configurable from any scope.

- Always writes the bundle to `<data_dir>/bug-reports/<session>-<timestamp>.md` (owner-only,
  via `owner_only`), never overwriting.
- `Submit::Gh`: if `gh auth status` succeeds, `gh issue create --repo REPO --label bug --title …
  --body-file <bundle body>` run through the argv API (no shell). Returns the issue URL.
- `Submit::Url` (fallback, or user's choice): prints `https://github.com/REPO/issues/new?title=…
  &labels=bug&body=…` with the body truncated to a safe length (~6000 chars) with a pointer to the
  bundle file for the rest. Nothing is opened automatically; the URL is shown for copying.
- Transcript is never put in the URL.

### 4. TUI flow (`fontaine-tui/src/ui/commands/bug.rs`, `bug_prompt.rs`)

`/bug [description]`: description from the argument or a prompt. A preview screen (pattern of
`confirm_prompt.rs`) shows the full rendered payload (scrollable), the level toggle
(`m`inimal / `f`ull, default minimal), the submission method (gh or URL), and the destination repo.
Enter confirms; Esc cancels with nothing written or sent. The bundle file is written only after
confirmation (a cancelled preview leaves no file). On submit: ledger `Record::BugReport { method, level,
url }` (with its fold arm) plus an audit `DecisionRecord` with tool label `bug` and `Tier::BugReport`
(label `bug_report`); the URL or file path is shown in the transcript. `Command::Bug` is
`main_only() == true`, so subagent views refuse it. The prompt lives in
`fontaine-tui/src/bug_prompt.rs` with a `Prompt` variant wired in `lib.rs`/`app.rs`.

### 5. `draft_bug_report` tool

Takes `title`, `area`, `details`; writes `<data_dir>/bug-drafts/<id>.json` after redaction, no
network. Registered as a `TOOLS` row in `descriptor.rs` (`Destructive::Always`, `RiskClass::Arbitrary`) with the new
`Tier::BugDraft` (never guardian-judged, after the floor); the pinned array in `rows_are_pinned` is
updated. Omitted for subagents and headless runs, capped at 3 per
session. The TUI shows a footer count of drafts; `/bug` lists drafts and loads one as the
description. `[feedback] drafts = "on" | "quiet" | "off"` (user-scope only, `config_scope.rs`;
`off` omits the tool). Submission stays user-only and outside the guardian.

### 6. Warnings retention

The TUI `Approver::warn` override (`fontaine-tui/src/lib.rs`, ~L172) records into a bounded ring buffer (last 50) on the runtime state so
the report can include them. Display-only state: recovering lock allowed with a one-line reason.

## Implementation Steps

1. `redact.rs` + unit tests.
2. `bug_report.rs` builder + unit tests (levels, truncation, details block).
3. `bug_submit.rs` (bundle file, gh argv, URL builder) + tests with a fake `gh` script.
4. Warning ring buffer; ledger `BugReport` record and audit record.
5. `/bug` command, preview prompt, wiring in `ui/mod.rs`.
6. `draft_bug_report` tool, risk class, `[feedback]` config, subagent/headless exclusion.
7. Docs and CHANGELOG.

## Files to Modify

`fontaine-core/src/{lib,ledger,audit,config,config_catalog,config_scope,agent/*}.rs`,
`fontaine-core/src/{export,tier,permissions}.rs`, `fontaine-tools/src/descriptor.rs`,
headless schema, `evals` expected_tier, `rust/fontaine/build.rs` (new), `AGENTS.md`,
`fontaine-tui/src/{commands,app,lib,bug_prompt}.rs`,
`fontaine-tui/src/ui/{mod.rs,commands/mod.rs}`, new files above, `docs/tui.md`,
`docs/configuration.md`, `docs/tools.md`, `CHANGELOG.md`.

## Trade-offs

- URL + local bundle first, `gh` optional (Gemini model) over owning a gist/upload service: the
  user sees the exact text and no extra dependency or credential is needed.
- Known-value scrubbing plus a pattern backstop over patterns alone: patterns miss custom keys.
- Transcript default-off (stricter than Claude Code) because the target repo is public.

## Decisions

- 2026-11-07: Model-initiated reporting is draft-only; the tool does no network I/O and never
  submits (resolves open question 2).
- 2026-11-07: Headless `fontaine bug --session <id>` CLI is out of scope for v1; the builder is
  pure so it can be added later.
- 2026-11-07: Target repo and labels are constants (open question 5).
- 2026-11-07: `draft_bug_report` gets a new `Tier::BugDraft` (never guardian-judged after the floor; update `Tier::ALL`, `as_str`, headless schema, evals expected_tier) because `COORDINATION` is documented as reaching only agents of the same tree, which a data-dir write does not fit.
- 2026-11-07: `draft_bug_report` has `Destructive::Always` + `RiskClass::Arbitrary` like `send_message`, since `path_policy_is_consistent` ties `Never` to `ReadOnly`.
- 2026-11-07: The bug submission audit entry uses a new `Tier::BugReport` (label `bug_report`, following `Tier::Afk`/`GuardianOff`), tool label `bug`, same schema/evals fallout.
- 2026-11-07: `/bug` is a slash command only (none headless) and is `main_only()`.

## Documentation

Update `docs/tui.md` (the command, preview, levels), `docs/configuration.md` (`[feedback]`),
`docs/tools.md` (`draft_bug_report`), the master plan's Decisions (`Tier::BugDraft`, `Tier::BugReport`), and the `AGENTS.md`
security-invariant list of never-guardian-judged exceptions.

## Testing Strategy

- Unit: redactor with planted values in every field (config, env, credential, transcript, warning)
  asserts none survive in body, bundle or URL; short-value skip; report levels; URL truncation and
  encoding; gh argv construction.
- Integration (TUI harness, `tests/tui.rs` style): `/bug` shows a preview; Esc sends/writes
  nothing; Enter with a fake `gh` on `PATH` creates the issue and records ledger + audit; without
  `gh` the URL fallback prints and the bundle exists.
- Choke-point test stays green with the new tool; a test asserts the tool is absent for
  subagents, headless runs and `drafts = "off"`, and that project scope cannot set `[feedback]`.

## Open Questions

None.