//! The pre-session setup screen: no main model resolves, so the provider dialog runs first and
//! the session starts once a model does.

use std::path::PathBuf;
use std::sync::Arc;

use fontaine_core::config::{self, Config, LoadParams};
use fontaine_core::dirs::Dirs;
use fontaine_tui::Input;
use fontaine_tui::setup::{SetupParams, drive, drive_linear};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use tokio::sync::mpsc::unbounded_channel;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Scratch {
    _tmp: tempfile::TempDir,
    dirs: Dirs,
    user_file: PathBuf,
    root: PathBuf,
}

impl Scratch {
    fn new(settings: &str) -> Scratch {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let dirs = Dirs {
            config: root.join("config"),
            data: root.join("data"),
        };
        std::fs::create_dir_all(&dirs.config).unwrap();
        std::fs::create_dir_all(&dirs.data).unwrap();
        let user_file = dirs.config.join("config.toml");
        std::fs::write(&user_file, settings).unwrap();
        Scratch {
            _tmp: tmp,
            dirs,
            user_file,
            root,
        }
    }

    fn config(&self) -> Config {
        config::load(&LoadParams {
            launch_dir: &self.root,
            config_dir: &self.dirs.config,
            data_dir: &self.dirs.data,
            settings_file: None,
            cli_rules: Default::default(),
            env: &|_| None,
        })
        .unwrap()
    }

    fn params(&self, error: &str) -> SetupParams {
        SetupParams {
            config: self.config(),
            dirs: self.dirs.clone(),
            env: Arc::new(|_| None),
            error: error.to_string(),
            model_override: None,
            session_id: "00000000-0000-4000-8000-000000000001".to_string(),
            user_file: self.user_file.clone(),
            screen_reader: None,
        }
    }
}

fn keys(script: &[Input]) -> tokio::sync::mpsc::UnboundedReceiver<Input> {
    let (tx, rx) = unbounded_channel();
    for input in script {
        tx.send(input.clone()).unwrap();
    }
    // The sender drops here: an exhausted script ends the setup like a closed terminal.
    rx
}

fn key(code: KeyCode) -> Input {
    Input::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn text(s: &str) -> Vec<Input> {
    s.chars().map(|c| key(KeyCode::Char(c))).collect()
}

fn terminal() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(100, 30)).unwrap()
}

async fn gateway() -> MockServer {
    let server = MockServer::start().await;
    let data: Vec<_> = ["gw-large", "gw-small"]
        .iter()
        .map(|id| json!({"id": id, "object": "model", "created": 0, "owned_by": "x"}))
        .collect();
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", "Bearer k-gw"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"object": "list", "data": data})),
        )
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn leaving_the_setup_starts_nothing() {
    let s = Scratch::new("");
    let out = drive(
        s.params("no main model"),
        terminal(),
        keys(&[key(KeyCode::Esc)]),
    )
    .await
    .unwrap();
    assert!(out.is_none());
    let out = drive(s.params("no main model"), terminal(), keys(&[]))
        .await
        .unwrap();
    assert!(out.is_none());
}

#[tokio::test]
async fn a_new_user_adds_an_account_logs_in_picks_a_model_and_the_session_starts() {
    let server = gateway().await;
    let s = Scratch::new("");
    let mut script = vec![key(KeyCode::Char('a'))];
    script.extend((0..3).map(|_| key(KeyCode::Down)));
    script.push(key(KeyCode::Enter));
    script.extend(text("gw"));
    script.push(key(KeyCode::Enter));
    script.extend(text(&server.uri()));
    script.push(key(KeyCode::Enter));
    // The key arrives as a paste, which the masked field takes.
    script.push(Input::Paste("k-gw".to_string()));
    script.push(key(KeyCode::Enter));
    script.extend(text("small"));
    script.push(key(KeyCode::Enter));

    let (models, config) = drive(
        s.params("no main model: set `[model] model`"),
        terminal(),
        keys(&script),
    )
    .await
    .unwrap()
    .expect("the session starts");

    assert_eq!(models.main.reference.to_string(), "gw/gw-small");
    assert!(config.accounts.contains_key("gw"));
    assert_eq!(config.model.as_deref(), Some("gw/gw-small"));
    let settings = std::fs::read_to_string(&s.user_file).unwrap();
    assert!(settings.contains("[accounts.gw]"), "{settings}");
    assert!(settings.contains("model = \"gw/gw-small\""), "{settings}");
    assert!(!settings.contains("k-gw"));
    let stored = std::fs::read_to_string(s.dirs.data.join("credentials.toml")).unwrap();
    assert!(stored.contains("k-gw"), "{stored}");
}

#[tokio::test]
async fn an_existing_account_can_be_logged_in_and_its_model_picked() {
    let server = gateway().await;
    let s = Scratch::new(&format!(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"gw/gone\"\n",
        server.uri()
    ));
    let mut script = vec![key(KeyCode::Enter)];
    script.extend(text("k-gw"));
    script.push(key(KeyCode::Enter));
    // Any key dismisses the result; `m` then picks a model of the selected account.
    script.push(key(KeyCode::Char(' ')));
    script.push(key(KeyCode::Char('m')));
    script.extend(text("large"));
    script.push(key(KeyCode::Enter));
    let (models, _) = drive(
        s.params("account `gw` does not offer model `gone`"),
        terminal(),
        keys(&script),
    )
    .await
    .unwrap()
    .expect("the session starts");
    // Logging in is not choosing: the stale model fails again until one is picked.
    assert_eq!(models.main.reference.to_string(), "gw/gw-large");
    let settings = std::fs::read_to_string(&s.user_file).unwrap();
    assert!(settings.contains("model = \"gw/gw-large\""), "{settings}");
}

