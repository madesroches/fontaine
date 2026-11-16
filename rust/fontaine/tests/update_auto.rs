//! The background update pass, rollback and the Windows helper's decisions, against a mock
//! release index and a disposable signing key. The helper's lock probe, clock and sleep are
//! injected so its logic runs on every OS.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fontaine::release_signature::{Channel, TrustedKeys};
#[cfg(unix)]
use fontaine::update;
use fontaine::update::Params;
#[cfg(unix)]
use fontaine::update::Rollback;
use fontaine::update::auto::{self, Mode, Pass};
use fontaine::update::helper::{self, Done, HelperArgs};
use minisign::KeyPair;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[cfg(windows)]
const TARGET: &str = "x86_64-pc-windows-msvc";
#[cfg(windows)]
const ASSET: &str = "fontaine-x86_64-pc-windows-msvc.zip";
#[cfg(windows)]
const BIN: &str = "fontaine.exe";
#[cfg(not(windows))]
const TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(not(windows))]
const ASSET: &str = "fontaine-x86_64-unknown-linux-gnu.tar.xz";
#[cfg(not(windows))]
const BIN: &str = "fontaine";
const REPO: &str = "/repos/madesroches/fontaine";
const OLD: &str = "0.1.0-dev.3+aaaaaaa";
const NEW: &str = "0.1.0-dev.5+bbbbbbb";

struct Signer(KeyPair);

impl Signer {
    fn new() -> Signer {
        Signer(KeyPair::generate_unencrypted_keypair().unwrap())
    }
    fn keys(&self) -> TrustedKeys {
        let other = Signer::new();
        TrustedKeys::from_pub_files(&other.pub_file(), &self.pub_file()).unwrap()
    }
    fn pub_file(&self) -> String {
        self.0.pk.to_box().unwrap().into_string()
    }
    fn sign(&self, data: &[u8], channel: &str, version: &str) -> String {
        let comment = format!(
            "fontaine-signature v1 channel={channel} file={ASSET} version={version} commit=abc"
        );
        minisign::sign(Some(&self.0.pk), &self.0.sk, data, Some(&comment), None)
            .unwrap()
            .into_string()
    }
}

#[cfg(windows)]
fn archive(binary: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(
            format!("fontaine-{TARGET}/{BIN}"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    writer.write_all(binary).unwrap();
    writer.finish().unwrap().into_inner()
}

#[cfg(not(windows))]
fn archive(binary: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(binary.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, format!("fontaine-{TARGET}/{BIN}"), binary)
        .unwrap();
    let tar = builder.into_inner().unwrap();
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut std::io::Cursor::new(tar), &mut xz).unwrap();
    xz
}

struct Install {
    _root: tempfile::TempDir,
    binary: PathBuf,
    receipt: PathBuf,
    data: PathBuf,
}

impl Install {
    fn new(version: &str) -> Install {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let prefix = root_path.join("install");
        std::fs::create_dir_all(&prefix).unwrap();
        let binary = prefix.join(BIN);
        std::fs::write(&binary, b"OLD").unwrap();
        let receipt = root_path.join("config").join("fontaine-receipt.json");
        std::fs::create_dir_all(receipt.parent().unwrap()).unwrap();
        std::fs::write(
            &receipt,
            serde_json::json!({
                "binaries": [BIN],
                "install_prefix": prefix,
                "install_layout": "flat",
                "version": version,
            })
            .to_string(),
        )
        .unwrap();
        Install {
            _root: root,
            binary,
            receipt,
            data: root_path.join("data"),
        }
    }

    fn receipt_text(&self) -> String {
        std::fs::read_to_string(&self.receipt).unwrap()
    }

    fn receipt_version(&self) -> String {
        let value: serde_json::Value = serde_json::from_str(&self.receipt_text()).unwrap();
        value["version"].as_str().unwrap().to_string()
    }

    fn log(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.data.join(auto::LOG_FILE))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn params<'a>(&self, server: &'a MockServer, keys: &'a TrustedKeys) -> Params<'a> {
        Params {
            api_base: Box::leak(server.uri().into_boxed_str()),
            receipt_path: self.receipt.clone(),
            current_exe: self.binary.clone(),
            target: TARGET,
            keys,
        }
    }

    fn bin_dir_extras(&self) -> Vec<String> {
        std::fs::read_dir(self.binary.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| !matches!(n.as_str(), BIN | "fontaine.prev" | "fontaine.prev.json"))
            .collect()
    }
}

struct Published<'a> {
    tag: &'a str,
    archive: Vec<u8>,
    signature: Option<String>,
}

