# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub private vulnerability reporting: open the
repository's **Security** tab and choose **Report a vulnerability**. Please do not open a public
issue or pull request for a vulnerability.

This covers guardian bypasses (a harmful tool call that is allowed), ways past the deterministic
permission floor, and ways for project files to loosen a permission decision. The interactive
`/guardian-disable` command is a deliberate, session-scoped development escape hatch that asks
for confirmation and is written to the audit log; reports of a way to reach it without the user
interactively asking (for example from a project file, a subagent, or a headless run) are still
in scope.

Include what you did, what you expected and what happened, and the version or commit you tested.

## Disclosure

We follow a 90-day coordinated disclosure window. We will acknowledge a report, work on a fix with
you, and publish details once a fix has shipped or the 90 days have passed, whichever comes first.
Dataset pull requests that reproduce a bypass are welcome only after the fix has shipped.
