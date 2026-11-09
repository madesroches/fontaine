//! Automatic updates of registered marketplaces and installed plugins.
//!
//! A background thread started at interactive session start fetches each upstream revision and
//! stages the validated ones beside the live checkout; the next interactive start swaps them in
//! before plugins load, so nothing executable changes under a running session. A plugin
//! revision applies only when its approval fingerprint equals the one recorded at install or at
//! the last manual `plugin update`; otherwise it is held, the old checkout stays live, and the
//! user is told to run `fontaine plugin update`.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fontaine_core::agent::EnvLookup;
use fontaine_core::dirs::Dirs;
use fontaine_core::file_lock;
use fontaine_core::notice::Notice;
use fontaine_core::plugin_ops::{self, cleanup_leftovers};
use fontaine_core::plugins::{
    self, InstalledPlugin, MarketplaceEntry, Registry, checkout_dir, marketplace_dir,
    registry_path, staged_dir,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::plugin::{
    MarketplaceFetch, MarketplaceOutcome, PluginFetch, PluginOutcome, UNATTENDED_GIT, Workdir,
    fetch_marketplace, fetch_plugin, read_catalog, remove_dir,
};
use fontaine_core::plugin_ops::{EnvRef, Fetch, PluginSource};

pub(crate) const STAMP_FILE: &str = "plugin-update.stamp";
pub(crate) const LOG_FILE: &str = "plugin-updates.jsonl";

/// An offline machine or a quick restart does not retry on every start.
const THROTTLE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Marketplace,
    Plugin,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Marketplace => "marketplace",
            Kind::Plugin => "plugin",
        }
    }
}

/// What happened to one entry, for the log and the notice. Entries that were already up to date
/// produce none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Staged; active from the next start.
    Moved {
        old: String,
        new: String,
    },
    /// A newer revision changes the executable surface the user approved.
    Held {
        new: String,
        reason: String,
    },
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Update {
    pub kind: Kind,
    pub name: String,
    pub outcome: Outcome,
}

impl Update {
    fn outcome_word(&self) -> &'static str {
        match self.outcome {
            Outcome::Moved { .. } => "moved",
            Outcome::Held { .. } => "held",
            Outcome::Failed { .. } => "failed",
        }
    }

    /// The line the interface shows once.
    pub(crate) fn notice(&self) -> Notice {
        let kind = self.kind.word();
        let name = &self.name;
        match &self.outcome {
            Outcome::Moved { new, .. } => Notice::info(format!(
                "{kind} `{name}` updated to {}; run /reload-plugins to apply it now, or it applies next session",
                plugins::short_pin(new)
            )),
            Outcome::Held { new, .. } => Notice::warning(
                format!(
                    "plugin `{name}` has a new revision ({}) that changes its hooks, servers, skill commands or scripts, so the old one stays active",
                    plugins::short_pin(new)
                ),
                Some(format!("run `/plugin update {name}` to review it")),
            ),
            Outcome::Failed { reason } => Notice::warning(
                format!("{kind} `{name}` could not auto-update: {reason}"),
                None,
            ),
        }
    }

    fn log_line(&self, now: SystemTime) -> String {
        let (old, new, reason) = match &self.outcome {
            Outcome::Moved { old, new } => (Some(old.as_str()), Some(new.as_str()), None),
            Outcome::Held { new, reason } => (None, Some(new.as_str()), Some(reason.as_str())),
            Outcome::Failed { reason } => (None, None, Some(reason.as_str())),
        };
        serde_json::json!({
            "timestamp": now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            "kind": self.kind.word(),
            "name": self.name,
            "old": old.map(plugins::short_pin),
            "new": new.map(plugins::short_pin),
            "outcome": self.outcome_word(),
            "reason": reason,
        })
        .to_string()
    }
}

/// Appends one JSON line per update to `<data dir>/plugin-updates.jsonl`.
pub(crate) fn append_log(
    data_dir: &Path,
    updates: &[Update],
    now: SystemTime,
) -> std::io::Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(data_dir)?;
    let mut text = String::new();
    for update in updates {
        text.push_str(&update.log_line(now));
        text.push('\n');
    }
    use std::io::Write;
    let mut file = fontaine_core::owner_only::open(&data_dir.join(LOG_FILE), true)?;
    file.write_all(text.as_bytes())
}

