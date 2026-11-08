//! `fontaine update` against a mock release index, with a disposable signing key standing in for
//! the embedded one. The archives are `.tar.xz`, which is what the non-Windows targets ship.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use fontaine::release_signature::{Channel, TrustedKeys};
use fontaine::update::{self, Outcome, Params, UpdateError};
use minisign::KeyPair;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TARGET: &str = "x86_64-unknown-linux-gnu";
const ASSET: &str = "fontaine-x86_64-unknown-linux-gnu.tar.xz";
const REPO: &str = "/repos/madesroches/fontaine";

struct Signer(KeyPair);

impl Signer {
    fn new() -> Signer {
        Signer(KeyPair::generate_unencrypted_keypair().unwrap())
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

fn archive(binary: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(binary.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(
            &mut header,
            "fontaine-x86_64-unknown-linux-gnu/fontaine",
            binary,
        )
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
}

impl Install {
    fn new(version: &str) -> Install {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().canonicalize().unwrap();
        let bin = prefix.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let binary = bin.join("fontaine");
        std::fs::write(&binary, b"OLD").unwrap();
        let receipt = prefix.join("config").join("fontaine-receipt.json");
        std::fs::create_dir_all(receipt.parent().unwrap()).unwrap();
        std::fs::write(
            &receipt,
            serde_json::json!({
                "binaries": ["fontaine"],
                "install_prefix": prefix,
                "install_layout": "flat",
                "version": version,
                "provider": {"source": "cargo-dist", "version": "0.32.0"},
            })
            .to_string(),
        )
        .unwrap();
        Install {
            _root: root,
            binary,
            receipt,
        }
    }

    fn receipt_version(&self) -> String {
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&self.receipt).unwrap()).unwrap();
        value["version"].as_str().unwrap().to_string()
    }

    /// Everything left in the bin directory besides the binary.
    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(self.binary.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n != "fontaine")
            .collect()
    }
}

struct Published<'a> {
    tag: &'a str,
    archive: Vec<u8>,
    signature: Option<String>,
}

async fn serve(server: &MockServer, channel: Channel, releases: &[Published<'_>]) {
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
    match channel {
        Channel::Dev => {
            Mock::given(method("GET"))
                .and(path(format!("{REPO}/releases")))
                .respond_with(ResponseTemplate::new(200).set_body_json(&listing))
                .mount(server)
                .await;
        }
        Channel::Release => {
            Mock::given(method("GET"))
                .and(path(format!("{REPO}/releases/latest")))
                .respond_with(ResponseTemplate::new(200).set_body_json(&listing[0]))
                .mount(server)
                .await;
        }
    }
}

async fn run_update(
    server: &MockServer,
    install: &Install,
    signer: &Signer,
) -> Result<Outcome, UpdateError> {
    let other = Signer::new();
    let keys = TrustedKeys::from_pub_files(&signer.pub_file(), &other.pub_file()).unwrap();
    let dev_keys = TrustedKeys::from_pub_files(&other.pub_file(), &signer.pub_file()).unwrap();
    // The signer is the release key for release-channel installs and the dev key otherwise.
    let keys = if std::fs::read_to_string(&install.receipt).is_ok_and(|r| r.contains("-dev.")) {
        &dev_keys
    } else {
        &keys
    };
    let params = Params {
        api_base: &server.uri(),
        receipt_path: install.receipt.clone(),
        current_exe: install.binary.clone(),
        target: TARGET,
        keys,
    };
    update::update(&reqwest::Client::new(), &params).await
}

#[tokio::test]
async fn dev_update_replaces_the_binary_and_the_receipt_version() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new("0.1.0-dev.3+aaaaaaa");
    let new = archive(b"NEW");
    serve(
        &server,
        Channel::Dev,
        &[
            Published {
                tag: "dev-5",
                signature: Some(signer.sign(&new, "dev", "0.1.0-dev.5+bbbbbbb")),
                archive: new,
            },
            Published {
                tag: "dev-4",
                archive: archive(b"MID"),
                signature: None,
            },
        ],
    )
    .await;
    let outcome = run_update(&server, &install, &signer).await.unwrap();
    assert!(matches!(&outcome, Outcome::Updated { to, .. } if to == "0.1.0-dev.5+bbbbbbb"));
    assert_eq!(outcome.report(), "updated to dev.5 (0.1.0-dev.5+bbbbbbb)");
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"NEW");
    assert_eq!(install.receipt_version(), "0.1.0-dev.5+bbbbbbb");
    let receipt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&install.receipt).unwrap()).unwrap();
    assert_eq!(receipt["install_layout"], "flat");
    assert!(install.leftovers().is_empty(), "{:?}", install.leftovers());
}

#[tokio::test]
async fn release_update_follows_the_latest_release() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new("0.1.0");
    let new = archive(b"NEW");
    serve(
        &server,
        Channel::Release,
        &[Published {
            tag: "v0.2.0",
            signature: Some(signer.sign(&new, "release", "0.2.0")),
            archive: new,
        }],
    )
    .await;
    let outcome = run_update(&server, &install, &signer).await.unwrap();
    assert_eq!(outcome.report(), "updated to 0.2.0");
    assert_eq!(install.receipt_version(), "0.2.0");
}

async fn assert_untouched(install: &Install, version: &str) {
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"OLD");
    assert_eq!(install.receipt_version(), version);
    assert!(install.leftovers().is_empty(), "{:?}", install.leftovers());
}

