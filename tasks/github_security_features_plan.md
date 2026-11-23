# Enable GitHub Secret Scanning, Push Protection and Dependabot Security Updates Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/759

## Overview
`gh api repos/madesroches/fontaine --jq .security_and_analysis` reports secret scanning, secret
scanning non-provider patterns, push protection and Dependabot security updates all `disabled`,
although the repo is public (the features are free) and holds release-signing secrets in
environments. Enable the four, and record the baseline in `SECURITY.md` so it is documented and
checkable.

## Current State
- The settings are repo-level GitHub configuration, not repo files; nothing in `.github/` controls
  them (`.github/` holds only `scripts/` and `workflows/`; no `dependabot.yml`).
- Dependabot alerts are already on; no fix PRs are opened.
- `SECURITY.md` covers the reporting policy and scope and does not mention these controls.

## Design
Apply one call, by a repo admin (the settings change affects the live repo and is not part of the
PR diff):

```
gh api -X PATCH repos/madesroches/fontaine --input - <<'EOF'
{"security_and_analysis":{
  "secret_scanning":{"status":"enabled"},
  "secret_scanning_push_protection":{"status":"enabled"},
  "secret_scanning_non_provider_patterns":{"status":"enabled"},
  "dependabot_security_updates":{"status":"enabled"}}}
EOF
```

`secret_scanning_validity_checks` is out of scope (not requested by the issue).
Dependabot security updates open fix PRs only for vulnerable dependencies; they need no
`dependabot.yml` and do not enable routine version updates.

## Implementation Steps
1. Run the call above as a maintainer with admin rights on the repo (see Manual Verification).
2. Add a short "Repository protections" paragraph to `SECURITY.md` naming the four controls.
3. Add a `CHANGELOG.md` entry under `[Unreleased]` in the "Build and release" subject, at the end of that subject's section.

## Files to Modify
- `SECURITY.md`
- `CHANGELOG.md`

## Trade-offs
- Committing a `.github/settings.yml` (a settings-as-code app) was rejected: it needs a third-party
  app with admin rights for four toggles.
- A `dependabot.yml` for version updates was rejected as out of scope; it would add PR churn.

## Decisions
- AFK: the settings change is applied by the maintainer, not by the automated chain, because it
  mutates live repository configuration outside the PR's review gate.

## Documentation
`SECURITY.md` only; `docs/` covers product behavior, not repo settings.

## Testing Strategy
No code changes, so no automated tests. The standard `cargo` checks are unaffected.

## Manual Verification
1. `gh api repos/madesroches/fontaine --jq .security_and_analysis` shows `enabled` for the four
   features. This cannot be automated in CI: reading it needs admin-scoped credentials.
2. Push protection: attempt to push a branch with a GitHub-format dummy token on a scratch branch
   and confirm the push is blocked; delete the branch afterward.

## Open Questions
None.