async fn serve(server: &MockServer, releases: &[Published<'_>]) {
    let base = server.uri();
    let mut listing = Vec::new();
    for r in releases {
        let mut assets = vec![serde_json::json!({
            "name": ASSET,
            "browser_download_url": format!("{base}/dl/{}/{ASSET}", r.tag),
        })];
        Mock::given(method("GET"))
            .and(path(format!("/dl/{}/{ASSET}", r.tag)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(r.archive.clone()))
            .mount(server)
            .await;
        if let Some(sig) = &r.signature {
            assets.push(serde_json::json!({
                "name": format!("{ASSET}.minisig"),
                "browser_download_url": format!("{base}/dl/{}/{ASSET}.minisig", r.tag),
            }));
            Mock::given(method("GET"))
                .and(path(format!("/dl/{}/{ASSET}.minisig", r.tag)))
                .respond_with(ResponseTemplate::new(200).set_body_string(sig.clone()))
                .mount(server)
                .await;
        }
        listing.push(serde_json::json!({"tag_name": r.tag, "draft": false, "assets": assets}));
    }
    Mock::given(method("GET"))
        .and(path(format!("{REPO}/releases")))
        .respond_with(ResponseTemplate::new(200).set_body_json(&listing))
        .mount(server)
        .await;
}

async fn serve_signed(server: &MockServer, signer: &Signer, tag: &str, version: &str) {
    let bytes = archive(b"NEW");
    serve(
        server,
        &[Published {
            tag,
            signature: Some(signer.sign(&bytes, "dev", version)),
            archive: bytes,
        }],
    )
    .await;
}

fn no_helper(_: &HelperArgs) -> std::io::Result<()> {
    panic!("no helper on this path")
}

async fn pass(
    install: &Install,
    server: &MockServer,
    signer: &Signer,
    now: SystemTime,
) -> Vec<fontaine_core::notice::Notice> {
    let keys = signer.keys();
    let pass = Pass {
        data_dir: &install.data,
        now,
        mode: Mode::Install,
        launcher: &no_helper,
    };
    pass.run(&reqwest::Client::new(), &install.params(server, &keys))
        .await
}

fn downloads(requests: &[wiremock::Request]) -> usize {
    requests
        .iter()
        .filter(|r| r.url.path().starts_with("/dl/"))
        .count()
}

#[cfg(unix)]
#[tokio::test]
async fn a_newer_signed_build_is_installed_and_announced_once() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new(OLD);
    serve_signed(&server, &signer, "dev-5", NEW).await;

    let notices = pass(&install, &server, &signer, SystemTime::now()).await;
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"NEW");
    assert_eq!(install.receipt_version(), NEW);
    assert_eq!(
        std::fs::read(install.binary.with_extension("prev")).unwrap(),
        b"OLD"
    );
    assert_eq!(install.log().last().unwrap()["outcome"], "updated");
    assert!(install.bin_dir_extras().is_empty());

    let startup = auto::startup_notices(&install.receipt, &install.binary, &install.data);
    assert_eq!(startup.len(), 1, "{startup:?}");
    assert_eq!(
        startup[0].headline,
        "Fontaine updated to dev.5 (0.1.0-dev.5+bbbbbbb)"
    );
    assert!(!install.data.join(auto::MARKER_FILE).exists());
    assert!(auto::startup_notices(&install.receipt, &install.binary, &install.data).is_empty());
}

#[tokio::test]
async fn a_release_update_says_the_version() {
    let marker = auto::Marker {
        from: "0.1.0".into(),
        to: "0.2.0".into(),
        channel: Channel::Release,
    };
    let install = Install::new("0.2.0");
    auto::write_marker(&install.data, &marker).unwrap();
    let notices = auto::startup_notices(&install.receipt, &install.binary, &install.data);
    assert_eq!(notices[0].headline, "Fontaine updated to v0.2.0");
}

