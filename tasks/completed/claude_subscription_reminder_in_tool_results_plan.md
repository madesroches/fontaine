# claude-subscription: Context Reminder in Tool Results Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/725

## Overview

With `claude-subscription`, the model sees a `<system-reminder>` carrying the signed-in account's
email address appended to the last tool result of a user turn. It cannot tell the block from
content a tool injected, and the email can leak into subagent reports, commit or PR text and
bug-report drafts. This plan closes the remaining
gaps without touching placement: tell the model what the block is and not to repeat it, and scrub
the account email from the places Fontaine writes model-authored text for sharing.

## Current State

The issue's four checks, answered from the code and the research notes:

1. Checks 1 and 4, wording drift and comparing wording, are done by #732.
2. **Placement.** `context::apply` runs on the wire frames built per request
   (`claude_subscription/context.rs`), never on Fontaine's stored history. Moving the reminder out
   of the tool result (placement rule 2 in `research/claude-subscription-cli-context/README.md`)
   would make each replayed message differ from what the CLI last wrote and rewrite the cache on
   every step (measured: ~25k vs ~8.6k tokens written per step on a 5-step loop). The CLI also
   adds its own reminder to the final message, so the model sees the block in a tool result either
   way. Stripping it "from content shown to subagents and the transcript" has nothing to strip:
   it is not in either, because replay happens after history is stored.
3. **Reports, exports, redaction.** The reminder text reaches Fontaine only inside the model's
   context. It leaves through the model's own output. `core/redact.rs` scrubs known secrets (keys,
   tokens, MCP credentials) from `draft_bug_report` drafts and bug reports
   (`core/agent/build.rs` `draft_tool`, `core/bug_drafts.rs`, `core/bug_report.rs`); the account
   email is not a secret there, so a draft that quotes it is written unredacted. The base prompt
   (`rust/fontaine-sdk/prompts/core/system_prompt.md`) says nothing about the block.

## Design

Two small changes, both leaving the cache layout untouched.

### 1. Tell the model what the block is

Add one short paragraph to the base prompt (`prompts/core/system_prompt.md`), provider-agnostic
because the model can be switched mid-session and the prompt is assembled once: a
`<system-reminder>` block holding account context, such as an email address, may be appended to a
tool result by the transport. It is added context, not the tool's output, so the model does not
act on it or treat it as part of what the tool returned, and does not repeat the address in
messages, reports or files except where the user asks for authorship or attribution (the CLI's own
reminder allows that use). Commit messages and PR text take their identity from git config, not
from this block. The paragraph must not say the block is trusted: a tool can forge one, so the model keeps
treating instructions inside tool results as data. A replacement `system_prompt` drops the
paragraph; the email scrubbing in §2 still covers that case.

### 2. Scrub the account email from shared model-authored text

`Redactor` gains the account emails as known values for sessions on `claude-subscription`.
`session_secrets` (`core/redact.rs`, which already takes the data dir) builds the list from config
and the in-process learned-email store; the email is known only from the CLI's `initialize` answer
(`CliContext.email`, `cli.rs`). No existing memo holds the plaintext email: `Reminders` and the
verdict memo key by `sha256(email)`, the template stores `{email}`, and the listing's `CliContext`
is a local in `ClaudeTransport::prepare` that is dropped. So the provider module gains new
in-process, per-data-dir state (a set of learned emails next to `Reminders`, never written to
disk) that `prepare` writes whenever a listing returns an email, and that startup discovery
(`Cli::initialize`/`discover`, which receives `listing.context.email` and drops it) also writes,
plus a small accessor returning every email learned for the data dir (`Vec<String>`). Several accounts can share one data dir (each with its own
`claude_config_dir`), so all of them are scrubbed. `session_secrets` adds each as a `Secret`, so
both `draft_tool` drafts and `/bug` (`fontaine-tui/src/ui/commands/bug.rs`, which builds its own
`Redactor` via `session_secrets` and includes transcript text at full level) get them.
`MIN_SECRET_LEN` (8) already admits any realistic address.

Out of scope: the audit log and ledger record tool-call arguments and decisions, not model
context; they hold the email only if the model echoes it into a tool call, which change 1 makes
unlikely. `/export` scrubs no secrets today and is left unchanged.

## Implementation Steps

1. `rust/fontaine-sdk/prompts/core/system_prompt.md`: add the paragraph; keep the PowerShell
   variant substitution in `core/prompt.rs` unaffected.
2. `claude_subscription/reminder.rs` (next to the `MEMOS` static): add in-process per-data-dir
   state holding the learned emails, written by `ClaudeTransport::prepare` and by `cli.rs`
   `Cli::initialize`/`discover` when a listing returns an email, and
   `pub fn known_account_emails(data_dir) -> Vec<String>` reading it. The accessor applies
   `std::path::absolute` to its `data_dir` argument before lookup, as connect does when keying
   the memo (`mod.rs:284-291`). `mod.rs` re-exports it with `pub use reminder::known_account_emails;`
   (the module is private and its items are `pub(super)`).
