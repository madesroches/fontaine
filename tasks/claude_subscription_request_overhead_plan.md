# claude-subscription: Request Overhead and Prompt-Cache Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/449

Issue: #449 (follow-up to #448). Follow-ups filed from this research: #479, #480, #481, #482,
#483, and a comment on #168.

## Overview

The issue asked what each `claude-subscription` model request costs beyond the inference, and
which costs are worth removing. Measuring first (issue item 3) showed that the biggest problem is
token usage, not latency: **no request ever reads its replayed history from the prompt cache.**
Every request re-writes the whole conversation after the system prompt to the cache, so cache
writes grow quadratically over a tool-heavy turn. Claude Code itself reads that prefix and writes
only the new step. Two things cause it. The CLI writes its own working directory (a fresh random
path per request today) into the prompt. And it attaches per-request context to the final user
message, so the cache entry it writes never matches the next request's prefix. This plan fixes
both from Fontaine's side: one stable, empty working directory, and one cache marker on the last
history user message. Then it removes the separate model-listing process from every request by
asking the request's own CLI process for the listing before the frame that asks for a response,
and runs the login probe alongside the process start instead of before it. Billing and
model-pinning checks keep their current strength and still run before every request.

## Measurements

CLI 2.1.319, Linux, 2026-11-07. A scratch harness reproduced the adapter's exact launch:
`fixed_args`, `FIXED_ENV`, `base_settings`, `write_files`, and the real inert inventory
(`target/debug/fontaine --internal-claude-inventory`). It ran a simulated tool loop: request *k*
replays *k* tool round trips. The setup was a 46 KB system prompt, 12 tools, ~2.2k-token tool
results, `claude-haiku-5-5`, and `claude-sonnet-5-5` where noted. Usage figures are the CLI's own
`result.usage`. The harness is not committed; #483 makes it a repeatable bench.

### Latency per request (today)

| Step | Cost | Note |
|---|---|---|
| `claude --version` | 0.02 s | once per connection already |
| `claude auth status` probe | 0.25 s | a full CLI start, every request |
| `initialize` listing process | 0.33 s | a full CLI start, every request |
| request process: spawn → first answer | 0.35–0.39 s | unavoidable per process |
| inert inventory start | 18 ms | connected asynchronously (`[MCP] --mcp-config servers running fully async`), off the critical path |
| history acks, sequential | ~4.5 ms per replayed user message | 101 acks took 0.45 s after start-up |
| query written → `message_start` | 0.6–2.3 s | API time to first byte; grows with uncached input |

So today a request spends ≈ 0.97 s + 4.5 ms × (user messages) before its query reaches the API.
After this plan it spends ≈ 0.37 s + 4.5 ms × (user messages).

### Prompt cache

`cache_read` / `cache_creation` input tokens per request. "Base" is the system prompt plus tools
(20,930 tokens in the harness).

| Scenario | k=1 | k=2 | k=3 | k=4 | k=5 |
|---|---|---|---|---|---|
| today (fresh cwd per request) | 20,930 / 2,903 | 20,930 / 5,103 | 20,930 / 7,303 | 20,930 / 9,503 | 20,930 / 11,703 |
| stable cwd, no marker | identical replay: 26,005 / **0**; next step: 20,930 / 7,275 (still a full re-write) | | | | |
| stable cwd + marker on the final frame | 20,930 / 2,904 | 20,930 / 5,104 | 20,930 / 7,304 | 20,930 / 9,504 | 20,930 / 11,704 |
| **stable cwd + marker on the last history user message** | 20,930 / 2,904 | 21,062 / 4,972 | 23,578 / 4,656 | 25,778 / 4,656 | 27,978 / 4,656 |

Over five steps, cache writes fall from 36.5k to 22.1k tokens, and the gap grows linearly with
the turn. Projected for a 30-step turn with the same step size, writes are ~1.04 M today and
~0.14 M after the change. A native session would write ~0.07 M. A fresh CLI session id per
request does not change the fixed figures (rerun with a new `--session-id` per request), so the
cache does not depend on the derived session ids.

