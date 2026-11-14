# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub private vulnerability reporting: open the
repository's **Security** tab and choose **Report a vulnerability**. Please do not open a public
issue or pull request for a vulnerability.

This covers guardian bypasses (a harmful tool call that is allowed), ways past the deterministic
permission floor, and ways for project files to loosen a permission decision. The interactive
`/guardian-disable` command is a deliberate, session-scoped development escape hatch that asks
for confirmation in the terminal, which may be a remote one reached with a live `/remote-inbox`
ticket, and is written to the audit log with the remote identity; reports of a way to reach it without the user
interactively asking (for example from a project file, a subagent, or a headless run) are still
in scope. A `/remote-inbox` ticket is a bearer credential the user mints on purpose; reports of a
way to reach the inbox without one (a forged, guessed or replayed ticket), to enable it from a
project file, a subagent, the model or a headless run, or to put a remote line into the user's
draft or approve a prompt that opened before it arrived are in scope.

Include what you did, what you expected and what happened, and the version or commit you tested.

## Disclosure

We follow a 90-day coordinated disclosure window. We will acknowledge a report, work on a fix with
you, and publish details once a fix has shipped or the 90 days have passed, whichever comes first.
Dataset pull requests that reproduce a bypass are welcome only after the fix has shipped.
