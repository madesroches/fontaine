//! The confirmation before `/plugin install` and `/plugin update` record anything: the plugin is
//! third-party code that will run hooks and servers on this machine, so the dialog shows the whole
//! executable surface the approval covers.

use crate::confirm_prompt::{Block, ConfirmPrompt};

/// Rows of the summary the dialog shows before it points at the transcript, where the command
/// has already printed all of it.
const SHOWN_ROWS: usize = 24;

/// What the approval is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Install,
    Update(Why),
}

/// Why an update needs approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    Differs,
    NoRecordedApproval,
    NoFingerprint,
}

/// `summary` is the plugin's executable surface and `fingerprint` the hash the approval records.
/// `Some(true)` from the prompt applies the change, `Some(false)` drops it.
pub fn new(
    change: Change,
    plugin: &str,
    summary: &[String],
    fingerprint: Option<&str>,
) -> ConfirmPrompt {
    let heading = match change {
        Change::Install => format!("Install plugin `{plugin}` and let it run what is listed?"),
        Change::Update(why) => {
            let reason = match why {
                Why::Differs => "Its executable surface differs from what you approved.",
                Why::NoRecordedApproval => "No previous approval is recorded for it.",
                Why::NoFingerprint => "Its executable surface could not be compared.",
            };
            format!("Update plugin `{plugin}`? {reason}")
        }
    };
    let mut blocks: Vec<Block> = summary.iter().cloned().map(Block::Text).collect();
    blocks.push(Block::Text(String::new()));
    blocks.push(Block::Text(match fingerprint {
        Some(fingerprint) => format!("fingerprint: {fingerprint}"),
        None => "no fingerprint could be computed, so this plugin will not auto-update".to_string(),
    }));
    ConfirmPrompt::new(heading, blocks, "y approves · n cancels").with_max_rows(SHOWN_ROWS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn text(prompt: &ConfirmPrompt) -> String {
        prompt
            .body(100, &Theme::preset(Default::default()))
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn an_install_names_the_plugin_lists_the_surface_and_shows_the_fingerprint() {
        let prompt = new(
            Change::Install,
            "demo",
            &[
                "plugin `demo` brings 1 skill(s)".to_string(),
                "  hook pre_tool_use: ${CLAUDE_PLUGIN_ROOT}/check.sh".to_string(),
            ],
            Some("sha256:abc"),
        );
        let body = text(&prompt);
        assert!(body.contains("Install plugin `demo`"), "{body}");
        assert!(body.contains("hook pre_tool_use"), "{body}");
        assert!(body.contains("fingerprint: sha256:abc"), "{body}");
    }

    #[test]
    fn an_update_says_why_it_asks_and_a_missing_fingerprint_says_so() {
        let body = text(&new(Change::Update(Why::Differs), "demo", &[], None));
        assert!(body.contains("Update plugin `demo`"), "{body}");
        assert!(body.contains("differs from what you approved"), "{body}");
        assert!(body.contains("will not auto-update"), "{body}");
        let body = text(&new(
            Change::Update(Why::NoRecordedApproval),
            "demo",
            &[],
            Some("f"),
        ));
        assert!(body.contains("No previous approval is recorded"), "{body}");
        let body = text(&new(Change::Update(Why::NoFingerprint), "demo", &[], None));
        assert!(body.contains("could not be compared"), "{body}");
    }

    #[test]
    fn a_long_surface_is_cut_with_a_pointer_to_the_transcript() {
        let lines: Vec<String> = (0..60).map(|n| format!("  hook {n}")).collect();
        let body = text(&new(Change::Install, "demo", &lines, Some("f")));
        assert!(body.contains("more lines"), "{body}");
    }
}
