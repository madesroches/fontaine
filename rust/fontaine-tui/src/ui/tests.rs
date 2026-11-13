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
    CommandOutcome, EndOfTurn, Serve, Submission, TurnEnd, end_of_turn, front_push, restore_block,
};
use crate::{App, Input, Prompt, StatuslineTrigger, Ui, transcript};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
fn a_dropped_draw_trigger_owes_one_trailing_run() {
    let mut ui = ui(unbounded_channel().1);
    let (tx, mut rx) = unbounded_channel();
    ui.statusline_tx = Some(tx);
    ui.statusline_last_run = Some(std::time::Instant::now());
    ui.trigger_statusline(StatuslineTrigger::Draw);
    assert!(ui.statusline_dirty && rx.try_recv().is_err());

    // The redraw of a run's own result must not owe another run.
    ui.statusline_dirty = false;
    ui.statusline_result_redraw = true;
    ui.trigger_statusline(StatuslineTrigger::Draw);
    assert!(!ui.statusline_dirty);

    ui.statusline_last_run = None;
    ui.statusline_dirty = true;
    ui.trigger_statusline(StatuslineTrigger::Draw);
    assert!(!ui.statusline_dirty && rx.try_recv().is_ok());
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
    let quota = [
        fontaine_core::quota::QuotaPoller::new(),
        fontaine_core::quota::QuotaPoller::new(),
    ];
    let (result_tx, result_rx) = tokio::sync::mpsc::unbounded_channel();
    Ui {
        hook_notifier: crate::notify::Notifier::new(fontaine_core::config::NotifyMethod::Off),
        afk_stops: tokio::sync::watch::channel(0).1,
        app: App::new(PermissionMode::Manual, "m".into()),
        input: unbounded_channel().1,
        prompts,
        agent_events: unbounded_channel().1,
        render: |_: &mut App| {},
        spinner_epoch: std::time::Instant::now(),
        recent: RecentModels::in_memory(),
        pending_usage_switch: None,
        paused_base: std::time::Duration::ZERO,
        paused_base_for: None,
        pending_pick: None,
        pending_resume: None,
        pending_afk_review: Vec::new(),
        config_pending: Default::default(),
        pending_config: VecDeque::new(),
        pending_provider: VecDeque::new(),
        data_dir: PathBuf::new(),
        pending_drafts: Default::default(),
        user_config: None,
        env: Arc::new(|_| None),
        mcp_status: Vec::new(),
        prompts_revision: None,
        clipboard: Box::new(|_| Ok(())),
        suspend: crate::no_suspend(),
        launcher: crate::ui::Launcher::Sh,
        switch: None,
        quota_rx: [quota[0].subscribe(), quota[1].subscribe()],
        quota,
        quota_warned: Default::default(),
        statusline_tx: None,
        statusline_rx: None,
        statusline_result_tx: result_tx,
        statusline_result_rx: result_rx,
        statusline_script: None,
        statusline_running: false,
        statusline_last_run: None,
        statusline_dirty: false,
        statusline_result_redraw: false,
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

#[test]
fn a_failing_quota_warns_once_per_account() {
    let mut warned = Default::default();
    assert!(super::warn_once(&mut warned, "kimi"));
    assert!(!super::warn_once(&mut warned, "kimi"));
    assert!(super::warn_once(&mut warned, "other"));
}

mod usage {
    use std::sync::Arc;

    use fontaine_core::RuntimeError;
    use fontaine_core::quota::{Quota, QuotaSource, QuotaWindow, UsageRole};
    use futures::future::BoxFuture;

    use crate::ui::{usage_entries, usage_targets};

    #[derive(Debug)]
    struct Fake(&'static str);

    impl QuotaSource for Fake {
        fn name(&self) -> &str {
            self.0
        }

        fn fetch(&self) -> BoxFuture<'_, Result<Quota, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::new("unused")) })
        }
    }

    fn source(name: &'static str) -> Arc<dyn QuotaSource> {
        Arc::new(Fake(name))
    }

    fn quota(ratio: f64) -> Quota {
        Quota {
            windows: vec![QuotaWindow {
                label: "month".into(),
                used_ratio: ratio,
                reset_at: None,
            }],
        }
    }

    #[test]
    fn each_role_points_at_its_own_account_source() {
        let (a, b) = (source("a"), source("b"));
        let [main, guardian] = usage_targets([
            (UsageRole::Main, "one", Some(&a)),
            (UsageRole::Guardian, "two", Some(&b)),
        ]);
        assert!(Arc::ptr_eq(main.unwrap().source, &a));
        let guardian = guardian.unwrap();
        assert!(Arc::ptr_eq(guardian.source, &b));
        assert_eq!(guardian.account, "two");
    }

    #[test]
    fn the_guardian_on_the_main_account_is_not_polled_again() {
        let a = source("a");
        let [main, guardian] = usage_targets([
            (UsageRole::Main, "one", Some(&a)),
            (UsageRole::Guardian, "one", Some(&a)),
        ]);
        assert!(main.is_some());
        assert!(guardian.is_none());
    }

    #[test]
    fn a_guardian_repointed_to_the_main_account_loses_its_target() {
        let (a, b) = (source("a"), source("b"));
        let apart = usage_targets([
            (UsageRole::Main, "one", Some(&a)),
            (UsageRole::Guardian, "two", Some(&b)),
        ]);
        assert!(apart[1].is_some());
        let same = usage_targets([
            (UsageRole::Main, "one", Some(&a)),
            (UsageRole::Guardian, "one", Some(&a)),
        ]);
        assert!(same[1].is_none());
    }

    #[test]
    fn a_role_without_a_source_has_no_target_or_entry() {
        let a = source("a");
        let targets = usage_targets([
            (UsageRole::Main, "one", None),
            (UsageRole::Guardian, "two", Some(&a)),
        ]);
        assert!(targets[0].is_none());
        let entries = usage_entries(&targets, [None, Some(quota(0.5))]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, UsageRole::Guardian);
        assert_eq!(entries[0].label, "a");
    }

    #[test]
    fn a_source_with_no_data_yet_has_an_entry_without_a_quota() {
        let (a, b) = (source("a"), source("b"));
        let targets = usage_targets([
            (UsageRole::Main, "one", Some(&a)),
            (UsageRole::Guardian, "two", Some(&b)),
        ]);
        let entries = usage_entries(&targets, [None, Some(quota(0.2))]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].account, "one");
        assert!(entries[0].quota.is_none());
        assert_eq!(entries[1].quota, Some(quota(0.2)));
    }
}

#[test]
fn vim_toggles_the_input_mode_and_persists_the_choice() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join("config.toml");
    std::fs::write(&file, "[tui]\ntheme = \"dark\"\n").unwrap();
    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    ui.user_config = Some(file.clone());

    ui.vim();
    assert_eq!(ui.app.input.mode, fontaine_core::config::EditorMode::Vim);
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("editor_mode = \"vim\""), "{saved}");
    assert!(saved.contains("theme = \"dark\""), "{saved}");

    ui.vim();
    assert_eq!(ui.app.input.mode, fontaine_core::config::EditorMode::Emacs);
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("editor_mode = \"emacs\""), "{saved}");

    // No user file: the toggle still applies to the session, and says it is not saved.
    ui.user_config = None;
    ui.vim();
    assert_eq!(ui.app.input.mode, fontaine_core::config::EditorMode::Vim);
    let notice = ui
        .app
        .transcript
        .entries
        .iter()
        .rev()
        .find_map(|e| match e {
            transcript::Entry::Notice { headline, .. } => Some(headline.as_str()),
            _ => None,
        })
        .expect("a notice");
    assert!(notice.contains("for this session only"), "{notice}");
}

