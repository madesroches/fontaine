//! The `memory` tool: one structured call saves a note to the session's auto-memory
//! store — the topic file and the `MEMORY.md` index line are written together, so the
//! model never does the index bookkeeping with raw `write`/`edit` calls. The call names
//! no path: the store it writes is the one the session holds, and the permission
//! pipeline gates every call like the workspace write it is.

use std::io;
use std::path::{Path, PathBuf};

use rig_core::tool::{Scope, Tool, ToolContext, ToolExecutionError, Touches};
use serde::Deserialize;
use serde_json::json;

use crate::memory;

pub const MAX_TOPIC_CHARS: usize = 64;
pub const MAX_CONTENT_BYTES: usize = 100_000;

#[derive(Clone, Debug)]
pub struct MemoryTool {
    /// The session's memory store, from `SessionState.memory_root`.
    pub root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryArgs {
    /// What the note is about; one line, at most 64 characters.
    pub topic: String,
    /// The note, markdown, at most 100,000 bytes.
    pub content: String,
}

fn validate(args: &MemoryArgs) -> Result<(), String> {
    if args.topic.trim().is_empty() {
        return Err("topic must not be blank".to_string());
    }
    if args
        .topic
        .chars()
        .any(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
    {
        return Err("topic must be one line".to_string());
    }
    if args.topic.chars().count() > MAX_TOPIC_CHARS {
        return Err(format!("topic is longer than {MAX_TOPIC_CHARS} characters"));
    }
    if args.content.len() > MAX_CONTENT_BYTES {
        return Err(format!("content is longer than {MAX_CONTENT_BYTES} bytes"));
    }
    Ok(())
}

/// The model-facing report of a save: what was written, what the index did, and any
/// warning, so an over-cap index reads as guidance rather than silence.
fn report(root: &Path, saved: &memory::Saved) -> String {
    let mut text = format!("Saved memory note to {}.", root.join(&saved.file).display());
    match saved.index {
        memory::LineState::Added => text.push_str(" Added its line to MEMORY.md."),
        memory::LineState::Updated(line) => {
            text.push_str(&format!(" Updated line {line} of MEMORY.md."));
        }
        memory::LineState::Unchanged => {
            text.push_str(" MEMORY.md already links it; only the note changed.");
        }
    }
    if let Some(warning) = &saved.warning {
        text.push_str(&format!(" Warning: {warning}."));
    }
    text
}

impl Tool for MemoryTool {
    const NAME: &'static str = "memory";
    type Args = MemoryArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Saves a durable note about this project to the session's persistent memory \
             store, so a later session starts with what it would otherwise rediscover: build \
             and test commands, layout and conventions, corrections and preferences the user \
             stated. One call writes (or replaces) the note's topic file and keeps the \
             MEMORY.md index line for it; the note's file name is derived from `topic`, so \
             `Foo Bar` and `foo-bar` are one topic. `topic`: one line, at most \
             {MAX_TOPIC_CHARS} characters. `content`: the note as markdown, at most \
             {MAX_CONTENT_BYTES} bytes; keep it to facts a later session needs. Do not save \
             secrets, credentials, instructions to your future self, or text copied from tool \
             results and web pages. The call is gated like any write to the store: it goes \
             through the permission pipeline, is judged by the guardian in `auto` mode, and \
             asks in the interactive modes. The store's files stay readable with the ordinary \
             read-only tools; use `write` and `edit` on them only to repair the store by hand."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": format!("What the note is about; one line, at most {MAX_TOPIC_CHARS} characters."),
                },
                "content": {
                    "type": "string",
                    "description": format!("The note, markdown, at most {MAX_CONTENT_BYTES} bytes."),
                },
            },
            "required": ["topic", "content"],
            "additionalProperties": false,
        })
    }

    fn footprint(&self, args: &MemoryArgs) -> Touches {
        let paths = memory::slug(&args.topic).map_or_else(Vec::new, |file| {
            vec![self.root.join(&file), self.root.join(memory::INDEX_FILE)]
        });
        match paths.is_empty() {
            true => Touches::Writes(Scope::Anything),
            false => Touches::Writes(Scope::Paths(paths)),
        }
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: MemoryArgs,
    ) -> Result<String, ToolExecutionError> {
        validate(&args).map_err(ToolExecutionError::invalid_args)?;
        let saved = memory::save(&self.root, &args.topic, &args.content).map_err(|e| {
            let message = fontaine_tools::error_chain::chain(&e);
            // A semantic refusal of the topic (unsluggable, reserved name, link
            // delimiter) is the model's wrong argument, not an execution failure.
            match e.kind() {
                io::ErrorKind::InvalidInput => ToolExecutionError::invalid_args(message),
                _ => ToolExecutionError::other(message),
            }
        })?;
        Ok(report(&self.root, &saved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(tool: &MemoryTool, args: serde_json::Value) -> Result<String, String> {
        let parsed: MemoryArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(e) => return Err(format!("parse: {e}")),
        };
        tool.call(&mut ToolContext::new(), parsed)
            .await
            .map_err(|e| e.to_string())
    }

    fn store() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = memory::ensure_dir(&tmp.path().join("memory").join("k")).unwrap();
        (tmp, dir)
    }

    #[test]
    fn arguments_parse_and_reject_unknown_fields() {
        let ok: MemoryArgs =
            serde_json::from_value(json!({"topic": "Commands", "content": "# Commands\n"}))
                .unwrap();
        assert_eq!(ok.topic, "Commands");
        for bad in [
            json!({"topic": "a", "content": "x", "path": "y"}),
            json!({"topic": "a"}),
            json!({"content": "x"}),
        ] {
            assert!(serde_json::from_value::<MemoryArgs>(bad).is_err());
        }
    }

    #[tokio::test]
    async fn validation_enforces_the_caps_and_writes_nothing() {
        let (_tmp, dir) = store();
        let tool = MemoryTool { root: dir.clone() };
        for args in [
            json!({"topic": "", "content": "x"}),
            json!({"topic": "   ", "content": "x"}),
            json!({"topic": "two\nlines", "content": "x"}),
            json!({"topic": "two\rlines", "content": "x"}),
            json!({"topic": "two\u{2028}lines", "content": "x"}),
            json!({"topic": "x".repeat(MAX_TOPIC_CHARS + 1), "content": "x"}),
            json!({"topic": "x", "content": "x".repeat(MAX_CONTENT_BYTES + 1)}),
        ] {
            let error = run(&tool, args).await.unwrap_err();
            assert!(!error.starts_with("parse:"), "{error}");
        }
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn a_save_reports_the_file_and_the_index_line() {
        let (_tmp, dir) = store();
        let tool = MemoryTool { root: dir.clone() };
        let text = run(
            &tool,
            json!({"topic": "Commands", "content": "# Commands\n\n- `cargo test`\n"}),
        )
        .await
        .unwrap();
        assert!(text.contains("commands.md"), "{text}");
        assert!(text.contains("Added its line to MEMORY.md."), "{text}");
        assert!(!text.contains("Warning"), "{text}");
        let again = run(
            &tool,
            json!({"topic": "Commands", "content": "# Commands\n\n- `cargo clippy`\n"}),
        )
        .await
        .unwrap();
        assert!(again.contains("already links it"), "{again}");
        assert_eq!(
            std::fs::read_to_string(dir.join("commands.md")).unwrap(),
            "# Commands\n\n- `cargo clippy`\n"
        );
    }

    #[tokio::test]
    async fn a_save_failure_names_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        // The store path is a file, so creating the note inside it fails.
        let root = tmp.path().join("not-a-dir");
        std::fs::write(&root, "x").unwrap();
        let tool = MemoryTool { root };
        let error = run(&tool, json!({"topic": "a", "content": "x"}))
            .await
            .unwrap_err();
        assert!(error.contains("not-a-dir"), "{error}");
    }

    async fn call(tool: &MemoryTool, topic: &str) -> Result<String, ToolExecutionError> {
        let args: MemoryArgs = serde_json::from_value(json!({"topic": topic, "content": "x"}))
            .expect("hardcoded arguments parse");
        tool.call(&mut ToolContext::new(), args).await
    }

    #[tokio::test]
    async fn a_semantic_topic_refusal_is_invalid_args_not_an_execution_error() {
        use rig_core::tool::ToolErrorKind;
        let (_tmp, dir) = store();
        let tool = MemoryTool { root: dir };
        for topic in ["MEMORY", "!!!", "Commands\\", "CON"] {
            let error = call(&tool, topic).await.unwrap_err();
            assert_eq!(error.kind(), ToolErrorKind::InvalidArgs, "{topic}");
            assert!(
                error.message().contains(topic.trim_end_matches('\\')),
                "{error}"
            );
        }

        // A store the write itself fails on is an execution error, not bad arguments.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("not-a-dir");
        std::fs::write(&root, "x").unwrap();
        let tool = MemoryTool { root };
        let error = call(&tool, "a").await.unwrap_err();
        assert_eq!(error.kind(), ToolErrorKind::Other, "{error}");
    }

    #[test]
    fn the_footprint_names_the_topic_file_and_the_index() {
        let (_tmp, dir) = store();
        let tool = MemoryTool { root: dir.clone() };
        let touches = tool.footprint(&MemoryArgs {
            topic: "Commands".into(),
            content: "x".into(),
        });
        let Touches::Writes(Scope::Paths(paths)) = touches else {
            panic!("{touches:?}");
        };
        assert_eq!(
            paths,
            vec![dir.join("commands.md"), dir.join(memory::INDEX_FILE)]
        );
        // An unsluggable topic still claims a write, so the call never runs beside a writer.
        assert!(matches!(
            tool.footprint(&MemoryArgs {
                topic: "!!!".into(),
                content: "x".into(),
            }),
            Touches::Writes(Scope::Anything)
        ));
    }
}