/// Whether the last pass is old enough for another.
pub(crate) fn stamp_due(data_dir: &Path, now: SystemTime) -> bool {
    let modified = std::fs::metadata(data_dir.join(STAMP_FILE)).and_then(|m| m.modified());
    match modified {
        // A stamp well in the future (the clock moved back) must not stall updates for good.
        Ok(at) => match now.duration_since(at) {
            Ok(age) => age >= THROTTLE,
            Err(ahead) => ahead.duration() > Duration::from_secs(60),
        },
        Err(_) => true,
    }
}

pub(crate) fn write_stamp(data_dir: &Path, now: SystemTime) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let seconds = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    fontaine_core::owner_only::write_atomic(
        &data_dir.join(STAMP_FILE),
        format!("{seconds}\n").as_bytes(),
    )
}

/// Starts the pass on its own thread. A plain `std::thread`, not `spawn_blocking`: a tokio
/// runtime waits for blocking tasks when it drops, and the process must not wait for a slow
/// network on exit. Outcomes reach the interface over `notices`.
pub(crate) fn spawn(dirs: Dirs, env: EnvLookup, notices: UnboundedSender<Notice>) {
    let sender = notices.clone();
    let started = std::thread::Builder::new()
        .name("plugin-auto-update".into())
        .spawn(move || run_background(&dirs, &*env, &notices));
    if let Err(e) = started {
        let _ = sender.send(Notice::warning(
            format!(
                "plugin auto-update did not start: {}",
                fontaine_tools::error_chain::chain(&e)
            ),
            None,
        ));
    }
}

fn run_background(dirs: &Dirs, env: EnvRef<'_>, notices: &UnboundedSender<Notice>) {
    let now = SystemTime::now();
    if !stamp_due(&dirs.data, now) {
        return;
    }
    let updates = Pass { dirs, env }.run(now);
    if let Err(e) = append_log(&dirs.data, &updates, now) {
        let _ = notices.send(Notice::warning(
            format!(
                "the plugin update log was not written: {}",
                fontaine_tools::error_chain::chain(&e)
            ),
            None,
        ));
    }
    for update in &updates {
        let _ = notices.send(update.notice());
    }
    // Written even when every fetch failed, so an offline machine does not retry each start.
    if let Err(e) = write_stamp(&dirs.data, now) {
        let _ = notices.send(Notice::warning(
            format!(
                "the plugin update stamp was not written, so the next start checks again: {}",
                fontaine_tools::error_chain::chain(&e)
            ),
            None,
        ));
    }
}

pub(crate) struct Pass<'a> {
    pub dirs: &'a Dirs,
    pub env: EnvRef<'a>,
}