#[test]
fn keybindings_lists_every_action_with_its_effective_keys() {
    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    ui.keybindings();
    let notice = ui
        .app
        .transcript
        .entries
        .iter()
        .rev()
        .find_map(|e| match e {
            transcript::Entry::Notice { headline, .. } => Some(headline.as_str()),
            _ => None,
        })
        .expect("a notice");
    for action in fontaine_core::keybindings::TuiAction::ALL {
        assert!(notice.contains(action.as_str()), "{notice}");
    }
    assert!(notice.contains("shift+tab"), "{notice}");
    assert!(notice.contains("default"), "{notice}");
    assert!(
        notice.contains("there is no user settings file"),
        "{notice}"
    );
}

fn subagent_ask() -> (Prompt, oneshot::Receiver<Answer>) {
    let (prompt, answer) = ask();
    let Prompt::Ask(mut request, reply) = prompt else {
        unreachable!("ask() builds an Ask");
    };
    request.agent = "worker-1".into();
    (Prompt::Ask(request, reply), answer)
}

use fontaine_tools::mcp::{ElicitForm, ElicitReply, ElicitRequest};

fn elicit_form() -> ElicitForm {
    ElicitForm {
        server: "srv".into(),
        id: String::new(),
        message: "Who?".into(),
        schema: fontaine_tools::mcp::elicitation::ElicitationSchema::builder()
            .required_string("name")
            .build()
            .unwrap(),
    }
}

