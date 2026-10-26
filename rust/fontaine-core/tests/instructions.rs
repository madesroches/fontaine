mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{World, write};
use fontaine_core::instructions::{
    self, InstructionFile, InstructionStage, Loaded, MemoryKind, Origin, Sources,
};
use fontaine_core::permission_rules::parse_rule;
use fontaine_core::permissions::{Answer, ApprovalRequest, Approver};
use fontaine_tools::gate::{CallView, ROOT_AGENT};
use fontaine_tools::result_pipeline::{ResultPipeline, ResultStage, StageKind, ToolOutcome};
use fontaine_tools::risk::RiskClass;
use fontaine_tools::scope::Scope;
use futures::future::BoxFuture;
use rig_core::tool::{ToolExecutionError, ToolOutput};
use serde_json::{Value, json};

fn sources_with(w: &World, home: Option<&Path>, roots: &[PathBuf], deny: &[&str]) -> Sources {
    Sources::new(
        w.config.clone(),
        home.map(Path::to_path_buf),
        Arc::new(Scope::new(w.work.clone(), roots.iter().cloned())),
        deny.iter().map(|d| parse_rule(d).unwrap()).collect(),
    )
}

fn sources(w: &World) -> Sources {
    sources_with(w, None, &[], &[])
}

/// What was loaded from inside the scratch world, trimmed: ancestors of the temp directory
/// are the machine's, not the test's.
fn texts(w: &World, loaded: &Loaded) -> Vec<String> {
    loaded
        .files
        .iter()
        .filter(|f| f.path.starts_with(&w.root))
        .map(|f| f.text.trim().to_string())
        .collect()
}

fn warned(loaded: &Loaded, needle: &str) -> bool {
    loaded.warnings.iter().any(|w| w.contains(needle))
}

#[test]
fn the_load_order_is_user_then_ancestors_down_to_the_root_with_claude_before_fontaine() {
    let w = World::new();
    let home = w.root.join("home");
    write(&home.join(".claude/CLAUDE.md"), "u-claude");
    write(&home.join(".claude/rules/a.md"), "u-rule");
    write(&w.config.join("FONTAINE.md"), "u-fontaine");
    write(&w.config.join("rules/b.md"), "u-fontaine-rule");
    write(&w.root.join("CLAUDE.md"), "anc-claude");
    write(&w.root.join("FONTAINE.md"), "anc-fontaine");
    write(&w.work.join("CLAUDE.md"), "root-claude");
    write(&w.work.join(".claude/CLAUDE.md"), "root-dot-claude");
    write(&w.work.join("FONTAINE.md"), "root-fontaine");
    write(&w.work.join(".fontaine/FONTAINE.md"), "root-dot-fontaine");
    write(&w.work.join(".claude/rules/r.md"), "root-claude-rule");
    write(&w.work.join(".fontaine/rules/r.md"), "root-fontaine-rule");
    write(&w.work.join("CLAUDE.local.md"), "root-claude-local");
    write(&w.work.join("FONTAINE.local.md"), "root-fontaine-local");
    let loaded = instructions::load(&sources_with(&w, Some(&home), &[], &[]));
    assert_eq!(
        texts(&w, &loaded),
        [
            "u-claude",
            "u-rule",
            "u-fontaine",
            "u-fontaine-rule",
            "anc-claude",
            "anc-fontaine",
            "root-claude",
            "root-dot-claude",
            "root-fontaine",
            "root-dot-fontaine",
            "root-claude-rule",
            "root-fontaine-rule",
            "root-claude-local",
            "root-fontaine-local",
        ]
    );
    assert!(!loaded.agents_mode);
}

#[test]
fn agents_mode_is_off_when_any_counting_name_exists_and_on_otherwise() {
    for (counting, at_root) in [
        ("CLAUDE.md", true),
        (".claude/CLAUDE.md", true),
        ("CLAUDE.local.md", true),
        ("FONTAINE.md", true),
        (".fontaine/FONTAINE.md", true),
        ("FONTAINE.local.md", true),
        ("CLAUDE.md", false),
    ] {
        let w = World::new();
        write(&w.work.join("AGENTS.md"), "agents-root");
        write(&w.work.join(".claude/AGENTS.md"), "agents-dot-claude");
        let on = instructions::load(&sources(&w));
        assert!(on.agents_mode);
        assert_eq!(texts(&w, &on), ["agents-root", "agents-dot-claude"]);

        let dir = if at_root { &w.work } else { &w.root };
        write(&dir.join(counting), "counting");
        let off = instructions::load(&sources(&w));
        assert!(!off.agents_mode, "{counting} should turn AGENTS mode off");
        assert!(
            !texts(&w, &off).iter().any(|t| t.starts_with("agents")),
            "{counting}"
        );
    }
}

