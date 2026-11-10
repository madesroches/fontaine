//! `/plugin` and `/reload-plugins`: managing plugins from inside the session and applying
//! skill and plugin changes without a restart.

mod common;

use std::path::Path;

use common::{Ui, World, text_turn};
use fontaine_core::plugins::{MarketplaceEntry, Registry, marketplace_dir, write_registry};
use ratatui::crossterm::event::KeyCode;

async fn idle(ui: &Ui) {
    ui.wait_until("an idle prompt", |s| {
        s.contains("shift+tab mode · /add-dir")
    })
    .await;
}

const OLD: &str = "---\ndescription: The old description\n---\nBody.\n";
const NEW: &str = "---\ndescription: The new description, longer\n---\nA different body.\n";

/// A local marketplace `local` offering the plugin `demo`: one skill and one hook.
fn marketplace(config: &Path) {
    let market = marketplace_dir(config, "local");
    let write = |rel: &str, text: &str| {
        let path = market.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        ".claude-plugin/marketplace.json",
        r#"{"name": "local", "plugins": [{"name": "demo", "source": "./plugins/demo"}, {"name": "tools", "source": "./plugins/tools"}]}"#,
    );
    write(
        "plugins/tools/.claude-plugin/plugin.json",
        r#"{"name": "tools"}"#,
    );
    write(
        "plugins/tools/skills/status/SKILL.md",
        "---\ndescription: Shows the repo status\nallowed-tools: Bash(git status)\n---\nRun !`git status`.\n",
    );
    write(
        "plugins/demo/.claude-plugin/plugin.json",
        r#"{"name": "demo"}"#,
    );
    write(
        "plugins/demo/skills/hello/SKILL.md",
        "---\ndescription: Says hello from the plugin\n---\nHello.\n",
    );
    write(
        "plugins/demo/hooks/hooks.json",
        r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/check.sh"}]}]}}"#,
    );
    write("plugins/demo/check.sh", "#!/bin/sh\nexit 0\n");
    write_registry(
        config,
        &Registry {
            marketplace: vec![MarketplaceEntry {
                name: "local".into(),
                url: "unused".into(),
                pin: "pin-one".into(),
                ..Default::default()
            }],
            plugin: Vec::new(),
        },
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edited_skill_is_noticed_applied_by_reload_and_shown_by_skills() {
    let w = World::new(vec![text_turn("ok")]).with_user_file("skills/review/SKILL.md", OLD);
    let file = w.config_dir.join("skills/review/SKILL.md");
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/skills");
        ui.wait_for("The old description").await;

        std::fs::write(&file, NEW).unwrap();
        // The notice comes at the next turn boundary, never on its own.
        ui.send("hello");
        ui.wait_for("skills/plugins changed on disk — run /reload-plugins")
            .await;
        ui.wait_for("ok").await;
        idle(&ui).await;

        ui.send("/reload-plugins");
        ui.wait_for("skills changed: review").await;
        ui.send("/skills");
        ui.wait_for("The new description, longer").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_install_confirms_the_executable_surface_then_loads_the_plugin_live() {
    let w = World::new(Vec::new());
    marketplace(&w.config_dir);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/plugin install demo@local");
        ui.wait_for_prompt("Install plugin `demo`").await;
        let screen = ui.screen();
        assert!(screen.contains("hook PreToolUse"), "{screen}");
        assert!(screen.contains("fingerprint: "), "{screen}");
        ui.press(KeyCode::Char('y'));
        ui.wait_for("installed plugin `demo`").await;
        ui.wait_for("loaded plugin `demo`").await;
        assert!(!ui.screen().contains("Honour the allowed-tools"));

        ui.send("/skills");
        ui.wait_for("demo:hello").await;
        ui.send("/plugin");
        ui.wait_for("demo (local)").await;
    })
    .await;
    let registry = fontaine_core::plugins::read_registry(&w.config_dir).unwrap();
    assert_eq!(registry.plugin.len(), 1);
    assert!(registry.plugin[0].approved.is_some());
    assert!(!w.config_dir.join("skill-trust.toml").exists());
    assert!(w.ledger_text().contains("plugin_change"));
    assert!(w.audit().iter().any(|r| r["tool"] == "plugin_change"));
}

fn registered_plugins(config: &Path) -> usize {
    fontaine_core::plugins::read_registry(config)
        .unwrap()
        .plugin
        .len()
}

#[tokio::test(flavor = "multi_thread")]
async fn trusting_a_skill_at_install_records_it_and_applies_it_in_the_session() {
    let w = World::new(Vec::new());
    marketplace(&w.config_dir);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/plugin install tools@local");
        ui.wait_for_prompt("Install plugin `tools`").await;
        ui.press(KeyCode::Char('y'));
        ui.wait_for_prompt("Honour the allowed-tools of tools:status?")
            .await;
        let screen = ui.screen();
        assert!(screen.contains("shell(git status)"), "{screen}");
        assert!(
            screen.contains("when the install finishes loading"),
            "{screen}"
        );
        ui.press(KeyCode::Char('y'));
        ui.wait_for("`tools:status` trusted").await;
        ui.wait_for("loaded plugin `tools`").await;
        ui.send("/skills");
        ui.wait_for("(trusted)").await;
    })
    .await;
    let text = std::fs::read_to_string(w.config_dir.join("skill-trust.toml")).unwrap();
    assert!(text.contains("name = \"tools:status\""), "{text}");
    assert!(w.ledger_text().contains("skill_trust"));
}