#[test]
fn a_marker_for_another_version_is_deleted_silently() {
    let install = Install::new(OLD);
    let marker = auto::Marker {
        from: "0.1.0-dev.2+ccccccc".into(),
        to: NEW.into(),
        channel: Channel::Dev,
    };
    auto::write_marker(&install.data, &marker).unwrap();
    assert!(auto::startup_notices(&install.receipt, &install.binary, &install.data).is_empty());
    assert!(!install.data.join(auto::MARKER_FILE).exists());
}

#[tokio::test]
async fn bad_archives_are_rejected_untouched_and_warned_about_once() {
    let signer = Signer::new();
    let good = archive(b"NEW");
    let cases: Vec<(&str, Option<String>)> = vec![
        ("unsigned", None),
        ("wrong key", Some(Signer::new().sign(&good, "dev", NEW))),
        ("wrong channel", Some(signer.sign(&good, "release", NEW))),
        (
            "wrong version",
            Some(signer.sign(&good, "dev", "0.1.0-dev.4+bbbbbbb")),
        ),
    ];
    for (name, signature) in cases {
        let server = MockServer::start().await;
        let install = Install::new(OLD);
        let before = install.receipt_text();
        serve(
            &server,
            &[Published {
                tag: "dev-5",
                archive: good.clone(),
                signature,
            }],
        )
        .await;
        let now = SystemTime::now();
        let notices = pass(&install, &server, &signer, now).await;
        assert_eq!(notices.len(), 1, "{name}: {notices:?}");
        assert!(notices[0].contains("dev-5"), "{name}");
        assert_eq!(std::fs::read(&install.binary).unwrap(), b"OLD", "{name}");
        assert_eq!(install.receipt_text(), before, "{name}");
        assert!(install.bin_dir_extras().is_empty(), "{name}");
        assert!(!install.data.join(auto::MARKER_FILE).exists());
        assert_eq!(install.log().last().unwrap()["outcome"], "verify-failed");

        // The next due pass fails the same way for the same candidate, and says nothing.
        let later = now + Duration::from_secs(25 * 3600);
        let notices = pass(&install, &server, &signer, later).await;
        assert!(notices.is_empty(), "{name}: {notices:?}");
        assert_eq!(install.log().len(), 2, "{name}");
    }
}

#[tokio::test]
async fn a_network_failure_is_logged_and_quiet() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let install = Install::new(OLD);
    let notices = pass(&install, &server, &Signer::new(), SystemTime::now()).await;
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(install.log()[0]["outcome"], "network-error");
    assert!(auto::startup_notices(&install.receipt, &install.binary, &install.data).is_empty());
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"OLD");
}

#[tokio::test]
async fn the_same_or_an_older_build_does_nothing() {
    for newest in ["dev-3", "dev-2"] {
        let server = MockServer::start().await;
        let install = Install::new(OLD);
        serve(
            &server,
            &[Published {
                tag: newest,
                archive: archive(b"NEW"),
                signature: None,
            }],
        )
        .await;
        let notices = pass(&install, &server, &Signer::new(), SystemTime::now()).await;
        assert!(notices.is_empty());
        assert_eq!(install.log()[0]["outcome"], "up-to-date");
        assert_eq!(downloads(&server.received_requests().await.unwrap()), 0);
        assert_eq!(std::fs::read(&install.binary).unwrap(), b"OLD");
    }
}

#[tokio::test]
async fn the_pass_runs_at_most_once_a_day() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new(OLD);
    serve(
        &server,
        &[Published {
            tag: "dev-3",
            archive: archive(b"x"),
            signature: None,
        }],
    )
    .await;
    let now = SystemTime::now();
    pass(&install, &server, &signer, now).await;
    let asked = server.received_requests().await.unwrap().len();
    pass(&install, &server, &signer, now + Duration::from_secs(3600)).await;
    assert_eq!(server.received_requests().await.unwrap().len(), asked);
    pass(
        &install,
        &server,
        &signer,
        now + Duration::from_secs(25 * 3600),
    )
    .await;
    assert!(server.received_requests().await.unwrap().len() > asked);
}

