use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use fontaine_core::config::PermissionMode;
use fontaine_core::notice::Severity;
use fontaine_core::permissions::{Answer, ApprovalRequest, Offers};
use fontaine_core::tier::Tier;
use fontaine_core::turn::{SessionEvent, TurnOutcome};
use fontaine_tools::gate::ROOT_AGENT;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::oneshot;

use crate::provider_dialog::DialogKey;
use crate::recent::RecentModels;
use crate::ui::providers::queue_provider_request;
use crate::ui::turn::{EndOfTurn, TurnEnd, end_of_turn};
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

fn ui(prompts: UnboundedReceiver<Prompt>) -> Ui<impl FnMut(&mut App)> {
    Ui {
        app: App::new(PermissionMode::Manual, "m".into()),
        input: unbounded_channel().1,
        prompts,
        render: |_: &mut App| {},
        recent: RecentModels::in_memory(),
        pending_pick: None,
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
    }
}

fn ask() -> (Prompt, oneshot::Receiver<Answer>) {
    let (reply, answer) = oneshot::channel();
    let request = ApprovalRequest {
        tool: "shell".into(),
        agent: ROOT_AGENT.into(),
        tool_use_id: None,
        args: serde_json::json!({}),
        summary: "ls".into(),
        tier: Tier::AllowRule,
        reason: "r".into(),
        offers: Offers::default(),
        proposed_rule: None,
    };
    (Prompt::Ask(Box::new(request), reply), answer)
}

fn texts(messages: &[&str]) -> Vec<String> {
    messages.iter().map(ToString::to_string).collect()
}

#[test]
fn queued_messages_are_sent_after_an_answer() {
    let end = TurnEnd::Finished(TurnOutcome::Answer("done".into()));
    assert_eq!(
        end_of_turn(&end, texts(&["one", "two"])),
        EndOfTurn::Submit("one\n\ntwo".into())
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
            end_of_turn(&end, texts(&["one", "two"])),
            EndOfTurn::RestoreToInput("one\n\ntwo".into())
        );
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