#[tokio::test]
async fn a_pick_replaces_the_model_given_on_the_command_line() {
    let server = gateway().await;
    let s = Scratch::new(&format!(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n",
        server.uri()
    ));
    std::fs::write(
        s.dirs.data.join("credentials.toml"),
        "[accounts.gw]\napi_key = \"k-gw\"\n",
    )
    .unwrap();
    let mut script = vec![key(KeyCode::Char('m'))];
    script.extend(text("small"));
    script.push(key(KeyCode::Enter));
    let mut params = s.params("account `gw` does not offer model `nope`");
    params.model_override = Some("gw/nope".to_string());
    let (models, _) = drive(params, terminal(), keys(&script))
        .await
        .unwrap()
        .expect("the session starts");
    assert_eq!(models.main.reference.to_string(), "gw/gw-small");
}

/// A writer whose bytes can be read after the setup took it.
#[derive(Clone, Default)]
struct Bytes(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl std::io::Write for Bytes {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Bytes {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).unwrap()
    }
}

fn assert_plain(out: &str) {
    assert!(
        out.chars().all(|c| !('\u{2500}'..='\u{257f}').contains(&c)),
        "box drawing in {out:?}"
    );
    assert!(
        out.replace("\r\n", "").find('\n').is_none(),
        "bare newline in {out:?}"
    );
}

#[tokio::test]
async fn the_linear_setup_prints_the_banner_and_the_dialog_as_plain_lines() {
    let s = Scratch::new("");
    let out = Bytes::default();
    let result = drive_linear(
        s.params("no main model: set `[model] model`"),
        out.clone(),
        keys(&[key(KeyCode::Esc)]),
    )
    .await
    .unwrap();
    assert!(result.is_none());
    let out = out.text();
    for needle in [
        "setup:\r\n",
        "No model is ready yet\r\n",
        "no main model: set `[model] model`\r\n",
        "providers:\r\n",
        "+ add account",
    ] {
        assert!(out.contains(needle), "{needle} missing from {out:?}");
    }
    assert_plain(&out);
}

#[tokio::test]
async fn the_linear_setup_announces_a_listing_and_a_new_error() {
    let server = gateway().await;
    let s = Scratch::new(&format!(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"gw/gone\"\n",
        server.uri()
    ));
    let mut script = vec![key(KeyCode::Enter)];
    script.extend(text("k-gw"));
    script.push(key(KeyCode::Enter));
    script.push(key(KeyCode::Char(' ')));
    script.push(key(KeyCode::Char('m')));
    script.extend(text("large"));
    script.push(key(KeyCode::Enter));
    let out = Bytes::default();
    let (models, _) = drive_linear(s.params("old error"), out.clone(), keys(&script))
        .await
        .unwrap()
        .expect("the session starts");
    assert_eq!(models.main.reference.to_string(), "gw/gw-large");
    let out = out.text();
    assert!(out.contains("listing the models of gw"), "{out:?}");
    assert!(out.contains("model:\r\n"), "{out:?}");
    assert!(out.contains("selected: "), "{out:?}");
    // Logging in re-resolved the stale model, which failed with a different message.
    assert_eq!(out.matches("old error").count(), 1, "{out:?}");
    assert!(out.contains("gone"), "{out:?}");
    assert_eq!(out.matches("No model is ready yet").count(), 1, "{out:?}");
    assert_plain(&out);
}

#[tokio::test]
async fn typing_in_a_linear_setup_field_echoes_only_the_new_characters() {
    let s = Scratch::new("");
    let out = Bytes::default();
    let mut script = vec![key(KeyCode::Char('a')), key(KeyCode::Enter)];
    script.extend(text("work"));
    drive_linear(s.params("no main model"), out.clone(), keys(&script))
        .await
        .unwrap();
    let out = out.text();
    assert!(!out.contains('▏'), "{out:?}");
    assert_eq!(out.matches("name: ").count(), 1, "{out:?}");
    assert!(out.ends_with("name: work"), "{out:?}");
    assert_plain(&out);
}

#[tokio::test]
async fn typing_a_key_in_the_linear_setup_echoes_bullets_only() {
    let s = Scratch::new(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"http://localhost:1\"\n",
    );
    let out = Bytes::default();
    let mut script = vec![key(KeyCode::Enter)];
    script.extend(text("k-gw"));
    drive_linear(s.params("no main model"), out.clone(), keys(&script))
        .await
        .unwrap();
    let out = out.text();
    assert!(!out.contains('▏') && !out.contains("k-gw"), "{out:?}");
    // The footer changes once typing starts, which reprints the prompt once; no key reprints it.
    assert_eq!(out.matches('•').count(), 4, "{out:?}");
    assert!(out.ends_with("API key: ••••"), "{out:?}");
}
