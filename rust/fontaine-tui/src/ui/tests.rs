use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use fontaine_agents::runner::ChildStatus;
use fontaine_core::agent::{Delivered, MessageRefusal};
use fontaine_core::config::PermissionMode;
use fontaine_core::notice::Severity;
use fontaine_core::permissions::{Answer, ApprovalRequest, Offers};
use fontaine_core::steering::Queued;
use fontaine_core::tier::Tier;
use fontaine_core::turn::{SessionEvent, TurnOutcome};
use fontaine_tools::gate::ROOT_AGENT;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::oneshot;

use crate::provider_dialog::DialogKey;
use crate::recent::RecentModels;
use crate::ui::providers::queue_provider_request;
use crate::ui::turn::{
    CommandOutcome, EndOfTurn, Submission, TurnEnd, end_of_turn, front_push, restore_block,
};
use crate::{App, Prompt, Ui, transcript};

#[test]
fn a_cancel_removes_the_queued_sign_in_it_cancels() {
    let mut queue = VecDeque::new();
    queue_provider_request(&mut queue, DialogKey::LogOut("a".into()));
    queue_provider_request(&mut queue, DialogKey::SignIn("b".into()));
    queue_provider_request(&mut queue, DialogKey::CancelSignIn);
    assert_eq!(queue, VecDeque::from([DialogKey::LogOut("a".into())]));
    queue_provider_request(&mut queue, DialogKey::CancelSignIn);
    assert_eq!(queue.len(), 1);
}

#[test]
fn the_spinner_tick_wins_only_while_it_shows() {
    use std::time::Duration;
    let mut app = App::new(PermissionMode::Manual, "m".into());
    let waits = [
        Some(Duration::from_secs(1)),
        Some(Duration::from_millis(500)),
    ];
    assert_eq!(
        super::soonest_tick(&app, &waits),
        Some(Duration::from_millis(500))
    );
    assert_eq!(super::soonest_tick(&app, &[None]), None);
    app.running = true;
    assert_eq!(super::soonest_tick(&app, &waits), Some(super::TICK));
    assert_eq!(super::soonest_tick(&app, &[None]), Some(super::TICK));
}

fn ui(prompts: UnboundedReceiver<Prompt>) -> Ui<impl FnMut(&mut App)> {
    Ui {
        app: App::new(PermissionMode::Manual, "m".into()),
        input: unbounded_channel().1,
        prompts,
        agent_events: unbounded_channel().1,
        render: |_: &mut App| {},
        spinner_epoch: std::time::Instant::now(),
        recent: RecentModels::in_memory(),
        pending_pick: None,
        pending_resume: None,
        config_pending: Default::default(),
        pending_config: VecDeque::new(),
        pending_provider: VecDeque::new(),
        data_dir: PathBuf::new(),
        user_config: None,
        env: Arc::new(|_| None),
        mcp_status: Vec::new(),
        clipboard: Box::new(|_| Ok(())),
        suspend: crate::no_suspend(),
        launcher: crate::ui::Launcher::Sh,
        switch: None,
    }
}

fn ask() -> (Prompt, oneshot::Receiver<Answer>) {
    let (reply, answer) = oneshot::channel();
    let request = ApprovalRequest {
        tool: "shell".into(),
        agent: ROOT_AGENT.into(),
        agent_name: String::new(),
        tool_use_id: None,
        args: serde_json::json!({}),
        summary: "ls".into(),
        tier: Tier::AllowRule,
        reason: "r".into(),
        offers: Offers::default(),
        proposed_rule: None,
        preview: None,
    };
    (Prompt::Ask(Box::new(request), reply), answer)
}

fn messages(texts: &[&str]) -> Vec<Queued> {
    texts
        .iter()
        .map(|text| Queued::Message((*text).to_string()))
        .collect()
}

fn skill(raw: &str) -> Queued {
    let rest = raw.trim_start_matches('/');
    let (name, argument) = rest.split_once(' ').unwrap_or((rest, ""));
    Queued::Skill {
        raw: raw.to_string(),
        name: name.to_string(),
        argument: argument.to_string(),
    }
}

#[test]
fn queued_messages_are_sent_after_an_answer() {
    let end = TurnEnd::Finished(TurnOutcome::Answer("done".into()));
    assert_eq!(
        end_of_turn(&end, messages(&["one", "two"])),
        EndOfTurn::Submit(vec![Submission::Message("one\n\ntwo".into())])
    );
}

#[test]
fn queued_messages_go_back_when_the_turn_did_not_complete() {
    for end in [
        TurnEnd::Interrupted,
        TurnEnd::Finished(TurnOutcome::Failed("boom".into())),
        TurnEnd::Finished(TurnOutcome::MaxTurns("limit".into())),
    ] {
        assert_eq!(
            end_of_turn(&end, messages(&["one", "two"])),
            EndOfTurn::RestoreToInput("one\n\ntwo".into())
        );
    }
}

