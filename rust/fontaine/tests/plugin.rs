//! The `fontaine plugin` CLI against a local-directory marketplace, entirely offline.

mod common;

use common::Sandbox;
use serde_json::json;

fn write_marketplace(sandbox: &Sandbox, root: &str) {
    sandbox.add_file(
        &format!("{root}/.claude-plugin/marketplace.json"),
        r#"{
            "name": "team",
            "plugins": [
                {"name": "review", "source": "./plugins/review", "description": "reviews"}
            ]
        }"#,
    );
    sandbox.add_file(
        &format!("{root}/plugins/review/.claude-plugin/plugin.json"),
        r#"{"name": "review"}"#,
    );
    sandbox.add_file(
        &format!("{root}/plugins/review/skills/review/SKILL.md"),
        "---\ndescription: reviews a diff\n---\nLook carefully.",
    );
}

#[tokio::test]
async fn the_plugin_cli_installs_lists_updates_and_removes_a_local_marketplace() {
    let sandbox = Sandbox::new();
    write_marketplace(&sandbox, "team-market");
    let market_path = sandbox.work.join("team-market");
    let market_path = market_path.to_string_lossy().into_owned();

    let add = sandbox
        .run(&["plugin", "marketplace", "add", &market_path])
        .await;
    assert_eq!(add.code, Some(0), "{}", add.stderr);

    let list = sandbox.run(&["plugin", "marketplace", "list"]).await;
    assert!(list.stdout.contains("team"), "{}", list.stdout);
    assert!(
        list.stdout.contains("1 offered, 0 installed"),
        "{}",
        list.stdout
    );

    let install = sandbox.run(&["plugin", "install", "review@team"]).await;
    assert_eq!(install.code, Some(0), "{}", install.stderr);
    assert!(install.stdout.contains("1 skill(s)"), "{}", install.stdout);

    let list = sandbox.run(&["plugin", "marketplace", "list"]).await;
    assert!(
        list.stdout.contains("1 offered, 1 installed"),
        "{}",
        list.stdout
    );

    let list = sandbox.run(&["plugin", "list", "--format", "json"]).await;
    let value: serde_json::Value = serde_json::from_str(list.stdout.trim()).unwrap();
    assert_eq!(value["plugins"][0]["name"], json!("review"));
    assert_eq!(value["plugins"][0]["enabled"], json!(true));
    assert_eq!(value["plugins"][0]["components"]["skills"], json!(1));
    assert_eq!(value["plugins"][0]["components"]["servers"], json!(0));

    let disable = sandbox.run(&["plugin", "disable", "review"]).await;
    assert_eq!(disable.code, Some(0), "{}", disable.stderr);
    let enable = sandbox.run(&["plugin", "enable", "review"]).await;
    assert_eq!(enable.code, Some(0), "{}", enable.stderr);

    // A path-source plugin moves only after `marketplace update`.
    let before = serde_json::from_str::<serde_json::Value>(
        &sandbox
            .run(&["plugin", "list", "--format", "json"])
            .await
            .stdout,
    )
    .unwrap();
    let old_pin = before["plugins"][0]["pin"].as_str().unwrap().to_string();
    sandbox.add_file(
        "team-market/plugins/review/skills/review/SKILL.md",
        "---\ndescription: reviews a diff\n---\nNew body.",
    );
    let update_market = sandbox
        .run(&["plugin", "marketplace", "update", "team"])
        .await;
    assert_eq!(update_market.code, Some(0), "{}", update_market.stderr);
    assert!(
        update_market.stdout.contains("moved"),
        "{}",
        update_market.stdout
    );

    let update_plugin = sandbox.run(&["plugin", "update", "review"]).await;
    assert_eq!(update_plugin.code, Some(0), "{}", update_plugin.stderr);
    let after = serde_json::from_str::<serde_json::Value>(
        &sandbox
            .run(&["plugin", "list", "--format", "json"])
            .await
            .stdout,
    )
    .unwrap();
    assert_ne!(after["plugins"][0]["pin"].as_str().unwrap(), old_pin);

    let remove = sandbox
        .run(&["plugin", "marketplace", "remove", "team"])
        .await;
    assert_ne!(
        remove.code,
        Some(0),
        "a marketplace with an installed plugin is refused"
    );
    let uninstall = sandbox.run(&["plugin", "uninstall", "review"]).await;
    assert_eq!(uninstall.code, Some(0), "{}", uninstall.stderr);
    let remove = sandbox
        .run(&["plugin", "marketplace", "remove", "team"])
        .await;
    assert_eq!(remove.code, Some(0), "{}", remove.stderr);
}