#[test]
fn an_elicitation_whose_call_ended_while_it_queued_is_dropped_and_a_live_one_opens() {
    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    let (reply, gone) = oneshot::channel();
    drop(gone);
    ui.on_prompt(
        None,
        Prompt::Elicit(Box::new(ElicitRequest::Form(elicit_form())), reply),
    );
    assert!(ui.app.elicitations.is_empty());

    let (reply, mut waiting) = oneshot::channel();
    ui.on_prompt(
        None,
        Prompt::Elicit(Box::new(ElicitRequest::Form(elicit_form())), reply),
    );
    assert_eq!(ui.app.elicitations.len(), 1);
    assert!(waiting.try_recv().is_err());
}

#[test]
fn prompts_that_arrive_during_afk_mode_are_answered_without_an_overlay() {
    use fontaine_core::permissions::{GuardianUnavailable, UnavailableChoice};
    use fontaine_tools::ask_user::QuestionReply;

    let (tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    ui.app.afk = true;

    let (prompt, mut answer) = ask();
    ui.on_prompt(None, prompt);
    assert_eq!(answer.try_recv(), Ok(Answer::Deny { feedback: None }));

    let (reply, mut question) = oneshot::channel();
    ui.on_prompt(None, Prompt::Question("main".into(), Vec::new(), reply));
    assert!(matches!(
        question.try_recv(),
        Ok(QuestionReply::Unavailable)
    ));

    let (reply, mut elicit) = oneshot::channel();
    ui.on_prompt(
        None,
        Prompt::Elicit(Box::new(ElicitRequest::Form(elicit_form())), reply),
    );
    assert_eq!(elicit.try_recv(), Ok(ElicitReply::Cancel));
    assert!(ui.app.elicitations.is_empty());

    let (reply, mut choice) = oneshot::channel();
    let info = GuardianUnavailable {
        model: "a/b".into(),
        reason: "r".into(),
        tool: "shell".into(),
    };
    ui.on_prompt(None, Prompt::Unavailable(Box::new(info), reply));
    assert_eq!(choice.try_recv(), Ok(UnavailableChoice::Deny));

    // What was still in the channel when AFK began, including a subagent's ask.
    let (prompt, mut subagent) = subagent_ask();
    tx.send(prompt).unwrap();
    ui.drain_prompts();
    assert_eq!(subagent.try_recv(), Ok(Answer::Deny { feedback: None }));
    assert!(ui.app.approval.is_none() && ui.app.questions.is_empty());
    assert!(ui.app.unavailable.is_none());
}

#[tokio::test]
async fn interrupting_a_prompt_fetch_denies_and_drains_pending_prompts() {
    let (runtime, _tmp) = runtime();
    let (input_tx, input_rx) = unbounded_channel();
    let (prompt_tx, prompt_rx) = unbounded_channel();
    let mut ui = ui(prompt_rx);
    ui.input = input_rx;

    // The fetch serves keys and prompts while it runs, so an elicitation that opened during
    // it is still open when the interrupt lands, and an ask can still wait in the channel.
    ui.app.set_running(true);
    let (reply, mut elicit) = oneshot::channel();
    ui.on_prompt(
        None,
        Prompt::Elicit(Box::new(ElicitRequest::Form(elicit_form())), reply),
    );
    assert_eq!(ui.app.elicitations.len(), 1);
    let (queued, mut answer) = ask();
    prompt_tx.send(queued).unwrap();

    // Ctrl+C interrupts even with a dialog open; the pending fetch is what serve_while drops.
    let mut fetch = Box::pin(std::future::pending::<
        Result<String, fontaine_core::RuntimeError>,
    >());
    input_tx
        .send(Input::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )))
        .unwrap();
    let end = ui
        .serve_while(&runtime, &mut fetch, Serve::Prompts)
        .await
        .expect_err("the interrupt ends the serve");
    drop(fetch);
    ui.app.set_running(false);
    assert!(matches!(end, TurnEnd::Interrupted));

    let outcome = ui.end_interrupted_prompt_fetch(&runtime, end);
    assert!(outcome.is_continue());
    assert_eq!(elicit.try_recv(), Ok(ElicitReply::Cancel));
    assert!(ui.app.elicitations.is_empty() && ui.app.approval.is_none());
    assert!(answer.try_recv().is_err(), "the queued ask is dropped");
    assert!(ui.prompts.try_recv().is_err(), "the channel is drained");
    let shown: String = ui
        .app
        .transcript
        .lines(80, &crate::theme::Theme::default())
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect();
    assert!(shown.contains("interrupted"), "{shown}");
}

