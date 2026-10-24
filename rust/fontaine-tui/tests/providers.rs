//! The `/login` dialog driven through the whole interface: listing, adding, signing in with a
//! key, logging out and removing, all against scratch directories and `wiremock` endpoints.

mod common;

use common::routed::{Route, Turn, start};
use common::{Ui, World};
use ratatui::crossterm::event::KeyCode;
use serde_json::json;

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

async fn dialog_open(ui: &Ui) {
    ui.wait_until("the provider dialog", |_| ui.live().contains("providers"))
        .await;
}

async fn dialog_closed(ui: &Ui) {
    ui.wait_until("the provider dialog to close", |_| {
        !ui.live().contains("Enter log in")
    })
    .await;
}

fn credentials(w: &World) -> String {
    std::fs::read_to_string(w.data.join("credentials.toml")).unwrap_or_default()
}

const WORK: &str = "\n[accounts.work]\nkind = \"anthropic\"\n";

#[tokio::test(flavor = "multi_thread")]
async fn the_dialog_lists_the_accounts_and_stores_a_key_without_showing_it() {
    let w = World::routed("http://127.0.0.1:9", WORK);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/login");
        dialog_open(&ui).await;
        let shown = ui.live();
        assert!(
            shown.contains("mock") && shown.contains("openai-compatible"),
            "{shown}"
        );
        assert!(
            shown.contains("work") && shown.contains("not signed in"),
            "{shown}"
        );
        assert!(shown.contains("+ add account"), "{shown}");

        ui.press(KeyCode::Down);
        ui.press(KeyCode::Enter);
        ui.wait_for("API key for `work`").await;
        ui.type_text("sk-secret-1");
        ui.wait_for("•••••••••••").await;
        assert!(!ui.screen().contains("sk-secret"), "{}", ui.screen());
        ui.press(KeyCode::Enter);
        ui.wait_for("stored the key for `work`").await;
        assert!(!ui.screen().contains("sk-secret"), "{}", ui.screen());
        ui.press(KeyCode::Char(' '));
        ui.wait_until("the list", |_| ui.live().contains("key stored"))
            .await;
        ui.press(KeyCode::Esc);
        dialog_closed(&ui).await;
        // Nothing the dialog took went to the message box.
        assert!(!ui.live().contains("sk-"), "{}", ui.live());
    })
    .await;
    let stored = credentials(&w);
    assert!(
        stored.contains("[accounts.work]") && stored.contains("sk-secret-1"),
        "{stored}"
    );
    assert!(!w.ledger_text().contains("sk-secret"));
    assert!(
        !std::fs::read_to_string(&w.user_config)
            .unwrap()
            .contains("sk-secret"),
        "the settings file never holds a key"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_can_be_pasted_into_the_masked_field() {
    let w = World::routed("http://127.0.0.1:9", WORK);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/login work");
        ui.wait_for("API key for `work`").await;
        ui.paste("sk-pasted-9\n");
        ui.wait_for("•••••••••••").await;
        assert!(!ui.screen().contains("sk-pasted"), "{}", ui.screen());
        ui.press(KeyCode::Enter);
        ui.wait_for("stored the key for `work`").await;
    })
    .await;
    assert!(credentials(&w).contains("sk-pasted-9"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_account_opens_the_list_with_an_error_line() {
    let w = World::routed("http://127.0.0.1:9", WORK);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/login nobody");
        ui.wait_for("unknown account `nobody`").await;
        assert!(ui.live().contains("mock, work"), "{}", ui.live());
    })
    .await;
}

