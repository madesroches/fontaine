# Checkpoints and /rewind

Fontaine saves a file's contents before the `write`, `edit` and `notebook_edit` tools change it, filed under the
prompt you were answering. `/rewind` (or Esc Esc at an idle, empty prompt) takes the code, the
conversation, or both back to the start of any earlier prompt of this conversation.

## What is tracked

- **Tracked:** every file `write`, `edit` and `notebook_edit` change, in any workspace root, including a file they
  create. A foreground subagent's edits belong to the prompt that started it; a background
  subagent's late edits belong to whatever prompt is current when they land.
- **Not tracked:** changes made through the `shell` tool, your own `!` commands, MCP tools, and any
  other process. A rewind leaves them as they are, and says so before it restores anything.
- **Not tracked, by design:** the [auto memory](memory.md) directory. It holds notes that outlive
  the session, so a code rewind never reverts or deletes them.
- **Over a cap:** a file larger than `max_file_bytes` is not saved, and once the session's saved
  copies reach `max_session_bytes` later files are not either. The edit still goes ahead. You get
  one warning per session for each reason, and the rewind lists such files as not restorable.

Saving is a recovery aid, not a permission decision: it runs only after the call was allowed and
never blocks or denies an edit.

A prompt counts when you submit it: a typed message, a slash command or skill that sends a prompt,
`/init`, `/plan <task>`, `/goal <condition>`, the first run of a `/loop` line, and a message queued
during a turn once it is sent as the next turn. Turns Fontaine starts itself (later `/loop`
iterations, scheduled tasks, goal continuation turns) and messages that steer a running turn do not
make a new one; their edits belong to the prompt before them. In a headless `fontaine -p` run each
message is a prompt too, so a later `--resume` can rewind it.

## Opening the picker

`/rewind` (alias `/checkpoint`) and Esc pressed twice in quick succession open a list of the
prompts since the last `/clear`, newest at the bottom, each with the number of files that prompt
and the later ones changed. The second Esc only counts when the first did nothing else: Esc still
interrupts a running turn, ends a pending self-paced `/loop`, and closes the slash menu, the `@`
list and a history search first. `/rewind` takes no argument.

Pick a prompt with Up, Down and Enter, then choose:

- **Restore code and conversation:** files and conversation both go back to before that prompt.
- **Restore conversation:** the model's context goes back; files stay as they are.
- **Restore code:** files go back; the conversation stays.
- **Never mind.**

A choice that restores code first shows what it would do: the files it restores, the files it
deletes because that prompt's turn created them, files that are **not restorable** (not saved, or
outside the workspace, or a symbolic link), and files **changed since Fontaine last wrote them**.
Those last were edited by you or another program after the agent's last change; restoring them
discards that work, so you choose between overwriting them and keeping them. Esc steps back.

Restoring the conversation puts the rewound prompt back in the input box to edit and send again,
clears an active [goal](tui.md#goals) (as `/clear` does, whichever prompt you go back to), and keeps the
task list, scheduled prompts, permission grants and mode. The transcript on screen is emptied the
way `/clear` empties it. Summarizing from a chosen point is not offered.

A rewind is something you ask for in the terminal, outside the permission pipeline, and the model
cannot request one. It is refused while a subagent runs or has a report to deliver.

## What the ledger keeps

A rewind never removes anything from the session ledger. It appends a record of the rewind, so
approvals, grants and the guardian's denials, and `/denied`, are complete. The one thing it changes
is the [guardian's view of what you asked](guardian.md): the messages the rewind retracted stop
counting as your intent, so a prompt you took back cannot keep authorizing calls. Rewinding code
only trims nothing.

The model's earlier conversation is kept in the session log, so rewinding to an earlier prompt
still works after a compaction and restores the messages from before it.

## Where it is stored

Under the session directory, `checkpoints/`, readable by you alone:

```
checkpoints/manifest.jsonl   one record per prompt, per saved file and per rewind
checkpoints/blobs/<hash>     the saved contents, named by their SHA-256 and shared between files
```

A fork (`/branch`, `--fork-session`) copies the directory with the rest of the session, so the copy
can rewind to the same prompts; `--resume` reopens it. After `/clear` earlier prompts can no longer
be rewound to.

Before anything is written back, each path is checked again: it must be inside the workspace roots
now, and neither the file nor any directory above it may be a symbolic link. Saved contents are
checked against their hash. Anything that fails is reported and left alone.

## Settings

The `[checkpoints]` keys are in [Configuration](configuration.md#checkpoints). Only the user file
sets them.