#[test]
fn skills_commands_and_shell_lines_are_submissions_of_their_own_in_typed_order() {
    let end = TurnEnd::Finished(TurnOutcome::Answer("done".into()));
    let entries = vec![
        Queued::Message("one".into()),
        skill("/review 42"),
        Queued::Message("two".into()),
        Queued::Command("/rename later".into()),
        Queued::Shell("!echo hi".into()),
        Queued::Message("three".into()),
    ];
    assert_eq!(
        end_of_turn(&end, entries),
        EndOfTurn::Submit(vec![
            Submission::Message("one".into()),
            Submission::Skill {
                raw: "/review 42".into(),
                name: "review".into(),
                argument: "42".into(),
            },
            // The message behind the skill is not steered ahead of it: it waits its turn.
            Submission::Message("two".into()),
            Submission::Command("/rename later".into()),
            Submission::Shell("!echo hi".into()),
            Submission::Message("three".into()),
        ])
    );
}

#[test]
fn a_restore_joins_every_entrys_raw_as_typed() {
    let entries = vec![
        skill("/review 42"),
        Queued::Message("plain".into()),
        Queued::Command("/rename later".into()),
        Queued::Shell("!echo hi".into()),
    ];
    for end in [
        TurnEnd::Interrupted,
        TurnEnd::Finished(TurnOutcome::Failed("x".into())),
    ] {
        assert_eq!(
            end_of_turn(&end, entries.clone()),
            EndOfTurn::RestoreToInput("/review 42\n\nplain\n\n/rename later\n\n!echo hi".into())
        );
    }
}

#[test]
fn replay_restoration_puts_older_pending_raws_first_and_the_draft_last() {
    let mut pending = VecDeque::from(vec![
        Submission::Skill {
            raw: "/review 42".into(),
            name: "review".into(),
            argument: "42".into(),
        },
        Submission::Message("behind".into()),
    ]);
    let outcome = CommandOutcome::Turn(Submission::Message("/plan do x".into()));
    if let CommandOutcome::Turn(sub) = outcome {
        front_push(&mut pending, sub);
    }
    // A dispatcher hand-off runs immediately, ahead of everything already pending.
    assert_eq!(
        pending.front(),
        Some(&Submission::Message("/plan do x".into()))
    );

    let mut app = App::new(PermissionMode::Manual, "m".into());
    app.input.set_text("draft");
    let block = restore_block(&pending, "newly typed");
    assert_eq!(block, "/plan do x\n\n/review 42\n\nbehind\n\nnewly typed");
    app.restore_to_input(&block);
    assert_eq!(
        app.input.text(),
        "/plan do x\n\n/review 42\n\nbehind\n\nnewly typed\n\ndraft"
    );
}

#[test]
fn the_front_push_puts_either_dispatcher_handoff_ahead_of_pending() {
    for handoff in [
        Submission::Message("do x".into()),
        Submission::Expanded {
            text: "/goal win".into(),
            prompt: "expanded".into(),
        },
    ] {
        let mut pending = VecDeque::from(vec![
            Submission::Command("/rename later".into()),
            Submission::Message("behind".into()),
        ]);
        front_push(&mut pending, handoff.clone());
        assert_eq!(pending.front(), Some(&handoff));
        assert_eq!(pending.len(), 3);
    }
}

#[test]
fn nothing_happens_without_retracted_messages() {
    let end = TurnEnd::Finished(TurnOutcome::Answer("done".into()));
    assert_eq!(end_of_turn(&end, Vec::new()), EndOfTurn::Nothing);
    assert_eq!(
        end_of_turn(&TurnEnd::Interrupted, Vec::new()),
        EndOfTurn::Nothing
    );
}

