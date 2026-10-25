# Tool Descriptor Table Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/181

## Overview

Adding a built-in or extension tool means editing parallel lists that nothing checks against each
other at compile time (name list, two risk tables, `names_no_path`, `destructive::derive`,
`validate_specifier` and the rule matchers, `claude_tool`, and a struct field per extension tool).
This plan replaces them with one central descriptor table in `fontaine-tools`, one row per tool,
that the permission code, the config validators and the registry read. The risk class stays in that
reviewed table and never moves onto the tool. No permission decision changes.

## Current State

- `fontaine-tools/src/names.rs`: `BUILTIN_TOOL_NAMES: [&str; 13]` (fixed-size) plus `is_builtin`,
  `is_mcp`. Used by rule parsing, `[agents]` toolset validation (`config.rs:1076`), skill
  `allowed-tools` (`skill`-loading in `fontaine-core/src/skills.rs`-area `names_a_known_tool`,
  `extensions.rs:267`) and `map_tool_entry`.
- `fontaine-tools/src/risk.rs`: `BUILTIN_RISK: [_; 8]` and `EXTENSION_RISK: [_; 5]` with
  `builtin_risk`, `extension_risk`, `registered_risk`. `builtins.rs:334` (`risk_of`) and
  `fontaine-core/src/tools.rs:96` (`register_extension`) look risk up by `T::NAME`, defaulting to
  `Arbitrary` when absent. `fontaine-evals/src/none.rs` and `fontaine-core/tests/common/pipeline.rs`
  call `registered_risk`.
- `fontaine-core/src/permissions.rs:1640` `names_no_path`: `skill | ask_user_question | todo_write`.
- `fontaine-core/src/destructive.rs:57` `derive`: a name match: never destructive for the
  read-only set, always for `webfetch`/MCP/unlisted, file-write logic for `write`/`edit`, shell
  analysis for `shell`; `_ => true` for everything else.
- `fontaine-core/src/permission_rules.rs`: `PATH_TOOLS` (`:98`), `validate_specifier` (`:153`) and
  the matcher/pattern `match call.tool` arms (`:425`, `:583`) each name tools; `send_message`,
  `ask_user_question`, `todo_write` take no specifier.
- `fontaine-core/src/extensions.rs:103` `claude_tool`: Claude Code name to Fontaine name
  (`MultiEdit`→`edit`, `Task`/`Agent`→`agent`, ...); its error message at `:134` lists the Claude
  names by hand.
- `fontaine-core/src/tools.rs:68` `ExtensionTools` has one field per extension tool and
  `registry` pushes each by hand; `agent/build.rs:127` builds the struct;
  `fontaine-core/tests/choke_point.rs:81` builds it again.
- Tests tied to the lists: `fontaine-tools/tests/choke_point.rs`, `fontaine-core/tests/choke_point.rs`
  (also pins `EXTENSION_RISK` literally), `fontaine-core/tests/docs.rs:203` (docs row per tool).
- Out of scope: `fontaine-evals/src/baseline.rs` `TOOL_NAMES` (a deliberately partial map used to
  drive `claude -p` baselines), and display-only name matches (`permissions::summarize`, the TUI
  transcript).

## Design

### The table

New `fontaine-tools/src/descriptor.rs` (re-exported through `names`/`risk` where callers already
import from there) holding the types and one `pub static TOOLS: &[ToolDescriptor]` (a slice, so
there is no length to bump):

```text
struct ToolDescriptor {
    name: &'static str,
    risk: RiskClass,
    assembly: Assembly,            // Builtins (built by builtins::build) | Extension (assembled by core)
    path_free: bool,               // names no path, so containment has nothing to check
    destructive: Destructive,      // Never | Always | FileWrite | Shell
    specifier: Specifier,          // None | Shell | Domain | Path | Name
    claude: &'static [&'static str], // Claude Code names; the first is canonical
}
```

`Destructive` and `Specifier` are closed enums: the table names the policy, the logic stays where
the facts are (`destructive.rs`, `permission_rules.rs`) and matches the enum exhaustively, so a new
variant forces every consumer to handle it. A tool not in the table keeps today's fail-safe
treatment: `Arbitrary` risk, destructive, no specifier, containment applies. MCP tools are not in
the table and keep `Arbitrary`, always destructive, no specifier.

Rows (existing behavior pinned exactly): read/ls/glob/grep `ReadOnly`, `Never`, `Path`;
write/edit `WorkspaceWrite`, `FileWrite`, `Path`; shell `Arbitrary`, `Shell`, `Shell`;
webfetch `Arbitrary`, `Always`, `Domain`; agent `Arbitrary`, `Always`, `Name`; skill `ReadOnly`,
`Never`, `Name`, path-free; send_message `Arbitrary`, `Always`, `None`; ask_user_question and
todo_write `ReadOnly`, `Never`, `None`, path-free. Claude aliases as in `claude_tool` today, with
`Agent` listed before `Task`.

### Lookups

`descriptor::find(name) -> Option<&ToolDescriptor>` plus the existing helpers reimplemented over
it: `is_builtin` (row exists), `registered_risk`, and `builtins`/`extensions` iterators for tests.
`BUILTIN_TOOL_NAMES`, `BUILTIN_RISK`, `EXTENSION_RISK`, `builtin_risk`, `extension_risk` go away.
`builtins::risk_of` and `register_extension` call `find(T::NAME)` and keep the `Arbitrary` default
for a missing row.

### Consumers

