//! The background-agents panel, parallel orchestration in the TUI, and the trust and approval
//! prompts for what a repository defines.

mod common;

use std::path::PathBuf;

use common::routed::{Route, Turn, start};
use common::{Ui, World};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use serde_json::json;

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

fn ctrl_t(ui: &Ui) {
    ui.press_with(KeyCode::Char('t'), KeyModifiers::CONTROL);
}

fn spawn(agent_type: &str, prompt: &str) -> serde_json::Value {
    json!({"agent_type": agent_type, "prompt": prompt, "background": true})
}

/// The allow rule that lets the main conversation start subagents without asking.
const ALLOW_AGENT: &str = "\n[permissions]\nallow = [\"agent\"]\n";

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_subagents_row_shows_killed_and_its_spend_against_its_budget() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool("c1", "agent", spawn("worker", "child-A: wait for the user")),
                Turn::text("launched"),
            ],
        ),
        // The child asks for a shell command, which needs the human, so it stays running.
        Route::new(
            "child-A",
            vec![Turn::tool(
                "k1",
                "shell",
                json!({"command": "echo from the child"}),
            )],
        ),
    ])
    .await;
    let w = World::routed(&server.uri(), ALLOW_AGENT);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        // The child's approval shows who is asking.
        ui.wait_for_prompt("Allow shell (agent-1)?").await;
        // The panel opens over the approval and lists the agent as waiting for the human.
        ctrl_t(&ui);
        ui.wait_for("waiting-on-permission").await;
        let screen = ui.screen();
        assert!(screen.contains("agent-1 worker"), "{screen}");
        assert!(screen.contains("1500 / 200000 tok"), "{screen}");
        // Kill it: the row says so and keeps what it spent.
        ui.press(KeyCode::Char('x'));
        ui.wait_for("killed").await;
        let screen = ui.screen();
        assert!(screen.contains("1500 / 200000 tok"), "{screen}");
        // Closing the panel, the cancelled approval does not come back.
        ui.press(KeyCode::Esc);
        ui.wait_until("the approval is gone", |s| !s.contains("Allow shell"))
            .await;
        // And nothing ran.
        assert!(!ui.screen().contains("from the child\n"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_subagents_one_killed_the_others_finish_and_every_call_is_audited_by_agent() {
    // The third child is still at work after the main conversation has read the first report
    // and gone idle, so its own report has to wake an idle interface.
    let child = |n: u32| {
        let command = if n == 3 {
            format!("sleep 1; echo run-{n}")
        } else {
            format!("echo run-{n}")
        };
        Route::new(
            &format!("child-{n}"),
            vec![
                Turn::tool("k", "shell", json!({ "command": command })),
                Turn::text(&format!("child {n} done")),
            ],
        )
    };
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool("c1", "agent", spawn("worker", "child-1: go")),
                Turn::tool("c2", "agent", spawn("worker", "child-2: go")),
                Turn::tool("c3", "agent", spawn("worker", "child-3: go")),
                Turn::text("all launched"),
                Turn::text("everyone reported"),
            ],
        ),
        child(1),
        child(2),
        child(3),
    ])
    .await;
    let w = World::routed(&server.uri(), ALLOW_AGENT);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        // Three children ask the human at once; the asks queue rather than overwrite.
        ui.wait_for_prompt("Allow shell (agent-").await;
        ctrl_t(&ui);
        ui.wait_until("three rows", |s| {
            ["agent-1", "agent-2", "agent-3"]
                .iter()
                .all(|a| s.contains(a))
                && s.matches("waiting-on-permission").count() == 3
        })
        .await;
        // Kill the second one.
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Char('x'));
        ui.wait_for("killed").await;
        ui.press(KeyCode::Esc);
        // The other two are answered; each runs and finishes.
        for answered in 1..=2 {
            ui.wait_for_prompt("Allow shell (agent-").await;
            ui.press(KeyCode::Char('y'));
            ui.wait_until("the answer was noted", |s| {
                s.matches("allowed once").count() >= answered
            })
            .await;
        }
        ui.wait_for("everyone reported").await;
        ctrl_t(&ui);
        ui.wait_until("final rows", |s| {
            s.matches("done").count() >= 2 && s.contains("killed")
        })
        .await;
        let screen = ui.screen();
        assert!(screen.contains("agent-2 worker"), "{screen}");
        ui.press(KeyCode::Esc);
    })
    .await;

    // Every shell call has a record keyed by the agent that made it, and the killed child's
    // call was never approved.
    let audit = w.audit();
    let shells: Vec<&serde_json::Value> = audit.iter().filter(|r| r["tool"] == "shell").collect();
    assert!(!shells.is_empty());
    for record in &shells {
        let agent = record["agent_id"].as_str().unwrap();
        assert!(
            ["agent-1", "agent-2", "agent-3"].contains(&agent),
            "{record}"
        );
    }
    let approved: Vec<&str> = shells
        .iter()
        .filter(|r| r["tier"] == "human" && r["decision"] == "allow")
        .map(|r| r["agent_id"].as_str().unwrap())
        .collect();
    assert_eq!(approved.len(), 2, "{shells:?}");
    assert!(!approved.contains(&"agent-2"));
    let spawns = audit.iter().filter(|r| r["tool"] == "agent").count();
    assert_eq!(spawns, 3);
}