#[test]
fn user_files_do_not_count_toward_agents_mode() {
    let w = World::new();
    write(&w.config.join("FONTAINE.md"), "user");
    write(&w.work.join("AGENTS.md"), "agents-root");
    let loaded = instructions::load(&sources(&w));
    assert!(loaded.agents_mode);
    assert_eq!(texts(&w, &loaded), ["user", "agents-root"]);
}

#[test]
fn a_deny_rule_on_read_keeps_a_file_out() {
    let w = World::new();
    write(&w.work.join("CLAUDE.md"), "kept");
    write(&w.work.join("CLAUDE.local.md"), "private");
    let loaded = instructions::load(&sources_with(&w, None, &[], &["read(CLAUDE.local.md)"]));
    assert_eq!(texts(&w, &loaded), ["kept"]);
    assert!(warned(&loaded, "CLAUDE.local.md"));
    assert!(warned(&loaded, "deny rule"));
}

#[test]
fn a_deny_rule_covers_the_user_tier_and_imports() {
    let w = World::new();
    write(&w.config.join("FONTAINE.md"), "user");
    write(&w.work.join("CLAUDE.md"), "root\n@secrets/x.md\n@ok.md");
    write(&w.work.join("secrets/x.md"), "secret");
    write(&w.work.join("ok.md"), "fine");
    let deny = &["read(secrets/**)", "read(//nonexistent)"];
    let loaded = instructions::load(&sources_with(&w, None, &[], deny));
    assert_eq!(
        texts(&w, &loaded),
        ["user", "root\n@secrets/x.md\n@ok.md", "fine"]
    );
    assert!(warned(&loaded, "secrets/x.md"));
    let denied_user = instructions::load(&sources_with(
        &w,
        None,
        &[],
        &[&format!(
            "read({}/**)",
            w.config.display().to_string().replacen('/', "//", 1)
        )],
    ));
    assert!(!texts(&w, &denied_user).contains(&"user".to_string()));
}

#[test]
fn claude_importing_agents_loads_it_once() {
    let w = World::new();
    write(&w.work.join("CLAUDE.md"), "@AGENTS.md\nclaude");
    write(&w.work.join("AGENTS.md"), "agents");
    let loaded = instructions::load(&sources(&w));
    assert_eq!(texts(&w, &loaded), ["@AGENTS.md\nclaude", "agents"]);
    assert_eq!(
        loaded.files[loaded.files.len() - 1].origin,
        Origin::Import {
            from: w.work.join("CLAUDE.md")
        }
    );
}

#[test]
fn identical_text_loads_once_and_a_symlink_to_a_loaded_file_is_a_repeat() {
    let w = World::new();
    write(&w.work.join("CLAUDE.md"), "same text\n");
    write(&w.work.join("FONTAINE.md"), "same text\n\n");
    write(&w.work.join("AGENTS.md"), "other");
    #[cfg(unix)]
    std::os::unix::fs::symlink(w.work.join("CLAUDE.md"), w.work.join("CLAUDE.local.md")).unwrap();
    let loaded = instructions::load(&sources(&w));
    assert_eq!(texts(&w, &loaded), ["same text"]);
}

#[test]
fn the_user_claude_file_is_met_once_when_the_walk_crosses_home() {
    let w = World::new();
    write(&w.root.join(".claude/CLAUDE.md"), "home-level");
    write(&w.work.join("CLAUDE.md"), "project");
    let loaded = instructions::load(&sources_with(&w, Some(&w.root), &[], &[]));
    assert_eq!(texts(&w, &loaded), ["home-level", "project"]);
    assert_eq!(loaded.files[0].origin, Origin::User);
}