- `permissions::names_no_path` becomes `find(tool).is_some_and(|d| d.path_free)`.
- `destructive::derive` keeps the MCP check, then matches `find(tool).map(|d| d.destructive)`;
  `None` and `Always` return true.
- `permission_rules`: `PATH_TOOLS` is removed; the path arms test `specifier == Path`.
  `validate_specifier` matches the `Specifier` enum; the `None` arm yields the existing
  "`{tool}` rules take no specifier" error and `Name` the existing "take one name" check. The
  matcher and pattern `match call.tool` arms (`:425`, `:583`) switch to `Specifier` too, so the
  string literals `shell`, `webfetch`, `agent`, `skill` leave that file's tool dispatch.
- `extensions::claude_tool` looks the name up across `claude` aliases; the "not a tool Fontaine
  has" message is built from the table's aliases.

### Registrations

`ExtensionTools` becomes a list of registrations:

```text
pub struct ExtensionTool(Box<dyn FnOnce(&Guard) -> Registered>);
impl ExtensionTool { pub fn new<T: Tool + 'static>(tool: T) -> Self }
pub struct ExtensionTools(pub Vec<ExtensionTool>);
```

`registry` iterates and registers each through the existing `register_extension`. Construction
needs runtime dependencies (agent tree, approver, session), so `agent/build.rs` still builds each
tool; it now pushes `ExtensionTool::new(...)` into a list rather than filling named fields. A
registered extension tool with no descriptor row registers as `Arbitrary` (fail-closed), and the
choke-point test fails on it.

## Implementation Steps

1. `fontaine-tools`: add `descriptor.rs` with the types, `TOOLS` and `find`; rework `names.rs` and
   `risk.rs` to delegate (`is_builtin`, `registered_risk`), delete the old arrays and fns;
   `builtins.rs` `risk_of` uses `find`. Unit tests for the table (below).
2. `fontaine-core`: switch `names_no_path`, `destructive::derive`, `permission_rules`
   (`PATH_TOOLS`, `validate_specifier`, matcher and pattern arms) and `claude_tool`/`map_tool_entry`
   to the descriptor.
3. `fontaine-core`: `ExtensionTools` to a registration list; update `tools.rs`, `agent/build.rs`.
4. Update tests: both `choke_point.rs` files, `tests/docs.rs`, `tests/common/pipeline.rs`,
   `fontaine-evals/src/none.rs` for the removed names.
5. Docs (below), then `cargo fmt --check`, `clippy --all-targets -D warnings`, `cargo test`.

## Files to Modify

- `rust/fontaine-tools/src/{descriptor.rs (new), lib.rs, names.rs, risk.rs, builtins.rs}`
- `rust/fontaine-core/src/{permissions.rs, destructive.rs, permission_rules.rs, extensions.rs, tools.rs, agent/build.rs}`
- `rust/fontaine-core/tests/{choke_point.rs, docs.rs, common/pipeline.rs}`, `rust/fontaine-tools/tests/choke_point.rs`
- `rust/fontaine-evals/src/none.rs`
- `CONTRIBUTING.md` or `docs/tools.md` (see Documentation)

## Trade-offs

- **Table in `fontaine-tools`, not on the `Tool` trait**: a trait method would let a tool declare
  its own class, which the issue forbids. A central table keeps risk reviewed in one place.
- **Policy enums instead of closures or flags only**: `path_free` is a plain flag, but
  destructiveness and specifier handling have real logic that needs core's facts; enums keep
  `fontaine-tools` free of core types while giving exhaustive matches.
- **Registrations as boxed closures**: keeps each tool's concrete type and `T::NAME` link without
  a dyn-compatible wrapper trait; the alternative of building tools from the table would pull
  session dependencies into `fontaine-tools`.
- **Slice instead of fixed-size array**: removes the count that every new tool bumped and conflicted
  on.

## Decisions

- 2026-10-25: `fontaine-evals/src/baseline.rs`'s partial `TOOL_NAMES` stays separate: it is the
  set of tools the Claude Code baseline can drive, not a list of every tool, and merging would
  change eval behavior.
- 2026-10-25: Constructing an extension tool still needs one line in `agent/build.rs` (it needs
  session state); "table row plus the tool's module" is met for everything that does not need
  runtime wiring.

## Documentation

- `docs/tools.md` keeps its inventory row per tool (the docs test now iterates the table).
- Add a short "Adding a tool" note to `CONTRIBUTING.md`: one table row in
  `fontaine-tools/src/descriptor.rs`, the tool module, its construction line, the docs row.
- `tasks/design/permissions.md`: point any mention of the per-list registration at the table, if present.

## Testing Strategy

All unit-level; no live services.

- Table invariants (in `fontaine-tools`): names unique and valid rule names; no row is both
  `path_free` and `Specifier::Path`; every `Destructive::FileWrite`/`Specifier::Path` row is not
  `path_free`; `Never` implies `ReadOnly`.
- Behavior pin: a test in `fontaine-tools` that spells out the full expected row set (name, risk,
  path_free, destructive, specifier, aliases) for the 13 tools, so any change to a decision input
  is a visible diff.
- Existing permission, destructive, permission-rule and extension tests
  (`fontaine-core/tests/{permissions,destructive,permission_rules}.rs`, `extensions.rs` unit
  tests) must pass unchanged; they are the "same decisions for every existing tool" check.
- Choke-point tests: registered names equal the table (builtins from `builtins::build`,
  extensions from the registration list), each registered tool's risk equals its row, MCP tools
  remain `Arbitrary`, a registration without a row is detected.
- `map_tool_entry` error message still names every Claude Code alias (assert it contains each).

## Open Questions

None.