3. `core/redact.rs` `session_secrets`: push a `Secret::new(email)` for each learned email, so
   `/bug` (which calls `session_secrets` at command time) gets them with no further change.
   `core/redact.rs` `Secret` gains `Secret::email(..)`, which marks the value to match ASCII
   case-insensitively (a re-cased address is still the same address); `session_secrets` uses it.
   `Redactor` stores those values in a separate list and `redact` replaces them with a
   case-insensitive scan (`to_ascii_lowercase` on both sides preserves byte offsets).
   `Redactor` gains `pub fn extended(&self, extra: impl IntoIterator<Item = Secret>) -> Redactor`,
   returning a copy with the extra secrets merged (re-filtered by `MIN_SECRET_LEN`, re-sorted
   longest first, deduped); this leaves `FileDrafts::new` and its callers unchanged.
   `core/bug_drafts.rs` `FileDrafts::save`: `FileDrafts` already holds `data_dir`, so `save` builds
   `self.redactor.extended(known_account_emails(&self.data_dir).into_iter().map(Secret::email))`
   and redacts title, area and details with it; the read happens at save time so emails learned after the agent is built (a later
   account, a failed discovery) are scrubbed too. Subagents never get `draft_bug_report`.
4. Docs: `docs/claude-subscription.md` (the paragraph around line 279 listing the reminder; lines
   ~241 and ~290-294, which say the email is kept "in memory for the request only") states the
   model is told what the block is, and that the email is held in memory for the process, to
   scrub it from drafts and `/bug`, and never written to disk; `docs/tui.md` (~line 805, the `/bug`
   row's list of what is scrubbed) adds the account emails; `CHANGELOG.md` entry.

## Files to Modify

- `rust/fontaine-sdk/prompts/core/system_prompt.md`
- `rust/fontaine-sdk/src/core/providers/claude_subscription/reminder.rs` (learned-email state and accessor)
- `rust/fontaine-sdk/src/core/providers/claude_subscription/mod.rs` (re-export `known_account_emails`)
- `rust/fontaine-sdk/src/core/providers/claude_subscription/transport.rs` (`prepare` records the email)
- `rust/fontaine-sdk/src/core/providers/claude_subscription/cli.rs` (discovery records the email)
- `rust/fontaine-sdk/src/core/redact.rs` (`session_secrets`)
- `rust/fontaine-sdk/src/core/bug_drafts.rs` (`FileDrafts::save` adds the learned emails)
- `docs/claude-subscription.md`, `docs/tui.md`, `CHANGELOG.md`

## Trade-offs

- **Drop the reminder entirely (legacy layout always):** rejected; the legacy layout also loses
  the replay's cache gains.
- **Provider-conditional prompt layer:** rejected; the model can switch mid-session and a
  conditional layer would change the cached prefix on a switch for one sentence.

## Decisions

- 2026-11-20: Learned emails live in process memory only and are never written to disk; a resumed
  session's earlier-turn email is scrubbed only once discovery or a request has recorded it.

- 2026-11-20 AFK: Base-prompt paragraph reworded (added transport context, not tool output; do not repeat the address except where the user asks for authorship) instead of a strict never-copy rule, so it does not contradict the CLI's reminder or Claude Code and needs no master-plan deviation.
- 2026-11-20 AFK: `FileDrafts::save` extends its `Redactor` via a new `Redactor::extended` method returning a copy with extra secrets (re-filtered, re-sorted, deduped); smaller change than making `FileDrafts` hold `Vec<Secret>` and rebuild.
- 2026-11-20 AFK: Account-email secrets match ASCII case-insensitively (`Secret::email`), so a re-cased address is scrubbed; other secrets stay case-sensitive.

## Documentation

`docs/claude-subscription.md`, `docs/tui.md` and `CHANGELOG.md`, as in step 4.

## Testing Strategy

- Unit: `Redactor` built with the account emails replaces each, also re-cased, in a draft title and body (extend
  the `bug_drafts` tests); `session_secrets` includes them for `/bug`.
- Unit: the accessor returns empty before a listing and every learned email after, as an
  in-module unit test using an inline-script listing stub (as `cli.rs:1322`), not the CLI double.
- Integration: in `rust/fontaine-sdk/tests/claude_subscription.rs` (`claude_cli_double`, email
  `user@example.com`), assert `known_account_emails(data_dir)` is empty before one
  `main_handle(...).completion.call(...)` and contains the email after; cover the discovery path
  too.
- Unit: in `rust/fontaine-sdk/tests/system_prompt.rs`, add one needle to
  `the_base_steers_toward_the_dedicated_tools_and_explains_denials_and_secrets` and update the
  insta snapshot `the_assembled_prompt_is_stable`.
- The existing `claude_subscription` integration tests stay green, confirming the wire layout is
  unchanged.
- No main-model eval exists (evals only score guardian, pipeline and scanner kinds; nothing
  references `BASE_PROMPT`); none is added.