#[test]
fn add_dir_roots_are_not_walked() {
    let w = World::new();
    let extra = w.root.join("extra");
    write(&extra.join("CLAUDE.md"), "extra");
    write(&w.work.join("CLAUDE.md"), "project");
    let loaded = instructions::load(&sources_with(&w, None, &[extra], &[]));
    assert_eq!(texts(&w, &loaded), ["project"]);
}

#[test]
fn blank_files_are_dropped_and_comments_are_stripped() {
    let w = World::new();
    write(&w.work.join("CLAUDE.md"), "  \n<!-- only a comment -->\n");
    write(&w.work.join("FONTAINE.md"), "<!-- gone -->\nkept\n");
    let loaded = instructions::load(&sources(&w));
    assert_eq!(texts(&w, &loaded), ["kept"]);
}

#[test]
fn imports_resolve_relative_to_the_importer_absolute_and_from_home() {
    let w = World::new();
    let home = w.root.join("home");
    write(&home.join("mine.md"), "from-home");
    write(&w.work.join("docs/a.md"), "a\n@b.md");
    write(&w.work.join("docs/b.md"), "b");
    write(&w.work.join("abs.md"), "abs");
    write(
        &w.config.join("FONTAINE.md"),
        &format!("user\n@~/mine.md\n@{}", w.work.join("abs.md").display()),
    );
    write(
        &w.work.join("CLAUDE.md"),
        "root\n@docs/a.md\n@missing.md\nmail me@example.com",
    );
    let loaded = instructions::load(&sources_with(&w, Some(&home), &[], &[]));
    let t = texts(&w, &loaded);
    // The absolute import in the user file loads first; the project file's walk sees it as seen.
    assert_eq!(t[1], "from-home");
    assert_eq!(t[2], "abs");
    assert_eq!(
        &t[3..],
        [
            "root\n@docs/a.md\n@missing.md\nmail me@example.com",
            "a\n@b.md",
            "b"
        ]
    );
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
}

#[test]
fn imports_stop_at_four_hops_and_cycles_load_each_file_once() {
    let w = World::new();
    write(&w.work.join("CLAUDE.md"), "h0\n@1.md");
    for hop in 1..=5 {
        let next = if hop == 5 {
            "@CLAUDE.md".to_string()
        } else {
            format!("@{}.md", hop + 1)
        };
        write(
            &w.work.join(format!("{hop}.md")),
            &format!("h{hop}\n{next}"),
        );
    }
    let loaded = instructions::load(&sources(&w));
    let heads: Vec<String> = texts(&w, &loaded)
        .iter()
        .map(|t| t.lines().next().unwrap().to_string())
        .collect();
    assert_eq!(heads, ["h0", "h1", "h2", "h3", "h4"]);
    assert!(warned(&loaded, "5.md"));
    assert!(warned(&loaded, "four levels"));
}

#[test]
fn an_import_cycle_loads_each_file_once() {
    let w = World::new();
    write(&w.work.join("CLAUDE.md"), "root\n@a.md");
    write(&w.work.join("a.md"), "a\n@CLAUDE.md\n@a.md");
    let loaded = instructions::load(&sources(&w));
    assert_eq!(texts(&w, &loaded).len(), 2);
    assert!(loaded.warnings.is_empty());
}

#[test]
fn repo_files_cannot_import_outside_the_workspace() {
    let w = World::new();
    write(&w.root.join("outside.md"), "outside");
    write(&w.work.join("CLAUDE.md"), "root\n@../outside.md");
    let loaded = instructions::load(&sources(&w));
    assert_eq!(texts(&w, &loaded), ["root\n@../outside.md"]);
    assert!(warned(&loaded, "outside.md"));
    assert!(warned(&loaded, "imported by"));
}

#[test]
fn a_user_tier_import_outside_the_workspace_is_allowed() {
    let w = World::new();
    write(&w.root.join("outside.md"), "outside");
    write(&w.config.join("FONTAINE.md"), "user\n@../outside.md");
    let loaded = instructions::load(&sources(&w));
    assert_eq!(texts(&w, &loaded), ["user\n@../outside.md", "outside"]);
}

