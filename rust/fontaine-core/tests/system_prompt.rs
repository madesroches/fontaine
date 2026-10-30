mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::World;
use fontaine_core::instructions::{self, Origin, Sources};
use fontaine_core::prompt::{self, BASE_PROMPT, InstructionFile, PromptParts};
use fontaine_tools::scope::Scope;

fn roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/work/project"),
        PathBuf::from("/work/shared"),
    ]
}

fn instructions() -> Vec<InstructionFile> {
    vec![
        InstructionFile {
            path: PathBuf::from("/home/me/.config/fontaine/FONTAINE.md"),
            text: "Always answer in French.\n".to_string(),
            origin: Origin::User,
        },
        InstructionFile {
            path: PathBuf::from("/work/project/FONTAINE.md"),
            text: "Run `cargo test` before finishing.\n".to_string(),
            origin: Origin::Walk,
        },
    ]
}

#[test]
fn the_assembled_prompt_is_stable() {
    let roots = roots();
    let instructions = instructions();
    let assembled = prompt::assemble(&PromptParts {
        roots: &roots,
        scratch: None,
        memory_root: None,
        system_prompt: None,
        instructions: &instructions,
        memory: None,
        skill_index: Some(
            "Skills you can load with the `skill` tool, each a set of instructions for a kind of task:\n- review: Review a pull request",
        ),
        scheduling: None,
        output_style: Some(("terse", "Answer in at most two sentences.")),
        append: Some("Keep answers under ten lines."),
    });
    insta::assert_snapshot!(assembled);
}

#[test]
fn the_base_steers_toward_the_dedicated_tools_and_explains_denials_and_secrets() {
    for needle in [
        "`ls`",
        "`read`",
        "`glob`",
        "`grep`",
        "instead of `cat`",
        "Reserve it for what the other tools cannot do",
        "Do not retry a denied call",
        "Never print, quote or repeat secrets",
        "only the user can add a root",
        "may also inspect files outside the roots",
        "never answer a denied `read`, `ls`, `glob` or `grep` by using `shell`",
        "{{workspace_roots}}",
    ] {
        assert!(BASE_PROMPT.contains(needle), "the base lacks `{needle}`");
    }
}

#[test]
fn the_roots_section_lists_every_root_with_the_primary_first() {
    let roots = roots();
    let assembled = prompt::assemble(&PromptParts {
        roots: &roots,
        scratch: None,
        memory_root: None,
        system_prompt: None,
        instructions: &[],
        memory: None,
        skill_index: None,
        scheduling: None,
        output_style: None,
        append: None,
    });
    assert!(!assembled.contains("{{workspace_roots}}"));
    let primary = assembled.find("- /work/project").unwrap();
    let shared = assembled.find("- /work/shared").unwrap();
    assert!(primary < shared);
}

#[test]
fn the_prompt_names_the_scratch_root_and_keeps_the_user_only_rule() {
    let mut roots = roots();
    let scratch = std::path::PathBuf::from("/run/user/1000/fontaine/abc");
    roots.push(scratch.clone());
    let assembled = prompt::assemble(&PromptParts {
        roots: &roots,
        scratch: Some(&scratch),
        memory_root: None,
        system_prompt: None,
        instructions: &[],
        memory: None,
        skill_index: None,
        scheduling: None,
        output_style: None,
        append: None,
    });
    assert!(!assembled.contains("{{"));
    assert!(assembled.contains("- /run/user/1000/fontaine/abc (scratch)"));
    assert!(assembled.contains("The exception is the scratch root /run/user/1000/fontaine/abc"));
    assert!(assembled.contains("only the user can add a root"));
}

#[test]
fn layers_come_in_order_base_then_instructions_then_append() {
    let roots = roots();
    let instructions = instructions();
    let assembled = prompt::assemble(&PromptParts {
        roots: &roots,
        scratch: None,
        memory_root: None,
        system_prompt: None,
        instructions: &instructions,
        memory: None,
        skill_index: None,
        scheduling: None,
        output_style: None,
        append: Some("APPENDED"),
    });
    let base = assembled.find("You are Fontaine").unwrap();
    let user = assembled.find("Always answer in French").unwrap();
    let project = assembled.find("Run `cargo test`").unwrap();
    let appended = assembled.find("APPENDED").unwrap();
    assert!(base < user && user < project && project < appended);

    // The skill list, the scheduling note and the output style sit between the instructions and the appended text.
    let layered = prompt::assemble(&PromptParts {
        roots: &roots,
        scratch: None,
        memory_root: None,
        system_prompt: None,
        instructions: &instructions,
        memory: None,
        skill_index: Some("SKILL INDEX"),
        scheduling: Some("SCHEDULING NOTE"),
        output_style: Some(("terse", "STYLE TEXT")),
        append: Some("APPENDED"),
    });
    let at = |needle: &str| layered.find(needle).unwrap();
    assert!(
        at("You are Fontaine") < at("Always answer in French")
            && at("Run `cargo test`") < at("SKILL INDEX")
            && at("SKILL INDEX") < at("SCHEDULING NOTE")
            && at("SCHEDULING NOTE") < at("STYLE TEXT")
            && at("STYLE TEXT") < at("APPENDED")
    );
}

