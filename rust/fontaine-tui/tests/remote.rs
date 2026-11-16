//! Remote control driven end to end: `/remote on` in the terminal UI, a client pairing and
//! attaching over loopback (relay disabled, no external service), and what the session does with
//! what the device sends.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Ui, World, text_turn, tool_turn};
use fontaine_remote::{
    AttachOptions, Client, ClientEvent, ClientMsg, MemoryClientStore, Reason, RelayMode, Ticket,
    WireAnswer,
};
use fontaine_sdk::core::config::PermissionMode;
use ratatui::crossterm::event::KeyCode;
use serde_json::{Value, json};

const DIRECT_ONLY: &str =
    "[remote]\nrelay = \"disabled\"\n[permissions]\nallow = [\"shell(sleep *)\"]\n";

/// Turns remote control on and returns the ticket, read from the clipboard through the dialog's
/// `c`.
async fn enable(ui: &Ui) -> Ticket {
    ui.send("/remote on");
    ui.wait_for("Remote ticket (shown once)").await;
    ui.press(KeyCode::Char('c'));
    ui.wait_until("a copied ticket", |_| !ui.clipboard().is_empty())
        .await;
    let ticket = ui.clipboard().pop().unwrap();
    ui.press(KeyCode::Enter);
    ui.wait_until("the dialog to close", |_| {
        !ui.live().contains("Remote ticket")
    })
    .await;
    Ticket::parse(&ticket).expect("the copied text is a ticket")
}

fn options() -> AttachOptions {
    AttachOptions {
        relay: Some(RelayMode::Disabled),
        ..AttachOptions::new("test-laptop")
    }
}