#[tokio::test]
async fn install_refuses_duplicate_names_and_name_mismatches() {
    let sandbox = Sandbox::new();
    write_marketplace(&sandbox, "team-market");
    sandbox.add_file(
        "team-market/.claude-plugin/marketplace.json",
        r#"{
            "name": "team",
            "plugins": [
                {"name": "review", "source": "./plugins/review"},
                {"name": "other", "source": "./plugins/review"}
            ]
        }"#,
    );
    let path = sandbox.work.join("team-market");
    let path = path.to_string_lossy().into_owned();
    assert_eq!(
        sandbox
            .run(&["plugin", "marketplace", "add", &path])
            .await
            .code,
        Some(0)
    );

    // The manifest of `other` is named `review`, which install must refuse.
    let install = sandbox.run(&["plugin", "install", "other@team"]).await;
    assert_ne!(install.code, Some(0), "{}", install.stdout);
    assert!(install.stderr.contains("review"), "{}", install.stderr);

    assert_eq!(
        sandbox
            .run(&["plugin", "install", "review@team"])
            .await
            .code,
        Some(0)
    );
    let dup = sandbox.run(&["plugin", "install", "review@team"]).await;
    assert_ne!(dup.code, Some(0));
    assert!(dup.stderr.contains("already installed"), "{}", dup.stderr);
}

fn registry_text(sandbox: &Sandbox) -> String {
    std::fs::read_to_string(sandbox.config.join("plugins.toml")).unwrap()
}

async fn add_and_install(sandbox: &Sandbox) -> String {
    write_marketplace(sandbox, "team-market");
    let path = sandbox.work.join("team-market");
    let path = path.to_string_lossy().into_owned();
    let add = sandbox.run(&["plugin", "marketplace", "add", &path]).await;
    assert_eq!(add.code, Some(0), "{}", add.stderr);
    let install = sandbox.run(&["plugin", "install", "review@team"]).await;
    assert_eq!(install.code, Some(0), "{}", install.stderr);
    install.stdout
}

#[tokio::test]
async fn the_approval_summary_lists_the_executable_surface_without_secret_values() {
    let mut sandbox = Sandbox::new();
    sandbox.set_env("REVIEW_TOKEN", "s3cr3t-value");
    write_marketplace(&sandbox, "team-market");
    let plugin = "team-market/plugins/review";
    sandbox.add_file(
        &format!("{plugin}/hooks/hooks.json"),
        r#"{"hooks": {"PreToolUse": [{"matcher": "mcp__srv__*", "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/check.sh --token ${REVIEW_TOKEN}"}]}]}}"#,
    );
    sandbox.add_file(
        &format!("{plugin}/.mcp.json"),
        r#"{"mcpServers": {"srv": {"command": "${CLAUDE_PLUGIN_ROOT}/serve", "args": ["--port", "1"], "env": {"TOKEN": "${REVIEW_TOKEN}"}}}}"#,
    );
    sandbox.add_file(
        &format!("{plugin}/skills/review/SKILL.md"),
        "---\ndescription: reviews\n---\nBranch: !`git branch --show-current`\n",
    );
    let path = sandbox.work.join("team-market");
    let path = path.to_string_lossy().into_owned();
    assert_eq!(
        sandbox
            .run(&["plugin", "marketplace", "add", &path])
            .await
            .code,
        Some(0)
    );
    let install = sandbox.run(&["plugin", "install", "review@team"]).await;
    assert_eq!(install.code, Some(0), "{}", install.stderr);
    let out = &install.stdout;
    assert!(out.contains("1 hook(s)"), "{out}");
    assert!(
        out.contains("hook PreToolUse [mcp__plugin_review_srv__*]: ${CLAUDE_PLUGIN_ROOT}/check.sh --token ${REVIEW_TOKEN}"),
        "{out}"
    );
    assert!(
        out.contains("MCP server plugin_review_srv: ${CLAUDE_PLUGIN_ROOT}/serve --port 1 (env TOKEN=${REVIEW_TOKEN})"),
        "{out}"
    );
    assert!(
        out.contains("skill command in skills/review/SKILL.md: git branch --show-current"),
        "{out}"
    );
    assert!(out.contains("fingerprint"), "{out}");
    assert!(
        !out.contains("s3cr3t-value") && !install.stderr.contains("s3cr3t-value"),
        "{out}"
    );
    assert!(registry_text(&sandbox).contains("approved = \""));
}

