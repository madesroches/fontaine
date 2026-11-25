//! The background half of self-update: the cadence, the one-pass lock, the log, the once-shown
//! warning, the applied marker and the Windows hand-off. Download, verification and the swap
//! itself are `prepare` and `install`, shared with `fontaine update`.

use std::cmp::Ordering;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fontaine_sdk::core::file_lock;
use fontaine_sdk::core::notice::Notice;
use fontaine_sdk::tools::error_chain::chain;
use tokio::sync::mpsc::UnboundedSender;

use super::helper::{self, HelperArgs, Manifest};
use super::receipt::{self, InstallInfo};
use super::release_index::{self, Build};
use super::{Outcome, Params, Prepare, PrepareOpts, Prepared, UpdateError, WORK_PREFIX};
use crate::release_signature::Channel;
use crate::stamp::{stamp_due, write_stamp};

pub const STAMP_FILE: &str = "update.stamp";
pub const LOCK_FILE: &str = "update.lock";
pub const LOG_FILE: &str = "updates.jsonl";
pub const MARKER_FILE: &str = "update-applied.json";
pub const STATE_FILE: &str = "update-state.json";
pub const SKIP_FILE: &str = "update-skip";
pub const STAGED_DIR: &str = "update-staged";

/// A work directory this old belongs to a crashed pass; a younger one may be a concurrent manual
/// update's or the Windows helper's.
const STALE_WORK: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether the background pass starts at all. `-p`, `fontaine send` and the other
/// non-interactive surfaces never do, and neither does an install that no receipt describes
/// (a package manager, `cargo install`, a container or a moved binary).
pub fn should_start(
    headless: bool,
    auto_update: bool,
    receipt_path: Option<&Path>,
    current_exe: Option<&Path>,
) -> bool {
    !headless
        && auto_update
        && matches!(
            (receipt_path, current_exe),
            (Some(receipt_path), Some(exe)) if receipt::locate(receipt_path, exe).is_ok()
        )
}

/// How a pass applies a verified build. Windows cannot replace the running executable, so it
/// stages the build for a helper that swaps once nothing runs from the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Install,
    Stage,
}

impl Mode {
    pub fn native() -> Mode {
        if cfg!(windows) {
            Mode::Stage
        } else {
            Mode::Install
        }
    }
}

/// A failure of a check or an update that the user is told about once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: String,
    pub candidate: Option<String>,
    pub reason: String,
}

impl Failure {
    pub fn new(kind: &str, candidate: Option<&str>, reason: impl Into<String>) -> Failure {
        Failure {
            kind: kind.to_string(),
            candidate: candidate.map(str::to_string),
            reason: reason.into(),
        }
    }

    fn notice(&self) -> Notice {
        let what = match &self.candidate {
            Some(candidate) => format!("the automatic update to {candidate} failed"),
            None => "the automatic update failed".to_string(),
        };
        Notice::warning(
            format!("{what}: {}", self.reason),
            Some(
                "run `fontaine update` to retry, or set [updates] auto_update = false".to_string(),
            ),
        )
    }
}

fn seconds(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Which build a log line is about.
#[derive(Clone, Copy)]
pub struct Subject<'a> {
    pub channel: Channel,
    pub installed: &'a str,
    pub candidate: Option<&'a str>,
}

/// Appends one JSON line to `<data>/updates.jsonl`.
pub fn append_log(
    data_dir: &Path,
    subject: Subject<'_>,
    outcome: &str,
    reason: Option<&str>,
    now: SystemTime,
) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let mut line = serde_json::json!({
        "timestamp": seconds(now),
        "channel": subject.channel.as_str(),
        "installed": subject.installed,
        "candidate": subject.candidate,
        "outcome": outcome,
        "reason": reason,
    })
    .to_string();
    line.push('\n');
    let mut file = fontaine_sdk::core::owner_only::open(&data_dir.join(LOG_FILE), true)?;
    file.write_all(line.as_bytes())
}