#[test]
fn an_ask_queued_when_the_turn_ends_never_opens() {
    let (tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    let (prompt, answer) = ask();
    tx.send(prompt).unwrap();
    tx.send(Prompt::Notice(Severity::Info, "kept".into()))
        .unwrap();
    ui.drain_prompts();
    assert!(ui.app.approval.is_none());
    assert!(ui.prompts.try_recv().is_err());
    drop(answer);
}

#[test]
fn an_ask_whose_call_is_gone_is_ignored() {
    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    let (prompt, answer) = ask();
    drop(answer);
    ui.on_prompt(None, prompt);
    assert!(ui.app.approval.is_none());
}

#[test]
fn a_finished_turn_leaves_no_call_pending() {
    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    ui.app.on_event(SessionEvent::ToolCall {
        id: "c1".into(),
        tool: "shell".into(),
        args: serde_json::json!({ "command": "ls" }),
    });
    ui.settle_turn(&TurnEnd::Finished(TurnOutcome::Answer("done".into())));
    assert!(ui.app.transcript.entries.iter().all(|e| !matches!(
        e,
        transcript::Entry::Tool {
            status: transcript::ToolStatus::Pending,
            ..
        }
    )));
}

#[test]
fn a_session_whose_ledger_cannot_be_read_is_refused_in_the_live_session() {
    use fontaine_core::session::Session;
    const THEIRS: &str = "00000000-0000-4000-8000-0000000000aa";
    let data = tempfile::tempdir().expect("a temp dir");
    let theirs = Session::create(data.path(), THEIRS).expect("create the session");
    let ledger = theirs.ledger.path().to_path_buf();
    drop(theirs);
    std::fs::write(&ledger, "{\"kind\":\"from_a_newer_build\"}\n{}\n").expect("write the ledger");

    let (_prompts_tx, prompts) = unbounded_channel();
    let mut ui = ui(prompts);
    ui.data_dir = data.path().to_path_buf();
    assert!(ui.switch_from("current", THEIRS).is_continue());
    assert!(ui.switch.is_none());
    let shown: String = ui
        .app
        .transcript
        .lines(80, &crate::theme::Theme::default())
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect();
    assert!(shown.contains("/resume:"), "{shown}");
}

#[test]
fn expand_prints_a_call_and_reports_an_unknown_one() {
    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    ui.expand("");
    ui.app.on_event(SessionEvent::ToolCall {
        id: "c1".into(),
        tool: "shell".into(),
        args: serde_json::json!({ "command": "ls" }),
    });
    ui.app.on_event(SessionEvent::ToolResult {
        id: "c1".into(),
        tool: "shell".into(),
        success: true,
        text: "a\nb".into(),
    });
    ui.expand("1");
    ui.expand("7");
    ui.expand("x");
    let shown: Vec<String> = ui
        .app
        .transcript
        .lines(80, &crate::theme::Theme::default())
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
        .collect();
    let all = shown.join("\n");
    assert!(all.contains("there is no tool call to expand"), "{all}");
    assert!(all.contains("there is no tool call number 7"), "{all}");
    assert!(
        all.contains("/expand takes a call number or `thinking`"),
        "{all}"
    );
    assert_eq!(
        shown.iter().filter(|l| l.as_str() == "  │ a").count(),
        2,
        "{all}"
    );
}

#[test]
fn expand_thinking_acts_on_the_reasoning_and_view_transcripts_honor_show_reasoning() {
    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    ui.expand("thinking");
    ui.app
        .transcript
        .on_event(SessionEvent::Reasoning("pondering".into()));
    ui.expand("thinking");
    let all = ui
        .app
        .transcript
        .lines(80, &crate::theme::Theme::default())
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("there is no reasoning to expand"), "{all}");
    assert_eq!(all.matches("pondering").count(), 2, "{all}");

    ui.app.transcript.show_reasoning = false;
    let view = ui.app.view_of("agent-1".into());
    view.transcript
        .on_event(SessionEvent::Reasoning("hidden".into()));
    assert!(view.transcript.entries.is_empty());
    let view = ui.app.view_of("agent-2".into());
    assert!(!view.transcript.show_reasoning);
}

#[test]
fn expand_and_copy_act_on_the_transcript_on_screen() {
    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    ui.app.view(Some("agent-1".into()));
    {
        let view = ui.app.view_of("agent-1".into());
        view.transcript.on_event(SessionEvent::ToolCall {
            id: "c1".into(),
            tool: "shell".into(),
            args: serde_json::json!({ "command": "ls" }),
        });
        view.transcript.on_event(SessionEvent::ToolResult {
            id: "c1".into(),
            tool: "shell".into(),
            success: true,
            text: "a\nb".into(),
        });
        view.transcript
            .on_event(SessionEvent::Assistant("the agent's reply".into()));
    }
    ui.expand("1");
    ui.expand("9");
    ui.copy("");
    let lines = |transcript: &crate::transcript::Transcript| -> String {
        transcript
            .lines(80, &crate::theme::Theme::default())
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let shown = lines(ui.app.shown());
    assert!(shown.contains("there is no tool call number 9"), "{shown}");
    assert!(shown.contains("copied reply 1"), "{shown}");
    assert!(ui.app.transcript.entries.is_empty());
}

#[test]
fn a_message_handed_to_a_resume_is_not_restored_to_the_input() {
    let mut ui = ui(unbounded_channel().1);
    let before = ui.app.transcript.entries.len();
    ui.delivered("agent-1", "hello", Ok(Delivered::HandedToResume));
    assert_eq!(ui.app.input.text(), "");
    assert_eq!(ui.app.transcript.entries.len(), before + 1);

    ui.delivered(
        "agent-1",
        "hello",
        Err(MessageRefusal::Ended(ChildStatus::Done)),
    );
    assert_eq!(ui.app.input.text(), "hello");
}
