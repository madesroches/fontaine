mod common;

use std::path::PathBuf;

use common::World;
use fontaine_core::prompt::{self, BASE_PROMPT, InstructionFile, PromptParts};

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
        },
        InstructionFile {
            path: PathBuf::from("/work/project/FONTAINE.md"),
            text: "Run `cargo test` before finishing.\n".to_string(),
        },
    ]
}

#[test]
fn the_assembled_prompt_is_stable() {
    let roots = roots();
    let instructions = instructions();
    let assembled = prompt::assemble(&PromptParts {
        roots: &roots,
        system_prompt: None,
        instructions: &instructions,
        skill_index: Some(
            "Skills you can load with the `skill` tool, each a set of instructions for a kind of task:\n- review: Review a pull request",
        ),
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
        system_prompt: None,
        instructions: &[],
        skill_index: None,
        output_style: None,
        append: None,
    });
    assert!(!assembled.contains("{{workspace_roots}}"));
    let primary = assembled.find("- /work/project").unwrap();
    let shared = assembled.find("- /work/shared").unwrap();
    assert!(primary < shared);
}

#[test]
fn layers_come_in_order_base_then_instructions_then_append() {
    let roots = roots();
    let instructions = instructions();
    let assembled = prompt::assemble(&PromptParts {
        roots: &roots,
        system_prompt: None,
        instructions: &instructions,
        skill_index: None,
        output_style: None,
        append: Some("APPENDED"),
    });
    let base = assembled.find("You are Fontaine").unwrap();
    let user = assembled.find("Always answer in French").unwrap();
    let project = assembled.find("Run `cargo test`").unwrap();
    let appended = assembled.find("APPENDED").unwrap();
    assert!(base < user && user < project && project < appended);

    // The skill list and the output style sit between the instructions and the appended text.
    let layered = prompt::assemble(&PromptParts {
        roots: &roots,
        system_prompt: None,
        instructions: &instructions,
        skill_index: Some("SKILL INDEX"),
        output_style: Some(("terse", "STYLE TEXT")),
        append: Some("APPENDED"),
    });
    let at = |needle: &str| layered.find(needle).unwrap();
    assert!(
        at("You are Fontaine") < at("Always answer in French")
            && at("Run `cargo test`") < at("SKILL INDEX")
            && at("SKILL INDEX") < at("STYLE TEXT")
            && at("STYLE TEXT") < at("APPENDED")
    );
}

#[test]
fn a_replacement_system_prompt_replaces_only_the_base() {
    let roots = roots();
    let instructions = instructions();
    let assembled = prompt::assemble(&PromptParts {
        roots: &roots,
        system_prompt: Some("You are a pirate."),
        instructions: &instructions,
        skill_index: None,
        output_style: None,
        append: Some("APPENDED"),
    });
    assert!(assembled.starts_with("You are a pirate."));
    assert!(!assembled.contains("You are Fontaine"));
    assert!(assembled.contains("Always answer in French"));
    assert!(assembled.contains("Run `cargo test`"));
    assert!(assembled.ends_with("APPENDED"));
}

#[test]
fn instruction_files_load_from_the_user_config_dir_and_the_primary_root_only() {
    let w = World::new();
    common::write(&w.config.join("FONTAINE.md"), "user level\n");
    common::write(&w.work.join("FONTAINE.md"), "project level\n");
    common::write(&w.work.join("sub/FONTAINE.md"), "nested, never read\n");
    common::write(&w.root.join("FONTAINE.md"), "parent, never read\n");
    let loaded = prompt::load_instructions(&w.config, &w.work);
    let texts: Vec<&str> = loaded.iter().map(|f| f.text.trim()).collect();
    assert_eq!(texts, ["user level", "project level"]);
    assert_eq!(loaded[1].path, w.work.join("FONTAINE.md"));
}

#[test]
fn absent_and_blank_instruction_files_are_skipped() {
    let w = World::new();
    assert!(prompt::load_instructions(&w.config, &w.work).is_empty());
    common::write(&w.work.join("FONTAINE.md"), "  \n\n");
    assert!(prompt::load_instructions(&w.config, &w.work).is_empty());
}
