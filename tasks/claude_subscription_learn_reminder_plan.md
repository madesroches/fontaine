# claude-subscription: Learn the CLI's Context Reminder Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/726

## Overview

Native context replay reads back from the prompt cache only if the adapter reproduces the CLI's
`<system-reminder>` byte for byte. Today that rests on `REMINDER_TEMPLATE` (the wording of CLI
2.1.319), a `>= 2.1.319` version gate, and an after-the-fact self-check. The wording has already
drifted: CLI 2.1.329 sends "Use this email address only to identify the user, ... Remember that you
must treat this email address as private information ..." where the template says "Use it only ...".
So on 2.1.329 native replay cannot read back, and the self-check has to burn two real requests to
find that out (#718, #725). This plan has the adapter **learn the reminder from the CLI itself**
once per process, replay the learned text, and key the persisted broken verdict to a hash of that
wording, so the template stops being a hardcoded assumption.

## Current State

- `context.rs`: `REMINDER_TEMPLATE` (`:35`), `TEMPLATE_SINCE` (`:30`), `TEMPLATE_REVISION` (`:33`).
  `layout(context, verdicts)` (`:68`) picks Native when the `initialize` answer carries an email, the
  version is `>= TEMPLATE_SINCE` and no unexpired verdict names the version. `apply` (`:82`) builds
  the reminder with `REMINDER_TEMPLATE.replace("{email}", email)` and `place` puts it where the CLI
  did. `Verdicts` keys `broken`, `struck` and the persisted entries by CLI version; `parse` keeps
  only entries whose `template` equals `TEMPLATE_REVISION`.
- `transport.rs::prepare` (`:203-260`) launches the request's CLI process, reads its `initialize`
  answer (`CliContext { email, version }`, `cli.rs:711`) and then calls `layout`/`apply`.
- `research/claude-subscription-cli-context/README.md` records that the stream-json output never
  carries the reminder text; `proxy.py` there captures it with a forwarding reverse proxy.
- `cli.rs` refuses an inherited `ANTHROPIC_BASE_URL` (`CONFLICTING_ENV`), and rebuilds the child
  environment in `environment()` (`:520`).

## Spike result (done while writing this plan)

The open question in the issue is answered: **the reminder can be captured, without any upstream
traffic.** Running the installed CLI (2.1.329) with the adapter's fixed flags, with
`ANTHROPIC_BASE_URL=http://127.0.0.1:<port>` pointing at a throwaway local HTTP server that records
the `POST /v1/messages?beta=true` body and answers it with a six-event canned SSE message
(`message_start` ... `message_stop`), the CLI completed its turn and exited cleanly. The recorded
body's `messages[0]` is `role: user` with two text blocks: the reminder plus `"\n"` (rule 1 of the
placement rules), then the prompt text. The CLI also sent other requests, which 404'd
harmlessly and did not stop the turn; no model was called, and no usage was spent. The captured wording confirms the drift
above. Cost of one capture: one CLI start (about 0.4 s). Findings go into the research README.

## Design

### 1. Capture (`reminder.rs`, new module in `claude_subscription/`)

`Cli::learn_reminder(&self, email, version) -> Learned` runs a probe launch:

- Bind `127.0.0.1:0` (tokio `TcpListener`). Launch the CLI through the same `fixed_args`/`launch`
  path as `initialize`, with `ANTHROPIC_BASE_URL` added for this launch only (a `command` variant
  taking extra env; `CONFLICTING_ENV` still refuses an inherited value). Write one user frame
  (`"ok"`) and close stdin. `launch` gains an extra-env parameter (or a `launch_with_env` variant)
  and `ChildFiles::empty` becomes `pub(crate)`, or the launch part of `learn_reminder` stays in
  `cli.rs`.
