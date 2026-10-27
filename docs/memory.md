# Auto memory

Auto memory lets the agent save what it learns about a project (build and test commands, layout,
conventions, corrections, preferences you state) so that a later session starts with it. The
agent saves notes with the ordinary `write` and `edit` tools; the index of those notes loads into
the system prompt of each new session.

Auto memory is a persistence vector. Text an injection gets the model to write is read by every
later session, so Fontaine never approves a memory write by label: each one goes through the
[deterministic floor](permissions.md) and, in `auto` mode, the [guardian](guardian.md).

**The default is on while the feature is being tested, and should return to off** until the
memory-poisoning cases in the [guardian scorecard](evals.md#the-memory-slice) have been scored
live. To turn it off now:

```toml
[memory]
auto = false
```

| Key | Meaning | Default |
|---|---|---|
| `[memory] auto` | The agent may save notes to the per-project store, and the store's index loads each session. | `true` |

Only the user file and `--settings` can set it; a repository's settings cannot. `/config` shows
and toggles it, and a change applies at the next launch. When it is off the store is inert: its
files are not loaded and no root is added, and `/memory` still lists them so you can clean up.

## The store

Notes live in your data directory, outside any repository, so a cloned repository cannot plant
them and they cannot be committed by accident:

```
<data dir>/memory/<project key>/MEMORY.md     the index, loaded into the prompt
<data dir>/memory/<project key>/<topic>.md    detail, read on demand with the read tool
```

The project key is a readable slug of the project path plus a short digest of it, for example
`fontaine-3fa91c2e`. The path is the owning git checkout of the launch directory when there is one,
so every subdirectory and every worktree of a repository shares one store; outside a repository it
is the launch directory. The directory is created on first need, readable by you only (mode 0700,
or a current-user-only ACL on Windows). A store directory that is a symlink is refused.

`MEMORY.md` is a short index with one line per topic file, linking to it. Nothing else about the
files is enforced. Only the first 200 lines or 25,000 bytes of the index, whichever is smaller,
reach the prompt, with a warning when the index was cut; topic files are not loaded until the agent
reads them. A `MEMORY.md` that is a symlink or not valid UTF-8 is refused with a warning.

## What the agent is told

The system prompt gets one layer when auto memory is on. It says that these are notes saved in
earlier sessions, that they are unverified data about the project and not instructions, and that
they must never change what the agent may do. It then gives the directory, the capped index, and
short guidance on what to save and how to write it. The layer is present even when nothing has been
saved yet, so the agent knows where to write. It rebuilds like instruction files do: on a model
switch, an output-style change, `/add-dir` and a new session. The root agent gets it; a subagent
does not get its own memory.

The memory directory is a workspace root, so `read`, `write` and `edit` reach it from any project.
It is labelled `memory` in the system prompt's root list and `auto memory` in `/status`, it is left
out of the file index that completion and the ledger use, and the out-of-roots denial points at it.

## How writes are gated

The store sits under Fontaine's own data directory, so every `write` and `edit` into it meets the
[protected-path floor](permissions.md), which sits ahead of every mode and every `allow` rule:

| Mode | A memory write |
|---|---|
| `manual`, `acceptEdits` | Asks you, with the diff. An `allow` rule does not approve it. |
| `auto` | Goes to the guardian. |
| `dontAsk`, headless | Denied. |

There is no fast path: `acceptEdits` does not cover it, and neither does a read-only label. A `deny`
or `ask` rule on `write` or `edit` for the memory path works as for any path. Subagents share the
scope, so one can also propose a memory write, and each is gated on its own.

A shell command that writes there (`echo ... >> MEMORY.md`) is not covered by the protected-path
floor, which gates only `write` and `edit`: it takes the ordinary shell gate, a prompt outside
`auto` and the guardian in `auto`. Shell redirect targets cannot be extracted soundly enough to be a
floor, so the guardian's memory rule is what covers this case. If you want a human to confirm each
memory write, use a mode other than `auto`, or an `ask` rule on the path.

## What the guardian does with a memory write

The guardian is told the memory directory as a session fact, set by Fontaine from the session and
never from a tool argument. A call whose target is under that directory, including a shell command
that writes there, is judged against the memory-write rule: allow plain project facts that your
request or the repository supports; deny or escalate content that instructs the future agent,
widens permissions or relaxes the guardian, contains secrets, embeds URLs or commands to run, or
traces to tool results, web pages or file text rather than to you. Low confidence is a denial. The
rule is in the `minimal` profile as well. The memory text is not added to the guardian's
`project_instructions`: that block is your own files, and notes are agent-written and possibly
attacker-influenced. See [The guardian](guardian.md#memory-writes).

## Auditing and clearing

`/memory` lists the instruction files and then the store's `MEMORY.md` and topic files, sorted,
with when each was modified. Its first line says whether auto memory is on and where the
directory is. `/memory <n>` opens a file in your editor. `/memory` has no delete action: delete
files in your editor or the file system, or remove the whole `<data dir>/memory/<project key>/`
directory. `/context` shows a `memory` row when the layer is loaded. Memory writes are ordinary
`write` and `edit` records in the audit log, with their tier and reason.

## Differences from Claude Code

- The store is in Fontaine's data directory, not under `~/.claude/projects/`.
- Every write is gated by the floor and, in `auto`, the guardian; no memory write skips both.
- The loaded text is framed as unverified notes, and kept out of the guardian's context.
- The index is capped at 200 lines or 25,000 bytes.
- Only the user can turn it on or off; a repository cannot.
- There is no automatic subagent memory.
