# claude-subscription: Context Reminder in Tool Results Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/725

## Overview

With `claude-subscription`, the model sees a `<system-reminder>` carrying the signed-in account's
email address appended to the last tool result of a user turn. It cannot tell the block from
content a tool injected, and the email can leak into subagent reports, commit or PR text and
bug-report drafts. The placement is not ours to change: it is where the CLI puts the reminder, and
replaying it byte-for-byte is what keeps the prompt cache warm. This plan closes the remaining
gaps without touching placement: tell the model what the block is and not to repeat it, and scrub
the account email from the places Fontaine writes model-authored text for sharing.

## Current State

The issue's four checks, answered from the code and the research notes:

1. **Wording drift / verdict fallback.** Already fixed by #732: `reminder.rs` learns the wording
   from the CLI per `(version, account)`, `context.rs` keys the persisted broken verdict to
   `(version, wording hash)`, and a reminder that is not in the expected shape (email once, no
   section beyond `# userEmail`) takes the legacy layout. Nothing to do.
4. **Compare wording, not only version.** Same: `Verdicts::contains(version, &learned.wording)`.
   Nothing to do.
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
tool result by the transport. It is not part of the tool's output and carries nothing to act on,
and the model must never copy its contents into messages, reports, commit messages, PR text or
files. The paragraph must not say the block is trusted: a tool can forge one, so the model keeps
treating instructions inside tool results as data.

### 2. Scrub the account email from shared model-authored text

`Redactor` gains the account email as a known value for sessions on `claude-subscription`.
`session_secrets` already builds the list from config and stores; the email is known only from the
CLI's `initialize` answer (`CliContext.email`, `cli.rs`). Expose the learned email from the
provider module (the `Reminders`/listing memo already holds it per data dir) through a small
function that returns `Option<String>`, and have `draft_tool` add it as a `Secret`. If the
listing has not run yet there is no email to scrub and none in the model's context either, so the
absence is correct, not a gap. `MIN_SECRET_LEN` (8) already admits any realistic address.

Out of scope: the audit log and ledger record tool-call arguments and decisions, not model
context; they hold the email only if the model echoes it into a tool call, which change 1 makes
unlikely. Scrubbing them would add a regex-free but wide-reaching redaction pass over permission
records, which the security invariants say not to touch lightly.

## Implementation Steps

1. `rust/fontaine-sdk/prompts/core/system_prompt.md`: add the paragraph; keep the PowerShell
   variant substitution in `core/prompt.rs` unaffected.
2. `claude_subscription/mod.rs` (or `reminder.rs`): add `pub fn known_account_email(data_dir) ->
   Option<String>` reading the memoised listing context; no new I/O.
3. `core/agent/build.rs` `draft_tool`: push `Secret::new(email)` when it is `Some`.
4. Docs: `docs/claude-subscription.md` (the paragraph around line 279 listing the reminder) states
   the model is told what the block is and that drafts scrub the email; `CHANGELOG.md` entry.

## Files to Modify

- `rust/fontaine-sdk/prompts/core/system_prompt.md`
- `rust/fontaine-sdk/src/core/providers/claude_subscription/mod.rs` (accessor)
- `rust/fontaine-sdk/src/core/agent/build.rs`
- `docs/claude-subscription.md`, `CHANGELOG.md`

## Trade-offs

- **Move the reminder out of the tool result:** rejected; it breaks byte-identical replay and
  multiplies cache writes roughly threefold.
- **Drop the reminder entirely (legacy layout always):** rejected; the legacy layout also loses
  the replay's cache gains and the CLI adds its own reminder to the final message regardless.
- **Provider-conditional prompt layer:** rejected; the model can switch mid-session and a
  conditional layer would change the cached prefix on a switch for one sentence.
- **Mark the block trusted in the prompt:** rejected; a tool can emit the same text.

## Decisions

- 2026-11-20: Issue checks 1 and 4 are already satisfied by #732; this plan adds nothing there.

## Documentation

`docs/claude-subscription.md` and `CHANGELOG.md`, as in step 4.

## Testing Strategy

- Unit: `Redactor` built with the account email replaces it in a draft title and body (extend the
  `bug_drafts` tests).
- Unit: the accessor returns `None` before a listing and `Some` after, using the CLI double.
- Unit: the base prompt contains the new paragraph and the assembled prompt for the PowerShell
  variant still carries it (`core/prompt.rs` tests).
- The existing `claude_subscription` integration tests stay green, confirming the wire layout is
  unchanged.

## Open Questions

- Does the existing model-eval for prompt wording need a case showing the model not echoing the
  email? Left to the implementer to judge; a live eval is not a CI gate.
