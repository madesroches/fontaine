//! The remote inbox driven end to end: `/remote-inbox on` in the terminal UI, a client sending a
//! ticketed message over loopback (relay disabled, no external service), and what the session
//! does with it.

mod common;

use std::time::Duration;

use common::{Ui, World, text_turn, tool_turn};
use fontaine_remote::{Reason, SendError, SendOptions, Ticket, send};
use fontaine_sdk::core::config::PermissionMode;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use serde_json::json;

const DIRECT_ONLY: &str =
    "[remote]\nrelay = \"disabled\"\n[permissions]\nallow = [\"shell(sleep *)\"]\n";

fn options() -> SendOptions {
    SendOptions {
        timeout: Duration::from_secs(15),
        ..SendOptions::default()
    }
}

/// Turns the inbox on and returns the ticket, read from the clipboard through the dialog's `c`.
async fn enable(ui: &Ui) -> Ticket {
    ui.send("/remote-inbox on");
    ui.wait_for("Remote inbox ticket (shown once)").await;
    ui.press(KeyCode::Char('c'));
    ui.wait_until("a copied ticket", |_| !ui.clipboard().is_empty())
        .await;
    let ticket = ui.clipboard().pop().unwrap();
    ui.press(KeyCode::Enter);
    ui.wait_until("the dialog to close", |_s| {
        !ui.live().contains("Remote inbox ticket")
    })
    .await;
    Ticket::parse(&ticket).expect("the copied text is a ticket")
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_session_runs_a_remote_message_as_a_user_turn_and_the_ticket_is_never_recorded() {
    let w = World::new(vec![text_turn("got it")]).with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let ticket = enable(&ui).await;
        let accepted = send(&ticket, "hello from afar", options()).await.unwrap();
        ui.wait_for("got it").await;
        let screen = ui.screen();
        assert!(screen.contains("[remote "), "{screen}");
        assert!(screen.contains("hello from afar"), "{screen}");
        assert!(screen.contains("inbox: no live ticket"), "{screen}");
        // One-shot: the same ticket does not work twice.
        let again = send(&ticket, "again", options()).await;
        assert!(
            matches!(again, Err(SendError::Rejected(Reason::Reused))),
            "{again:?}"
        );
        ui.idle().await;
        assert!(!screen.contains(&ticket.encode()));
        assert!(accepted.message_id.starts_with("msg-"));
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(requests[0].contains("hello from afar"));
    let ledger = w.ledger_text();
    let message = ledger
        .lines()
        .find(|l| l.contains("hello from afar"))
        .expect("the message is in the ledger");
    let record: serde_json::Value = serde_json::from_str(message).unwrap();
    assert_eq!(record["text"], "hello from afar");
    assert!(record["remote"]["node"].is_string(), "{record}");
    assert!(
        record["remote"]["message_id"]
            .as_str()
            .unwrap()
            .starts_with("msg-")
    );
    // An idle remote line is never bound to the agent's last reply.
    assert!(record.get("origin").is_none(), "{record}");
    // The ticket reaches no durable record.
    assert!(w.requests().iter().all(|r| !r.contains("fontaine-send1")));
    let ticket_text = std::fs::read_to_string(
        w.data
            .join("audit")
            .join(format!("{}.jsonl", w.session_id())),
    )
    .unwrap();
    assert!(!ticket_text.contains("fontaine-send1"), "{ticket_text}");
    assert!(!ledger.contains("fontaine-send1"));
    let rules: Vec<String> = w
        .audit()
        .iter()
        .filter(|r| r["tool"] == "remote_inbox")
        .filter_map(|r| r["rule"].as_str().map(String::from))
        .collect();
    for expected in ["enabled", "message_accepted"] {
        assert!(rules.iter().any(|r| r == expected), "{rules:?}");
    }
    assert!(rules.iter().any(|r| r == "message_rejected"), "{rules:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_sent_during_a_tool_call_arrives_with_the_next_result_and_the_draft_is_untouched()
{
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 2"})),
        text_turn("noted"),
    ])
    .with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let ticket = enable(&ui).await;
        ui.send("start");
        ui.wait_for("• shell sleep 2").await;
        // A multi-line draft the human is still typing, and a message queued behind the turn.
        ui.type_text("draft one");
        ui.press_with(KeyCode::Enter, KeyModifiers::ALT);
        ui.type_text("draft two");
        send(&ticket, "also check the docs", options())
            .await
            .unwrap();
        ui.wait_for("noted").await;
        ui.idle().await;
        let live = ui.live();
        assert!(
            live.contains("draft one") && live.contains("draft two"),
            "{live}"
        );
        assert!(ui.screen().contains("[remote "), "{}", ui.screen());
    })
    .await;
    let requests = w.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(!requests[0].contains("also check the docs"));
    assert!(
        requests[1].contains("[Remote message from"),
        "{}",
        requests[1]
    );
    assert!(requests[1].contains("also check the docs"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_y_answers_an_approval_that_opened_before_it_arrived() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("all done"),
    ])
    .with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let ticket = enable(&ui).await;
        ui.send("make a note");
        ui.wait_for_prompt("Allow write?").await;
        send(&ticket, "Allow Once", options()).await.unwrap();
        ui.wait_for("all done").await;
        ui.idle().await;
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(w.work.join("notes.txt")).unwrap(),
        "hello"
    );
    let rules: Vec<String> = w
        .audit()
        .iter()
        .filter(|r| r["tool"] == "remote_inbox")
        .filter_map(|r| r["rule"].as_str().map(String::from))
        .collect();
    assert!(rules.iter().any(|r| r == "dialog_answer"), "{rules:?}");
    // The answer is a dialog answer, not a message the model read.
    assert!(w.requests().iter().all(|r| !r.contains("Allow Once")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_text_that_is_not_an_option_leaves_the_approval_pending_and_is_delivered_after_it() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("all done"),
    ])
    .with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let ticket = enable(&ui).await;
        ui.send("make a note");
        ui.wait_for_prompt("Allow write?").await;
        send(&ticket, "what is this about", options())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(ui.live().contains("Allow write?"), "{}", ui.live());
        ui.press(KeyCode::Char('y'));
        ui.wait_for("all done").await;
        ui.idle().await;
    })
    .await;
    assert!(
        w.requests()
            .iter()
            .any(|r| r.contains("what is this about")),
        "the queued line is delivered once the dialog closes"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_guardian_disable_leaves_its_confirmation_pending_and_is_audited() {
    let w = World::new(Vec::new())
        .in_mode(PermissionMode::Manual)
        .with_config(DIRECT_ONLY);
    let w = &w;
    w.run(|ui| async move {
        let ticket = enable(&ui).await;
        send(&ticket, "/guardian-disable", options()).await.unwrap();
        ui.wait_for_live_prompt("guardian").await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        // Nothing confirmed it: still pending, guardian still on.
        assert!(w.audit().iter().all(|r| r["rule"] != "guardian_off"));
        ui.press(KeyCode::Char('y'));
        ui.wait_for("guardian off for this session").await;
    })
    .await;
    let record = w
        .audit()
        .into_iter()
        .find(|r| r["rule"] == "guardian_off")
        .expect("the toggle is audited");
    assert!(
        record["explanation"]
            .as_str()
            .unwrap()
            .contains("requested by remote line"),
        "{record}"
    );
    assert!(!w.ledger_text().contains("\"off\":true,\"remote\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_ticket_request_is_refused_and_off_tears_the_endpoint_down() {
    let w = World::new(vec![text_turn("unused")]).with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let ticket = enable(&ui).await;
        send(&ticket, "/remote-inbox ticket", options())
            .await
            .unwrap();
        ui.wait_for("cannot be minted from a remote line").await;
        ui.send("/remote-inbox off");
        ui.wait_for("remote inbox off").await;
        let result = send(
            &ticket,
            "late",
            SendOptions {
                timeout: Duration::from_secs(4),
                retry: false,
                ..SendOptions::default()
            },
        )
        .await;
        assert!(
            matches!(
                result,
                Err(SendError::Connect(_) | SendError::Closed | SendError::Timeout)
            ),
            "{result:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_enable_while_on_is_a_no_op_that_shows_status() {
    let w = World::new(Vec::new()).with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        enable(&ui).await;
        ui.send("/remote-inbox on");
        ui.wait_for("the remote inbox is on; a ticket is live")
            .await;
        ui.send("/remote-inbox status");
        ui.wait_until("a second status", |s| {
            s.matches("the remote inbox is on").count() >= 2
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clear_queued_ahead_of_a_remote_line_drops_the_line_with_an_audit_record() {
    let w = World::new(vec![
        tool_turn("c1", "shell", json!({"command": "sleep 2"})),
        text_turn("done"),
    ])
    .with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let ticket = enable(&ui).await;
        ui.send("start");
        ui.wait_for("• shell sleep 2").await;
        ui.send("/clear");
        send(&ticket, "after the clear", options()).await.unwrap();
        ui.wait_for("cleared the conversation").await;
        ui.idle().await;
    })
    .await;
    assert!(
        w.requests().iter().all(|r| !r.contains("after the clear")),
        "{:?}",
        w.requests()
    );
    let dropped = w
        .audit()
        .into_iter()
        .find(|r| r["rule"] == "message_dropped")
        .expect("the dropped line is audited");
    assert!(
        dropped["explanation"]
            .as_str()
            .unwrap()
            .contains("the conversation was cleared"),
        "{dropped}"
    );
}
