//! The system prompt, assembled in layers: the base, `FONTAINE.md` instructions, the skill
//! index, the output style, then `--append-system-prompt`. `--system-prompt` replaces the base
//! layer only.

use std::path::{Path, PathBuf};

pub const BASE_PROMPT: &str = include_str!("../prompts/system_prompt.md");

pub const INSTRUCTIONS_FILE: &str = "FONTAINE.md";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstructionFile {
    pub path: PathBuf,
    pub text: String,
}

/// The instruction files that apply: the user-level one, then the primary root's. Absent and
/// unreadable files are skipped; there is no parent-directory walk and no imports.
pub fn load_instructions(config_dir: &Path, primary_root: &Path) -> Vec<InstructionFile> {
    [config_dir, primary_root]
        .iter()
        .filter_map(|dir| {
            let path = dir.join(INSTRUCTIONS_FILE);
            let text = std::fs::read_to_string(&path).ok()?;
            (!text.trim().is_empty()).then_some(InstructionFile { path, text })
        })
        .collect()
}

pub struct PromptParts<'a> {
    pub roots: &'a [PathBuf],
    /// Replaces the base layer when set.
    pub system_prompt: Option<&'a str>,
    pub instructions: &'a [InstructionFile],
    pub append: Option<&'a str>,
}

fn base(roots: &[PathBuf]) -> String {
    let list = roots
        .iter()
        .map(|root| format!("- {}", root.display()))
        .collect::<Vec<_>>()
        .join("\n");
    BASE_PROMPT
        .replace("{{workspace_roots}}", &list)
        .trim_end()
        .to_string()
}

pub fn assemble(parts: &PromptParts<'_>) -> String {
    let mut layers = vec![match parts.system_prompt {
        Some(replacement) => replacement.to_string(),
        None => base(parts.roots),
    }];
    for file in parts.instructions {
        layers.push(format!(
            "Instructions from {}:\n\n{}",
            file.path.display(),
            file.text.trim_end()
        ));
    }
    if let Some(append) = parts.append {
        layers.push(append.to_string());
    }
    layers.join("\n\n")
}