Through Fontaine itself, `fontaine -p "Reply with just OK." --model claudemax/sonnet` run twice
gave 8,952 read and ~3,165 written per run: only the tools and the start of the system prompt
were reused. For the same prompt, `claude -p` used 11,382 read and 8,236 written. So Fontaine's
base context (~12.1k tokens) is smaller than Claude Code's (~19.6k), and the base prompt is not
where usage is lost.

### What the CLI adds to each request

Asked to quote everything it was given that the user had not typed, the model reported the
following additions on top of `--system-prompt-file`. The cache figures above corroborate each
one.

- an identity line before Fontaine's system prompt, and an `# Environment` block holding the CLI's
  **working directory**, git status, platform, shell and OS version. A different path changes
  the prompt after the base, which is why the identical replay above only hit once the cwd was
  fixed;
- a `<system-reminder>` with the account's `userContext` (the account email) and a "Today's date"
  line, **on the final user message only**. Replayed as history on the next request, that
  message no longer carries them. So the entry the CLI cached at its last-message breakpoint
  never matches again, and a marker on the final frame does nothing (table row 3). A marker one
  user message earlier sits before the injection and is reused (row 4). The cost is that each
  request writes the last two messages instead of one; #480 tracks removing the injection.

### Candidates the issue listed

| Candidate | Finding | Verdict |
|---|---|---|
| 1. Cache the model listing | Saves 0.33 s per request, but a time window lets a credit-billed or re-resolved model through until it expires | Replaced by a stronger variant: the listing comes from the request's own process (0 s extra, no staleness) |
| 2. Reuse the inventory process | Starts in 18 ms, asynchronously, off the critical path | Not done |
| 3. Measure | Done, above | Recorded here and in `docs/claude-subscription.md` |
| 4. Batch the history acks | Writing all history frames before reading makes the CLI coalesce them (101 frames → 2 results), and the resulting API request is wrong: 3,219 tokens written against 9,504 for the same history replayed sequentially, i.e. history was dropped | Not done: acks stay sequential (~4.5 ms each) |

### Other findings, filed

- #479: a `max_tokens` stop triggers the CLI's output-limit recovery, up to three extra
  inferences in the same process (seen: four in one request). The adapter then fails the request
  as a protocol error after the extra inference was already sent.
- #480: the injected identity, environment, account email and date: fidelity, privacy, and the
  residual 2× cache write.
- #481: derive the per-request login check from the request's own `initialize` answer (`account`
  has `subscriptionType` and `apiProvider`; `system/init` has `apiKeySource`) once every login
  state is qualified live.
- #482: pre-start the next CLI process while a tool runs (hides the 0.35–0.39 s start-up, including
  for the guardian).
- #483: a live bench comparing per-step cache usage with `claude -p`, so the next CLI change
  that breaks caching shows up as a number.
- #168 (comment): `thinking: disabled` through the CLI is an API 400, because the CLI always adds
  the `clear_thinking_20251015` strategy. Latent: this provider sends no thinking-off today.

## Current State

- `ClaudeTransport::open` (`rust/fontaine-core/src/providers/claude_subscription/transport.rs:122`)
  translates the body, then awaits `handshake()` (`transport.rs:240`). That calls
  `Cli::discover()` (`cli.rs:368`): `preflight()` (environment check, `check_version` once,
  `check_login` = `claude auth status` every time) then `initialize()` (`cli.rs:380`), which
  launches a separate listing process. `handshake` then checks the selected row (listed, not
  credit-billed, same canonical id). Only after all of that does it create a `Scratch`, write the
  files and launch the request process.
- `Cli::launch` (`cli.rs:481`) runs every process with `current_dir(&scratch.path)`, and
  `Scratch::new` (`cli.rs:733`) is a fresh `tempfile` directory (`fontaine-claude-XXXXXX`) per
  launch. That path is what the CLI writes into the prompt's environment block.
- `protocol::frames_of` (`protocol.rs:469`) builds the stream-json frames; `user_content`
  (`protocol.rs:529`) and `assistant_content` strip every `cache_control`. The claude-subscription
  wire is built without rig's prompt caching (`mod.rs`, `Connected::model`), so the CLI's own
  breakpoints are the only ones.
- `FIXED_ENV` (`cli.rs:131`) does not switch off the CLI's claude.ai connector fetch. Each process
  calls `GET /v1/mcp_servers` at start (debug log: `[claudeai-mcp] Fetching …`), and
  `--strict-mcp-config` then discards the result.