/// A whole runtime over an empty scripted model, for tests that serve keys and prompts.
fn runtime() -> (fontaine_core::agent::AgentRuntime, tempfile::TempDir) {
    use fontaine_core::agent::{AgentRuntime, AgentSetup, EnvLookup, SessionOrigin};
    use fontaine_core::config::{AccountKind, LoadParams};
    use fontaine_core::extensions;
    use fontaine_core::models::{
        GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models, Switcher,
    };
    use fontaine_core::permissions::DenyAll;
    use fontaine_core::session::Session;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = |name: &str| {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let (config_dir, data, work) = (dir("config"), dir("data"), dir("work"));
    let config = fontaine_core::config::load(&LoadParams {
        launch_dir: &work,
        config_dir: &config_dir,
        data_dir: &data,
        settings_file: None,
        cli_rules: Default::default(),
        env: &|_| None,
    })
    .unwrap();
    let env: EnvLookup = Arc::new(|_: &str| None);
    let model = ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named("openai-compatible"),
        MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new()).into(),
    );
    let servers: Vec<String> = config.mcp_servers.keys().cloned().collect();
    let loaded = extensions::load(&mut extensions::LoadParams {
        config_dir: &config_dir,
        project_root: &work,
        servers: &servers,
        plugins: &[],
        warnings: &mut Vec::new(),
    })
    .unwrap();
    let switcher = Switcher::new(config.clone(), data.clone(), env.clone(), None);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let runtime = AgentRuntime::new(AgentSetup {
        config,
        config_dir: config_dir.clone(),
        data_dir: data.clone(),
        models: Models {
            main_chain: fontaine_core::model_roles::ModelChain::single(model.reference.clone()),
            main: model.clone(),
            main_listing: Default::default(),
            guardian: Some(GuardianModel::Ready(model)),
            guardian_choice: GuardianChoice::MainFallback,
            notices: Vec::new(),
        },
        session: Session::create(&data, &format!("00000000-0000-4000-8000-{n:012}")).unwrap(),
        roots: vec![work],
        scratch: None,
        memory_root: None,
        mode: PermissionMode::Manual,
        scrub_env: Vec::new(),
        git_bash: None,
        // Startup refuses to run on Windows with no shell; nothing here launches one.
        powershell: cfg!(windows).then(|| fontaine_tools::powershell::PowerShellSettings {
            path: "powershell.exe".into(),
        }),
        system_prompt: None,
        append_system_prompt: None,
        json_schema: None,
        headless: false,
        afk: false,
        scheduling: false,
        approver: Arc::new(DenyAll),
        home: None,
        user_config: None,
        guardian_profile: fontaine_guardian::Profile::Full,
        notices: Vec::new(),
        startup_warnings: Vec::new(),
        scanner: Default::default(),
        switcher: Some(switcher),
        extensions: loaded,
        plugins: Default::default(),
        project_executables: true,
        confirm_project_servers: false,
        origin: SessionOrigin::Fresh,
        env,
        warn_no_search: true,
        explicit_budget_cap: false,
    })
    .unwrap();
    (runtime, tmp)
}

