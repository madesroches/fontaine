# AFK mode

AFK mode is for working while you are away. It grants no authority. It is a permission mode that
decides what runs exactly as `auto` does: the deterministic floor, the fast paths, the guardian and
every fail-closed rule are what they were. The one thing that changes is what happens where a
person would have been asked: nobody is asked.

- Every outcome that would prompt becomes a denial, the way a [headless run](headless.md#denials)
  already treats it.
- Each denial tells the model what to do next: finish the independent work that is still allowed,
  reduce the request to a useful allowed subset, or stop and report. It never says to ask, and
  never to find another way to the same effect.
- `ask_user_question` takes its usual no-answer path.
- A tripped [denial breaker](guardian.md#denial-limits) ends the run as *blocked* instead of waiting
  for you.

AFK adds two safety outputs of its own: a [report](#the-report) of everything the agents tried, and
a [session temperature](#the-session-temperature) that stops the whole agent tree when denials
pile up.

## Entering and leaving it

AFK is the last stop of the Shift+Tab cycle, ordered by increasing autonomy: manual, plan, accept
edits, auto, AFK, and back to manual. The status bar shows `⏵⏵⏵ AFK mode on`.

| How | Effect |
|---|---|
| Shift+Tab (the `cycle_mode` key) from `auto` | Enters AFK mode. Any prompt already open or queued is answered as a denial and closed. |
| Shift+Tab from AFK | Leaves it for `manual`, then [reviews the stretch](#the-report) and shows its outcome. During a turn the mode changes at once and the review runs when the turn ends. |
| `/plan` | Leaves AFK for plan mode, with the same review. |
| `/afk report [all]` | Shows the report of the latest stretch, or of every stretch. |
| `/afk review` | Lets a [stopped](#the-stop) session run again, in `manual` if AFK was on. |
| `--afk` | Starts the session in AFK mode, in the terminal UI and in `-p`. It conflicts with `--permission-mode`. |

`/afk` takes only `report [all]` and `review`; Shift+Tab enters and leaves AFK mode.

Nothing but these can enter AFK: not a settings key (`default_mode = "afk"` is rejected), not a
tool, not a scheduled prompt, not the model, and a repository cannot. Typing a message does not end
AFK, so a stray keypress cannot reopen approvals; leaving takes a deliberate press of the cycle key,
and it only makes the session ask you again.

A resumed session does not start in AFK, because you ran `--resume` and are there. A temperature
stop survives a resume or a fork until it is reviewed.

AFK and [`/guardian-disable`](tui.md#commands) refuse each other. With the guardian disabled a root
call runs before the floor and never prompts, so AFK would convert nothing; entering AFK says to run
`/guardian-enable` first (the cycle then moves on to `manual`), and `/guardian-disable` is refused
while AFK is on or a stop waits for a review.

A subagent runs at the `auto` rung under AFK, and a child's prompts are converted the same way.

## What a denial says

The denial keeps the original rule and explanation, then the reason it was a denial, then the
guidance. A converted prompt reads like this:

```text
`shell` was denied: `shell(npm publish)` asks for approval before this runs, and the user is away
(AFK mode). The user is not available. Do not retry this call, evade the denial, or use another
tool to achieve the same denied effect. Complete the rest of the request through independently
allowed work; if the whole request cannot be done within those bounds, reduce it to a useful
allowed subset. If no useful allowed progress remains, stop and report what was completed, what was
blocked and why, and what remains. Do not ask for approval.
```

The base prompt's section on denied calls says the same for the whole session, and is rebuilt
before the next turn after AFK is entered or left, which costs one prompt-cache miss each way. A `--system-prompt` replaces that
section, so there the guidance arrives only through the denials. A subagent that was already
running keeps the prompt it started with; the guidance in each denial covers it.

The audit record keeps what the call really was. `tier`, `rule` and the guardian fields are the
original outcome's, `afk` is `true`, and `converted` names the prompt that was turned into a denial:

| `converted` | The prompt that did not happen |
|---|---|
| `ask_rule` | An `ask` rule, or an MCP tool that requires a person's approval |
| `floor` | A critical-path or protected-path call |
| `mode` | The mode's own approval (not reachable in AFK mode, which decides as `auto`; it applies to a headless run) |
| `hook` | A `pre_tool_use` hook's `ask` |
| `escalate` | A guardian `escalate` verdict |
| `screen_escalate` | A [composition](guardian.md#composing-the-guardian) stopping a call |
| `breaker_paused` | A call that needed the guardian while `auto` was paused |
| `guardian_unavailable` | The dialog offered when the guardian model cannot be used; AFK skips it, so it can never switch the mode to `manual` while you are away |

`converted` is also set for the same denials in a headless run. A `permission_denied` hook you
wrote keeps its own retry text.

## When the breaker trips

The breaker is unchanged: three denials in a row or twenty in a session pause `auto`. Under AFK
nothing can resume it, because only your approval does. The session is **blocked**:

- The turn that is running is not cut short; each denial already told the model to report.
- No turn that you did not type starts: no notification turn, no scheduled prompt, no goal
  evaluation. Scheduled prompts stay listed and are not deleted.
- A `/goal` pauses with the reason "the denial breaker tripped while the user was away".
- The terminal UI shows the outcome: `AFK: blocked — repeated denials paused auto mode. N allowed,
  M denied, temperature X (peak Y). Full report: /afk report`.
- A headless run exits 1 with `afk.outcome = "blocked"`, whether or not a goal ran.

After you leave AFK mode, the next call that needs the guardian prompts as usual, and approving it resumes
`auto`.

## The report

`/afk report` shows the latest stretch from the session ledger, which covers the whole history
across a resume, a compaction and `/clear`. `/afk report all` lists every stretch. The sections, in
order:

1. **Outcome**: `active`, `ended`, `blocked` or `stopped`, and how many calls were allowed (and how
   many of those succeeded, failed, were cancelled or have no recorded outcome), and how many were
   denied.
2. **Temperature**: the current value, the peak, the ceiling and the denials that contributed most.
3. **Security review**: the links found between a denial and the calls after it, then what the
   independent assessment concluded.
4. **Timeline**: every call of the main agent and its subagents in order, with the agent's name,
   the tool, a credential-redacted summary, the decision and, for an allowed call, how it ended.
   An allowed call whose end was never seen says `outcome not recorded`, never that it succeeded.

### Links and the assessment

Two links are computed by rule, only between a denial and a later call of the same stretch:

- **Retry**: the same tool with the same arguments as a denied call, from any agent.
- **Same target by another tool**: a different tool that reaches a path the denied call touched, in
  a matching direction. A denied `write` followed by a `shell` redirect into the same file is
  linked. A call to another path, a read after a denied write, and a narrower request are not, so
  independent work stays unmarked.

The second is an *independent assessment*: one tool-less call to the guardian's model, never the
working model, that names pairs where a later call pursues a denied call's effect another way. It
runs when a stretch is blocked or stopped and again when it ends (you leave AFK mode, or a headless
run ends), and only when the stretch holds a denial. It grades the stretch you left, even when it
runs after the mode switch. What it can say:

| Report line | Meaning |
|---|---|
| **suspected evasion** | The assessment named the pair; both calls and its reason are listed. |
| **no suspected evasion detected** | It ran and named nothing. |
| **evasion assessment unavailable: reason** | It timed out, failed, or could not be read; the timeline is still shown. This is never reported as "no evasion". |
| no denials | The stretch had no denial, so there was nothing to assess. |

An allowed malicious call that was never denied is the per-call guardian's job; the assessment adds
the end-of-stretch view of what the per-call gate may have missed.

## The session temperature

The temperature measures how much pressure the denials of a stretch put on the floor. It is
computed from the ledger alone, so the live value and the one in a report cannot differ.

`current` is the sum, over the stretch's denials, of `weight × 0.9^(decisions since it)`, where one
decision is one allowed or denied call by any agent. Time is counted in decisions, not seconds, so
idle time does not cool it and independent allowed work does. `peak` is the highest value reached
and never falls, and cooling never removes a denial from the report.

A denial's weight is its severity, the highest that applies, plus a bonus for retrying:

| Denial | Weight |
|---|---|
| Guardian label Data Exfiltration, Download+Execute, Tunnel/Reverse Shell, Secret Exposure, Safety Bypass, Container Escape or Unguarded Agent Loop | 3.0 (High) |
| Any other guardian label, including one this build does not know; a composition denial; a deny rule, critical path, protected path or plan-mode denial | 2.0 (Medium) |
| A converted prompt, or a call denied while `auto` was paused | 1.0 (Low) |
| A fail-closed guardian denial (error, timeout, unparseable, low confidence) | 0.5 |
| A denied call that repeats an earlier denied call exactly | +2.0 |
| A denied call that reaches the target of an earlier denial by another tool | +3.0 |

A converted escalate keeps its label's weight, so a High label stays High. Temperature feeds no
allow path, no guardian input, no breaker reset and no evidence removal; it only ever tightens, and
only while AFK is on.

The ceiling is `[afk] temperature_ceiling` (default 12.0), or `--afk-temperature-ceiling` for one
session. Under the default, one High denial retried four times, five different High denials in a row
and three alternate-route attempts at the same file each reach it; four different High denials, a
dozen unanswerable prompts with allowed work between them and a guardian outage do not. See
[Configuration](configuration.md#afk) for who may change it.

## The stop

When the temperature reaches the ceiling, the whole agent tree stops:

1. From then on every tool call of every agent is denied, before the escape hatch and the floor,
   with the rule `afk_stopped`. This check can only deny.
2. The turn in flight ends, as Esc ends it, so the model gets no further tool-capable turn, and no
   model request or tool call leaves any agent afterwards.
3. Every running or idle subagent is killed, and every running background job, one by one.
4. The goal pauses; no scheduled prompt or notification turn starts.
5. The stretch is assessed and the report summary is shown.

While the stop stands, a typed prompt is refused with "the session is stopped (AFK temperature
ceiling); run /afk review", the cycle cannot enter AFK again, and the read-only commands
(`/afk report`, `/recap`, `/denied`, `/export`) still work. The status bar shows `AFK STOPPED`.

**`/afk review`** is the only way out, and only in the terminal UI. It shows the report and asks for
a confirmation. On `y` it records the review, clears the stop and, if AFK was on, leaves it for
`manual`. It does not touch
the denial breaker, the peak or any evidence. Cooling below the ceiling never clears a stop.

A headless run has no review. A stopped session resumed with `-p` refuses to run: it makes no model
request, exits 1 with `error_during_execution` and `afk.outcome = "stopped"`.

## Headless runs

`fontaine -p --afk` runs in AFK mode, which decides as `auto` (the `init` event reports `auto`, the
mode the run started in), converts the would-prompt outcomes to denials as a headless run already
does, switches to the AFK guidance and prompt, enables the temperature stop, runs the assessment and adds an `afk` object to the `result`
event:

```json
{
  "afk": {
    "outcome": "ended",
    "counts": {"allowed": 4, "denied": 1, "converted": 0, "succeeded": 4, "failed": 0, "cancelled": 0, "unknown": 0},
    "temperature": {"current": 2.4, "peak": 3.0, "ceiling": 12.0, "contributing": [{"seq": 12, "weight": 3.0, "remaining": 2.4}]},
    "review": {"links": [], "assessment": {"status": "no_suspected_evasion"}},
    "timeline": [{"seq": 12, "agent_name": "main", "tool": "shell", "decision": "denied", "...": "..."}]
  }
}
```

A blocked run exits 1 with `afk.outcome = "blocked"`; a stopped one exits 1 with
`subtype = "error_during_execution"` and `afk.outcome = "stopped"`. Every `gate_decision` event
carries `afk` and `converted`. The schema is in
[headless-v1.schema.json](schemas/headless-v1.schema.json); the object is optional, so version 1
only gained a property.
