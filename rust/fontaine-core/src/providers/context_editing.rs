//! The `clear_tool_uses` edit an `anthropic` account sends when `context_editing` is enabled.

use serde_json::{Value, json};

use crate::config::ContextEditing;

const EDIT_TYPE: &str = "clear_tool_uses_20250919";
const DEFAULT_TRIGGER_TOKENS: u32 = 100_000;
const DEFAULT_KEEP: u32 = 3;
const DEFAULT_CLEAR_AT_LEAST_TOKENS: u32 = 20_000;

/// Tools whose results carry state the model reads back later, so clearing them would lose it.
const DEFAULT_EXCLUDE_TOOLS: [&str; 4] = ["board_show", "board_update", "memory", "todo_write"];

/// The trigger, the kept tool uses and the minimum freed size, with defaults filled in.
pub(crate) fn thresholds(editing: &ContextEditing) -> (u32, u32, u32) {
    (
        editing.trigger_tokens.unwrap_or(DEFAULT_TRIGGER_TOKENS),
        editing.keep.unwrap_or(DEFAULT_KEEP),
        editing
            .clear_at_least_tokens
            .unwrap_or(DEFAULT_CLEAR_AT_LEAST_TOKENS),
    )
}

pub fn edit_json(editing: &ContextEditing) -> Value {
    let (trigger, keep, clear_at_least) = thresholds(editing);
    let mut edit = json!({
        "type": EDIT_TYPE,
        "trigger": {"type": "input_tokens", "value": trigger},
        "keep": {"type": "tool_uses", "value": keep},
        "clear_at_least": {"type": "input_tokens", "value": clear_at_least},
    });
    let exclude: Vec<&str> = match &editing.exclude_tools {
        Some(tools) => tools.iter().map(String::as_str).collect(),
        None => DEFAULT_EXCLUDE_TOOLS.to_vec(),
    };
    if !exclude.is_empty() {
        edit["exclude_tools"] = json!(exclude);
    }
    edit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> ContextEditing {
        ContextEditing {
            enabled: true,
            ..ContextEditing::default()
        }
    }

    #[test]
    fn defaults() {
        assert_eq!(
            edit_json(&enabled()),
            json!({
                "type": "clear_tool_uses_20250919",
                "trigger": {"type": "input_tokens", "value": 100000},
                "keep": {"type": "tool_uses", "value": 3},
                "clear_at_least": {"type": "input_tokens", "value": 20000},
                "exclude_tools": ["board_show", "board_update", "memory", "todo_write"],
            })
        );
    }

    #[test]
    fn every_key_set() {
        let editing = ContextEditing {
            enabled: true,
            trigger_tokens: Some(50_000),
            keep: Some(5),
            clear_at_least_tokens: Some(10_000),
            exclude_tools: Some(vec!["mcp__s__t".to_string()]),
        };
        assert_eq!(
            edit_json(&editing),
            json!({
                "type": "clear_tool_uses_20250919",
                "trigger": {"type": "input_tokens", "value": 50000},
                "keep": {"type": "tool_uses", "value": 5},
                "clear_at_least": {"type": "input_tokens", "value": 10000},
                "exclude_tools": ["mcp__s__t"],
            })
        );
    }

    #[test]
    fn empty_exclude_list_omits_the_key() {
        let editing = ContextEditing {
            exclude_tools: Some(Vec::new()),
            ..enabled()
        };
        assert!(edit_json(&editing).get("exclude_tools").is_none());
    }

    #[test]
    fn default_exclusions_name_real_tools() {
        for name in DEFAULT_EXCLUDE_TOOLS {
            assert!(
                fontaine_tools::descriptor::find(name).is_some(),
                "`{name}` is not a tool"
            );
        }
    }
}