#[test]
fn a_file_above_the_work_tree_loads_but_its_outside_imports_do_not() {
    let w = World::new();
    write(
        &w.root.join("CLAUDE.md"),
        "ancestor\n@sibling.md\n@work/inside.md",
    );
    write(&w.root.join("sibling.md"), "sibling");
    write(&w.work.join("inside.md"), "inside");
    let loaded = instructions::load(&sources(&w));
    assert_eq!(
        texts(&w, &loaded),
        ["ancestor\n@sibling.md\n@work/inside.md", "inside"]
    );
    assert!(warned(&loaded, "sibling.md"));
}

#[cfg(unix)]
#[test]
fn a_repo_file_that_links_outside_is_refused() {
    let w = World::new();
    write(&w.root.join("secret"), "TOP-SECRET");
    std::os::unix::fs::symlink(w.root.join("secret"), w.work.join("CLAUDE.md")).unwrap();
    let loaded = instructions::load(&sources(&w));
    assert!(texts(&w, &loaded).is_empty());
    assert!(warned(&loaded, "resolves outside"));
}

#[cfg(unix)]
#[test]
fn a_symlinked_rules_directory_pointing_outside_is_refused_per_file() {
    let w = World::new();
    write(&w.root.join("shared-rules/x.md"), "leaked");
    std::fs::create_dir_all(w.work.join(".claude")).unwrap();
    std::os::unix::fs::symlink(w.root.join("shared-rules"), w.work.join(".claude/rules")).unwrap();
    let loaded = instructions::load(&sources(&w));
    assert!(texts(&w, &loaded).is_empty());
    assert!(warned(&loaded, "x.md"));
}

#[test]
fn a_file_that_is_not_utf8_is_skipped_with_a_warning() {
    let w = World::new();
    std::fs::write(w.work.join("CLAUDE.md"), [0xff, 0xfe, 0x00]).unwrap();
    let loaded = instructions::load(&sources(&w));
    assert!(texts(&w, &loaded).is_empty());
    assert!(warned(&loaded, "not UTF-8"));
}

#[test]
fn rules_load_in_path_order_and_path_scoped_ones_wait() {
    let w = World::new();
    write(&w.work.join(".claude/rules/z.md"), "z");
    write(
        &w.work.join(".claude/rules/a/b.md"),
        "---\npaths: x/**\n---\nscoped",
    );
    write(
        &w.work.join(".claude/rules/a.md"),
        "---\ndescription: d\n---\nplain",
    );
    let loaded = instructions::load(&sources(&w));
    assert_eq!(texts(&w, &loaded), ["plain", "z"]);
    assert_eq!(loaded.path_rules.len(), 1);
    assert_eq!(loaded.path_rules[0].base, w.work);
    assert!(loaded.path_rules[0].matches(&w.work.join("x/y/z.rs")));
}

#[test]
fn a_user_path_rule_matches_relative_to_the_primary_root_even_under_home() {
    let w = World::new();
    write(
        &w.root.join(".claude/rules/r.md"),
        "---\npaths: src/**\n---\nscoped",
    );
    let loaded = instructions::load(&sources_with(&w, Some(&w.root), &[], &[]));
    assert_eq!(loaded.path_rules.len(), 1);
    assert_eq!(loaded.path_rules[0].base, w.work);
    assert!(loaded.path_rules[0].matches(&w.work.join("src/a.rs")));
    assert!(!loaded.path_rules[0].matches(&w.root.join("src/a.rs")));
}

#[test]
fn a_large_launch_set_warns_with_the_largest_files() {
    let w = World::new();
    write(&w.work.join("CLAUDE.md"), &"x".repeat(41_000));
    write(&w.work.join("FONTAINE.md"), "small");
    let loaded = instructions::load(&sources(&w));
    assert!(warned(&loaded, "CLAUDE.md"));
    assert!(warned(&loaded, "41000 characters"));
}

