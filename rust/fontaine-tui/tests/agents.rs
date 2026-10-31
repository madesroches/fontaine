//! The agents panel, parallel orchestration in the TUI, and the trust and approval
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
    json!({"agent_type": agent_type, "prompt": prompt})
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
    let w = World::routed(
        &server.uri(),
        "\n[permissions]\nallow = [\"agent\"]\n\n[agents]\nsubagent_token_budget = 200000\n",
    );
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

/// The user-scope rules the agent-view tests run under: subagents may start and may sleep.
const ALLOW_AGENT_AND_SLEEP: &str = "\n[permissions]\nallow = [\"agent\", \"shell(sleep *)\"]\n";

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_is_viewed_messaged_resumed_and_the_main_chat_comes_back_whole() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool("c1", "agent", spawn("worker", "child-A: go")),
                Turn::text("launched"),
                Turn::text("first report seen"),
                Turn::text("second report seen"),
            ],
        ),
        Route::new(
            "child-A",
            vec![
                Turn::tool("k1", "shell", json!({"command": "sleep 2"})),
                Turn::text("first answer"),
                Turn::text("resumed answer"),
            ],
        ),
    ])
    .await;
    let w = World::routed(&server.uri(), ALLOW_AGENT_AND_SLEEP);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        // The list sits below the status line, `main` first.
        ui.wait_until("the agent list", |s| s.contains("◯ agent-1 worker running"))
            .await;
        assert!(ui.screen().contains("● main"), "{}", ui.screen());

        // Down to the list, Down to the agent, Enter.
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Enter);
        ui.wait_for("agent-1 · worker · running").await;
        let screen = ui.screen();
        assert!(screen.contains("> child-A: go"), "{screen}");
        assert!(
            !screen.contains("root task"),
            "the main chat is gone from the terminal: {screen}"
        );

        // Typed while the agent runs: shown dimmed, then read at its next tool result.
        ui.send("also count the lines");
        ui.wait_for("queued: also count the lines").await;
        ui.wait_for("> also count the lines").await;
        ui.wait_for("first answer").await;
        ui.wait_for("agent-1 · worker · done").await;

        // The agent has ended, and typing resumes it.
        ui.send("and the words");
        ui.wait_for("> and the words").await;
        ui.wait_for("resumed answer").await;

        // Back to main: the whole chat is reprinted and holds no trace of the agent's lines.
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Enter);
        ui.wait_for("you resumed agent-1").await;
        ui.wait_for("second report seen").await;
        let screen = ui.screen();
        assert!(screen.contains("launched"), "{screen}");
        assert!(screen.contains("first report seen"), "{screen}");
        assert!(!screen.contains("resumed answer"), "{screen}");
        assert!(!screen.contains("> child-A: go"), "{screen}");
    })
    .await;

    let requests = server.received_requests().await.unwrap_or_default();
    let child: Vec<String> = requests
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .filter(|body| body.contains("child-A: go") && !body.contains("root task"))
        .collect();
    assert_eq!(child.len(), 3, "{child:?}");
    assert!(
        child[1].contains("The user sent this message while you were working")
            && child[1].contains("also count the lines"),
        "the agent read the message with its tool result"
    );
    assert!(
        child[2].contains("and the words"),
        "the resume carried the text"
    );

    // The words typed to the agent are intent for it, recorded when it read them.
    let ledger = w.ledger_text();
    assert!(
        ledger.contains(r#""text":"also count the lines","agent":"agent-1""#),
        "{ledger}"
    );
    assert!(
        ledger.contains(r#""event":"resumed""#)
            && ledger.find(r#""event":"resumed""#) < ledger.find(r#""text":"and the words""#),
        "{ledger}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_to_a_killed_agent_is_refused_and_the_text_stays() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool("c1", "agent", spawn("worker", "child-A: wait for the user")),
                Turn::text("launched"),
            ],
        ),
        Route::new(
            "child-A",
            vec![Turn::tool("k1", "shell", json!({"command": "echo hi"}))],
        ),
    ])
    .await;
    let w = World::routed(&server.uri(), ALLOW_AGENT);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        ui.wait_for_prompt("Allow shell (agent-1)?").await;
        ctrl_t(&ui);
        ui.wait_for("waiting-on-permission").await;
        ui.press(KeyCode::Char('x'));
        ui.wait_for("killed").await;
        ui.press(KeyCode::Esc);
        ui.wait_until("the approval is gone", |s| !s.contains("Allow shell"))
            .await;

        ui.press(KeyCode::Down);
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Enter);
        ui.wait_for("agent-1 · worker · killed").await;
        ui.send("hello again");
        ui.wait_for("agent-1 was killed; it cannot be resumed")
            .await;
        assert!(ui.live().contains("hello again"), "{}", ui.live());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_joining_subagent_takes_a_turn_on_the_words_typed_into_its_view() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool("c1", "agent", spawn("lead", "lead-L: coordinate")),
                Turn::text("launched"),
                Turn::text("lead report seen"),
            ],
        ),
        Route::new(
            "lead-L",
            vec![
                Turn::tool("l1", "agent", spawn("explore", "gc-G: dig")),
                Turn::text("lead answered early"),
                Turn::text("lead read the words"),
                Turn::text("lead read the report"),
            ],
        ),
        Route::new("gc-G", vec![Turn::text("gc-final-report")]).delay(0, 3000),
    ])
    .await;
    let w = World::routed(
        &server.uri(),
        &format!("{ALLOW_AGENT}\n[agents.types.lead]\nmodel = \"$main\"\n"),
    );
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        ui.wait_until("both rows", |s| {
            s.contains("agent-1") && s.contains("agent-2")
        })
        .await;
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Enter);
        ui.wait_for("agent-1 · lead").await;
        // The lead has answered while the grandchild is still out, so it is joining.
        ui.wait_for("lead answered early").await;
        ui.send("mid-join words");
        ui.wait_for("lead read the words").await;
        ui.wait_for("lead read the report").await;
        ui.wait_for("agent-1 · lead · done").await;
    })
    .await;

    let requests = server.received_requests().await.unwrap_or_default();
    let lead: Vec<String> = requests
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .filter(|body| body.contains("lead-L: coordinate") && !body.contains("root task"))
        .collect();
    let typed = lead
        .iter()
        .position(|b| b.contains("mid-join words"))
        .expect("a lead request carries the typed words");
    assert!(
        !lead[typed].contains("gc-final-report"),
        "the turn on the words came before the grandchild's report"
    );
    assert!(lead.iter().any(|b| b.contains("gc-final-report")));

    let ledger = w.ledger_text();
    assert!(
        ledger.contains(r#""text":"mid-join words","agent":"agent-1""#),
        "{ledger}"
    );
    assert!(ledger.contains(r#""kind":"user_message""#), "{ledger}");
}

#[tokio::test(flavor = "multi_thread")]
async fn export_with_a_subagent_writes_a_folder_with_the_index_and_transcripts() {
    // The wiring no unit test reaches: spawn -> persisted log -> tree/ledger -> /export.
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool("c1", "agent", spawn("worker", "child task: dig in")),
                Turn::text("launched"),
            ],
        ),
        Route::new("dig in", vec![Turn::text("child did the work")]),
    ])
    .await;
    let w = World::routed(&server.uri(), ALLOW_AGENT);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        ui.wait_for("launched").await;
        idle(&ui).await;
        ui.send("/export");
        ui.wait_for("exported the session and 1 subagent transcript to")
            .await;
        // A second export to the same default target is an error, never a merge.
        ui.send("/export");
        ui.wait_for("already exists").await;
    })
    .await;
    let dir = w.work.join("fontaine-00000000");
    let main = std::fs::read_to_string(dir.join("main.md")).unwrap();
    assert!(main.contains("## User\n\nroot task"), "{main}");
    assert!(main.contains("## Subagents"), "{main}");
    assert!(
        main.contains("- `agent-1` — worker, child of `root`: done — [transcript](agent-1.md)"),
        "{main}"
    );
    let child = std::fs::read_to_string(dir.join("agent-1.md")).unwrap();
    assert!(
        child.starts_with("# Subagent `agent-1` (worker)\n\nSession `"),
        "{child}"
    );
    assert!(child.contains("Parent: `root`"), "{child}");
    assert!(child.contains("Status: done"), "{child}");
    assert!(child.contains("child did the work"), "{child}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
        for name in ["main.md", "agent-1.md"] {
            let mode = std::fs::metadata(dir.join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{name}: {mode:o}");
        }
    }
}
