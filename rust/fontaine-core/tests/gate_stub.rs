mod common;

use std::sync::Arc;

use common::World;
use fontaine_core::config::Config;
use fontaine_core::gate::SessionState;
use fontaine_core::ledger::Record;
use fontaine_core::session::Session;
use fontaine_core::tools::{RegistryParams, registry};
use fontaine_tools::builtins::Registered;
use fontaine_tools::scope::Scope;
use serde_json::{Value, json};

struct Harness {
    tools: Vec<Registered>,
    session: Session,
}

impl Harness {
    fn new(w: &World, config: &Config) -> Harness {
        let session = Session::create(&w.data, "00000000-0000-4000-8000-000000000001").unwrap();
        let state = Arc::new(SessionState {
            mode: config.permissions.default_mode,
            scope: Arc::new(Scope::new(config.workspace.clone(), [])),
            deny: config.permissions.deny.clone(),
            ask: config.permissions.ask.clone(),
            allow: config.permissions.allow.clone(),
            ledger: session.ledger.clone(),
        });
        let tools = registry(RegistryParams {
            config,
            state,
            scrub_env: Vec::new(),
            git_bash: None,
        })
        .unwrap();
        Harness { tools, session }
    }

    async fn call(&self, name: &str, args: Value) -> Result<String, String> {
        let tool = self.tools.iter().find(|t| t.name == name).unwrap();
        tool.tool
            .run(args)
            .await
            .map(|o| o.render())
            .map_err(|e| e.message().to_string())
    }
}

fn setup(settings: &str) -> (World, Harness) {
    let w = World::new();
    w.user_file(settings);
    std::fs::write(w.work.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(w.work.join("secret.txt"), "hidden\n").unwrap();
    let config = w.load().unwrap();
    let harness = Harness::new(&w, &config);
    (w, harness)
}

#[tokio::test]
async fn an_in_scope_read_is_allowed() {
    let (w, h) = setup("");
    assert!(
        h.call("read", json!({"path": "a.txt"}))
            .await
            .unwrap()
            .contains("alpha")
    );
    let absolute = w.work.join("a.txt");
    assert!(
        h.call("read", json!({"path": absolute.to_str().unwrap()}))
            .await
            .unwrap()
            .contains("alpha")
    );
    assert!(h.call("ls", json!({})).await.unwrap().contains("a.txt"));
    assert!(
        h.call("grep", json!({"pattern": "alp"}))
            .await
            .unwrap()
            .contains("a.txt")
    );
    assert!(
        h.call("glob", json!({"pattern": "*.txt"}))
            .await
            .unwrap()
            .contains("a.txt")
    );
}

#[tokio::test]
async fn an_out_of_scope_read_is_denied() {
    let (w, h) = setup("");
    let outside = w.config.join("config.toml");
    std::fs::write(&outside, "x").unwrap();
    for (tool, args) in [
        ("read", json!({"path": outside.to_str().unwrap()})),
        ("read", json!({"path": "../config/config.toml"})),
        ("ls", json!({"path": w.config.to_str().unwrap()})),
        ("grep", json!({"pattern": "x", "path": ".."})),
        ("glob", json!({"pattern": "../config/*"})),
    ] {
        let denied = h.call(tool, args.clone()).await.unwrap_err();
        assert!(denied.contains("denied"), "{tool} {args}: {denied}");
        assert!(denied.contains("outside the workspace roots"), "{denied}");
    }
}

#[tokio::test]
async fn every_write_edit_shell_and_webfetch_call_is_denied() {
    let (w, h) = setup("");
    for (tool, args) in [
        ("write", json!({"path": "new.txt", "content": "x"})),
        ("write", json!({"path": "a.txt", "content": "x"})),
        (
            "edit",
            json!({"path": "a.txt", "edits": [{"old_text": "alpha", "new_text": "beta"}]}),
        ),
        ("shell", json!({"command": "touch from-shell.txt"})),
        ("webfetch", json!({"url": "http://127.0.0.1:9/"})),
    ] {
        let denied = h.call(tool, args.clone()).await.unwrap_err();
        assert!(denied.contains("denied"), "{tool}: {denied}");
    }
    assert!(!w.work.join("new.txt").exists());
    assert!(!w.work.join("from-shell.txt").exists());
    assert_eq!(
        std::fs::read_to_string(w.work.join("a.txt")).unwrap(),
        "alpha\n"
    );
}

#[tokio::test]
async fn a_deny_rule_naming_the_tool_denies_every_call_to_it() {
    let (_w, h) = setup("[permissions]\ndeny = [\"read(./secret.txt)\"]\n");
    for path in ["secret.txt", "a.txt"] {
        let denied = h.call("read", json!({ "path": path })).await.unwrap_err();
        assert!(denied.contains("permission rule"), "{path}: {denied}");
    }
    // Tools no rule names are unaffected.
    assert!(h.call("ls", json!({})).await.is_ok());
}

#[tokio::test]
async fn an_ask_rule_naming_the_tool_denies_it_too() {
    let (_w, h) = setup("[permissions]\nask = [\"read\"]\n");
    assert!(h.call("read", json!({"path": "a.txt"})).await.is_err());
    assert!(h.call("grep", json!({"pattern": "alpha"})).await.is_ok());
}

#[tokio::test]
async fn an_allow_rule_does_not_loosen_the_stub() {
    let (_w, h) = setup("[permissions]\nallow = [\"shell\", \"write\"]\n");
    assert!(h.call("shell", json!({"command": "true"})).await.is_err());
    assert!(
        h.call("write", json!({"path": "x", "content": "y"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn decisions_are_recorded_in_the_ledger() {
    let (_w, h) = setup("");
    h.call("read", json!({"path": "a.txt"})).await.unwrap();
    h.call("shell", json!({"command": "true"}))
        .await
        .unwrap_err();
    let records: Vec<Record> = h
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .map(|e| e.record)
        .collect();
    assert!(
        matches!(&records[0], Record::Action { tool, agent } if tool == "read" && agent == "root")
    );
    assert!(matches!(&records[1], Record::Denial { tool, .. } if tool == "shell"));
}
