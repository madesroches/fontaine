//! `fontaine auto-mode setup` and the startup warning that points at it. `gh` is never on the
//! `PATH` of these runs, so every GitHub fact falls back to unknown and nothing leaves the
//! machine.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire, write};
use serde_json::Value;

const WARNING: &str = "auto mode doesn't know this project's environment";

/// A repository in the sandbox's workspace with a github.com remote, and a `PATH` with no `gh`.
fn sandbox(url: &str) -> Sandbox {
    let mut s = Sandbox::compatible(url);
    let git = s.work.join(".git");
    std::fs::create_dir_all(git.join("objects")).unwrap();
    std::fs::create_dir_all(git.join("refs")).unwrap();
    write(&git.join("HEAD"), "ref: refs/heads/main\n");
    write(
        &git.join("config"),
        "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = https://github.com/acme/proj.git\n",
    );
    s.add_file("Cargo.toml", "[package]\nname = \"proj\"\n");
    let empty = s.work.parent().unwrap().join("no-gh");
    std::fs::create_dir_all(&empty).unwrap();
    s.set_env("PATH", &empty.display().to_string());
    s
}

async fn endpoint() -> Endpoint {
    Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await
}

fn json(out: &common::Output) -> Value {
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", out.stdout))
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_without_gh_records_unknown_visibility_and_config_shows_it() {
    let model = endpoint().await;
    let s = sandbox(&model.url());

    let before = json(&s.run(&["auto-mode", "config", "--format", "json"]).await);
    assert_eq!(before["project_environment"], Value::Null);
    let text = s.run(&["auto-mode", "config"]).await;
    assert!(
        text.stdout
            .contains("project_environment: none — run fontaine auto-mode setup"),
        "{}",
        text.stdout
    );

    let out = s.run(&["auto-mode", "setup", "--no-model"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        out.stdout
            .contains("remote https://github.com/acme/proj.git"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("visibility unknown"), "{}", out.stdout);
    assert!(
        out.stdout.contains("`gh` is not installed"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("model scan: skipped (--no-model)"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("stack: rust"), "{}", out.stdout);

    let after = json(&s.run(&["auto-mode", "config", "--format", "json"]).await);
    let record = &after["project_environment"];
    assert_eq!(
        record["remotes"][0]["url"],
        "https://github.com/acme/proj.git"
    );
    assert_eq!(record["remotes"][0]["visibility"], "unknown");
    assert_eq!(record["scan"], Value::Null);
    let text = s.run(&["auto-mode", "config"]).await;
    assert!(
        text.stdout.contains("project_environment  (collected"),
        "{}",
        text.stdout
    );
    assert!(model.requests().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_json_reports_the_fallbacks_and_where_it_saved() {
    let model = endpoint().await;
    let s = sandbox(&model.url());
    let out = s
        .run(&["auto-mode", "setup", "--no-model", "--format", "json"])
        .await;
    let value = json(&out);
    assert!(
        value["fallbacks"][0]
            .as_str()
            .unwrap()
            .contains("`gh` is not installed")
    );
    let saved = std::path::PathBuf::from(value["saved_to"].as_str().unwrap());
    assert!(saved.starts_with(&s.data), "{saved:?}");
    assert!(saved.is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_terminal_the_scan_prompt_is_never_built_and_nothing_is_saved_from_it() {
    let model = endpoint().await;
    let s = sandbox(&model.url());
    let out = s.run_with_stdin(&["auto-mode", "setup"], "y\n").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        out.stdout.contains("model scan: skipped (no terminal"),
        "{}",
        out.stdout
    );
    assert!(
        model.requests().await.is_empty(),
        "no model call without a terminal"
    );
    let config = json(&s.run(&["auto-mode", "config", "--format", "json"]).await);
    assert_eq!(config["project_environment"]["scan"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_fails_with_exit_one_only_when_it_cannot_write_the_record() {
    let model = endpoint().await;
    let s = sandbox(&model.url());
    // A file where the record's directory must go.
    write(&s.data.join("auto_mode"), "not a directory");
    let out = s.run(&["auto-mode", "setup", "--no-model"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stdout);
    assert!(
        out.stderr.contains("cannot save the project environment"),
        "{}",
        out.stderr
    );
}

async fn headless(s: &Sandbox, mode: &str) -> common::Output {
    s.run(&["-p", "--permission-mode", mode, "go"]).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_auto_run_without_facts_warns_and_one_with_facts_does_not() {
    let model = endpoint().await;
    let s = sandbox(&model.url());

    let out = headless(&s, "auto").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stderr.contains(WARNING), "{}", out.stderr);
    assert!(
        out.stderr.contains("fontaine auto-mode setup"),
        "{}",
        out.stderr
    );

    let manual = headless(&s, "manual").await;
    assert!(!manual.stderr.contains(WARNING), "{}", manual.stderr);

    let setup = s.run(&["auto-mode", "setup", "--no-model"]).await;
    assert_eq!(setup.code, Some(0), "{}", setup.stderr);
    let out = headless(&s, "auto").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(!out.stderr.contains(WARNING), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_reminder_can_be_silenced_from_the_user_config() {
    let model = endpoint().await;
    let s = sandbox(&model.url());
    let base = std::fs::read_to_string(s.config.join("config.toml")).unwrap();
    s.user_config(&format!("{base}\n[auto_mode]\nsetup_reminder = false\n"));
    let out = headless(&s, "auto").await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(!out.stderr.contains(WARNING), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_file_cannot_silence_the_reminder() {
    let model = endpoint().await;
    let s = sandbox(&model.url());
    write(
        &s.work.join(".fontaine/settings.toml"),
        "[auto_mode]\nsetup_reminder = false\n",
    );
    let out = headless(&s, "auto").await;
    assert_ne!(out.code, Some(0));
    assert!(
        out.stderr
            .contains("`auto_mode.setup_reminder` cannot be set from project scope"),
        "{}",
        out.stderr
    );
}
