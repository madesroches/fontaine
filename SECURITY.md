# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub private vulnerability reporting: open the
repository's **Security** tab and choose **Report a vulnerability**. Please do not open a public
issue or pull request for a vulnerability.

This covers guardian bypasses (a harmful tool call that is allowed), ways past the deterministic
permission floor, and ways for project files to loosen a permission decision. The interactive
`/guardian-disable` command is a deliberate, session-scoped development escape hatch that asks
for confirmation in the host terminal, and is written to the audit log; a device attached with
`fontaine attach` can never run it or answer its confirmation. Reports of a way to reach it without the user
interactively asking (for example from a project file, a subagent, a headless run or a remote
device) are still in scope. A `/remote` ticket is a bearer secret the user mints on purpose that only
lets a device ask to pair; the user confirms the pairing in the host terminal. Reports of a way to
attach or pair a device without a valid ticket and that confirmation (a forged, guessed or replayed
ticket, a device confirming its own pairing, a revoked device still attached), to enable remote
control from a project file, a subagent, the model or a headless run, to put a device line into the
user's draft, to have a device answer a local-only dialog or choose "always allow", or to run a
command outside the device allow-list are in scope.

Include what you did, what you expected and what happened, and the version or commit you tested.

## Repository protections

The repository has GitHub secret scanning, push protection and Dependabot security updates
enabled, alongside Dependabot alerts.

## Disclosure

We follow a 90-day coordinated disclosure window. We will acknowledge a report, work on a fix with
you, and publish details once a fix has shipped or the 90 days have passed, whichever comes first.
Dataset pull requests that reproduce a bypass are welcome only after the fix has shipped.