#[test]
fn the_guardian_order_puts_the_most_specific_first_and_the_cap_keeps_the_root_file() {
    let w = World::new();
    write(&w.config.join("FONTAINE.md"), "user");
    write(&w.root.join("CLAUDE.md"), "ancestor");
    write(&w.work.join("CLAUDE.md"), "root\n@imp.md");
    write(&w.work.join("imp.md"), "imported");
    write(&w.work.join(".claude/rules/r.md"), "rule");
    write(&w.work.join("CLAUDE.local.md"), "local");
    let loaded = instructions::load(&sources(&w));
    let ordered: Vec<String> = instructions::guardian_order(&loaded.files)
        .iter()
        .filter(|f| f.path.starts_with(&w.root))
        .map(|f| f.text.trim().to_string())
        .collect();
    assert_eq!(
        ordered,
        [
            "root\n@imp.md",
            "imported",
            "rule",
            "local",
            "ancestor",
            "user"
        ]
    );
    let big = vec![
        InstructionFile {
            path: w.work.join("CLAUDE.md"),
            text: "ROOT-FILE".to_string(),
            origin: Origin::Walk,
        },
        InstructionFile {
            path: w.root.join("CLAUDE.md"),
            text: "x".repeat(20_000),
            origin: Origin::Walk,
        },
    ];
    let text =
        fontaine_core::adjudication::project_instructions(&instructions::guardian_order(&big))
            .unwrap();
    assert!(text.contains("ROOT-FILE"));
    assert_eq!(text.chars().count(), 16_000);
}

#[test]
fn init_picks_an_existing_file_before_creating_fontaine_md() {
    let w = World::new();
    assert_eq!(
        instructions::init_target(&w.work),
        (w.work.join("FONTAINE.md"), false)
    );
    write(&w.work.join("AGENTS.md"), "a");
    assert_eq!(
        instructions::init_target(&w.work),
        (w.work.join("AGENTS.md"), true)
    );
    write(&w.work.join(".claude/CLAUDE.md"), "c");
    assert_eq!(
        instructions::init_target(&w.work),
        (w.work.join(".claude/CLAUDE.md"), true)
    );
    write(&w.work.join("CLAUDE.md"), "c");
    assert_eq!(
        instructions::init_target(&w.work),
        (w.work.join("CLAUDE.md"), true)
    );
    write(&w.work.join("FONTAINE.md"), "f");
    assert_eq!(
        instructions::init_target(&w.work),
        (w.work.join("FONTAINE.md"), true)
    );
}

#[test]
fn memory_entries_mark_the_default_files_that_do_not_exist() {
    let w = World::new();
    write(&w.work.join("CLAUDE.md"), "c");
    let loaded = instructions::load(&sources(&w));
    let entries =
        instructions::memory_entries(&loaded, &[w.work.join("sub/CLAUDE.md")], &w.config, &w.work);
    let kind = |path: PathBuf| {
        entries
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.kind.clone())
    };
    assert!(matches!(
        kind(w.work.join("CLAUDE.md")),
        Some(MemoryKind::Launch(Origin::Walk))
    ));
    assert_eq!(
        kind(w.work.join("sub/CLAUDE.md")),
        Some(MemoryKind::OnDemand)
    );
    assert_eq!(
        kind(w.config.join("FONTAINE.md")),
        Some(MemoryKind::NotCreated)
    );
    assert_eq!(
        kind(w.work.join("FONTAINE.md")),
        Some(MemoryKind::NotCreated)
    );
}

struct Quiet;
impl Approver for Quiet {
    fn approve<'a>(&'a self, _: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }
    fn warn(&self, _: &str) {}
}

fn stage(w: &World, deny: &[&str]) -> (Arc<InstructionStage>, Loaded) {
    let sources = sources_with(w, None, &[], deny);
    let loaded = instructions::load(&sources);
    let stage = Arc::new(InstructionStage::new(sources.clone(), Arc::new(Quiet)));
    stage.reseed(sources, &loaded);
    (stage, loaded)
}

async fn call(
    stage: &InstructionStage,
    agent: &str,
    tool: &str,
    path: &Path,
    outcome: ToolOutcome,
) -> String {
    let args: Value = json!({ "path": path.to_string_lossy() });
    let view = CallView {
        tool,
        risk: RiskClass::ReadOnly,
        args: &args,
        agent,
        annotations: None,
        requires_user_interaction: false,
    };
    match stage.process(view, outcome).await {
        Ok(output) => output.render(),
        Err(error) => error.model_output().render(),
    }
}

fn ok(text: &str) -> ToolOutcome {
    Ok(ToolOutput::text(text))
}