#[tokio::test]
async fn plugin_update_records_the_approval_and_clears_held_and_staged_state() {
    let sandbox = Sandbox::new();
    add_and_install(&sandbox).await;
    let approved_at_install = registry_text(&sandbox)
        .lines()
        .find(|l| l.starts_with("approved"))
        .unwrap()
        .to_string();

    // State the background pass leaves behind: a held revision and a staged one.
    let mut registry: toml::Table = toml::from_str(&registry_text(&sandbox)).unwrap();
    for table in ["plugin", "marketplace"] {
        let entry = registry
            .get_mut(table)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .first_mut()
            .unwrap()
            .as_table_mut()
            .unwrap();
        entry.insert("staged_pin".into(), "deadbeefdeadbeef".into());
        if table == "plugin" {
            entry.insert("held_pin".into(), "deadbeefdeadbeef".into());
        }
    }
    std::fs::write(
        sandbox.config.join("plugins.toml"),
        toml::to_string(&registry).unwrap(),
    )
    .unwrap();
    let installed = sandbox.config.join("plugins/installed/team");
    std::fs::create_dir_all(installed.join(".staged-review")).unwrap();
    std::fs::create_dir_all(sandbox.config.join("plugins/marketplaces/.staged-team")).unwrap();

    let list = sandbox.run(&["plugin", "list"]).await;
    assert!(list.stdout.contains("held deadbee"), "{}", list.stdout);
    let json = sandbox.run(&["plugin", "list", "--format", "json"]).await;
    let value: serde_json::Value = serde_json::from_str(json.stdout.trim()).unwrap();
    assert_eq!(value["plugins"][0]["held_pin"], json!("deadbeefdeadbeef"));
    assert_eq!(value["plugins"][0]["auto_update"], json!(true));
    assert_eq!(value["marketplaces"][0]["auto_update"], json!(true));

    let update = sandbox.run(&["plugin", "update", "review"]).await;
    assert_eq!(update.code, Some(0), "{}", update.stderr);
    let text = registry_text(&sandbox);
    assert!(text.contains(&approved_at_install), "{text}");
    assert!(!text.contains("held_pin"), "{text}");
    // The plugin's staged revision is discarded; the marketplace's is its own entry's.
    assert!(!installed.join(".staged-review").exists());
    assert!(
        text.matches("staged_pin").count() == 1,
        "only the marketplace keeps one: {text}"
    );

    let update = sandbox
        .run(&["plugin", "marketplace", "update", "team"])
        .await;
    assert_eq!(update.code, Some(0), "{}", update.stderr);
    assert!(!registry_text(&sandbox).contains("staged_pin"));
    assert!(
        !sandbox
            .config
            .join("plugins/marketplaces/.staged-team")
            .exists()
    );
}