fn read_state(data_dir: &Path) -> serde_json::Value {
    std::fs::read_to_string(data_dir.join(STATE_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

fn write_state(data_dir: &Path, state: &serde_json::Value) -> std::io::Result<()> {
    fontaine_sdk::core::owner_only::write_atomic(
        &data_dir.join(STATE_FILE),
        state.to_string().as_bytes(),
    )
}

fn same_failure(entry: &serde_json::Value, failure: &Failure) -> bool {
    entry["kind"].as_str() == Some(failure.kind.as_str())
        && entry["candidate"].as_str() == failure.candidate.as_deref()
}

/// Logs a failure and queues it for the next interactive launch to show, unless that same
/// `(candidate, kind)` was already shown. Shared with the Windows helper, which has no interface.
pub fn record_failure(
    data_dir: &Path,
    subject: Subject<'_>,
    failure: &Failure,
    now: SystemTime,
) -> std::io::Result<()> {
    let logged = append_log(data_dir, subject, &failure.kind, Some(&failure.reason), now);
    let _guard = file_lock::lock(&data_dir.join(STATE_FILE))?;
    let mut state = read_state(data_dir);
    if !same_failure(&state["warned"], failure) {
        state["pending"] = serde_json::json!({
            "kind": failure.kind,
            "candidate": failure.candidate,
            "reason": failure.reason,
        });
        write_state(data_dir, &state)?;
    }
    logged
}

/// The warning for a failure nobody was told about yet; marks it as told.
pub fn drain_pending(data_dir: &Path) -> std::io::Result<Option<Notice>> {
    if !data_dir.join(STATE_FILE).exists() {
        return Ok(None);
    }
    let _guard = file_lock::lock(&data_dir.join(STATE_FILE))?;
    let mut state = read_state(data_dir);
    let Some(pending) = state.as_object_mut().and_then(|o| o.remove("pending")) else {
        return Ok(None);
    };
    let failure = Failure {
        kind: pending["kind"].as_str().unwrap_or("failed").to_string(),
        candidate: pending["candidate"].as_str().map(str::to_string),
        reason: pending["reason"].as_str().unwrap_or("unknown").to_string(),
    };
    let notice = (!same_failure(&state["warned"], &failure)).then(|| failure.notice());
    state["warned"] = serde_json::json!({"kind": failure.kind, "candidate": failure.candidate});
    write_state(data_dir, &state)?;
    Ok(notice)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Marker {
    pub from: String,
    pub to: String,
    pub channel: Channel,
}

impl Marker {
    fn notice(&self) -> Notice {
        let updated = Outcome::Updated {
            channel: self.channel,
            from: self.from.clone(),
            to: self.to.clone(),
            receipt_warning: None,
        };
        match self.channel {
            Channel::Dev => Notice::info(format!("Fontaine {}", updated.report())),
            Channel::Release => Notice::info(format!("Fontaine updated to v{}", self.to)),
        }
    }
}

/// Records that a background swap completed, for the next launch to announce.
pub fn write_marker(data_dir: &Path, marker: &Marker) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let text = serde_json::json!({
        "from": marker.from,
        "to": marker.to,
        "channel": marker.channel.as_str(),
    })
    .to_string();
    fontaine_sdk::core::owner_only::write_atomic(&data_dir.join(MARKER_FILE), text.as_bytes())
}

fn read_marker(data_dir: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(data_dir.join(MARKER_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(Marker {
        from: value["from"].as_str()?.to_string(),
        to: value["to"].as_str()?.to_string(),
        channel: Channel::parse(value["channel"].as_str()?)?,
    })
}

/// What an interactive start tells the user before the background pass runs: a failure of an
/// earlier pass, and a finished update. The marker is compared with the receipt's version for the
/// running executable, not `VERSION`, which for a dev build never equals the signed string.
pub fn startup_notices(receipt_path: &Path, current_exe: &Path, data_dir: &Path) -> Vec<Notice> {
    let mut notices = Vec::new();
    match drain_pending(data_dir) {
        Ok(Some(notice)) => notices.push(notice),
        Ok(None) => {}
        Err(e) => notices.push(Notice::warning(
            format!(
                "the automatic update state could not be read: {}",
                chain(&e)
            ),
            None,
        )),
    }
    let Ok(info) = receipt::locate(receipt_path, current_exe) else {
        return notices;
    };
    let marker_path = data_dir.join(MARKER_FILE);
    if marker_path.exists() {
        match read_marker(data_dir) {
            Some(marker) if marker.to == info.version => notices.push(marker.notice()),
            _ => {}
        }
        if let Err(e) = std::fs::remove_file(&marker_path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            notices.push(Notice::warning(
                format!(
                    "the update notice marker could not be removed: {}",
                    chain(&e)
                ),
                None,
            ));
        }
    }
    notices
}

/// Records the build `fontaine update --rollback` rolled back from, so the background pass does
/// not install it again.
pub fn write_skip(data_dir: &Path, channel: Channel, version: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let text = serde_json::json!({"channel": channel.as_str(), "version": version}).to_string();
    fontaine_sdk::core::owner_only::write_atomic(&data_dir.join(SKIP_FILE), text.as_bytes())
}

fn read_skip(data_dir: &Path, channel: Channel) -> Option<Build> {
    let text = std::fs::read_to_string(data_dir.join(SKIP_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    if value["channel"].as_str()? != channel.as_str() {
        return None;
    }
    release_index::installed_build(channel, value["version"].as_str()?)
}

/// Removes work directories of a crashed pass; younger ones are left alone.
pub fn clean_work_dirs(binary_dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(binary_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry.file_name().to_string_lossy().starts_with(WORK_PREFIX)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|at| now.duration_since(at).ok())
                .is_some_and(|age| age > STALE_WORK);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

pub struct Pass<'a> {
    pub data_dir: &'a Path,
    pub now: SystemTime,
    pub mode: Mode,
    /// Starts the Windows helper; injected so the hand-off runs on every OS without one.
    pub launcher: &'a dyn Fn(&HelperArgs) -> std::io::Result<()>,
}

impl Pass<'_> {
    /// One background pass. Returns the warnings to show now; everything else goes to the log.
    pub async fn run(&self, client: &reqwest::Client, params: &Params<'_>) -> Vec<Notice> {
        let mut notices = Vec::new();
        let Ok(info) = receipt::locate(&params.receipt_path, &params.current_exe) else {
            return notices;
        };
        if let Some(dir) = info.binary_path.parent() {
            clean_work_dirs(dir, self.now);
        }
        if self.mode == Mode::Stage && self.hand_off_waiting_stage(&info, &mut notices) {
            return notices;
        }
        if !stamp_due(self.data_dir, STAMP_FILE, self.now) {
            return notices;
        }
        if let Err(e) = std::fs::create_dir_all(self.data_dir) {
            notices.push(self.warn("the update state directory cannot be created", &e));
            return notices;
        }
        let _pass_lock = match file_lock::try_lock(&self.data_dir.join(LOCK_FILE)) {
            Ok(Some(lock)) => lock,
            Ok(None) => return notices,
            Err(e) => {
                notices.push(self.warn("the update lock cannot be taken", &e));
                return notices;
            }
        };
        // Before any network call, so an offline machine retries tomorrow and two launches at
        // once check once.
        if let Err(e) = write_stamp(self.data_dir, STAMP_FILE, self.now) {
            notices.push(self.warn(
                "the update stamp was not written, so the next start checks again",
                &e,
            ));
        }

        let skip = read_skip(self.data_dir, info.channel);
        let mut opts = PrepareOpts {
            skip,
            candidate: None,
        };
        let result = super::prepare(client, params, &mut opts).await;
        if let (Some(skip), Some(tag)) = (skip, &opts.candidate)
            && release_index::tag_build(info.channel, tag)
                .is_some_and(|b| release_index::compare(skip, b) == Some(Ordering::Greater))
        {
            let _ = std::fs::remove_file(self.data_dir.join(SKIP_FILE));
        }
        let candidate = opts.candidate.as_deref();
        let subject = Subject {
            channel: info.channel,
            installed: &info.version,
            candidate,
        };
        match result {
            // A pass for an install that stopped matching its receipt has nothing to say.
            Err(UpdateError::NotSelfUpdatable { .. }) => {}
            Err(UpdateError::Network(cause)) => {
                self.log(
                    subject,
                    "network-error",
                    Some(&chain(&*cause)),
                    &mut notices,
                );
            }
            Err(e @ UpdateError::Verify(_)) => {
                self.fail(subject, "verify-failed", &chain(&e), &mut notices);
            }
            Err(e @ UpdateError::Install(_)) => {
                self.fail(subject, "install-failed", &chain(&e), &mut notices);
            }
            Ok(Prepare::UpToDate { .. }) => self.log(subject, "up-to-date", None, &mut notices),
            Ok(Prepare::Ready(prepared)) => match self.mode {
                Mode::Install => self.install_now(*prepared, subject, &mut notices),
                Mode::Stage => self.stage(&prepared, &info, subject, &mut notices),
            },
        }
        notices
    }

    fn warn(&self, what: &str, e: &(dyn std::error::Error + 'static)) -> Notice {
        Notice::warning(format!("{what}: {}", chain(e)), None)
    }

    fn log(
        &self,
        subject: Subject<'_>,
        outcome: &str,
        reason: Option<&str>,
        notices: &mut Vec<Notice>,
    ) {
        if let Err(e) = append_log(self.data_dir, subject, outcome, reason, self.now) {
            notices.push(self.warn("the update log was not written", &e));
        }
    }

    fn fail(&self, subject: Subject<'_>, kind: &str, reason: &str, notices: &mut Vec<Notice>) {
        let failure = Failure::new(kind, subject.candidate, reason);
        if let Err(e) = record_failure(self.data_dir, subject, &failure, self.now) {
            notices.push(self.warn("the update failure could not be recorded", &e));
        }
        match drain_pending(self.data_dir) {
            Ok(Some(notice)) => notices.push(notice),
            Ok(None) => {}
            Err(e) => notices.push(self.warn("the update state could not be read", &e)),
        }
    }

    fn install_now(&self, prepared: Prepared, subject: Subject<'_>, notices: &mut Vec<Notice>) {
        match super::install(prepared) {
            Ok(Outcome::Updated {
                receipt_warning: None,
                channel,
                from,
                to,
            }) => {
                let marker = Marker { from, to, channel };
                if let Err(e) = write_marker(self.data_dir, &marker) {
                    notices.push(self.warn("the update notice could not be saved", &e));
                }
                self.log(subject, "updated", None, notices);
            }
            Ok(Outcome::Updated {
                receipt_warning: Some(why),
                ..
            }) => {
                let reason = format!(
                    "the binary was replaced but the install receipt was not updated: {why}"
                );
                self.fail(subject, "receipt-failed", &reason, notices);
            }
            Ok(Outcome::UpToDate { .. }) => self.log(subject, "up-to-date", None, notices),
            Err(e @ UpdateError::Verify(_)) => {
                self.fail(subject, "verify-failed", &chain(&e), notices);
            }
            Err(e) => self.fail(subject, "swap-failed", &chain(&e), notices),
        }
    }

    fn stage(
        &self,
        prepared: &Prepared,
        info: &InstallInfo,
        subject: Subject<'_>,
        notices: &mut Vec<Notice>,
    ) {
        if let Err(e) = helper::stage(self.data_dir, prepared) {
            self.fail(
                subject,
                "install-failed",
                &format!("the update could not be staged: {}", chain(&e)),
                notices,
            );
            return;
        }
        self.log(subject, "staged", None, notices);
        self.launch(info, notices);
    }

    /// Whether the helper is running (started now or already).
    fn launch(&self, info: &InstallInfo, notices: &mut Vec<Notice>) -> bool {
        let args = HelperArgs {
            target: info.binary_path.clone(),
            receipt: info.receipt_path.clone(),
            data: self.data_dir.to_path_buf(),
        };
        match (self.launcher)(&args) {
            Ok(()) => true,
            Err(e) => {
                let subject = Subject {
                    channel: info.channel,
                    installed: &info.version,
                    candidate: None,
                };
                let reason = format!("the update helper did not start: {}", chain(&e));
                self.fail(subject, "helper-failed", &reason, notices);
                false
            }
        }
    }

    /// A stage a previous launch left (the target was running, or the helper gave up) is applied
    /// without a network check. Returns whether a helper is running; a failed launch lets the pass
    /// go on to the network check.
    fn hand_off_waiting_stage(&self, info: &InstallInfo, notices: &mut Vec<Notice>) -> bool {
        let mut needs_helper = false;
        for dir in helper::stage_dirs(self.data_dir) {
            let Ok(manifest) = helper::read_manifest(&dir) else {
                // The helper deletes and reports a stage it cannot read.
                needs_helper |= dir.join(helper::MANIFEST_FILE).exists();
                continue;
            };
            if manifest_is_newer(info, &manifest) != Some(false) {
                needs_helper = true;
            } else {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        needs_helper && self.launch(info, notices)
    }
}

/// `Some(false)` when the stage is for this channel and not newer than the install; `None` when
/// it cannot be compared (another channel), which the helper reports and deletes.
fn manifest_is_newer(info: &InstallInfo, manifest: &Manifest) -> Option<bool> {
    if manifest.channel != info.channel {
        return None;
    }
    let installed = release_index::installed_build(info.channel, &info.version)?;
    let candidate = release_index::installed_build(info.channel, &manifest.version)?;
    Some(release_index::compare(installed, candidate) == Some(Ordering::Greater))
}

/// Starts the pass on its own thread, as the plugin pass does: a tokio runtime waits for blocking
/// tasks when it drops, and the process must not wait for a slow network on exit.
pub(crate) fn spawn(
    data_dir: PathBuf,
    receipt_path: PathBuf,
    current_exe: PathBuf,
    network: fontaine_sdk::tools::tls::LoadedNetwork,
    notices: UnboundedSender<Notice>,
) {
    let sender = notices.clone();
    let started = std::thread::Builder::new()
        .name("auto-update".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = notices.send(Notice::warning(
                        format!("automatic update did not start: {}", chain(&e)),
                        None,
                    ));
                    return;
                }
            };
            let client = match super::build_client(&network) {
                Ok(client) => client,
                Err(e) => {
                    let _ = notices.send(Notice::warning(
                        format!("automatic update did not start: {}", chain(&e)),
                        None,
                    ));
                    return;
                }
            };
            let params = Params {
                download_base: release_index::DEFAULT_DOWNLOAD_BASE,
                receipt_path,
                current_exe,
                target: env!("FONTAINE_BUILD_TARGET"),
                keys: crate::release_signature::TrustedKeys::embedded(),
            };
            let pass = Pass {
                data_dir: &data_dir,
                now: SystemTime::now(),
                mode: Mode::native(),
                launcher: &helper::launch,
            };
            for notice in runtime.block_on(pass.run(&client, &params)) {
                let _ = notices.send(notice);
            }
        });
    if let Err(e) = started {
        let _ = sender.send(Notice::warning(
            format!("automatic update did not start: {}", chain(&e)),
            None,
        ));
    }
}