- Tests: `rust/fontaine-core/tests/claude_subscription.rs` drives the adapter against the portable
  double `rust/fontaine-core/examples/claude_cli_double.rs`. The double logs each invocation's argv,
  env and cwd and every stdin frame to `log.jsonl`, and already answers a `control_request`
  `initialize` inside a conversation process before the user frames (`claude_cli_double.rs:208`).

## Design

### 1. One stable, empty working directory

`Cli` gains a `workdir: PathBuf`, and `launch` runs every CLI process there instead of in the
per-launch scratch directory. `Scratch` keeps holding the per-launch files (settings, MCP config,
system prompt, manifest), which reach the CLI only as absolute paths in argv and never appear in
the prompt. Using it for every launch (probes, listing, requests) keeps one code path.

- **Location:** `<data_dir>/claude-cli-cwd`, from `ConnectContext::data_dir`. That path is stable
  across requests, namespaces, sessions and resumes, so a resumed session and the guardian share
  the same prompt prefix. It sits outside any workspace and git repository, so the environment
  block's "Is a git repository" stays `false` and the CLI never reads a project.
- **Creation:** `ClaudeSubscription::connect` creates it with `owner_only::create_dir`. An
  `AlreadyExists` result is accepted only when `symlink_metadata` shows a directory, not a
  symlink. A failure is a connect error carrying the I/O source.