fn openai_models(ids: &[&str]) -> wiremock::ResponseTemplate {
    let data: Vec<_> = ids
        .iter()
        .map(|id| json!({"id": id, "object": "model", "created": 0, "owned_by": "x"}))
        .collect();
    wiremock::ResponseTemplate::new(200).set_body_json(json!({"object": "list", "data": data}))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_added_account_is_saved_logged_in_and_offers_its_models() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer};
    let gateway = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", "Bearer k-gw"))
        .respond_with(openai_models(&["gw-small", "gw-large"]))
        .mount(&gateway)
        .await;
    let w = World::routed("http://127.0.0.1:9", "");
    let url = gateway.uri();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/login");
        dialog_open(&ui).await;
        ui.press(KeyCode::Char('a'));
        ui.wait_for("kind of account").await;
        // Registry order: anthropic, openai, gemini, openai-compatible.
        for _ in 0..3 {
            ui.press(KeyCode::Down);
        }
        ui.press(KeyCode::Enter);
        ui.wait_for("new openai-compatible account").await;
        ui.type_text("gw");
        ui.press(KeyCode::Enter);
        ui.wait_for("base URL:").await;
        ui.press(KeyCode::Enter);
        ui.wait_for("a base URL is required").await;
        ui.type_text(&url);
        ui.press(KeyCode::Enter);
        ui.wait_for("API key for `gw`").await;
        ui.wait_for("added account `gw`").await;
        ui.type_text("k-gw");
        ui.press(KeyCode::Enter);
        ui.wait_for("stored the key for `gw`").await;
        ui.wait_until("the picker", |_| ui.live().contains("type to filter"))
            .await;
        let shown = ui.live();
        assert!(
            shown.contains("gw-small") && shown.contains("gw-large"),
            "{shown}"
        );
        ui.type_text("large");
        ui.press(KeyCode::Enter);
        ui.wait_for("model is now gw/gw-large").await;
    })
    .await;
    let settings = std::fs::read_to_string(&w.user_config).unwrap();
    assert!(settings.contains("[accounts.gw]"), "{settings}");
    assert!(
        settings.contains(&format!("base_url = \"{}\"", gateway.uri())),
        "{settings}"
    );
    assert!(settings.contains("model = \"gw/gw-large\""), "{settings}");
    assert!(!settings.contains("k-gw"));
    assert!(credentials(&w).contains("k-gw"));
}

#[tokio::test(flavor = "multi_thread")]
async fn log_out_and_remove_ask_first_and_remove_is_refused_for_the_main_account() {
    let w = World::routed("http://127.0.0.1:9", WORK);
    std::fs::write(
        w.data.join("credentials.toml"),
        "[accounts.work]\napi_key = \"sk-work\"\n",
    )
    .unwrap();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/login");
        dialog_open(&ui).await;
        ui.wait_for("key stored").await;
        ui.press(KeyCode::Down);
        ui.press(KeyCode::Char('o'));
        ui.wait_for("Log out of `work`?").await;
        ui.press(KeyCode::Char('n'));
        ui.wait_until("the list", |_| ui.live().contains("key stored"))
            .await;
        ui.press(KeyCode::Char('o'));
        ui.press(KeyCode::Char('y'));
        ui.wait_for("logged out of `work`").await;
        ui.press(KeyCode::Char(' '));
        ui.wait_until("the list", |_| ui.live().contains("not signed in"))
            .await;

        ui.press(KeyCode::Up);
        ui.press(KeyCode::Char('d'));
        ui.wait_for("Remove account `mock`?").await;
        ui.press(KeyCode::Char('y'));
        ui.wait_for("in use by the main model").await;
        ui.press(KeyCode::Char(' '));

        ui.press(KeyCode::Down);
        ui.press(KeyCode::Char('d'));
        ui.wait_for("Remove account `work`?").await;
        ui.press(KeyCode::Char('y'));
        ui.wait_for("removed account `work`").await;
    })
    .await;
    assert!(!credentials(&w).contains("sk-work"));
    let settings = std::fs::read_to_string(&w.user_config).unwrap();
    assert!(
        !settings.contains("accounts.work") && settings.contains("accounts.mock"),
        "{settings}"
    );
}

const OTHER: &str = "\n[accounts.other]\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"second\"]\n";

#[tokio::test(flavor = "multi_thread")]
async fn a_request_made_during_a_notification_turn_is_held_until_the_turn_ends() {
    let server = start(vec![
        Route::new(
            "root task",
            vec![
                Turn::tool(
                    "c1",
                    "agent",
                    json!({"agent_type": "worker", "prompt": "child-A: finish", "background": true}),
                ),
                Turn::text("launched"),
                Turn::text("noted the report"),
            ],
        )
        .delay(2, 3000),
        Route::new("child-A", vec![Turn::text("child done")]).delay(0, 1500),
    ])
    .await;
    let w = World::routed(
        &server.uri(),
        &format!("{OTHER}{WORK}\n[permissions]\nallow = [\"agent\"]\n"),
    );
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("root task");
        ui.wait_for("launched").await;
        idle(&ui).await;
        ui.send("/login work");
        ui.wait_for("API key for `work`").await;
        ui.wait_for("a background agent reported back").await;
        ui.type_text("sk-held");
        ui.press(KeyCode::Enter);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !ui.screen().contains("stored the key"),
            "the turn is still running:\n{}",
            ui.screen()
        );
        ui.wait_for("stored the key for `work`").await;
        assert!(ui.screen().contains("noted the report"), "{}", ui.screen());
    })
    .await;
    assert!(credentials(&w).contains("sk-held"));
}
