//! `fontaine update`: replace the installed binary with the newest build on the channel it was
//! installed from, after checking the archive against that channel's embedded key.

pub mod install;
pub mod receipt;
pub mod release_index;

use std::cmp::Ordering;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use fontaine_core::config::Config;
use fontaine_tools::error_chain::chain;

use crate::release_signature::{Channel, TrustedKeys, Verified};
use receipt::{InstallInfo, ReceiptError};
use release_index::{Build, Release};

pub const EXIT_UPDATE_NOT_SELF_UPDATABLE: u8 = 3;
pub const EXIT_UPDATE_NETWORK: u8 = 4;
pub const EXIT_UPDATE_VERIFY: u8 = 5;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_REDIRECTS: usize = 5;
const MAX_ARCHIVE: u64 = 512 << 20;
const MAX_SIGNATURE: usize = 64 << 10;

type Cause = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
pub enum UpdateError {
    NotSelfUpdatable {
        channel: Option<Channel>,
        cause: Cause,
    },
    Network(Cause),
    Verify(Cause),
    Install(Cause),
}

impl UpdateError {
    pub fn exit_code(&self) -> u8 {
        match self {
            UpdateError::NotSelfUpdatable { .. } => EXIT_UPDATE_NOT_SELF_UPDATABLE,
            UpdateError::Network(_) => EXIT_UPDATE_NETWORK,
            UpdateError::Verify(_) => EXIT_UPDATE_VERIFY,
            UpdateError::Install(_) => 1,
        }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::NotSelfUpdatable { .. } => {
                f.write_str("this install cannot update itself")
            }
            UpdateError::Network(_) => f.write_str("the update could not be fetched"),
            UpdateError::Verify(_) => {
                f.write_str("the downloaded update failed verification; nothing was changed")
            }
            UpdateError::Install(_) => f.write_str("the update could not be installed"),
        }
    }
}

impl std::error::Error for UpdateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UpdateError::NotSelfUpdatable { cause, .. }
            | UpdateError::Network(cause)
            | UpdateError::Verify(cause)
            | UpdateError::Install(cause) => Some(&**cause),
        }
    }
}

/// What to tell someone whose install `fontaine update` cannot replace.
pub fn upgrade_hint(channel: Option<Channel>) -> String {
    let release = "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/latest/download/fontaine-installer.sh | sh";
    let dev = "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.sh | sh";
    let windows_release = "powershell -ExecutionPolicy Bypass -c \"irm https://github.com/madesroches/fontaine/releases/latest/download/fontaine-installer.ps1 | iex\"";
    let windows = "powershell -ExecutionPolicy Bypass -c \"irm https://github.com/madesroches/fontaine/releases/download/dev/fontaine-installer.ps1 | iex\"";
    let one = |channel: &str, shell: &str, windows: &str| {
        format!(
            "Reinstall from the {channel} channel instead (Linux and macOS):\n  {shell}\n\
             Windows:\n  {windows}\nSee docs/installing.md."
        )
    };
    match channel {
        Some(Channel::Release) => one("release", release, windows_release),
        Some(Channel::Dev) => one("dev", dev, windows),
        None => format!(
            "Install with a Fontaine installer to enable `fontaine update`:\n  {release}\n\
             or, for the dev channel:\n  {dev}\nWindows (dev channel):\n  {windows}\n\
             See docs/installing.md."
        ),
    }
}

pub struct Params<'a> {
    pub api_base: &'a str,
    pub receipt_path: PathBuf,
    pub current_exe: PathBuf,
    /// The target triple this binary was built for, which names its release asset.
    pub target: &'a str,
    pub keys: &'a TrustedKeys,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Updated {
        channel: Channel,
        from: String,
        to: String,
        /// Set when the receipt could not be rewritten.
        receipt_warning: Option<String>,
    },
    UpToDate {
        channel: Channel,
        version: String,
    },
}

impl Outcome {
    pub fn report(&self) -> String {
        match self {
            Outcome::Updated {
                channel: Channel::Dev,
                to,
                ..
            } => match release_index::dev_build_number(to) {
                Some(n) => format!("updated to dev.{n} ({to})"),
                None => format!("updated to {to}"),
            },
            Outcome::Updated { to, .. } => format!("updated to {to}"),
            Outcome::UpToDate { channel, version } => {
                format!("already up to date ({version}, {channel} channel)")
            }
        }
    }
}