#[tokio::test]
async fn an_offline_pass_still_writes_the_stamp() {
    let install = Install::new(OLD);
    // A dropped mock server's port can be reused by a parallel test; port 1 is never served.
    let uri = "http://127.0.0.1:1".to_string();
    let keys = Signer::new().keys();
    let params = Params {
        api_base: &uri,
        receipt_path: install.receipt.clone(),
        current_exe: install.binary.clone(),
        target: TARGET,
        keys: &keys,
    };
    let pass = Pass {
        data_dir: &install.data,
        now: SystemTime::now(),
        mode: Mode::Install,
        launcher: &no_helper,
    };
    let notices = pass.run(&reqwest::Client::new(), &params).await;
    assert!(notices.is_empty(), "{notices:?}");
    assert!(install.data.join(auto::STAMP_FILE).exists());
}

#[tokio::test]
async fn a_pass_holding_the_lock_makes_a_second_one_stop() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new(OLD);
    serve_signed(&server, &signer, "dev-5", NEW).await;
    std::fs::create_dir_all(&install.data).unwrap();
    let held = fontaine_core::file_lock::try_lock(&install.data.join(auto::LOCK_FILE))
        .unwrap()
        .unwrap();
    pass(&install, &server, &signer, SystemTime::now()).await;
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!install.data.join(auto::STAMP_FILE).exists());
    drop(held);
    pass(&install, &server, &signer, SystemTime::now()).await;
    assert_eq!(install.receipt_version(), NEW);
}

#[cfg(unix)]
#[tokio::test]
async fn a_pass_racing_a_manual_update_installs_once() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new(OLD);
    serve_signed(&server, &signer, "dev-5", NEW).await;
    let keys = signer.keys();
    let params = install.params(&server, &keys);
    let client = reqwest::Client::new();
    let background = Pass {
        data_dir: &install.data,
        now: SystemTime::now(),
        mode: Mode::Install,
        launcher: &no_helper,
    };
    let (manual, notices) = tokio::join!(
        update::update(&client, &params),
        background.run(&client, &params)
    );
    manual.unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(install.receipt_version(), NEW);
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"NEW");
    let updated = install
        .log()
        .iter()
        .filter(|l| l["outcome"] == "updated")
        .count();
    assert!(updated <= 1);
    // Whichever lost the race found the build already installed: the old binary was kept once.
    assert_eq!(
        std::fs::read(install.binary.with_extension("prev")).unwrap(),
        b"OLD"
    );
}

#[test]
fn only_a_stale_work_dir_is_removed_at_the_start_of_a_pass() {
    let install = Install::new(OLD);
    let dir = install.binary.parent().unwrap();
    let fresh = dir.join(".fontaine-update-fresh");
    let old = dir.join(".fontaine-update-old");
    std::fs::create_dir(&fresh).unwrap();
    std::fs::create_dir(&old).unwrap();
    auto::clean_work_dirs(dir, SystemTime::now() + Duration::from_secs(25 * 3600));
    // Both are older than a day relative to that clock, so move the clock back for the fresh one.
    assert!(!fresh.exists() && !old.exists());
    std::fs::create_dir(&fresh).unwrap();
    auto::clean_work_dirs(dir, SystemTime::now());
    assert!(fresh.exists());
}

#[test]
fn the_pass_starts_only_for_an_interactive_receipt_install_that_opted_in() {
    let install = Install::new(OLD);
    let (receipt, exe) = (
        Some(install.receipt.as_path()),
        Some(install.binary.as_path()),
    );
    assert!(auto::should_start(false, true, receipt, exe));
    assert!(!auto::should_start(true, true, receipt, exe), "-p or send");
    assert!(!auto::should_start(false, false, receipt, exe), "opted out");
    assert!(!auto::should_start(false, true, None, exe));
    assert!(!auto::should_start(false, true, receipt, None));
    let other = install.binary.with_file_name("other");
    std::fs::write(&other, b"x").unwrap();
    assert!(
        !auto::should_start(false, true, receipt, Some(&other)),
        "other binary"
    );
    std::fs::remove_file(&install.receipt).unwrap();
    assert!(!auto::should_start(false, true, receipt, exe), "no receipt");
}