#[tokio::test]
async fn auto_update_can_be_turned_off_per_marketplace_and_per_plugin() {
    let sandbox = Sandbox::new();
    add_and_install(&sandbox).await;

    let off = sandbox
        .run(&["plugin", "marketplace", "auto-update", "team", "no"])
        .await;
    assert_eq!(off.code, Some(0), "{}", off.stderr);
    let off = sandbox
        .run(&["plugin", "auto-update", "review", "no"])
        .await;
    assert_eq!(off.code, Some(0), "{}", off.stderr);
    assert_eq!(
        registry_text(&sandbox)
            .matches("auto_update = false")
            .count(),
        2
    );

    let markets = sandbox.run(&["plugin", "marketplace", "list"]).await;
    assert!(
        markets.stdout.contains("auto-update off"),
        "{}",
        markets.stdout
    );
    let list = sandbox.run(&["plugin", "list"]).await;
    assert!(list.stdout.contains("auto-update off"), "{}", list.stdout);
    let json = sandbox.run(&["plugin", "list", "--format", "json"]).await;
    let value: serde_json::Value = serde_json::from_str(json.stdout.trim()).unwrap();
    assert_eq!(value["plugins"][0]["auto_update"], json!(false));
    assert_eq!(value["marketplaces"][0]["auto_update"], json!(false));

    let on = sandbox
        .run(&["plugin", "auto-update", "review", "yes"])
        .await;
    assert_eq!(on.code, Some(0), "{}", on.stderr);
    assert_eq!(
        registry_text(&sandbox)
            .matches("auto_update = false")
            .count(),
        1
    );

    let missing = sandbox.run(&["plugin", "auto-update", "nope", "no"]).await;
    assert_ne!(missing.code, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_headless_run_never_stages_swaps_or_stamps() {
    let endpoint =
        common::Endpoint::start(common::Wire::Chat, vec![common::Turn::text("done")]).await;
    let sandbox = Sandbox::compatible(&endpoint.url());
    add_and_install(&sandbox).await;

    // The source moved, and an earlier interactive run staged a revision of the marketplace.
    sandbox.add_file(
        "team-market/plugins/review/skills/review/SKILL.md",
        "---\ndescription: reviews a diff\n---\nNew body.",
    );
    let staged = sandbox.config.join("plugins/marketplaces/.staged-team");
    std::fs::create_dir_all(&staged).unwrap();
    let mut registry: toml::Table = toml::from_str(&registry_text(&sandbox)).unwrap();
    registry["marketplace"].as_array_mut().unwrap()[0]
        .as_table_mut()
        .unwrap()
        .insert("staged_pin".into(), "deadbeefdeadbeef".into());
    std::fs::write(
        sandbox.config.join("plugins.toml"),
        toml::to_string(&registry).unwrap(),
    )
    .unwrap();
    let before = registry_text(&sandbox);
    let live_skill = sandbox
        .config
        .join("plugins/installed/team/review/skills/review/SKILL.md");
    let live_before = std::fs::read_to_string(&live_skill).unwrap();

    let out = sandbox.headless(&["go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);

    assert_eq!(registry_text(&sandbox), before, "the registry is untouched");
    assert!(staged.is_dir(), "the staged revision is not swapped in");
    assert_eq!(std::fs::read_to_string(&live_skill).unwrap(), live_before);
    assert!(!sandbox.data.join("plugin-update.stamp").exists());
    assert!(!sandbox.data.join("plugin-updates.jsonl").exists());
}

fn trust_file(sandbox: &Sandbox) -> Option<String> {
    std::fs::read_to_string(sandbox.config.join("skill-trust.toml")).ok()
}

#[tokio::test]
async fn trust_refuses_without_a_terminal_and_records_the_digest_with_yes() {
    let sandbox = Sandbox::new();
    add_and_install(&sandbox).await;
    let installed = sandbox.config.join("plugins/installed/team/review");
    write_text(
        &installed.join("skills/review/SKILL.md"),
        "---\ndescription: reviews\nallowed-tools: Bash(git *), Read\n---\nRun !`git status` first.\n",
    );
    write_text(
        &installed.join("commands/sub/deploy.md"),
        "---\ndescription: deploys\nallowed-tools: Bash(make deploy)\n---\nGo.\n",
    );
    // A repository's skill is never read or named.
    sandbox.add_file(
        ".fontaine/skills/proj/SKILL.md",
        "---\nallowed-tools: Bash(ls)\n---\nbody\n",
    );

    let refused = sandbox.run(&["plugin", "trust", "review:review"]).await;
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(refused.stderr.contains("--yes"), "{}", refused.stderr);
    assert!(
        refused.stdout.contains("wildcard shell rules"),
        "{}",
        refused.stdout
    );
    assert!(trust_file(&sandbox).is_none());

    let trusted = sandbox
        .run(&["plugin", "trust", "review:review", "--yes"])
        .await;
    assert_eq!(trusted.code, Some(0), "{}", trusted.stderr);
    assert!(
        trusted.stdout.contains("next session start"),
        "{}",
        trusted.stdout
    );
    assert!(trusted.stdout.contains("!git status"), "{}", trusted.stdout);
    let text = trust_file(&sandbox).unwrap();
    assert!(text.contains("name = \"review:review\""), "{text}");
    assert!(text.contains("digest = \"sha256:"), "{text}");
    assert!(
        text.contains("shell(git *)") && text.contains("!git status"),
        "{text}"
    );

    // A multi-segment plugin skill resolves by its composed name.
    let deploy = sandbox
        .run(&["plugin", "trust", "review:sub:deploy", "--yes"])
        .await;
    assert_eq!(deploy.code, Some(0), "{}", deploy.stderr);

    let project = sandbox.run(&["plugin", "trust", "proj", "--yes"]).await;
    assert_eq!(project.code, Some(2));
    assert!(
        project.stderr.contains("no skill or installed plugin"),
        "{}",
        project.stderr
    );
}

#[tokio::test]
async fn trusting_a_plugin_records_each_of_its_skills_and_untrust_removes_them() {
    let sandbox = Sandbox::new();
    add_and_install(&sandbox).await;
    let both = sandbox.run(&["plugin", "trust", "review", "--yes"]).await;
    assert_eq!(both.code, Some(0), "{}", both.stderr);
    assert!(trust_file(&sandbox).unwrap().contains("review:review"));

    let gone = sandbox.run(&["plugin", "untrust", "review"]).await;
    assert_eq!(gone.code, Some(0), "{}", gone.stderr);
    assert!(!trust_file(&sandbox).unwrap().contains("review:review"));
    let again = sandbox.run(&["plugin", "untrust", "review"]).await;
    assert_eq!(again.code, Some(2));
}

#[tokio::test]
async fn trust_finds_user_directory_skills_and_refuses_an_ambiguous_name() {
    let sandbox = Sandbox::new();
    write_text(
        &sandbox.config.join("skills/mine/SKILL.md"),
        "---\ndescription: m\nallowed-tools: Bash(git *)\n---\nbody\n",
    );
    let ok = sandbox.run(&["plugin", "trust", "mine", "--yes"]).await;
    assert_eq!(ok.code, Some(0), "{}", ok.stderr);
    assert!(trust_file(&sandbox).unwrap().contains("name = \"mine\""));

    add_and_install(&sandbox).await;
    write_text(
        &sandbox.config.join("skills/review/SKILL.md"),
        "---\ndescription: r\n---\nbody\n",
    );
    let ambiguous = sandbox.run(&["plugin", "trust", "review", "--yes"]).await;
    assert_eq!(ambiguous.code, Some(2));
    assert!(ambiguous.stderr.contains("both"), "{}", ambiguous.stderr);
}

#[tokio::test]
async fn trust_and_untrust_refuse_to_overwrite_a_corrupt_record() {
    let sandbox = Sandbox::new();
    write_text(
        &sandbox.config.join("skills/mine/SKILL.md"),
        "---\ndescription: m\n---\nbody\n",
    );
    write_text(&sandbox.config.join("skill-trust.toml"), "not toml [\n");
    let trust = sandbox.run(&["plugin", "trust", "mine", "--yes"]).await;
    assert_eq!(trust.code, Some(2));
    let untrust = sandbox.run(&["plugin", "untrust", "mine"]).await;
    assert_eq!(untrust.code, Some(2));
    assert_eq!(trust_file(&sandbox).unwrap(), "not toml [\n");
}

fn write_text(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