impl Pass<'_> {
    fn fx(&self) -> Fetch<'_> {
        Fetch {
            config_dir: &self.dirs.config,
            env: self.env,
            git: UNATTENDED_GIT,
        }
    }

    /// Fetches and stages every entry that has not opted out. A failing entry leaves its old
    /// checkout live and does not stop the rest. Marketplaces go first, so a path-source
    /// plugin follows its marketplace in the same pass.
    pub(crate) fn run(&self, now: SystemTime) -> Vec<Update> {
        let mut updates = Vec::new();
        let Ok(snapshot) = plugins::read_registry(&self.dirs.config) else {
            // `load_installed` already tells the user about an unusable registry.
            return updates;
        };
        if snapshot.marketplace.is_empty() && snapshot.plugin.is_empty() {
            return updates;
        }
        // Skipped when another process holds the lock: the next pass cleans up.
        let _ = self.locked(|registry| {
            cleanup_leftovers(&self.dirs.config, registry, now);
            Ok(None::<()>)
        });
        for entry in snapshot
            .marketplace
            .iter()
            .filter(|m| m.auto_update != Some(false))
        {
            let outcome = market_outcome(self.marketplace(entry));
            updates.extend(report(Kind::Marketplace, &entry.name, outcome));
        }
        // Staging changed the registry; the plugins see the marketplaces' staged revisions.
        let Ok(registry) = plugins::read_registry(&self.dirs.config) else {
            return updates;
        };
        for plugin in registry
            .plugin
            .iter()
            .filter(|p| p.auto_update != Some(false))
        {
            let outcome = plugin_outcome(self.plugin(&registry, plugin));
            updates.extend(report(Kind::Plugin, &plugin.name, outcome));
        }
        updates
    }

    /// Runs `change` on a fresh read of the registry under its exclusive lock. It returns
    /// `Some` when it changed the registry, which is then written back. `None` is "nothing to
    /// do": another process holds the lock, or the entry moved since the snapshot.
    fn locked<R>(
        &self,
        change: impl FnOnce(&mut Registry) -> Result<Option<R>, String>,
    ) -> Result<Option<R>, String> {
        let lock = registry_path(&self.dirs.config).with_extension("lock");
        let guard = file_lock::try_lock(&lock).map_err(|e| {
            format!(
                "cannot lock {}: {}",
                lock.display(),
                fontaine_tools::error_chain::chain(&e)
            )
        })?;
        let Some(_guard) = guard else {
            return Ok(None);
        };
        let mut registry = plugins::read_registry(&self.dirs.config).map_err(|e| e.to_string())?;
        let Some(value) = change(&mut registry)? else {
            return Ok(None);
        };
        plugins::write_registry(&self.dirs.config, &registry).map_err(|e| e.to_string())?;
        Ok(Some(value))
    }

    fn marketplace(&self, entry: &MarketplaceEntry) -> MarketplaceOutcome {
        let mut known = vec![entry.pin.as_str()];
        known.extend(entry.staged_pin.as_deref());
        let (temp, new) = match fetch_marketplace(&self.fx(), entry, Workdir::Auto, &known) {
            Ok(MarketplaceFetch::UpToDate) => {
                return MarketplaceOutcome::UpToDate {
                    pin: entry.pin.clone(),
                };
            }
            Ok(MarketplaceFetch::Ready { temp, new }) => (temp, new),
            Err(e) => return MarketplaceOutcome::Failed(e),
        };
        let checkout = marketplace_dir(&self.dirs.config, &entry.name);
        let staged = self.locked(|registry| {
            // A manual update or another session moved the pin since the snapshot.
            let Some(at) = registry
                .marketplace
                .iter()
                .position(|m| m.name == entry.name && m.pin == entry.pin)
            else {
                return Ok(None);
            };
            put_staged(&temp, &checkout)?;
            registry.marketplace[at].staged_pin = Some(new.clone());
            Ok(Some(()))
        });
        // Staged, the scratch directory is gone already; otherwise it is only waste.
        remove_dir(&temp);
        match staged {
            Ok(Some(())) => MarketplaceOutcome::Moved {
                old: entry.pin.clone(),
                new,
            },
            Ok(None) => MarketplaceOutcome::UpToDate {
                pin: entry.pin.clone(),
            },
            Err(e) => MarketplaceOutcome::Failed(e),
        }
    }

    fn plugin(&self, registry: &Registry, plugin: &InstalledPlugin) -> PluginOutcome {
        // Installed by an older build: nothing records what the user approved.
        let Some(approved) = plugin.approved.as_deref() else {
            return PluginOutcome::UpToDate {
                pin: plugin.pin.clone(),
            };
        };
        let Some(market) = registry
            .marketplace
            .iter()
            .find(|m| m.name == plugin.marketplace)
        else {
            return PluginOutcome::Failed(format!(
                "the marketplace `{}` is not registered",
                plugin.marketplace
            ));
        };
        let live = marketplace_dir(&self.dirs.config, &market.name);
        // A staged marketplace carries the newest catalog, and a path source follows its pin.
        let (catalog_dir, market_pin) = match &market.staged_pin {
            Some(staged_pin) if staged_dir(&live).is_dir() => (staged_dir(&live), staged_pin),
            _ => (live, &market.pin),
        };
        let catalog = match read_catalog(&catalog_dir) {
            Ok(catalog) => catalog,
            Err(e) => return PluginOutcome::Failed(e.into()),
        };
        let Some(entry) = catalog.plugins.iter().find(|p| p.name == plugin.name) else {
            return PluginOutcome::Failed(format!(
                "the catalog of `{}` no longer has it",
                market.name
            ));
        };
        let source = PluginSource {
            market,
            checkout: &catalog_dir,
            pin: market_pin,
        };
        // A revision already staged or already held is neither fetched nor announced again.
        let mut known = vec![plugin.pin.as_str()];
        known.extend(plugin.staged_pin.as_deref());
        known.extend(plugin.held_pin.as_deref());
        let fetched = match fetch_plugin(&self.fx(), &source, entry, Workdir::Auto, &known) {
            Ok(PluginFetch::UpToDate) => {
                return PluginOutcome::UpToDate {
                    pin: plugin.pin.clone(),
                };
            }
            Ok(PluginFetch::Ready(fetched)) => *fetched,
            Err(e) => return PluginOutcome::Failed(e.into()),
        };
        let reason = match plugins::approval_fingerprint(&fetched.temp) {
            Ok(fingerprint) if fingerprint == approved => None,
            Ok(_) => Some(
                "its hooks, servers, skill commands or scripts differ from what you approved"
                    .to_string(),
            ),
            Err(e) => Some(format!("its executable surface could not be checked: {e}")),
        };
        let checkout = checkout_dir(&self.dirs.config, &plugin.marketplace, &plugin.name);
        let new = fetched.pin;
        let temp = fetched.temp;
        let held = reason.clone();
        let recorded = self.locked(|registry| {
            let Some(record) = registry
                .plugin
                .iter_mut()
                .find(|p| p.name == plugin.name && p.pin == plugin.pin)
            else {
                return Ok(None);
            };
            match &held {
                Some(_) => {
                    record.held_pin = Some(new.clone());
                }
                None => {
                    put_staged(&temp, &checkout)?;
                    record.staged_pin = Some(new.clone());
                    record.held_pin = None;
                    record.source = entry.source.display();
                }
            }
            Ok(Some(()))
        });
        // A held revision is not kept, only its pin; a staged one has been moved out already.
        remove_dir(&temp);
        match (recorded, reason) {
            (Err(e), _) => PluginOutcome::Failed(e),
            (Ok(None), _) => PluginOutcome::UpToDate {
                pin: plugin.pin.clone(),
            },
            (Ok(Some(())), Some(reason)) => PluginOutcome::Held { new, reason },
            (Ok(Some(())), None) => PluginOutcome::Moved {
                old: plugin.pin.clone(),
                new,
            },
        }
    }
}