#[cfg(unix)]
#[tokio::test]
async fn rollback_restores_the_previous_build_and_the_skip_stops_a_reinstall() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new(OLD);
    serve_signed(&server, &signer, "dev-5", NEW).await;
    let now = SystemTime::now();
    pass(&install, &server, &signer, now).await;
    assert_eq!(install.receipt_version(), NEW);

    let result = update::rollback(&install.receipt, &install.binary, &install.data).unwrap();
    assert!(
        matches!(&result, Rollback::Restored { from, to, .. } if from == NEW && to == OLD),
        "{result:?}"
    );
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"OLD");
    assert_eq!(install.receipt_version(), OLD);

    let later = now + Duration::from_secs(25 * 3600);
    pass(&install, &server, &signer, later).await;
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"OLD", "skipped");

    // A manual update ignores the skip.
    let keys = signer.keys();
    update::update(&reqwest::Client::new(), &install.params(&server, &keys))
        .await
        .unwrap();
    assert_eq!(install.receipt_version(), NEW);

    // A build newer than the skipped one clears the skip.
    let server = MockServer::start().await;
    serve_signed(&server, &signer, "dev-6", "0.1.0-dev.6+ddddddd").await;
    pass(
        &install,
        &server,
        &signer,
        later + Duration::from_secs(25 * 3600),
    )
    .await;
    assert_eq!(install.receipt_version(), "0.1.0-dev.6+ddddddd");
    assert!(!install.data.join(auto::SKIP_FILE).exists());
}

#[cfg(unix)]
#[test]
fn rollback_without_a_sidecar_or_previous_binary_has_nothing_to_restore() {
    let install = Install::new(OLD);
    let none = update::rollback(&install.receipt, &install.binary, &install.data).unwrap();
    assert!(matches!(none, Rollback::NothingToRestore(_)));
    std::fs::write(install.binary.with_extension("prev"), b"PREV").unwrap();
    let none = update::rollback(&install.receipt, &install.binary, &install.data).unwrap();
    assert!(matches!(none, Rollback::NothingToRestore(_)), "no sidecar");
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"OLD");
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_receipt_write_is_warned_once_and_leaves_no_marker() {
    use std::os::unix::fs::PermissionsExt;
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new(OLD);
    serve_signed(&server, &signer, "dev-5", NEW).await;
    // Atomic rewrite needs the directory writable; the swap itself does not touch it.
    let dir = install.receipt.parent().unwrap();
    // The receipt lock file must exist already: creating it needs the directory too.
    std::fs::write(dir.join("fontaine-receipt.lock"), b"").unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(dir.join("probe"), b"").is_ok() {
        // Running as a user the mode does not bind (root): the failure cannot be provoked.
        return;
    }
    let notices = pass(&install, &server, &signer, SystemTime::now()).await;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("receipt"));
    assert!(!install.data.join(auto::MARKER_FILE).exists());
    assert_eq!(install.log().last().unwrap()["outcome"], "receipt-failed");
    assert_eq!(install.receipt_version(), OLD);
    // The swap completed, so the next due pass must not claim a second install.
    let again = pass(
        &install,
        &server,
        &signer,
        SystemTime::now() + Duration::from_secs(90_000),
    )
    .await;
    assert!(again.is_empty(), "{again:?}");
    // No second swap: the kept previous binary is still the original, not the installed build.
    assert_eq!(
        std::fs::read(install.binary.with_extension("prev")).unwrap(),
        b"OLD"
    );
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"NEW");
}

