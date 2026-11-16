//! The Windows half of background update. The running executable is locked, so a session never
//! replaces itself: the pass stages a verified archive, and a detached copy of the trusted binary
//! (the helper) waits until no Fontaine process runs from the target, then re-verifies and swaps.
//! The logic takes its lock probe and clock as arguments, so it runs on every OS.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fontaine_sdk::core::file_lock;
use fontaine_sdk::tools::error_chain::chain;

use super::auto::{self, Failure, Marker, Subject};
use super::receipt::{self, InstallInfo};
use super::release_index::{self, Build, Release};
use super::{Outcome, Prepared, UpdateError};
use crate::release_signature::{Channel, TrustedKeys};

pub const MANIFEST_FILE: &str = "manifest.json";
pub const HELPER_LOCK: &str = "update-helper.lock";
#[cfg(windows)]
const HELPER_DIR: &str = "update-helper";
const POLL: Duration = Duration::from_secs(2);
const GIVE_UP: Duration = Duration::from_secs(24 * 60 * 60);

/// What the helper is told: the swap target, the receipt that describes it and the data dir. The
/// helper never derives these from its own location, which is a copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperArgs {
    pub target: PathBuf,
    pub receipt: PathBuf,
    pub data: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    pub channel: Channel,
    pub version: String,
}

fn staged_root(data_dir: &Path) -> PathBuf {
    data_dir.join(auto::STAGED_DIR)
}

pub fn stage_dirs(data_dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(staged_root(data_dir))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE))
        .map_err(|e| format!("the stage manifest cannot be read: {}", chain(&e)))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "the stage manifest is not valid JSON")?;
    let channel = value["channel"]
        .as_str()
        .and_then(Channel::parse)
        .ok_or("the stage manifest names no channel")?;
    let version = value["version"]
        .as_str()
        .ok_or("the stage manifest names no version")?;
    Ok(Manifest {
        channel,
        version: version.to_string(),
    })
}

/// Copies the verified archive, its signature and a manifest into `<data>/update-staged/<version>`
/// and removes every other stage. Copied, not renamed: the data dir may be on another filesystem.
/// The manifest goes last and by rename, so a stage with a manifest is complete.
pub fn stage(data_dir: &Path, prepared: &Prepared) -> std::io::Result<()> {
    let root = staged_root(data_dir);
    let dir = root.join(&prepared.verified.version);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let name = prepared
        .archive
        .file_name()
        .expect("an archive path has a file name");
    std::fs::copy(&prepared.archive, dir.join(name))?;
    std::fs::copy(
        prepared.signature_path(),
        super::signature_file(&dir.join(name)),
    )?;
    let text = serde_json::json!({
        "channel": prepared.verified.channel.as_str(),
        "version": prepared.verified.version,
    })
    .to_string();
    let tmp = dir.join(format!("{MANIFEST_FILE}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, dir.join(MANIFEST_FILE))?;
    for other in stage_dirs(data_dir) {
        if other != dir {
            let _ = std::fs::remove_dir_all(other);
        }
    }
    Ok(())
}

/// Everything the helper's decisions depend on that is not the file system.
pub struct Env<'a> {
    pub keys: &'a TrustedKeys,
    pub target_triple: &'a str,
    /// Whether some process still runs from the target, so it cannot be replaced.
    pub is_locked: &'a dyn Fn(&Path) -> bool,
    pub sleep: &'a dyn Fn(Duration),
    pub now: &'a dyn Fn() -> SystemTime,
    pub poll: Duration,
    pub give_up: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    /// Another helper holds the lock.
    AlreadyRunning,
    /// The receipt does not describe the target, or no stage is waiting.
    NothingToDo,
    Applied {
        to: String,
    },
    /// A bad or stale stage was deleted.
    Discarded,
    GaveUp,
    Failed,
}

fn release_of(manifest: &Manifest, asset: &str) -> Option<Release> {
    let build = match manifest.channel {
        Channel::Release => {
            release_index::tag_build(Channel::Release, &format!("v{}", manifest.version))?
        }
        Channel::Dev => Build::Dev(release_index::dev_build_number(&manifest.version)?),
    };
    Some(Release {
        tag: match build {
            Build::Dev(n) => format!("dev-{n}"),
            Build::Release(_) => format!("v{}", manifest.version),
        },
        build,
        assets: vec![release_index::Asset {
            name: asset.to_string(),
            url: String::new(),
        }],
    })
}

/// Waits for the target to be free, then applies the newest stage. Every failure is recorded for
/// the next interactive launch; the helper has no interface of its own.
pub fn run(args: &HelperArgs, env: &Env<'_>) -> Done {
    let lock = match file_lock::try_lock(&args.data.join(HELPER_LOCK)) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Done::AlreadyRunning,
        Err(_) => return Done::Failed,
    };
    let _lock = lock;
    // The target path stands for `current_exe`: this process is a copy and never the target.
    let Ok(info) = receipt::locate(&args.receipt, &args.target) else {
        return Done::NothingToDo;
    };
    let mut stages = stage_dirs(&args.data);
    sort_oldest_first(&mut stages);
    let mut result = Done::NothingToDo;
    // Newest last; `stage` keeps one, but a crash can leave two.
    while let Some(dir) = stages.pop() {
        let outcome = apply_stage(args, env, &info, &dir);
        match outcome {
            // A discarded stage says nothing about the older ones, which may still be valid.
            Done::NothingToDo => {}
            Done::Discarded => {
                if result == Done::NothingToDo {
                    result = Done::Discarded;
                }
            }
            other => {
                result = other;
                for rest in stages.drain(..) {
                    let _ = std::fs::remove_dir_all(rest);
                }
            }
        }
    }
    result
}