/// An HTTP client that identifies itself and follows a bounded number of redirects, never to a
/// plain-http location from an https one.
pub fn build_client(
    network: &fontaine_tools::tls::LoadedNetwork,
) -> reqwest::Result<reqwest::Client> {
    fontaine_tools::tls::build_client(network, || {
        reqwest::Client::builder()
            .user_agent(fontaine_tools::FONTAINE_USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let downgrade = attempt
                    .previous()
                    .last()
                    .is_some_and(|p| p.scheme() == "https")
                    && attempt.url().scheme() != "https";
                if attempt.previous().len() >= MAX_REDIRECTS || downgrade {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
    })
}

fn archive_name(target: &str) -> String {
    let ext = if cfg!(windows) { "zip" } else { "tar.xz" };
    format!("fontaine-{target}.{ext}")
}

// A missing release is deterministic, so it is not the retryable network kind.
fn index_error(e: release_index::IndexError) -> UpdateError {
    match e {
        release_index::IndexError::NoRelease(_) => UpdateError::Install(Box::new(e)),
        _ => UpdateError::Network(Box::new(e)),
    }
}

fn not_self_updatable(e: ReceiptError) -> UpdateError {
    let channel = match &e {
        ReceiptError::OtherBinary { channel, .. } => Some(*channel),
        _ => None,
    };
    UpdateError::NotSelfUpdatable {
        channel,
        cause: Box::new(e),
    }
}

#[derive(Debug)]
struct DownloadError {
    url: String,
    source: Option<reqwest::Error>,
    status: Option<reqwest::StatusCode>,
    io: Option<std::io::Error>,
    too_large: bool,
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(status) = self.status {
            write!(f, "{} answered {status}", self.url)
        } else if self.too_large {
            write!(f, "{} is larger than expected", self.url)
        } else if self.io.is_some() {
            write!(f, "{} could not be saved", self.url)
        } else {
            write!(f, "{} could not be downloaded", self.url)
        }
    }
}

impl std::error::Error for DownloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e as _)
            .or(self.io.as_ref().map(|e| e as _))
    }
}

impl DownloadError {
    /// A full disk is a local fault and an oversized body is not the archive that was signed;
    /// only transport failures are worth retrying.
    fn into_update_error(self) -> UpdateError {
        if self.io.is_some() {
            UpdateError::Install(Box::new(self))
        } else if self.too_large {
            UpdateError::Verify(Box::new(self))
        } else {
            UpdateError::Network(Box::new(self))
        }
    }
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    limit: u64,
    mut sink: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> Result<(), DownloadError> {
    let fail = |source, status, io, too_large| DownloadError {
        url: url.to_string(),
        source,
        status,
        io,
        too_large,
    };
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| fail(Some(e), None, None, false))?;
    if !response.status().is_success() {
        return Err(fail(None, Some(response.status()), None, false));
    }
    let mut total = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| fail(Some(e), None, None, false))?
    {
        total += chunk.len() as u64;
        if total > limit {
            return Err(fail(None, None, None, true));
        }
        sink(&chunk).map_err(|e| fail(None, None, Some(e), false))?;
    }
    Ok(())
}

/// The caller's half of `verify_file`: the signed binding must name this channel and exactly the
/// build the release index offered, so a validly signed older or foreign archive is refused.
fn check_binding(
    channel: Channel,
    release: &Release,
    asset: &str,
    verified: &Verified,
) -> Result<(), String> {
    if verified.channel != channel || verified.file != asset {
        return Err("the signature is for a different channel or file".to_string());
    }
    let agrees = match release.build {
        Build::Release(_) => release_index::tag_build(channel, &format!("v{}", verified.version))
            .is_some_and(|b| b == release.build),
        Build::Dev(n) => release_index::dev_build_number(&verified.version) == Some(n),
    };
    if !agrees {
        return Err(format!(
            "the signed version {} does not match the release {}",
            verified.version, release.tag
        ));
    }
    Ok(())
}