fn marker(w: &World) -> PathBuf {
    w.work.join("marker.txt")
}

fn touch_marker() -> String {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let name = if cfg!(windows) {
        "touch_marker.exe"
    } else {
        "touch_marker"
    };
    let path = dir.join(name);
    assert!(path.exists(), "{} is not built", path.display());
    // TOML literal strings keep Windows backslashes.
    path.to_string_lossy().into_owned()
}

fn project_server(w: &World) -> String {
    format!(
        "[mcp.servers.repo]\ncommand = '{}'\nargs = ['{}']\n",
        touch_marker(),
        marker(w).display()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_the_repository_defines_does_not_start_when_the_user_declined_trust() {
    let w = World::new(vec![]).untrusted();
    let w = {
        let settings = project_server(&w);
        w.with_project_settings(&settings)
    };
    w.run(|ui| async move {
        idle(&ui).await;
        // Nothing asked, nothing started.
        assert!(!ui.screen().contains("Start the MCP server"));
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    })
    .await;
    assert!(!marker(&w).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_reports_a_declined_project_server_as_left_off() {
    let w = World::new(vec![]);
    let settings = project_server(&w);
    let w = w.with_project_settings(&settings);
    w.run(|ui| async move {
        ui.wait_for_prompt("Start the MCP server `repo`?").await;
        ui.press(KeyCode::Char('n'));
        idle(&ui).await;
        ui.send("/mcp");
        ui.wait_for("repo: left off").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_reports_a_project_server_of_an_untrusted_workspace() {
    let w = World::new(vec![]).untrusted();
    let settings = project_server(&w);
    let w = w.with_project_settings(&settings);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/mcp");
        ui.wait_for("repo: not loaded (workspace not trusted)")
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trusted_root_still_asks_before_each_new_project_server() {
    let w = World::new(vec![]);
    let settings = project_server(&w);
    let w = w.with_project_settings(&settings);

    // Declining leaves it off, and it is asked about again next time.
    w.run(|ui| async move {
        ui.wait_for_prompt("Start the MCP server `repo`?").await;
        ui.press(KeyCode::Char('n'));
        ui.wait_for("was left off").await;
        idle(&ui).await;
    })
    .await;
    assert!(!marker(&w).exists());

    // Approving starts it and is remembered for this definition.
    w.run(|ui| async move {
        ui.wait_for_prompt("Start the MCP server `repo`?").await;
        ui.press(KeyCode::Char('y'));
        idle(&ui).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    })
    .await;
    assert!(marker(&w).exists(), "the approved server started");
    let trust = std::fs::read_to_string(w.data.join("trust.toml")).unwrap();
    assert!(
        trust.contains("[[approved_servers]]") && trust.contains("repo"),
        "{trust}"
    );

    // A later session does not ask about the same definition again.
    std::fs::remove_file(marker(&w)).unwrap();
    w.run(|ui| async move {
        idle(&ui).await;
        assert!(!ui.screen().contains("Start the MCP server"));
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    })
    .await;
    assert!(marker(&w).exists());

    // A changed definition is a new question.
    let changed = format!(
        "{}\nargs = ['{}', 'extra']\n",
        project_server(&w).split("args").next().unwrap(),
        marker(&w).display()
    );
    std::fs::write(w.work.join(".fontaine/settings.toml"), changed).unwrap();
    w.run(|ui| async move {
        ui.wait_for_prompt("Start the MCP server `repo`?").await;
        ui.press(KeyCode::Char('n'));
        idle(&ui).await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_in_the_users_own_settings_starts_without_asking() {
    let w = World::new(vec![]);
    let config = format!(
        "[mcp.servers.mine]\ncommand = '{}'\nargs = ['{}']\n",
        touch_marker(),
        marker(&w).display()
    );
    let w = w.with_config(&config);
    w.run(|ui| async move {
        idle(&ui).await;
        assert!(!ui.screen().contains("Start the MCP server"));
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    })
    .await;
    assert!(marker(&w).exists());
}

fn hook_settings(w: &World, reply: Option<&str>) -> String {
    let reply = reply.map(|r| format!(" '{r}'")).unwrap_or_default();
    format!(
        "[[hooks.pre_tool_use]]\ncommand = '''\n'{}' '{}'{reply}\n'''\n",
        touch_marker(),
        marker(w).display()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hook_the_repository_defines_does_not_run_when_the_user_declined_trust() {
    let w = World::new(vec![
        common::tool_turn("c1", "read", json!({"path": "a.txt"})),
        common::text_turn("done"),
    ])
    .untrusted();
    std::fs::write(w.work.join("a.txt"), "a").unwrap();
    let settings = hook_settings(&w, None);
    let w = w.with_project_settings(&settings);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("read it");
        ui.wait_for("done").await;
        idle(&ui).await;
    })
    .await;
    assert!(!marker(&w).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trusted_repositorys_hook_runs_but_its_allow_does_not_approve_the_call() {
    let allow = json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "permissionDecision": "allow"}})
    .to_string();
    let w = World::new(vec![
        common::tool_turn("c1", "write", json!({"path": "new.txt", "content": "x"})),
        common::text_turn("done"),
    ]);
    let settings = hook_settings(&w, Some(&allow));
    let w = w.with_project_settings(&settings);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("write it");
        // The hook allowed it and the call still asks.
        ui.wait_for_prompt("Allow write").await;
        ui.press(KeyCode::Char('n'));
        ui.wait_for("done").await;
    })
    .await;
    assert!(marker(&w).exists(), "the hook started in the trusted root");
    assert!(!w.work.join("new.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_local_server_waits_for_workspace_trust_too() {
    let w = World::new(vec![]).untrusted();
    let dir = w.work.join(".fontaine");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("settings.local.toml"), project_server(&w)).unwrap();
    w.run(|ui| async move {
        idle(&ui).await;
        assert!(!ui.screen().contains("Start the MCP server"));
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    })
    .await;
    assert!(!marker(&w).exists());
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn killing_a_job_row_stops_its_process_and_tells_the_model() {
    let server = start(vec![Route::new(
        "root task",
        vec![
            Turn::tool(
                "c1",
                "shell",
                json!({
                    "command": "sleep 120 & echo $! > job.pid; wait",
                    "run_in_background": true,
                }),
            ),
            Turn::text("job started"),
            Turn::text("saw the kill"),
        ],
    )])
    .await;
    let w = World::routed(&server.uri(), "\n[permissions]\nallow = [\"shell\"]\n");
    let work = w.work.clone();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        ui.wait_for("job started").await;
        idle(&ui).await;
        ctrl_t(&ui);
        ui.wait_for("Background jobs").await;
        let screen = ui.screen();
        assert!(screen.contains("sleep 120"), "{screen}");
        // With no subagents, the job is the selected row.
        ui.press(KeyCode::Char('x'));
        ui.wait_for("killed job-").await;
        ui.wait_for("saw the kill").await;
    })
    .await;
    let pid: u32 = std::fs::read_to_string(work.join("job.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|stat| {
            !stat
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z'))
        })
        .unwrap_or(false);
    assert!(!alive, "the job's process is gone");
    let bodies: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect();
    assert!(bodies.last().unwrap().contains("was killed by the user"));
}