/// Orders stages by the build their manifest names, not by directory name: `dev.10` sorts before
/// `dev.9` as text. A stage with an unusable manifest sorts first, so it is tried last.
fn sort_oldest_first(stages: &mut [PathBuf]) {
    let build_of = |dir: &Path| {
        let manifest = read_manifest(dir).ok()?;
        release_index::installed_build(manifest.channel, &manifest.version)
    };
    let mut keyed: Vec<_> = stages
        .iter()
        .map(|dir| (build_of(dir), dir.clone()))
        .collect();
    keyed.sort_by(|(a, _), (b, _)| match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => release_index::compare(*b, *a).unwrap_or(std::cmp::Ordering::Equal),
    });
    for (slot, (_, dir)) in stages.iter_mut().zip(keyed) {
        *slot = dir;
    }
}

fn apply_stage(args: &HelperArgs, env: &Env<'_>, info: &InstallInfo, dir: &Path) -> Done {
    let record = |kind: &str, candidate: Option<&str>, reason: &str| {
        let candidate = candidate.map(str::to_string);
        let subject = Subject {
            channel: info.channel,
            installed: &info.version,
            candidate: candidate.as_deref(),
        };
        let failure = Failure::new(kind, candidate.as_deref(), reason);
        let _ = auto::record_failure(&args.data, subject, &failure, (env.now)());
    };
    let discard = |kind: &str, candidate: Option<&str>, reason: &str| {
        record(kind, candidate, reason);
        let _ = std::fs::remove_dir_all(dir);
        Done::Discarded
    };

    let manifest = match read_manifest(dir) {
        Ok(manifest) => manifest,
        Err(why) => return discard("verify-failed", None, &why),
    };
    // The release tag, the form the background pass records, so one build has one candidate key.
    let tag = release_of(&manifest, "").map(|r| r.tag);
    let candidate = Some(tag.as_deref().unwrap_or(manifest.version.as_str()));
    if manifest.channel != info.channel {
        return discard(
            "verify-failed",
            candidate,
            "the staged update is for a different channel than the install",
        );
    }
    let (Some(installed), Some(staged)) = (
        release_index::installed_build(info.channel, &info.version),
        release_index::installed_build(info.channel, &manifest.version),
    ) else {
        return discard("verify-failed", candidate, "the staged version is unusable");
    };
    if release_index::compare(installed, staged) != Some(std::cmp::Ordering::Greater) {
        let _ = std::fs::remove_dir_all(dir);
        return Done::Discarded;
    }

    let mut waited = Duration::ZERO;
    while (env.is_locked)(&args.target) {
        if waited >= env.give_up {
            record(
                "gave-up",
                candidate,
                "Fontaine kept running from the install for a day, so the update was not applied",
            );
            return Done::GaveUp;
        }
        (env.sleep)(env.poll);
        waited += env.poll;
    }

    let name = super::archive_name(env.target_triple);
    let archive = dir.join(&name);
    let signature = std::fs::read_to_string(super::signature_file(&archive)).ok();
    let verified = match crate::release_signature::verify_file(
        env.keys,
        info.channel,
        &archive,
        &name,
        signature.as_deref(),
    ) {
        Ok(verified) => verified,
        Err(e) => return discard("verify-failed", candidate, &chain(&e)),
    };
    let Some(release) = release_of(&manifest, &name) else {
        return discard("verify-failed", candidate, "the staged version is unusable");
    };
    if let Err(why) = super::check_binding(info.channel, &release, &name, &verified) {
        return discard("verify-failed", candidate, &why);
    }

    let prepared = match Prepared::new(info.clone(), archive, verified) {
        Ok(prepared) => prepared,
        Err(e) => {
            record("swap-failed", candidate, &chain(&e));
            return Done::Failed;
        }
    };
    let subject = Subject {
        channel: info.channel,
        installed: &info.version,
        candidate,
    };
    match super::install(prepared) {
        Ok(Outcome::Updated {
            channel,
            from,
            to,
            receipt_warning: None,
        }) => {
            let _ = auto::append_log(&args.data, subject, "updated", None, (env.now)());
            if let Err(e) = auto::write_marker(
                &args.data,
                &Marker {
                    from,
                    to: to.clone(),
                    channel,
                },
            ) {
                record(
                    "marker-failed",
                    candidate,
                    &format!(
                        "the update was applied but its notice could not be saved: {}",
                        chain(&e)
                    ),
                );
            }
            let _ = std::fs::remove_dir_all(dir);
            Done::Applied { to }
        }
        Ok(Outcome::Updated {
            receipt_warning: Some(why),
            ..
        }) => {
            record(
                "receipt-failed",
                candidate,
                &format!("the binary was replaced but the install receipt was not updated: {why}"),
            );
            let _ = std::fs::remove_dir_all(dir);
            Done::Failed
        }
        // A newer build was installed while this stage waited.
        Ok(Outcome::UpToDate { .. }) => {
            let _ = std::fs::remove_dir_all(dir);
            Done::Discarded
        }
        Err(e @ UpdateError::Verify(_)) => discard("verify-failed", candidate, &chain(&e)),
        Err(e) => {
            record("swap-failed", candidate, &chain(&e));
            Done::Failed
        }
    }
}

