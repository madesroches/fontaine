# MCP servers

Fontaine can use tools from [Model Context Protocol](https://modelcontextprotocol.io) servers. A
server runs as a child process (stdio) or is reached over streamable HTTP, and each tool it lists
becomes a tool the model can call. MCP tools go through the same permission pipeline as every other
tool; nothing a server says about itself changes how they are judged.

In the terminal UI, `/mcp` lists the servers and whether each is connected, pending (being
reconnected), failed, left off or not loaded, and `/mcp reconnect` restarts them; see
[Reconnecting](#reconnecting).

## Declaring servers

Servers are declared under `[mcp.servers.<name>]`. Set exactly one of `command` (stdio) or `url`
(streamable HTTP).

```toml
[mcp.servers.github]
command = "github-mcp-server"
args = ["stdio"]
env_vars = ["GITHUB_TOKEN"]

[mcp.servers.docs]
url = "https://mcp.example.com/docs"
```

| Key | Meaning |
|---|---|
| `command` | The program of a stdio server. It starts in the primary workspace root. |
| `args` | Its arguments. Stdio only. |
| `env_vars` | The names of environment variables to pass through. Stdio only. |
| `env` | Literal variables set on the server, after the pass-through and [`[env]`](#declaring-servers) layers, replacing a variable of the same name. Stdio only. |
| `url` | The endpoint of a streamable HTTP server. It is reached through the proxy, extra CA roots and client certificate of [`[network]`](configuration.md#network). |

A server name contains letters, digits, `_` and `-`, and has no `__` and no leading or trailing `_`.

**Environment.** A stdio server does not inherit Fontaine's environment. It gets a short base set
(`PATH`, `HOME`, `USER`, `LANG`, the temporary-directory variables and, on Windows, the system
variables it needs to find programs) and the variables named in `env_vars`. Values never appear in
configuration, and the API keys of your model accounts and the telemetry credentials are not
passed unless you name them. Every entry of [`[env]`](configuration.md#env) is set on the server as
well, replacing a copied variable of the same name, and a server's own `env` map lands last,
replacing a variable the other layers set. `env` values are literal: they are not expanded and
cannot reach into the process environment. A [plugin's](plugins.md) `.mcp.json` `env` values, in
contrast, are expanded at load, and the variables a plugin references are named in the install
summary. The server's standard error is not shown in the terminal UI; it goes to
the debug file (`--debug-file`).

**Where they are declared.** The user file, `--settings` and an untracked `settings.local.toml` of
a trusted root declare servers that start at once. A server declared in a repository's shared
`.fontaine/settings.toml` is [asked about](#servers-a-repository-defines) first. Extra workspace
roots (`--add-dir`) never contribute servers. A [plugin's](plugins.md) `.mcp.json` servers start
without the workspace-trust gate: install approved them, in the terminal UI and headless runs
alike.

## Tool names

A tool `create_issue` of the server `github` is registered as `mcp__github__create_issue`, the
name [permission rules](permissions.md#rules) use. A character in a tool name that the
rule grammar cannot name becomes `_`. When a server announces that its tool list changed, Fontaine
fetches the list again and registers the new tools the same way; tools that went away are removed.
Only tools are used; resources and prompts are not. A server may also ask you for input while a
tool runs, as [described below](#requests-for-input).

A server that cannot be started, or that never answers the handshake, is reported (a notice in the
terminal UI, a line on stderr in a headless run) and left out; the session goes on without it.
Starting a remote server is retried first when the failure looks temporary: a refused or reset
connection, a timeout, a 5xx or a 429. Fontaine tries up to three more times, after 1, 2 and 4
seconds, and all the attempts of one server share a 30 second deadline, so a refused server delays
startup by about 7 seconds and an unresponsive one by at most 30. Authentication, not-found and
protocol errors, and a stdio server that fails, are not retried. The servers start at once, so a
slow one does not delay the others. A server that failed to start can still be restarted by hand
with [`/mcp reconnect`](#reconnecting).

## The trust model

- **Every MCP tool is `arbitrary`.** It is registered behind the same gate as the built-in tools
  and, like `shell` and `webfetch`, never earns the read-only fast path. In `manual` it asks, in
  `auto` the [guardian](guardian.md) judges it, in `dontAsk` and headless runs it is denied unless
  an allow rule covers it. The one exception is a tool its server marks as
  [requiring your approval](#requiring-approval-for-a-tool): it always needs a person's approval
  and no allow rule pre-approves it, so it is denied in `dontAsk` and headless runs.
- **What a server declares is only text, with one exception.** A tool can claim to be read-only,
  idempotent or non-destructive. Those hints are passed to the guardian as untrusted text inside the `tool_call`
  block of its request, labelled as the server's own claim, and never as a fact or a gate:

  ```
  server_declared_annotations (the server's own claim, not a fact): {"title":"Lookup","readOnlyHint":true}
  ```

  A `readOnlyHint` never skips the guardian. The exception is the `requiresUserInteraction`
  marker below, the only declared value that changes a decision, and only toward asking.
- **Rules are the way to pre-approve.** `mcp__github`, `mcp__github__create_issue` and
  `mcp__github__*` in `allow`, `ask` or `deny` work like any other rule. Allow rules come from
  user-trust scopes only, so a repository can never allow its own server's tools. No rule
  pre-approves a tool its server marked as requiring your approval.
- **Credentials stay out.** Nothing is sent to a streamable HTTP server but the protocol's own
  requests; Fontaine adds no credentials of its own.

### Requiring approval for a tool

A server can mark a tool as needing a person's approval on every call, as Claude Code allows, by
setting `_meta["anthropic/requiresUserInteraction"]` to the JSON Boolean `true` in that tool's
`tools/list` entry:

```json
{
  "name": "grant_access",
  "description": "Grant a user access to a project",
  "inputSchema": {"type": "object"},
  "_meta": {"anthropic/requiresUserInteraction": true}
}
```

Fontaine treats the marker as an implicit `ask` rule on that one tool, at the floor of the
[permission pipeline](permissions.md#the-pipeline). The tool asks wherever a person can answer, `auto` included,
and the prompt offers no "allow for this session" or "always allow". An allow rule or a hook's
allow does not skip it, and no guardian verdict approves it. In `dontAsk` and in headless runs the
call is denied. The audit record carries `tier = ask_rule` and
`rule = anthropic/requiresUserInteraction`; it never names the server or the tool. Any other value
(`"true"`, `1`, `false`) is ignored. A `deny` rule still wins.

Once a server has marked a tool, the marker holds until that server's connection ends (the next
session), even if a later tool list drops it. A marker added by a later tool list takes effect
when Fontaine refreshes the list.

## Servers a repository defines

A server in a repository's `.fontaine/settings.toml` is an executable the repository chose. In the
terminal UI:

1. Before anything it defines can start, an untrusted workspace root gets the [trust
   prompt](tui.md#workspace-trust). If you decline, the session runs without the repository's
   servers and hooks, and the question comes back next session.
2. In a trusted root each repository-defined server is then approved on its own, the first time it
   would start: the prompt shows the command line or URL and, for a command, the names of the environment variables it asks to receive (never their values). Your approval is recorded in
   `<data dir>/trust.toml` for that root, that name and that exact definition, so a server whose
   command, arguments, environment variable names or URL change is asked about again, and a repository cannot approve its own
   servers.

A headless run (`fontaine -p`) starts them without asking, as Claude Code's `-p` does. In both
surfaces a project server's tools are ordinary `arbitrary` tools, except that a tool can mark itself
as [requiring your approval](#requiring-approval-for-a-tool): a repository can add a server, but it
cannot make a call of that server run without the same approval any call needs, and it can only
make that approval stricter.

```toml trust.toml
[[trusted]]
root = '/home/me/code/myrepo'

[[approved_servers]]
root = '/home/me/code/myrepo'
name = "github"
digest = "9f2c1c8e1a7d4b1f"
```

## Requests for input

A server can ask you for input while a tool runs (MCP elicitation). Fontaine supports the form mode:
the terminal UI shows a dialog with the server's message and one row per field the server asked for
(text, number, whole number, yes/no, a choice, or several choices), checks what you type against
what the server required, and sends your answer back to that server. You can fill the form in,
decline it (the server carries on without the input) or cancel it (the server stops what it was
doing). The keys are in [the terminal UI](tui.md#requests-for-input-from-an-mcp-server). A server's
request to open a link (URL mode) is always declined: Fontaine never opens a browser on a server's
say-so.

- **Only the terminal UI shows a dialog.** A [headless run](headless.md#questions), [AFK mode](afk.md),
  the [doctor](doctor.md) probe and a call made by a subagent answer `cancel` without showing
  anything. The dialog closes, and is answered `cancel`, when the call that caused it ends.
- **Your answer goes to the server and nowhere else.** It never reaches the model, the transcript,
  the audit log or telemetry, and the dialog says so. Don't enter secrets you wouldn't give that
  server. The server's message is cleaned of control characters before it is shown.
- **It decides nothing.** Answering never changes a permission decision; the call that led to the
  request was already approved like any other.

Fontaine offers the MCP protocol version 2025-11-25, which delivers a request for input as a
request to the client. A server that only speaks a newer version still connects, but its requests
for input are not served.

## Reconnecting

A remote (streamable HTTP) server that drops mid-session is reconnected. Fontaine notices when a
tool call fails because the transport is dead, or when the connection ends, closes it and retries up
to five times, waiting 1, 2, 4, 8 and 16 seconds before the attempts. Each attempt has a 30 second
limit. While it retries, `/mcp` shows the server as `pending (reconnecting, attempt N of 5)` and its
tools are withdrawn; a success registers them again and says so in the terminal UI. An
authentication or protocol error stops the retries at once, since waiting cannot fix it. After the
last attempt the server is marked failed:

```
MCP server `docs` disconnected · run /mcp reconnect docs
```

A stdio server that exits is marked failed straight away and its process is not started again, in a
headless run as well; there is no automatic retry for it.

`/mcp reconnect <name>` restarts one server, connected or not, at once and without waiting;
`/mcp reconnect all` restarts every failed server. Only servers Fontaine tried to start this
session can be restarted, including ones that failed at startup. A server you left off, or that
the workspace not being trusted keeps from loading, is never started this way, so the command cannot
get around workspace trust or your approval. It is not available while a turn runs.

## Large outputs

A successful tool result that is too large is saved to a file instead of entering the model's
context. The limit is 50,000 characters or `MAX_MCP_OUTPUT_TOKENS` tokens (default 25,000, counted
as the result's bytes divided by 4), whichever is hit first. The result is written in full to
`<scratch>/mcp-results/<tool>-<n>.txt`, in the session's scratch directory, and the model gets this
instead:

```
Output too large (60000 chars, ~15000 tokens), saved to <path>. Use read or grep on that file to see it.
```

The model reads the file with `read` or `grep` like any file in the workspace, and the content is
screened then like any other `read`. The file is removed with the session. A tool can raise the
character limit for its own results by setting `_meta["anthropic/maxResultSizeChars"]` to a positive
integer in its `tools/list` entry, up to 500,000; the token limit still applies. Anything else is
ignored.

If the session has no scratch directory, or the file cannot be written, the result keeps its first
and last halves of the limit with a `[… N chars cut …]` marker between them; Fontaine warns once
when a save fails. A result a server flags as an error (`isError`) longer than 11,000 characters keeps its
first and last 5,000 characters with the same marker. A result that carries an image is passed
through unchanged. `MAX_MCP_OUTPUT_TOKENS` is described under
[environment variables](configuration.md#command-line-and-environment).

## Telemetry

Starting a server emits an `mcp_server_connection` event, and so does each reconnect attempt, and
each call a `fontaine.mcp.call` span. Answering a request for input emits an `mcp_elicitation`
event with the action (`accept`, `decline` or `cancel`) and the server, never the message or the
answer.
Server and tool names appear in them, and in every other tool field, only when
`log_tool_details = true`; otherwise they read `third-party`. See [Telemetry](telemetry.md).