#[tokio::test]
async fn a_read_under_a_subdirectory_injects_its_instructions_once() {
    let w = World::new();
    write(&w.work.join("sub/CLAUDE.md"), "SUB-MARKER");
    write(&w.work.join("sub/f.txt"), "body");
    let (stage, _) = stage(&w, &[]);
    let first = call(
        &stage,
        ROOT_AGENT,
        "read",
        &w.work.join("sub/f.txt"),
        ok("body"),
    )
    .await;
    assert!(first.starts_with("body"));
    assert!(first.contains("[Harness note: instruction files that apply to"));
    assert!(first.contains("SUB-MARKER"));
    assert!(first.contains(&format!(
        "Instructions from {}",
        w.work.join("sub/CLAUDE.md").display()
    )));
    let again = call(
        &stage,
        ROOT_AGENT,
        "read",
        &w.work.join("sub/f.txt"),
        ok("body"),
    )
    .await;
    assert_eq!(again, "body");
    assert_eq!(stage.loaded_on_demand(), [w.work.join("sub/CLAUDE.md")]);
}

#[tokio::test]
async fn write_and_edit_trigger_it_and_a_failed_call_or_other_tool_does_not() {
    let w = World::new();
    write(&w.work.join("sub/CLAUDE.md"), "SUB-MARKER");
    let (stage, _) = stage(&w, &[]);
    let target = w.work.join("sub/f.txt");
    let failed: ToolOutcome = Err(ToolExecutionError::refused("no".to_string()));
    assert!(
        !call(&stage, ROOT_AGENT, "read", &target, failed)
            .await
            .contains("SUB-MARKER")
    );
    assert_eq!(
        call(&stage, ROOT_AGENT, "grep", &target, ok("x")).await,
        "x"
    );
    assert!(
        call(&stage, ROOT_AGENT, "edit", &target, ok("x"))
            .await
            .contains("SUB-MARKER")
    );
}

#[tokio::test]
async fn path_rules_inject_when_they_match_and_not_otherwise() {
    let w = World::new();
    write(
        &w.work.join(".claude/rules/ts.md"),
        "---\npaths:\n  - src/**/*.ts\n---\nTS-RULE",
    );
    write(
        &w.work.join("nested/.claude/rules/n.md"),
        "---\npaths: \"*.py\"\n---\nPY-RULE",
    );
    let (stage, _) = stage(&w, &[]);
    let hit = call(
        &stage,
        ROOT_AGENT,
        "read",
        &w.work.join("src/a/b.ts"),
        ok("x"),
    )
    .await;
    assert!(hit.contains("TS-RULE"));
    let miss = call(
        &stage,
        ROOT_AGENT,
        "read",
        &w.work.join("src/a.rs"),
        ok("x"),
    )
    .await;
    assert_eq!(miss, "x");
    let nested = call(
        &stage,
        ROOT_AGENT,
        "read",
        &w.work.join("nested/m.py"),
        ok("x"),
    )
    .await;
    assert!(nested.contains("PY-RULE"));
    let other = call(
        &stage,
        ROOT_AGENT,
        "read",
        &w.work.join("nested/deep/m.py"),
        ok("x"),
    )
    .await;
    assert_eq!(other, "x");
}

#[tokio::test]
async fn a_path_outside_the_primary_root_injects_nothing() {
    let w = World::new();
    write(&w.root.join("other/CLAUDE.md"), "OTHER");
    write(&w.root.join("CLAUDE.md"), "ANCESTOR");
    let extra = w.root.join("other");
    let sources = sources_with(&w, None, std::slice::from_ref(&extra), &[]);
    let loaded = instructions::load(&sources);
    let stage = InstructionStage::new(sources.clone(), Arc::new(Quiet));
    stage.reseed(sources, &loaded);
    assert_eq!(
        call(&stage, ROOT_AGENT, "read", &extra.join("f.txt"), ok("x")).await,
        "x"
    );
}