/// Whether the target is held open by a running process. Windows refuses to open a running
/// executable for writing; elsewhere the question never comes up.
pub fn target_in_use(target: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(target)
            .is_err()
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        false
    }
}

/// The hidden `update-helper` subcommand.
pub fn main(args: &HelperArgs) -> std::process::ExitCode {
    let env = Env {
        keys: TrustedKeys::embedded(),
        target_triple: env!("FONTAINE_BUILD_TARGET"),
        is_locked: &target_in_use,
        sleep: &std::thread::sleep,
        now: &SystemTime::now,
        poll: POLL,
        give_up: GIVE_UP,
    };
    match run(args, &env) {
        Done::Failed => std::process::ExitCode::from(1),
        _ => std::process::ExitCode::SUCCESS,
    }
}

/// Starts a detached copy of the running executable as the helper, unless one already runs. The
/// copy is what is locked while it waits, never the target.
pub fn launch(args: &HelperArgs) -> std::io::Result<()> {
    if file_lock::try_lock(&args.data.join(HELPER_LOCK))?.is_none() {
        return Ok(());
    }
    spawn_copy(args)
}

#[cfg(windows)]
fn spawn_copy(args: &HelperArgs) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let dir = args.data.join(HELPER_DIR);
    std::fs::create_dir_all(&dir)?;
    // A running helper's copy cannot be deleted and is skipped.
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
    let copy = dir.join(format!("fontaine-helper-{}.exe", std::process::id()));
    std::fs::copy(std::env::current_exe()?, &copy)?;
    // The helper outlives the session; inheriting its cwd would keep the project dir in use.
    std::process::Command::new(copy)
        .current_dir(&args.data)
        .arg("update-helper")
        .arg("--target")
        .arg(&args.target)
        .arg("--receipt")
        .arg(&args.receipt)
        .arg("--data")
        .arg(&args.data)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(drop)
}

#[cfg(not(windows))]
fn spawn_copy(_: &HelperArgs) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the update helper exists only on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_sort_by_build_number_not_by_name() {
        let data = tempfile::tempdir().unwrap();
        let mut dirs = Vec::new();
        for version in ["0.1.0-dev.10+bbbbbbb", "0.1.0-dev.9+aaaaaaa"] {
            let dir = staged_root(data.path()).join(version);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(MANIFEST_FILE),
                serde_json::json!({"channel": "dev", "version": version}).to_string(),
            )
            .unwrap();
            dirs.push(dir);
        }
        sort_oldest_first(&mut dirs);
        assert!(dirs[0].ends_with("0.1.0-dev.9+aaaaaaa"));
        assert!(dirs[1].ends_with("0.1.0-dev.10+bbbbbbb"));
    }

    #[cfg(windows)]
    #[test]
    fn target_in_use_tracks_an_open_handle() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fontaine.exe");
        std::fs::write(&file, b"x").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file)
            .unwrap();
        assert!(target_in_use(&file));
        drop(held);
        assert!(!target_in_use(&file));
    }
}