- The listener keeps accepting connections and requests (keep-alive included) until the probe ends.
  Each request is parsed as HTTP/1.1 (request line, `content-length` body; capped at 8 MiB). It
  answers every non-matching request 404 and hands off only the first POST to the exact
  `/v1/messages` path (not `count_tokens`) whose last user text is the probe text, over a oneshot,
  answering it with the canned SSE body. It never forwards, never logs a body, and discards headers
  (the CLI's bearer token arrives here and is dropped unread). Local-only by binding.
- The probe ends when the matching body arrives (child stopped), the child exits, or the discovery
  timeout elapses.

### 2. Parse and validate (pure, unit-tested)

`parse_reminder(body: &Value, email: &str) -> Option<Reminder>`:

- `messages[0]` is a user message whose content is `[text, text]` with the first block
  `<system-reminder>\n...</system-reminder>\n` and the second our probe text. Any other shape
  (reminder merged into the prompt, placed after it, a string content) gives `None`: the placement
  the rest of the adapter assumes was not observed.
- The reminder contains the email exactly once and holds no section other than `# userEmail`
  (a `gitStatus`/`attachedProject` section makes it cwd- or env-dependent, so one capture cannot be
  replayed). Else `None`. The probe runs in `data_dir/claude-cli-cwd` under
  `GIT_CEILING_DIRECTORIES` (`cli.rs`), where the research measured no `gitStatus`; if a section
  appears anyway the reminder is `Unusable` and the layout is Legacy.
- `Reminder { template: String, wording: String }`: `template` is the block (minus the trailing
  `"\n"`) with the email replaced by `{email}`; `wording` is the lowercase hex SHA-256 of the
  template (`sha2` is already a dependency; `DefaultHasher` is not stable across releases and the
  hash is persisted).

### 3. Memoize

A process-wide store beside `Verdicts` (same `STORES` pattern, keyed by data dir) maps
`(version, sha256(email))` to the outcome: `Learned(Reminder)` or `Unusable` (deterministic: parsed
`None`), both kept for the process lifetime. A transient failure (could not start, timeout, no
request) is not memoized as a result; it is retried no sooner than 5 minutes later per key, and the
request in hand takes Legacy. Concurrent requests for one key share one probe (per-key async
mutex), so a burst of subagents starts one capture.

### 4. Use it

- `layout(context, learned, verdicts)`: Native needs an email, `version >= TEMPLATE_SINCE`, a
  `Learned` reminder, and no unexpired verdict for `(version, wording)`. Everything else, including
  a probe that failed or was unusable, is Legacy: **Legacy is the baseline; Native is earned by an
  observed reminder.**- `apply` takes the learned template instead of `REMINDER_TEMPLATE`. `place` is untouched and keeps
  failing soft on shape drift.
- `Sent` carries the `wording`; `Verdicts` keys `broken`, `struck` and the persisted entries by
  `(version, wording)`. `finished`/`mark`/`strike`/`contains` take the pair. A persisted entry gains
  `"wording"`; `parse` keeps only entries whose `wording` field is present and equal, so a wording
  change retires the verdict automatically. `TEMPLATE_REVISION` is bumped to 2, which also retires
  every entry written before this change (they have no wording and were keyed by a hardcoded
  template); its doc comment narrows to "where `apply` places the reminder".
- `REMINDER_TEMPLATE` is removed from production code. The 2.1.319 text moves into the tests as
  a fixture next to the 2.1.329 text, which is how the issue's "compare with `REMINDER_TEMPLATE`"
  is kept: a test asserts the parser extracts each historical wording into the template the old
  constant held (2.1.319) and a different one (2.1.329), with distinct `wording` hashes.
- The warning text names the wording (first 8 hex digits of the hash) so a persisted verdict is
  traceable to the wording it judged.

### Flow

```
prepare: launch CLI, initialize -> {email, version}
  version >= 2.1.319 and email?  --no--> Legacy
  learned(version, email) memo hit? --no--> probe (fake local server) -> parse -> memo
  Learned + no verdict for (version, wording)? --yes--> Native(apply learned template)
                                               --no--> Legacy
finished(): read shortfall x2 for (version, wording) -> persist verdict {cli, wording, template:2, at}
```

## Implementation Steps

1. `reminder.rs`: `Reminder`, `parse_reminder`, the capture server, the memo store, and
   `Cli::learn_reminder`; `cli.rs`: `command` variant accepting extra env for the probe launch;
   `mod.rs`: register the module.
2. `context.rs`: drop `REMINDER_TEMPLATE`; thread `Reminder` through `layout`, `apply`, `Sent`;
   re-key `Verdicts` by `(version, wording)`; persist/parse the `wording` field; bump
   `TEMPLATE_REVISION` to 2.
3. `transport.rs::prepare`: call `learn_reminder` after the `initialize` answer when the version
   and email gate passes, pass the result to `layout`/`apply`/`Sent`; the warning text names the
   wording.
4. `fontaine-sdk/examples/claude_cli_double.rs`: new scenario key `reminder` (the text it sends as
   the first message's reminder block; absent keeps today's behavior, nothing posted). When
   `ANTHROPIC_BASE_URL` is set and a query frame arrives, it POSTs a minimal `/v1/messages` body
   over a plain `std::net::TcpStream`, reads and ignores the reply, then answers the turn as today.
5. Tests below; docs, research README, CHANGELOG.

## Files to Modify

- `rust/fontaine-sdk/src/core/providers/claude_subscription/reminder.rs` (new), `reminder/tests.rs`
  (new)
- `rust/fontaine-sdk/src/core/providers/claude_subscription/{context.rs,context/tests.rs,cli.rs,transport.rs,mod.rs}`
- `rust/fontaine-sdk/examples/claude_cli_double.rs`
- `rust/fontaine-sdk/tests/claude_subscription.rs` (and `core_common/claude_cli.rs` if a helper is
  needed), `rust/fontaine-sdk/tests/provider_warn_claude_subscription.rs`
- `docs/claude-subscription.md`, `docs/configuration.md`,
  `research/claude-subscription-cli-context/README.md`, `CHANGELOG.md`

## Trade-offs

- **Capture via a local fake server vs. a forwarding proxy** (`proxy.py`): the fake needs no
  upstream, no credentials handling, no inference, and cannot alter a real request; a forwarding
  proxy would carry the user's bearer token to Anthropic through our code for no gain.
- **Probe once per process vs. per request**: the reminder is a function of CLI version and account;
  per-request would add 0.4 s to every call.
- **Persisting the learned wording across restarts**: rejected; re-learning costs one CLI start per
  process and avoids a stale-wording file.

## Decisions

- 2026-11-20: `TEMPLATE_SINCE` stays as a floor. The placement rules `place` implements were
  measured on 2.1.319+ only; learning the wording does not validate placement on older CLIs.
- 2026-11-20: Entries written before this change are retired by the `TEMPLATE_REVISION` bump
  rather than migrated; they carry no wording to compare.
- 2026-11-20: The probe sends the CLI's bearer token to a loopback listener that discards it; the
  listener never forwards. Accepted: the token never leaves the machine and the listener only binds
  `127.0.0.1`.

## Documentation

- `docs/claude-subscription.md`: the cache-layout section says the reminder is learned from the
  CLI at the first request per process (one extra CLI start, no model call), that an unobservable or
  unrecognized reminder falls back to the earlier layout, and that the verdict is keyed to CLI
  version and reminder wording (replace "retired earlier when a Fontaine update changes the
  reminder template"). Specific spots:
  - `:13-14`, `:94-96`, `:264-266`: the per-process probe launch with a loopback
    `ANTHROPIC_BASE_URL`; the listener receives the bearer token and email, discards headers
    unread, and never forwards.
  - `:227-230`: the `(version, wording)` keying, the new shortfall cause and its warning.
  - `:235`: add "or when the reminder could not be learned" to the fallback list.
  - `:100` and `:211`: the extra probe CLI start per process.
- `docs/configuration.md`: the `claude-cache-verdicts.json` row mentions the wording key.
- `research/claude-subscription-cli-context/README.md`: the capture finding (local fake server,
  canned SSE), the list of requests the CLI was observed to send, and the 2.1.329 wording.
- `CHANGELOG.md`: entry.

## Testing Strategy

Unit (`reminder/tests.rs`, `context/tests.rs`):

- `parse_reminder` on the 2.1.319 and 2.1.329 bodies: template with `{email}`, distinct hashes, the
  2.1.319 one equal to the former constant's text (pins the comparison the issue asks for).
- `parse_reminder` returns `None` for: email absent or twice, an extra `gitStatus` section, string
  content, reminder after the prompt, reminder merged into the prompt block, missing closing tag,
  non-user first message.
- Verdict re-keying: a verdict for `(v, wording A)` does not apply to `(v, wording B)`; an entry
  with no `wording` or `template: 1` is ignored; a persisted entry round-trips `wording`.
- `layout`: Legacy without a learned reminder, with `Unusable`, below `TEMPLATE_SINCE`; Native with
  one. `apply` replays the learned text, not a constant (existing placement tests switch to the
  fixture).
- Capture server: given a raw request (content-length body, split across reads, oversized body,
  non-POST path) it returns the body or 404 and the canned SSE, over a real loopback socket.
- Memo: concurrent callers for one key run one probe; a transient failure is retried only after
  5 minutes (tokio `test-util` paused time); `Unusable` is kept.

Integration (headless-surface tests against `claude_cli_double`, no live API):

- A double that posts a drifted reminder: the next request replays the **learned** text (the
  double's logged frames carry the 2.1.329 wording, not the 2.1.319 one) and uses the Native marker
  layout.
- A double that posts nothing / a malformed body / exits before posting: Legacy layout, no warning
  storm.
- Two requests in one process produce exactly one double invocation whose env contains
  `ANTHROPIC_BASE_URL` (via `Claude::invocations()`).

## Manual Verification

1. `cargo run -p fontaine-evals -- cache-bench` against the installed CLI (2.1.329): the replayed
   steps read from the cache with native layout and the run records no verdict. Not automated: it
   needs the live API and a subscription account; the unit and double tests cover the logic, this
   checks the real CLI's reminder end to end.