#[tokio::test]
async fn two_agents_each_get_their_own_first_load_and_a_reset_loads_again() {
    let w = World::new();
    write(&w.work.join("sub/CLAUDE.md"), "SUB-MARKER");
    let (stage, _) = stage(&w, &[]);
    let target = w.work.join("sub/f.txt");
    assert!(
        call(&stage, "main", "read", &target, ok("x"))
            .await
            .contains("SUB-MARKER")
    );
    assert!(
        call(&stage, "child", "read", &target, ok("x"))
            .await
            .contains("SUB-MARKER")
    );
    assert_eq!(call(&stage, "child", "read", &target, ok("x")).await, "x");
    assert!(
        call(&stage, ROOT_AGENT, "read", &target, ok("x"))
            .await
            .contains("SUB-MARKER")
    );
    stage.reset_main();
    assert!(stage.loaded_on_demand().is_empty());
    assert!(
        call(&stage, ROOT_AGENT, "read", &target, ok("x"))
            .await
            .contains("SUB-MARKER")
    );
    assert_eq!(call(&stage, "child", "read", &target, ok("x")).await, "x");
}

#[tokio::test]
async fn a_reseed_keeps_what_an_agent_already_received() {
    let w = World::new();
    write(&w.work.join("sub/CLAUDE.md"), "SUB-MARKER");
    let (stage, loaded) = stage(&w, &[]);
    let target = w.work.join("sub/f.txt");
    assert!(
        call(&stage, ROOT_AGENT, "read", &target, ok("x"))
            .await
            .contains("SUB-MARKER")
    );
    stage.reseed(sources(&w), &loaded);
    assert_eq!(
        call(&stage, ROOT_AGENT, "read", &target, ok("x")).await,
        "x"
    );
}

#[tokio::test]
async fn agents_files_load_on_demand_only_in_agents_mode_and_without_a_counting_file() {
    let w = World::new();
    write(&w.work.join("a/AGENTS.md"), "AGENTS-A");
    write(&w.work.join("b/AGENTS.md"), "AGENTS-B");
    write(&w.work.join("b/CLAUDE.md"), "CLAUDE-B");
    let (stage, loaded) = stage(&w, &[]);
    assert!(loaded.agents_mode);
    assert!(
        call(&stage, ROOT_AGENT, "read", &w.work.join("a/f"), ok("x"))
            .await
            .contains("AGENTS-A")
    );
    let b = call(&stage, ROOT_AGENT, "read", &w.work.join("b/f"), ok("x")).await;
    assert!(b.contains("CLAUDE-B") && !b.contains("AGENTS-B"));

    let w2 = World::new();
    write(&w2.work.join("CLAUDE.md"), "root");
    write(&w2.work.join("a/AGENTS.md"), "AGENTS-A");
    let (stage2, loaded2) = self::stage(&w2, &[]);
    assert!(!loaded2.agents_mode);
    assert_eq!(
        call(&stage2, ROOT_AGENT, "read", &w2.work.join("a/f"), ok("x")).await,
        "x"
    );
}

#[tokio::test]
async fn deny_rules_and_confinement_apply_to_on_demand_files() {
    let w = World::new();
    write(&w.work.join("sub/CLAUDE.md"), "SUB");
    let (stage, _) = stage(&w, &["read(sub/CLAUDE.md)"]);
    assert_eq!(
        call(&stage, ROOT_AGENT, "read", &w.work.join("sub/f"), ok("x")).await,
        "x"
    );
}

struct Recorder(&'static str, Arc<std::sync::Mutex<Vec<&'static str>>>);
impl ResultStage for Recorder {
    fn process<'a>(&'a self, _: CallView<'a>, outcome: ToolOutcome) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.1.lock().unwrap().push(self.0);
            outcome
        })
    }
}

#[tokio::test]
async fn the_instruction_stage_runs_before_the_scanner_and_after_notifications() {
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let pipeline = ResultPipeline::new()
        .with_stage(
            StageKind::PostToolUse,
            Arc::new(Recorder("post", order.clone())),
        )
        .with_stage(
            StageKind::Scanner,
            Arc::new(Recorder("scanner", order.clone())),
        )
        .with_stage(
            StageKind::Instructions,
            Arc::new(Recorder("instructions", order.clone())),
        )
        .with_stage(
            StageKind::Notifications,
            Arc::new(Recorder("notifications", order.clone())),
        );
    let args = json!({});
    let view = CallView {
        tool: "read",
        risk: RiskClass::ReadOnly,
        args: &args,
        agent: ROOT_AGENT,
        annotations: None,
        requires_user_interaction: false,
    };
    let _ = pipeline.run(view, ok("x")).await;
    assert_eq!(
        *order.lock().unwrap(),
        ["notifications", "instructions", "scanner", "post"]
    );
}