#[cfg(unix)]
#[test]
fn rollback_exits_3_for_an_install_without_a_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fontaine"))
        .args(["update", "--rollback"])
        .current_dir(tmp.path())
        .env("FONTAINE_CONFIG_DIR", tmp.path().join("config"))
        .env("FONTAINE_DATA_DIR", tmp.path().join("data"))
        .env("XDG_CONFIG_HOME", tmp.path().join("xdg"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
}

struct HelperRig {
    install: Install,
    signer: Signer,
    keys: TrustedKeys,
}

impl HelperRig {
    fn new() -> HelperRig {
        let signer = Signer::new();
        let keys = signer.keys();
        HelperRig {
            install: Install::new(OLD),
            signer,
            keys,
        }
    }

    fn stage_as(&self, signature: Option<&str>, channel: &str, version: &str, bytes: &[u8]) {
        let dir = self.install.data.join(auto::STAGED_DIR).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ASSET), bytes).unwrap();
        if let Some(sig) = signature {
            std::fs::write(dir.join(format!("{ASSET}.minisig")), sig).unwrap();
        }
        std::fs::write(
            dir.join(helper::MANIFEST_FILE),
            serde_json::json!({"channel": channel, "version": version}).to_string(),
        )
        .unwrap();
    }

    fn stage_good(&self, version: &str) {
        let bytes = archive(b"NEW");
        let sig = self.signer.sign(&bytes, "dev", version);
        self.stage_as(Some(&sig), "dev", version, &bytes);
    }

    fn args(&self) -> HelperArgs {
        HelperArgs {
            target: self.install.binary.clone(),
            receipt: self.install.receipt.clone(),
            data: self.install.data.clone(),
        }
    }

    fn run(&self, locked_for: u32, give_up: Duration) -> (Done, Vec<Duration>) {
        let polls = Cell::new(0);
        let slept = RefCell::new(Vec::new());
        let is_locked = |_: &Path| {
            polls.set(polls.get() + 1);
            polls.get() <= locked_for
        };
        let sleep = |d: Duration| slept.borrow_mut().push(d);
        let now = SystemTime::now;
        let env = helper::Env {
            keys: &self.keys,
            target_triple: TARGET,
            is_locked: &is_locked,
            sleep: &sleep,
            now: &now,
            poll: Duration::from_secs(2),
            give_up,
        };
        let done = helper::run(&self.args(), &env);
        (done, slept.into_inner())
    }

    fn stages(&self) -> usize {
        helper::stage_dirs(&self.install.data).len()
    }
}

#[test]
fn the_helper_waits_for_the_target_then_swaps_and_records_the_marker() {
    let rig = HelperRig::new();
    rig.stage_good(NEW);
    let (done, slept) = rig.run(3, Duration::from_secs(3600));
    assert_eq!(done, Done::Applied { to: NEW.into() });
    assert_eq!(slept.len(), 3);
    assert_eq!(std::fs::read(&rig.install.binary).unwrap(), b"NEW");
    assert_eq!(rig.install.receipt_version(), NEW);
    assert_eq!(rig.stages(), 0);
    let marker = std::fs::read_to_string(rig.install.data.join(auto::MARKER_FILE)).unwrap();
    assert!(marker.contains(NEW));
}

#[test]
fn the_helper_gives_up_after_the_limit_and_keeps_the_stage_and_warns_once() {
    let rig = HelperRig::new();
    rig.stage_good(NEW);
    let (done, slept) = rig.run(u32::MAX, Duration::from_secs(6));
    assert_eq!(done, Done::GaveUp);
    assert_eq!(slept.len(), 3);
    assert_eq!(std::fs::read(&rig.install.binary).unwrap(), b"OLD");
    assert_eq!(rig.stages(), 1);
    assert_eq!(rig.install.log().last().unwrap()["outcome"], "gave-up");

    let data = &rig.install.data;
    let first = auto::drain_pending(data).unwrap();
    assert!(first.is_some_and(|n| n.contains("kept running")));
    // A second give-up for the same build is logged but not shown again.
    rig.run(u32::MAX, Duration::from_secs(2));
    assert!(auto::drain_pending(data).unwrap().is_none());
}

#[test]
fn a_second_helper_exits_at_once() {
    let rig = HelperRig::new();
    rig.stage_good(NEW);
    std::fs::create_dir_all(&rig.install.data).unwrap();
    let held = fontaine_core::file_lock::try_lock(&rig.install.data.join(helper::HELPER_LOCK))
        .unwrap()
        .unwrap();
    assert_eq!(rig.run(0, Duration::from_secs(1)).0, Done::AlreadyRunning);
    assert_eq!(std::fs::read(&rig.install.binary).unwrap(), b"OLD");
    drop(held);
}

