# claude-subscription: the CLI's context reminder (CLI 2.1.319, 2026-11-07)

Measurements behind the native cache layout in
`rust/fontaine-core/src/providers/claude_subscription/context.rs` (issue #480). The scripts run
the adapter's `fixed_args` / `FIXED_ENV` subset against the real CLI on `haiku`; `proxy.py` is a
logging reverse proxy (`ANTHROPIC_BASE_URL`) that writes each `/v1/messages` body to a directory.
They read the account email from `claude auth status --json` at runtime and contain none.

| Script | What it probes |
|---|---|
| `probe.py` | shared harness: launches the CLI with the adapter's flags and returns usage |
| `tool_probe.py` | a final tool result with array content |
| `tool_probe2.py` | parallel tool results, with and without a trailing text block |
| `tool_probe3.py` | rules 4 and 5: trailing empty text block, and empty / absent / `[]` result content |
| `loop_probe.py` | the 5-step tool loop, today's layout against the replayed reminder |
| `proxy.py` | the logging proxy |

## Where the context goes

- The `session_context` reminder goes on the user message that queries; its position depends on
  that message's shape (rules below). A `shouldQuery:false` frame directly before it is merged
  into it first.
- `# Environment` is a `system`-role message after the first user message.
- From the second request on, `<total_tokens>15000000 tokens left</total_tokens>` and the date go
  in a trailing `system`-role message after the final user message, where the CLI puts its own
  breakpoint. Only the reminder sits inside a message that is replayed later.

## Placement rules (merged content of the queried user message)

1. No `tool_result` block: a new first text block, `REMINDER + "\n"`.
2. Last block is a `tool_result`: with string content, `"\n\n" + REMINDER` appended to the string;
   with array content, a new last text block, exactly `REMINDER`, pushed onto the array (existing
   blocks unchanged). With parallel results only the last changes.
3. Has a `tool_result` but ends with another block (text): a new last text block, `REMINDER`.
4. Last block is a `tool_result` followed by an empty text block: the empty block is dropped and a
   new last text block, exactly `REMINDER`, is appended.
5. Result content variants under rule 2: `""` becomes the string `REMINDER`; absent content becomes
   the string `REMINDER`; `[]` becomes `[{"type":"text","text": REMINDER}]`.

Not verified, so the adapter leaves them unchanged: a `tool_result` whose array content ends in a
`document` or image block and a final `document`/image block in a run with no `tool_result`. A
consecutive-user-frame run (two `shouldQuery:false` frames before an assistant) gets the reminder
by the merged-content rules, without that merging having been measured live.

## Reminder sections

The reminder can hold `userEmail`, `attachedProject`, `gitStatus` and `perforceMode`. With an
initialized one-commit git repository above the CLI's cwd, run with and without
`GIT_CEILING_DIRECTORIES`, and with `CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS=1` or
`includeGitInstructions:false`, no `gitStatus` section appears in this mode (run inline in a
shell, no script). The `# Environment` block then says "Is a git repository: true", stable per
machine. `attachedProject` and `perforceMode` need env vars the adapter's rebuilt environment
drops.

The CLI dedups only against `session_context` attachments in its own message list, which plain
replayed text is not, so it adds a new reminder to every final message, and no stdout event carries
the text.

## Other findings

- The `initialize` answer holds `account.email` (plus `organization`, `subscriptionType`,
  `apiProvider`) and `claude_code_version`.
- Splitting the final frame does not help: a query frame with `content: []`, `""` or one empty text
  block is answered, but the CLI merges the preceding `shouldQuery:false` frame into it and the
  reminder still leads the merged message.

## 5-step tool loop, about 8.2k-token tool results, haiku

| Layout | write per step, steps 3-5 | read |
|---|---|---|
| marker on last history user message (earlier layout) | 25,321 | grows |
| reminder replayed, marker on the final frame | 8,584 (8,478 at 1 h + 106 at 5 min) | 26,638 / 43,594 / 60,550 |

With the replay each step writes its one new tool result, the reminder (about 170 tokens) and the
date tail (about 40 tokens), matching a native session plus the reminder.