#[test]
fn entering_afk_mode_closes_every_open_and_queued_prompt() {
    use fontaine_core::permissions::{GuardianUnavailable, UnavailableChoice};
    use fontaine_tools::ask_user::QuestionReply;

    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    let (open, mut first) = ask();
    let (queued, mut second) = subagent_ask();
    ui.on_prompt(None, open);
    ui.on_prompt(None, queued);
    let (reply, mut question) = oneshot::channel();
    ui.on_prompt(None, Prompt::Question("main".into(), Vec::new(), reply));
    let (reply, mut choice) = oneshot::channel();
    let info = GuardianUnavailable {
        model: "a/b".into(),
        reason: "r".into(),
        tool: "shell".into(),
    };
    ui.on_prompt(None, Prompt::Unavailable(Box::new(info), reply));
    let (reply, mut elicit) = oneshot::channel();
    ui.on_prompt(
        None,
        Prompt::Elicit(Box::new(ElicitRequest::Form(elicit_form())), reply),
    );
    assert!(ui.app.approval.is_some() && ui.app.unavailable.is_some());
    assert_eq!(ui.app.elicitations.len(), 1);

    assert_eq!(ui.app.answer_for_afk(), 5);
    assert_eq!(elicit.try_recv(), Ok(ElicitReply::Cancel));
    assert_eq!(first.try_recv(), Ok(Answer::Deny { feedback: None }));
    assert_eq!(second.try_recv(), Ok(Answer::Deny { feedback: None }));
    assert!(matches!(
        question.try_recv(),
        Ok(QuestionReply::Unavailable)
    ));
    assert_eq!(choice.try_recv(), Ok(UnavailableChoice::Deny));
    assert!(ui.app.approval.is_none() && ui.app.queued_approvals.is_empty());
    assert!(ui.app.questions.is_empty() && ui.app.unavailable.is_none());
}

#[tokio::test]
async fn a_failed_pause_does_not_outlive_the_turn_that_met_it() {
    use fontaine_core::quota::UsageRole;
    use fontaine_core::usage_pause::PauseInfo;
    let (runtime, _tmp) = runtime();
    let (_tx, rx) = unbounded_channel();
    let mut ui = ui(rx);
    ui.pending_usage_switch = Some("other/model".into());
    let pause = runtime.state.usage_pause();
    pause.engage(PauseInfo::new(UsageRole::Main, "work", None));
    pause.cancel();
    assert!(pause.failure().is_some());
    ui.finish_turn(&runtime, &TurnEnd::Interrupted).await;
    assert!(pause.failure().is_none(), "idle requests are not failed");
    assert!(ui.pending_usage_switch.is_none());
}
