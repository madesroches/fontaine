# Session Ledger Design

Part of the Fontaine design; the master plan `tasks/fontaine_harness_plan.md` holds the Phase Tracker, workspace layout and `## Decisions`.

## Session ledger

Two stores per session. rig-memory holds the model context, which compaction may rewrite. The
**session ledger** (`fontaine-core::ledger`) is Fontaine-owned and append-only: user messages
verbatim, mode changes, approvals and session grants, denial records, actions taken, the workspace
roots, trust state, and the git remotes and pre-session file index captured at start (the index
respects `.gitignore` and is capped at 50,000 paths — overflow is recorded as truncated; paths
the index does not hold are reported to the guardian as `unindexed`). The guardian's
`AdjudicationRequest` is built only from the ledger — never from a compaction summary or other
model-written text. In a subagent tree there is one intent source, the root session's ledger: every agent's request takes `task_intent` and `recent_user_messages` from the root's human messages; spawn prompts and `send_message` bodies are untrusted-delimited context only. Resume restores both stores but starts in the user's resolved default mode and
drops session-scope grants; fork (`--fork-session`) copies both stores under a new session id, also
without session grants; a rewind of the conversation (rig-memory `as_of`) truncates the context and
appends to the ledger — denial and approval history is never removed. Both stores persist as JSONL
under `<data dir>/sessions/<session-id>/` (owner-only). Ledger in Phase 2; guardian
reads it in Phase 4; resume/fork in Phase 5.