/// Waits for the client event `pick` accepts, failing on an attach failure.
async fn until<T>(client: &mut Client, mut pick: impl FnMut(&ClientEvent) -> Option<T>) -> T {
    let wait = async {
        loop {
            let event = client.events.recv().await.expect("the client is running");
            if let Some(found) = pick(&event) {
                return found;
            }
            if let ClientEvent::Failed(error) = &event {
                panic!("the attach failed: {error:?}");
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), wait)
        .await
        .expect("a client event in time")
}

async fn failure(client: &mut Client) -> String {
    let wait = async {
        loop {
            let event = client.events.recv().await.expect("the client is running");
            if let ClientEvent::Failed(error) = event {
                return format!("{error:?}");
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), wait)
        .await
        .expect("a failure in time")
}

/// Pairs a device: the host shows the confirmation and the person answers it.
async fn pair(ui: &Ui, ticket: Ticket, answer: char) -> Client {
    let client = Client::start(
        fontaine_remote::Target::Ticket(Box::new(ticket)),
        options(),
        Arc::new(MemoryClientStore::default()),
    );
    ui.wait_for_live_prompt("Pair this device?").await;
    ui.press(KeyCode::Char(answer));
    client
}

async fn attach(ui: &Ui) -> Client {
    let ticket = enable(ui).await;
    let mut client = pair(ui, ticket, 'y').await;
    until(&mut client, |e| {
        matches!(e, ClientEvent::Connected).then_some(())
    })
    .await;
    client
}

fn prompt(id: &str, text: &str) -> ClientMsg {
    ClientMsg::Prompt {
        message_id: id.into(),
        text: text.into(),
    }
}

fn dialog_of(event: &ClientEvent) -> Option<Value> {
    match event {
        ClientEvent::Event { event, .. }
            if event["kind"] == "dialog" && !event["dialog"].is_null() =>
        {
            Some(event["dialog"].clone())
        }
        _ => None,
    }
}

fn rules(w: &World) -> Vec<String> {
    w.audit()
        .iter()
        .filter(|r| r["tool"] == "remote")
        .filter_map(|r| r["rule"].as_str().map(String::from))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_device_prompts_the_session_and_the_ticket_is_never_recorded() {
    let w = World::new(vec![text_turn("got it")]).with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let mut client = attach(&ui).await;
        client.send(prompt("m1", "hello from afar"));
        until(&mut client, |e| match e {
            ClientEvent::Accepted { message_id } => (message_id == "m1").then_some(()),
            _ => None,
        })
        .await;
        ui.wait_for("got it").await;
        let screen = ui.screen();
        assert!(screen.contains("[remote "), "{screen}");
        assert!(screen.contains("hello from afar"), "{screen}");
        assert!(screen.contains("remote: 1 device"), "{screen}");
        ui.idle().await;
        client.stop();
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
    let record: Value = serde_json::from_str(message).unwrap();
    assert_eq!(record["text"], "hello from afar");
    assert!(record["remote"]["node"].is_string(), "{record}");
    assert_eq!(record["remote"]["message_id"], "m1");
    assert!(record.get("origin").is_none(), "{record}");
    // The ticket reaches no durable record.
    let audit = std::fs::read_to_string(
        w.data
            .join("audit")
            .join(format!("{}.jsonl", w.session_id())),
    )
    .unwrap();
    let history = std::fs::read_to_string(w.data.join("history.jsonl")).unwrap_or_default();
    for text in [&audit, &ledger, &history] {
        assert!(!text.contains("fontaine-attach1"), "{text}");
    }
    assert!(w.requests().iter().all(|r| !r.contains("fontaine-attach1")));
    let rules = rules(&w);
    for expected in [
        "enabled",
        "pairing_confirmed",
        "device_paired",
        "message_accepted",
    ] {
        assert!(rules.iter().any(|r| r == expected), "{rules:?}");
    }
    let paired = std::fs::read_to_string(w.data.join("paired_devices.json")).unwrap();
    assert!(paired.contains("test-laptop"), "{paired}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declined_pairing_stores_nothing_and_tells_the_device() {
    let w = World::new(Vec::new()).with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let ticket = enable(&ui).await;
        let mut client = pair(&ui, ticket, 'n').await;
        let error = failure(&mut client).await;
        assert!(error.contains("PairingDeclined"), "{error}");
        ui.idle().await;
    })
    .await;
    assert!(!w.data.join("paired_devices.json").exists());
    assert!(rules(&w).iter().any(|r| r == "pairing_declined"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_answers_an_approval_and_its_node_reaches_the_ledger_and_audit() {
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
        let mut client = attach(&ui).await;
        ui.send("make a note");
        ui.wait_for_prompt("Allow write?").await;
        let dialog = until(&mut client, |e| {
            dialog_of(e).filter(|d| d["dialog"] == "approval")
        })
        .await;
        let choices: Vec<&str> = dialog["choices"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(choices.contains(&"allow once") && !choices.contains(&"always allow"));
        client.send(ClientMsg::ApprovalAnswer {
            id: dialog["id"].as_u64().unwrap(),
            answer: WireAnswer {
                choice: "allow once".into(),
                feedback: None,
            },
        });
        ui.wait_for("all done").await;
        ui.idle().await;
        client.stop();
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(w.work.join("notes.txt")).unwrap(),
        "hello"
    );
    let approval = w
        .ledger_text()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|r| r["kind"] == "approval")
        .expect("the approval is in the ledger");
    assert!(approval["remote"]["node"].is_string(), "{approval}");
    assert!(
        w.audit().iter().any(|r| r["explanation"]
            .as_str()
            .is_some_and(|e| e.contains("answered by remote device"))),
        "{:?}",
        w.audit()
    );
    assert!(rules(&w).iter().any(|r| r == "dialog_answer"));
    // The answer is a dialog answer, not a message the model read.
    assert!(w.requests().iter().all(|r| !r.contains("allow once")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_drops_leaves_the_approval_pending_for_someone_else() {
    let w = World::new(vec![
        tool_turn(
            "c1",
            "write",
            json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_turn("all done"),
    ])
    .with_config(DIRECT_ONLY);
    let work = w.work.clone();
    w.run(|ui| async move {
        let client = attach(&ui).await;
        ui.send("make a note");
        ui.wait_for_prompt("Allow write?").await;
        client.stop();
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(ui.live().contains("Allow write?"), "{}", ui.live());
        assert!(!work.join("notes.txt").exists());
        ui.press(KeyCode::Char('y'));
        ui.wait_for("all done").await;
        ui.idle().await;
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(w.work.join("notes.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_prompt_while_a_dialog_is_open_is_queued_and_never_answers_it() {
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
        let client = attach(&ui).await;
        ui.send("make a note");
        ui.wait_for_prompt("Allow write?").await;
        client.send(prompt("m2", "y"));
        client.send(prompt("m3", "allow once"));
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(ui.live().contains("Allow write?"), "{}", ui.live());
        ui.press(KeyCode::Char('y'));
        ui.wait_for("all done").await;
        ui.idle().await;
        client.stop();
    })
    .await;
    assert!(
        w.requests().iter().any(|r| r.contains("allow once")),
        "the queued line is delivered as a message"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_cannot_run_guardian_disable_and_the_refusal_is_audited() {
    let w = World::new(Vec::new())
        .in_mode(PermissionMode::Manual)
        .with_config(DIRECT_ONLY);
    let w = &w;
    w.run(|ui| async move {
        let mut client = attach(&ui).await;
        for line in [
            "/guardian-disable",
            "/guardian-enable",
            "/remote on",
            "/afk review",
        ] {
            client.send(ClientMsg::Slash {
                message_id: format!("s-{line}"),
                line: line.into(),
            });
            let text = until(&mut client, |e| match e {
                ClientEvent::Notice { text, .. } if text.contains("host terminal") => {
                    Some(text.clone())
                }
                _ => None,
            })
            .await;
            assert!(
                text.contains(line.split_whitespace().next().unwrap()),
                "{text}"
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        // Nothing opened a confirmation and nothing was toggled.
        assert!(
            !ui.live().contains("Turn the guardian off"),
            "{}",
            ui.live()
        );
        client.stop();
    })
    .await;
    assert!(w.audit().iter().all(|r| r["rule"] != "guardian_off"));
    let refused = rules(w).iter().filter(|r| *r == "command_refused").count();
    assert_eq!(refused, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_afk_session_asks_no_device_and_tells_it_so() {
    let w = World::new(Vec::new())
        .in_mode(PermissionMode::Afk)
        .with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let mut client = attach(&ui).await;
        let notice = until(&mut client, |e| match e {
            ClientEvent::Notice { text, .. } if text.contains("AFK mode is on") => {
                Some(text.clone())
            }
            _ => None,
        })
        .await;
        assert!(notice.contains("unattended"), "{notice}");
        client.stop();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_device_closes_its_connection_and_removes_it_from_the_file() {
    let w = World::new(Vec::new()).with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let mut client = attach(&ui).await;
        ui.send("/remote devices");
        ui.wait_for("test-laptop").await;
        ui.send("/remote revoke test-laptop");
        let error = failure(&mut client).await;
        assert!(error.contains("Revoked"), "{error}");
        ui.wait_for("revoked test-laptop").await;
    })
    .await;
    let paired = std::fs::read_to_string(w.data.join("paired_devices.json")).unwrap();
    assert!(!paired.contains("test-laptop"), "{paired}");
    assert!(rules(&w).iter().any(|r| r == "device_revoked"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clear_says_goodbye_to_devices_and_keeps_them_paired() {
    let w = World::new(Vec::new()).with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let mut client = attach(&ui).await;
        ui.send("/clear");
        until(&mut client, |e| matches!(e, ClientEvent::Bye).then_some(())).await;
        ui.wait_for("cleared the conversation").await;
    })
    .await;
    let paired = std::fs::read_to_string(w.data.join("paired_devices.json")).unwrap();
    assert!(paired.contains("test-laptop"), "{paired}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_enable_while_on_is_a_no_op_that_shows_status() {
    let w = World::new(Vec::new()).with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        enable(&ui).await;
        ui.send("/remote on");
        ui.wait_for("remote control is on").await;
        ui.send("/remote status");
        ui.wait_until("a second status", |s| {
            s.matches("remote control is on").count() >= 2
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn another_session_holding_the_host_lock_refuses_remote_on() {
    let w = World::new(Vec::new()).with_config(DIRECT_ONLY);
    let lock_path = w.data.join("remote_host.lock");
    std::fs::create_dir_all(&w.data).unwrap();
    let held = fontaine_sdk::core::file_lock::try_lock(&lock_path)
        .unwrap()
        .expect("the lock was free");
    w.run(|ui| async move {
        ui.send("/remote on");
        ui.wait_for("another session is the remote host").await;
    })
    .await;
    drop(held);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unpaired_device_without_a_secret_gets_not_paired() {
    let w = World::new(Vec::new()).with_config(DIRECT_ONLY);
    w.run(|ui| async move {
        let ticket = enable(&ui).await;
        let stored = fontaine_remote::StoredHost {
            node: ticket.node.id.to_string(),
            label: "h".into(),
            addr: ticket.node.clone(),
        };
        let mut client = Client::start(
            fontaine_remote::Target::Host(Box::new(stored)),
            options(),
            Arc::new(MemoryClientStore::default()),
        );
        let error = failure(&mut client).await;
        assert!(
            error.contains(&format!("{:?}", Reason::NotPaired)),
            "{error}"
        );
    })
    .await;
}