#[test]
fn a_replacement_system_prompt_replaces_only_the_base() {
    let roots = roots();
    let instructions = instructions();
    let assembled = prompt::assemble(&PromptParts {
        roots: &roots,
        scratch: None,
        memory_root: None,
        system_prompt: Some("You are a pirate."),
        instructions: &instructions,
        memory: None,
        skill_index: None,
        scheduling: None,
        output_style: None,
        append: Some("APPENDED"),
    });
    assert!(assembled.starts_with("You are a pirate."));
    assert!(!assembled.contains("You are Fontaine"));
    assert!(assembled.contains("Always answer in French"));
    assert!(assembled.contains("Run `cargo test`"));
    assert!(assembled.ends_with("APPENDED"));
}

fn sources(w: &World) -> Sources {
    Sources::new(
        w.config.clone(),
        None,
        Arc::new(Scope::new(w.work.clone(), [])),
        Vec::new(),
    )
}

#[test]
fn instruction_files_load_from_the_user_config_dir_the_primary_root_and_its_ancestors() {
    let w = World::new();
    common::write(&w.config.join("FONTAINE.md"), "user level\n");
    common::write(&w.work.join("FONTAINE.md"), "project level\n");
    common::write(&w.work.join("sub/FONTAINE.md"), "nested, read on demand\n");
    common::write(&w.root.join("FONTAINE.md"), "parent level\n");
    let loaded = instructions::load(&sources(&w));
    let texts: Vec<&str> = loaded.files.iter().map(|f| f.text.trim()).collect();
    assert_eq!(texts, ["user level", "parent level", "project level"]);
    assert_eq!(loaded.files[2].path, w.work.join("FONTAINE.md"));
}

#[test]
fn absent_and_blank_instruction_files_are_skipped() {
    let w = World::new();
    assert!(instructions::load(&sources(&w)).files.is_empty());
    common::write(&w.work.join("FONTAINE.md"), "  \n\n");
    assert!(instructions::load(&sources(&w)).files.is_empty());
}

#[test]
fn the_scheduling_note_is_a_layer_that_is_present_only_when_given() {
    let roots = roots();
    let without = prompt::assemble(&PromptParts {
        roots: &roots,
        scratch: None,
        memory_root: None,
        system_prompt: None,
        instructions: &[],
        memory: None,
        skill_index: None,
        scheduling: None,
        output_style: None,
        append: None,
    });
    assert!(!without.contains("cron_create") && !without.contains("schedule_wakeup"));
    let with = prompt::assemble(&PromptParts {
        roots: &roots,
        scratch: None,
        memory_root: None,
        system_prompt: None,
        instructions: &[],
        memory: None,
        skill_index: None,
        scheduling: Some(prompt::SCHEDULING_PROMPT),
        output_style: None,
        append: None,
    });
    assert!(with.starts_with(&without));
    for needle in ["cron_create", "harness note", "schedule_wakeup"] {
        assert!(with.contains(needle), "the note lacks `{needle}`");
    }
}

#[test]
fn the_memory_layer_follows_the_instruction_files_and_labels_its_root() {
    let mut roots = roots();
    let memory_root = PathBuf::from("/data/memory/project-1234abcd");
    roots.push(memory_root.clone());
    let instructions = instructions();
    let layer = fontaine_core::memory::layer(&memory_root, Some("- [cmds](commands.md)"));
    let assembled = prompt::assemble(&PromptParts {
        roots: &roots,
        scratch: None,
        memory_root: Some(&memory_root),
        system_prompt: None,
        instructions: &instructions,
        memory: Some(&layer),
        skill_index: Some("skills"),
        scheduling: None,
        output_style: None,
        append: None,
    });
    assert!(assembled.contains("- /data/memory/project-1234abcd (memory)"));
    assert!(assembled.contains("not instructions"));
    assert!(assembled.contains("Memory directory: /data/memory/project-1234abcd"));
    assert!(assembled.contains("- [cmds](commands.md)"));
    let at = |needle: &str| assembled.find(needle).unwrap();
    assert!(at("Run `cargo test` before finishing.") < at("These are notes you saved"));
    assert!(at("These are notes you saved") < at("skills"));
}