- **Emptiness, checked at every launch:** `Cli::launch` refuses to start (`ErrorKind::Cli`, with a
  message naming the directory and telling the user to empty it) when `read_dir(workdir)` yields
  any entry. This keeps the property the docs state today ("each CLI process runs in an empty
  private directory") and fails closed if something, a future CLI included, starts leaving files
  there. Fontaine never writes into it. In the harness runs, the CLI left nothing in its cwd.
- `Cli::new` takes the workdir. The eval harness (`rust/fontaine-evals/src/live.rs`) connects
  through the same `ConnectContext` and gets it for free.

### 2. One cache marker on the last history user message

In `frames_of`, after the frames are built and the existing stripping has run, the adapter puts
`{"type": "ephemeral", "ttl": "1h"}` on one block. That block is the last cacheable block of the
**last user frame before the final (query) frame**. A cacheable block is a non-empty `text`, an
`image`, a `document` or a `tool_result`. A string-content user message becomes one text block to
carry it. A request with only one user message gets no marker. The final frame never gets one,
and assistant frames never do.

Why that position: the CLI's own last-message breakpoint covers the final frame plus the CLI's
injected tail, which the next request cannot reproduce. The last history user message is the
newest position whose bytes the next request replays unchanged. A marker there is read back by
the next request, through the API's 20-block lookback from that request's own marker.

Why `1h`: the CLI writes its own entries with the 1-hour TTL (`ephemeral_1h_input_tokens` in every
measured response), and an earlier breakpoint must not have a shorter TTL than a later one. `1h`
is valid whatever TTL the CLI picks for its own breakpoint, and it survives a long tool run
between steps.

Breakpoint budget: the API allows four. With the CLI's own breakpoints, the marker was accepted on
every measured request. If a future CLI used all four, the request fails with an API 400 that the
adapter reports as a non-retryable native error. That shows up immediately and doesn't cost
silently, and #483's bench catches it.

### 3. The listing check on the request's own process; the login probe alongside

`open` becomes:

```
refused? ─► translate ─► check_environment ─► check_version (cached after the first)
                                                   │
             ┌─────────────────────────────────────┴───────────────────────────┐
             ▼                                                                 ▼
   check_login (`auth status`)                 launch request process in the stable workdir
             │                                 write `initialize` control request (first frame)
             │                                 read rows (bounded by `timeouts.discovery`)
             │                                 check_row(rows, selection)
             │                                 replay history frames, one ack each
             └──────────────────────── join ───────────────────────────────────┘
                                         │
                     login failed ─► that error (the child is dropped: tree killed)
                     replay failed ─► that error
                                         │
                                         ▼
                          write the query frame (the only one that asks for a response)
```

- **What it keeps.** Every check runs before every request, against the CLI's state at that
  moment, and nothing that can start an inference is written until all of them pass. History
  frames carry `shouldQuery: false`, and the CLI answers them with zero-turn results (a
  `num_turns` other than 0 is already a protocol error in `Reader::result`), so they never reach
  the API. The version check still completes before any request process starts, because an older
  CLI could treat a history frame differently. That is why it stays ahead of the fork, not
  concurrent with it.
- **Error precedence** is deterministic and matches today's order: the environment, then the
  version, then the login error, then the row error, then replay errors. The two branches are
  `join`ed, not `try_join`ed, so a login failure is reported even when the request process also
  failed because of it.
- **DRY:** `Cli::initialize`'s request/response exchange moves into
  `CliChild::list_models(&mut self, idle) -> Result<Vec<ModelRow>, ClaudeError>`. Both the listing
  process (`Cli::initialize`, still used by `list()` and `/login`) and the request process use it.
  The selected-row checks move out of `handshake` into a pure
  `check_row(rows: &[ModelRow], selection: &Selection) -> Result<(), ClaudeError>` with the same
  three refusals and texts. `handshake` is removed.
- **Timeout:** the `initialize` answer on the request process keeps the listing's bound
  (`timeouts.discovery`, `ErrorKind::SetupTimeout`, not retried). The ack reads keep `read_idle`.
- `list()` / `discover()` are unchanged: model discovery still runs the full preflight and its own
  listing process.

### 4. The CLI's claude.ai connector fetch off

`FIXED_ENV` gains `("ENABLE_CLAUDEAI_MCP_SERVERS", "false")`. Verified on 2.1.319: with the
adapter's launch, the debug log shows one `[claudeai-mcp] Fetching` line without the variable and
none with it. This drops one network request per CLI
process, and the user's connector list no longer reaches a process that throws it away. It changes
nothing the adapter validates: `--strict-mcp-config` already kept those servers out of
`system/init`.

## Implementation Steps

1. **Stable workdir** (`cli.rs`, `mod.rs`)
   - Add `workdir` to `Cli` and `Cli::new`. In `launch`, run in `workdir` and refuse a non-empty
     one. `Scratch` stays the owner of the files and their removal.
   - In `ClaudeSubscription::connect`, create or accept `<data_dir>/claude-cli-cwd` as described.
     Update the eval harness's `Cli::new` call if it builds one directly.
2. **Marker** (`protocol.rs`)
   - Add `mark_history(frames: &mut [Frame])`, called at the end of `frames_of`. Put the
     `TTL`/marker value in one `const`.
3. **Handshake on the request process** (`cli.rs`, `transport.rs`)
   - Extract `CliChild::list_models` from `Cli::initialize`, and make `Cli::initialize` call it.
   - Replace `handshake` with `check_row`, and restructure `open` into the two joined branches
     above. Keep the existing ack loop and the early-exit classification (`exit_failure`) inside
     the replay branch.
4. **Connector fetch** (`cli.rs`): add the `FIXED_ENV` entry.
5. **Double** (`claude_cli_double.rs`): add a scenario key `init_on_request` (rows answered on the
   conversation process) only if the existing `models` key is not enough. It already answers there,
   so most likely no change. Make `init_hang` apply to the conversation process too (it does
   today, since the same code path answers both).
6. **Tests** (below), then **docs** and **CHANGELOG**.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/cli.rs`: `workdir`, launch check,
  `CliChild::list_models`, `FIXED_ENV`.
- `rust/fontaine-core/src/providers/claude_subscription/transport.rs`: `open` restructure,
  `check_row`, `handshake` removed.
- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`: `mark_history`.
- `rust/fontaine-core/src/providers/claude_subscription/protocol/tests.rs`: marker tests.
- `rust/fontaine-core/src/providers/claude_subscription/mod.rs`: workdir creation in `connect`.
- `rust/fontaine-core/tests/claude_subscription.rs`: request-flow tests.
- `rust/fontaine-core/examples/claude_cli_double.rs`: only if step 5 needs it.
- `rust/fontaine-evals/src/live.rs`: only if it constructs `Cli` directly.
- `docs/claude-subscription.md`, `CHANGELOG.md`.

## Trade-offs

- **Listing on the request process vs. a cached listing (the issue's candidate 1).** A window of
  N seconds saves the same 0.33 s, but a model that turns credit-billed is caught up to N seconds
  late, and the issue requires the checks not to get weaker. Asking the process that will serve
  the request costs nothing extra (it answers in its own start-up time, 0.35 s) and is fresher
  than today, where the listing comes from a different process that ran just before. Chosen.
- **Login probe alongside vs. dropped.** `initialize`'s `account` and `system/init`'s `apiKeySource`
  probably make `auth status` redundant, but the signed-out and Console-login shapes were not
  observable without changing the machine's login. Running the probe concurrently removes its
  latency now without weakening anything; #481 qualifies dropping it.
- **Stable directory under the data dir vs. a fixed path in the temp dir, or per connection.** A
  fixed name in a shared temp dir invites symlink and squatting races. A per-connection directory
  loses the cache across resumes and between concurrent sessions' guardians. The data dir is
  owner-only and already Fontaine's.
- **Stable private directory vs. the real workspace as the CLI's cwd.** The workspace would make
  the injected environment block truthful. But the CLI would then see a git repository: its
  `gitStatus` context changes with every commit and would break the cache in its own way. It would
  also put project files within the reach of a builtin plugin (`cc-plugin-agents-md`) that today
  only stays off because the directory is empty. Rejected. The block's wrong path is tracked in
  #480.
- **Marker on the last history user message vs. on the final frame or on the last assistant
  message.** The final frame was measured to give no reuse. The last assistant message is never
  replayed with the bytes of a position the previous request marked, so it gives none either.
  The last history user message measured a flat write per step.
- **Sequential acks kept.** Pipelining saves ≤ 4.5 ms per replayed user message and was measured
  to corrupt the replayed history.

## Decisions

- 2026-11-07 Measurement showed token usage, not latency, is the dominant cost. The cache fix
  (sections 1 and 2) leads this plan, and the latency work is limited to what keeps the checks
  as strong as today.
- 2026-11-07 Candidates 2 (inventory reuse) and 4 (batched acks) are not done, on the numbers
  above. Candidate 1 is replaced by the listing on the request's own process.
- 2026-11-07 The residual 2× per-step cache write, from the CLI's injection on the final user
  message, is accepted here and tracked in #480.
- 2026-11-07 The `max_tokens` recovery bug (#479) and the thinking-off 400 (#168 comment) were
  found during this measurement but are fixed separately, to keep this change about per-request
  overhead.

## Security

- The billing guard is unchanged in strength: environment, version, login and the selected row
  are all checked before every request's query frame. The only change in timing is that the
  request process may start before the login and row checks finish, and it writes nothing that
  can start an inference until they pass. A failed check drops the child, which kills its tree.
- The CLI still runs in an empty, owner-only directory, now a stable one that every launch checks
  for emptiness. Per-request files stay in a fresh owner-only directory removed with the process.
- The marker is a `cache_control` field Fontaine adds itself, at a position Fontaine chooses.
  Request content can't place or move it, because every incoming `cache_control` is still
  stripped first.
- The connector fetch switch removes a network request and does not widen anything.

## Documentation

`docs/claude-subscription.md`:

- **Checks before every request:** step 3 now runs alongside the start of the request's CLI
  process, and step 4 is asked of that same process before the frame that asks for a response.
  No response is requested until every check passes, and a refused request still runs no turn.
- **What a request is:** the CLI runs in one stable, empty, owner-only directory in Fontaine's
  data directory (`claude-cli-cwd`), checked empty before every launch. Per-request files stay in a
  temporary directory. The adapter marks the last replayed user message for the prompt cache. The
  CLI's claude.ai connectors are not fetched. Update the instruction-files paragraph that says
  "an empty private scratch directory".
- **Supported CLI versions:** replace the closing latency/cache sentence with the measured
  behavior for 2.1.319. Each request costs a CLI start (~0.4 s) plus ~5 ms per replayed user
  message. Replayed history is read from the prompt cache up to the previous request's last
  replayed user message. Each request writes the last two messages to the cache, about twice a
  native Claude Code session's write per step, because the CLI adds its own context to the final
  message.

`CHANGELOG.md` under `[Unreleased]`: a `### Changed` line for the cache reuse and the per-request
latency drop, with the issue number.

No other page describes the per-request process at this level. `docs/architecture.md`'s "runs the
Claude CLI once per request" stays true.

## Testing Strategy

Unit tests (`protocol/tests.rs`, `transport.rs`, `cli.rs`):

- `mark_history`: exactly one marker in the frames, on the last cacheable block of the last
  history user frame. None when there is a single user message. Never on the final frame or an
  assistant frame. A trailing empty text block is skipped for the previous cacheable block. String
  content becomes one marked text block. A `cache_control` arriving in the request (any position)
  is stripped and does not survive next to the adapter's marker. The value is
  `{"type": "ephemeral", "ttl": "1h"}`.
- `check_row`: missing alias, credit-billed row and changed canonical id each give today's kind
  and text. A matching row passes.
- `FIXED_ENV` contains `ENABLE_CLAUDEAI_MCP_SERVERS=false` (extend the rebuilt-environment
  assertions in `the_cli_gets_a_rebuilt_environment`).

Integration tests against the double (`tests/claude_subscription.rs`). These pin billing
behavior, a silent-failure class, so they are automated:

- **Gate before the query.** For a credit-billed row, a changed canonical id, a row no longer
  listed, a signed-out `auth status` and a hung `initialize`, the request fails with today's kind,
  and the double's frame log holds no query frame (every `user` frame logged carries
  `shouldQuery: false`). This rewrites `a_changed_model_row_is_refused_at_request_time`: its
  `model_runs().is_empty()` assertion becomes "no query frame". The request process may now start,
  but it never requests a response.
- **Precedence.** A signed-out `auth status` together with a credit-billed row reports
  `ErrorKind::Auth`.
- **No separate listing process per request.** Two completions after discovery start exactly two
  CLI processes with `--model` and no extra listing process. Each request process's first stdin
  frame is the `initialize` control request.
- **Stable workdir.** Every invocation's cwd (main, guardian and a subagent namespace, across two
  requests) is the same `<data_dir>/claude-cli-cwd`. It is not the directory holding the files named
  in argv, and it is empty after the requests. A file placed in it makes the next request fail with
  `ErrorKind::Cli` naming the directory, before any process starts. Update
  `dropping_a_stream_kills_the_process_tree_and_removes_the_directory` to check the per-request
  file directory (the parent of `--settings`) instead of the cwd.
- **Version first.** `versions_are_checked_before_anything_else` keeps passing: an old CLI starts
  no request process.
- **Conflict seen live.**
  `a_conflict_introduced_after_connecting_is_seen_before_the_next_request` keeps passing unchanged.
- The rest of the existing suite (protocol violations, retries, kill on drop, denial orders)
  passes unchanged.

## Manual Verification

These need the real, signed-in CLI, which tests never run by policy. #483 will automate them as a
live bench.

1. Cache reuse through Fontaine. Before and after the change, in an empty scratch workspace with
   six small files:
   `cargo run -p fontaine -- -p "Read a.txt, b.txt, c.txt, d.txt, e.txt and f.txt one at a time, then reply DONE." --model claudemax/sonnet --output-format json`.
   Expected: `usage.main.cache_creation_input_tokens` drops sharply (the history is no longer
   re-written each step), and `cache_read_input_tokens` grows. Run the same prompt through
   `claude -p --model sonnet --output-format json --no-session-persistence` and record both totals
   in the PR. Expect Fontaine's cache write at about twice the native per-step write plus its
   smaller base.
2. Marker accepted. Step 1 completes with no API 400. The marker adds one breakpoint to the CLI's
   own.
3. Latency. Time step 1's run before and after (`time cargo run -p fontaine -- -p …`, built
   beforehand). Expect about 0.6 s less per model request (seven requests for six reads plus the
   answer), allowing for API variance.

The connector-fetch switch was qualified while writing this plan (section 4), so it needs no step
here.

## Open Questions

None blocking. Two choices were made on the user's behalf and can be revisited:

- The marker's TTL is `1h` to match the CLI. If the account should prefer the cheaper 5-minute
  write, it can be lowered only if the CLI's own breakpoint moves to 5 minutes too.
- The stable directory is shared by every Fontaine session of the user. A per-account directory
  (`claude-cli-cwd/<account>`) would also work, but would split the cache between two
  `claude-subscription` accounts that use the same login.
