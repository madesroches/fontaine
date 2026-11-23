//! Finding the newest build on a channel, and comparing it with the installed one.

use std::cmp::Ordering;

use serde::Deserialize;

use crate::release_signature::Channel;

pub const DEFAULT_API_BASE: &str = "https://api.github.com";
pub const DEFAULT_DOWNLOAD_BASE: &str = "https://github.com";
const REPO: &str = "madesroches/fontaine";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub build: Build,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// What a tag stands for: a semver on the release channel, a build number on dev.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    Release(SemVer),
    Dev(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemVer(pub u64, pub u64, pub u64);

impl SemVer {
    /// `X.Y.Z`, ignoring any `-pre` or `+build` suffix.
    pub fn parse(text: &str) -> Option<SemVer> {
        let core = text.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let mut next = || parts.next()?.parse::<u64>().ok();
        let version = SemVer(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

/// `<semver>-dev.<n>+<hex>` -> `n`.
pub fn dev_build_number(version: &str) -> Option<u64> {
    let (head, build) = version.split_once('+')?;
    if build.is_empty() || !build.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let (base, n) = head.split_once("-dev.")?;
    SemVer::parse(base).filter(|_| !base.contains(['-', '+']))?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// The build a receipt or signed version string names on `channel`.
pub fn installed_build(channel: Channel, version: &str) -> Option<Build> {
    match channel {
        Channel::Release => SemVer::parse(version).map(Build::Release),
        Channel::Dev => dev_build_number(version).map(Build::Dev),
    }
}

/// How `candidate` compares with `installed`; `None` when they are on different channels.
pub fn compare(installed: Build, candidate: Build) -> Option<Ordering> {
    match (installed, candidate) {
        (Build::Release(a), Build::Release(b)) => Some(b.cmp(&a)),
        (Build::Dev(a), Build::Dev(b)) => Some(b.cmp(&a)),
        _ => None,
    }
}

/// The tag's build on `channel`, if the tag has the channel's shape.
pub fn tag_build(channel: Channel, tag: &str) -> Option<Build> {
    match channel {
        Channel::Release => {
            let rest = tag.strip_prefix('v')?;
            if rest.contains(['-', '+']) {
                return None;
            }
            SemVer::parse(rest).map(Build::Release)
        }
        Channel::Dev => {
            let n = tag.strip_prefix("dev-")?;
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            n.parse().ok().map(Build::Dev)
        }
    }
}

#[derive(Debug)]
pub enum IndexError {
    Request(reqwest::Error),
    Status(reqwest::StatusCode, String),
    Parse(serde_json::Error),
    NoRelease(Channel),
    /// The dev release pointer (`latest-dev.json`) could not be fetched or is invalid.
    Pointer {
        url: String,
        cause: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Request(_) => f.write_str("the release list could not be fetched"),
            IndexError::Status(status, url) => write!(f, "{url} answered {status}"),
            IndexError::Parse(_) => f.write_str("the release list is not the expected JSON"),
            IndexError::NoRelease(c) => write!(f, "no published {c} release was found"),
            IndexError::Pointer { url, .. } => write!(
                f,
                "the dev release pointer {url} could not be fetched or is invalid; try again shortly"
            ),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexError::Request(e) => Some(e),
            IndexError::Parse(e) => Some(e),
            IndexError::Pointer { cause, .. } => cause.as_deref().map(|e| e as _),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

impl ApiRelease {
    fn into_release(self, build: Build) -> Release {
        Release {
            tag: self.tag_name,
            build,
            assets: self
                .assets
                .into_iter()
                .map(|a| Asset {
                    name: a.name,
                    url: a.browser_download_url,
                })
                .collect(),
        }
    }
}

async fn get(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, IndexError> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(IndexError::Request)?;
    let status = response.status();
    if !status.is_success() {
        return Err(IndexError::Status(status, url.to_string()));
    }
    Ok(response
        .bytes()
        .await
        .map_err(IndexError::Request)?
        .to_vec())
}

#[derive(Deserialize)]
struct Pointer {
    tag: String,
    assets: Vec<String>,
}

/// Fetches and parses the dev pointer with a plain GET: it lives on the download host, not the
/// API, so it has no GitHub `Accept` header and every failure maps to `IndexError::Pointer`.
async fn fetch_pointer(client: &reqwest::Client, url: &str) -> Result<Pointer, IndexError> {
    let fail = |cause: Option<Box<dyn std::error::Error + Send + Sync>>| IndexError::Pointer {
        url: url.to_string(),
        cause,
    };
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| fail(Some(Box::new(e))))?;
    let response = response
        .error_for_status()
        .map_err(|e| fail(Some(Box::new(e))))?;
    let body = response
        .bytes()
        .await
        .map_err(|e| fail(Some(Box::new(e))))?;
    serde_json::from_slice(&body).map_err(|e| fail(Some(Box::new(e))))
}

/// The newest published build on `channel`. `api_base` (release channel) and `download_base`
/// (dev channel pointer and assets) are parameters so tests can point them at a mock server.
pub async fn latest(
    client: &reqwest::Client,
    api_base: &str,
    download_base: &str,
    channel: Channel,
) -> Result<Release, IndexError> {
    match channel {
        Channel::Release => {
            let base = api_base.trim_end_matches('/');
            let body = get(client, &format!("{base}/repos/{REPO}/releases/latest")).await?;
            let release: ApiRelease = serde_json::from_slice(&body).map_err(IndexError::Parse)?;
            let build =
                tag_build(channel, &release.tag_name).ok_or(IndexError::NoRelease(channel))?;
            Ok(release.into_release(build))
        }
        Channel::Dev => {
            let base = download_base.trim_end_matches('/');
            let url = format!("{base}/{REPO}/releases/download/dev/latest-dev.json");
            let pointer = fetch_pointer(client, &url).await?;
            let build = tag_build(channel, &pointer.tag).ok_or_else(|| IndexError::Pointer {
                url: url.clone(),
                cause: Some(format!("{:?} is not a dev build tag", pointer.tag).into()),
            })?;
            let tag = pointer.tag;
            let assets = pointer
                .assets
                .into_iter()
                .map(|name| Asset {
                    url: format!("{base}/{REPO}/releases/download/{tag}/{name}"),
                    name,
                })
                .collect();
            Ok(Release { tag, build, assets })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_versions_parse() {
        assert_eq!(dev_build_number("0.1.0-dev.42+abcdef1"), Some(42));
        assert_eq!(dev_build_number("0.1.0-dev.x+abcdef1"), None);
        assert_eq!(dev_build_number("0.1.0-dev.4"), None);
        assert_eq!(dev_build_number("0.1.0+abc"), None);
        assert_eq!(dev_build_number("0.1-dev.4+abc"), None);
        assert_eq!(dev_build_number("0.1.0-dev.4+xyz"), None);
    }

    #[test]
    fn tags_have_channel_shapes() {
        assert_eq!(
            tag_build(Channel::Release, "v1.2.3"),
            Some(Build::Release(SemVer(1, 2, 3)))
        );
        assert_eq!(tag_build(Channel::Release, "v1.2.3-rc.1"), None);
        assert_eq!(tag_build(Channel::Release, "dev-4"), None);
        assert_eq!(tag_build(Channel::Dev, "dev-12"), Some(Build::Dev(12)));
        assert_eq!(tag_build(Channel::Dev, "dev"), None);
        assert_eq!(tag_build(Channel::Dev, "dev-1a"), None);
    }

    #[test]
    fn comparison_is_numeric_and_per_channel() {
        let rel = |a, b, c| Build::Release(SemVer(a, b, c));
        assert_eq!(
            compare(rel(0, 9, 0), rel(0, 10, 0)),
            Some(Ordering::Greater)
        );
        assert_eq!(compare(rel(1, 0, 0), rel(1, 0, 0)), Some(Ordering::Equal));
        assert_eq!(compare(rel(1, 0, 1), rel(1, 0, 0)), Some(Ordering::Less));
        assert_eq!(
            compare(Build::Dev(9), Build::Dev(10)),
            Some(Ordering::Greater)
        );
        assert_eq!(compare(Build::Dev(10), Build::Dev(9)), Some(Ordering::Less));
        assert_eq!(compare(Build::Dev(1), rel(1, 0, 0)), None);
    }

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const POINTER_PATH: &str = "/madesroches/fontaine/releases/download/dev/latest-dev.json";

    async fn serve_pointer(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(POINTER_PATH))
            .respond_with(response)
            .mount(server)
            .await;
    }

    async fn dev_latest(server: &MockServer) -> Result<Release, IndexError> {
        let uri = server.uri();
        latest(&reqwest::Client::new(), &uri, &uri, Channel::Dev).await
    }

    #[tokio::test]
    async fn the_dev_pointer_names_the_release_without_an_api_call() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::path_regex("^/repos/"))
            .respond_with(ResponseTemplate::new(403))
            .expect(0)
            .mount(&server)
            .await;
        serve_pointer(
            &server,
            ResponseTemplate::new(200)
                .set_body_string(r#"{"tag":"dev-7","assets":["a.tar.gz","a.tar.gz.minisig"]}"#),
        )
        .await;
        let release = dev_latest(&server).await.unwrap();
        assert_eq!(release.tag, "dev-7");
        assert_eq!(release.build, Build::Dev(7));
        let asset = release.asset("a.tar.gz.minisig").unwrap();
        assert_eq!(
            asset.url,
            format!(
                "{}/madesroches/fontaine/releases/download/dev-7/a.tar.gz.minisig",
                server.uri()
            )
        );
        assert_eq!(release.assets.len(), 2);
    }

    #[tokio::test]
    async fn a_missing_pointer_is_an_error_that_keeps_the_status() {
        let server = MockServer::start().await;
        serve_pointer(&server, ResponseTemplate::new(404)).await;
        let err = dev_latest(&server).await.unwrap_err();
        assert!(matches!(err, IndexError::Pointer { .. }));
        let chain = fontaine_sdk::tools::error_chain::chain(&err);
        assert!(chain.contains("404"), "{chain}");
    }

    #[tokio::test]
    async fn a_malformed_pointer_is_an_error() {
        let server = MockServer::start().await;
        serve_pointer(
            &server,
            ResponseTemplate::new(200).set_body_string("not json"),
        )
        .await;
        let err = dev_latest(&server).await.unwrap_err();
        assert!(matches!(err, IndexError::Pointer { .. }));
    }

    #[tokio::test]
    async fn a_pointer_with_a_bad_tag_is_an_error() {
        let server = MockServer::start().await;
        serve_pointer(
            &server,
            ResponseTemplate::new(200).set_body_string(r#"{"tag":"v1.0.0","assets":[]}"#),
        )
        .await;
        let err = dev_latest(&server).await.unwrap_err();
        assert!(matches!(err, IndexError::Pointer { .. }));
    }
}