#[tokio::test]
async fn a_signature_by_another_key_installs_nothing() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new("0.1.0-dev.3+aaaaaaa");
    let new = archive(b"EVIL");
    serve(
        &server,
        Channel::Dev,
        &[Published {
            tag: "dev-5",
            signature: Some(Signer::new().sign(&new, "dev", "0.1.0-dev.5+bbbbbbb")),
            archive: new,
        }],
    )
    .await;
    let err = run_update(&server, &install, &signer).await.unwrap_err();
    assert!(matches!(err, UpdateError::Verify(_)), "{err:?}");
    assert_eq!(err.exit_code(), update::EXIT_UPDATE_VERIFY);
    assert_untouched(&install, "0.1.0-dev.3+aaaaaaa").await;
}

#[tokio::test]
async fn a_tampered_archive_installs_nothing() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new("0.1.0-dev.3+aaaaaaa");
    let signed = archive(b"NEW");
    let signature = signer.sign(&signed, "dev", "0.1.0-dev.5+bbbbbbb");
    serve(
        &server,
        Channel::Dev,
        &[Published {
            tag: "dev-5",
            signature: Some(signature),
            archive: archive(b"EVIL"),
        }],
    )
    .await;
    let err = run_update(&server, &install, &signer).await.unwrap_err();
    assert!(matches!(err, UpdateError::Verify(_)), "{err:?}");
    assert_untouched(&install, "0.1.0-dev.3+aaaaaaa").await;
}

#[tokio::test]
async fn an_unsigned_archive_installs_nothing() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new("0.1.0-dev.3+aaaaaaa");
    serve(
        &server,
        Channel::Dev,
        &[Published {
            tag: "dev-5",
            archive: archive(b"EVIL"),
            signature: None,
        }],
    )
    .await;
    let err = run_update(&server, &install, &signer).await.unwrap_err();
    assert!(matches!(err, UpdateError::Verify(_)), "{err:?}");
    assert_untouched(&install, "0.1.0-dev.3+aaaaaaa").await;
}

#[tokio::test]
async fn a_signed_build_replayed_under_a_newer_tag_installs_nothing() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new("0.1.0-dev.3+aaaaaaa");
    let old = archive(b"OLDER");
    serve(
        &server,
        Channel::Dev,
        &[Published {
            tag: "dev-9",
            signature: Some(signer.sign(&old, "dev", "0.1.0-dev.4+bbbbbbb")),
            archive: old,
        }],
    )
    .await;
    let err = run_update(&server, &install, &signer).await.unwrap_err();
    assert!(matches!(err, UpdateError::Verify(_)), "{err:?}");
    assert_untouched(&install, "0.1.0-dev.3+aaaaaaa").await;
}

#[tokio::test]
async fn the_same_or_an_older_build_is_up_to_date_and_downloads_nothing() {
    for newest in ["dev-3", "dev-2"] {
        let server = MockServer::start().await;
        let signer = Signer::new();
        let install = Install::new("0.1.0-dev.3+aaaaaaa");
        serve(
            &server,
            Channel::Dev,
            &[Published {
                tag: newest,
                archive: archive(b"NEW"),
                signature: None,
            }],
        )
        .await;
        let outcome = run_update(&server, &install, &signer).await.unwrap();
        assert!(matches!(outcome, Outcome::UpToDate { .. }), "{newest}");
        let downloads = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().starts_with("/dl/"))
            .count();
        assert_eq!(downloads, 0);
        assert_untouched(&install, "0.1.0-dev.3+aaaaaaa").await;
    }
}

#[tokio::test]
async fn an_index_failure_is_a_network_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let signer = Signer::new();
    let install = Install::new("0.1.0-dev.3+aaaaaaa");
    let err = run_update(&server, &install, &signer).await.unwrap_err();
    assert_eq!(err.exit_code(), update::EXIT_UPDATE_NETWORK);
    assert_untouched(&install, "0.1.0-dev.3+aaaaaaa").await;
}

#[tokio::test]
async fn an_install_without_a_receipt_is_not_self_updatable_and_writes_nothing() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    let install = Install::new("0.1.0-dev.3+aaaaaaa");
    let config_dir = install.receipt.parent().unwrap().to_path_buf();
    std::fs::remove_file(&install.receipt).unwrap();
    let err = run_update(&server, &install, &signer).await.unwrap_err();
    assert!(
        matches!(err, UpdateError::NotSelfUpdatable { .. }),
        "{err:?}"
    );
    assert_eq!(err.exit_code(), update::EXIT_UPDATE_NOT_SELF_UPDATABLE);
    assert_eq!(std::fs::read(&install.binary).unwrap(), b"OLD");
    assert_eq!(std::fs::read_dir(config_dir).unwrap().count(), 0);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn the_subcommand_exits_3_for_an_install_it_cannot_replace() {
    let tmp = tempfile::tempdir().unwrap();
    let root: &Path = tmp.path();
    let config = root.join("config");
    std::fs::create_dir_all(&config).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fontaine"))
        .arg("update")
        .current_dir(root)
        .env("FONTAINE_CONFIG_DIR", &config)
        .env("FONTAINE_DATA_DIR", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot update itself"), "{stderr}");
    assert!(stderr.contains("fontaine-installer.sh"), "{stderr}");
    assert!(!root.join("xdg").exists());
}
