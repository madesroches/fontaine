//! The session's task lists. Each agent has its own, so a subagent's calls never overwrite the
//! root's; only the root's list is kept on disk, because agent ids do not survive a resume.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::ROOT_AGENT;
use fontaine_tools::todo::{TodoError, TodoItem, TodoStore};

use crate::owner_only;
use crate::permissions::Approver;

pub const TODOS_FILE: &str = "todos.json";

pub struct SessionTodos {
    path: PathBuf,
    // The file is written under this lock: write_atomic's temp name is per process, so two
    // concurrent writers would race on it.
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    lists: HashMap<String, Vec<TodoItem>>,
    warning: Option<String>,
}

impl SessionTodos {
    /// A session without a saved list.
    pub fn new(session_dir: &Path) -> SessionTodos {
        SessionTodos {
            path: session_dir.join(TODOS_FILE),
            state: Mutex::default(),
        }
    }

    /// The root's saved list. A file that cannot be read or parsed leaves the list empty and a
    /// warning for [`take_warning`](Self::take_warning); a missing one is not a problem.
    pub fn load(session_dir: &Path) -> SessionTodos {
        let todos = SessionTodos::new(session_dir);
        let loaded = match std::fs::read(&todos.path) {
            Ok(bytes) => serde_json::from_slice::<Vec<TodoItem>>(&bytes)
                .map_err(|e| chain(&e))
                .map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(chain(&e)),
        };
        let mut state = todos.lock();
        match loaded {
            Ok(Some(items)) => {
                state.lists.insert(ROOT_AGENT.to_string(), items);
            }
            Ok(None) => {}
            Err(reason) => {
                state.warning = Some(format!(
                    "the saved task list {} was not loaded: {reason}",
                    todos.path.display()
                ));
            }
        }
        drop(state);
        todos
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("no code panics holding this lock")
    }

    /// The root agent's list.
    pub fn root(&self) -> Vec<TodoItem> {
        self.lock()
            .lists
            .get(ROOT_AGENT)
            .cloned()
            .unwrap_or_default()
    }

    /// The warning left by [`load`](Self::load), once.
    pub fn take_warning(&self) -> Option<String> {
        self.lock().warning.take()
    }

    /// Empties the root's list and its file.
    pub fn clear(&self) -> Result<(), TodoError> {
        self.replace(ROOT_AGENT, Vec::new())
    }
}

impl TodoStore for SessionTodos {
    fn replace(&self, agent: &str, items: Vec<TodoItem>) -> Result<(), TodoError> {
        let mut state = self.lock();
        let result = if agent == ROOT_AGENT {
            let text = serde_json::to_vec(&items).expect("a list of plain items serializes");
            owner_only::write_atomic(&self.path, &text).map_err(|source| TodoError::Write {
                path: self.path.clone(),
                source,
            })
        } else {
            Ok(())
        };
        // The in-memory list changes even when the save fails, so the panel shows what the
        // model believes it has recorded.
        state.lists.insert(agent.to_string(), items);
        result
    }
}

/// The store the tool is given: a failed save is also told to the human, not only to the model.
pub struct ReportedTodos {
    pub todos: Arc<SessionTodos>,
    pub approver: Arc<dyn Approver>,
}

impl TodoStore for ReportedTodos {
    fn replace(&self, agent: &str, items: Vec<TodoItem>) -> Result<(), TodoError> {
        let result = self.todos.replace(agent, items);
        if let Err(e) = &result {
            self.approver
                .warn(&format!("the task list was not saved: {}", chain(e)));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_tools::todo::TodoStatus;

    fn item(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_string(),
            status,
        }
    }

    #[test]
    fn root_replace_is_written_through_and_loaded_back() {
        let dir = tempfile::tempdir().unwrap();
        let todos = SessionTodos::new(dir.path());
        let list = vec![
            item("a", TodoStatus::InProgress),
            item("b", TodoStatus::Pending),
        ];
        todos.replace(ROOT_AGENT, list.clone()).unwrap();
        assert_eq!(todos.root(), list);
        let file = std::fs::read_to_string(dir.path().join(TODOS_FILE)).unwrap();
        assert_eq!(serde_json::from_str::<Vec<TodoItem>>(&file).unwrap(), list);

        let reopened = SessionTodos::load(dir.path());
        assert_eq!(reopened.root(), list);
        assert_eq!(reopened.take_warning(), None);
    }

    #[test]
    fn a_subagent_list_stays_in_memory_and_apart_from_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let todos = SessionTodos::new(dir.path());
        todos
            .replace("agent-1", vec![item("sub", TodoStatus::Pending)])
            .unwrap();
        assert!(!dir.path().join(TODOS_FILE).exists());
        assert!(todos.root().is_empty());
        todos
            .replace(ROOT_AGENT, vec![item("root", TodoStatus::Pending)])
            .unwrap();
        todos.replace("agent-1", Vec::new()).unwrap();
        assert_eq!(todos.root()[0].content, "root");
        let file = std::fs::read_to_string(dir.path().join(TODOS_FILE)).unwrap();
        assert!(file.contains("root") && !file.contains("sub"));
    }

    #[test]
    fn a_missing_file_is_an_empty_list_without_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let todos = SessionTodos::load(dir.path());
        assert!(todos.root().is_empty());
        assert_eq!(todos.take_warning(), None);
    }

    #[test]
    fn a_corrupt_file_is_an_empty_list_with_one_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(TODOS_FILE), "{not json").unwrap();
        let todos = SessionTodos::load(dir.path());
        assert!(todos.root().is_empty());
        let warning = todos.take_warning().expect("a warning");
        assert!(warning.contains("todos.json"), "{warning}");
        assert_eq!(todos.take_warning(), None);
    }

    #[test]
    fn clear_empties_the_list_and_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let todos = SessionTodos::new(dir.path());
        todos
            .replace(ROOT_AGENT, vec![item("a", TodoStatus::Pending)])
            .unwrap();
        todos.clear().unwrap();
        assert!(todos.root().is_empty());
        assert!(SessionTodos::load(dir.path()).root().is_empty());
    }

    #[test]
    fn a_failed_write_is_an_error_and_is_warned() {
        use std::sync::Mutex as StdMutex;
        #[derive(Default)]
        struct Warnings(StdMutex<Vec<String>>);
        impl Approver for Warnings {
            fn approve<'a>(
                &'a self,
                _: crate::permissions::ApprovalRequest,
            ) -> futures::future::BoxFuture<'a, crate::permissions::Answer> {
                Box::pin(async { crate::permissions::Answer::Deny { feedback: None } })
            }
            fn warn(&self, message: &str) {
                self.0.lock().unwrap().push(message.to_string());
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone");
        let approver = Arc::new(Warnings::default());
        let store = ReportedTodos {
            todos: Arc::new(SessionTodos::new(&missing)),
            approver: approver.clone(),
        };
        let list = vec![item("a", TodoStatus::Pending)];
        assert!(store.replace(ROOT_AGENT, list.clone()).is_err());
        assert_eq!(store.todos.root(), list);
        assert_eq!(approver.0.lock().unwrap().len(), 1);
    }
}