pub async fn update(client: &reqwest::Client, params: &Params<'_>) -> Result<Outcome, UpdateError> {
    let located =
        receipt::locate(&params.receipt_path, &params.current_exe).map_err(not_self_updatable)?;
    let _lock = fontaine_core::file_lock::lock(&params.receipt_path)
        .map_err(|e| UpdateError::Install(Box::new(e)))?;
    // Another update may have finished while this one waited for the lock.
    let info: InstallInfo =
        receipt::locate(&params.receipt_path, &params.current_exe).map_err(not_self_updatable)?;
    drop(located);
    let channel = info.channel;
    let installed = release_index::installed_build(channel, &info.version).ok_or_else(|| {
        UpdateError::NotSelfUpdatable {
            channel: Some(channel),
            cause: Box::new(ReceiptError::Malformed("version is not a Fontaine version")),
        }
    })?;
    let dir = info
        .binary_path
        .parent()
        .expect("a binary path always has a parent");

    if let Err(e) = install::remove_stale(&info.binary_path) {
        eprintln!(
            "fontaine: warning: the previous update's leftover file could not be deleted: {}",
            chain(&e)
        );
    }

    let release = release_index::latest(client, params.api_base, channel)
        .await
        .map_err(index_error)?;
    if release_index::compare(installed, release.build) != Some(Ordering::Greater) {
        return Ok(Outcome::UpToDate {
            channel,
            version: info.version,
        });
    }

    let name = archive_name(params.target);
    let archive_asset = release.asset(&name).ok_or_else(|| {
        UpdateError::Install(
            format!("release {} has no {name} for this platform", release.tag).into(),
        )
    })?;
    let work = tempfile::Builder::new()
        .prefix(".fontaine-update-")
        .tempdir_in(dir)
        .map_err(|e| UpdateError::Install(Box::new(e)))?;
    let archive_path = work.path().join(&name);
    let mut file =
        std::fs::File::create(&archive_path).map_err(|e| UpdateError::Install(Box::new(e)))?;
    download(client, &archive_asset.url, MAX_ARCHIVE, |chunk| {
        file.write_all(chunk)
    })
    .await
    .map_err(DownloadError::into_update_error)?;
    drop(file);

    let signature = match release.asset(&format!("{name}.minisig")) {
        Some(asset) => {
            let mut bytes = Vec::new();
            download(client, &asset.url, MAX_SIGNATURE as u64, |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            })
            .await
            .map_err(DownloadError::into_update_error)?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        None => None,
    };
    let verified = crate::release_signature::verify_file(
        params.keys,
        channel,
        &archive_path,
        &name,
        signature.as_deref(),
    )
    .map_err(|e| UpdateError::Verify(Box::new(e)))?;
    check_binding(channel, &release, &name, &verified)
        .map_err(|why| UpdateError::Verify(why.into()))?;

    install::replace_binary(&archive_path, &info.binary_path, work.path())
        .map_err(|e| UpdateError::Install(Box::new(e)))?;

    let receipt_warning = receipt::write_version(&info.receipt_path, &verified.version)
        .err()
        .map(|e| chain(&e));
    Ok(Outcome::Updated {
        channel,
        from: info.version,
        to: verified.version,
        receipt_warning,
    })
}

fn print_error(e: &UpdateError) {
    eprintln!("fontaine: {}", chain(e));
    if let UpdateError::NotSelfUpdatable { channel, .. } = e {
        eprintln!("{}", upgrade_hint(*channel));
    }
}

pub fn run(config: &Config, host: &crate::Host) -> ExitCode {
    let network = match config.loaded_network(None) {
        Ok(network) => network,
        Err(e) => {
            eprintln!("fontaine: {}", chain(&e));
            return ExitCode::from(crate::EXIT_CONFIG);
        }
    };
    let client = match build_client(&network) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("fontaine: cannot build the HTTP client: {}", chain(&e));
            return ExitCode::from(crate::EXIT_CONFIG);
        }
    };
    let keys = TrustedKeys::embedded();
    let (Some(receipt_path), Ok(current_exe)) =
        (receipt::receipt_path(&*host.env), std::env::current_exe())
    else {
        let e = UpdateError::NotSelfUpdatable {
            channel: None,
            cause: "the install receipt location or the running executable is unknown".into(),
        };
        print_error(&e);
        return ExitCode::from(e.exit_code());
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("fontaine: cannot start the async runtime: {}", chain(&e));
            return ExitCode::from(1);
        }
    };
    let params = Params {
        api_base: release_index::DEFAULT_API_BASE,
        receipt_path,
        current_exe,
        target: env!("FONTAINE_BUILD_TARGET"),
        keys,
    };
    exit_for(runtime.block_on(update(&client, &params)))
}

