# job_cancel Stops Subagents Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/463

## Overview

The model has no way to stop a subagent it started, and nothing it sees says so until a guessed
`job_cancel {"id":"agent-23"}` fails with "only the user can stop a subagent". Claude Code's
`TaskStop` stops a background agent as well as a shell, and Fontaine already extended `job_output`
to `agent-*` ids for the same reason (#301). Extend `job_cancel` the same way: it stops a subagent
the caller started. Stopping one is not destructive to the workspace, and every call the child
made was already gated.

## Current State

- `job_cancel` (`fontaine-tools/src/jobs/tools.rs:365`) is `root_only` and calls `Jobs::cancel`;
  an `agent-*` id reaches `JobError::UnknownJob`, whose text (`fontaine-tools/src/jobs.rs:81`) is
  "is a subagent, not a job; only the user can stop a subagent", with no next step.
- Killing is `fontaine_agents::spawn::kill_agent(tree, ledger, id)` (`spawn.rs:326`); it needs the
  session ledger, so it is reachable from `AgentRuntime::kill_agent` (`fontaine-core/src/agent/messaging.rs:102`),
  used by the agents panel `x` key. A kill is final: the agent cannot be resumed, its descendants
  are killed, and it delivers no report.
- `job_output` reaches subagents through the `AgentWaits` seam (`jobs.rs:153`), implemented by
  `AgentTree` (`tree.rs:1932`), wired as `agent_waits` (`core/src/agent/build.rs:211`). It resolves
  an id or name, and refuses an agent the caller did not start (`AgentWaitError::NotYours`).
  `is_subagent_ref` (`tools.rs:278`) routes any id not starting `job-`.
- The `agent` description (`spawn.rs:983`) says how a subagent goes idle but not how it ends.
  `docs/tools.md:22` and `docs/agents.md` ("Watching and stopping subagents") say the panel stops one.

## Design

**Seam.** A new trait in `fontaine-tools/src/jobs.rs`, beside `AgentWaits`:

```
pub trait AgentKills: Send + Sync {
    /// Stops `id` (an id or name), a child of `caller`, with its running descendants.
    fn kill(&self, caller: &str, id: &str) -> Result<AgentKilled, AgentWaitError>;
}
```

`AgentKilled` carries the resolved id and whether it was running, idle, or already ended. Reusing
`AgentWaitError` (`Unknown`, `NotYours`) keeps one ownership rule shared with `job_output`.

**Implementation** in `fontaine-agents` (a small struct holding `Arc<AgentTree>` and the
`Arc<dyn RootLedger>`): resolve the ref with the same lookup `agent_state` uses, check the parent
is `caller`, call `kill_agent`. Wired in `core/src/agent/build.rs` next to `agent_waits`, carried
through `ToolEnv` and the core tool params like `agent_waits`.

**Tool.** `JobCancelTool` gains `agents: Option<Arc<dyn AgentKills>>`. In `call`: an
`is_subagent_ref` id goes to the seam and returns a short line ("agent-23 (fixer) stopped. It
cannot be resumed; start a new agent for the work, or `send_message` an agent to redirect it
instead."); `job-*` ids keep the current path. `root_only` narrows to the job branch only, so a
subagent can stop its own children, mirroring `job_output`'s 2026-10-30 rule. An already-ended
agent reports that and is left alone, as a finished job is.

**Descriptions.** `job_cancel`: "Stop a background job or monitor, or a subagent you started
(`agent-N` or its name). Stopping a subagent is final: it cannot be resumed and delivers no
report." `agent`: add one sentence, "To end one before it finishes, `job_cancel` it; to redirect
it, `send_message`." `send_message` description: one clause that it cannot stop an agent.

**Errors.** `JobError::UnknownJob` for `agent-*` is now unreachable from `job_cancel`; keep the
text for the remaining `job_output`-free path only if still reachable, else drop the special case
and the "only the user can stop" wording. `NotYours` and `Unknown` text matches `job_output`'s.

**Permission.** `job_cancel` stays class `arbitrary` and keeps its `destructive == Never`
exemption (`descriptor.rs:369`): the target is an id the caller owns, not a path. The floor and
guardian gate it as before; the kill runs only after the call is allowed.

**Telemetry/ledger.** `kill_agent` already records the ledger `finished` event and
`subagent_event("killed", ...)`; nothing new. The agents panel `x` is unchanged.

## Implementation Steps

1. `fontaine-tools/src/jobs.rs`: add `AgentKilled` and `AgentKills`; adjust the `UnknownJob` text.
2. `fontaine-tools/src/jobs/tools.rs`: route agent refs in `JobCancelTool`, narrow `root_only` to
   job ids, update the description.
3. `fontaine-tools/src/builtins.rs`: carry `agent_kills` in the env and hand it to the tool.
4. `fontaine-agents`: implement `AgentKills` (tree plus ledger), checking ownership.
5. `fontaine-core` (`tools.rs`, `agent/build.rs`): wire it for every agent's registry.
6. `fontaine-agents/src/spawn.rs`: update the `agent` and `send_message` descriptions.
7. Docs and Decisions (below).

## Files to Modify

- `rust/fontaine-tools/src/jobs.rs`, `src/jobs/tools.rs`, `src/builtins.rs`
- `rust/fontaine-agents/src/spawn.rs`, plus the new seam impl (`src/tree.rs` or `src/spawn.rs`)
- `rust/fontaine-core/src/tools.rs`, `src/agent/build.rs`
- `docs/tools.md`, `docs/agents.md`, `docs/extensions.md` (alias table already maps `TaskStop`)
- `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **User-only kill plus better descriptions** (issue option 2): rejected. It leaves the model unable
  to do what a user asks ("stop agent-23 and respawn it on X"), and Claude Code can do it.
- **A new `agent_stop` tool**: rejected. `job_output` already unified jobs and agents under one
  id space (#301), and Claude Code has one `TaskStop`; a second tool adds a registry entry, a risk
  class and a description for no gain.
- **Root-only kill**: rejected in favor of own-children, matching `job_output`, so a lead subagent
  can replace a worker without a round trip through the root.

## Decisions

- 2026-11-06: Deviation from the 2026-10-26/30 "the agents panel stays the way to kill one" lines: `job_cancel` also stops a subagent the caller started, matching Claude Code's `TaskStop`; a kill stays final and the human's panel kill is unchanged (#463).

## Documentation

- `docs/tools.md`: `job_cancel` row and section — jobs, monitors and own subagents; final kill.
- `docs/agents.md`: "Watching and stopping subagents" — the model can also stop its own children.
- `CHANGELOG.md` entry.

## Testing Strategy

- Unit (`fontaine-tools/tests/jobs.rs`, fake `AgentKills`): `job_cancel` on an `agent-N` id and on a
  name calls the seam and renders the line; `NotYours` and `Unknown` render the `job_output`
  wording; a `job-*` id still behaves as before; a non-root caller may cancel an agent ref but a
  job id still returns the root-only refusal; an ended agent reports "already ended".
- Seam (`fontaine-agents/tests/seam.rs`, beside the `kill_agent` tests): the real implementation
  kills a running child and its descendants, refuses a sibling or the parent, and a killed child
  cannot be resumed.
- Description test: the `agent` and `job_cancel` descriptions mention the stop path, pinning the
  reported bug (the model guessed because nothing said how).
- Keep the choke-point and `headless_smoke` tool-list tests green.

## Open Questions

None.