fn market_outcome(outcome: MarketplaceOutcome) -> Option<Outcome> {
    match outcome {
        MarketplaceOutcome::UpToDate { .. } => None,
        MarketplaceOutcome::Moved { old, new } => Some(Outcome::Moved { old, new }),
        MarketplaceOutcome::Failed(reason) => Some(Outcome::Failed { reason }),
    }
}

fn plugin_outcome(outcome: PluginOutcome) -> Option<Outcome> {
    match outcome {
        PluginOutcome::UpToDate { .. } => None,
        PluginOutcome::Moved { old, new } => Some(Outcome::Moved { old, new }),
        PluginOutcome::Held { new, reason } => Some(Outcome::Held { new, reason }),
        PluginOutcome::Failed(reason) => Some(Outcome::Failed { reason }),
    }
}

fn report(kind: Kind, name: &str, outcome: Option<Outcome>) -> Option<Update> {
    outcome.map(|outcome| Update {
        kind,
        name: name.to_string(),
        outcome,
    })
}

/// Moves a validated scratch directory beside the live checkout as its staged revision,
/// replacing an older staged one.
fn put_staged(temp: &Path, checkout: &Path) -> Result<(), String> {
    let staged = staged_dir(checkout);
    remove_dir(&staged);
    std::fs::rename(temp, &staged).map_err(|e| {
        format!(
            "cannot stage {}: {}",
            staged.display(),
            fontaine_tools::error_chain::chain(&e)
        )
    })
}

/// Applies the revisions earlier passes staged, before plugins load. Returns warnings for the
/// ones that could not be applied; those stay staged and are retried at the next start.
pub(crate) fn apply_staged(config_dir: &Path, now: SystemTime) -> Vec<String> {
    plugin_ops::apply_staged(config_dir, now, &mut |_| true).warnings
}

#[cfg(test)]
mod tests;