#[test]
fn a_stage_that_fails_reverification_is_deleted_and_logged() {
    let rig = HelperRig::new();
    let bytes = archive(b"EVIL");
    let sig = Signer::new().sign(&bytes, "dev", NEW);
    rig.stage_as(Some(&sig), "dev", NEW, &bytes);
    let (done, _) = rig.run(0, Duration::from_secs(1));
    assert_eq!(done, Done::Discarded);
    assert_eq!(std::fs::read(&rig.install.binary).unwrap(), b"OLD");
    assert_eq!(rig.stages(), 0);
    assert_eq!(
        rig.install.log().last().unwrap()["outcome"],
        "verify-failed"
    );
    // No helper is wanted next launch, and the next due pass is free to stage anew.
    let pending = auto::drain_pending(&rig.install.data).unwrap();
    assert!(pending.is_some());
}

#[test]
fn a_stage_for_another_channel_is_not_swapped() {
    let rig = HelperRig::new();
    let bytes = archive(b"REL");
    let sig = rig.signer.sign(&bytes, "release", "0.9.0");
    rig.stage_as(Some(&sig), "release", "0.9.0", &bytes);
    let (done, _) = rig.run(0, Duration::from_secs(1));
    assert_eq!(done, Done::Discarded);
    assert_eq!(std::fs::read(&rig.install.binary).unwrap(), b"OLD");
    assert_eq!(rig.stages(), 0);
    assert_eq!(
        rig.install.log().last().unwrap()["outcome"],
        "verify-failed"
    );
}

#[test]
fn a_stage_not_newer_than_the_receipt_is_deleted_without_a_failure() {
    let rig = HelperRig::new();
    rig.stage_good("0.1.0-dev.2+aaaaaaa");
    let (done, _) = rig.run(0, Duration::from_secs(1));
    assert_eq!(done, Done::Discarded);
    assert_eq!(rig.stages(), 0);
    assert!(rig.install.log().is_empty());
    assert_eq!(std::fs::read(&rig.install.binary).unwrap(), b"OLD");
}

#[test]
fn a_newer_build_installed_while_waiting_wins() {
    let rig = HelperRig::new();
    rig.stage_good(NEW);
    let receipt = rig.install.receipt.clone();
    let bump = |_: &Path| {
        let text = std::fs::read_to_string(&receipt).unwrap();
        if text.contains(NEW) {
            return false;
        }
        std::fs::write(&receipt, text.replace(OLD, "0.1.0-dev.9+eeeeeee")).unwrap();
        false
    };
    let sleep = |_: Duration| {};
    let now = SystemTime::now;
    let env = helper::Env {
        keys: &rig.keys,
        target_triple: TARGET,
        is_locked: &bump,
        sleep: &sleep,
        now: &now,
        poll: Duration::from_secs(2),
        give_up: Duration::from_secs(10),
    };
    assert_eq!(helper::run(&rig.args(), &env), Done::Discarded);
    assert_eq!(std::fs::read(&rig.install.binary).unwrap(), b"OLD");
    assert_eq!(rig.install.receipt_version(), "0.1.0-dev.9+eeeeeee");
    assert_eq!(rig.stages(), 0);
}

#[test]
fn the_helper_finds_the_receipt_through_the_target_path_not_its_own() {
    let rig = HelperRig::new();
    let mut args = rig.args();
    // A helper runs from a copy elsewhere; only the target path names the receipt's binary.
    let elsewhere = rig.install.binary.with_file_name("helper-copy");
    std::fs::write(&elsewhere, b"COPY").unwrap();
    rig.stage_good(NEW);
    let (done, _) = rig.run(0, Duration::from_secs(1));
    assert_eq!(done, Done::Applied { to: NEW.into() });
    args.target = elsewhere;
    let (nothing, _) = {
        let env = helper::Env {
            keys: &rig.keys,
            target_triple: TARGET,
            is_locked: &|_| false,
            sleep: &|_| {},
            now: &SystemTime::now,
            poll: Duration::from_secs(1),
            give_up: Duration::from_secs(1),
        };
        (helper::run(&args, &env), ())
    };
    assert_eq!(nothing, Done::NothingToDo);
}