#[tokio::test(flavor = "multi_thread")]
async fn declining_the_skill_trust_installs_the_plugin_untrusted() {
    let w = World::new(Vec::new());
    marketplace(&w.config_dir);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/plugin install tools@local");
        ui.wait_for_prompt("Install plugin `tools`").await;
        ui.press(KeyCode::Char('y'));
        ui.wait_for_prompt("Honour the allowed-tools of tools:status?")
            .await;
        ui.press(KeyCode::Esc);
        ui.wait_for("left untrusted: tools:status").await;
        ui.wait_for("loaded plugin `tools`").await;
        ui.send("/skills");
        ui.wait_for("tools:status").await;
        assert!(!ui.screen().contains("(trusted)"));
    })
    .await;
    assert_eq!(registered_plugins(&w.config_dir), 1);
    assert!(!w.config_dir.join("skill-trust.toml").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_at_the_skill_trust_question_cancels_the_install() {
    let w = World::new(Vec::new());
    marketplace(&w.config_dir);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/plugin install tools@local");
        ui.wait_for_prompt("Install plugin `tools`").await;
        ui.press(KeyCode::Char('y'));
        ui.wait_for_prompt("Honour the allowed-tools of tools:status?")
            .await;
    })
    .await;
    assert_eq!(registered_plugins(&w.config_dir), 0);
    assert!(!w.config_dir.join("skill-trust.toml").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn declining_an_install_records_nothing() {
    let w = World::new(Vec::new());
    marketplace(&w.config_dir);
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/plugin install demo@local");
        ui.wait_for_prompt("Install plugin `demo`").await;
        ui.press(KeyCode::Char('n'));
        ui.wait_for("install cancelled").await;
        ui.send("/skills");
        ui.wait_for("no skills").await;
    })
    .await;
    assert!(
        fontaine_core::plugins::read_registry(&w.config_dir)
            .unwrap()
            .plugin
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn disable_unloads_enable_loads_and_uninstall_removes_the_plugin() {
    let w = World::new(Vec::new());
    marketplace(&w.config_dir);
    let config = w.config_dir.clone();
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/plugin install demo@local");
        ui.wait_for_prompt("Install plugin `demo`").await;
        ui.press(KeyCode::Char('y'));
        ui.wait_for("loaded plugin `demo`").await;

        ui.send("/plugin disable demo");
        ui.wait_for("unloaded plugin `demo`").await;
        ui.send("/skills");
        ui.wait_for("no skills").await;
        ui.send("/plugin enable demo");
        ui.wait_for("plugin `demo` is now enabled").await;
        ui.send("/skills");
        ui.wait_for("demo:hello").await;

        ui.send("/plugin uninstall demo");
        ui.wait_for("uninstalled plugin `demo`").await;
        assert!(!fontaine_core::plugins::checkout_dir(&config, "local", "demo").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_plugin_command_says_how_to_use_it() {
    let w = World::new(Vec::new());
    w.run(|ui| async move {
        idle(&ui).await;
        ui.send("/plugin frobnicate");
        ui.wait_for("usage: /plugin").await;
        ui.send("/plugin install nope@nowhere");
        ui.wait_for("no marketplace named `nowhere`").await;
    })
    .await;
}
