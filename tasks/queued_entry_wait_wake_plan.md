# Wake a Blocking Wait for Queued Commands and Skills Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/434

## Overview
A user entry typed while the model is parked in a `job_output` wait only wakes the wait when it is
a plain message at the head of the queue. A queued `/afk`, other built-in, skill or `!` line
neither wakes the wait nor reaches the model, so the model keeps waiting (up to `wait_secs`)
while the TUI shows `queued: /afk`. Make any new queued entry end the wait promptly and tell the
model, in the wait's own answer, that the user has something pending.

## Current State
- Root cause (confirmed in code): `WaitWake::pending` (`rust/fontaine-tools/src/jobs.rs:163`) is
  implemented by `CoreWake` (`rust/fontaine-core/src/child_runner.rs:~200`) as
  `tree.has_message(agent) || steering.has_deliverable_message_for(agent)`.
  `Steering::has_deliverable_message` (`rust/fontaine-core/src/steering.rs:103`) is true only when
  the head of `queued` is `Queued::Message` and no delivery hold is set. A `Queued::Command`,
  `Skill` or `Shell` entry returns false, so the wait never wakes.
- Those entries are deliberately not delivered to the model mid-turn: `SteeringHook::steer`
  (`steering.rs:180-243`) drains only the leading run of `Message` entries, and a
  Command/Skill/Shell entry blocks itself and everything behind it; the TUI runs them after the turn
  (`fontaine-tui/src/ui/turn.rs`, `converse`). That ordering is correct and stays.
- `JobOutputTool::call` (`rust/fontaine-tools/src/jobs/tools.rs:~311-345`) races `arm` against
  `woken` (polls `pending` every `WAKE_POLL` = 250 ms). On wake it re-reads with a zero wait and
  appends the fixed text `[Stopped waiting: a message arrived.]`.
- Plain `Message` entries already work end to end: the wait wakes, and the same result carries the
  message via the steering hook.
- Per-subagent queues (`queue_for`) hold only plain text and already wake via
  `has_deliverable_message_for`; unaffected.

## Design
Three additions, all keeping the queue-ordering rules untouched.

1. **Announce unannounced queued entries.** `SteeringState` gains `announced: usize`, the count of
   leading queue entries already announced to the model. `Steering` gains:
   - `has_unannounced(&self) -> bool`: not held and `queued.len() > max(p, announced)`, where `p`
     is the length of the leading run of `Queued::Message` (the same `take_while` prefix `steer`
     drains and delivers in full). Plain messages are never announced, so a lone queued
     `Message("hi")` neither announces nor is named in the notice.
   - `take_announcement(&self) -> Option<String>`: when `has_unannounced`, returns a notice naming
     the entries from index `max(p, announced)` onward and sets `announced = queued.len()`.
   Maintenance of `announced`: `retract` takes the whole queue, so it resets `announced` to 0;
   `steer` subtracts the number of messages it drained (saturating); `queue` leaves it alone. The
   root-only check keeps this separate from `queued_for`.
2. **Wake on it.** `CoreWake::pending` for the root agent becomes
   `tree.has_message(agent) || steering.has_deliverable_message_for(agent) ||
   steering.has_unannounced()`. Because announcing sets `announced`, the next `job_output` wait the
   model starts parks normally instead of waking again at once. A deliverable `Message` still wakes
   independently of `announced`.
3. **Say it in the wait's answer.** `WaitWake` gains a defaulted method
   `fn take_notice(&self, agent: &str) -> Option<String> { None }`; `CoreWake` implements it for
   the root as `steering.take_announcement()`. In `JobOutputTool::call`'s wake branch, after the
   zero-wait re-read, append `take_notice` when `Some`, otherwise the existing
   `[Stopped waiting: a message arrived.]`. Notice text, user words marked as such:

   ```
   [Stopped waiting: the user queued input while you waited: `/afk`. It runs when this turn ends;
   wrap up or change course if it concerns your current work.]
   ```
   The wrapper is fixed harness text; entry text is quoted verbatim and only the first line of
   each entry, truncated with `first_line_cut` (`rust/fontaine-core/src/session.rs:907`, made
   `pub(crate)`) at 100 chars per entry, so a multi-line paste cannot forge a harness frame.