#[tokio::test]
async fn windows_mode_stages_a_verified_build_and_starts_a_helper_without_replacing_it() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new(OLD);
    serve_signed(&server, &signer, "dev-5", NEW).await;
    let launched = RefCell::new(Vec::new());
    let launcher = |a: &HelperArgs| {
        launched.borrow_mut().push(a.clone());
        Ok(())
    };
    let keys = signer.keys();
    let pass = Pass {
        data_dir: &install.data,
        now: SystemTime::now(),
        mode: Mode::Stage,
        launcher: &launcher,
    };
    let notices = pass
        .run(&reqwest::Client::new(), &install.params(&server, &keys))
        .await;
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"OLD");
    assert_eq!(launched.borrow().len(), 1);
    assert_eq!(launched.borrow()[0].target, install.binary);
    let stage = install.data.join(auto::STAGED_DIR).join(NEW);
    assert!(stage.join(helper::MANIFEST_FILE).exists());
    assert!(stage.join(ASSET).exists());
    assert!(install.bin_dir_extras().is_empty());

    // The next launch, with a stage waiting: a helper starts with no stamp, update.lock or network.
    let before = server.received_requests().await.unwrap().len();
    std::fs::remove_file(install.data.join(auto::STAMP_FILE)).unwrap();
    let held = fontaine_core::file_lock::try_lock(&install.data.join(auto::LOCK_FILE))
        .unwrap()
        .unwrap();
    let notices = pass
        .run(&reqwest::Client::new(), &install.params(&server, &keys))
        .await;
    drop(held);
    assert!(notices.is_empty());
    assert_eq!(launched.borrow().len(), 2);
    assert_eq!(server.received_requests().await.unwrap().len(), before);
    assert!(!install.data.join(auto::STAMP_FILE).exists());
}

#[tokio::test]
async fn a_failed_helper_launch_is_warned_once_and_the_network_pass_goes_on() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new(OLD);
    serve_signed(&server, &signer, "dev-5", NEW).await;
    let launched = Cell::new(0);
    let launcher = |_: &HelperArgs| {
        launched.set(launched.get() + 1);
        Err(std::io::Error::other("no copy"))
    };
    let keys = signer.keys();
    let pass = Pass {
        data_dir: &install.data,
        now: SystemTime::now(),
        mode: Mode::Stage,
        launcher: &launcher,
    };
    let first = pass
        .run(&reqwest::Client::new(), &install.params(&server, &keys))
        .await;
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(
        first[0]
            .to_string()
            .contains("the update helper did not start")
    );
    // The stage is still waiting; the next start tries again, fails the same way and stays quiet.
    std::fs::remove_file(install.data.join(auto::STAMP_FILE)).unwrap();
    let second = pass
        .run(&reqwest::Client::new(), &install.params(&server, &keys))
        .await;
    assert!(second.is_empty(), "{second:?}");
    assert!(launched.get() >= 2);
    assert!(
        install.data.join(auto::STAMP_FILE).exists(),
        "the pass went on to the network step"
    );
}

#[tokio::test]
async fn a_stage_that_is_not_newer_is_dropped_and_the_network_pass_goes_on() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new(OLD);
    serve_signed(&server, &signer, "dev-5", NEW).await;
    let stale = install
        .data
        .join(auto::STAGED_DIR)
        .join("0.1.0-dev.2+aaaaaaa");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(
        stale.join(helper::MANIFEST_FILE),
        r#"{"channel":"dev","version":"0.1.0-dev.2+aaaaaaa"}"#,
    )
    .unwrap();
    let launched = Cell::new(0);
    let launcher = |_: &HelperArgs| {
        launched.set(launched.get() + 1);
        Ok(())
    };
    let keys = signer.keys();
    let pass = Pass {
        data_dir: &install.data,
        now: SystemTime::now(),
        mode: Mode::Stage,
        launcher: &launcher,
    };
    pass.run(&reqwest::Client::new(), &install.params(&server, &keys))
        .await;
    assert!(!stale.exists());
    assert_eq!(launched.get(), 1, "only the fresh stage starts a helper");
}
