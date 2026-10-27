# Instruction files

Fontaine reads instruction files and puts them in the agent's system prompt. They come in three
names, so a repository that is already set up for Claude Code or for other agents works without
changes:

| Name | Native to | Notes |
|---|---|---|
| `FONTAINE.md` | Fontaine | Read last at its location. |
| `CLAUDE.md` | Claude Code | Read first at its location. |
| `AGENTS.md` | Other agents | Read only when no `CLAUDE.md` or `FONTAINE.md` exists (see [AGENTS.md mode](#agentsmd-mode)). |

The semantics follow Anthropic's documentation of Claude Code's memory files
(https://code.claude.com/docs/en/memory); deviations are listed under
[Differences from Claude Code](#differences-from-claude-code).

## Where files are read from

**Your own files**, loaded in every session, in this order:

1. `~/.claude/CLAUDE.md`
2. `~/.claude/rules/**/*.md`
3. `<config dir>/FONTAINE.md`
4. `<config dir>/rules/**/*.md`

**The directory walk** runs over each directory from the filesystem root down to the primary
workspace root. In each directory the order is:

1. `CLAUDE.md`, `.claude/CLAUDE.md`, `FONTAINE.md`, `.fontaine/FONTAINE.md`
2. `.claude/rules/**/*.md`, then `.fontaine/rules/**/*.md`
3. `CLAUDE.local.md`, then `FONTAINE.local.md`
4. In [AGENTS.md mode](#agentsmd-mode) only: `AGENTS.md`, then `.claude/AGENTS.md`

Within one location Claude Code's file comes first and Fontaine's second. Rule files in one
directory are ordered by their path relative to it, so the prompt is the same from one run to the
next. Roots added with `--add-dir` or `/add-dir` contribute nothing.

Files in directories below the primary root are not read at launch. They are read on demand, as
described in [Nested files and path rules](#nested-files-and-path-rules).

### AGENTS.md mode

`AGENTS.md` is read only when none of `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`,
`FONTAINE.md`, `.fontaine/FONTAINE.md` and `FONTAINE.local.md` exists in the primary root or any
directory above it. Your own files and rules do not count. A `CLAUDE.md` that says `@AGENTS.md`
loads `AGENTS.md` through the import, exactly once.

## Imports

A line in an instruction file can import another file with `@path`. An import is an `@` at the
start of a line or after whitespace, followed by a path that runs to the next whitespace; write
`\ ` for a space inside the path. Paths are relative to the importing file, absolute, or start with
`~/`. Imports inside code spans, fenced code blocks and quotes are ignored, and so is `@` inside a
word, so `user@example.com` is not an import.

The imported file is loaded right after the file that imported it, and the `@path` text stays in
the importer. Imports nest at most four levels deep, and a cycle loads each file once.

An import of a file that does not exist is skipped silently, since `@mentions` in prose are common.
An import that exists but is refused is skipped with a warning naming both files.

## What is refused

- **Files that a repository controls** (anything inside the workspace roots or the enclosing git
  work tree) must resolve, after following symlinks, to a path inside the workspace roots or that
  work tree. A checked-in `CLAUDE.md` that links to `~/.ssh/id_rsa` is skipped with a warning.
  Their imports are held to the same rule.
- **Files above the work tree**, such as `~/CLAUDE.md`, load as found, but their imports are held to
  that rule too, because another local user may be able to write there.
- **Your own files** and their imports are not confined.
- **Deny rules.** A file that a `read` [deny rule](permissions.md) covers is kept out of the prompt,
  wherever it comes from, your own files included.

## Duplicates

A file whose canonical path, or whose text (after comments and frontmatter are removed), was already
loaded is skipped silently. The first occurrence in load order wins. So a symlink from `CLAUDE.md`
to `AGENTS.md`, identical `CLAUDE.md` and `FONTAINE.md` files, and `~/.claude/CLAUDE.md` met again
when the walk passes through your home directory each load once. A file with no text left is
dropped.

## Comments

Block-level HTML comments (`<!-- ... -->` that start a line) are removed before the text reaches the
model. Comments inside fenced code blocks are kept.

## Rules and `paths`

A rule is a markdown file under a `rules/` directory. Its frontmatter is removed; only `paths` is
read, as a YAML list or a comma-separated string:

```markdown
---
paths:
  - "src/**/*.rs"
  - "tests/*.rs"
---
Use `expect` with a reason instead of `unwrap`.
```

A rule without `paths`, or whose frontmatter does not parse, applies always. A rule with `paths`
applies once a file that matches is read or written. Patterns are globs relative to the directory
that holds the rule's `.claude` or `.fontaine` directory (for your own rules, the primary root). `*`
does not cross a directory separator, `**` does, and `{a,b}` alternatives work, so `*.md` matches
only at the base. An invalid pattern is dropped with a warning and the rule's other patterns keep
working.

## Nested files and path rules

After a successful `read`, `write` or `edit` of a file under the primary root, Fontaine looks at each
directory below the root down to the file's directory. It loads those directories' instruction
files and unconditional rules, and the path-scoped rules that match the file, and appends them to
the tool result:

```text
[Harness note: instruction files that apply to <path>, loaded from disk.]

Instructions from <path>:

<text>
```

Each agent receives a file once. `/clear` and compaction make the main agent receive them again;
a model switch, an output-style change or `/add-dir` do not, because the conversation and its
earlier tool results survive them. `AGENTS.md` files are read this way only in AGENTS.md mode and
only in a directory with none of the `CLAUDE`/`FONTAINE` names. Instruction text that arrives in a
tool result goes through the tool-result scanner like any other result. A `cat` in a shell does
not count as a read.

## What the guardian reads

The guardian's `project_instructions` is the launch set as it stood when the session started, with
imports. It is ordered most specific first: the primary root's level, then each directory above it,
then your own files, and cut at 16,000 characters, so the cut drops the broadest files. The guardian
never sees files loaded on demand, and edits made during the session do not reach it. See
[The guardian](guardian.md#what-the-guardian-sees).

## Size

There is no cap on the prompt. When the launch files add up to more than 40,000 characters, the
session warns once and names the largest files. `/context all` shows what each file costs.

## `/memory`

`/memory` lists the instruction files: every launch file with where it came from (and the file that
imported it), the files loaded on demand so far, and your `FONTAINE.md` and the primary root's, marked
`not created` when they do not exist. After them come the [auto-memory](memory.md) files for the
project, `MEMORY.md` first, each marked `saved note` with when it was modified, and the first line
says whether auto memory is on and where its directory is. `/memory <n>` opens entry `n` in your editor, creating the file
(and its directory) first when it does not exist.

The editor is `$VISUAL`, then `$EDITOR`, else `vi` (`notepad` on Windows). It runs as
`sh -c '<editor> "$1"'`, so `code --wait` works; on Windows the same form runs through Git Bash, and
without Git Bash the editor string is split on whitespace. While it runs, the terminal is yours: the
interface steps aside and is redrawn when the editor exits. The editor is your own program started by
your own command, so it is not gated, like `!` commands.

Edits apply to the agent at the next model switch, output-style change, `/add-dir` or new session.
The guardian keeps the files it read at the start.

## `/init`

`/init` asks the agent to read the codebase and write the project instruction file: the build, test
and lint commands, the layout, and the conventions a newcomer would not discover. The file is the
first that exists among `FONTAINE.md`, `CLAUDE.md`, `.claude/CLAUDE.md` and `AGENTS.md` in the
primary root, which the agent improves and never overwrites; with none, a new `FONTAINE.md`. The
write goes through the normal permission pipeline, and the session records `/init: create or improve
<file>` as your request, so the guardian sees it as intent. `/init` takes no argument.

## Differences from Claude Code

- A repository file that resolves outside the workspace and its work tree is refused; Claude Code
  follows symlinks.
- An import that leaves the workspace and work tree is skipped with a warning; Claude Code asks for
  one-time approval.
- Imports from files above the work tree are confined; Claude Code allows them.
- `read` deny rules keep files out of the prompt, your own included.
- `FONTAINE*` names count toward AGENTS.md mode like their `CLAUDE*` twins.
- Shell reads do not load nested files.
- `.claude` is a [protected directory](permissions.md), because Fontaine reads it into prompts.
- `/memory` is a numbered list, not a picker.