The message-delivery path, the delivery hold, retract/edit, and ledger/guardian-intent rules
(a message becomes intent only when delivered) are unchanged. The announcement is a pointer, not
the entry, so a queued `/afk` never becomes model-authored intent.

Other blocking waits (`join_wait`, headless `wait_for_news`, `ask_user`, `schedule_wakeup`) are
out of scope: `join_wait` is a child's wait and reads only that child's own queue, so it is unaffected, and headless has no mid-turn input.

## Implementation Steps
1. `rust/fontaine-core/src/steering.rs`: add `announced` to `SteeringState`; add
   `has_unannounced` and `take_announcement`; adjust `retract` and `steer` to keep `announced`
   consistent; unit tests below.
2. `rust/fontaine-tools/src/jobs.rs`: add defaulted `take_notice` to `WaitWake`.
3. `rust/fontaine-core/src/child_runner.rs`: `CoreWake::pending` root clause and `take_notice`.
4. `rust/fontaine-tools/src/jobs/tools.rs`: use `take_notice` in the wake branch; update
   `JobOutputTool::description()` (~280-290), "Any wait returns early when a message reaches you",
   to also mention the user queuing input.
5. Docs: `docs/tui.md` ("Messages during a turn"), `docs/agents.md` ("Waiting for a subagent"),
   `docs/tools.md` (`job_output`, and the monitor bullet ~395-396, "waits (for wait_secs, and until a
   message arrives)").
6. Master plan `## Decisions`: dated line at the end of `### Agents` (queued commands/skills wake
   a wait and are announced, not delivered); `CHANGELOG.md` entry under `### Fixed`.

## Files to Modify
- `rust/fontaine-core/src/steering.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine-tools/src/jobs.rs`
- `rust/fontaine-tools/src/jobs/tools.rs`
- `rust/fontaine-tools/tests/jobs.rs`
- `docs/tui.md`, `docs/agents.md`, `docs/tools.md`
- `tasks/fontaine_harness_plan.md`, `CHANGELOG.md`

## Trade-offs
- Delivering the queued command text to the model as if it were a message was rejected: commands
  and skills expand and run in the TUI after the turn, and delivering them early would reorder the
  user's input and make `/afk`-style text model-visible intent.
- Waking on every queue change without an `announced` mark was rejected: the model's next wait
  would wake instantly and spin until the turn ended.
- Cancelling the wait and ending the turn was rejected: the issue asks to give the model an
  opportunity to react, and interrupting is the user's Esc.

## Decisions
None yet.

## Documentation
`docs/tui.md` "Messages during a turn": a queued command or skill ends a blocking wait and the
model is told it is pending. `docs/agents.md` and `docs/tools.md` `job_output` and the monitor bullet: the
early-return list gains queued entries.

## Testing Strategy
All unit/integration, no live APIs:
- `steering.rs`: a lone queued `Message` yields no announcement (`has_unannounced` false,
  `take_announcement` `None`); a queued `Command` makes `has_unannounced` true; `take_announcement` returns the
  raw text once and then `None`; a second queued entry announces only the new one; `retract` and
  delivery keep `announced` consistent; a delivery hold suppresses it; `has_deliverable_message`
  behavior unchanged.
- `fontaine-tools/tests/jobs.rs` with `FakeWake`: a wake with a notice appends the notice instead
  of the fixed text; a wake without one keeps the old text. Pins the reported bug (`/afk` queued
  during a `wait_secs: 600` subagent wait).
- `fontaine-core` test with the real `CoreWake` and `Steering` (closes the gap that no test wakes
  a live wait through the real wake object): queue `Queued::Command`, assert `pending` flips true,
  and after `take_notice` a fresh `pending` is false; also a lone `Queued::Message`: `pending` is
  true (existing deliverable path) and `take_notice` is `None`, so the old text is kept.

## Open Questions
None.