fn exit_for(result: Result<Outcome, UpdateError>) -> ExitCode {
    match result {
        Ok(outcome) => {
            if let Outcome::Updated {
                receipt_warning: Some(why),
                ..
            } = &outcome
            {
                eprintln!(
                    "fontaine: warning: the binary was replaced but the install receipt was not \
                     updated: {why}"
                );
            }
            println!("fontaine: {}", outcome.report());
            ExitCode::SUCCESS
        }
        Err(e) => {
            print_error(&e);
            ExitCode::from(e.exit_code())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use release_index::SemVer;

    #[test]
    fn a_missing_release_is_not_retryable() {
        let missing = index_error(release_index::IndexError::NoRelease(Channel::Dev));
        assert!(matches!(missing, UpdateError::Install(_)));
        assert_eq!(missing.exit_code(), 1);
        let status = index_error(release_index::IndexError::Status(
            reqwest::StatusCode::BAD_GATEWAY,
            "https://example.test".into(),
        ));
        assert_eq!(status.exit_code(), EXIT_UPDATE_NETWORK);
    }

    fn release(build: Build, tag: &str) -> Release {
        Release {
            tag: tag.to_string(),
            build,
            assets: Vec::new(),
        }
    }

    fn verified(channel: Channel, file: &str, version: &str) -> Verified {
        Verified {
            channel,
            file: file.to_string(),
            version: version.to_string(),
            commit: "c".to_string(),
        }
    }

    #[test]
    fn binding_must_match_channel_file_and_version() {
        let dev = release(Build::Dev(7), "dev-7");
        let ok = verified(Channel::Dev, "a.tar.xz", "0.1.0-dev.7+abcdef1");
        assert!(check_binding(Channel::Dev, &dev, "a.tar.xz", &ok).is_ok());
        let wrong_n = verified(Channel::Dev, "a.tar.xz", "0.1.0-dev.6+abcdef1");
        assert!(check_binding(Channel::Dev, &dev, "a.tar.xz", &wrong_n).is_err());
        let wrong_channel = verified(Channel::Release, "a.tar.xz", "0.1.0-dev.7+abcdef1");
        assert!(check_binding(Channel::Dev, &dev, "a.tar.xz", &wrong_channel).is_err());
        let wrong_file = verified(Channel::Dev, "b.tar.xz", "0.1.0-dev.7+abcdef1");
        assert!(check_binding(Channel::Dev, &dev, "a.tar.xz", &wrong_file).is_err());

        let rel = release(Build::Release(SemVer(1, 2, 3)), "v1.2.3");
        let ok = verified(Channel::Release, "a", "1.2.3");
        assert!(check_binding(Channel::Release, &rel, "a", &ok).is_ok());
        let old = verified(Channel::Release, "a", "1.2.2");
        assert!(check_binding(Channel::Release, &rel, "a", &old).is_err());
    }

    #[tokio::test]
    async fn a_sink_write_error_is_an_install_error_not_a_network_one() {
        use wiremock::matchers::any;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 16]))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let err = download(&client, &server.uri(), 1024, |_| {
            Err(std::io::Error::other("no space left on device"))
        })
        .await
        .unwrap_err()
        .into_update_error();
        assert!(matches!(err, UpdateError::Install(_)));
        assert_eq!(err.exit_code(), 1);

        let err = download(&client, &server.uri(), 8, |_| Ok(()))
            .await
            .unwrap_err()
            .into_update_error();
        assert!(matches!(err, UpdateError::Verify(_)));
    }

    #[test]
    fn hint_names_the_channel_installer() {
        assert!(upgrade_hint(Some(Channel::Dev)).contains("download/dev/fontaine-installer.sh"));
        let release = upgrade_hint(Some(Channel::Release));
        assert!(release.contains("latest/download/fontaine-installer.sh"));
        assert!(!release.contains("download/dev/fontaine-installer.sh | sh"));
        assert!(release.contains("latest/download/fontaine-installer.ps1"));
        assert!(!release.contains("download/dev/fontaine-installer.ps1"));
    }

    #[test]
    fn reports_are_one_line() {
        let updated = Outcome::Updated {
            channel: Channel::Dev,
            from: "0.1.0-dev.1+aaaaaaa".into(),
            to: "0.1.0-dev.2+bbbbbbb".into(),
            receipt_warning: None,
        };
        assert_eq!(updated.report(), "updated to dev.2 (0.1.0-dev.2+bbbbbbb)");
    }
}
