# Session Ledger Design

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Session ledger

The Claude CLI adapter (`claude-subscription`) keeps no conversation of its own: each model call
starts a fresh CLI process that replays the history Fontaine holds from rig-memory and the ledger,
with a CLI session id derived (SHA-256 formatted as a UUID) from the Fontaine session id, the
caller (root, guardian or a subagent's request id), the canonical model, the system prompt and the
first frame, so an unchanged prefix keeps its id and no two callers share one. Nothing is resumed
or persisted by the CLI; compaction and a model change simply derive a new id.

Two stores per session. rig-memory holds the model context, which compaction may rewrite. The
**session ledger** (`fontaine-core::ledger`) is Fontaine-owned and append-only: user messages
verbatim (each `UserMessage` carries an `origin`: `dialog` for the answers a question dialog recorded, `prompt` for a message typed at the idle prompt (or the one headless `--input-format text` prompt) as the first submission after the previous answer was shown, and the default `other` for every other writer and for older ledgers), `AgentReply` records (the trailing 2,000 characters of a root turn's final answer; `/clear` drops them and a rewind keeps those placed before the first dropped prompt), mode changes, approvals and session grants, denial records, actions taken, the workspace
roots, trust state, the names of the `[env]` overrides (never their values), and the git remotes (read with `gix` from the repo config without following `include`/`includeIf`; none when there is no `.git` or `gix` errors — Fontaine never spawns `git`) and pre-session file index captured at start (the index
respects `.gitignore` and is capped at 50,000 paths — overflow is recorded as truncated; paths
the index does not hold are reported to the guardian as `unindexed`, or as `session_scratch` when they sit inside the session scratch directory; the guardian view also marks a path `absent` when nothing is there and `git_tracked` when git's index holds it, both set from the disk and the index when the request is built). The guardian's
`AdjudicationRequest` is built only from the ledger — never from a compaction summary or other
model-written text. The exception is the untrusted `agent_message_before_reply` block, the root turn's final answer recorded in the ledger, which pairs with the latest user message only when that message has origin `prompt`. In a subagent tree there is one intent source, the root session's ledger: every agent's request takes `task_intent` and `recent_user_messages` from the root's human messages since the last `ContextCleared` record (written by `/clear`, which empties the model context in the same session and refuses to run while a subagent does; the ledger keeps every other fact); spawn prompts and `send_message` bodies are untrusted-delimited context only. Resume restores both stores but starts in the user's resolved default mode and
drops session-scope grants; fork (`--fork-session`) copies both stores under a new session id, also
without session grants; a rewind of the conversation (rig-memory `as_of`, run by `/rewind`) puts the context back and
appends a `Rewound { turn, what, dropped_user_messages }` record to the ledger — denial and approval
history is never removed. `LedgerView` trims `user_messages` from the tail by `dropped_user_messages`
(the raw count, since steering, subagent questions and schedules also append `UserMessage`), so a
retracted prompt stops authorizing actions while the file stays append-only; a code-only rewind
trims nothing and `session::list` ignores the record. File pre-images for `/rewind` live beside
the stores in `checkpoints/` (`manifest.jsonl` and content-addressed `blobs/`), written by the
`write` and `edit` wrapper after the gate allows the call; a `/clear` writes an epoch entry there
and the picker lists only the current epoch's prompts. Both stores persist as JSONL
under `<data dir>/sessions/<session-id>/` (owner-only); the ledger is `ledger/ledger.jsonl` there,
outside the `logs/` and `files/` folders rig-memory's store reads (a top-level `*.jsonl` would make
it refuse the dir). Beside them sit the root agent's task list (`todos.json`) and the session's
scheduled prompts (`schedule.json`, written atomically and owner-only on every change). A due
scheduled prompt runs as a harness-started turn, never as a `UserMessage`; the ledger records
`Scheduled { created | fired | deleted | expired }` for the audit trail, and `LedgerView` ignores
it, so the guardian's intent is untouched. `/clear` also clears the schedule, since it drops the
intent a model-created job was scheduled under; resume drops expired and wakeup jobs and re-arms
the rest from now. Ledger in Phase 2; guardian
reads it in Phase 4; resume/fork in Phase 5.

Session names and listing. `SessionNamed { name }` is appended by `--name` and `/rename`; the
latest wins, and `LedgerView` ignores it like `Scheduled`, so the guardian's intent is untouched.
A name is 1-80 characters with no control characters (`session::validate_name`). `session::list`
scans each ledger once (tolerating a torn or foreign one by skipping it) for the primary root, the
last name, the first message since the last `ContextCleared` and, for a branch, the parent in the
last `Resumed` whose `how` starts with `fork`; the id is the directory name, because a fork copies
the source's records. `latest_for_root` is the first of `list(.., Some(root))`. `session::resolve`
(for `--resume` and `/resume <text>`) tests `text` as a session id before any path join, lets an
existing session directory win, and otherwise matches the name case-insensitively among sessions
started in the root: none is `Missing`, several `Ambiguous` (exit 2, ids listed). Every fork writes
the copy's own `SessionNamed` (the given name, else the source's plus ` (branch)`), and the TUI's
`/branch` also writes the copy's `Resumed` (`fork (branch)`), so nothing inherited is read as the
copy's.

Switching sessions in the TUI. `fontaine_tui::drive` returns `DriveOutcome::Exit` or
`Switch(SessionTarget { source, record_resume })`. `/resume` validates the target and returns it;
`/branch` forks first (refused while a subagent runs or has a report, like `/clear`) and returns the
copy with `record_resume` false. The run loop in `fontaine::run` then tears the old session down and
opens the target with a derived `Cli` (`resume` set; `continue`, `fork_session`, `session_id` and
`name` cleared), so a stale `--session-id` or `--continue` is never re-applied. The telemetry guard,
the configuration and the trust decision are kept; each iteration enters a new `fontaine.session`
span with `start_type` `resume`, resolves models again and restarts MCP servers and the scanner.

Quitting the TUI for good (`/exit`, `/quit`, empty-prompt Ctrl-D, or the input closing) is not a
switch. The quit latches in the interface, cancels the session's work through the runtime's final
shutdown (nested agent tasks aborted rather than killed gracefully, jobs killed, MCP reconnects
retired) and shares one two-second budget among the root turn's recovery (`Unsettled::SessionEnded`,
folding what the partial turn had committed), the `session_end` hook, process cleanup, the
terminal restore and the telemetry flush. A child that never reached a hook boundary leaves only
what it journaled to its own log; no report is made up. If the budget runs out the watchdog
ends the process: the root turn's run record was written before the run started, so the next
`--resume` folds the turn from the journal, as after any forced exit. `DriveOutcome::Switch` keeps
the graceful teardown, including `KILL_SETTLE`.

`/export` renders the rig-memory conversation (not the ledger) as Markdown; see `export.rs`.
