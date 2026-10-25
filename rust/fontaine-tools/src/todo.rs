//! The `todo_write` tool: the model's own task list. It replaces the whole list on every call
//! and names no file, so it can touch nothing but the store behind the [`TodoStore`] seam.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error_chain::chain;
use crate::gate::{AgentId, ROOT_AGENT};

pub const MAX_ITEMS: usize = 50;
pub const MAX_CONTENT_CHARS: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// The list could not be saved. Callers only report it, so the cause stays a `source()`.
#[derive(Debug)]
pub enum TodoError {
    Write { path: PathBuf, source: io::Error },
}

impl std::fmt::Display for TodoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoError::Write { path, .. } => write!(f, "cannot save {}", path.display()),
        }
    }
}

impl std::error::Error for TodoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TodoError::Write { source, .. } => Some(source),
        }
    }
}

/// Where task lists are kept. `fontaine-core` implements it over the session.
pub trait TodoStore: Send + Sync {
    /// Makes `items` the whole list of `agent`.
    fn replace(&self, agent: &str, items: Vec<TodoItem>) -> Result<(), TodoError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoArgs {
    pub todos: Vec<TodoItem>,
}

#[derive(Clone)]
pub struct TodoTool {
    pub store: Arc<dyn TodoStore>,
}

fn validate(items: &[TodoItem]) -> Result<(), String> {
    if items.len() > MAX_ITEMS {
        return Err(format!(
            "the list has {} items; the limit is {MAX_ITEMS}",
            items.len()
        ));
    }
    for (index, item) in items.iter().enumerate() {
        let n = index + 1;
        if item.content.trim().is_empty() {
            return Err(format!("item {n} has empty content"));
        }
        if item.content.chars().count() > MAX_CONTENT_CHARS {
            return Err(format!(
                "item {n} is longer than {MAX_CONTENT_CHARS} characters"
            ));
        }
    }
    let active = items
        .iter()
        .filter(|item| item.status == TodoStatus::InProgress)
        .count();
    if active > 1 {
        return Err(format!(
            "{active} items are in_progress; at most one may be"
        ));
    }
    Ok(())
}

fn summary(items: &[TodoItem]) -> String {
    let count = |status| items.iter().filter(|item| item.status == status).count();
    format!(
        "Updated todo list: {} items ({} pending, {} in progress, {} completed)",
        items.len(),
        count(TodoStatus::Pending),
        count(TodoStatus::InProgress),
        count(TodoStatus::Completed),
    )
}

impl Tool for TodoTool {
    const NAME: &'static str = "todo_write";
    type Args = TodoArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Keep a task list for multi-step work. Each call replaces the whole list, so send \
             every item each time, with its current status. At most one item may be in_progress. \
             Up to {MAX_ITEMS} items of {MAX_CONTENT_CHARS} characters; an empty list clears it."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete task list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "What the task is." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                            },
                        },
                        "required": ["content", "status"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["todos"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: TodoArgs,
    ) -> Result<String, ToolExecutionError> {
        validate(&args.todos).map_err(ToolExecutionError::invalid_args)?;
        let agent = context
            .get::<AgentId>()
            .ok()
            .flatten()
            .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0);
        let text = summary(&args.todos);
        self.store
            .replace(&agent, args.todos)
            .map_err(|e| ToolExecutionError::other(chain(&e)))?;
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Memory(Mutex<Vec<(String, Vec<TodoItem>)>>);

    impl TodoStore for Memory {
        fn replace(&self, agent: &str, items: Vec<TodoItem>) -> Result<(), TodoError> {
            self.0.lock().unwrap().push((agent.to_string(), items));
            Ok(())
        }
    }

    struct Failing;

    impl TodoStore for Failing {
        fn replace(&self, _: &str, _: Vec<TodoItem>) -> Result<(), TodoError> {
            Err(TodoError::Write {
                path: PathBuf::from("/s/todos.json"),
                source: io::Error::other("disk full"),
            })
        }
    }

    fn item(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_string(),
            status,
        }
    }

    async fn run(store: Arc<dyn TodoStore>, items: Vec<TodoItem>) -> Result<String, String> {
        TodoTool { store }
            .call(&mut ToolContext::new(), TodoArgs { todos: items })
            .await
            .map_err(|e| e.to_string())
    }

    #[test]
    fn arguments_parse_and_reject_unknown_fields() {
        let ok: TodoArgs =
            serde_json::from_value(json!({"todos": [{"content": "a", "status": "in_progress"}]}))
                .unwrap();
        assert_eq!(ok.todos, vec![item("a", TodoStatus::InProgress)]);
        for bad in [
            json!({"todos": [], "extra": 1}),
            json!({"todos": [{"content": "a", "status": "pending", "id": 1}]}),
            json!({"todos": [{"content": "a", "status": "done"}]}),
        ] {
            assert!(serde_json::from_value::<TodoArgs>(bad).is_err());
        }
    }

    #[tokio::test]
    async fn the_result_counts_each_status() {
        let memory = Arc::new(Memory::default());
        let text = run(
            memory.clone(),
            vec![
                item("a", TodoStatus::Completed),
                item("b", TodoStatus::InProgress),
                item("c", TodoStatus::Pending),
                item("d", TodoStatus::Pending),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            text,
            "Updated todo list: 4 items (2 pending, 1 in progress, 1 completed)"
        );
        let stored = memory.0.lock().unwrap();
        assert_eq!(stored[0].0, ROOT_AGENT);
        assert_eq!(stored[0].1.len(), 4);
    }

    #[tokio::test]
    async fn an_empty_array_clears_the_list() {
        let memory = Arc::new(Memory::default());
        let text = run(memory.clone(), Vec::new()).await.unwrap();
        assert!(text.contains("0 items"));
        assert!(memory.0.lock().unwrap()[0].1.is_empty());
    }

    #[tokio::test]
    async fn each_limit_is_enforced_and_nothing_is_stored() {
        let memory = Arc::new(Memory::default());
        let many = (0..=MAX_ITEMS)
            .map(|i| item(&format!("t{i}"), TodoStatus::Pending))
            .collect();
        let long = vec![item(
            &"x".repeat(MAX_CONTENT_CHARS + 1),
            TodoStatus::Pending,
        )];
        let two_active = vec![
            item("a", TodoStatus::InProgress),
            item("b", TodoStatus::InProgress),
        ];
        for bad in [
            many,
            long,
            vec![item("", TodoStatus::Pending)],
            vec![item("  ", TodoStatus::Pending)],
            two_active,
        ] {
            assert!(run(memory.clone(), bad).await.is_err());
        }
        assert!(memory.0.lock().unwrap().is_empty());
        let at_limit = vec![item(&"x".repeat(MAX_CONTENT_CHARS), TodoStatus::Pending)];
        assert!(run(memory, at_limit).await.is_ok());
    }

    #[tokio::test]
    async fn a_store_failure_becomes_an_error_with_its_cause() {
        let error = run(Arc::new(Failing), vec![item("a", TodoStatus::Pending)])
            .await
            .unwrap_err();
        assert!(error.contains("/s/todos.json"), "{error}");
        assert!(error.contains("disk full"), "{error}");
    }
}
